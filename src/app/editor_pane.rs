//! Editor mode: the embedded editor pane as part of the review.
//!
//! `InputMode::Editor` gives the whole screen and all input to the
//! [`EditorPane`](crate::helix::EditorPane) held here. The pane is built on
//! first use and then kept for the life of the process; leaving the mode
//! only hides it. Like the `$EDITOR` hand-off, nothing here touches the
//! terminal: `main.rs` routes events in and writes the cursor style out.
//!
//! One chord stays tuicr's while the pane has focus: `Ctrl-g` goes back to
//! the review. Everything else, `Ctrl-C` and `Ctrl-l` included, is the
//! editor's.
//!
//! Also here, because both editors share them: where a file the review
//! asks to open goes ([`App::route_editor_target`]: the pane, or `$EDITOR`
//! as before), what the review does when either gives the screen back
//! ([`App::after_editor_returned`]), and the hold on quitting while the
//! pane has unsaved buffers ([`App::quit_blocked_by_editor`]).
//!
//! A panic inside the editor ends the editor, not the review: the pane
//! slot catches it, and [`App::finish_editor_pane_return`] goes back to
//! the review and says what was lost.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::cursor::SetCursorStyle;
use crossterm::event::{Event, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect, Size};

use super::editor_target::is_snapshot;
use super::{App, DiffSource, InputMode, MergeView, MessageType};
use crate::editor::{EditorCommand, EditorSurface, EditorTarget};
use crate::helix::{
    CursorShape, EditorPane, PaneCursor, PaneFactory, PaneOutcome, PaneSlot, PaneState,
    is_return_chord,
};

/// Longest the review waits for saves the pane says it has started
/// ([`PaneState::pending_writes`]) before it reloads on leaving the editor,
/// and before it answers a quit. A save that takes longer still reaches the
/// review when the pane reports it.
const PENDING_WRITE_WAIT: Duration = Duration::from_millis(400);

/// Pause between pumps while waiting for those saves: the write itself runs
/// on the editor's own threads.
const PENDING_WRITE_STEP: Duration = Duration::from_millis(2);

/// The app's side of the editor pane: the pane itself and what ties it to
/// the review.
pub struct EditorPaneHost {
    slot: PaneSlot,
    /// Whether files open in the pane at all: config `embedded_editor`,
    /// `:set embedded_editor`. On unless turned off.
    enabled: bool,
    /// `:q!` was given: unsaved buffers no longer hold the quit back.
    quit_forced: bool,
    /// Mode to go back to when the editor is left.
    return_mode: InputMode,
    /// What Editor mode was last opened on.
    target: Option<EditorTarget>,
    /// Editor mode was left since [`App::take_editor_pane_return`].
    returned: bool,
    /// What the pane was last told about focus.
    focused: bool,
    /// Where the pane was last drawn, in screen coordinates; empty until
    /// the first frame. Mouse events are handed over with it.
    area: Rect,
    /// Cursor the last frame asked for.
    cursor: Option<PaneCursor>,
    /// Cursor shape last written to the terminal; `None` is the terminal's
    /// own.
    applied_cursor: Option<CursorShape>,
    /// Files the pane reported since [`App::take_editor_pane_files`].
    files: EditorPaneFiles,
    /// Files the pane was given or opened whose first save may still be to
    /// come: their local-edit baselines are kept until the pane no longer
    /// holds them unsaved.
    unsettled: Vec<PathBuf>,
    /// Every file the pane was given or said it opened. It keeps them
    /// loaded while hidden and reports each only once, so any of them can
    /// be edited on a later visit without another word from the pane.
    held: Vec<PathBuf>,
    /// Saves the pane reported while hidden that the review has not caught
    /// up with: a `:w` typed just before `Ctrl-g` is written a few pumps
    /// after the review reloaded.
    late_saves: Vec<PathBuf>,
    /// How long to wait for writes in flight; [`PENDING_WRITE_WAIT`]
    /// outside tests.
    pub(in crate::app) write_wait: Duration,
    /// A quit that waits for the editor to finish writing: asked for while
    /// every unsaved buffer had a save in flight that outlasted
    /// `write_wait` (a formatter that takes its time). Carried out when the
    /// saves have landed, dropped by the user's next key.
    deferred_quit: Option<DeferredQuit>,
    /// The editor crashed, and the status line has not said so yet.
    crash_note: Option<String>,
}

/// What a quit that waits for the editor's writes does once they landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredQuit {
    /// Quit.
    Plain,
    /// [`App::discard_session_and_quit`], which is what was asked.
    DiscardSession,
}

impl Default for EditorPaneHost {
    fn default() -> Self {
        Self {
            slot: PaneSlot::default(),
            enabled: true,
            quit_forced: false,
            return_mode: InputMode::Normal,
            target: None,
            returned: false,
            focused: false,
            area: Rect::default(),
            cursor: None,
            applied_cursor: None,
            files: EditorPaneFiles::default(),
            unsettled: Vec::new(),
            held: Vec::new(),
            late_saves: Vec::new(),
            write_wait: PENDING_WRITE_WAIT,
            deferred_quit: None,
            crash_note: None,
        }
    }
}

