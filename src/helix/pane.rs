//! The host-side contract for an embedded editor pane.
//!
//! [`EditorPane`] speaks tuicr's own vocabulary: crossterm events in, a
//! ratatui buffer out. An implementation owns the translation to whatever
//! is behind it, so the in-process Helix facade and a PTY-hosted `hx` can
//! both sit behind the same trait and the rest of tuicr cannot tell which.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};

/// What one call into the pane changed, as far as the host cares.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneOutcome {
    /// The pane has a new frame to show.
    pub redraw: bool,
    /// The user closed the editor's last view (`:q`, `:wq`, `C-w q`): the
    /// host should go back to the review. The pane itself stays alive.
    pub close_requested: bool,
    /// Files the pane reported as written since the previous call. Not every
    /// write is reported (a `:wq` flush is not), so the file system stays
    /// the authority: the host refreshes on every return to the review. It
    /// also refreshes for a save reported while the pane is hidden, which
    /// is how a `:w` that finishes after `Ctrl-g` reaches the review, so a
    /// write that completes in the background must be reported here.
    pub saved: Vec<PathBuf>,
    /// Files with a path that the pane opened since the previous call,
    /// including ones reached by navigation rather than asked for.
    pub opened: Vec<PathBuf>,
}

impl PaneOutcome {
    /// An outcome that asks for nothing but a repaint.
    pub fn redraw_only() -> Self {
        Self {
            redraw: true,
            ..Self::default()
        }
    }
}

/// Shape of the terminal cursor while the pane has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

/// Where the pane wants the terminal cursor after a render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneCursor {
    /// Column in screen coordinates (the render area's origin already added).
    pub x: u16,
    /// Row in screen coordinates.
    pub y: u16,
    pub shape: CursorShape,
}

/// A snapshot of what the editor is showing and what it has not saved.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneState {
    /// File in the focused view; `None` for a scratch buffer.
    pub path: Option<PathBuf>,
    /// One-based line of the primary cursor.
    pub line: usize,
    /// One-based column of the primary cursor, in characters.
    pub col: usize,
    /// The editor's own mode, as it would label it (`NOR`, `INS`, `SEL`).
    /// Empty when the implementation cannot tell.
    pub mode: String,
    /// Files with unsaved changes, hidden buffers included.
    pub modified: Vec<PathBuf>,
    /// Scratch buffers (no path) with unsaved changes.
    pub modified_scratch: usize,
    /// Saves the editor has started and not finished: a `:w` is written in
    /// the background and reported in [`PaneOutcome::saved`] some pumps
    /// later, and until then its buffer still counts as `modified`. The
    /// host waits a moment for these before it reloads the review or
    /// answers a quit. Zero when the implementation cannot tell; a save
    /// that lands later still reaches the review through `saved`.
    pub pending_writes: usize,
    /// Something about the editor's setup the user can act on, short
    /// enough to end a status line: the Helix pane's reads ``run `tuicr
    /// --helix-setup` once: no syntax highlighting yet``. The pane says it
    /// in its own status line too, but that line is the editor's to clear;
    /// the review repeats it when the editor gives the screen back. `None`
    /// when all is well.
    pub notice: Option<String>,
}

