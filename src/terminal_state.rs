use std::cell::Cell;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crossterm::{
    cursor::SetCursorStyle,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};

mod keyboard_probe;

pub use keyboard_probe::supports_keyboard_enhancement;

/// Mirrors [`TerminalSession`]'s record of a cursor shape it wrote, for the
/// panic hook, which has no session to ask: the embedded editor's bar or
/// underline must not be left in the shell.
static CURSOR_STYLED: AtomicBool = AtomicBool::new(false);

/// Whether the session's keyboard level is on the terminal's stack: pushed
/// on the way into TUI mode where the startup probe found the protocol,
/// and not popped yet. For the panic hook, like [`CURSOR_STYLED`].
static KEYBOARD_LEVEL_PUSHED: AtomicBool = AtomicBool::new(false);

/// How many [`TerminalSession`]s have been entered and not yet ended (one, in
/// a running tuicr), for the panic hook, which may run on any thread. A
/// session suspended for `$EDITOR` still counts: the terminal is then
/// somebody else's to draw on, which is as good a reason to keep a panic
/// message off it.
static OPEN_SESSIONS: AtomicUsize = AtomicUsize::new(0);

/// Whether a TUI session has the terminal, or lent it to `$EDITOR` and will
/// take it back. While it does, text printed by anything but the session
/// lands on top of a frame.
pub fn session_open() -> bool {
    OPEN_SESSIONS.load(Ordering::Relaxed) > 0
}

thread_local! {
    /// The panic hook has popped the session's keyboard level on this
    /// thread ([`restore_stdio_best_effort`]). The session is dropped by the
    /// unwinding that follows, on the same thread, and must not pop a
    /// second one: that level would be whatever tuicr was started under.
    static KEYBOARD_LEVEL_POPPED_BY_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// Keyboard flags for the whole session.
///
/// REPORT_EVENT_TYPES distinguishes Press from Repeat from Release so the
/// two-press file walk can require an actual key release between presses.
/// Without it, terminals emit Press for every auto-repeat tick, so the
/// walk is turned off and j/k stop at the file's edges instead.
fn session_keyboard_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
}

/// Keyboard flags while the embedded editor has the keyboard: the session's
/// plus REPORT_ALTERNATE_KEYS. Setting a level's flags replaces all of them,
/// so the session flags are repeated. With the shifted key reported, `Alt-(`
/// arrives as that and not as `Alt-Shift-9`, which is how the editor's
/// keymap spells its bindings.
fn editor_keyboard_flags() -> KeyboardEnhancementFlags {
    session_keyboard_flags() | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
}

/// Replaces the kitty keyboard flags of the terminal's current level:
/// `CSI = flags ; 1 u`. crossterm has the push and the pop but not this.
///
/// The editor's flags are set on the session's own level and not pushed as
/// a second one because not every terminal keeps a stack. zellij keeps one
/// on/off switch: a pop on leaving the editor turned key reporting off for
/// the rest of the session there, while it stays on across this sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SetKeyboardEnhancementFlags(KeyboardEnhancementFlags);

impl crossterm::Command for SetKeyboardEnhancementFlags {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        write!(f, "\x1b[={};1u", self.0.bits())
    }

    /// The legacy console has no such flags, and the session never asks:
    /// keyboard enhancement is not detected there.
    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Terminal capabilities enabled for one tuicr TUI session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalFeatures {
    mouse_enabled: bool,
    keyboard_enhancements_supported: bool,
}

impl TerminalFeatures {
    /// Returns an empty feature set for a terminal session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures whether mouse capture should be active in the TUI.
    pub fn mouse_enabled(mut self, enabled: bool) -> Self {
        self.mouse_enabled = enabled;
        self
    }

    /// Configures whether keyboard enhancement flags should be pushed.
    ///
    /// tuicr only enables these flags after probing support because unsupported
    /// terminals can echo the probe escape sequences into stdout.
    pub fn keyboard_enhancements_supported(mut self, supported: bool) -> Self {
        self.keyboard_enhancements_supported = supported;
        self
    }

    /// Enters TUI terminal mode and returns the owning session.
    pub fn enter<W: Write>(self, writer: W) -> anyhow::Result<TerminalSession<W>> {
        TerminalSession::enter(writer, self)
    }
}