impl EditorPaneHost {
    /// Take the process's one pane over from the host of the app that the
    /// repo switcher is replacing (`App::carry_process_state_from`).
    ///
    /// The editor can be built once per process, so it does not go with
    /// the repository it was first opened in: its buffers, unsaved ones
    /// included, and its language servers stay, and a quit is still held
    /// back by what it has not saved. With the pane come the things that
    /// are true of the pane and not of a review: the files it holds (each
    /// is reported once, when loaded), what it was last told about focus,
    /// where it was last drawn, the cursor shape left on the terminal, and
    /// a crash nobody has been told about. What ties it to the review that
    /// is being left (the target, late saves, a quit put off) is dropped
    /// with that review, and whether files open in it at all is the new
    /// app's own setting again.
    ///
    /// `old` is left with an empty slot, so dropping it ends nothing.
    pub(in crate::app) fn carry_pane_from(&mut self, old: &mut EditorPaneHost) {
        std::mem::swap(&mut self.slot, &mut old.slot);
        self.held = std::mem::take(&mut old.held);
        self.focused = old.focused;
        self.area = old.area;
        self.applied_cursor = old.applied_cursor.take();
        self.crash_note = old.crash_note.take();
    }
}

impl Drop for EditorPaneHost {
    /// The last chance to end the pane properly: flush the saves it has in
    /// flight and stop its language servers.
    ///
    /// On the normal way out the event loop has already called
    /// [`App::shutdown_editor_pane`] and reported what failed, and this
    /// finds nothing to do. It is for every other way out: an I/O error
    /// returning from the middle of the loop would otherwise drop the pane
    /// as it stands, and the editor behind it stops without flushing.
    ///
    /// Not while panicking: the pane may be what panicked.
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        for err in self.slot.shutdown() {
            eprintln!("Warning: embedded editor: {err:#}");
        }
    }
}

/// Files the pane reported touching, each listed once, oldest first.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EditorPaneFiles {
    /// Written by the editor. Not every write is reported, so treat this as
    /// a hint and the file system as the truth.
    pub saved: Vec<PathBuf>,
    /// Opened in the editor, by request or by navigation.
    pub opened: Vec<PathBuf>,
}

impl EditorPaneFiles {
    pub fn is_empty(&self) -> bool {
        self.saved.is_empty() && self.opened.is_empty()
    }
}

/// Where the event loop sends a file the review asked to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorRoute {
    /// The embedded editor opened it and has the screen. The review is
    /// brought up to date when it gives the screen back
    /// ([`App::finish_editor_pane_return`]).
    Embedded,
    /// `$EDITOR` gets it, exactly as before there was an embedded editor.
    External {
        /// One line for the status bar saying why the embedded editor was
        /// not used. `None` when there is nothing to explain: this tuicr
        /// has no embedded editor (nothing ever said how to build one), or
        /// `$EDITOR` is what the user chose (the option is off, or
        /// `$EDITOR` opens its own window). The status line is then
        /// exactly what it was before there was an embedded editor.
        reason: Option<String>,
    },
}

fn push_unique(list: &mut Vec<PathBuf>, path: PathBuf) {
    if !list.contains(&path) {
        list.push(path);
    }
}

/// An editor target for a file known only by its path.
fn path_target(path: PathBuf) -> EditorTarget {
    EditorTarget {
        path,
        line: None,
        label: String::new(),
    }
}

/// Status-line wording for a quit held back by unsaved editor buffers.
/// The line is clipped at the right edge, so the names come last.
fn unsaved_buffers_message(state: &PaneState) -> String {
    const NAMED: usize = 3;
    let count = state.modified.len() + state.modified_scratch;
    let mut names: Vec<String> = state
        .modified
        .iter()
        .take(NAMED)
        .map(|path| match path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => path.display().to_string(),
        })
        .collect();
    if state.modified.len() > NAMED {
        names.push(format!("{} more", state.modified.len() - NAMED));
    }
    if state.modified_scratch > 0 {
        names.push(format!("{} scratch", state.modified_scratch));
    }
    let buffers = if count == 1 { "buffer" } else { "buffers" };
    format!(
        "{count} unsaved editor {buffers} (:q! quits anyway, e reopens the editor): {}",
        names.join(", ")
    )
}

/// Status-line wording for a quit that waits for the editor's writes.
fn waiting_for_writes_message(state: &PaneState) -> String {
    let names: Vec<String> = state
        .modified
        .iter()
        .map(|path| match path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => path.display().to_string(),
        })
        .collect();
    format!(
        "Quitting when the editor has written {} (any key cancels)",
        names.join(", ")
    )
}

/// Status-line wording after the embedded editor crashed.
fn crash_message(panic: &str) -> String {
    format!(
        "The embedded editor crashed and was closed; changes it had not saved are lost. \
         Files open in $EDITOR until tuicr is restarted. It {panic} (more in panic.log)"
    )
}