/// An editor that draws into a rectangle of tuicr's frame and is fed
/// tuicr's input while it has focus.
///
/// # Threading
///
/// An implementation need not be `Send`, and the Helix one is not: every
/// method must be called from the thread that built the pane, which is the
/// thread running the event loop.
///
/// # Lifetime
///
/// A process has at most one pane. It is built on first use, kept for the
/// rest of the run (hiding it does not drop it, so buffers and language
/// servers stay warm), and ended once with [`EditorPane::shutdown`].
///
/// # What the review relies on
///
/// The pane is hidden far more often than it is dropped, so it outlives
/// what the review does to the working tree in between. An implementation
/// has to get these right; the scripted fake cannot check them for it:
///
/// - [`EditorPane::open`] shows the file as it is on disk now, not a buffer
///   left from an earlier visit.
/// - Every path it reports ([`PaneOutcome::saved`], [`PaneOutcome::opened`],
///   [`PaneState::modified`]) is absolute and spelled as tuicr spelled it
///   in `open` (the same prefix for the repository, no symlink resolved on
///   top): the review compares them with its own paths.
/// - A file is reported in [`PaneOutcome::opened`] by the call that loaded
///   it, before any later call could write it: the baseline for the
///   edited-line tint is the file as that call left it.
/// - A background save is reported in [`PaneOutcome::saved`] when it has
///   landed, also while the pane is hidden: that is how a `:w` that
///   finishes after `Ctrl-g` reaches the review.
/// - The process's panic hook, its signal handlers and its terminal are
///   tuicr's. An implementation installs no hook or handler and writes
///   nothing to the terminal, and a panic on one of its own threads is
///   caught there: tuicr logs it, keeps the screen and carries on.
///
/// # Panics
///
/// A panic inside any of these methods ends the pane, not tuicr: the
/// [`PaneSlot`] catches it, drops the pane and never calls it again (see
/// [`PaneSlot::take_crash`]). What the editor held unsaved is lost with
/// it, so an implementation still must not panic where it can help it.
pub trait EditorPane {
    /// Show `path` in the focused view, with the cursor on the one-based
    /// `line` when given. Opening does not give the pane focus.
    ///
    /// The view must show the file as it is on disk now. tuicr rewrites
    /// files between visits (the merge view writes its picks just before
    /// it hands the file over; a discard, a checkout or a pull changes the
    /// working tree; so does an agent), and the editor behind the pane
    /// does not notice by itself. So when the pane already holds a buffer
    /// for `path`:
    ///
    /// - without unsaved changes, the buffer is read from disk again
    ///   before it is shown. Showing the old text would put the cursor on
    ///   a line number worked out for the new one, and a forced write
    ///   would put the old text back over what tuicr just wrote.
    /// - with unsaved changes, they are kept, and when the file on disk is
    ///   no longer what the buffer was loaded from the pane says so in its
    ///   own status line: tuicr's is covered while the pane has the screen.
    ///
    /// # Errors
    ///
    /// An error leaves the review where it was and hands the file to
    /// `$EDITOR`, which reads it from disk; the pane is kept. The error's
    /// text is what tuicr's status line says afterwards (`embedded editor
    /// not used: ...`), so it is written for the user. That is also the way
    /// out for a pane that will not show a buffer it cannot bring up to
    /// date: unsaved changes over a file that was rewritten under them.
    fn open(&mut self, path: &Path, line: Option<usize>) -> anyhow::Result<PaneOutcome>;

    /// A key press or repeat. The host never passes a release, and never
    /// passes its own reserved chord (see [`is_return_chord`]).
    fn handle_key(&mut self, key: KeyEvent) -> PaneOutcome;

    /// A mouse event in screen coordinates, with the rectangle the pane was
    /// last drawn in so the implementation can make them pane-local.
    fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> PaneOutcome;

    /// A bracketed paste, delivered whole.
    fn handle_paste(&mut self, text: &str) -> PaneOutcome;

    /// The pane gained (`true`) or lost (`false`) input focus. It gains it
    /// right after an [`EditorPane::open`], every time Editor mode is
    /// entered: the moment to look at what else changed on disk while the
    /// pane was hidden (`open` only speaks for the file it names).
    fn focus_changed(&mut self, focused: bool) -> PaneOutcome;

    /// tuicr has moved to another repository (the repo switcher), whose
    /// root is `dir`, and opens that repository's files from now on. The
    /// editor follows: relative paths, its file picker and the workspace
    /// its language servers start in go by `dir` from here. The buffers
    /// it holds stay as they are, unsaved changes included.
    ///
    /// The process's working directory is already `dir` when this is
    /// called. Only called while the pane is hidden.
    fn working_dir_changed(&mut self, dir: &Path) -> PaneOutcome;

    /// Let background work (language servers, file watching, timers) make
    /// progress. Called on every pass of the event loop, shown or hidden,
    /// and must never wait for work to arrive.
    ///
    /// The one wait allowed is for a save the editor has itself started
    /// writing, bounded and once per write (the Helix pane waits at most a
    /// quarter of a second): the review must never read a file half saved.
    fn pump(&mut self) -> PaneOutcome;

    /// Draw the current frame into `area` of `buf`, first resizing the
    /// editor when `area` is not the size it last drew at. Returns where
    /// the terminal cursor belongs, or `None` to hide it.
    fn render(&mut self, area: Rect, buf: &mut Buffer) -> Option<PaneCursor>;

    /// What the editor is showing and what is unsaved.
    fn state(&self) -> PaneState;

    /// Flush pending writes, stop background work, and report what failed.
    /// The pane is gone afterwards and cannot be built again.
    fn shutdown(self: Box<Self>) -> Vec<anyhow::Error>;
}

/// Why a [`PaneFactory`] could not build the pane, and whether asking it
/// again could work.
///
/// The editor behind a pane can be constructed once per process, but not
/// every failure uses that one chance up: a config file that does not
/// parse fails before anything is built, and the user can fix the file.
/// An `anyhow::Error` converts into the final kind, so `?` in a factory
/// never promises a retry it has not thought about.
#[derive(Debug)]
pub struct PaneStartError {
    error: anyhow::Error,
    retry: bool,
}

impl PaneStartError {
    /// The editor cannot be built in this process any more. The factory is
    /// dropped, and every later open falls back to `$EDITOR` with this
    /// reason until tuicr is restarted.
    pub fn fatal(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            retry: false,
        }
    }

    /// Nothing was built and nothing stands in the way of trying again:
    /// the factory is kept and run again the next time a file is opened.
    pub fn retryable(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            retry: true,
        }
    }

    /// Whether the factory may be run again.
    pub fn can_retry(&self) -> bool {
        self.retry
    }
}