/// Owns terminal mode changes for the active TUI.
///
/// `TerminalSession` is the boundary that keeps raw mode,
/// alternate-screen state,
/// mouse capture,
/// bracketed paste,
/// and keyboard enhancement flags paired with the ratatui terminal.
/// Dropping the session performs a best-effort restore so early returns do not
/// leave the user's terminal in TUI mode.
pub struct TerminalSession<W: Write> {
    terminal: Terminal<CrosstermBackend<W>>,
    features: TerminalFeatures,
    active: bool,
    /// The session's keyboard level also reports alternate keys
    /// ([`Self::set_alternate_keys`]).
    alternate_keys: bool,
    /// A cursor shape other than the terminal's own was written
    /// ([`Self::set_cursor_style`]) and has not been taken back.
    cursor_styled: bool,
    /// Counted in [`session_open`] until restored or dropped.
    open: bool,
}

impl<W: Write> TerminalSession<W> {
    fn enter(writer: W, features: TerminalFeatures) -> anyhow::Result<Self> {
        let backend = CrosstermBackend::new(writer);
        let mut terminal = Terminal::new(backend)?;
        if let Err(err) = activate_writer(terminal.backend_mut(), features) {
            deactivate_writer_best_effort(
                terminal.backend_mut(),
                Teardown::of_failed_entry(features),
            );
            return Err(err);
        }
        OPEN_SESSIONS.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            terminal,
            features,
            active: true,
            alternate_keys: false,
            cursor_styled: false,
            open: true,
        })
    }

    /// Draws one frame while the TUI terminal session is active.
    pub fn draw<F>(&mut self, render_callback: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.terminal.draw(render_callback).map(|_| ())
    }

    /// Returns the terminal backend for low-level terminal commands.
    ///
    /// This is used for synchronized-update escape sequences that ratatui does
    /// not manage directly.
    pub fn backend_mut(&mut self) -> &mut CrosstermBackend<W> {
        self.terminal.backend_mut()
    }

    /// Whether mouse capture is on for this session.
    pub fn mouse_enabled(&self) -> bool {
        self.features.mouse_enabled
    }

    /// Turns mouse capture on or off for the running session (`:set
    /// mouse!`). With capture off the terminal keeps its own mouse
    /// behaviour: a drag selects text, the wheel is the terminal's. While
    /// suspended only the choice is recorded; resuming applies it.
    pub fn set_mouse_enabled(&mut self, enabled: bool) -> anyhow::Result<()> {
        if self.features.mouse_enabled == enabled {
            return Ok(());
        }
        if self.active {
            if enabled {
                execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
            } else {
                execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
            }
        }
        self.features.mouse_enabled = enabled;
        Ok(())
    }

    /// Whether the session's keyboard level reports alternate keys.
    pub fn alternate_keys_active(&self) -> bool {
        self.alternate_keys
    }

    /// Turns the reporting of alternate keys on for as long as the embedded
    /// editor has the keyboard, and off again when it gives it back.
    ///
    /// The flags of the session's own keyboard level are replaced
    /// (`CSI = flags ; 1 u`); no level is pushed or popped, because not
    /// every terminal keeps a stack of them. Leaving TUI mode pops that one
    /// level whatever its flags, so there is nothing extra to undo on any
    /// way out.
    ///
    /// Does nothing when the flags are already as asked, when the startup
    /// probe did not find keyboard enhancement (the terminal would print
    /// the sequence), or while suspended. Resuming pushes the session's
    /// plain flags again, so the caller asks again after a resume; the
    /// event loop does on every pass.
    pub fn set_alternate_keys(&mut self, enabled: bool) -> anyhow::Result<()> {
        if self.alternate_keys == enabled
            || !self.active
            || !self.features.keyboard_enhancements_supported
        {
            return Ok(());
        }
        let flags = if enabled {
            editor_keyboard_flags()
        } else {
            session_keyboard_flags()
        };
        execute!(
            self.terminal.backend_mut(),
            SetKeyboardEnhancementFlags(flags)
        )?;
        self.alternate_keys = enabled;
        Ok(())
    }

    /// Writes a cursor shape (the embedded editor's block, bar or
    /// underline, or the terminal's own back) and remembers whether one is
    /// in force, so that every way out of TUI mode, an early return on an
    /// error included, gives the terminal its own shape back.
    pub fn set_cursor_style(&mut self, style: SetCursorStyle) -> anyhow::Result<()> {
        if !self.active {
            return Ok(());
        }
        execute!(self.terminal.backend_mut(), style)?;
        self.note_cursor_styled(!matches!(style, SetCursorStyle::DefaultUserShape));
        Ok(())
    }

    fn note_cursor_styled(&mut self, styled: bool) {
        self.cursor_styled = styled;
        CURSOR_STYLED.store(styled, Ordering::Relaxed);
    }

    /// Restores the terminal state for the normal exit path.
    ///
    /// After this succeeds,
    /// dropping the session will not attempt a second restore.
    pub fn restore(&mut self) -> anyhow::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.deactivate()?;
        self.close();
        Ok(())
    }

    /// The session is over, not merely suspended: stop counting it.
    fn close(&mut self) {
        if std::mem::take(&mut self.open) {
            OPEN_SESSIONS.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Temporarily leaves TUI terminal mode.
    ///
    /// The returned guard re-enters TUI mode on `resume()` or,
    /// as a best-effort fallback,
    /// when the guard is dropped.
    pub fn suspend(&mut self) -> anyhow::Result<TerminalSuspension<'_, W>> {
        self.deactivate()?;
        Ok(TerminalSuspension {
            session: Some(self),
        })
    }

    fn activate(&mut self) -> anyhow::Result<()> {
        if self.active {
            return Ok(());
        }
        if let Err(err) = activate_writer(self.terminal.backend_mut(), self.features) {
            deactivate_writer_best_effort(
                self.terminal.backend_mut(),
                Teardown::of_failed_entry(self.features),
            );
            return Err(err);
        }
        self.active = true;
        Ok(())
    }

    fn deactivate(&mut self) -> anyhow::Result<()> {
        if !self.active {
            return Ok(());
        }
        // The session's keyboard level is popped with whatever flags it
        // has, and the cursor shape goes with the rest, whatever else
        // fails: whoever gets the terminal next must not inherit either.
        let cursor_styled = self.cursor_styled;
        self.alternate_keys = false;
        self.note_cursor_styled(false);
        deactivate_writer(
            self.terminal.backend_mut(),
            Teardown::of(self.features, cursor_styled),
        )?;
        self.active = false;
        Ok(())
    }

    /// Clears the screen and drops the back buffer so the next draw repaints
    /// every cell.
    ///
    /// Used after an editor hands the terminal back, and for `Ctrl-l` and
    /// resize events, where the terminal may hold cells that ratatui's diff
    /// against its last frame would otherwise leave alone.
    ///
    /// `Terminal::clear` cannot be used here: it starts by reading the cursor
    /// position, whose `ESC [ 6 n` query goes to stdout. When stdout is not
    /// the terminal the reply never arrives, and the two-second timeout fails
    /// the clear before it resets the back buffer, so the next draw diffs
    /// against the pre-editor screen and paints nothing.
    ///
    /// Resizing to the current size clears and resets the same way, but sizes
    /// itself from `/dev/tty`.
    pub fn repaint_from_scratch(&mut self) -> anyhow::Result<()> {
        let area = self.terminal.size()?.into();
        self.terminal.resize(area)?;
        Ok(())
    }
}