impl App {
    /// Say how the embedded editor is built. Call once at startup; the pane
    /// itself is only built when a file is first opened in it.
    ///
    /// Returns `false`, changing nothing, once a pane exists, failed to
    /// start for good, or was shut down: the editor behind it can be
    /// constructed once per process.
    pub fn install_editor_pane_factory(&mut self, factory: PaneFactory) -> bool {
        self.editor_pane.slot.install(factory)
    }

    /// Whether this tuicr has an embedded editor at all: something said
    /// how to build one, whatever became of it since. Without one the
    /// palette does not list the editor's own actions.
    pub fn editor_pane_installed(&self) -> bool {
        self.editor_pane.slot.was_installed()
    }

    /// Whether opening a file in the pane can still work: it exists, or it
    /// has a factory that may still build it.
    pub fn editor_pane_available(&self) -> bool {
        self.editor_pane.slot.is_available()
    }

    /// Why a file cannot be opened in the embedded editor, when it cannot:
    /// wording for the status line when falling back to `$EDITOR`.
    pub fn editor_pane_unavailable_reason(&self) -> Option<&str> {
        self.editor_pane.slot.unavailable_reason()
    }

    /// Whether the pane has the screen and the keyboard.
    pub fn editor_pane_focused(&self) -> bool {
        self.input_mode == InputMode::Editor
    }

    /// What the editor is showing and what it has not saved; `None` until
    /// the pane has been built.
    pub fn editor_pane_state(&self) -> Option<PaneState> {
        self.editor_pane.slot.get().map(EditorPane::state)
    }

    /// Files the pane has saved or opened since the last call.
    pub fn take_editor_pane_files(&mut self) -> EditorPaneFiles {
        std::mem::take(&mut self.editor_pane.files)
    }

    /// `Some`, once, after Editor mode was left by any route (`Ctrl-g`, the
    /// editor closing its last view, another view taking the screen): the
    /// target it was last opened on. This is the moment to refresh the
    /// review from disk, as after `$EDITOR` returns.
    pub fn take_editor_pane_return(&mut self) -> Option<EditorTarget> {
        if !std::mem::take(&mut self.editor_pane.returned) {
            return None;
        }
        self.editor_pane.target.clone()
    }

    /// Open `target` in the embedded editor and give it the screen.
    ///
    /// `screen` is the terminal size, used only to build the pane the first
    /// time; after that the pane sizes itself to whatever it is drawn in.
    ///
    /// On `Err` nothing changed and the text says why: there is no pane
    /// (see [`App::editor_pane_unavailable_reason`]) or it could not open
    /// the file. The caller falls back to `$EDITOR` and reports the reason.
    pub fn open_editor_pane(&mut self, target: &EditorTarget, screen: Size) -> Result<(), String> {
        // Back to work in the editor: the quit that was waiting is off.
        self.editor_pane.deferred_quit = None;
        let pane = self.editor_pane.slot.get_or_create(screen)?;
        let line = target.line.map(|line| line as usize);
        let outcome = pane
            .open(&target.path, line)
            .map_err(|err| format!("{err:#}"))?;
        if self.input_mode != InputMode::Editor {
            // Before the mode changes: baselines are not taken over the
            // merge view, which is told apart by the mode.
            self.rebaseline_held_files();
            self.editor_pane.return_mode = self.input_mode;
            self.input_mode = InputMode::Editor;
        }
        self.editor_pane.target = Some(target.clone());
        push_unique(&mut self.editor_pane.held, target.path.clone());
        push_unique(&mut self.editor_pane.unsettled, target.path.clone());
        self.sync_editor_pane_focus();
        self.absorb_pane_outcome(outcome);
        Ok(())
    }

    /// Take a local-edit baseline again for every file the pane holds, as
    /// Editor mode is entered.
    ///
    /// A file left as it was stops being tracked when the editor is left,
    /// so that later changes by other tools are not tinted as edits. The
    /// pane still has it loaded, though, and reports a file as opened only
    /// the first time: without this, editing it on a later visit (switching
    /// to its buffer, following a definition into it again) would show no
    /// tint. A file that kept its baseline keeps it; the others are
    /// measured from how this visit found them.
    fn rebaseline_held_files(&mut self) {
        for path in self.editor_pane.held.clone() {
            self.snapshot_local_edit_baseline(&path_target(path.clone()));
            push_unique(&mut self.editor_pane.unsettled, path);
        }
    }

    /// Whether files the review opens go to the embedded editor (when
    /// there is one) and not to `$EDITOR`.
    pub fn embedded_editor_enabled(&self) -> bool {
        self.editor_pane.enabled
    }

    /// Apply the `embedded_editor` config key at startup. Unlike
    /// [`App::set_embedded_editor`] this leaves no status message behind.
    pub fn init_embedded_editor(&mut self, enabled: bool) {
        self.editor_pane.enabled = enabled;
    }