impl From<anyhow::Error> for PaneStartError {
    fn from(error: anyhow::Error) -> Self {
        Self::fatal(error)
    }
}

impl std::fmt::Display for PaneStartError {
    /// The whole chain on one line, as the status line shows it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.error)
    }
}

/// Builds the process's one pane at the size it will first be drawn at.
///
/// Called when a file is first opened in the pane, and again on a later
/// open only after it returned [`PaneStartError::retryable`]. Once it has
/// returned a pane, or a final error, it is never called again.
pub type PaneFactory = Box<dyn FnMut(Size) -> Result<Box<dyn EditorPane>, PaneStartError>>;

/// Why there is no pane when no factory was ever installed.
pub const NO_FACTORY: &str = "the embedded editor is not set up";
/// Why there is no pane after [`PaneSlot::shutdown`].
pub const SHUT_DOWN: &str = "the embedded editor has shut down";
/// Added to the reason of a start that failed for good: fixing what was
/// wrong does not help until then.
pub const RESTART_HINT: &str = "restart tuicr to try again";
/// Why there is no pane after a call into it panicked.
pub const CRASHED: &str = "the embedded editor crashed";

thread_local! {
    /// How many calls into the pane this thread is inside ([`contain`]).
    static CONTAINING: Cell<usize> = const { Cell::new(0) };
    /// What the panic hook made of a panic inside one of those calls.
    static CONTAINED_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// For the process's panic hook: whether the panic it was called for is
/// inside a call into the editor pane on this thread, which the
/// [`PaneSlot`] catches. The review goes on after such a panic, so the
/// hook must leave the terminal alone and print nothing; it still logs.
///
/// `describe` is the hook's one-line account of the panic (the payload
/// alone does not say where it was). It is kept for the status line and
/// only called for a contained panic.
pub fn note_contained_panic(describe: impl FnOnce() -> String) -> bool {
    let contained = CONTAINING
        .try_with(|depth| depth.get() > 0)
        .unwrap_or(false);
    if contained {
        let text = describe();
        let _ = CONTAINED_PANIC.try_with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut() {
                *slot = Some(text);
            }
        });
    }
    contained
}

