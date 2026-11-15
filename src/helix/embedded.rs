//! The editor pane backed by Helix: the whole editor, running in this
//! process and drawing into memory, behind [`EditorPane`].
//!
//! Everything Helix-shaped stops here. Input is translated on the way in
//! ([`super::input`]), cells on the way out ([`super::blit`]), and the
//! paths the editor reports are handed on as tuicr spelled them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{KeyEvent, MouseEvent};
use helix_embed as hx;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};

use super::blit::blit;
use super::input;
use super::pane::{
    CursorShape, EditorPane, PaneCursor, PaneFactory, PaneOutcome, PaneStartError, PaneState,
};
use super::setup::RuntimeDirs;

/// The smallest pane the editor is drawn in, whatever its layout: below
/// this there is nothing to work with. A layout with splits needs more
/// (two rows for each view stacked on another, a few columns for each one
/// beside another), which the editor reports itself; see
/// [`HelixPane::smallest_pane`]. Below either the pane shows a notice and
/// the editor keeps the size it had.
const MIN_SIZE: Size = Size {
    width: 20,
    height: 6,
};

/// Longest [`EditorPane::open`] waits for saves the editor still has to
/// carry out before it compares buffers with the disk: a save in flight
/// would look like a file something else wrote.
const SAVE_WAIT_BEFORE_OPEN: Duration = Duration::from_millis(400);

/// How the Helix-backed pane is set up.
#[derive(Debug, Clone)]
pub struct HelixPaneOptions {
    /// Where the editor's queries, themes and grammars are.
    pub runtime: RuntimeDirs,
    /// The Helix `config.toml` to load. `None` is the user's own
    /// (`~/.config/helix/config.toml`); it is only ever read.
    pub config_file: Option<PathBuf>,
    /// Where the editor logs. `None` is Helix's own log file.
    pub log_file: Option<PathBuf>,
    /// Whether the terminal shows 24-bit colour, as far as tuicr knows.
    /// Helix also turns it on from its own config and from `COLORTERM`.
    pub true_color: bool,
    /// Whether the terminal's background is dark; `None` if unknown. It
    /// picks the variant of a Helix theme that has a light and a dark one.
    pub dark: Option<bool>,
}

impl HelixPaneOptions {
    /// The setup for this user: runtime files and the editor's log in
    /// tuicr's own data directory, the user's own Helix config. `None`
    /// when the system gives no data directory.
    pub fn for_user(true_color: bool, dark: Option<bool>) -> Option<Self> {
        let runtime = RuntimeDirs::for_user()?;
        let log_file = runtime.installed.parent().map(|dir| dir.join("helix.log"));
        Some(Self {
            runtime,
            config_file: None,
            log_file,
            true_color,
            dark,
        })
    }
}

/// A factory for [`crate::app::App::install_editor_pane_factory`] that
/// builds the Helix-backed pane.
///
/// Nothing of Helix is touched until the factory runs, which is when a
/// file is first opened in the editor. A failure the user can fix (a Helix
/// config that does not parse) is reported as retryable; once Helix has
/// started to build its editor a failure is final for this process.
pub fn pane_factory(options: HelixPaneOptions) -> PaneFactory {
    Box::new(move |size| {
        let pane = HelixPane::start(&options, size)?;
        Ok(Box::new(pane) as Box<dyn EditorPane>)
    })
}

/// What the editor reported over the calls one call from the host makes,
/// with the paths still as the editor spells them.
#[derive(Debug, Default)]
struct Report {
    redraw: bool,
    close_requested: bool,
    saved: Vec<PathBuf>,
    opened: Vec<PathBuf>,
}

impl Report {
    fn add(&mut self, outcome: hx::Outcome) {
        self.redraw |= outcome.redraw;
        self.close_requested |= outcome.close_requested;
        self.saved.extend(outcome.saved);
        self.opened.extend(outcome.opened);
    }
}

