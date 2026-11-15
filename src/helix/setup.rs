//! The embedded editor's runtime files: where they live and how they get
//! there.
//!
//! Helix reads its syntax queries, themes and compiled tree-sitter grammars
//! from a runtime directory. The embedded editor has one of its own, in
//! tuicr's data directory, so that it never depends on a Helix installed
//! beside it, on `~/.config/helix/runtime`, or on a path a package manager
//! chose. The files are copied there from the `runtime` directory of the
//! Helix sources this tuicr was built from, and a stamp records which
//! Helix that was.
//!
//! `tuicr --helix-setup` ([`run_setup`]) does the copy and then fetches and
//! builds the grammars, which takes `git`, a C compiler and the network.
//! The copy alone is cheap, so the first time the editor opens it is redone
//! whenever the stamp is not this build's ([`RuntimeDirs::prepare`]).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Name of the runtime directory inside tuicr's data directory.
pub const RUNTIME_DIR_NAME: &str = "helix-runtime";

/// The file in the runtime directory that holds the stamp.
const STAMP_FILE: &str = "tuicr-helix-rev";

/// What is copied from the Helix sources: the tree-sitter queries, the
/// themes, and the tutor's text. Grammars are built, not copied.
const ASSETS: [&str; 3] = ["queries", "themes", "tutor"];

/// The longest a setup notice may be: it has to fit an 80-column status
/// line, also behind what the review's own line says first.
pub const NOTICE_WIDTH: usize = 60;

/// No runtime files, and the Helix sources they are copied from are not
/// on this machine. The command explains that when run.
const NOTICE_NO_SOURCE: &str = "run `tuicr --helix-setup`: no runtime files, plain text only";
/// The copy of the runtime files failed. The command says why.
const NOTICE_NOT_COPIED: &str = "run `tuicr --helix-setup`: runtime files not installed";
/// The runtime files are there and no grammar is built.
const NOTICE_NO_GRAMMARS: &str = "run `tuicr --helix-setup` once: no syntax highlighting yet";

/// Version and commit of the Helix this tuicr was built with: what the
/// stamp of an up-to-date runtime directory says.
pub fn helix_rev() -> &'static str {
    helix_embed::HELIX_REV
}

/// The two places the editor's runtime files can be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDirs {
    /// tuicr's own copy, and where grammars are built:
    /// `<data dir>/helix-runtime`.
    pub installed: PathBuf,
    /// The `runtime` directory of the Helix sources this tuicr was built
    /// from. It is a path on the machine that built tuicr and may be gone.
    pub bundled: PathBuf,
}

impl RuntimeDirs {
    /// The directories of this user and this build. `None` when the system
    /// gives no data directory (no home directory).
    pub fn for_user() -> Option<Self> {
        let dirs = directories::ProjectDirs::from("", "", "tuicr")?;
        // The build wrote it as `<crate>/../runtime`; when it is there, say
        // where it really is.
        let bundled = PathBuf::from(helix_embed::BUNDLED_RUNTIME_DIR);
        let bundled = bundled.canonicalize().unwrap_or(bundled);
        Some(Self {
            installed: dirs.data_dir().join(RUNTIME_DIR_NAME),
            bundled,
        })
    }

    /// Where the editor looks, in order: tuicr's own copy first, then the
    /// Helix sources if they are still there.
    pub fn search_path(&self) -> Vec<PathBuf> {
        let mut path = vec![self.installed.clone()];
        if self.bundled.is_dir() {
            path.push(self.bundled.clone());
        }
        path
    }

    /// The Helix the installed files were copied from; `None` when nothing
    /// was installed, or a copy did not finish.
    pub fn installed_rev(&self) -> Option<String> {
        let stamp = fs::read_to_string(self.installed.join(STAMP_FILE)).ok()?;
        let stamp = stamp.trim();
        (!stamp.is_empty()).then(|| stamp.to_string())
    }

    /// Whether the installed files are those of this build's Helix.
    pub fn is_current(&self) -> bool {
        self.installed_rev().as_deref() == Some(helix_rev())
    }