/// Run a call into the pane so that a panic inside it comes back as an
/// error: what the panic hook noted, or failing that the panic's message.
fn contain<T>(call: impl FnOnce() -> T) -> Result<T, String> {
    struct Depth;
    impl Drop for Depth {
        fn drop(&mut self) {
            CONTAINING.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
    CONTAINING.with(|depth| depth.set(depth.get() + 1));
    let _depth = Depth;
    // The pane is dropped after a panic and never called again, so no
    // state the unwinding left half changed is ever looked at.
    catch_unwind(AssertUnwindSafe(call)).map_err(|payload| {
        let noted = CONTAINED_PANIC.with(|slot| slot.borrow_mut().take());
        noted.unwrap_or_else(|| {
            let message = payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned());
            match message {
                Some(message) => format!("panicked: {message}"),
                None => "panicked".to_string(),
            }
        })
    })
}

/// The pane as the slot holds it. Every call goes through [`contain`]: a
/// panic inside the editor drops the editor, and from then on each method
/// answers as a pane that has closed, so that tuicr goes back to the
/// review instead of going down with every comment the user has not
/// exported yet.
struct Contained {
    /// `None` once a call panicked.
    pane: RefCell<Option<Box<dyn EditorPane>>>,
    /// The panic that ended the pane, until [`PaneSlot::take_crash`].
    crash: RefCell<Option<String>>,
}

impl Contained {
    fn new(pane: Box<dyn EditorPane>) -> Self {
        Self {
            pane: RefCell::new(Some(pane)),
            crash: RefCell::new(None),
        }
    }

    /// Run `call` on the pane. `None` when the pane has crashed, in this
    /// call or an earlier one.
    fn call<T>(&self, call: impl FnOnce(&mut dyn EditorPane) -> T) -> Option<T> {
        let mut slot = self.pane.borrow_mut();
        let pane = slot.as_mut()?;
        match contain(|| call(pane.as_mut())) {
            Ok(value) => Some(value),
            Err(panic) => {
                // Dropping it stops its language servers. That runs the
                // editor's code once more, so it is contained as well.
                let pane = slot.take();
                let _ = contain(move || drop(pane));
                self.crash.borrow_mut().get_or_insert(panic);
                None
            }
        }
    }

    /// What an input call answers once the pane is gone: the host leaves
    /// Editor mode, as it does when the editor closes its last view.
    fn closed() -> PaneOutcome {
        PaneOutcome {
            redraw: true,
            close_requested: true,
            ..PaneOutcome::default()
        }
    }
}

impl EditorPane for Contained {
    fn open(&mut self, path: &Path, line: Option<usize>) -> anyhow::Result<PaneOutcome> {
        self.call(|pane| pane.open(path, line))
            .unwrap_or_else(|| Err(anyhow::anyhow!("{CRASHED}; {RESTART_HINT}")))
    }

    fn handle_key(&mut self, key: KeyEvent) -> PaneOutcome {
        self.call(|pane| pane.handle_key(key))
            .unwrap_or_else(Self::closed)
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> PaneOutcome {
        self.call(|pane| pane.handle_mouse(mouse, area))
            .unwrap_or_else(Self::closed)
    }

    fn handle_paste(&mut self, text: &str) -> PaneOutcome {
        self.call(|pane| pane.handle_paste(text))
            .unwrap_or_else(Self::closed)
    }

    fn focus_changed(&mut self, focused: bool) -> PaneOutcome {
        self.call(|pane| pane.focus_changed(focused))
            .unwrap_or_default()
    }

    fn working_dir_changed(&mut self, dir: &Path) -> PaneOutcome {
        self.call(|pane| pane.working_dir_changed(dir))
            .unwrap_or_default()
    }

    fn pump(&mut self) -> PaneOutcome {
        self.call(|pane| pane.pump()).unwrap_or_default()
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) -> Option<PaneCursor> {
        self.call(|pane| pane.render(area, buf)).flatten()
    }

    /// Nothing is unsaved in an editor that is gone: its buffers went
    /// with it, and a quit has nothing left to wait for.
    fn state(&self) -> PaneState {
        self.call(|pane| pane.state()).unwrap_or_default()
    }

    fn shutdown(self: Box<Self>) -> Vec<anyhow::Error> {
        let Some(pane) = self.pane.borrow_mut().take() else {
            return Vec::new();
        };
        contain(move || pane.shutdown()).unwrap_or_else(|panic| {
            vec![anyhow::anyhow!(
                "the embedded editor {panic} while shutting down"
            )]
        })
    }
}

enum SlotState {
    /// Nothing says how to build a pane.
    Empty,
    /// A factory is installed and has not built a pane: it has not been
    /// run, or every run so far failed in a way it called retryable.
    Factory(PaneFactory),
    Ready(Contained),
    /// The factory failed for good, or the pane was shut down. Final: the
    /// editor is constructed at most once per process. The text is what
    /// the status line says when the host falls back to `$EDITOR`.
    Gone(String),
}

/// Holds the process's one [`EditorPane`]: nothing until first use, then
/// the pane (or the reason it could not be built) for the rest of the run.
pub struct PaneSlot {
    state: SlotState,
}

impl Default for PaneSlot {
    fn default() -> Self {
        Self {
            state: SlotState::Empty,
        }
    }
}

impl PaneSlot {
    /// A slot that will build its pane with `factory` when first asked.
    pub fn with_factory(factory: PaneFactory) -> Self {
        Self {
            state: SlotState::Factory(factory),
        }
    }

    /// Say how the pane is built. Allowed until a factory has settled the
    /// matter: returns `false`, changing nothing, once a pane exists,
    /// failed to start for good, or was shut down.
    pub fn install(&mut self, factory: PaneFactory) -> bool {
        match self.state {
            SlotState::Empty | SlotState::Factory(_) => {
                self.state = SlotState::Factory(factory);
                true
            }
            SlotState::Ready(_) | SlotState::Gone(_) => false,
        }
    }

    /// The pane, building it when there is none yet. `size` is used only
    /// for building.
    ///
    /// The error is the reason there is no pane. After a failure the
    /// factory called retryable ([`PaneStartError::retryable`]) the next
    /// call runs the factory again, so a user who fixed their editor config
    /// gets the editor without restarting tuicr. After a final one the
    /// error is the same on every later call, and says a restart is needed.
    pub fn get_or_create(&mut self, size: Size) -> Result<&mut dyn EditorPane, String> {
        let state = std::mem::replace(
            &mut self.state,
            SlotState::Gone("the embedded editor failed while starting".to_string()),
        );
        self.state = match state {
            // A factory that panics has used its one chance up.
            SlotState::Factory(mut factory) => match contain(|| factory(size)) {
                Ok(Ok(pane)) => SlotState::Ready(Contained::new(pane)),
                Ok(Err(err)) if err.can_retry() => {
                    self.state = SlotState::Factory(factory);
                    return Err(err.to_string());
                }
                Ok(Err(err)) => SlotState::Gone(format!("{err}; {RESTART_HINT}")),
                Err(panic) => SlotState::Gone(format!(
                    "the embedded editor {panic} while starting; {RESTART_HINT}"
                )),
            },
            other => other,
        };
        match &mut self.state {
            SlotState::Ready(pane) => Ok(pane),
            SlotState::Gone(reason) => Err(reason.clone()),
            SlotState::Empty | SlotState::Factory(_) => Err(NO_FACTORY.to_string()),
        }
    }

    /// The pane, if it has been built.
    pub fn get(&self) -> Option<&dyn EditorPane> {
        match &self.state {
            SlotState::Ready(pane) => Some(pane),
            _ => None,
        }
    }

    /// The pane, if it has been built. Never builds one.
    pub fn get_mut(&mut self) -> Option<&mut dyn EditorPane> {
        match &mut self.state {
            SlotState::Ready(pane) => Some(pane),
            _ => None,
        }
    }

    /// `Some`, once, after a call into the pane panicked: what the panic
    /// was, on one line. The pane was dropped when it happened, and with
    /// it every buffer it held; the slot has none from here on, and says
    /// so with [`CRASHED`] when asked for one.
    ///
    /// Between the panic and this call the dead pane answers every call as
    /// one that has closed its last view, so a host that only looks here
    /// once per pass of its loop misses nothing.
    pub fn take_crash(&mut self) -> Option<String> {
        let SlotState::Ready(pane) = &self.state else {
            return None;
        };
        let panic = pane.crash.borrow_mut().take()?;
        self.state = SlotState::Gone(format!("{CRASHED}; {RESTART_HINT}"));
        Some(panic)
    }

    /// Whether this process was ever told how to build a pane. `false`
    /// means there is no embedded editor to speak of, which is not a
    /// failure anyone needs telling about; it stays `true` after the
    /// factory failed or the pane shut down.
    pub fn was_installed(&self) -> bool {
        !matches!(self.state, SlotState::Empty)
    }

    /// Whether a pane exists.
    pub fn is_ready(&self) -> bool {
        matches!(self.state, SlotState::Ready(_))
    }

    /// Whether asking for the pane could succeed: it exists, or there is a
    /// factory that may still build it.
    pub fn is_available(&self) -> bool {
        matches!(self.state, SlotState::Factory(_) | SlotState::Ready(_))
    }

    /// Why asking for the pane would fail, when it would.
    pub fn unavailable_reason(&self) -> Option<&str> {
        match &self.state {
            SlotState::Empty => Some(NO_FACTORY),
            SlotState::Gone(reason) => Some(reason),
            SlotState::Factory(_) | SlotState::Ready(_) => None,
        }
    }

    /// End the pane, if one was built, and report what it could not flush.
    /// A factory that was never used is dropped unused. Either way the slot
    /// has no pane from here on.
    pub fn shutdown(&mut self) -> Vec<anyhow::Error> {
        match std::mem::replace(&mut self.state, SlotState::Gone(SHUT_DOWN.to_string())) {
            SlotState::Ready(pane) => Box::new(pane).shutdown(),
            SlotState::Gone(reason) => {
                // Keep the first reason: it says more than "shut down".
                self.state = SlotState::Gone(reason);
                Vec::new()
            }
            SlotState::Empty | SlotState::Factory(_) => Vec::new(),
        }
    }
}

/// Whether `key` is the chord tuicr keeps for itself while the pane has
/// focus: `Ctrl-g`, which goes back to the review. It is never forwarded.
/// Helix binds nothing to it by default.
///
/// The chord is matched by the character the terminal reports, like every
/// binding of the editor itself. Lock keys do not matter (crossterm keeps
/// caps lock and num lock out of `modifiers`), and legacy reporting sends
/// the same byte whatever the layout. Known limit: under kitty key
/// reporting with a non-Latin layout active, the key in G's position
/// reports that layout's letter (`CSI 1087::103;5u` on a Russian one, and
/// crossterm keeps the 1087 and drops the base-layout 103), so the chord
/// is not seen until the layout is switched back. Closing the editor's
/// last view (`:q`) leaves too, under the same condition.
pub fn is_return_chord(key: &KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    key.code == KeyCode::Char('g') && key.modifiers == KeyModifiers::CONTROL
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix::fake::{FakePane, PaneCall};
    use crossterm::event::{KeyCode, KeyModifiers};

    const SIZE: Size = Size {
        width: 80,
        height: 24,
    };

    #[test]
    fn should_have_no_pane_without_a_factory() {
        let mut slot = PaneSlot::default();

        assert!(!slot.is_available());
        assert!(!slot.was_installed());
        assert_eq!(slot.get_or_create(SIZE).err().as_deref(), Some(NO_FACTORY));
        assert!(slot.get_mut().is_none());
    }

    #[test]
    fn should_remember_a_factory_was_installed_whatever_became_of_it() {
        let (factory, _handle) = FakePane::factory();
        let mut built = PaneSlot::with_factory(factory);
        assert!(built.was_installed(), "before first use");
        built.get_or_create(SIZE).expect("pane");
        assert!(built.was_installed());
        built.shutdown();
        assert!(built.was_installed(), "after shutdown");

        let mut failed = PaneSlot::with_factory(FakePane::failing_factory("no runtime directory"));
        let _ = failed.get_or_create(SIZE);
        assert!(failed.was_installed(), "after a failed start");
    }

    #[test]
    fn should_build_the_pane_once_at_the_size_first_asked_for() {
        let (factory, handle) = FakePane::factory();
        let mut slot = PaneSlot::with_factory(factory);
        assert!(slot.is_available());
        assert!(!slot.is_ready(), "nothing is built until first use");

        assert!(slot.get_or_create(SIZE).is_ok());
        let other = Size {
            width: 10,
            height: 5,
        };
        assert!(slot.get_or_create(other).is_ok());

        assert!(slot.is_ready());
        assert_eq!(
            handle.borrow().created_at,
            vec![SIZE],
            "the factory runs once; a second ask reuses the pane"
        );
    }

    #[test]
    fn should_remember_why_the_factory_failed_for_good_and_not_retry() {
        let mut slot = PaneSlot::with_factory(FakePane::failing_factory("no runtime directory"));

        let first = slot.get_or_create(SIZE).err();
        let second = slot.get_or_create(SIZE).err();

        assert_eq!(
            first.as_deref(),
            Some("no runtime directory; restart tuicr to try again")
        );
        assert_eq!(second, first);
        assert_eq!(slot.unavailable_reason(), first.as_deref());
        assert!(!slot.is_available());
    }

    #[test]
    fn should_run_the_factory_again_after_a_failure_it_called_retryable() {
        // A typo in the editor's config fails before anything is built.
        // The user fixes the file and presses `e` again.
        let (factory, handle) = FakePane::factory();
        handle
            .borrow_mut()
            .start_errors
            .push_back(PaneStartError::retryable(anyhow::anyhow!(
                "bad config config.toml: expected `=`"
            )));
        let mut slot = PaneSlot::with_factory(factory);

        let first = slot.get_or_create(SIZE).err();

        assert_eq!(
            first.as_deref(),
            Some("bad config config.toml: expected `=`"),
            "no restart is needed, so none is asked for"
        );
        assert!(slot.is_available(), "the factory is kept");
        assert!(!slot.is_ready());
        assert_eq!(slot.unavailable_reason(), None);
        assert!(handle.borrow().created_at.is_empty());

        assert!(slot.get_or_create(SIZE).is_ok());
        assert!(slot.get_or_create(SIZE).is_ok());

        assert_eq!(handle.borrow().created_at, vec![SIZE], "built once");
    }

    #[test]
    fn should_stop_asking_once_a_retried_start_fails_for_good() {
        let (factory, handle) = FakePane::factory();
        {
            let mut script = handle.borrow_mut();
            script
                .start_errors
                .push_back(PaneStartError::retryable(anyhow::anyhow!("bad config")));
            script
                .start_errors
                .push_back(PaneStartError::fatal(anyhow::anyhow!("no runtime")));
        }
        let mut slot = PaneSlot::with_factory(factory);

        let _ = slot.get_or_create(SIZE);
        let second = slot.get_or_create(SIZE).err();
        let third = slot.get_or_create(SIZE).err();

        assert_eq!(
            second.as_deref(),
            Some("no runtime; restart tuicr to try again")
        );
        assert_eq!(third, second);
        assert!(!slot.is_available());
        assert!(handle.borrow().created_at.is_empty());
    }

    #[test]
    fn should_treat_a_plain_error_from_a_factory_as_final() {
        // `?` on an `anyhow::Result` inside a factory must not promise a
        // retry nobody thought about: the editor is built once per process.
        let error = PaneStartError::from(anyhow::anyhow!("inner").context("outer"));

        assert!(!error.can_retry());
        assert_eq!(error.to_string(), "outer: inner");
        assert!(PaneStartError::retryable(anyhow::anyhow!("x")).can_retry());
    }

    #[test]
    fn should_take_a_factory_only_until_one_has_been_run() {
        let mut untried = PaneSlot::default();
        let (first, first_handle) = FakePane::factory();
        let (second, second_handle) = FakePane::factory();
        assert!(untried.install(first));
        assert!(untried.install(second), "an unused factory can be replaced");
        untried.get_or_create(SIZE).expect("pane");
        assert!(first_handle.borrow().created_at.is_empty());
        assert_eq!(second_handle.borrow().created_at, vec![SIZE]);

        let (late, late_handle) = FakePane::factory();
        assert!(!untried.install(late), "a pane exists");

        let mut failed = PaneSlot::with_factory(FakePane::failing_factory("no runtime directory"));
        let _ = failed.get_or_create(SIZE);
        let (retry, _) = FakePane::factory();
        assert!(!failed.install(retry), "the editor starts once per process");

        let (flaky, flaky_handle) = FakePane::factory();
        flaky_handle
            .borrow_mut()
            .start_errors
            .push_back(PaneStartError::retryable(anyhow::anyhow!("bad config")));
        let mut retrying = PaneSlot::with_factory(flaky);
        let _ = retrying.get_or_create(SIZE);
        let (other, other_handle) = FakePane::factory();
        assert!(
            retrying.install(other),
            "nothing was built, so the factory can still be replaced"
        );
        retrying.get_or_create(SIZE).expect("pane");
        assert_eq!(other_handle.borrow().created_at, vec![SIZE]);

        let mut closed = PaneSlot::default();
        closed.shutdown();
        let (after, _) = FakePane::factory();
        assert!(!closed.install(after));
        assert!(late_handle.borrow().created_at.is_empty());
    }

    #[test]
    fn should_shut_the_pane_down_once_and_keep_the_slot_empty() {
        let (factory, handle) = FakePane::factory();
        let mut slot = PaneSlot::with_factory(factory);
        slot.get_or_create(SIZE).expect("pane");
        handle
            .borrow_mut()
            .shutdown_errors
            .push("could not write a.rs".to_string());

        let errors = slot.shutdown();

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].to_string(), "could not write a.rs");
        assert_eq!(handle.borrow().calls.last(), Some(&PaneCall::Shutdown));
        assert_eq!(slot.get_or_create(SIZE).err().as_deref(), Some(SHUT_DOWN));
        assert!(slot.shutdown().is_empty(), "a second shutdown has no pane");
    }

    #[test]
    fn should_keep_the_failure_reason_across_shutdown() {
        let mut slot = PaneSlot::with_factory(FakePane::failing_factory("no runtime directory"));
        let _ = slot.get_or_create(SIZE);

        assert!(slot.shutdown().is_empty());

        assert_eq!(
            slot.unavailable_reason(),
            Some("no runtime directory; restart tuicr to try again")
        );
    }

    // --- a panic inside the editor ends the editor, not tuicr -------------

    fn pane_in(slot: &mut PaneSlot) -> &mut dyn EditorPane {
        slot.get_mut().expect("a pane")
    }

    #[test]
    fn should_end_the_pane_and_not_the_process_when_a_key_panics_in_it() {
        let (factory, handle) = FakePane::factory();
        let mut slot = PaneSlot::with_factory(factory);
        slot.get_or_create(SIZE).expect("pane");
        handle.borrow_mut().panic_in.push("key");
        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);

        let outcome = pane_in(&mut slot).handle_key(key);

        assert!(
            outcome.close_requested && outcome.redraw,
            "the host goes back to the review: {outcome:?}"
        );
        // Until the host has asked, the dead pane answers as a closed one
        // and is not called again.
        let calls = handle.borrow().calls.len();
        assert!(pane_in(&mut slot).handle_key(key).close_requested);
        assert_eq!(pane_in(&mut slot).pump(), PaneOutcome::default());
        assert_eq!(pane_in(&mut slot).state(), PaneState::default());
        assert!(
            pane_in(&mut slot)
                .open(Path::new("/repo/a.rs"), None)
                .is_err()
        );
        assert_eq!(handle.borrow().calls.len(), calls, "nothing reaches it");

        let panic = slot.take_crash().expect("the crash is reported");
        assert!(panic.contains("the fake editor panicked in key"), "{panic}");
        assert_eq!(slot.take_crash(), None, "once");
        assert!(slot.get_mut().is_none());
        assert!(!slot.is_available());
        assert_eq!(
            slot.get_or_create(SIZE).err().as_deref(),
            Some("the embedded editor crashed; restart tuicr to try again")
        );
        assert!(slot.was_installed());
        assert!(slot.shutdown().is_empty(), "nothing is left to shut down");
        assert!(
            !handle.borrow().calls.contains(&PaneCall::Shutdown),
            "the pane was dropped, not asked to flush state nobody can trust"
        );
    }

    #[test]
    fn should_contain_a_panic_in_every_call_into_the_pane() {
        let area = Rect::new(0, 0, 80, 24);
        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        let mouse = MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        for call in [
            "open",
            "key",
            "mouse",
            "paste",
            "focus",
            "working_dir",
            "pump",
            "render",
            "state",
        ] {
            let (factory, handle) = FakePane::factory();
            let mut slot = PaneSlot::with_factory(factory);
            slot.get_or_create(SIZE).expect("pane");
            handle.borrow_mut().panic_in.push(call);
            let pane = pane_in(&mut slot);
            let mut buf = Buffer::empty(area);

            match call {
                "open" => assert!(pane.open(Path::new("/repo/a.rs"), None).is_err()),
                "key" => assert!(pane.handle_key(key).close_requested),
                "mouse" => assert!(pane.handle_mouse(mouse, area).close_requested),
                "paste" => assert!(pane.handle_paste("text").close_requested),
                "focus" => assert_eq!(pane.focus_changed(true), PaneOutcome::default()),
                "working_dir" => assert_eq!(
                    pane.working_dir_changed(Path::new("/elsewhere")),
                    PaneOutcome::default()
                ),
                "pump" => assert_eq!(pane.pump(), PaneOutcome::default()),
                "render" => assert_eq!(pane.render(area, &mut buf), None),
                "state" => assert_eq!(pane.state(), PaneState::default()),
                other => unreachable!("{other}"),
            }

            let panic = slot
                .take_crash()
                .unwrap_or_else(|| panic!("{call}: no crash"));
            assert!(panic.contains(call), "{call}: {panic}");
            assert_eq!(
                slot.unavailable_reason().map(|r| r.contains(CRASHED)),
                Some(true)
            );
        }
    }

    #[test]
    fn should_survive_a_pane_that_also_panics_while_it_is_dropped() {
        let (factory, handle) = FakePane::factory();
        let mut slot = PaneSlot::with_factory(factory);
        slot.get_or_create(SIZE).expect("pane");
        handle.borrow_mut().panic_in.extend(["pump", "drop"]);

        assert_eq!(pane_in(&mut slot).pump(), PaneOutcome::default());

        let panic = slot.take_crash().expect("the first panic is the one told");
        assert!(panic.contains("pump"), "{panic}");
    }

    #[test]
    fn should_call_a_factory_that_panics_a_start_that_failed_for_good() {
        let (factory, handle) = FakePane::factory();
        handle.borrow_mut().panic_in.push("start");
        let mut slot = PaneSlot::with_factory(factory);

        let first = slot.get_or_create(SIZE).err().expect("no pane");

        assert!(
            first.starts_with("the embedded editor panicked")
                && first.contains("the fake editor panicked in start")
                && first.ends_with("while starting; restart tuicr to try again"),
            "{first}"
        );
        assert_eq!(slot.get_or_create(SIZE).err(), Some(first), "not retried");
        assert!(handle.borrow().created_at.is_empty());
        assert_eq!(slot.take_crash(), None, "there never was a pane to crash");
    }

    #[test]
    fn should_report_a_panic_while_shutting_down_as_what_could_not_be_flushed() {
        let (factory, handle) = FakePane::factory();
        let mut slot = PaneSlot::with_factory(factory);
        slot.get_or_create(SIZE).expect("pane");
        handle.borrow_mut().panic_in.push("shutdown");

        let errors = slot.shutdown();

        assert_eq!(errors.len(), 1);
        let error = errors[0].to_string();
        assert!(
            error.contains("the fake editor panicked in shutdown")
                && error.ends_with("while shutting down"),
            "{error}"
        );
    }

    #[test]
    fn should_tell_the_panic_hook_only_about_panics_inside_the_pane() {
        // Outside a call into the pane a panic is the hook's to handle as
        // ever, and it is not asked to describe it.
        assert!(!note_contained_panic(|| unreachable!("not asked")));

        // Inside one, the hook's account of it is what the host is told.
        let caught = contain(|| {
            assert!(note_contained_panic(|| {
                "panicked at editor.rs:1:1: no row".to_string()
            }));
            panic!("no row");
        });
        assert_eq!(
            caught.err().as_deref(),
            Some("panicked at editor.rs:1:1: no row")
        );
        assert!(!note_contained_panic(|| unreachable!("over")));

        // Without a hook that says more, the panic's own message.
        let caught: Result<(), String> = contain(|| panic!("just this"));
        assert_eq!(caught.err().as_deref(), Some("panicked: just this"));
    }

    #[test]
    fn should_reserve_only_plain_ctrl_g() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);

        assert!(is_return_chord(&key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_return_chord(&key(
            KeyCode::Char('g'),
            KeyModifiers::NONE
        )));
        assert!(!is_return_chord(&key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
        assert!(!is_return_chord(&key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn should_know_the_chord_with_a_lock_key_on_and_only_by_its_latin_letter() {
        use crossterm::event::{KeyEventKind, KeyEventState};

        // kitty reports caps lock and num lock with the key; crossterm
        // files them under `state`, so the chord still matches.
        for state in [KeyEventState::CAPS_LOCK, KeyEventState::NUM_LOCK] {
            assert!(is_return_chord(&KeyEvent::new_with_kind_and_state(
                KeyCode::Char('g'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
                state,
            )));
        }

        // The documented limit: with a Russian layout active, kitty key
        // reporting sends the key in G's position as its own letter, and
        // crossterm drops the base-layout key that would say it is G. The
        // key goes to the editor like any other.
        assert!(!is_return_chord(&KeyEvent::new(
            KeyCode::Char('\u{43f}'),
            KeyModifiers::CONTROL
        )));
    }
}