struct HelixPane {
    editor: hx::EmbeddedEditor,
    /// The size the editor is laid out at.
    size: Size,
    /// Where the editor's runtime files are, to look again while they are
    /// not all there.
    runtime: RuntimeDirs,
    /// What the user can do about the editor's setup (no grammars built
    /// yet), on one line. Said in the editor's status line every time a
    /// file is shown, and by the review through [`PaneState::notice`],
    /// until a look at the runtime directory finds nothing missing.
    notice: Option<String>,
    /// What `open` last put in the editor's status line, until the user's
    /// next key. The editor clears that line for reasons of its own (a
    /// language server that finished loading takes whatever is there with
    /// it, a sixth of a second after a file is first opened), so `pump`
    /// puts it back; see [`say_again`].
    said: Option<String>,
    /// tuicr's spelling of a path, by the editor's, for the paths the two
    /// spell differently. The editor normalizes a path it is given
    /// (`a/../b` becomes `b`); the review compares paths as it wrote them.
    spelled: HashMap<PathBuf, PathBuf>,
    /// What the editor reported in a call that hands the host no outcome
    /// (the resize inside `render`), or in an `open` that then failed. It
    /// goes out with the next outcome.
    carried: Report,
}

impl HelixPane {
    fn start(options: &HelixPaneOptions, size: Size) -> Result<Self, PaneStartError> {
        // Without the files the editor still opens, as plain text.
        let notice = options.runtime.prepare();
        if let Some(dir) = options.log_file.as_deref().and_then(Path::parent) {
            // Helix logs nowhere when the directory is missing; that is no
            // reason not to start.
            let _ = std::fs::create_dir_all(dir);
        }
        let size = Size::new(
            size.width.max(MIN_SIZE.width),
            size.height.max(MIN_SIZE.height),
        );
        let editor = hx::EmbeddedEditor::new(hx::EmbedOptions {
            size: (size.width, size.height),
            runtime_dirs: options.runtime.search_path(),
            config_file: options.config_file.clone(),
            log_file: options.log_file.clone(),
            true_color: options.true_color,
            dark: options.dark,
            // The review names files under the repository root as git
            // reports it, with symbolic links resolved. Helix would take
            // the spelling in `$PWD`, and then hold a file the review
            // opens and the same file opened from inside the editor as
            // two buffers when tuicr was started through a link.
            working_dir: std::env::current_dir().ok(),
        })
        .map_err(|err| {
            // Helix builds its editor once per process. An error from
            // before that point (its config does not parse) leaves the
            // chance unused, and the user can fix the file and try again.
            if hx::EmbeddedEditor::can_be_created() {
                PaneStartError::retryable(err)
            } else {
                PaneStartError::fatal(err)
            }
        })?;
        Ok(Self {
            editor,
            size,
            runtime: options.runtime.clone(),
            notice,
            said: None,
            spelled: HashMap::new(),
            carried: Report::default(),
        })
    }

    /// The smallest pane the editor is drawn in right now: [`MIN_SIZE`],
    /// or what its splits need where that is more.
    fn smallest_pane(&self) -> Size {
        let (columns, rows) = self.editor.min_size();
        Size::new(MIN_SIZE.width.max(columns), MIN_SIZE.height.max(rows))
    }

    /// What the pane shows in place of an editor it has no room for.
    fn too_small_notice(area: Rect, needed: Size) -> String {
        if needed == MIN_SIZE {
            return "Too small for the editor. Ctrl-g goes back.".to_string();
        }
        format!(
            "Too small for the editor's windows: {}x{} here, they need {}x{}. \
             Ctrl-w o keeps one, Ctrl-g goes back.",
            area.width, area.height, needed.width, needed.height
        )
    }

    /// Put `message` in the editor's status line, and keep it there until
    /// the user's next key (see [`HelixPane::said`]).
    fn say(&mut self, message: String) -> hx::Outcome {
        let outcome = self.editor.set_status(message.clone());
        self.said = Some(message);
        outcome
    }

