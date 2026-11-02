//! `git status` for the changes window: porcelain v2 parsed into
//! [`StatusEntry`] rows, submodule rows enriched with commit counts, the
//! entries inside dirty submodules nested under them, and the operations the
//! window runs (stage, unstage, discard, commit). Every operation takes the
//! worktree explicitly, so a nested submodule is driven exactly like the root.

use std::path::{Path, PathBuf};

use super::run_git_command;
use crate::error::Result;
use crate::vcs::traits::{ChangeArea, StatusEntry, SubmoduleState};

/// How deep nested submodules are walked.
const MAX_SUBMODULE_DEPTH: usize = 3;

/// Every changed path under `root`, with dirty submodules walked.
pub(super) fn working_tree_status(root: &Path) -> Result<Vec<StatusEntry>> {
    status_in(root, Path::new(""), 0)
}

/// Status of the worktree at `root/repo`, its entries at `depth`.
fn status_in(root: &Path, repo: &Path, depth: usize) -> Result<Vec<StatusEntry>> {
    let workdir = root.join(repo);
    // `--no-optional-locks` keeps a status poll from fighting an editor or
    // another git over the index lock; `--ignore-submodules=none` reports
    // every submodule, whatever `submodule.<name>.ignore` says, because this
    // window exists to tell the truth about them.
    let output = run_git_command(
        &workdir,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
    )?;
    let mut entries = Vec::new();
    for parsed in parse_porcelain_v2(&output, repo, depth) {
        let Parsed {
            mut entry,
            old_sha,
            new_sha,
        } = parsed;
        if let Some(state) = entry.submodule.as_mut() {
            let sub_dir = workdir.join(&entry.path);
            if state.commit_changed {
                let new_sha = new_sha.or_else(|| submodule_head(&sub_dir));
                if let (Some(old), Some(new)) = (old_sha, new_sha) {
                    (state.behind, state.ahead) = commit_counts(&sub_dir, &old, &new);
                }
            }
            let dirty = entry.area == ChangeArea::Unstaged && state.is_dirty();
            let nested = if dirty && depth < MAX_SUBMODULE_DEPTH && sub_dir.join(".git").exists() {
                status_in(root, &entry.worktree(), depth + 1).unwrap_or_default()
            } else {
                Vec::new()
            };
            entries.push(entry);
            entries.extend(nested);
        } else {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// A parsed record plus the commit shas a submodule row counts between:
/// `old` is the commit the superproject records, `new` the one the
/// submodule is on (`None` for the worktree side, where it is HEAD).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parsed {
    entry: StatusEntry,
    old_sha: Option<String>,
    new_sha: Option<String>,
}

/// Parse `git status --porcelain=v2 -z` output into entries at `depth`
/// belonging to `repo`, grouped in [`ChangeArea`] order and in git's path
/// order within a group. A path changed on both sides yields two entries.
fn parse_porcelain_v2(text: &str, repo: &Path, depth: usize) -> Vec<Parsed> {
    let mut out: Vec<Parsed> = Vec::new();
    let mut fields = text.split('\0');
    while let Some(record) = fields.next() {
        match record.chars().next() {
            // 1 XY sub mH mI mW hH hI path
            Some('1') => {
                let parts: Vec<&str> = record.splitn(9, ' ').collect();
                if parts.len() == 9 {
                    push_tracked(
                        &mut out,
                        TrackedRecord {
                            xy: parts[1],
                            sub: parts[2],
                            head_sha: parts[6],
                            index_sha: parts[7],
                            path: parts[8],
                            orig_path: None,
                        },
                        repo,
                        depth,
                    );
                }
            }
            // 2 XY sub mH mI mW hH hI Xscore path NUL origPath
            Some('2') => {
                let parts: Vec<&str> = record.splitn(10, ' ').collect();
                let orig_path = fields.next();
                if parts.len() == 10 {
                    push_tracked(
                        &mut out,
                        TrackedRecord {
                            xy: parts[1],
                            sub: parts[2],
                            head_sha: parts[6],
                            index_sha: parts[7],
                            path: parts[9],
                            orig_path,
                        },
                        repo,
                        depth,
                    );
                }
            }
            // u XY sub m1 m2 m3 mW h1 h2 h3 path
            Some('u') => {
                let parts: Vec<&str> = record.splitn(11, ' ').collect();
                if parts.len() == 11 {
                    out.push(Parsed {
                        entry: StatusEntry {
                            repo: repo.to_path_buf(),
                            area: ChangeArea::Conflict,
                            path: PathBuf::from(parts[10]),
                            orig_path: None,
                            code: parts[1].to_string(),
                            submodule: submodule_flags(parts[2]).map(|_| SubmoduleState::default()),
                            depth,
                        },
                        old_sha: None,
                        new_sha: None,
                    });
                }
            }
            Some('?') => {
                if let Some(path) = record.strip_prefix("? ") {
                    out.push(Parsed {
                        entry: StatusEntry {
                            repo: repo.to_path_buf(),
                            area: ChangeArea::Untracked,
                            path: PathBuf::from(path),
                            orig_path: None,
                            code: "?".to_string(),
                            submodule: None,
                            depth,
                        },
                        old_sha: None,
                        new_sha: None,
                    });
                }
            }
            // Ignored paths (`!`, not requested) and `#` headers.
            _ => {}
        }
    }
    // Stable: git's path order survives inside each area.
    out.sort_by_key(|parsed| parsed.entry.area);
    out
}

struct TrackedRecord<'a> {
    xy: &'a str,
    sub: &'a str,
    head_sha: &'a str,
    index_sha: &'a str,
    path: &'a str,
    orig_path: Option<&'a str>,
}

/// A `1` or `2` record: one entry per changed side.
fn push_tracked(out: &mut Vec<Parsed>, record: TrackedRecord<'_>, repo: &Path, depth: usize) {
    let mut xy = record.xy.chars();
    let x = xy.next().unwrap_or('.');
    let y = xy.next().unwrap_or('.');
    let flags = submodule_flags(record.sub);
    if x != '.' {
        out.push(Parsed {
            entry: StatusEntry {
                repo: repo.to_path_buf(),
                area: ChangeArea::Staged,
                path: PathBuf::from(record.path),
                orig_path: record
                    .orig_path
                    .filter(|_| matches!(x, 'R' | 'C'))
                    .map(PathBuf::from),
                code: x.to_string(),
                submodule: flags.map(|_| SubmoduleState {
                    commit_changed: x == 'M',
                    ..SubmoduleState::default()
                }),
                depth,
            },
            old_sha: nonzero_sha(record.head_sha),
            new_sha: nonzero_sha(record.index_sha),
        });
    }
    if y != '.' {
        out.push(Parsed {
            entry: StatusEntry {
                repo: repo.to_path_buf(),
                area: ChangeArea::Unstaged,
                path: PathBuf::from(record.path),
                orig_path: None,
                code: y.to_string(),
                submodule: flags.map(|(commit_changed, modified, untracked)| SubmoduleState {
                    commit_changed,
                    modified,
                    untracked,
                    ..SubmoduleState::default()
                }),
                depth,
            },
            old_sha: nonzero_sha(record.index_sha),
            new_sha: None,
        });
    }
}

/// `S<C><M><U>` → (commit changed, tracked content modified, untracked
/// content); `None` for `N...` (not a submodule).
fn submodule_flags(field: &str) -> Option<(bool, bool, bool)> {
    let bytes = field.as_bytes();
    if bytes.len() < 4 || bytes[0] != b'S' {
        return None;
    }
    Some((bytes[1] == b'C', bytes[2] == b'M', bytes[3] == b'U'))
}

fn nonzero_sha(sha: &str) -> Option<String> {
    (!sha.is_empty() && !sha.chars().all(|c| c == '0')).then(|| sha.to_string())
}

fn submodule_head(sub_dir: &Path) -> Option<String> {
    run_git_command(sub_dir, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
}

/// `(behind, ahead)`: commits only in `old`, commits only in `new`. Zero
/// when either commit is missing from the submodule's object store.
fn commit_counts(sub_dir: &Path, old: &str, new: &str) -> (usize, usize) {
    let range = format!("{old}...{new}");
    let Ok(output) = run_git_command(sub_dir, &["rev-list", "--count", "--left-right", &range])
    else {
        return (0, 0);
    };
    let mut parts = output.split_whitespace();
    let behind = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let ahead = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    (behind, ahead)
}

// Operations. Each takes the worktree it runs in.

fn path_args(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

pub(super) fn stage_paths(workdir: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let owned = path_args(paths);
    let mut args = vec!["add", "--"];
    args.extend(owned.iter().map(String::as_str));
    run_git_command(workdir, &args).map(|_| ())
}

/// `reset` needs a HEAD; on an unborn branch fall back to dropping the
/// entries from the index.
pub(super) fn unstage_paths(workdir: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let owned = path_args(paths);
    let mut args = vec!["reset", "-q", "--"];
    args.extend(owned.iter().map(String::as_str));
    run_git_command(workdir, &args)
        .or_else(|_| {
            let mut args = vec!["rm", "-q", "--cached", "--"];
            args.extend(owned.iter().map(String::as_str));
            run_git_command(workdir, &args)
        })
        .map(|_| ())
}

pub(super) fn stage_all(workdir: &Path) -> Result<()> {
    run_git_command(workdir, &["add", "-A"]).map(|_| ())
}

pub(super) fn unstage_all(workdir: &Path) -> Result<()> {
    run_git_command(workdir, &["reset", "-q"])
        .or_else(|_| run_git_command(workdir, &["rm", "-r", "-q", "--cached", "--", "."]))
        .map(|_| ())
}

/// A tracked file goes back to HEAD; a file only in the index leaves the
/// index and the disk; an untracked file (or directory) is deleted.
pub(super) fn discard_path(workdir: &Path, path: &Path) -> Result<()> {
    let path = path.to_string_lossy();
    let tracked = run_git_command(workdir, &["ls-files", "--error-unmatch", "--", &path]).is_ok();
    let in_head = run_git_command(workdir, &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok();
    if !tracked {
        run_git_command(workdir, &["clean", "-f", "-d", "-q", "--", &path])
    } else if in_head {
        run_git_command(workdir, &["checkout", "HEAD", "--", &path])
    } else {
        run_git_command(workdir, &["rm", "-q", "-f", "--", &path])
    }
    .map(|_| ())
}

pub(super) fn commit(workdir: &Path, message: &str, amend: bool) -> Result<String> {
    let mut args = vec!["commit", "-q"];
    if amend {
        args.push("--amend");
    }
    args.extend(["-m", message]);
    run_git_command(workdir, &args)
        .and_then(|_| run_git_command(workdir, &["rev-parse", "--short", "HEAD"]))
        .map(|id| id.trim().to_string())
}

pub(super) fn head_commit_message(workdir: &Path) -> Result<String> {
    run_git_command(workdir, &["log", "-1", "--format=%B"])
}

/// Check the submodule at `path` out on the commit the superproject's
/// index records (its HEAD's commit when `to_head`, unstaging first). No
/// fetch: a commit the submodule does not have is an error, not a download.
pub(super) fn submodule_checkout(workdir: &Path, path: &Path, to_head: bool) -> Result<()> {
    let path = path.to_string_lossy();
    if to_head {
        // Best effort: on an unborn branch there is nothing to reset to.
        let _ = run_git_command(workdir, &["reset", "-q", "--", &path]);
    }
    run_git_command(
        workdir,
        &[
            "submodule",
            "update",
            "--checkout",
            "--no-fetch",
            "--",
            &path,
        ],
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(text: &str) -> Vec<StatusEntry> {
        parse_porcelain_v2(text, Path::new(""), 0)
            .into_iter()
            .map(|p| p.entry)
            .collect()
    }

    fn row(entries: &[StatusEntry], area: ChangeArea, path: &str) -> StatusEntry {
        entries
            .iter()
            .find(|e| e.area == area && e.path == Path::new(path))
            .cloned()
            .unwrap_or_else(|| panic!("no {area:?} row for {path}: {entries:?}"))
    }

    const ZERO: &str = "0000000000000000000000000000000000000000";
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn a_path_changed_on_both_sides_lands_in_both_areas() {
        let text = format!("1 MM N... 100644 100644 100644 {A} {B} src/both.rs\0");
        let got = entries(&text);
        assert_eq!(got.len(), 2);
        assert_eq!(row(&got, ChangeArea::Staged, "src/both.rs").code, "M");
        assert_eq!(row(&got, ChangeArea::Unstaged, "src/both.rs").code, "M");
        assert!(got.iter().all(|e| e.submodule.is_none() && e.depth == 0));
    }

    #[test]
    fn areas_are_grouped_in_display_order_keeping_path_order() {
        let text = format!(
            "1 .M N... 100644 100644 100644 {A} {A} b.rs\0\
             ? zed.txt\0\
             1 A. N... 000000 100644 100644 {ZERO} {A} c.rs\0\
             1 .M N... 100644 100644 100644 {A} {A} a.rs\0\
             u UU N... 100644 100644 100644 100644 {A} {B} {A} conflict.rs\0\
             ? alpha.txt\0"
        );
        let got = entries(&text);
        let seq: Vec<(ChangeArea, String)> = got
            .iter()
            .map(|e| (e.area, e.path.display().to_string()))
            .collect();
        assert_eq!(
            seq,
            vec![
                (ChangeArea::Conflict, "conflict.rs".to_string()),
                (ChangeArea::Staged, "c.rs".to_string()),
                (ChangeArea::Unstaged, "b.rs".to_string()),
                (ChangeArea::Unstaged, "a.rs".to_string()),
                (ChangeArea::Untracked, "zed.txt".to_string()),
                (ChangeArea::Untracked, "alpha.txt".to_string()),
            ]
        );
        assert_eq!(row(&got, ChangeArea::Conflict, "conflict.rs").code, "UU");
        assert_eq!(row(&got, ChangeArea::Untracked, "zed.txt").code, "?");
    }

    #[test]
    fn a_rename_keeps_its_old_path_on_the_staged_side_only() {
        let text = format!("2 RM N... 100644 100644 100644 {A} {A} R100 new.rs\0old.rs\0");
        let got = entries(&text);
        let staged = row(&got, ChangeArea::Staged, "new.rs");
        assert_eq!(staged.code, "R");
        assert_eq!(staged.orig_path.as_deref(), Some(Path::new("old.rs")));
        let unstaged = row(&got, ChangeArea::Unstaged, "new.rs");
        assert_eq!(unstaged.code, "M");
        assert_eq!(unstaged.orig_path, None);
    }

    #[test]
    fn paths_with_spaces_survive() {
        let text = format!(
            "1 .M N... 100644 100644 100644 {A} {A} dir with space/file name.txt\0? other file.txt\0"
        );
        let got = entries(&text);
        assert_eq!(
            row(&got, ChangeArea::Unstaged, "dir with space/file name.txt").code,
            "M"
        );
        assert_eq!(row(&got, ChangeArea::Untracked, "other file.txt").code, "?");
    }

    #[test]
    fn submodule_flags_become_state_on_each_side() {
        let text = format!("1 MM SCMU 160000 160000 160000 {A} {B} lib/sub\0");
        let parsed = parse_porcelain_v2(&text, Path::new(""), 0);
        let staged = parsed
            .iter()
            .find(|p| p.entry.area == ChangeArea::Staged)
            .expect("staged row");
        assert_eq!(
            staged.entry.submodule,
            Some(SubmoduleState {
                commit_changed: true,
                ..SubmoduleState::default()
            })
        );
        assert_eq!(staged.old_sha.as_deref(), Some(A));
        assert_eq!(staged.new_sha.as_deref(), Some(B));
        let unstaged = parsed
            .iter()
            .find(|p| p.entry.area == ChangeArea::Unstaged)
            .expect("unstaged row");
        assert_eq!(
            unstaged.entry.submodule,
            Some(SubmoduleState {
                commit_changed: true,
                modified: true,
                untracked: true,
                ..SubmoduleState::default()
            })
        );
        assert_eq!(unstaged.old_sha.as_deref(), Some(B));
        assert_eq!(unstaged.new_sha, None);
        assert_eq!(unstaged.entry.glyph(), 'S');
    }

    #[test]
    fn a_newly_added_submodule_has_no_commit_to_count_from() {
        let text = format!("1 A. S... 000000 160000 160000 {ZERO} {A} lib/new\0");
        let parsed = parse_porcelain_v2(&text, Path::new(""), 0);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].entry.code, "A");
        assert_eq!(parsed[0].old_sha, None);
        assert!(!parsed[0].entry.submodule.as_ref().unwrap().commit_changed);
    }

    #[test]
    fn dirty_only_submodule_shows_on_the_worktree_side() {
        let text = format!("1 .M S.M. 160000 160000 160000 {A} {A} lib/sub\0");
        let got = entries(&text);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].area, ChangeArea::Unstaged);
        let state = got[0].submodule.clone().unwrap();
        assert!(!state.commit_changed && state.modified && !state.untracked);
        assert!(state.is_dirty());
    }

    #[test]
    fn entries_carry_their_repo_and_depth() {
        let text = format!("1 .M N... 100644 100644 100644 {A} {A} inner.rs\0");
        let got: Vec<StatusEntry> = parse_porcelain_v2(&text, Path::new("lib/sub"), 1)
            .into_iter()
            .map(|p| p.entry)
            .collect();
        assert_eq!(got[0].repo, Path::new("lib/sub"));
        assert_eq!(got[0].depth, 1);
        assert_eq!(got[0].worktree(), Path::new("lib/sub/inner.rs"));
    }

    #[test]
    fn empty_output_is_a_clean_tree() {
        assert!(entries("").is_empty());
    }

    // Real repositories: a superproject with a submodule.

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultRefFormat=files",
                "-c",
                "protocol.file.allow=always",
                "-c",
                "user.name=tuicr",
                "-c",
                "user.email=tuicr@example.com",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// `super/` recording `lib/sub` at its first commit, both clean.
    fn superproject_with_submodule() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        git(&sub, &["init", "-q", "-b", "main"]);
        write(&sub, "a.txt", "one\n");
        git(&sub, &["add", "a.txt"]);
        git(&sub, &["commit", "-q", "-m", "sub base"]);

        let root = dir.path().join("super");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        write(&root, "top.txt", "top\n");
        git(&root, &["add", "top.txt"]);
        git(&root, &["commit", "-q", "-m", "base"]);
        git(
            &root,
            &["submodule", "add", "-q", sub.to_str().unwrap(), "lib/sub"],
        );
        git(&root, &["commit", "-q", "-m", "add submodule"]);
        (dir, root)
    }

    #[test]
    fn clean_superproject_reports_nothing() {
        let (_dir, root) = superproject_with_submodule();
        assert!(working_tree_status(&root).unwrap().is_empty());
    }

    #[test]
    fn dirty_submodule_rows_are_nested_under_the_submodule() {
        let (_dir, root) = superproject_with_submodule();
        let sub = root.join("lib/sub");
        // A new commit inside the submodule, a modified tracked file, and an
        // untracked file: every flag at once.
        write(&sub, "b.txt", "two\n");
        git(&sub, &["add", "b.txt"]);
        git(&sub, &["commit", "-q", "-m", "sub second"]);
        write(&sub, "a.txt", "one changed\n");
        write(&sub, "scratch.txt", "untracked\n");
        write(&root, "top.txt", "top changed\n");

        let got = working_tree_status(&root).unwrap();
        let seq: Vec<(usize, ChangeArea, String)> = got
            .iter()
            .map(|e| (e.depth, e.area, e.worktree().display().to_string()))
            .collect();
        assert_eq!(
            seq,
            vec![
                (0, ChangeArea::Unstaged, "lib/sub".to_string()),
                (1, ChangeArea::Unstaged, "lib/sub/a.txt".to_string()),
                (1, ChangeArea::Untracked, "lib/sub/scratch.txt".to_string()),
                (0, ChangeArea::Unstaged, "top.txt".to_string()),
            ]
        );
        let sub_row = &got[0];
        assert_eq!(
            sub_row.submodule,
            Some(SubmoduleState {
                commit_changed: true,
                modified: true,
                untracked: true,
                ahead: 1,
                behind: 0,
            })
        );
        assert_eq!(got[1].repo, Path::new("lib/sub"));
        assert_eq!(got[1].code, "M");
        assert_eq!(got[2].code, "?");
    }

    #[test]
    fn staging_the_pointer_moves_the_submodule_to_the_staged_area() {
        let (_dir, root) = superproject_with_submodule();
        let sub = root.join("lib/sub");
        write(&sub, "b.txt", "two\n");
        git(&sub, &["add", "b.txt"]);
        git(&sub, &["commit", "-q", "-m", "sub second"]);

        stage_paths(&root, &[PathBuf::from("lib/sub")]).unwrap();
        let got = working_tree_status(&root).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].area, ChangeArea::Staged);
        assert_eq!(got[0].code, "M");
        let state = got[0].submodule.clone().unwrap();
        assert!(state.commit_changed);
        assert_eq!((state.ahead, state.behind), (1, 0));

        unstage_paths(&root, &[PathBuf::from("lib/sub")]).unwrap();
        let got = working_tree_status(&root).unwrap();
        assert_eq!(got[0].area, ChangeArea::Unstaged);
    }

    #[test]
    fn nested_operations_run_inside_the_submodule() {
        let (_dir, root) = superproject_with_submodule();
        let sub = root.join("lib/sub");
        write(&sub, "a.txt", "one changed\n");
        write(&sub, "scratch.txt", "untracked\n");

        stage_paths(&root.join("lib/sub"), &[PathBuf::from("a.txt")]).unwrap();
        let got = working_tree_status(&root).unwrap();
        let nested: Vec<(ChangeArea, String)> = got
            .iter()
            .filter(|e| e.depth == 1)
            .map(|e| (e.area, e.path.display().to_string()))
            .collect();
        assert_eq!(
            nested,
            vec![
                (ChangeArea::Staged, "a.txt".to_string()),
                (ChangeArea::Untracked, "scratch.txt".to_string()),
            ]
        );

        let id = commit(&root.join("lib/sub"), "inside", false).unwrap();
        assert!(!id.is_empty());
        assert_eq!(
            head_commit_message(&root.join("lib/sub")).unwrap().trim(),
            "inside"
        );
        // The superproject now sees a moved pointer plus the untracked file.
        let got = working_tree_status(&root).unwrap();
        let state = got[0].submodule.clone().unwrap();
        assert!(state.commit_changed && state.untracked && !state.modified);
        assert_eq!(state.ahead, 1);

        discard_path(&root.join("lib/sub"), Path::new("scratch.txt")).unwrap();
        assert!(!sub.join("scratch.txt").exists());
    }

    #[test]
    fn submodule_checkout_returns_to_the_recorded_commit() {
        let (_dir, root) = superproject_with_submodule();
        let sub = root.join("lib/sub");
        let recorded = git(&sub, &["rev-parse", "HEAD"]);
        write(&sub, "b.txt", "two\n");
        git(&sub, &["add", "b.txt"]);
        git(&sub, &["commit", "-q", "-m", "sub second"]);
        assert_ne!(git(&sub, &["rev-parse", "HEAD"]), recorded);

        submodule_checkout(&root, Path::new("lib/sub"), false).unwrap();
        assert_eq!(git(&sub, &["rev-parse", "HEAD"]), recorded);
        assert!(working_tree_status(&root).unwrap().is_empty());
    }

    #[test]
    fn discard_handles_tracked_new_and_untracked_files() {
        let (_dir, root) = superproject_with_submodule();
        write(&root, "top.txt", "changed\n");
        write(&root, "new.txt", "new\n");
        git(&root, &["add", "new.txt"]);
        write(&root, "junk/deep.txt", "junk\n");

        discard_path(&root, Path::new("top.txt")).unwrap();
        discard_path(&root, Path::new("new.txt")).unwrap();
        discard_path(&root, Path::new("junk/")).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("top.txt")).unwrap(),
            "top\n"
        );
        assert!(!root.join("new.txt").exists());
        assert!(!root.join("junk").exists());
        assert!(working_tree_status(&root).unwrap().is_empty());
    }

    #[test]
    fn stage_all_and_unstage_all_cover_the_whole_worktree() {
        let (_dir, root) = superproject_with_submodule();
        write(&root, "top.txt", "changed\n");
        write(&root, "new.txt", "new\n");
        stage_all(&root).unwrap();
        let got = working_tree_status(&root).unwrap();
        assert!(got.iter().all(|e| e.area == ChangeArea::Staged), "{got:?}");
        unstage_all(&root).unwrap();
        let got = working_tree_status(&root).unwrap();
        assert!(got.iter().all(|e| e.area != ChangeArea::Staged), "{got:?}");
    }
}