    /// `:set embedded_editor` / `noembedded_editor` / `embedded_editor!`
    /// (`None` toggles). Session-only, like every `:set`: `config.toml` is
    /// not written. Turning it off closes nothing: buffers the pane already
    /// holds stay open, and still hold a quit back while unsaved.
    pub fn set_embedded_editor(&mut self, enabled: Option<bool>) {
        let enabled = enabled.unwrap_or(!self.editor_pane.enabled);
        self.editor_pane.enabled = enabled;
        let unavailable = self
            .editor_pane
            .slot
            .unavailable_reason()
            .map(str::to_string);
        match (enabled, unavailable) {
            (false, _) => self.set_message("Embedded editor off: files open in $EDITOR"),
            (true, None) => {
                self.set_message("Embedded editor on: files open inside tuicr (Ctrl-g returns)");
            }
            (true, Some(reason)) => {
                self.set_warning(format!("Embedded editor on, but not usable: {reason}"));
            }
        }
    }

    /// Decide where `target` opens, and open it when that is the embedded
    /// editor.
    ///
    /// `external` is the `$EDITOR` invocation the target gets otherwise;
    /// `screen` is the terminal size, used only to build the pane the first
    /// time. The caller has already kept the file's local-edit baseline.
    ///
    /// `$EDITOR` keeps the file when the embedded editor is turned off,
    /// when tuicr runs with `--stdout`, when the target is a read-only PR
    /// snapshot, when `$EDITOR` is a windowed editor (tuicr stays on screen
    /// beside it, which the pane cannot better), and when the pane cannot
    /// start or cannot open the file. None of those builds a pane that
    /// does not exist yet.
    ///
    /// A reason comes with it only when the embedded editor was wanted and
    /// something stood in its way. Turning the option off and setting a
    /// windowed `$EDITOR` are the user's own choices: saying so after every
    /// edit would be noise, and would push the edit count toward the edge
    /// the status line is clipped at.
    pub fn route_editor_target(
        &mut self,
        target: &EditorTarget,
        external: &EditorCommand,
        screen: Size,
    ) -> EditorRoute {
        if !self.editor_pane.slot.was_installed()
            || !self.editor_pane.enabled
            || external.surface() == EditorSurface::Gui
        {
            return EditorRoute::External { reason: None };
        }
        let kept = if self.output_to_stdout {
            Some("embedded editor is not used with --stdout".to_string())
        } else if is_snapshot(&target.path) {
            // The pane has no way to open a file read-only, and a snapshot
            // is a copy nobody should be invited to edit.
            Some("read-only PR copies open in $EDITOR".to_string())
        } else {
            None
        };
        let reason = match kept {
            Some(reason) => reason,
            None => match self.open_editor_pane(target, screen) {
                Ok(()) => return EditorRoute::Embedded,
                Err(reason) => format!("embedded editor not used: {reason}"),
            },
        };
        EditorRoute::External {
            reason: Some(reason),
        }
    }

    /// Add why the embedded editor was not used to what the status line
    /// says about the `$EDITOR` hand-off that just ended. An error or
    /// warning about the hand-off itself is left alone: it says more.
    pub fn note_editor_fallback(&mut self, reason: &str) {
        match &mut self.message {
            Some(message) if message.message_type == MessageType::Info => {
                message.content.push_str(" \u{00b7} ");
                message.content.push_str(reason);
            }
            Some(_) => {}
            None => self.set_message(reason),
        }
    }

    /// Bring the review up to date after an editor was used on `target`:
    /// `$EDITOR` exiting, or the embedded editor giving the screen back.
    ///
    /// The disk is the authority either way, because neither editor
    /// reports every write. The merge view re-parses its file, a review
    /// that includes the working tree reloads its diff, and any other
    /// review only recomputes which lines were edited. The status line
    /// says what was opened and what changed.
    pub fn after_editor_returned(&mut self, target: &EditorTarget) {
        self.refresh_after_editor(target, "Opened");
    }

    /// [`App::after_editor_returned`], with the word the status line starts
    /// on: `Opened` when an editor gave the screen back, `Saved` when the
    /// embedded editor finished a write after it already had.
    fn refresh_after_editor(&mut self, target: &EditorTarget, verb: &str) {
        // A buffer the embedded editor still holds unsaved may be written
        // later, so its baseline has to wait for that save.
        if !self.editor_pane_holds_unsaved(&target.path) {
            self.local_edit_editor_returned(target);
        }
        if self.input_mode == InputMode::MergeConflict {
            // Merge-view edit: re-parse the file so hand-resolved markers
            // disappear and remaining ones become conflicts again.
            self.merge_reload_current_file();
        } else if self.diff_source.includes_worktree_changes() {
            match self.reload_diff_files() {
                Ok((count, invalidated)) => {
                    let invalidated_suffix = if invalidated > 0 {
                        format!(", {invalidated} changed since last review")
                    } else {
                        String::new()
                    };
                    let edits_suffix = self
                        .local_edit_summary(target)
                        .map(|summary| format!(" \u{00b7} {summary}"))
                        .unwrap_or_default();
                    self.set_message(format!(
                        "{verb} {} and reloaded {count} files{invalidated_suffix}{edits_suffix}",
                        target.label
                    ));
                }
                Err(err) => {
                    self.set_error(format!("Reload after editor failed: {err}"));
                }
            }
        } else {
            // Nothing reloads here, so recompute the edited lines for the
            // message by hand.
            self.refresh_local_edits();
            let message = match self.local_edit_summary(target) {
                Some(summary) if matches!(self.diff_source, DiffSource::CommitRange(_)) => {
                    format!(
                        "{verb} {} \u{00b7} {summary}; :local compares the review with your working tree",
                        target.label
                    )
                }
                Some(summary) => format!("{verb} {} \u{00b7} {summary}", target.label),
                None => format!("{verb} {}", target.label),
            };
            self.set_message(message);
        }
    }