    /// The path as tuicr spelled it when it opened the file.
    fn host_path(&self, path: PathBuf) -> PathBuf {
        match self.spelled.get(&path) {
            Some(spelled) => spelled.clone(),
            None => path,
        }
    }

    /// Hand the host what the editor reported, `outcome` included, and
    /// whatever earlier calls left to carry.
    fn report(&mut self, outcome: Option<hx::Outcome>) -> PaneOutcome {
        let mut report = std::mem::take(&mut self.carried);
        if let Some(outcome) = outcome {
            report.add(outcome);
        }
        self.host_outcome(report)
    }

    fn host_outcome(&self, report: Report) -> PaneOutcome {
        let paths = |paths: Vec<PathBuf>| -> Vec<PathBuf> {
            paths.into_iter().map(|path| self.host_path(path)).collect()
        };
        PaneOutcome {
            redraw: report.redraw,
            close_requested: report.close_requested,
            saved: paths(report.saved),
            opened: paths(report.opened),
        }
    }

    /// One line for the editor's status line about what the disk did to
    /// the buffers while the pane was hidden, when the user has to know.
    /// `shown` is the file now in view, as the editor spells it.
    fn disk_notice(reload: &hx::Reload, shown: Option<&Path>) -> Option<String> {
        let name = |path: &Path| match path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => path.display().to_string(),
        };
        if let Some(shown) = shown
            && reload.conflicting.iter().any(|path| path == shown)
        {
            return Some(format!(
                "{} changed on disk; your unsaved changes are kept (:reload reads the file, :w! overwrites it)",
                name(shown)
            ));
        }
        if let Some((path, err)) = reload.failed.first() {
            return Some(format!("{} could not be read again: {err:#}", name(path)));
        }
        if !reload.conflicting.is_empty() {
            let names: Vec<String> = reload.conflicting.iter().map(|path| name(path)).collect();
            return Some(format!(
                "changed on disk under unsaved changes: {}",
                names.join(", ")
            ));
        }
        None
    }
}

impl EditorPane for HelixPane {
    fn open(&mut self, path: &Path, line: Option<usize>) -> anyhow::Result<PaneOutcome> {
        let mut report = std::mem::take(&mut self.carried);

        // The editor's own writes first, so that they are on disk and
        // known to it before the buffers are compared with the files.
        if self.editor.saves_pending() > 0 {
            report.add(self.editor.flush_saves(SAVE_WAIT_BEFORE_OPEN));
        }
        // The pane was hidden, and the review, git or an agent may have
        // rewritten files meanwhile (the merge view writes its picks just
        // before it hands the file over). Helix does not watch its files:
        // every buffer without unsaved changes is read again if its file
        // changed. A buffer with unsaved changes is kept as it is.
        let reload = self.editor.reload_changed();
        let conflict = !reload.conflicting.is_empty() || !reload.failed.is_empty();

        match self.editor.open(path, line, None) {
            Ok(outcome) => report.add(outcome),
            Err(err) => {
                report.add(reload.outcome);
                self.carried = report;
                return Err(err);
            }
        }
        let shown = self.editor.state().path;
        if let Some(shown) = &shown
            && shown != path
        {
            self.spelled.insert(shown.clone(), path.to_path_buf());
        }

        // While something was missing, look again: `tuicr --helix-setup`
        // may have run in another terminal since.
        if self.notice.is_some() {
            self.notice = self.runtime.prepare();
        }
        // tuicr's status line is covered while the editor has the screen,
        // so what the user has to know is said in the editor's.
        let message = if conflict {
            Self::disk_notice(&reload, shown.as_deref())
        } else {
            self.notice.clone()
        };
        report.add(reload.outcome);
        self.said = None;
        if let Some(message) = message {
            report.add(self.say(message));
        }
        Ok(self.host_outcome(report))
    }