impl<W: Write> Drop for TerminalSession<W> {
    fn drop(&mut self) {
        if self.active {
            let cursor_styled = self.cursor_styled;
            self.alternate_keys = false;
            self.note_cursor_styled(false);
            // A panic on this thread ran the hook before this drop, and the
            // hook popped the keyboard level. Everything else is undone
            // again: it does no harm twice, and under `--stdout` this
            // writer, not the hook's, is the one the terminal is on.
            let popped_by_hook = KEYBOARD_LEVEL_POPPED_BY_HOOK
                .try_with(|popped| popped.replace(false))
                .unwrap_or(false);
            let teardown = Teardown::of(self.features, cursor_styled);
            deactivate_writer_best_effort(
                self.terminal.backend_mut(),
                Teardown {
                    keyboard_level: teardown.keyboard_level && !popped_by_hook,
                    ..teardown
                },
            );
            self.active = false;
        }
        self.close();
    }
}

/// Guard for a temporarily suspended TUI terminal session.
///
/// Holding this value means tuicr has left raw mode and the alternate screen so
/// another foreground process can use the terminal.
/// Dropping the guard re-enters TUI mode on a best-effort basis.
pub struct TerminalSuspension<'a, W: Write> {
    session: Option<&'a mut TerminalSession<W>>,
}