    /// Refresh the review if Editor mode was left since the last call, by
    /// `Ctrl-g`, by the editor closing its last view, or by another view
    /// taking the screen; or if the hidden editor has since reported a
    /// save. Returns whether it did, which needs a repaint.
    ///
    /// The event loop calls this after every event the pane took and after
    /// every [`App::poll_editor_pane`]. It runs on every return, whether or
    /// not the pane reported a save: `:wq` writes without reporting one.
    ///
    /// A `:w` is written in the background, so `Ctrl-g` right behind it
    /// gets here before the file changed. Writes the pane says are in
    /// flight are waited for, briefly, so one reload shows them; a save
    /// that lands later (or that the pane could not announce) is caught
    /// when it is reported, on a later call.
    ///
    /// It is also where a crash of the editor is noticed
    /// ([`PaneSlot::take_crash`]): Editor mode is left, the review is
    /// refreshed like after any other return (the editor may have saved
    /// before it went down), and the status line says what happened.
    pub fn finish_editor_pane_return(&mut self) -> bool {
        self.note_editor_pane_crash();
        let refreshed = match self.take_editor_pane_return() {
            Some(target) => {
                self.settle_editor_pane_writes();
                // Whatever landed while waiting is on disk for the refresh
                // below.
                self.editor_pane.late_saves.clear();
                self.settle_editor_baselines();
                self.after_editor_returned(&target);
                self.note_editor_setup();
                true
            }
            None => self.finish_late_editor_saves(),
        };
        // Last, so that the refresh's own message does not replace it.
        match self.editor_pane.crash_note.take() {
            Some(note) => {
                self.set_error(note);
                true
            }
            None => refreshed,
        }
    }

    /// Take in that a call into the pane panicked, if one did: the pane is
    /// gone, with everything it had not saved.
    fn note_editor_pane_crash(&mut self) {
        let Some(panic) = self.editor_pane.slot.take_crash() else {
            return;
        };
        self.leave_editor_mode();
        // Nothing is held any more, so nothing waits for a save, and a
        // quit that waited for one would now go ahead without a word
        // about the crash.
        self.editor_pane.held.clear();
        self.editor_pane.deferred_quit = None;
        self.editor_pane.crash_note = Some(crash_message(&panic));
    }

    /// Add what the pane says about its setup (no grammars built yet) to
    /// the status line of a return to the review. The editor says it in
    /// its own status line too, but clears that line for its own reasons;
    /// here it stays until the next message.
    fn note_editor_setup(&mut self) {
        let Some(notice) = self.editor_pane_state().and_then(|state| state.notice) else {
            return;
        };
        self.note_editor_fallback(&notice);
    }

    /// Catch the review up with saves the editor reported after it was
    /// left. Returns whether the review changed.
    ///
    /// Like the other background refreshes it waits while the user is in
    /// the middle of something else (a comment, a prompt): it runs in the
    /// review proper, the merge view, or the view the editor went back to,
    /// where the refresh on leaving already ran.
    fn finish_late_editor_saves(&mut self) -> bool {
        if self.editor_pane.late_saves.is_empty() {
            return false;
        }
        if self.input_mode == InputMode::Editor {
            // Back in the editor: leaving it refreshes everything.
            self.editor_pane.late_saves.clear();
            return false;
        }
        let fits = matches!(
            self.input_mode,
            InputMode::Normal | InputMode::MergeConflict
        ) || self.input_mode == self.editor_pane.return_mode;
        if !fits {
            return false;
        }
        let saved = std::mem::take(&mut self.editor_pane.late_saves);
        self.settle_editor_baselines();
        if self.input_mode == InputMode::MergeConflict {
            // Re-parsing drops the picks made in the view, so only the file
            // on screen is re-read, and only when it is the one written.
            let shown = self
                .merge_view
                .as_ref()
                .and_then(MergeView::current_file_state)
                .map(|file| self.vcs_info.root_path.join(&file.path));
            if !shown.is_some_and(|path| saved.contains(&path)) {
                return false;
            }
            self.merge_reload_current_file();
            return true;
        }
        // Name the file the user asked for when it is among them.
        let target = match self.editor_pane.target.clone() {
            Some(target) if saved.contains(&target.path) => target,
            _ => self.saved_file_target(saved[0].clone()),
        };
        self.refresh_after_editor(&target, "Saved");
        true
    }

    /// An editor target for a file the pane wrote, labelled by its path in
    /// the repository as the review's own targets are.
    fn saved_file_target(&self, path: PathBuf) -> EditorTarget {
        let label = path
            .strip_prefix(&self.vcs_info.root_path)
            .unwrap_or(&path)
            .display()
            .to_string();
        EditorTarget {
            path,
            line: None,
            label,
        }
    }