    fn handle_key(&mut self, key: KeyEvent) -> PaneOutcome {
        // The editor clears its status line on a key, and so the user has
        // seen what was there.
        self.said = None;
        let outcome =
            input::key_event(key).map(|key| self.editor.handle_event(hx::Event::Key(key)));
        self.report(outcome)
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> PaneOutcome {
        let outcome = input::mouse_event(mouse, area)
            .map(|mouse| self.editor.handle_event(hx::Event::Mouse(mouse)));
        self.report(outcome)
    }

    fn handle_paste(&mut self, text: &str) -> PaneOutcome {
        self.said = None;
        let outcome = self.editor.handle_event(hx::Event::Paste(text.to_string()));
        self.report(Some(outcome))
    }

    /// Passed on as the terminal's own focus events, which is what the
    /// editor's `auto-save` on losing focus listens for. What changed on
    /// disk while the pane was hidden was already dealt with by the `open`
    /// that precedes every gain of focus, for all buffers.
    fn focus_changed(&mut self, focused: bool) -> PaneOutcome {
        let event = if focused {
            hx::Event::FocusGained
        } else {
            // Nobody is looking at the status line of a hidden editor.
            self.said = None;
            hx::Event::FocusLost
        };
        let outcome = self.editor.handle_event(event);
        self.report(Some(outcome))
    }

    /// Helix keeps its own record of the working directory and finds the
    /// workspace of a language server from it, so it has to be told. A
    /// directory it cannot enter leaves it where it was: files still open
    /// by their full paths, only the file picker and new language servers
    /// go on looking at the repository that was left.
    fn working_dir_changed(&mut self, dir: &Path) -> PaneOutcome {
        let outcome = self.editor.set_working_dir(dir).ok();
        self.report(outcome)
    }

    /// The facade's pump returns at once, with one exception that is its
    /// own: the call that finds a write in flight waits for it, at most a
    /// quarter of a second and once per write, so that the review never
    /// reads a file half saved.
    fn pump(&mut self) -> PaneOutcome {
        let mut report = std::mem::take(&mut self.carried);
        report.add(self.editor.pump());
        if let Some(message) = &self.said
            && say_again(self.editor.status())
        {
            report.add(self.editor.set_status(message.clone()));
        }
        self.host_outcome(report)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) -> Option<PaneCursor> {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return None;
        }
        // Asked on every frame: a split the user makes raises it.
        let needed = self.smallest_pane();
        if area.width < needed.width || area.height < needed.height {
            Clear.render(area, buf);
            Paragraph::new(Self::too_small_notice(area, needed))
                .wrap(Wrap { trim: true })
                .render(area, buf);
            return None;
        }
        let size = Size::new(area.width, area.height);
        if size != self.size {
            self.size = size;
            let mut outcome = self.editor.resize(size.width, size.height);
            // The new frame is drawn right here.
            outcome.redraw = false;
            self.carried.add(outcome);
        }
        let frame = self.editor.frame();
        blit(frame.buffer, area, buf);
        let shape = match frame.cursor_kind {
            // Helix paints a block cursor into the cells itself.
            hx::CursorKind::Hidden => return None,
            hx::CursorKind::Block => CursorShape::Block,
            hx::CursorKind::Bar => CursorShape::Bar,
            hx::CursorKind::Underline => CursorShape::Underline,
        };
        let (column, row) = frame.cursor?;
        Some(PaneCursor {
            x: area.x.saturating_add(column),
            y: area.y.saturating_add(row),
            shape,
        })
    }

    fn state(&self) -> PaneState {
        let state = self.editor.state();
        PaneState {
            path: state.path.map(|path| self.host_path(path)),
            line: state.line,
            col: state.col,
            mode: mode_label(state.mode).to_string(),
            modified: state
                .modified
                .into_iter()
                .map(|path| self.host_path(path))
                .collect(),
            modified_scratch: state.modified_scratch,
            pending_writes: self.editor.saves_pending(),
            notice: self.notice.clone(),
        }
    }

    fn shutdown(self: Box<Self>) -> Vec<anyhow::Error> {
        self.editor.shutdown()
    }
}