    /// Copy the queries, themes and tutor from the Helix sources into
    /// tuicr's runtime directory, replacing what is there, and stamp it.
    /// Built grammars and their sources are left alone.
    ///
    /// The stamp is removed first and written last, so a copy that was
    /// interrupted is not taken for a finished one.
    pub fn copy_assets(&self) -> anyhow::Result<()> {
        if !self.bundled.is_dir() {
            bail!(
                "the Helix runtime files this tuicr was built with are not at {}; \
                 they are copied from the Helix source tree at build time, so build \
                 tuicr on this machine",
                self.bundled.display()
            );
        }
        fs::create_dir_all(&self.installed)
            .with_context(|| format!("could not create {}", self.installed.display()))?;
        let stamp = self.installed.join(STAMP_FILE);
        match fs::remove_file(&stamp) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("could not remove {}", stamp.display()));
            }
        }
        for name in ASSETS {
            let from = self.bundled.join(name);
            let to = self.installed.join(name);
            replace_with_copy(&from, &to)
                .with_context(|| format!("could not copy {}", from.display()))?;
        }
        fs::write(&stamp, format!("{}\n", helix_rev()))
            .with_context(|| format!("could not write {}", stamp.display()))?;
        Ok(())
    }

    /// Get the runtime directory ready before the editor opens a file.
    ///
    /// When the installed files are not this build's, they are copied again
    /// (grammars are not built: that is `tuicr --helix-setup`). Nothing
    /// here stops the editor from opening: it shows plain text without the
    /// files. The return value is what to tell the user in that case.
    ///
    /// The notice is one line for a status line, which is cut off at the
    /// terminal's right edge: it starts with the command to run and fits
    /// 80 columns with room to spare ([`NOTICE_WIDTH`]). The command says
    /// the rest.
    pub fn prepare(&self) -> Option<String> {
        if !self.is_current() {
            if !self.bundled.is_dir() {
                return Some(NOTICE_NO_SOURCE.to_string());
            }
            if self.copy_assets().is_err() {
                return Some(NOTICE_NOT_COPIED.to_string());
            }
        }
        if !self.has_grammars() {
            return Some(NOTICE_NO_GRAMMARS.to_string());
        }
        None
    }

    /// Whether any compiled grammar is in reach of the editor.
    pub fn has_grammars(&self) -> bool {
        self.search_path()
            .iter()
            .any(|dir| !grammar_libraries(dir).is_empty())
    }
}

/// The compiled grammars in `runtime/grammars`, by file name, sorted.
pub fn grammar_libraries(runtime: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(runtime.join("grammars")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            // `.so` on Linux, `.dylib` on macOS, `.dll` on Windows.
            matches!(
                entry.path().extension().and_then(|ext| ext.to_str()),
                Some("so" | "dylib" | "dll")
            )
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Put a copy of `from` (a file or a directory tree) at `to`, replacing
/// whatever is there. The copy is made beside `to` and moved into place, so
/// `to` is never half of the old and half of the new.
fn replace_with_copy(from: &Path, to: &Path) -> anyhow::Result<()> {
    let name = to
        .file_name()
        .with_context(|| format!("{} has no file name", to.display()))?;
    let mut staging_name = std::ffi::OsString::from(".");
    staging_name.push(name);
    staging_name.push(format!(".new-{}", std::process::id()));
    let staging = to.with_file_name(staging_name);
    remove_any(&staging)?;

    let copied = copy_tree(from, &staging).and_then(|()| {
        remove_any(to)?;
        fs::rename(&staging, to).with_context(|| format!("could not move into {}", to.display()))
    });
    if copied.is_err() {
        let _ = remove_any(&staging);
    }
    copied
}

/// Remove a file or a directory tree; nothing there is not an error.
fn remove_any(path: &Path) -> anyhow::Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => Err(err),
    };
    removed.with_context(|| format!("could not remove {}", path.display()))
}