    /// Files the editor is done with stop waiting for a first save; the
    /// next refresh then forgets the ones left as they were.
    fn settle_editor_baselines(&mut self) {
        let unsaved = self
            .editor_pane_state()
            .map(|state| state.modified)
            .unwrap_or_default();
        let (waiting, settled): (Vec<_>, Vec<_>) = std::mem::take(&mut self.editor_pane.unsettled)
            .into_iter()
            .partition(|path| unsaved.contains(path));
        self.editor_pane.unsettled = waiting;
        for path in settled {
            self.local_edit_editor_returned(&path_target(path));
        }
    }

    /// Pump the pane until the saves it says it has started
    /// ([`PaneState::pending_writes`]) have finished, or the wait runs out.
    /// Returns at once when there are none, which is also what a pane that
    /// cannot count them reports.
    fn settle_editor_pane_writes(&mut self) {
        let deadline = Instant::now() + self.editor_pane.write_wait;
        loop {
            let pending = self
                .editor_pane_state()
                .map_or(0, |state| state.pending_writes);
            if pending == 0 || Instant::now() >= deadline {
                return;
            }
            let Some(pane) = self.editor_pane.slot.get_mut() else {
                return;
            };
            let outcome = pane.pump();
            self.absorb_pane_outcome(outcome);
            std::thread::sleep(PENDING_WRITE_STEP);
        }
    }

    /// `:q!`: quit without asking the embedded editor about unsaved
    /// buffers. What they hold is lost.
    pub fn force_quit(&mut self) {
        self.editor_pane.quit_forced = true;
        self.should_quit = true;
    }

    /// Hold a quit back while the embedded editor has unsaved buffers.
    ///
    /// Returns `true` after taking the quit request back and saying in the
    /// status line how many buffers are unsaved. `:q!`
    /// ([`App::force_quit`]) is never held back. The event loop asks before
    /// acting on `should_quit`, which covers every way of quitting.
    ///
    /// A buffer whose `:w` is still being written counts as unsaved until
    /// the write lands. It gets a moment (`write_wait`); when that is not
    /// enough (a formatter that takes a second or two runs before the
    /// write) and nothing else is unsaved, the quit is not refused as
    /// "unsaved", which it is not, but put off: the status line says what
    /// is being written, and [`App::poll_editor_pane`] carries the quit
    /// out when the pane reports the saves. Any key takes it back
    /// ([`App::cancel_deferred_quit`]).
    pub fn quit_blocked_by_editor(&mut self) -> bool {
        if self.editor_pane.quit_forced {
            return false;
        }
        self.settle_editor_pane_writes();
        let Some(state) = self.editor_pane_state() else {
            return false;
        };
        if state.modified.is_empty() && state.modified_scratch == 0 {
            return false;
        }
        self.should_quit = false;
        // Every unsaved buffer can be one of those being written. Which
        // ones are, the pane does not say; the check when the writes have
        // landed settles it.
        let only_writes_in_flight = state.modified_scratch == 0
            && state.pending_writes > 0
            && state.modified.len() <= state.pending_writes;
        if only_writes_in_flight {
            self.editor_pane.deferred_quit = Some(DeferredQuit::Plain);
            self.set_sticky_warning(waiting_for_writes_message(&state));
        } else {
            self.editor_pane.deferred_quit = None;
            self.set_error(unsaved_buffers_message(&state));
        }
        true
    }

    /// Whether a quit is waiting for the editor to finish writing.
    pub fn quit_deferred(&self) -> bool {
        self.editor_pane.deferred_quit.is_some()
    }

    /// The quit that is waiting for the editor's writes is to discard the
    /// session first: [`App::discard_session_and_quit`] asked.
    pub(in crate::app) fn defer_session_discard(&mut self) {
        if self.editor_pane.deferred_quit.is_some() {
            self.editor_pane.deferred_quit = Some(DeferredQuit::DiscardSession);
        }
    }

    /// The user went on working: a quit that was waiting for the editor's
    /// writes is off. The event loop calls this for every key and paste.
    /// Returns whether there was one.
    pub fn cancel_deferred_quit(&mut self) -> bool {
        if self.editor_pane.deferred_quit.take().is_none() {
            return false;
        }
        self.set_message("Quit cancelled");
        true
    }

    /// Carry out a quit that waited for the editor's writes, once none is
    /// in flight. Returns whether the status line or the quit flag changed.
    fn finish_deferred_quit(&mut self) -> bool {
        let Some(deferred) = self.editor_pane.deferred_quit else {
            return false;
        };
        let Some(state) = self.editor_pane_state() else {
            self.editor_pane.deferred_quit = None;
            return false;
        };
        if state.pending_writes > 0 {
            return false;
        }
        self.editor_pane.deferred_quit = None;
        if !state.modified.is_empty() || state.modified_scratch > 0 {
            // The write failed (the editor's status line says why), or
            // the buffer was changed again behind it.
            self.set_error(unsaved_buffers_message(&state));
            return true;
        }
        match deferred {
            DeferredQuit::Plain => self.should_quit = true,
            DeferredQuit::DiscardSession => self.discard_session_and_quit(),
        }
        true
    }

