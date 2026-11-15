//! A scripted [`EditorPane`] for tests: it records every call, hands back
//! the outcomes a test queued, and draws text a test can look for.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::Style;

use super::pane::{
    CursorShape, EditorPane, PaneCursor, PaneFactory, PaneOutcome, PaneStartError, PaneState,
};

/// First row of every frame the fake draws, at the area's top-left.
pub(crate) const FAKE_BANNER: &str = "FAKE EDITOR";
/// Drawn in the area's bottom-right cell, so a test can see how far the
/// pane was allowed to draw.
pub(crate) const FAKE_CORNER: &str = "#";

/// One call the host made, in the order it made them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneCall {
    Open { path: PathBuf, line: Option<usize> },
    Key(KeyEvent),
    Mouse { event: MouseEvent, area: Rect },
    Paste(String),
    Focus(bool),
    WorkingDir(PathBuf),
    Pump,
    Render(Rect),
    Shutdown,
}

/// What a test reads back from the fake and scripts ahead of time. Shared
/// through an `Rc`, which also makes the fake `!Send` like the real pane.
#[derive(Debug, Default)]
pub(crate) struct FakeScript {
    /// Every call so far.
    pub calls: Vec<PaneCall>,
    /// Sizes the factory was run with: one entry per pane built.
    pub created_at: Vec<Size>,
    /// Failures to start: each run of the factory returns the next one
    /// before it builds anything.
    pub start_errors: VecDeque<PaneStartError>,
    /// Returned in order by `open`, `handle_key`, `handle_mouse` and
    /// `handle_paste`; a default outcome once it runs dry.
    pub outcomes: VecDeque<PaneOutcome>,
    /// Returned in order by `pump`; a default outcome once it runs dry. A
    /// pumped outcome that reports saves finishes those writes: `state`
    /// loses that many `pending_writes` and those paths from `modified`.
    pub pump_outcomes: VecDeque<PaneOutcome>,
    /// What a background save writes: when `pump` reports a path as saved,
    /// the fake first writes this to it, as an editor's `:w` lands on disk
    /// some pumps after it was typed.
    pub disk_on_save: HashMap<PathBuf, String>,
    /// What was on disk at each `open` that succeeded (`None` for a file
    /// that could not be read): what a real pane has to show for a buffer
    /// it holds without unsaved changes.
    pub disk_at_open: Vec<Option<String>>,
    /// Makes the next `open` fail with this message.
    pub open_error: Option<String>,
    /// Pane-local `(column, row, shape)` to report from `render`; `None`
    /// hides the cursor.
    pub cursor: Option<(u16, u16, CursorShape)>,
    /// Sizes the editor was resized to, one entry per change.
    pub resizes: Vec<Size>,
    /// What `state` reports. `open` fills in `path` and `line`.
    pub state: PaneState,
    /// What `shutdown` reports.
    pub shutdown_errors: Vec<String>,
    /// Calls that panic, as an editor with a bug does: any of `start`
    /// (the factory), `open`, `key`, `mouse`, `paste`, `focus`, `pump`,
    /// `working_dir`, `render`, `state`, `shutdown` and `drop`.
    pub panic_in: Vec<&'static str>,
}

impl FakeScript {
    /// The keys forwarded so far, in order.
    pub fn keys(&self) -> Vec<KeyEvent> {
        self.calls
            .iter()
            .filter_map(|call| match call {
                PaneCall::Key(key) => Some(*key),
                _ => None,
            })
            .collect()
    }

    /// How many times the pane was pumped.
    pub fn pumps(&self) -> usize {
        self.calls
            .iter()
            .filter(|call| matches!(call, PaneCall::Pump))
            .count()
    }

    /// Queue an outcome for the next input call.
    pub fn then(&mut self, outcome: PaneOutcome) {
        self.outcomes.push_back(outcome);
    }
}

/// A test's view of the fake after it has been boxed into the app.
pub(crate) type FakeHandle = Rc<RefCell<FakeScript>>;

pub(crate) struct FakePane {
    script: FakeHandle,
    size: Size,
}

impl FakePane {
    /// A factory for the app's pane slot, and the handle to watch it by.
    pub(crate) fn factory() -> (PaneFactory, FakeHandle) {
        let handle = FakeHandle::default();
        let script = Rc::clone(&handle);
        let factory: PaneFactory = Box::new(move |size| {
            if script.borrow().panic_in.contains(&"start") {
                panic!("the fake editor panicked in start");
            }
            if let Some(error) = script.borrow_mut().start_errors.pop_front() {
                return Err(error);
            }
            script.borrow_mut().created_at.push(size);
            let script = Rc::clone(&script);
            Ok(Box::new(FakePane { script, size }) as Box<dyn EditorPane>)
        });
        (factory, handle)
    }