impl<W: Write> TerminalSuspension<'_, W> {
    /// Re-enters TUI terminal mode and consumes the suspension guard.
    ///
    /// Calling this explicit method is preferred on the normal path because it
    /// reports terminal restoration failures to the caller.
    pub fn resume(mut self) -> anyhow::Result<()> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        session.activate()?;
        session.repaint_from_scratch()?;
        Ok(())
    }
}

impl<W: Write> Drop for TerminalSuspension<'_, W> {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        if session.activate().is_ok() {
            let _ = session.repaint_from_scratch();
        }
    }
}

/// Restores terminal mode on stdout without requiring a live session object.
///
/// This is intended for panic hooks,
/// where ownership may already be unwinding and the session value might not be
/// reachable.
pub fn restore_stdio_best_effort() {
    // A panic can come while the embedded editor has set its own cursor
    // shape, which may not outlive the TUI. The flag is left as it is: the
    // session, dropped next, resets the shape on its own writer too, which
    // under `--stdout` is the one the terminal is on.
    let cursor_styled = CURSOR_STYLED.load(Ordering::Relaxed);
    let keyboard_level = KEYBOARD_LEVEL_PUSHED.load(Ordering::Relaxed);
    restore_writer_after_panic(&mut io::stdout(), cursor_styled, keyboard_level);
}

/// What the panic hook writes, and the note it leaves for the session.
///
/// The editor's keyboard flags need nothing of their own: they go with the
/// session's one level, popped here if the session pushed one
/// (`keyboard_level`). That pop is the only part of leaving TUI mode that
/// cannot be repeated, so the session dropped by the unwinding that
/// follows is told to skip it: one level was pushed on the way in, and a
/// second pop would take the level of whatever tuicr runs under.
fn restore_writer_after_panic<W: Write>(writer: &mut W, cursor_styled: bool, keyboard_level: bool) {
    deactivate_writer_best_effort(
        writer,
        Teardown {
            mouse_enabled: true,
            cursor_styled,
            keyboard_level,
        },
    );
    let _ = KEYBOARD_LEVEL_POPPED_BY_HOOK.try_with(|popped| popped.set(true));
}

fn activate_writer<W: Write>(writer: &mut W, features: TerminalFeatures) -> anyhow::Result<()> {
    enable_raw_mode()?;
    execute!(writer, EnterAlternateScreen)?;
    if features.mouse_enabled {
        execute!(writer, EnableMouseCapture)?;
    }

    // Bracketed paste makes multi-line and control-character pastes arrive as
    // one `Event::Paste` instead of letting each character drive normal-mode
    // actions like Enter submit or command-mode entry.
    execute!(writer, EnableBracketedPaste)?;

    push_session_keyboard_level(writer, features.keyboard_enhancements_supported);
    Ok(())
}

/// Pushes the session's keyboard level where the terminal was found to have
/// such a thing. See `session_keyboard_flags` for what the flags are for.
fn push_session_keyboard_level<W: Write>(writer: &mut W, supported: bool) {
    if supported {
        let _ = execute!(
            writer,
            PushKeyboardEnhancementFlags(session_keyboard_flags())
        );
    }
    KEYBOARD_LEVEL_PUSHED.store(supported, Ordering::Relaxed);
    // Whatever the panic hook popped before this, it was not this level. (A
    // panic while suspended for `$EDITOR` re-enters TUI mode on its way
    // out, through the suspension's drop, and the session dropped after
    // that has a level of its own to pop again.)
    let _ = KEYBOARD_LEVEL_POPPED_BY_HOOK.try_with(|popped| popped.set(false));
}

/// What leaving TUI mode has to undo besides what it always undoes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Teardown {
    /// Mouse capture is on.
    mouse_enabled: bool,
    /// A cursor shape other than the terminal's own is in force (the
    /// embedded editor's): the terminal gets its own back.
    cursor_styled: bool,
    /// The keyboard level `activate_writer` pushed is there to pop. Not so
    /// where none was pushed (the startup probe did not find the protocol),
    /// and for a session dropped behind the panic hook, which popped it.
    keyboard_level: bool,
}