    /// The event loop's question once a quit has been asked for: does it go
    /// ahead?
    ///
    /// `false` when [`App::quit_blocked_by_editor`] held it back. The
    /// refusal is then in the status line, and `ctrl_c_timer` is stopped: a
    /// second `Ctrl-C` that asked for this quit leaves the two-second timer
    /// of the first running, and the loop clears the status line when that
    /// timer runs out, which took the refusal away with tuicr still up and
    /// nothing on screen to say why.
    pub fn quit_goes_ahead(&mut self, ctrl_c_timer: &mut Option<Instant>) -> bool {
        if !self.quit_blocked_by_editor() {
            return true;
        }
        *ctrl_c_timer = None;
        false
    }

    /// Whether the pane holds `path` with changes it has not written.
    fn editor_pane_holds_unsaved(&self, path: &Path) -> bool {
        self.editor_pane
            .slot
            .get()
            .is_some_and(|pane| pane.state().modified.iter().any(|held| held == path))
    }

    /// Go back to the review. The pane keeps its buffers and is only
    /// hidden. A no-op outside Editor mode.
    pub fn leave_editor_mode(&mut self) {
        if self.input_mode != InputMode::Editor {
            return;
        }
        let back = self.editor_pane.return_mode;
        // The merge view can end while the editor covers it.
        self.input_mode = if back == InputMode::MergeConflict && self.merge_view.is_none() {
            InputMode::Normal
        } else {
            back
        };
        self.sync_editor_pane_focus();
    }

    /// Whether `event` belongs to the editor pane and not to tuicr.
    ///
    /// While the pane has focus that is every key, mouse and paste event.
    /// The event loop asks this before any of its own key handling, so
    /// `Ctrl-C`, `Ctrl-l` and the prefix keys reach the editor. Resizes
    /// stay with tuicr; the pane picks the new size up when it is drawn.
    pub fn editor_pane_takes(&self, event: &Event) -> bool {
        self.input_mode == InputMode::Editor
            && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
    }