    /// A factory whose editor cannot start, now or later.
    pub(crate) fn failing_factory(reason: &'static str) -> PaneFactory {
        Box::new(move |_| Err(PaneStartError::fatal(anyhow::anyhow!(reason))))
    }

    fn record(&self, call: PaneCall) {
        self.script.borrow_mut().calls.push(call);
    }

    /// Panic if the test scripted a bug into `call`.
    fn panic_if_scripted(&self, call: &'static str) {
        let scripted = self.script.borrow().panic_in.contains(&call);
        if scripted {
            panic!("the fake editor panicked in {call}");
        }
    }

    fn next_outcome(&self) -> PaneOutcome {
        self.script
            .borrow_mut()
            .outcomes
            .pop_front()
            .unwrap_or_default()
    }
}

impl EditorPane for FakePane {
    fn open(&mut self, path: &Path, line: Option<usize>) -> anyhow::Result<PaneOutcome> {
        self.record(PaneCall::Open {
            path: path.to_path_buf(),
            line,
        });
        self.panic_if_scripted("open");
        let mut script = self.script.borrow_mut();
        if let Some(message) = script.open_error.take() {
            return Err(anyhow::anyhow!(message));
        }
        script.disk_at_open.push(std::fs::read_to_string(path).ok());
        script.state.path = Some(path.to_path_buf());
        script.state.line = line.unwrap_or(1);
        script.state.col = 1;
        Ok(script.outcomes.pop_front().unwrap_or_default())
    }

    fn handle_key(&mut self, key: KeyEvent) -> PaneOutcome {
        self.record(PaneCall::Key(key));
        self.panic_if_scripted("key");
        self.next_outcome()
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> PaneOutcome {
        self.record(PaneCall::Mouse { event: mouse, area });
        self.panic_if_scripted("mouse");
        self.next_outcome()
    }

    fn handle_paste(&mut self, text: &str) -> PaneOutcome {
        self.record(PaneCall::Paste(text.to_string()));
        self.panic_if_scripted("paste");
        self.next_outcome()
    }

    fn focus_changed(&mut self, focused: bool) -> PaneOutcome {
        self.record(PaneCall::Focus(focused));
        self.panic_if_scripted("focus");
        PaneOutcome::default()
    }

    fn working_dir_changed(&mut self, dir: &Path) -> PaneOutcome {
        self.record(PaneCall::WorkingDir(dir.to_path_buf()));
        self.panic_if_scripted("working_dir");
        PaneOutcome::default()
    }

    fn pump(&mut self) -> PaneOutcome {
        self.record(PaneCall::Pump);
        self.panic_if_scripted("pump");
        let mut script = self.script.borrow_mut();
        let outcome = script.pump_outcomes.pop_front().unwrap_or_default();
        // A reported save is a write that finished: it is on disk and its
        // buffer is clean.
        for path in &outcome.saved {
            if let Some(content) = script.disk_on_save.remove(path) {
                std::fs::write(path, content).expect("the fake's save");
            }
        }
        let state = &mut script.state;
        state.pending_writes = state.pending_writes.saturating_sub(outcome.saved.len());
        state.modified.retain(|path| !outcome.saved.contains(path));
        outcome
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) -> Option<PaneCursor> {
        self.record(PaneCall::Render(area));
        self.panic_if_scripted("render");
        let mut script = self.script.borrow_mut();
        if area.as_size() != self.size {
            self.size = area.as_size();
            script.resizes.push(self.size);
        }
        if area.is_empty() {
            return None;
        }
        let title = match &script.state.path {
            Some(path) => format!("{FAKE_BANNER} {}:{}", path.display(), script.state.line),
            None => format!("{FAKE_BANNER} [scratch]"),
        };
        buf.set_stringn(area.x, area.y, title, area.width as usize, Style::default());
        buf.set_string(
            area.right() - 1,
            area.bottom() - 1,
            FAKE_CORNER,
            Style::default(),
        );
        script.cursor.map(|(col, row, shape)| PaneCursor {
            x: area.x + col,
            y: area.y + row,
            shape,
        })
    }

    fn state(&self) -> PaneState {
        self.panic_if_scripted("state");
        self.script.borrow().state.clone()
    }

    fn shutdown(self: Box<Self>) -> Vec<anyhow::Error> {
        self.record(PaneCall::Shutdown);
        self.panic_if_scripted("shutdown");
        self.script
            .borrow()
            .shutdown_errors
            .iter()
            .map(|message| anyhow::anyhow!(message.clone()))
            .collect()
    }
}

impl Drop for FakePane {
    fn drop(&mut self) {
        // Not on top of the panic that is dropping it: that would abort.
        if !std::thread::panicking() {
            self.panic_if_scripted("drop");
        }
    }
}