impl Teardown {
    /// The usual way out of a session with `features`: its keyboard level
    /// comes off, if it pushed one.
    ///
    /// A level is pushed only where the probe found the protocol
    /// ([`push_session_keyboard_level`]), so only there is one popped. A
    /// pop without a push is not harmless everywhere: when the probe went
    /// unanswered (a multiplexer in between, a slow link) on a terminal
    /// that does keep a stack, it took the level of the program tuicr was
    /// started from, on exit and around every `$EDITOR` hand-off.
    fn of(features: TerminalFeatures, cursor_styled: bool) -> Self {
        Self {
            mouse_enabled: features.mouse_enabled,
            cursor_styled,
            keyboard_level: features.keyboard_enhancements_supported,
        }
    }

    /// Undoing an entry that failed part of the way: the keyboard level is
    /// pushed last, after everything that can fail, so there is none.
    fn of_failed_entry(features: TerminalFeatures) -> Self {
        Self {
            mouse_enabled: features.mouse_enabled,
            cursor_styled: false,
            keyboard_level: false,
        }
    }
}

/// Exactly one keyboard level is popped, the one `activate_writer` pushed,
/// and none when `teardown` says there is none: never pushed, or gone
/// already.
fn deactivate_writer<W: Write>(writer: &mut W, teardown: Teardown) -> anyhow::Result<()> {
    // A panic between Begin/EndSynchronizedUpdate would otherwise leave
    // the terminal pinned on the last frame — an invisible crash that
    // looks like a freeze. Ending sync mode is a no-op when not active.
    let _ = execute!(writer, crossterm::terminal::EndSynchronizedUpdate);
    if teardown.cursor_styled {
        let _ = execute!(writer, SetCursorStyle::DefaultUserShape);
    }
    if teardown.keyboard_level {
        let _ = execute!(writer, PopKeyboardEnhancementFlags);
        KEYBOARD_LEVEL_PUSHED.store(false, Ordering::Relaxed);
    }
    let _ = execute!(writer, DisableBracketedPaste);
    if teardown.mouse_enabled {
        let _ = execute!(writer, DisableMouseCapture);
    }
    disable_raw_mode()?;
    execute!(writer, LeaveAlternateScreen)?;
    Ok(())
}

fn deactivate_writer_best_effort<W: Write>(writer: &mut W, teardown: Teardown) {
    let _ = deactivate_writer(writer, teardown);
}