    /// Hand the pane an event [`App::editor_pane_takes`] claimed. Returns
    /// whether the screen needs repainting.
    ///
    /// Key releases are dropped, as everywhere else in tuicr. `Ctrl-g` is
    /// never forwarded: it leaves Editor mode.
    pub fn editor_pane_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Release => false,
            Event::Key(key) if is_return_chord(&key) => {
                self.leave_editor_mode();
                true
            }
            Event::Key(key) => self.drive_editor_pane(|pane| pane.handle_key(key)),
            Event::Mouse(mouse) => {
                let area = self.editor_pane.area;
                // Nothing has been drawn yet, so there is nothing to hit.
                if area.is_empty() {
                    return false;
                }
                self.drive_editor_pane(|pane| pane.handle_mouse(mouse, area))
            }
            Event::Paste(text) => self.drive_editor_pane(|pane| pane.handle_paste(&text)),
            _ => false,
        }
    }

    /// Let the pane's background work make progress. Runs on every pass of
    /// the event loop, shown or hidden, once a pane exists.
    ///
    /// Returns whether the screen needs repainting: a hidden pane's new
    /// frame does not count.
    pub fn poll_editor_pane(&mut self) -> bool {
        // Something other than `Ctrl-g` may have changed the mode under the
        // pane (a background result opening another view).
        let mut redraw = self.sync_editor_pane_focus();
        let Some(pane) = self.editor_pane.slot.get_mut() else {
            if self.input_mode == InputMode::Editor {
                // Editor mode with nothing to show or type into.
                self.leave_editor_mode();
                return true;
            }
            return redraw;
        };
        let outcome = pane.pump();
        redraw |= self.absorb_pane_outcome(outcome);
        // Before the quit that may be waiting: a pane that just crashed
        // reports nothing unsaved and nothing in flight.
        self.note_editor_pane_crash();
        redraw |= self.finish_deferred_quit();
        redraw
    }

    /// Draw the pane into `area`, resizing it to fit, and remember where it
    /// went. Returns the cursor to show, already checked to be inside
    /// `area`; `None` hides it. Draws nothing when there is no pane.
    pub(crate) fn draw_editor_pane(&mut self, area: Rect, buf: &mut Buffer) -> Option<PaneCursor> {
        self.editor_pane.area = area;
        self.editor_pane.cursor = self
            .editor_pane
            .slot
            .get_mut()
            .and_then(|pane| pane.render(area, buf))
            .filter(|cursor| area.contains(Position::new(cursor.x, cursor.y)));
        self.editor_pane.cursor
    }

    /// Whether a pane has been built, so there is something to draw.
    pub(crate) fn editor_pane_ready(&self) -> bool {
        self.editor_pane.slot.is_ready()
    }

    /// The cursor style to write to the terminal after a frame, when it is
    /// not the one already there.
    ///
    /// ratatui places and hides the cursor but knows nothing of its shape,
    /// so the event loop writes this itself. The editor's shape applies
    /// while it shows a cursor; any other mode gets the terminal's own
    /// shape back.
    pub fn take_cursor_style_change(&mut self) -> Option<SetCursorStyle> {
        let wanted = match (
            self.input_mode == InputMode::Editor,
            self.editor_pane.cursor,
        ) {
            (true, Some(cursor)) => Some(cursor.shape),
            // Hidden for a frame: leave the shape alone until it is back.
            (true, None) => self.editor_pane.applied_cursor,
            (false, _) => None,
        };
        if wanted == self.editor_pane.applied_cursor {
            return None;
        }
        self.editor_pane.applied_cursor = wanted;
        Some(cursor_style(wanted))
    }

    /// The command that gives the terminal its own cursor shape back, if
    /// the editor's is still set. For the way out of the TUI.
    pub fn restore_cursor_style(&mut self) -> Option<SetCursorStyle> {
        self.editor_pane
            .applied_cursor
            .take()
            .map(|_| cursor_style(None))
    }

    /// End the pane before tuicr exits: pending writes are flushed and
    /// language servers stopped. Returns what could not be done. There is
    /// no embedded editor afterwards.
    pub fn shutdown_editor_pane(&mut self) -> Vec<anyhow::Error> {
        self.leave_editor_mode();
        self.editor_pane.slot.shutdown()
    }

    /// The repo switcher has moved tuicr, and the process's working
    /// directory, to the repository at `dir`: tell the editor, when one is
    /// running, so that its relative paths, its file picker and its
    /// language servers follow. An editor that is only built later starts
    /// in the directory of that moment and needs no telling.
    ///
    /// Called by `main.rs` right after its `set_current_dir`, not by
    /// `switch_repo`: the Helix pane changes the working directory of the
    /// process itself, which tests that switch must not have happen.
    pub fn editor_pane_working_dir_changed(&mut self, dir: &Path) -> bool {
        let Some(pane) = self.editor_pane.slot.get_mut() else {
            return false;
        };
        let outcome = pane.working_dir_changed(dir);
        self.absorb_pane_outcome(outcome)
    }

    /// Run one call into the pane and take in what it reports.
    fn drive_editor_pane(&mut self, call: impl FnOnce(&mut dyn EditorPane) -> PaneOutcome) -> bool {
        let Some(pane) = self.editor_pane.slot.get_mut() else {
            self.leave_editor_mode();
            return true;
        };
        let outcome = call(pane);
        self.absorb_pane_outcome(outcome)
    }

    /// Act on what a call into the pane reported. Returns whether the
    /// screen needs repainting, which a hidden pane never causes.
    fn absorb_pane_outcome(&mut self, outcome: PaneOutcome) -> bool {
        let PaneOutcome {
            redraw,
            close_requested,
            saved,
            opened,
        } = outcome;
        let visible = self.input_mode == InputMode::Editor;
        let over_merge_view = if visible {
            self.editor_pane.return_mode == InputMode::MergeConflict
        } else {
            self.input_mode == InputMode::MergeConflict
        };
        for path in opened {
            // The baseline for the edited-line tint has to be the file as
            // it was before the editor could change it, and the editor can
            // on the very next key. The merge view tracks its file itself.
            if !over_merge_view {
                self.snapshot_local_edit_baseline(&path_target(path.clone()));
            }
            push_unique(&mut self.editor_pane.held, path.clone());
            push_unique(&mut self.editor_pane.unsettled, path.clone());
            push_unique(&mut self.editor_pane.files.opened, path);
        }
        for path in saved {
            // The review reloaded when the editor was left; a save that
            // lands after that is one more reason to (see
            // `finish_editor_pane_return`).
            if !visible {
                push_unique(&mut self.editor_pane.late_saves, path.clone());
            }
            push_unique(&mut self.editor_pane.files.saved, path);
        }
        if close_requested && visible {
            self.leave_editor_mode();
            return true;
        }
        redraw && visible
    }

    /// Tell the pane when it gains or loses focus, which is whenever
    /// Editor mode is entered or left by any route.
    fn sync_editor_pane_focus(&mut self) -> bool {
        let focused = self.input_mode == InputMode::Editor;
        if self.editor_pane.focused == focused {
            return false;
        }
        self.editor_pane.focused = focused;
        if !focused {
            self.editor_pane.returned = true;
        }
        let Some(pane) = self.editor_pane.slot.get_mut() else {
            return false;
        };
        let outcome = pane.focus_changed(focused);
        self.absorb_pane_outcome(outcome)
    }
}

/// Steady shapes, as Helix asks its own terminal for.
fn cursor_style(shape: Option<CursorShape>) -> SetCursorStyle {
    match shape {
        None => SetCursorStyle::DefaultUserShape,
        Some(CursorShape::Block) => SetCursorStyle::SteadyBlock,
        Some(CursorShape::Bar) => SetCursorStyle::SteadyBar,
        Some(CursorShape::Underline) => SetCursorStyle::SteadyUnderScore,
    }
}