/// Copy a file, or a directory with everything below it. Symbolic links
/// are followed: the copy holds files only.
fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<()> {
    let meta = fs::metadata(from).with_context(|| format!("could not read {}", from.display()))?;
    if !meta.is_dir() {
        fs::copy(from, to).with_context(|| format!("could not write {}", to.display()))?;
        return Ok(());
    }
    fs::create_dir_all(to).with_context(|| format!("could not create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("could not list {}", from.display()))? {
        let entry = entry.with_context(|| format!("could not list {}", from.display()))?;
        copy_tree(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

/// `tuicr --helix-setup`: install the editor's runtime files and build the
/// grammars, reporting to `out`.
///
/// Copies the queries, themes and tutor, stamps the directory, then
/// fetches the grammar sources with `git` and compiles them into the same
/// directory. Which grammars is the user's choice (`use-grammars` in
/// Helix's `languages.toml`); without one it is every grammar Helix knows.
/// Helix prints its own progress for that part to stdout.
///
/// A grammar that cannot be fetched or built makes this fail, after every
/// other one was fetched and built: a source that cannot be fetched does
/// not keep the rest from being compiled. What was done is kept, so
/// running it again does only what is left.
pub fn run_setup(dirs: &RuntimeDirs, out: &mut dyn Write) -> anyhow::Result<()> {
    writeln!(out, "Helix {} for tuicr's embedded editor", helix_rev())?;
    writeln!(out, "Runtime directory: {}", dirs.installed.display())?;

    dirs.copy_assets()?;
    writeln!(
        out,
        "Copied {} from {}",
        ASSETS.join(", "),
        dirs.bundled.display()
    )?;
    out.flush()?;

    // Only tuicr's own directory: the grammars are built into the first
    // one listed, and nothing here should depend on the sources staying.
    let built = helix_embed::setup_grammars(vec![dirs.installed.clone()], true);

    // Said either way: when one grammar failed, the others are there.
    let grammars = grammar_libraries(&dirs.installed);
    writeln!(
        out,
        "{} grammars ready in {}",
        grammars.len(),
        dirs.installed.join("grammars").display()
    )?;
    out.flush()?;
    built.context(
        "the grammars are not all built; what was done is kept, so run \
         `tuicr --helix-setup` again once the cause is fixed",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the Helix source tree's `runtime` directory, and an
    /// empty place to install into.
    fn fixture() -> (tempfile::TempDir, RuntimeDirs) {
        let dir = tempfile::tempdir().expect("temp dir");
        let bundled = dir.path().join("src").join("runtime");
        fs::create_dir_all(bundled.join("queries").join("rust")).unwrap();
        fs::write(
            bundled.join("queries").join("rust").join("highlights.scm"),
            "(comment) @comment\n",
        )
        .unwrap();
        fs::create_dir_all(bundled.join("themes").join("licenses")).unwrap();
        fs::write(bundled.join("themes").join("onedark.toml"), "# theme\n").unwrap();
        fs::write(
            bundled
                .join("themes")
                .join("licenses")
                .join("onedark.license"),
            "MIT\n",
        )
        .unwrap();
        fs::write(bundled.join("tutor"), "welcome\n").unwrap();
        // Not copied: built on the user's machine.
        fs::create_dir_all(bundled.join("grammars")).unwrap();
        fs::write(bundled.join("grammars").join("rust.so"), "elf").unwrap();
        let dirs = RuntimeDirs {
            installed: dir.path().join("data").join(RUNTIME_DIR_NAME),
            bundled,
        };
        (dir, dirs)
    }

    #[test]
    fn should_copy_queries_themes_and_tutor_and_stamp_the_directory() {
        let (_dir, dirs) = fixture();
        assert_eq!(dirs.installed_rev(), None);
        assert!(!dirs.is_current());

        dirs.copy_assets().expect("copy");

        let read = |rel: &str| fs::read_to_string(dirs.installed.join(rel)).unwrap();
        assert_eq!(read("queries/rust/highlights.scm"), "(comment) @comment\n");
        assert_eq!(read("themes/onedark.toml"), "# theme\n");
        assert_eq!(read("themes/licenses/onedark.license"), "MIT\n");
        assert_eq!(read("tutor"), "welcome\n");
        assert_eq!(dirs.installed_rev().as_deref(), Some(helix_rev()));
        assert!(dirs.is_current());
        assert!(
            !dirs.installed.join("grammars").exists(),
            "grammars are built by `--helix-setup`, never copied"
        );
    }

    #[test]
    fn should_replace_old_files_and_keep_built_grammars() {
        let (_dir, dirs) = fixture();
        dirs.copy_assets().expect("first copy");
        // A query file an older Helix had and this one dropped, an edited
        // theme, and what `--helix-setup` built.
        let stale = dirs.installed.join("queries").join("rust").join("old.scm");
        fs::write(&stale, "old").unwrap();
        fs::write(dirs.installed.join("themes").join("onedark.toml"), "edited").unwrap();
        let grammars = dirs.installed.join("grammars");
        fs::create_dir_all(grammars.join("sources").join("rust")).unwrap();
        fs::write(grammars.join("rust.dylib"), "macho").unwrap();
        fs::write(dirs.installed.join(STAMP_FILE), "25.01 (0000000)\n").unwrap();
        assert!(!dirs.is_current());

        dirs.copy_assets().expect("second copy");

        assert!(!stale.exists(), "files the new version dropped are gone");
        assert_eq!(
            fs::read_to_string(dirs.installed.join("themes").join("onedark.toml")).unwrap(),
            "# theme\n"
        );
        assert_eq!(
            fs::read_to_string(grammars.join("rust.dylib")).unwrap(),
            "macho"
        );
        assert!(grammars.join("sources").join("rust").is_dir());
        assert!(dirs.is_current());
        let leftovers: Vec<_> = fs::read_dir(&dirs.installed)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.contains(".new-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn should_refuse_to_copy_from_sources_that_are_gone_and_touch_nothing() {
        let (_dir, mut dirs) = fixture();
        dirs.copy_assets().expect("copy");
        dirs.bundled = dirs.bundled.with_file_name("no-such-runtime");

        let err = dirs.copy_assets().expect_err("nothing to copy from");

        assert!(format!("{err:#}").contains("no-such-runtime"), "{err:#}");
        // What was installed before is untouched, stamp included.
        assert!(dirs.is_current());
        assert!(dirs.installed.join("tutor").exists());
    }

    #[test]
    fn should_stop_the_setup_command_before_any_grammar_when_the_copy_fails() {
        let (_dir, mut dirs) = fixture();
        dirs.bundled = dirs.bundled.with_file_name("no-such-runtime");
        let mut out = Vec::new();

        let err = run_setup(&dirs, &mut out).expect_err("nothing to copy from");

        assert!(format!("{err:#}").contains("no-such-runtime"), "{err:#}");
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains(helix_rev()), "{out}");
        assert!(out.contains(&dirs.installed.display().to_string()), "{out}");
        assert!(!out.contains("grammars ready"), "{out}");
        assert!(!dirs.installed.exists());
    }

    #[test]
    fn should_not_take_an_interrupted_copy_for_a_finished_one() {
        let (_dir, dirs) = fixture();
        dirs.copy_assets().expect("copy");
        // The sources lose a piece: the next copy fails half way.
        fs::remove_file(dirs.bundled.join("tutor")).unwrap();

        let err = dirs.copy_assets().expect_err("the tutor is missing");

        assert!(format!("{err:#}").contains("tutor"), "{err:#}");
        assert_eq!(dirs.installed_rev(), None);
        assert!(!dirs.is_current());
    }

    #[test]
    fn should_list_the_installed_copy_first_and_the_sources_while_they_exist() {
        let (_dir, mut dirs) = fixture();
        assert_eq!(
            dirs.search_path(),
            vec![dirs.installed.clone(), dirs.bundled.clone()]
        );

        dirs.bundled = dirs.bundled.with_file_name("no-such-runtime");
        assert_eq!(dirs.search_path(), vec![dirs.installed.clone()]);
    }

    #[test]
    fn should_install_the_files_by_itself_before_the_editor_first_opens() {
        let (_dir, dirs) = fixture();

        // The sources have a built grammar, so there is nothing to say.
        assert_eq!(dirs.prepare(), None);

        assert!(dirs.is_current());
        assert!(dirs.installed.join("queries").join("rust").is_dir());
        assert!(
            !dirs.installed.join("grammars").exists(),
            "the automatic copy never builds grammars"
        );
    }

    #[test]
    fn should_redo_the_copy_when_the_stamp_is_another_helix_s() {
        let (_dir, dirs) = fixture();
        dirs.copy_assets().expect("copy");
        fs::write(dirs.installed.join(STAMP_FILE), "24.07 (abcdef12)\n").unwrap();
        fs::remove_file(dirs.installed.join("tutor")).unwrap();

        assert_eq!(dirs.prepare(), None);

        assert!(dirs.is_current());
        assert!(dirs.installed.join("tutor").exists());
    }

    #[test]
    fn should_leave_current_files_alone() {
        let (_dir, dirs) = fixture();
        dirs.copy_assets().expect("copy");
        // Were the copy redone, this would be back.
        fs::remove_file(dirs.installed.join("tutor")).unwrap();

        assert_eq!(dirs.prepare(), None);

        assert!(!dirs.installed.join("tutor").exists());
    }

    #[test]
    fn should_say_so_when_the_files_are_stale_and_the_sources_are_gone() {
        let (_dir, mut dirs) = fixture();
        dirs.bundled = dirs.bundled.with_file_name("no-such-runtime");

        let notice = dirs.prepare().expect("something to tell the user");

        assert!(notice.contains("--helix-setup"), "{notice}");
        assert!(notice.contains("no runtime files"), "{notice}");
        assert!(!dirs.installed.exists(), "nothing was written");
    }

    #[test]
    fn should_say_so_when_the_files_cannot_be_copied() {
        let (dir, mut dirs) = fixture();
        // A file where the data directory should be.
        let blocker = dir.path().join("not-a-directory");
        fs::write(&blocker, "").unwrap();
        dirs.installed = blocker.join(RUNTIME_DIR_NAME);

        let notice = dirs.prepare().expect("the copy failed");

        assert_eq!(notice, NOTICE_NOT_COPIED);
    }

    #[test]
    fn should_keep_every_notice_short_and_start_it_with_the_command() {
        // A status line is one row, cut off at the right edge: at 80
        // columns the old wording ended before it named the command.
        for notice in [NOTICE_NO_SOURCE, NOTICE_NOT_COPIED, NOTICE_NO_GRAMMARS] {
            assert!(notice.starts_with("run `tuicr --helix-setup`"), "{notice}");
            assert!(
                notice.chars().count() <= NOTICE_WIDTH,
                "{} columns: {notice}",
                notice.chars().count()
            );
        }
    }

    #[test]
    fn should_say_how_to_get_highlighting_while_no_grammar_is_built() {
        let (_dir, dirs) = fixture();
        fs::remove_file(dirs.bundled.join("grammars").join("rust.so")).unwrap();

        let notice = dirs.prepare().expect("no grammar anywhere");

        assert!(notice.contains("no syntax highlighting"), "{notice}");
        assert!(notice.contains("tuicr --helix-setup"), "{notice}");
        assert!(dirs.is_current(), "the files were still installed");

        // Once one is built into tuicr's directory there is nothing to say.
        let grammars = dirs.installed.join("grammars");
        fs::create_dir_all(&grammars).unwrap();
        fs::write(grammars.join("rust.so"), "elf").unwrap();
        fs::write(grammars.join("notes.txt"), "not a library").unwrap();
        assert_eq!(dirs.prepare(), None);
        assert_eq!(grammar_libraries(&dirs.installed), ["rust.so"]);
    }

    #[test]
    fn should_keep_the_runtime_inside_tuicr_s_own_data_directory() {
        // Not `~/.config/helix/runtime`, not a package manager's path.
        let Some(dirs) = RuntimeDirs::for_user() else {
            return;
        };
        assert_eq!(
            dirs.installed.file_name().and_then(|name| name.to_str()),
            Some(RUNTIME_DIR_NAME)
        );
        let parent = dirs.installed.parent().expect("a data directory");
        assert!(
            parent.components().any(|part| part.as_os_str() == "tuicr"),
            "{}",
            parent.display()
        );
        assert!(dirs.bundled.ends_with("runtime"), "{:?}", dirs.bundled);
    }
}