// Unix only: tearing a session down goes through crossterm's raw-mode
// bookkeeping, which needs a console handle on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::{TerminalOptions, Viewport};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// What a session wrote, still readable after the session owns the
    /// writer.
    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        /// Everything written since the last call.
        fn take(&self) -> String {
            String::from_utf8(std::mem::take(&mut *self.0.borrow_mut())).expect("utf-8")
        }
    }

    fn ansi(command: impl crossterm::Command) -> String {
        let mut out = String::new();
        command.write_ansi(&mut out).expect("ansi");
        out
    }

    fn pops(written: &str) -> usize {
        written.matches(&ansi(PopKeyboardEnhancementFlags)).count()
    }

    /// Levels pushed: `CSI > flags u`, whatever the flags.
    fn pushes(written: &str) -> usize {
        written.matches("\x1b[>").count()
    }

    /// A session that believes it is active but never touched a terminal:
    /// raw mode was not entered, and a fixed viewport keeps ratatui from
    /// asking for the size.
    fn fake_session(sink: &Sink, keyboard_enhancement: bool) -> TerminalSession<Sink> {
        let terminal = Terminal::with_options(
            CrosstermBackend::new(sink.clone()),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
            },
        )
        .expect("terminal");
        TerminalSession {
            terminal,
            features: TerminalFeatures::new().keyboard_enhancements_supported(keyboard_enhancement),
            active: true,
            alternate_keys: false,
            cursor_styled: false,
            open: false,
        }
    }

    #[test]
    fn should_report_alternate_keys_on_top_of_the_session_flags() {
        let flags = editor_keyboard_flags();

        assert!(flags.contains(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS));
        assert!(
            flags.contains(session_keyboard_flags()),
            "setting a level's flags replaces them all, so the session's are repeated"
        );
        assert!(
            !session_keyboard_flags().contains(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS),
            "the review's own keys are read without alternates"
        );
    }

    #[test]
    fn should_write_the_set_flags_sequence_of_the_kitty_protocol() {
        // `CSI = flags ; 1 u`: mode 1 replaces every flag of the current
        // level. These are the bytes zellij was probed with.
        assert_eq!(
            ansi(SetKeyboardEnhancementFlags(editor_keyboard_flags())),
            "\x1b[=7;1u"
        );
        assert_eq!(
            ansi(SetKeyboardEnhancementFlags(session_keyboard_flags())),
            "\x1b[=3;1u"
        );
    }

    #[test]
    fn should_set_the_editor_flags_once_and_the_session_s_back_once() {
        let sink = Sink::default();
        let mut session = fake_session(&sink, true);

        session.set_alternate_keys(true).expect("on");
        session.set_alternate_keys(true).expect("on again");
        assert_eq!(
            sink.take(),
            ansi(SetKeyboardEnhancementFlags(editor_keyboard_flags())),
            "asking twice writes once"
        );
        assert!(session.alternate_keys_active());

        session.set_alternate_keys(false).expect("off");
        session.set_alternate_keys(false).expect("off again");
        assert_eq!(
            sink.take(),
            ansi(SetKeyboardEnhancementFlags(session_keyboard_flags())),
            "the session's flags are put back, once"
        );
        assert!(!session.alternate_keys_active());
    }

    #[test]
    fn should_neither_push_nor_pop_a_keyboard_level_around_the_editor() {
        // zellij keeps one on/off switch, not a stack: a pop on leaving the
        // editor turned kitty key reporting off for the rest of the
        // session, and Shift+Enter in the comment box became Enter.
        let sink = Sink::default();
        let mut session = fake_session(&sink, true);

        session.set_alternate_keys(true).expect("on");
        session.set_alternate_keys(false).expect("off");
        session.set_alternate_keys(true).expect("on");
        session.set_alternate_keys(false).expect("off");

        let written = sink.take();
        assert_eq!(pops(&written), 0, "{written:?}");
        assert_eq!(pushes(&written), 0, "{written:?}");
    }

    #[test]
    fn should_not_set_keyboard_flags_the_terminal_was_not_found_to_support() {
        let sink = Sink::default();
        let mut session = fake_session(&sink, false);

        session.set_alternate_keys(true).expect("on");
        session.set_alternate_keys(false).expect("off");

        assert_eq!(
            sink.take(),
            "",
            "an unprobed terminal would print the sequence"
        );
        assert!(!session.alternate_keys_active());
    }

    #[test]
    fn should_pop_one_keyboard_level_when_suspending_from_the_editor() {
        let sink = Sink::default();
        let mut session = fake_session(&sink, true);
        session.set_alternate_keys(true).expect("on");
        sink.take();

        let suspension = session.suspend().expect("suspend");
        // Resuming would put the terminal the tests run in into raw mode.
        std::mem::forget(suspension);

        assert_eq!(
            pops(&sink.take()),
            1,
            "the session's one level, editor flags and all: $EDITOR gets the terminal as tuicr found it"
        );
        assert!(
            !session.alternate_keys_active(),
            "resuming pushes the plain session flags, so the editor's are asked for again"
        );

        session.set_alternate_keys(true).expect("on");
        assert_eq!(sink.take(), "", "nothing is written while suspended");
        assert!(!session.alternate_keys_active());
    }

    #[test]
    fn should_pop_one_keyboard_level_on_every_way_out_whatever_the_editor_did() {
        // One level is pushed on the way in, so one comes off on the way
        // out. With the editor's flags as a second level, two came off
        // wherever the editor still had the keyboard.
        let never = Sink::default();
        let mut session = fake_session(&never, true);
        session.restore().expect("restore");
        assert_eq!(pops(&never.take()), 1, "the editor was never opened");

        let left = Sink::default();
        let mut session = fake_session(&left, true);
        session.set_alternate_keys(true).expect("on");
        session.set_alternate_keys(false).expect("off");
        left.take();
        session.restore().expect("restore");
        assert_eq!(pops(&left.take()), 1, "the editor was opened and left");

        let restored = Sink::default();
        let mut session = fake_session(&restored, true);
        session.set_alternate_keys(true).expect("on");
        restored.take();
        session.restore().expect("restore");
        assert_eq!(pops(&restored.take()), 1, "a normal exit from the editor");
        drop(session);
        assert_eq!(pops(&restored.take()), 0, "restored once, not again");

        let dropped = Sink::default();
        let mut session = fake_session(&dropped, true);
        session.set_alternate_keys(true).expect("on");
        dropped.take();
        drop(session);
        assert_eq!(
            pops(&dropped.take()),
            1,
            "an early return drops the session"
        );
    }

    #[test]
    fn should_pop_no_keyboard_level_where_none_was_pushed() {
        // The startup probe went unanswered, so no level was pushed. A pop
        // on the way out would take the level of whatever tuicr was
        // started from, if the terminal does keep a stack after all.
        let restored = Sink::default();
        let mut session = fake_session(&restored, false);
        session.restore().expect("restore");
        let written = restored.take();
        assert_eq!(pops(&written), 0, "a normal exit: {written:?}");
        assert!(
            written.contains(&ansi(LeaveAlternateScreen)),
            "the rest of the way out is the same: {written:?}"
        );

        let suspended = Sink::default();
        let mut session = fake_session(&suspended, false);
        let suspension = session.suspend().expect("suspend");
        std::mem::forget(suspension);
        assert_eq!(pops(&suspended.take()), 0, "a hand-off to $EDITOR");

        let dropped = Sink::default();
        drop(fake_session(&dropped, false));
        assert_eq!(pops(&dropped.take()), 0, "an early return");

        let panicked = Sink::default();
        let session = fake_session(&panicked, false);
        restore_writer_after_panic(&mut panicked.clone(), false, false);
        drop(session);
        assert_eq!(
            pops(&panicked.take()),
            0,
            "the panic hook and the unwinding"
        );
    }

    #[test]
    fn should_pop_no_keyboard_level_after_an_entry_that_failed() {
        // The level is pushed last, so an entry that failed before it has
        // none to take back, whatever the terminal supports.
        let features = TerminalFeatures::new()
            .mouse_enabled(true)
            .keyboard_enhancements_supported(true);
        let teardown = Teardown::of_failed_entry(features);
        assert!(!teardown.keyboard_level);
        assert!(teardown.mouse_enabled);
        assert!(Teardown::of(features, false).keyboard_level);
    }

    fn cursor_resets(written: &str) -> usize {
        written
            .matches(&ansi(SetCursorStyle::DefaultUserShape))
            .count()
    }

    #[test]
    fn should_pop_one_keyboard_level_for_a_panic_in_the_editor() {
        // The hook gives the terminal back, then the unwinding drops the
        // session. Both popped: two levels for the one pushed (four, while
        // the editor's flags were a level of their own), and the extra one
        // belonged to whatever tuicr was started under.
        let sink = Sink::default();
        let mut session = fake_session(&sink, true);
        session.set_alternate_keys(true).expect("on");
        session
            .set_cursor_style(SetCursorStyle::SteadyBar)
            .expect("style");
        sink.take();

        restore_writer_after_panic(&mut sink.clone(), true, true);
        let hook = sink.take();
        assert_eq!(pops(&hook), 1, "{hook:?}");
        assert_eq!(pushes(&hook), 0, "{hook:?}");
        assert_eq!(cursor_resets(&hook), 1, "{hook:?}");

        drop(session);
        let unwinding = sink.take();
        assert_eq!(
            pops(&unwinding),
            0,
            "the hook's pop was the session's level: {unwinding:?}"
        );
        assert_eq!(
            cursor_resets(&unwinding),
            1,
            "the rest is undone again, on the session's own writer"
        );
        assert!(
            unwinding.contains(&ansi(LeaveAlternateScreen)),
            "{unwinding:?}"
        );
    }

    #[test]
    fn should_skip_the_pop_behind_the_panic_hook_once_only() {
        let sink = Sink::default();
        let first = fake_session(&sink, true);
        restore_writer_after_panic(&mut sink.clone(), false, true);
        drop(first);
        sink.take();

        // Nothing but a session dropped right behind the hook skips it.
        let mut second = fake_session(&sink, true);
        second.restore().expect("restore");
        assert_eq!(pops(&sink.take()), 1);
    }

    #[test]
    fn should_pop_a_level_pushed_after_the_panic_hook_ran() {
        // A panic while suspended for `$EDITOR`: the hook runs with no
        // level of the session's on the terminal, then the suspension's
        // drop re-enters TUI mode and pushes one. The session dropped after
        // that has to pop it, or the shell is left with kitty key reporting.
        let sink = Sink::default();
        let mut session = fake_session(&sink, true);
        let suspension = session.suspend().expect("suspend");
        std::mem::forget(suspension);
        restore_writer_after_panic(&mut sink.clone(), false, false);
        sink.take();

        // What resuming does to the keyboard, without the raw mode.
        push_session_keyboard_level(&mut sink.clone(), true);
        session.active = true;
        assert_eq!(pushes(&sink.take()), 1);
        drop(session);

        assert_eq!(pops(&sink.take()), 1);
    }

    #[test]
    fn should_count_a_session_as_open_until_it_is_restored_or_dropped() {
        // The only test that counts its sessions, so the count is its own.
        let counted = |sink: &Sink| {
            let mut session = fake_session(sink, true);
            session.open = true;
            OPEN_SESSIONS.fetch_add(1, Ordering::Relaxed);
            session
        };
        let sink = Sink::default();
        assert!(!session_open(), "a session that was never entered");

        let mut session = counted(&sink);
        assert!(session_open());
        let suspension = session.suspend().expect("suspend");
        std::mem::forget(suspension);
        assert!(
            session_open(),
            "lent to $EDITOR: a panic message would land on its screen"
        );
        // Resuming, without the raw mode.
        session.active = true;
        session.restore().expect("restore");
        assert!(!session_open(), "restored for good");
        drop(session);
        assert!(!session_open(), "and not counted down twice");

        let session = counted(&sink);
        assert!(session_open());
        drop(session);
        assert!(!session_open(), "an early return drops it");
    }

    #[test]
    fn should_give_the_cursor_shape_back_when_the_session_is_dropped() {
        // An I/O error in the event loop returns early: nothing but the
        // session's drop is left to take the editor's bar cursor back.
        let sink = Sink::default();
        let mut session = fake_session(&sink, true);
        session
            .set_cursor_style(SetCursorStyle::SteadyBar)
            .expect("style");
        assert_eq!(sink.take(), ansi(SetCursorStyle::SteadyBar));

        drop(session);

        assert_eq!(cursor_resets(&sink.take()), 1);
    }

    #[test]
    fn should_give_the_cursor_shape_back_when_suspending_and_on_restore() {
        let sink = Sink::default();
        let mut session = fake_session(&sink, true);
        session
            .set_cursor_style(SetCursorStyle::SteadyUnderScore)
            .expect("style");
        sink.take();
        let suspension = session.suspend().expect("suspend");
        std::mem::forget(suspension);
        assert_eq!(cursor_resets(&sink.take()), 1, "$EDITOR gets its own");

        let sink = Sink::default();
        let mut session = fake_session(&sink, true);
        session
            .set_cursor_style(SetCursorStyle::SteadyBlock)
            .expect("style");
        sink.take();
        session.restore().expect("restore");
        assert_eq!(cursor_resets(&sink.take()), 1, "a normal exit");
    }

    #[test]
    fn should_leave_the_cursor_shape_alone_when_none_is_in_force() {
        let untouched = Sink::default();
        let mut session = fake_session(&untouched, true);
        session.restore().expect("restore");
        assert_eq!(
            cursor_resets(&untouched.take()),
            0,
            "a session that never set a shape does not reset the user's"
        );

        let taken_back = Sink::default();
        let mut session = fake_session(&taken_back, true);
        session
            .set_cursor_style(SetCursorStyle::SteadyBar)
            .expect("style");
        session
            .set_cursor_style(SetCursorStyle::DefaultUserShape)
            .expect("back");
        taken_back.take();
        session.restore().expect("restore");
        assert_eq!(
            cursor_resets(&taken_back.take()),
            0,
            "already given back on leaving the editor"
        );
    }
}