/// Whether the pane's own message goes back into the editor's status line,
/// which now shows `shown`.
///
/// Only into an empty line: the editor took the message away without the
/// user having pressed a key. A message of the editor's own that replaced
/// it (a save that landed, an error from a language server) is left
/// alone, and the pane's comes back when that one is cleared in turn.
fn say_again(shown: Option<&str>) -> bool {
    shown.is_none()
}

/// The mode as Helix's own status line labels it by default.
fn mode_label(mode: hx::Mode) -> &'static str {
    match mode {
        hx::Mode::Normal => "NOR",
        hx::Mode::Insert => "INS",
        hx::Mode::Select => "SEL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The pane itself is covered by `tests/helix_pane.rs`: Helix can be
    // started once per process, so a test that builds one needs a test
    // binary of its own.

    fn reload(conflicting: &[&str], failed: &[&str]) -> hx::Reload {
        hx::Reload {
            conflicting: conflicting.iter().map(PathBuf::from).collect(),
            failed: failed
                .iter()
                .map(|path| (PathBuf::from(path), anyhow::anyhow!("no such file")))
                .collect(),
            ..hx::Reload::default()
        }
    }

    #[test]
    fn should_say_nothing_when_the_disk_and_the_buffers_agree() {
        assert_eq!(
            HelixPane::disk_notice(&reload(&[], &[]), Some(Path::new("/repo/a.rs"))),
            None
        );
    }

    #[test]
    fn should_tell_the_user_their_unsaved_buffer_was_overtaken_on_disk() {
        let notice = HelixPane::disk_notice(
            &reload(&["/repo/src/a.rs", "/repo/b.rs"], &[]),
            Some(Path::new("/repo/src/a.rs")),
        )
        .expect("the file in view is one of them");

        assert!(notice.starts_with("a.rs changed on disk"), "{notice}");
        assert!(notice.contains("unsaved changes are kept"), "{notice}");
        assert!(notice.contains(":reload"), "{notice}");
        assert!(notice.contains(":w!"), "{notice}");
    }

    #[test]
    fn should_name_the_other_buffers_that_were_overtaken() {
        let notice = HelixPane::disk_notice(
            &reload(&["/repo/src/a.rs", "/repo/b.rs"], &[]),
            Some(Path::new("/repo/c.rs")),
        )
        .expect("two buffers are behind the disk");

        assert_eq!(notice, "changed on disk under unsaved changes: a.rs, b.rs");
    }

    #[test]
    fn should_say_which_file_could_not_be_read_again() {
        let notice = HelixPane::disk_notice(&reload(&[], &["/repo/gone.rs"]), None)
            .expect("a reload failed");

        assert_eq!(notice, "gone.rs could not be read again: no such file");
    }

    #[test]
    fn should_put_its_message_back_only_into_a_status_line_the_editor_cleared() {
        // A language server that finished loading clears the line.
        assert!(say_again(None));
        // Its own message is there, or one of the editor's that matters
        // more right now.
        assert!(!say_again(Some(
            "run `tuicr --helix-setup` once: no syntax highlighting yet"
        )));
        assert!(!say_again(Some("'main.rs' written, 12L 301B")));
    }

    #[test]
    fn should_say_what_the_splits_need_when_the_pane_is_too_small_for_them() {
        let area = Rect::new(0, 0, 100, 6);
        assert_eq!(
            HelixPane::too_small_notice(area, MIN_SIZE),
            "Too small for the editor. Ctrl-g goes back."
        );
        let notice = HelixPane::too_small_notice(area, Size::new(20, 7));
        assert!(notice.contains("100x6 here"), "{notice}");
        assert!(notice.contains("need 20x7"), "{notice}");
        assert!(notice.contains("Ctrl-g goes back"), "{notice}");
    }

    #[test]
    fn should_label_modes_as_helix_does() {
        assert_eq!(mode_label(hx::Mode::Normal), "NOR");
        assert_eq!(mode_label(hx::Mode::Insert), "INS");
        assert_eq!(mode_label(hx::Mode::Select), "SEL");
    }
}
