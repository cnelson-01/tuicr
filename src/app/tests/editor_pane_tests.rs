//! Editor mode against a scripted pane: who gets which event, what the
//! frame shows, and when the pane's background work repaints the screen.
//! Then the hand-off around it: which files go to the pane and which stay
//! with `$EDITOR`, what the review does when the pane gives the screen
//! back, and the hold on quitting while it has unsaved buffers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect, Size};

use super::render_perf_tests::{app_with, file};
use crate::app::*;
use crate::editor::{EditorCommand, EditorTarget};
use crate::helix::fake::{FAKE_BANNER, FAKE_CORNER, FakeHandle, FakePane, PaneCall};
use crate::helix::{CursorShape, NO_FACTORY, PaneOutcome, PaneStartError, PaneState, SHUT_DOWN};
use crate::input::catalog::Prefix;
use crate::input::{Action, map_key_to_action};
use crate::model::{DiffFile, DiffLine, FileStatus};
use crate::vcs::traits::{ResolvedRevisionRange, VcsBackend, VcsInfo, VcsType};

const SCREEN: Size = Size {
    width: 80,
    height: 24,
};
const FULL: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

fn target(path: &str, line: Option<u32>) -> EditorTarget {
    EditorTarget {
        path: PathBuf::from(path),
        line,
        label: path.to_string(),
    }
}

/// An app that can build a fake pane but has not yet.
fn app_with_pane() -> (App, FakeHandle) {
    let mut app = app_with(vec![file("a.rs", 3)]);
    let (factory, pane) = FakePane::factory();
    assert!(app.install_editor_pane_factory(factory));
    (app, pane)
}

/// An app showing `/repo/a.rs:7` in the fake pane, opened from the diff.
fn app_in_editor() -> (App, FakeHandle) {
    let (mut app, pane) = app_with_pane();
    app.open_editor_pane(&target("/repo/a.rs", Some(7)), SCREEN)
        .expect("the fake pane opens anything");
    assert_eq!(app.input_mode, InputMode::Editor);
    (app, pane)
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn ctrl(c: char) -> Event {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn plain(c: char) -> Event {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

fn key_event(event: &Event) -> KeyEvent {
    match event {
        Event::Key(key) => *key,
        other => panic!("not a key: {other:?}"),
    }
}

/// What the event loop does with an event before any of its own handling:
/// `Some(needs_redraw)` when the pane took it, `None` when it is tuicr's.
fn offer(app: &mut App, event: Event) -> Option<bool> {
    app.editor_pane_takes(&event)
        .then(|| app.editor_pane_event(event))
}

fn draw_at(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .expect("draw");
    terminal
}

fn draw(app: &mut App) -> Terminal<TestBackend> {
    draw_at(app, SCREEN.width, SCREEN.height)
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

fn screen_text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n")
}

// --- the pane: built once, on first use ---------------------------------

#[test]
fn should_say_why_when_there_is_no_pane_to_open_in() {
    let mut app = app_with(vec![file("a.rs", 3)]);

    let refused = app.open_editor_pane(&target("/repo/a.rs", None), SCREEN);

    assert_eq!(refused, Err(NO_FACTORY.to_string()));
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(!app.editor_pane_available());
    assert_eq!(app.editor_pane_unavailable_reason(), Some(NO_FACTORY));
}

#[test]
fn should_say_why_when_the_pane_cannot_start_and_not_try_again() {
    let mut app = app_with(vec![file("a.rs", 3)]);
    app.install_editor_pane_factory(FakePane::failing_factory("helix runtime not found"));
    assert!(app.editor_pane_available(), "nothing has been tried yet");

    let first = app.open_editor_pane(&target("/repo/a.rs", None), SCREEN);
    let second = app.open_editor_pane(&target("/repo/a.rs", None), SCREEN);

    assert_eq!(
        first,
        Err("helix runtime not found; restart tuicr to try again".to_string())
    );
    assert_eq!(second, first);
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(!app.editor_pane_available());
}

#[test]
fn should_build_the_pane_on_first_open_and_give_it_the_screen() {
    let (mut app, pane) = app_with_pane();
    assert!(
        pane.borrow().created_at.is_empty(),
        "installing the factory must not start the editor"
    );
    assert!(app.editor_pane_state().is_none());

    app.open_editor_pane(&target("/repo/a.rs", Some(7)), SCREEN)
        .expect("open");

    assert_eq!(app.input_mode, InputMode::Editor);
    assert!(app.editor_pane_focused());
    assert_eq!(pane.borrow().created_at, vec![SCREEN]);
    assert_eq!(
        pane.borrow().calls,
        vec![
            PaneCall::Open {
                path: PathBuf::from("/repo/a.rs"),
                line: Some(7),
            },
            PaneCall::Focus(true),
        ]
    );
}

#[test]
fn should_keep_one_pane_for_every_visit() {
    let (mut app, pane) = app_in_editor();
    offer(&mut app, ctrl('g'));

    app.open_editor_pane(&target("/repo/b.rs", None), SCREEN)
        .expect("open");

    assert_eq!(app.input_mode, InputMode::Editor);
    assert_eq!(
        pane.borrow().created_at.len(),
        1,
        "the editor is constructed once per process"
    );
    assert_eq!(
        pane.borrow().calls[2..],
        [
            PaneCall::Focus(false),
            PaneCall::Open {
                path: PathBuf::from("/repo/b.rs"),
                line: None,
            },
            PaneCall::Focus(true),
        ]
    );
}

#[test]
fn should_not_replace_a_pane_that_already_exists() {
    let (mut app, pane) = app_in_editor();
    let (other, other_pane) = FakePane::factory();

    assert!(!app.install_editor_pane_factory(other));
    offer(&mut app, plain('x'));

    assert_eq!(pane.borrow().keys().len(), 1);
    assert!(other_pane.borrow().created_at.is_empty());
}

#[test]
fn should_stay_in_the_review_when_the_file_cannot_be_opened() {
    let (mut app, pane) = app_with_pane();
    pane.borrow_mut().open_error = Some("permission denied".to_string());

    let refused = app.open_editor_pane(&target("/repo/a.rs", None), SCREEN);

    assert_eq!(refused, Err("permission denied".to_string()));
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(
        !pane.borrow().calls.contains(&PaneCall::Focus(true)),
        "a pane that shows nothing new must not take focus"
    );
    assert!(
        app.editor_pane_available(),
        "one unreadable file does not take the editor away"
    );
}

#[test]
fn should_report_what_the_editor_has_not_saved() {
    let (app, pane) = app_in_editor();
    pane.borrow_mut().state.modified = vec![PathBuf::from("/repo/a.rs")];
    pane.borrow_mut().state.mode = "INS".to_string();

    let state = app.editor_pane_state().expect("a pane exists");

    assert_eq!(
        state,
        PaneState {
            path: Some(PathBuf::from("/repo/a.rs")),
            line: 7,
            col: 1,
            mode: "INS".to_string(),
            modified: vec![PathBuf::from("/repo/a.rs")],
            modified_scratch: 0,
            pending_writes: 0,
            notice: None,
        }
    );
}

// --- routing: the pane comes before every key tuicr handles itself -------

#[test]
fn should_send_ctrl_c_to_the_pane_and_not_arm_quit() {
    let (mut app, pane) = app_in_editor();

    let taken = offer(&mut app, ctrl('c'));

    assert!(taken.is_some(), "Ctrl-C must not reach the exit handler");
    assert_eq!(pane.borrow().keys(), vec![key_event(&ctrl('c'))]);
    assert!(!app.should_quit);
    assert!(app.message.is_none(), "no `Press Ctrl+C again` prompt");
    assert_eq!(app.input_mode, InputMode::Editor);
}

#[test]
fn should_send_ctrl_l_to_the_pane_and_not_redraw_tuicr() {
    let (mut app, pane) = app_in_editor();
    app.take_full_redraw();

    let taken = offer(&mut app, ctrl('l'));

    assert!(taken.is_some());
    assert_eq!(pane.borrow().keys(), vec![key_event(&ctrl('l'))]);
    assert!(
        !app.take_full_redraw(),
        "the global redraw key belongs to the editor here"
    );
}

#[test]
fn should_send_prefix_keys_to_the_pane_and_start_no_prefix() {
    let (mut app, pane) = app_in_editor();
    let leader = app.leader_key;

    for c in [leader, 'e', 'z', 'z', 'Z', 'Q', 'd', 'd'] {
        assert!(offer(&mut app, plain(c)).is_some(), "`{c}` is the pane's");
        assert_eq!(app.pending_prefix, None, "`{c}` must not arm a prefix");
    }

    let typed: Vec<KeyCode> = pane.borrow().keys().iter().map(|k| k.code).collect();
    assert_eq!(
        typed,
        [leader, 'e', 'z', 'z', 'Z', 'Q', 'd', 'd'].map(KeyCode::Char)
    );
    assert!(!app.should_quit, "`ZQ` quits tuicr only outside the editor");
    assert!(
        app.show_file_list,
        "`<leader>e` hides the tree only there too"
    );
}

#[test]
fn should_send_a_key_to_the_pane_even_with_a_prefix_left_pending() {
    // A prefix armed just before the editor opened must not swallow the
    // editor's first key.
    let (mut app, pane) = app_in_editor();
    app.pending_prefix = Some(Prefix::Leader);

    assert!(offer(&mut app, plain('c')).is_some());

    assert_eq!(pane.borrow().keys(), vec![key_event(&plain('c'))]);
    assert_eq!(app.input_mode, InputMode::Editor);
}

#[test]
fn should_send_tuicr_s_other_keys_to_the_pane() {
    let (mut app, pane) = app_in_editor();
    let keys = [
        plain('q'),
        plain(':'),
        plain('?'),
        plain('/'),
        ctrl('p'),
        key(KeyCode::Esc, KeyModifiers::NONE),
        key(KeyCode::Enter, KeyModifiers::NONE),
        key(KeyCode::BackTab, KeyModifiers::SHIFT),
        key(KeyCode::Char('g'), KeyModifiers::NONE),
        key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ),
    ];

    for event in keys.clone() {
        assert!(offer(&mut app, event).is_some());
    }

    let sent: Vec<KeyEvent> = keys.iter().map(key_event).collect();
    assert_eq!(pane.borrow().keys(), sent, "raw, unmapped and in order");
    assert_eq!(app.input_mode, InputMode::Editor);
    assert!(app.palette.is_none());
    assert!(!app.should_quit);
}

#[test]
fn should_keep_ctrl_g_and_go_back_to_the_review() {
    let (mut app, pane) = app_in_editor();

    let taken = offer(&mut app, ctrl('g'));

    assert_eq!(taken, Some(true), "the review has to be painted");
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(pane.borrow().keys().is_empty(), "Ctrl-g is never forwarded");
    assert_eq!(pane.borrow().calls.last(), Some(&PaneCall::Focus(false)));
}

#[test]
fn should_go_back_to_the_mode_the_editor_was_opened_from() {
    let (mut app, _pane) = app_with_pane();
    app.input_mode = InputMode::FindResults;
    app.open_editor_pane(&target("/repo/a.rs", None), SCREEN)
        .expect("open");

    // Opening another file from inside the editor keeps the way back.
    app.open_editor_pane(&target("/repo/b.rs", None), SCREEN)
        .expect("open");
    offer(&mut app, ctrl('g'));

    assert_eq!(app.input_mode, InputMode::FindResults);
}

#[test]
fn should_go_back_to_the_diff_when_the_merge_view_ended_meanwhile() {
    let (mut app, _pane) = app_with_pane();
    app.input_mode = InputMode::MergeConflict;
    app.open_editor_pane(&target("/repo/a.rs", None), SCREEN)
        .expect("open");
    assert!(app.merge_view.is_none());

    offer(&mut app, ctrl('g'));

    assert_eq!(app.input_mode, InputMode::Normal);
}

#[test]
fn should_drop_key_releases_and_forward_repeats() {
    let (mut app, pane) = app_in_editor();
    let release = KeyEvent::new_with_kind(
        KeyCode::Char('j'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    let repeat =
        KeyEvent::new_with_kind(KeyCode::Char('j'), KeyModifiers::NONE, KeyEventKind::Repeat);

    let released = offer(&mut app, Event::Key(release));
    let repeated = offer(&mut app, Event::Key(repeat));

    assert_eq!(
        released,
        Some(false),
        "a release is the pane's to drop: tuicr must not act on it either"
    );
    assert!(repeated.is_some());
    assert_eq!(pane.borrow().keys(), vec![repeat]);
}

#[test]
fn should_not_treat_the_release_of_ctrl_g_as_the_chord() {
    let (mut app, _pane) = app_in_editor();
    let release = KeyEvent::new_with_kind(
        KeyCode::Char('g'),
        KeyModifiers::CONTROL,
        KeyEventKind::Release,
    );

    offer(&mut app, Event::Key(release));

    assert_eq!(app.input_mode, InputMode::Editor);
}

#[test]
fn should_leave_every_event_to_tuicr_outside_editor_mode() {
    let (mut app, pane) = app_in_editor();
    offer(&mut app, ctrl('g'));
    let calls = pane.borrow().calls.len();

    for event in [
        ctrl('c'),
        plain('j'),
        Event::Paste("text".to_string()),
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }),
    ] {
        assert_eq!(offer(&mut app, event), None);
    }

    assert_eq!(
        pane.borrow().calls.len(),
        calls,
        "a hidden pane gets no input"
    );
}

#[test]
fn should_leave_resizes_to_tuicr() {
    let (app, _pane) = app_in_editor();

    assert!(!app.editor_pane_takes(&Event::Resize(100, 30)));
}

#[test]
fn should_map_no_action_in_editor_mode() {
    // The key maps are never consulted for the editor; if one is, nothing
    // of tuicr's may run.
    for code in [KeyCode::Char('q'), KeyCode::Char('e'), KeyCode::Esc] {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(map_key_to_action(key, InputMode::Editor, ';'), Action::None);
    }
}

// --- paste and mouse ----------------------------------------------------

#[test]
fn should_forward_a_paste_whole() {
    let (mut app, pane) = app_in_editor();

    let taken = offer(&mut app, Event::Paste("fn main() {\n}\n".to_string()));

    assert!(taken.is_some());
    assert_eq!(
        pane.borrow().calls.last(),
        Some(&PaneCall::Paste("fn main() {\n}\n".to_string()))
    );
}

#[test]
fn should_forward_the_mouse_with_the_rect_the_pane_was_drawn_in() {
    let (mut app, pane) = app_in_editor();
    draw(&mut app);
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 12,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };

    let taken = offer(&mut app, Event::Mouse(click));

    assert!(taken.is_some());
    assert_eq!(
        pane.borrow().calls.last(),
        Some(&PaneCall::Mouse {
            event: click,
            area: FULL,
        }),
        "screen coordinates plus the pane's rect: the pane makes them local"
    );
}

#[test]
fn should_follow_the_frame_size_for_the_mouse_rect() {
    let (mut app, pane) = app_in_editor();
    draw(&mut app);
    draw_at(&mut app, 100, 30);
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 90,
        row: 28,
        modifiers: KeyModifiers::NONE,
    };

    offer(&mut app, Event::Mouse(wheel));

    assert_eq!(
        pane.borrow().calls.last(),
        Some(&PaneCall::Mouse {
            event: wheel,
            area: Rect::new(0, 0, 100, 30),
        })
    );
}

#[test]
fn should_drop_the_mouse_before_the_pane_has_been_drawn() {
    let (mut app, pane) = app_in_editor();
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 12,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };

    let taken = offer(&mut app, Event::Mouse(click));

    assert_eq!(
        taken,
        Some(false),
        "taken from tuicr, but there is no rect yet"
    );
    assert!(
        !pane
            .borrow()
            .calls
            .iter()
            .any(|call| matches!(call, PaneCall::Mouse { .. }))
    );
}

// --- rendering ----------------------------------------------------------

#[test]
fn should_give_the_pane_the_whole_frame() {
    let (mut app, pane) = app_in_editor();

    let terminal = draw(&mut app);

    let buffer = terminal.backend().buffer();
    assert!(
        row_text(buffer, 0).starts_with(&format!("{FAKE_BANNER} /repo/a.rs:7")),
        "top row: {:?}",
        row_text(buffer, 0)
    );
    assert_eq!(
        buffer[(79, 23)].symbol(),
        FAKE_CORNER,
        "the pane reaches the last cell: no status bar under it"
    );
    assert_eq!(pane.borrow().calls.last(), Some(&PaneCall::Render(FULL)));
    // The fake draws its banner and its corner mark and nothing else, so
    // anything more would be tuicr's header, tree, diff or status bar.
    for y in 1..23 {
        assert_eq!(row_text(buffer, y).trim(), "", "row {y} is the pane's");
    }
    assert_eq!(row_text(buffer, 23).trim(), FAKE_CORNER);
}

#[test]
fn should_draw_the_review_again_after_leaving() {
    let (mut app, pane) = app_in_editor();
    draw(&mut app);
    offer(&mut app, ctrl('g'));
    let renders = pane.borrow().calls.len();

    let terminal = draw(&mut app);

    let screen = screen_text(terminal.backend().buffer());
    assert!(!screen.contains(FAKE_BANNER));
    assert!(
        screen.contains("NORMAL"),
        "the status bar is back: {screen}"
    );
    assert_eq!(
        pane.borrow().calls.len(),
        renders,
        "a hidden pane is not drawn"
    );
}

#[test]
fn should_resize_the_pane_to_the_frame_only_when_it_changes() {
    let (mut app, pane) = app_in_editor();

    draw(&mut app);
    draw(&mut app);
    assert!(
        pane.borrow().resizes.is_empty(),
        "built at this size already"
    );

    draw_at(&mut app, 100, 30);
    draw_at(&mut app, 100, 30);

    assert_eq!(
        pane.borrow().resizes,
        vec![Size {
            width: 100,
            height: 30
        }]
    );
}

#[test]
fn should_place_the_cursor_where_the_pane_says() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().cursor = Some((5, 3, CursorShape::Bar));

    let terminal = draw(&mut app);

    assert!(terminal.backend().cursor_visible());
    assert_eq!(terminal.backend().cursor_position(), Position::new(5, 3));
    assert_eq!(
        app.take_cursor_style_change(),
        Some(SetCursorStyle::SteadyBar)
    );
    assert_eq!(
        app.take_cursor_style_change(),
        None,
        "written once, not on every frame"
    );
}

#[test]
fn should_follow_the_pane_s_cursor_shape() {
    let (mut app, pane) = app_in_editor();
    let mut written = Vec::new();

    for shape in [
        CursorShape::Block,
        CursorShape::Bar,
        CursorShape::Bar,
        CursorShape::Underline,
    ] {
        pane.borrow_mut().cursor = Some((0, 0, shape));
        draw(&mut app);
        written.extend(app.take_cursor_style_change());
    }

    assert_eq!(
        written,
        vec![
            SetCursorStyle::SteadyBlock,
            SetCursorStyle::SteadyBar,
            SetCursorStyle::SteadyUnderScore,
        ]
    );
}

#[test]
fn should_hide_the_cursor_when_the_pane_shows_none() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().cursor = Some((5, 3, CursorShape::Bar));
    draw(&mut app);
    app.take_cursor_style_change();

    pane.borrow_mut().cursor = None;
    let terminal = draw(&mut app);

    assert!(!terminal.backend().cursor_visible());
    assert_eq!(
        app.take_cursor_style_change(),
        None,
        "a cursor hidden for a frame keeps its shape for when it is back"
    );
}

#[test]
fn should_hide_a_cursor_reported_outside_the_pane() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().cursor = Some((80, 3, CursorShape::Block));

    let terminal = draw(&mut app);

    assert!(!terminal.backend().cursor_visible());
    assert_eq!(app.take_cursor_style_change(), None);
}

#[test]
fn should_give_the_terminal_its_cursor_shape_back_on_leaving() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().cursor = Some((5, 3, CursorShape::Bar));
    draw(&mut app);
    app.take_cursor_style_change();

    offer(&mut app, ctrl('g'));
    let terminal = draw(&mut app);

    assert!(!terminal.backend().cursor_visible());
    assert_eq!(
        app.take_cursor_style_change(),
        Some(SetCursorStyle::DefaultUserShape)
    );
    assert_eq!(app.restore_cursor_style(), None, "nothing left to undo");
}

#[test]
fn should_give_the_cursor_shape_back_when_tuicr_exits_from_the_editor() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().cursor = Some((5, 3, CursorShape::Underline));
    draw(&mut app);
    app.take_cursor_style_change();

    assert_eq!(
        app.restore_cursor_style(),
        Some(SetCursorStyle::DefaultUserShape)
    );
    assert_eq!(app.restore_cursor_style(), None);
}

#[test]
fn should_not_touch_the_cursor_shape_when_the_editor_never_showed_one() {
    let mut app = app_with(vec![file("a.rs", 3)]);
    draw(&mut app);

    assert_eq!(app.take_cursor_style_change(), None);
    assert_eq!(app.restore_cursor_style(), None);
}

#[test]
fn should_say_so_and_recover_when_editor_mode_has_no_pane() {
    let mut app = app_with(vec![file("a.rs", 3)]);
    app.input_mode = InputMode::Editor;

    let terminal = draw(&mut app);
    assert!(
        screen_text(terminal.backend().buffer()).contains("embedded editor is not running"),
        "never a blank screen"
    );
    assert!(!terminal.backend().cursor_visible());

    assert!(app.poll_editor_pane(), "leaving needs a repaint");
    assert_eq!(app.input_mode, InputMode::Normal);
}

// --- redraw, close and the pump -----------------------------------------

#[test]
fn should_repaint_for_a_key_only_when_the_pane_has_a_new_frame() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().then(PaneOutcome::default());
    pane.borrow_mut().then(PaneOutcome::redraw_only());

    let quiet = offer(&mut app, plain('j'));
    let changed = offer(&mut app, plain('j'));

    assert_eq!(quiet, Some(false));
    assert_eq!(changed, Some(true));
}

#[test]
fn should_leave_editor_mode_when_a_key_closes_the_last_view() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().then(PaneOutcome {
        close_requested: true,
        ..PaneOutcome::default()
    });

    let taken = offer(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(taken, Some(true), "the review has to be painted");
    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(pane.borrow().calls.last(), Some(&PaneCall::Focus(false)));
    assert_eq!(
        pane.borrow().created_at.len(),
        1,
        "closing hides the pane; it is not dropped"
    );
    assert!(app.editor_pane_state().is_some());
}

#[test]
fn should_leave_editor_mode_when_the_pump_reports_a_close() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().pump_outcomes.push_back(PaneOutcome {
        close_requested: true,
        ..PaneOutcome::default()
    });

    assert!(app.poll_editor_pane());

    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(pane.borrow().calls.last(), Some(&PaneCall::Focus(false)));
}

#[test]
fn should_repaint_for_a_pumped_frame_only_while_the_pane_is_visible() {
    let (mut app, pane) = app_in_editor();

    pane.borrow_mut()
        .pump_outcomes
        .push_back(PaneOutcome::redraw_only());
    assert!(app.poll_editor_pane(), "visible: the new frame is shown");
    assert!(!app.poll_editor_pane(), "nothing new: no repaint");

    offer(&mut app, ctrl('g'));
    pane.borrow_mut()
        .pump_outcomes
        .push_back(PaneOutcome::redraw_only());
    assert!(
        !app.poll_editor_pane(),
        "hidden: its frames must not repaint the review"
    );
    assert_eq!(
        pane.borrow().pumps(),
        3,
        "a hidden pane is still pumped, so language servers keep up"
    );
}

#[test]
fn should_ignore_a_close_from_a_hidden_pane() {
    let (mut app, pane) = app_in_editor();
    offer(&mut app, ctrl('g'));
    app.input_mode = InputMode::Help;
    pane.borrow_mut().pump_outcomes.push_back(PaneOutcome {
        close_requested: true,
        redraw: true,
        ..PaneOutcome::default()
    });

    assert!(!app.poll_editor_pane());

    assert_eq!(app.input_mode, InputMode::Help);
}

#[test]
fn should_not_build_or_pump_a_pane_nobody_opened() {
    let (mut app, pane) = app_with_pane();

    assert!(!app.poll_editor_pane());

    assert!(pane.borrow().created_at.is_empty());
    assert!(pane.borrow().calls.is_empty());
}

#[test]
fn should_tell_the_pane_when_another_view_takes_the_screen() {
    // A background result (a PR finishing loading, say) can switch modes
    // without going through Ctrl-g.
    let (mut app, pane) = app_in_editor();
    app.input_mode = InputMode::Normal;

    app.poll_editor_pane();

    assert_eq!(
        pane.borrow().calls,
        vec![
            PaneCall::Open {
                path: PathBuf::from("/repo/a.rs"),
                line: Some(7),
            },
            PaneCall::Focus(true),
            PaneCall::Focus(false),
            PaneCall::Pump,
        ]
    );
}

#[test]
fn should_report_each_return_to_the_review_once_with_the_last_target() {
    let (mut app, pane) = app_in_editor();
    assert_eq!(
        app.take_editor_pane_return(),
        None,
        "still in the editor: nothing to refresh yet"
    );

    // Ctrl-g.
    app.open_editor_pane(&target("/repo/b.rs", Some(3)), SCREEN)
        .expect("open");
    offer(&mut app, ctrl('g'));
    assert_eq!(
        app.take_editor_pane_return(),
        Some(target("/repo/b.rs", Some(3)))
    );
    assert_eq!(app.take_editor_pane_return(), None, "reported once");

    // The editor closing its last view.
    app.open_editor_pane(&target("/repo/a.rs", None), SCREEN)
        .expect("open");
    pane.borrow_mut().pump_outcomes.push_back(PaneOutcome {
        close_requested: true,
        ..PaneOutcome::default()
    });
    app.poll_editor_pane();
    assert_eq!(
        app.take_editor_pane_return(),
        Some(target("/repo/a.rs", None))
    );

    // Another view taking the screen without asking.
    app.open_editor_pane(&target("/repo/a.rs", None), SCREEN)
        .expect("open");
    app.input_mode = InputMode::Normal;
    app.poll_editor_pane();
    assert!(app.take_editor_pane_return().is_some());
    assert_eq!(app.take_editor_pane_return(), None);
}

// --- files the pane touches ---------------------------------------------

#[test]
fn should_collect_saved_and_opened_files_once_each() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().then(PaneOutcome {
        opened: vec![PathBuf::from("/elsewhere/lib.rs")],
        ..PaneOutcome::default()
    });
    pane.borrow_mut().then(PaneOutcome {
        saved: vec![PathBuf::from("/repo/a.rs")],
        ..PaneOutcome::default()
    });
    pane.borrow_mut().pump_outcomes.push_back(PaneOutcome {
        saved: vec![PathBuf::from("/repo/a.rs")],
        opened: vec![PathBuf::from("/elsewhere/lib.rs")],
        ..PaneOutcome::default()
    });

    offer(&mut app, plain('g'));
    offer(&mut app, plain('d'));
    app.poll_editor_pane();

    assert_eq!(
        app.take_editor_pane_files(),
        EditorPaneFiles {
            saved: vec![PathBuf::from("/repo/a.rs")],
            opened: vec![PathBuf::from("/elsewhere/lib.rs")],
        }
    );
    assert!(app.take_editor_pane_files().is_empty(), "taken once");
}

#[test]
fn should_keep_a_baseline_of_a_file_from_the_moment_the_pane_opens_it() {
    // The edited-line tint compares against the file before the first
    // edit, and the editor can write on the very next key.
    let repo = tempfile::tempdir().expect("tempdir");
    let path = repo.path().join("a.rs");
    std::fs::write(&path, "one\n").expect("write");
    let (mut app, pane) = app_with_pane();
    app.vcs_info.root_path = repo.path().to_path_buf();
    pane.borrow_mut().then(PaneOutcome {
        opened: vec![path.clone()],
        ..PaneOutcome::default()
    });

    app.open_editor_pane(
        &EditorTarget {
            path: path.clone(),
            line: None,
            label: "a.rs".to_string(),
        },
        SCREEN,
    )
    .expect("open");
    std::fs::write(&path, "one\ntwo\n").expect("write");
    app.refresh_local_edits();

    let edited = app
        .local_edits
        .get(Path::new("a.rs"))
        .expect("a baseline was taken when the pane opened the file");
    assert_eq!(edited.changed.iter().copied().collect::<Vec<_>>(), [2]);
}

// --- shutdown -----------------------------------------------------------

#[test]
fn should_shut_the_pane_down_once_and_report_what_failed() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut()
        .shutdown_errors
        .push("could not write a.rs".to_string());

    let errors = app.shutdown_editor_pane();

    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].to_string(), "could not write a.rs");
    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(
        pane.borrow().calls[2..],
        [PaneCall::Focus(false), PaneCall::Shutdown]
    );
    assert!(app.shutdown_editor_pane().is_empty());
    assert_eq!(
        app.open_editor_pane(&target("/repo/a.rs", None), SCREEN),
        Err(SHUT_DOWN.to_string()),
        "the editor cannot be built a second time"
    );
}

#[test]
fn should_shut_down_quietly_when_the_editor_was_never_opened() {
    let (mut app, pane) = app_with_pane();

    assert!(app.shutdown_editor_pane().is_empty());

    assert!(pane.borrow().created_at.is_empty());
}

#[test]
fn should_shut_the_pane_down_when_the_app_is_dropped_without_a_shutdown() {
    // An I/O error returns from the middle of the event loop: nothing
    // calls `shutdown_editor_pane`, and a pane dropped as it stands loses
    // the save it had in flight.
    let (app, pane) = app_in_editor();

    drop(app);

    assert_eq!(pane.borrow().calls.last(), Some(&PaneCall::Shutdown));
}

#[test]
fn should_not_shut_the_pane_down_a_second_time_when_dropped() {
    let (mut app, pane) = app_in_editor();
    app.shutdown_editor_pane();

    drop(app);

    let shutdowns = pane
        .borrow()
        .calls
        .iter()
        .filter(|call| matches!(call, PaneCall::Shutdown))
        .count();
    assert_eq!(shutdowns, 1);
}

// --- hand-off: the pane, or $EDITOR as before ---------------------------

/// What `$EDITOR` would run for `target` when it is the terminal editor
/// `editor`.
fn external(editor: &str, target: &EditorTarget) -> EditorCommand {
    EditorCommand::from_editor(editor, target)
}

/// The event loop's hand-off for `target` with a terminal `$EDITOR`.
fn hand_off(app: &mut App, target: &EditorTarget) -> EditorRoute {
    app.snapshot_local_edit_baseline(target);
    app.route_editor_target(target, &external("vim", target), SCREEN)
}

fn fallback_reason(route: EditorRoute) -> String {
    match route {
        EditorRoute::External {
            reason: Some(reason),
        } => reason,
        other => panic!("expected $EDITOR with a reason, got {other:?}"),
    }
}

#[test]
fn should_open_the_target_in_the_pane_at_its_line_and_enter_editor_mode() {
    let (mut app, pane) = app_with_pane();
    let target = target("/repo/src/lib.rs", Some(42));

    let route = hand_off(&mut app, &target);

    assert_eq!(route, EditorRoute::Embedded);
    assert_eq!(app.input_mode, InputMode::Editor);
    assert_eq!(pane.borrow().created_at, vec![SCREEN]);
    assert_eq!(
        pane.borrow().calls,
        vec![
            PaneCall::Open {
                path: PathBuf::from("/repo/src/lib.rs"),
                line: Some(42),
            },
            PaneCall::Focus(true),
        ]
    );
}

#[test]
fn should_open_a_target_without_a_line_at_the_editor_s_own_position() {
    let (mut app, pane) = app_with_pane();

    let route = hand_off(&mut app, &target("/repo/a.rs", None));

    assert_eq!(route, EditorRoute::Embedded);
    assert_eq!(
        pane.borrow().calls[0],
        PaneCall::Open {
            path: PathBuf::from("/repo/a.rs"),
            line: None,
        }
    );
}

#[test]
fn should_use_editor_without_comment_when_no_embedded_editor_was_set_up() {
    // A build without the embedded editor installs no factory: `e` is what
    // it always was, and there is nothing to explain on every edit.
    let mut app = app_with(vec![file("a.rs", 3)]);

    let route = hand_off(&mut app, &target("/repo/a.rs", Some(3)));

    assert_eq!(route, EditorRoute::External { reason: None });
    assert_eq!(app.input_mode, InputMode::Normal);
}

#[test]
fn should_use_editor_without_comment_when_the_option_is_off() {
    // Whoever turned the option off knows: repeating it after every edit
    // would be noise.
    let (mut app, pane) = app_with_pane();
    app.init_embedded_editor(false);

    let route = hand_off(&mut app, &target("/repo/a.rs", None));

    assert_eq!(route, EditorRoute::External { reason: None });
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(
        pane.borrow().created_at.is_empty(),
        "an editor nobody wants is not started"
    );
}

#[test]
fn should_leave_the_status_line_as_it_was_when_the_option_is_off() {
    // What the event loop does around `$EDITOR`, with the option off and
    // with no embedded editor at all: the same line either way.
    let status_after_editor = |review: &mut Review| {
        let target = review.target("f.rs");
        let route = hand_off(&mut review.app, &target);
        review.save("f.rs", "one\ntwo\n");
        review.app.after_editor_returned(&target);
        if let EditorRoute::External {
            reason: Some(reason),
        } = route
        {
            review.app.note_editor_fallback(&reason);
        }
        review.message().to_string()
    };
    let mut turned_off = Review::new();
    turned_off.app.init_embedded_editor(false);
    let mut never_had_one = Review::new();
    never_had_one.app.editor_pane = EditorPaneHost::default();

    let status = status_after_editor(&mut turned_off);

    assert_eq!(
        status,
        "Opened f.rs and reloaded 2 files \u{00b7} 1 line edited"
    );
    assert_eq!(status, status_after_editor(&mut never_had_one));
}

#[test]
fn should_use_editor_and_say_so_under_stdout() {
    let (mut app, pane) = app_with_pane();
    app.output_to_stdout = true;

    let reason = fallback_reason(hand_off(&mut app, &target("/repo/a.rs", None)));

    assert!(reason.contains("--stdout"), "{reason}");
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(pane.borrow().created_at.is_empty());
}

#[test]
fn should_leave_a_windowed_editor_its_file() {
    // tuicr stays on screen beside a windowed editor; taking the screen for
    // the pane would be a step back for someone who set one up.
    let (mut app, pane) = app_with_pane();
    let target = target("/repo/a.rs", Some(9));

    let route = app.route_editor_target(&target, &external("code", &target), SCREEN);

    assert_eq!(
        route,
        EditorRoute::External { reason: None },
        "their choice of $EDITOR, so nothing to explain after every edit"
    );
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(pane.borrow().created_at.is_empty());
}

#[test]
fn should_take_the_file_from_a_windowed_editor_asked_to_block() {
    // `code --wait` is run like a terminal editor: tuicr suspends until it
    // exits. That is the case the pane replaces.
    let (mut app, _pane) = app_with_pane();
    let target = target("/repo/a.rs", None);

    let route = app.route_editor_target(&target, &external("code --wait", &target), SCREEN);

    assert_eq!(route, EditorRoute::Embedded);
}

#[test]
fn should_keep_a_read_only_pr_copy_in_editor() {
    // The pane cannot open a file read-only, and a snapshot of the
    // reviewed revision is not a file to edit.
    let (mut app, pane) = app_with_pane();
    let snapshot = EditorTarget {
        path: std::env::temp_dir()
            .join("tuicr")
            .join("snapshots")
            .join("acme-app-pr7-1a2b3c4")
            .join("src")
            .join("main.rs"),
        line: Some(12),
        label: "src/main.rs @ 1a2b3c4 (read-only PR copy)".to_string(),
    };

    let reason = fallback_reason(hand_off(&mut app, &snapshot));

    assert!(reason.contains("read-only PR"), "{reason}");
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(pane.borrow().created_at.is_empty());
    assert!(
        pane.borrow().calls.is_empty(),
        "the snapshot path is handed to $EDITOR as it is, never to the pane"
    );
}

#[test]
fn should_use_editor_and_say_why_when_the_pane_cannot_start() {
    let mut app = app_with(vec![file("a.rs", 3)]);
    app.install_editor_pane_factory(FakePane::failing_factory("helix runtime not found"));

    let first = fallback_reason(hand_off(&mut app, &target("/repo/a.rs", None)));
    let second = fallback_reason(hand_off(&mut app, &target("/repo/b.rs", None)));

    assert_eq!(
        first,
        "embedded editor not used: helix runtime not found; restart tuicr to try again"
    );
    assert_eq!(second, first, "the reason is the same on every later edit");
    assert_eq!(app.input_mode, InputMode::Normal);
}

#[test]
fn should_try_the_embedded_editor_again_after_a_start_failure_that_can_be_fixed() {
    // A typo in the editor's own config: the first `e` falls back to
    // `$EDITOR` and says why. Once the file is fixed the next `e` gets
    // the embedded editor, in the same tuicr.
    let (mut app, pane) = app_with_pane();
    pane.borrow_mut()
        .start_errors
        .push_back(PaneStartError::retryable(anyhow::anyhow!(
            "bad config /home/u/.config/helix/config.toml: expected `=`"
        )));

    let first = fallback_reason(hand_off(&mut app, &target("/repo/a.rs", None)));

    assert_eq!(
        first,
        "embedded editor not used: bad config /home/u/.config/helix/config.toml: expected `=`"
    );
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(app.editor_pane_available());

    assert_eq!(
        hand_off(&mut app, &target("/repo/a.rs", None)),
        EditorRoute::Embedded
    );
    assert_eq!(pane.borrow().created_at.len(), 1);
}

#[test]
fn should_use_editor_for_a_file_the_pane_cannot_open_and_keep_the_pane() {
    let (mut app, pane) = app_with_pane();
    hand_off(&mut app, &target("/repo/a.rs", None));
    offer(&mut app, ctrl('g'));
    pane.borrow_mut().open_error = Some("permission denied".to_string());

    let reason = fallback_reason(hand_off(&mut app, &target("/repo/secret.rs", None)));

    assert_eq!(reason, "embedded editor not used: permission denied");
    assert_eq!(app.input_mode, InputMode::Normal);

    // One file failing does not cost the editor.
    assert_eq!(
        hand_off(&mut app, &target("/repo/b.rs", None)),
        EditorRoute::Embedded
    );
    assert_eq!(pane.borrow().created_at.len(), 1);
}

#[test]
fn should_add_the_fallback_reason_to_what_the_status_line_says() {
    let mut app = app_with(vec![file("a.rs", 3)]);

    app.set_message("Opened a.rs and reloaded 1 files");
    app.note_editor_fallback("embedded editor is not used with --stdout");
    let message = app.message.clone().expect("message");
    assert_eq!(
        message.content,
        "Opened a.rs and reloaded 1 files \u{00b7} embedded editor is not used with --stdout"
    );
    assert_eq!(message.message_type, MessageType::Info);

    // A failed hand-off is the news; the reason would bury it.
    app.set_error("Failed to launch editor: not found");
    app.note_editor_fallback("embedded editor is not used with --stdout");
    assert_eq!(
        app.message.as_ref().map(|m| m.content.as_str()),
        Some("Failed to launch editor: not found")
    );

    app.message = None;
    app.note_editor_fallback("embedded editor is not used with --stdout");
    assert_eq!(
        app.message.as_ref().map(|m| m.content.as_str()),
        Some("embedded editor is not used with --stdout")
    );
}

// --- return: the review catches up with the disk ------------------------

/// Serves `files` for every kind of diff and counts how often it is asked.
struct CountingVcs {
    info: VcsInfo,
    files: Vec<DiffFile>,
    loads: Arc<AtomicUsize>,
}

impl CountingVcs {
    fn load(&self) -> crate::error::Result<Vec<DiffFile>> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Ok(self.files.clone())
    }
}

impl VcsBackend for CountingVcs {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(
        &self,
        _hl: &crate::syntax::SyntaxHighlighter,
    ) -> crate::error::Result<Vec<DiffFile>> {
        self.load()
    }

    fn get_commit_range_diff(
        &self,
        _range: &ResolvedRevisionRange<'_>,
        _hl: &crate::syntax::SyntaxHighlighter,
    ) -> crate::error::Result<Vec<DiffFile>> {
        self.load()
    }

    fn fetch_context_lines(
        &self,
        _path: &Path,
        _status: FileStatus,
        _ref_commit: Option<&str>,
        _start: u32,
        _end: u32,
    ) -> crate::error::Result<Vec<DiffLine>> {
        Ok(Vec::new())
    }

    fn file_line_count(
        &self,
        _path: &Path,
        _status: FileStatus,
        _ref_commit: Option<&str>,
    ) -> crate::error::Result<u32> {
        Ok(0)
    }
}

/// A working-tree review of `f.rs` and `g.rs` in a real directory, with a
/// fake pane ready to be built. `loads` counts diff reloads.
struct Review {
    app: App,
    pane: FakeHandle,
    loads: Arc<AtomicUsize>,
    dir: tempfile::TempDir,
}

impl Review {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["f.rs", "g.rs"] {
            std::fs::write(dir.path().join(name), "one\n").expect("write");
        }
        let vcs_info = VcsInfo {
            root_path: dir.path().to_path_buf(),
            head_commit: "head".into(),
            branch_name: Some("main".into()),
            vcs_type: VcsType::Git,
        };
        let session = ReviewSession::new(
            vcs_info.root_path.clone(),
            vcs_info.head_commit.clone(),
            vcs_info.branch_name.clone(),
            SessionDiffSource::WorkingTree,
        );
        let files = vec![file("f.rs", 3), file("g.rs", 3)];
        let loads = Arc::new(AtomicUsize::new(0));
        let mut app = App::build(
            Box::new(CountingVcs {
                info: vcs_info.clone(),
                files: files.clone(),
                loads: Arc::clone(&loads),
            }),
            vcs_info,
            crate::theme::Theme::dark(),
            None,
            false,
            files,
            session,
            DiffSource::WorkingTree,
            InputMode::Normal,
            Vec::new(),
            None,
            None,
        )
        .expect("build app");
        let (factory, pane) = FakePane::factory();
        assert!(app.install_editor_pane_factory(factory));
        Self {
            app,
            pane,
            loads,
            dir,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn target(&self, name: &str) -> EditorTarget {
        EditorTarget {
            path: self.path(name),
            line: None,
            label: name.to_string(),
        }
    }

    /// `e` on `name`, as the event loop hands it over.
    fn open(&mut self, name: &str) {
        let target = self.target(name);
        assert_eq!(hand_off(&mut self.app, &target), EditorRoute::Embedded);
    }

    /// What the editor writes to disk.
    fn save(&self, name: &str, content: &str) {
        std::fs::write(self.path(name), content).expect("write");
    }

    /// What the event loop does with an event while the pane has focus:
    /// hand it over, then refresh the review if that left Editor mode.
    fn press(&mut self, event: Event) -> bool {
        offer(&mut self.app, event).expect("the pane has focus");
        self.app.finish_editor_pane_return()
    }

    /// One pass of the event loop's polls, the same way.
    fn tick(&mut self) -> bool {
        self.app.poll_editor_pane();
        self.app.finish_editor_pane_return()
    }

    fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }

    fn message(&self) -> &str {
        self.app
            .message
            .as_ref()
            .map(|message| message.content.as_str())
            .unwrap_or("")
    }

    fn edited_lines(&self, name: &str) -> Option<Vec<u32>> {
        self.app
            .local_edits
            .get(Path::new(name))
            .map(|file| file.changed.iter().copied().collect())
    }
}

#[test]
fn should_reload_the_review_from_disk_when_ctrl_g_returns_to_it() {
    let mut review = Review::new();
    review.open("f.rs");
    review.save("f.rs", "one\ntwo\n");
    assert_eq!(review.loads(), 0, "nothing reloads while the editor is up");

    let refreshed = review.press(ctrl('g'));

    assert!(refreshed);
    assert_eq!(review.app.input_mode, InputMode::Normal);
    assert_eq!(review.loads(), 1);
    assert_eq!(review.edited_lines("f.rs"), Some(vec![2]));
    assert_eq!(
        review.message(),
        "Opened f.rs and reloaded 2 files \u{00b7} 1 line edited"
    );
}

#[test]
fn should_reload_when_the_editor_closes_its_last_view() {
    // `:wq` writes and closes without reporting a save: only the disk
    // knows, so the return itself is the trigger.
    let mut review = Review::new();
    review.open("f.rs");
    review.save("f.rs", "ONE\n");
    review.pane.borrow_mut().then(PaneOutcome {
        close_requested: true,
        ..PaneOutcome::default()
    });

    assert!(review.press(key(KeyCode::Enter, KeyModifiers::NONE)));

    assert_eq!(review.loads(), 1);
    assert_eq!(review.edited_lines("f.rs"), Some(vec![1]));
}

#[test]
fn should_reload_when_background_work_closes_the_editor() {
    let mut review = Review::new();
    review.open("f.rs");
    review
        .pane
        .borrow_mut()
        .pump_outcomes
        .push_back(PaneOutcome {
            close_requested: true,
            ..PaneOutcome::default()
        });

    assert!(review.tick());

    assert_eq!(review.app.input_mode, InputMode::Normal);
    assert_eq!(review.loads(), 1);
}

#[test]
fn should_reload_on_every_return_and_only_on_a_return() {
    let mut review = Review::new();
    review.open("f.rs");

    assert!(!review.press(plain('j')), "still in the editor");
    assert!(!review.tick());
    assert_eq!(review.loads(), 0);

    assert!(review.press(ctrl('g')));
    assert!(!review.tick(), "one return, one refresh");
    assert_eq!(review.loads(), 1);

    review.open("g.rs");
    assert!(review.press(ctrl('g')));
    assert_eq!(
        review.loads(),
        2,
        "a visit that reported no save still reloads"
    );
    assert_eq!(review.message(), "Opened g.rs and reloaded 2 files");
}

#[test]
fn should_refresh_the_same_way_after_editor_and_after_the_pane() {
    // One function serves both: what `$EDITOR` exiting does...
    let mut after_editor = Review::new();
    let target = after_editor.target("f.rs");
    after_editor.app.snapshot_local_edit_baseline(&target);
    after_editor.save("f.rs", "one\ntwo\nthree\n");
    after_editor.app.after_editor_returned(&target);

    // ...is what leaving the pane does.
    let mut after_pane = Review::new();
    after_pane.open("f.rs");
    after_pane.save("f.rs", "one\ntwo\nthree\n");
    after_pane.press(ctrl('g'));

    assert_eq!(
        after_editor.message(),
        "Opened f.rs and reloaded 2 files \u{00b7} 2 lines edited"
    );
    assert_eq!(after_pane.message(), after_editor.message());
    assert_eq!(after_pane.loads(), after_editor.loads());
    assert_eq!(
        after_pane.edited_lines("f.rs"),
        after_editor.edited_lines("f.rs")
    );
}

#[test]
fn should_only_recompute_edited_lines_when_the_review_is_a_commit_range() {
    let mut review = Review::new();
    review.app.diff_source = DiffSource::CommitRange(vec!["aaaaaaaaaa".into()]);
    review.open("f.rs");
    review.save("f.rs", "one\ntwo\n");

    assert!(review.press(ctrl('g')));

    assert_eq!(
        review.loads(),
        0,
        "the commits did not change; nothing reloads"
    );
    assert_eq!(
        review.message(),
        "Opened f.rs \u{00b7} 1 line edited; :local compares the review with your working tree"
    );
}

const CONFLICTED: &str = "top\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> x\n";

impl Review {
    /// Put the merge view up over the review, on `f.rs` with one conflict.
    fn enter_merge_view(&mut self) {
        self.save("f.rs", CONFLICTED);
        let parsed = crate::vcs::parse_conflict_file(CONFLICTED);
        let resolutions = vec![ConflictResolution::Unresolved; parsed.conflict_count()];
        self.app.merge_view = Some(MergeView {
            files: vec![MergeFileState {
                path: PathBuf::from("f.rs"),
                file: parsed,
                resolutions,
                dirty: false,
                staged: false,
                load_error: None,
            }],
            current_file: 0,
            current_conflict: 0,
            scroll_offset: 0,
            viewport_height: 20,
            return_mode: InputMode::Normal,
            discard_armed: false,
            ours_label: "main".to_string(),
            theirs_label: "feature".to_string(),
            ours_commit: None,
            theirs_commit: None,
        });
        self.app.input_mode = InputMode::MergeConflict;
    }

    fn merge_file(&self) -> &MergeFileState {
        let view = self.app.merge_view.as_ref().expect("merge view");
        view.current_file_state().expect("file")
    }

    /// The editor finishes writing `content` to `name` on its next pump,
    /// as a `:w` does some pumps after it was typed.
    fn save_lands(&mut self, name: &str, content: &str) {
        let path = self.path(name);
        let mut script = self.pane.borrow_mut();
        script
            .disk_on_save
            .insert(path.clone(), content.to_string());
        script.pump_outcomes.push_back(PaneOutcome {
            saved: vec![path],
            ..PaneOutcome::default()
        });
    }
}

#[test]
fn should_reparse_the_merge_view_s_file_and_not_reload_the_diff() {
    // `e` in the merge view opens the conflicted file; what comes back is
    // re-read by the merge view, as after `$EDITOR`.
    let mut review = Review::new();
    review.enter_merge_view();
    review.open("f.rs");
    review.save("f.rs", "top\nresolved by hand\n");

    assert!(review.press(ctrl('g')));

    assert_eq!(review.app.input_mode, InputMode::MergeConflict);
    assert_eq!(review.loads(), 0, "the merge view tracks its own file");
    assert_eq!(review.merge_file().file.conflict_count(), 0);
    assert_eq!(
        review.message(),
        "Reloaded f.rs \u{00b7} no conflicts remain \u{00b7} press w to stage"
    );
    assert_eq!(
        review.edited_lines("f.rs"),
        None,
        "no edit tint for a file the merge view owns"
    );
}

#[test]
fn should_hand_the_merge_view_s_file_over_again_after_writing_a_new_pick() {
    // `e`, Ctrl-g, take ours, `e` again. tuicr writes the picks to disk
    // just before each hand-off, and by the second one the pane has held
    // the file since the first. The pane is asked to open it again, after
    // the write, so that it can read it again (`EditorPane::open`): a pane
    // left showing its old buffer would let `:w!` put the conflict back.
    let mut review = Review::new();
    review.enter_merge_view();
    review.app.merge_edit_current_file();
    let first = review.app.take_pending_editor_target().expect("queued");
    assert_eq!(hand_off(&mut review.app, &first), EditorRoute::Embedded);
    review.press(ctrl('g'));
    assert_eq!(review.app.input_mode, InputMode::MergeConflict);

    {
        let view = review.app.merge_view.as_mut().expect("merge view");
        view.files[0].resolutions[0] = ConflictResolution::Ours;
        view.files[0].dirty = true;
    }
    review.app.merge_edit_current_file();
    let second = review
        .app
        .take_pending_editor_target()
        .expect("queued again");
    assert_eq!(hand_off(&mut review.app, &second), EditorRoute::Embedded);

    let script = review.pane.borrow();
    let opens: Vec<&PaneCall> = script
        .calls
        .iter()
        .filter(|call| matches!(call, PaneCall::Open { .. }))
        .collect();
    assert_eq!(opens.len(), 2, "asked to open again, not merely shown");
    assert_eq!(opens[0], opens[1], "the same file, at the same conflict");
    assert_eq!(
        script.disk_at_open,
        vec![
            Some(CONFLICTED.to_string()),
            Some("top\nours\n".to_string())
        ],
        "each open comes after the write it has to show"
    );
}

#[test]
fn should_give_the_merge_view_s_file_to_editor_when_the_pane_will_not_reopen_it() {
    // The other answer a pane has for a file it already holds: it has
    // unsaved changes to it and tuicr rewrote the file under them, so
    // there is no buffer it can show that is both. It says so through
    // `open`'s error. `$EDITOR` then gets the file as it is on disk, the
    // reason is what the status line adds, the merge view keeps the
    // screen, and the pane keeps its buffer and its hold on quitting.
    let mut review = Review::new();
    review.enter_merge_view();
    review.app.merge_edit_current_file();
    let first = review.app.take_pending_editor_target().expect("queued");
    assert_eq!(hand_off(&mut review.app, &first), EditorRoute::Embedded);
    review.pane.borrow_mut().state.modified = vec![review.path("f.rs")];
    review.press(ctrl('g'));

    {
        let view = review.app.merge_view.as_mut().expect("merge view");
        view.files[0].resolutions[0] = ConflictResolution::Ours;
        view.files[0].dirty = true;
    }
    review.app.merge_edit_current_file();
    let second = review
        .app
        .take_pending_editor_target()
        .expect("queued again");
    review.pane.borrow_mut().open_error =
        Some("f.rs changed on disk and has unsaved changes in the editor".to_string());

    let reason = fallback_reason(hand_off(&mut review.app, &second));

    assert_eq!(
        reason,
        "embedded editor not used: f.rs changed on disk and has unsaved changes in the editor"
    );
    assert_eq!(review.app.input_mode, InputMode::MergeConflict);
    assert_eq!(
        std::fs::read_to_string(review.path("f.rs")).expect("read"),
        "top\nours\n",
        "the pick is on disk for $EDITOR"
    );
    assert!(review.app.editor_pane_available(), "the pane is kept");
    assert_eq!(
        review.pane.borrow().disk_at_open,
        vec![Some(CONFLICTED.to_string())],
        "the refused open showed nothing"
    );
    review.app.should_quit = true;
    assert!(
        review.app.quit_blocked_by_editor(),
        "its unsaved buffer still holds a quit back"
    );
}

#[test]
fn should_tint_edits_to_a_file_reached_by_navigation() {
    // Go-to-definition lands in g.rs, which tuicr never handed over: its
    // baseline is taken when the pane says it opened the file, before the
    // key after that can change it.
    let mut review = Review::new();
    review.open("f.rs");
    let elsewhere = review.path("g.rs");
    review.pane.borrow_mut().then(PaneOutcome {
        opened: vec![elsewhere],
        ..PaneOutcome::default()
    });

    review.press(plain('g'));
    assert_eq!(
        review.edited_lines("g.rs"),
        Some(Vec::new()),
        "a baseline exists before the next key is forwarded"
    );
    review.press(plain('d'));
    review.save("g.rs", "one\nadded\n");
    review.press(ctrl('g'));

    assert_eq!(review.edited_lines("g.rs"), Some(vec![2]));
    assert_eq!(
        review.edited_lines("f.rs"),
        None,
        "the file tuicr opened was left as it was, so it is not tracked"
    );
}

#[test]
fn should_tint_edits_to_a_file_opened_by_background_work() {
    let mut review = Review::new();
    review.open("f.rs");
    let elsewhere = review.path("g.rs");
    review
        .pane
        .borrow_mut()
        .pump_outcomes
        .push_back(PaneOutcome {
            opened: vec![elsewhere],
            ..PaneOutcome::default()
        });

    review.tick();
    review.save("g.rs", "changed\n");
    review.press(ctrl('g'));

    assert_eq!(review.edited_lines("g.rs"), Some(vec![1]));
}

#[test]
fn should_keep_waiting_for_the_save_of_a_buffer_left_unsaved() {
    // Ctrl-g hides the editor with f.rs edited but not written. The file
    // on disk still matches its baseline; forgetting the baseline now
    // would leave the eventual save untinted.
    let mut review = Review::new();
    review.open("f.rs");
    review.pane.borrow_mut().state.modified = vec![review.path("f.rs")];

    review.press(ctrl('g'));
    assert_eq!(
        review.edited_lines("f.rs"),
        Some(Vec::new()),
        "still tracked, nothing edited on disk yet"
    );

    // Back in the editor through another file, `:wa` writes f.rs.
    review.open("g.rs");
    review.save("f.rs", "one\nlate\n");
    review.pane.borrow_mut().state.modified.clear();
    review.press(ctrl('g'));

    assert_eq!(review.edited_lines("f.rs"), Some(vec![2]));
    assert_eq!(review.edited_lines("g.rs"), None);
}

// --- later visits to files the pane still holds -------------------------

impl Review {
    /// A first visit that follows a definition from f.rs into g.rs, edits
    /// nothing, and goes back to the review. The pane keeps both loaded.
    fn visit_g_and_leave_it_as_it_was(&mut self) {
        self.open("f.rs");
        let elsewhere = self.path("g.rs");
        self.pane.borrow_mut().then(PaneOutcome {
            opened: vec![elsewhere],
            ..PaneOutcome::default()
        });
        self.press(plain('g'));
        self.press(ctrl('g'));
        assert_eq!(
            self.edited_lines("g.rs"),
            None,
            "left as it was, so not tracked between visits"
        );
    }
}

#[test]
fn should_tint_edits_made_on_a_later_visit_to_a_file_the_pane_still_holds() {
    // The pane reports a file as opened once, when it loads it. Switching
    // back to the g.rs buffer on a second visit says nothing.
    let mut review = Review::new();
    review.visit_g_and_leave_it_as_it_was();

    review.open("f.rs");
    assert_eq!(
        review.edited_lines("g.rs"),
        Some(Vec::new()),
        "a baseline again before the first key of the visit"
    );
    review.save("g.rs", "one\nadded\n");
    review.press(ctrl('g'));

    assert_eq!(review.edited_lines("g.rs"), Some(vec![2]));
    assert_eq!(
        review.edited_lines("f.rs"),
        None,
        "and a file left alone on this visit is forgotten again"
    );
}

#[test]
fn should_measure_a_later_visit_from_the_file_as_that_visit_found_it() {
    let mut review = Review::new();
    review.visit_g_and_leave_it_as_it_was();
    // Another tool, between the visits.
    review.save("g.rs", "zero\none\n");

    review.open("f.rs");
    review.save("g.rs", "zero\none\ntwo\n");
    review.press(ctrl('g'));

    assert_eq!(
        review.edited_lines("g.rs"),
        Some(vec![3]),
        "only what the editor added, not what changed before it was opened"
    );
}

#[test]
fn should_keep_the_first_baseline_of_a_file_edited_on_an_earlier_visit() {
    let mut review = Review::new();
    review.open("f.rs");
    review.save("f.rs", "one\ntwo\n");
    review.press(ctrl('g'));

    review.open("g.rs");
    review.save("f.rs", "one\ntwo\nthree\n");
    review.press(ctrl('g'));

    assert_eq!(
        review.edited_lines("f.rs"),
        Some(vec![2, 3]),
        "still measured from before the first edit"
    );
}

#[test]
fn should_take_no_baselines_for_held_files_from_the_merge_view() {
    let mut review = Review::new();
    review.visit_g_and_leave_it_as_it_was();
    review.enter_merge_view();

    review.open("f.rs");

    assert_eq!(review.edited_lines("g.rs"), None);
    assert_eq!(review.edited_lines("f.rs"), None);
}

// --- saves that land after the editor was left --------------------------

#[test]
fn should_reload_again_when_a_save_lands_after_the_editor_was_left() {
    // `:w` then `Ctrl-g` without a pause: the editor writes in the
    // background and reports the save a dozen pumps later, after the
    // review has already reloaded from a disk that had not changed yet.
    let mut review = Review::new();
    review.open("f.rs");
    review.pane.borrow_mut().state.modified = vec![review.path("f.rs")];

    assert!(review.press(ctrl('g')));
    assert_eq!(review.loads(), 1);
    assert_eq!(review.message(), "Opened f.rs and reloaded 2 files");
    assert!(!review.tick(), "nothing has been written yet");

    review.save_lands("f.rs", "one\ntwo\n");
    let refreshed = review.tick();

    assert!(refreshed, "the save is a reason to catch up again");
    assert_eq!(review.loads(), 2);
    assert_eq!(review.edited_lines("f.rs"), Some(vec![2]));
    assert_eq!(
        review.message(),
        "Saved f.rs and reloaded 2 files \u{00b7} 1 line edited"
    );
    assert!(!review.tick(), "one save, one more refresh");
    assert_eq!(review.loads(), 2);
}

#[test]
fn should_name_the_file_that_was_saved_when_it_is_not_the_one_opened() {
    // `:wa` behind a visit that was opened on f.rs writes g.rs.
    let mut review = Review::new();
    review.open("f.rs");
    let elsewhere = review.path("g.rs");
    review.pane.borrow_mut().then(PaneOutcome {
        opened: vec![elsewhere.clone()],
        ..PaneOutcome::default()
    });
    review.press(plain('g'));
    review.pane.borrow_mut().state.modified = vec![elsewhere];
    review.press(ctrl('g'));

    review.save_lands("g.rs", "one\nlate\n");

    assert!(review.tick());
    assert_eq!(review.edited_lines("g.rs"), Some(vec![2]));
    assert_eq!(
        review.message(),
        "Saved g.rs and reloaded 2 files \u{00b7} 1 line edited"
    );
}

#[test]
fn should_not_reload_for_a_save_reported_while_the_editor_is_up() {
    // Leaving the editor reloads anyway; a reload behind it would only
    // flicker.
    let mut review = Review::new();
    review.open("f.rs");
    review.save_lands("f.rs", "one\ntwo\n");

    assert!(!review.tick());
    assert_eq!(review.loads(), 0);

    assert!(review.press(ctrl('g')));
    assert!(!review.tick());
    assert_eq!(review.loads(), 1, "the return, and nothing after it");
}

#[test]
fn should_wait_for_a_write_in_flight_before_reloading_on_return() {
    // A pane that counts its unfinished saves lets the review wait for
    // them: one reload, and it already shows the edit.
    let mut review = Review::new();
    review.open("f.rs");
    {
        let mut script = review.pane.borrow_mut();
        script.state.modified = vec![review.path("f.rs")];
        script.state.pending_writes = 1;
        script.pump_outcomes.extend([
            PaneOutcome::default(),
            PaneOutcome::default(),
            PaneOutcome::default(),
        ]);
    }
    review.save_lands("f.rs", "one\ntwo\n");

    assert!(review.press(ctrl('g')));

    assert_eq!(review.pane.borrow().pumps(), 4, "pumped until it landed");
    assert_eq!(review.loads(), 1);
    assert_eq!(
        review.message(),
        "Opened f.rs and reloaded 2 files \u{00b7} 1 line edited"
    );
    assert!(!review.tick(), "nothing is left to catch up with");
    assert_eq!(review.loads(), 1);
}

#[test]
fn should_stop_waiting_for_a_write_that_does_not_finish_and_catch_it_later() {
    // Format-on-save can sit behind a language server for longer than
    // anyone should stare at a frozen screen.
    let mut review = Review::new();
    review.app.editor_pane.write_wait = Duration::from_millis(20);
    review.open("f.rs");
    {
        let mut script = review.pane.borrow_mut();
        script.state.modified = vec![review.path("f.rs")];
        script.state.pending_writes = 1;
    }

    let started = Instant::now();
    assert!(review.press(ctrl('g')));

    assert!(started.elapsed() < Duration::from_secs(5), "bounded");
    assert!(review.pane.borrow().pumps() > 0, "it did wait");
    assert_eq!(review.message(), "Opened f.rs and reloaded 2 files");

    review.save_lands("f.rs", "one\ntwo\n");
    assert!(review.tick());
    assert_eq!(review.edited_lines("f.rs"), Some(vec![2]));
}

#[test]
fn should_hold_a_late_save_until_the_user_is_back_in_the_review() {
    // Reloading under a half-typed command or comment would be the diff
    // watcher's mistake all over again: it waits for Normal mode too.
    let mut review = Review::new();
    review.open("f.rs");
    review.pane.borrow_mut().state.modified = vec![review.path("f.rs")];
    review.press(ctrl('g'));
    review.app.enter_command_mode();
    review.save_lands("f.rs", "one\ntwo\n");

    assert!(!review.tick());
    assert!(!review.tick());
    assert_eq!(review.loads(), 1, "not while the prompt is open");

    review.app.exit_command_mode();
    assert!(review.tick());
    assert_eq!(review.loads(), 2);
    assert_eq!(review.edited_lines("f.rs"), Some(vec![2]));
}

#[test]
fn should_drop_a_late_save_once_the_editor_is_up_again() {
    let mut review = Review::new();
    review.open("f.rs");
    review.pane.borrow_mut().state.modified = vec![review.path("f.rs")];
    review.press(ctrl('g'));
    review.app.enter_command_mode();
    review.save_lands("f.rs", "one\ntwo\n");
    review.tick();
    review.app.exit_command_mode();

    // Back into the editor before the review caught up: the next return
    // reloads, and that is the only reload still owed.
    review.open("g.rs");
    assert!(!review.tick());
    assert_eq!(review.loads(), 1);
    assert!(review.press(ctrl('g')));
    assert_eq!(review.loads(), 2);
    assert!(!review.tick());
    assert_eq!(review.loads(), 2);
    assert_eq!(review.edited_lines("f.rs"), Some(vec![2]));
}

#[test]
fn should_reparse_the_merge_view_s_file_when_its_save_lands_late() {
    let mut review = Review::new();
    review.enter_merge_view();
    review.open("f.rs");
    review.pane.borrow_mut().state.modified = vec![review.path("f.rs")];
    review.press(ctrl('g'));
    assert_eq!(review.merge_file().file.conflict_count(), 1);

    review.save_lands("f.rs", "top\nresolved by hand\n");

    assert!(review.tick());
    assert_eq!(review.merge_file().file.conflict_count(), 0);
    assert_eq!(review.loads(), 0, "the merge view tracks its own file");
    assert_eq!(
        review.message(),
        "Reloaded f.rs \u{00b7} no conflicts remain \u{00b7} press w to stage"
    );
}

#[test]
fn should_keep_the_merge_view_s_picks_when_another_file_s_save_lands_late() {
    // Re-parsing the file on screen resets its picks; a save of some
    // other buffer is no reason to.
    let mut review = Review::new();
    review.enter_merge_view();
    review.open("f.rs");
    review.pane.borrow_mut().state.modified = vec![review.path("g.rs")];
    review.press(ctrl('g'));
    {
        let view = review.app.merge_view.as_mut().expect("merge view");
        view.files[0].resolutions[0] = ConflictResolution::Ours;
        view.files[0].dirty = true;
    }

    review.save_lands("g.rs", "one\nlate\n");

    assert!(!review.tick());
    assert_eq!(
        review.merge_file().resolutions,
        vec![ConflictResolution::Ours]
    );
    assert!(review.merge_file().dirty);
}

// --- the help popup -------------------------------------------------------

/// Every line of the `?` help, as it is searched.
fn help_lines(app: &mut App) -> Vec<String> {
    app.input_mode = InputMode::Help;
    draw(app);
    app.help_state.searchable_lines.clone()
}

#[test]
fn should_list_the_editor_s_keys_in_help_where_there_is_an_embedded_editor() {
    let (mut app, _pane) = app_with_pane();

    let lines = help_lines(&mut app);

    let row = |keys: &str| {
        lines
            .iter()
            .find(|line| line.trim_start().starts_with(keys))
            .unwrap_or_else(|| panic!("no help row for {keys}"))
    };
    assert!(row("Ctrl-g").contains("review"), "{}", row("Ctrl-g"));
    assert!(row(":set embedded_editor[!]").contains("embedded editor"));
    assert!(row(":set noembedded_editor").contains("$EDITOR"));
    assert!(row(":q!").contains("unsaved buffers"));
    assert!(row("tuicr --helix-setup").contains("grammars"));
    let edit = lines
        .iter()
        .position(|line| line.trim_start().starts_with(":edit"))
        .expect("the :edit row");
    assert!(
        lines[edit + 1].contains("embedded editor"),
        "listed under :edit, where opening a file is described"
    );
}

#[test]
fn should_leave_the_editor_s_keys_out_of_help_without_an_embedded_editor() {
    // A tuicr built without the embedded editor: the rows would describe
    // an editor it does not have.
    let mut app = app_with(vec![file("a.rs", 3)]);

    let lines = help_lines(&mut app);

    assert!(
        lines.iter().any(|line| line.contains(":edit")),
        "the help itself is there"
    );
    for absent in ["Ctrl-g", "embedded", ":q!", "--helix-setup"] {
        assert!(
            !lines.iter().any(|line| line.contains(absent)),
            "{absent} is listed"
        );
    }
}

// --- quitting with unsaved editor buffers -------------------------------

/// Type `command` at the `:` prompt and submit it.
fn run_command(app: &mut App, command: &str) {
    app.enter_command_mode();
    app.command_buffer.push_str(command);
    crate::handler::handle_command_action(app, Action::SubmitInput);
}

/// An app whose hidden editor pane holds `files` and `scratch` scratch
/// buffers unsaved.
fn app_with_unsaved(files: &[&str], scratch: usize) -> (App, FakeHandle) {
    let (mut app, pane) = app_in_editor();
    offer(&mut app, ctrl('g'));
    app.finish_editor_pane_return();
    app.message = None;
    {
        let mut script = pane.borrow_mut();
        script.state.modified = files.iter().map(PathBuf::from).collect();
        script.state.modified_scratch = scratch;
    }
    (app, pane)
}

fn error_message(app: &App) -> &str {
    match &app.message {
        Some(message) if message.message_type == MessageType::Error => &message.content,
        other => panic!("expected an error in the status line, got {other:?}"),
    }
}

#[test]
fn should_refuse_to_quit_and_count_the_unsaved_buffers() {
    let (mut app, _pane) = app_with_unsaved(&["/repo/src/a.rs", "/repo/src/b.rs"], 1);

    run_command(&mut app, "q");
    assert!(app.should_quit, "the command asks; the event loop checks");
    let blocked = app.quit_blocked_by_editor();

    assert!(blocked);
    assert!(!app.should_quit, "the quit is taken back");
    assert_eq!(
        error_message(&app),
        "3 unsaved editor buffers (:q! quits anyway, e reopens the editor): a.rs, b.rs, 1 scratch"
    );
}

#[test]
fn should_say_one_buffer_in_the_singular() {
    let (mut app, _pane) = app_with_unsaved(&["/repo/src/a.rs"], 0);
    app.should_quit = true;

    assert!(app.quit_blocked_by_editor());

    assert!(
        error_message(&app).starts_with("1 unsaved editor buffer (")
            && error_message(&app).ends_with(": a.rs"),
        "{}",
        error_message(&app)
    );
}

#[test]
fn should_name_a_few_unsaved_files_and_count_the_rest() {
    let (mut app, _pane) =
        app_with_unsaved(&["/r/a.rs", "/r/b.rs", "/r/c.rs", "/r/d.rs", "/r/e.rs"], 0);
    app.should_quit = true;

    assert!(app.quit_blocked_by_editor());

    assert!(
        error_message(&app).starts_with("5 unsaved editor buffers (")
            && error_message(&app).ends_with(": a.rs, b.rs, c.rs, 2 more"),
        "{}",
        error_message(&app)
    );
}

#[test]
fn should_refuse_for_an_unsaved_scratch_buffer_alone() {
    let (mut app, _pane) = app_with_unsaved(&[], 2);
    app.should_quit = true;

    assert!(app.quit_blocked_by_editor());

    assert!(
        error_message(&app).starts_with("2 unsaved editor buffers (")
            && error_message(&app).ends_with(": 2 scratch"),
        "{}",
        error_message(&app)
    );
}

#[test]
fn should_refuse_every_way_of_quitting_not_only_the_command() {
    // Ctrl-C twice, ZZ / ZQ and the quit keys of the modal views all set
    // `should_quit` directly; the event loop asks before acting on it.
    let (mut app, _pane) = app_with_unsaved(&["/repo/a.rs"], 0);

    app.should_quit = true;

    assert!(app.quit_blocked_by_editor());
    assert!(!app.should_quit);
}

#[test]
fn should_quit_when_forced_whatever_is_unsaved() {
    let (mut app, _pane) = app_with_unsaved(&["/repo/a.rs"], 1);

    run_command(&mut app, "q!");

    assert!(app.should_quit);
    assert!(!app.quit_blocked_by_editor(), ":q! is not asked");
    assert!(app.should_quit);
    assert!(app.message.is_none(), "and gets no lecture");
}

#[test]
fn should_quit_freely_when_the_editor_has_nothing_unsaved() {
    let (mut app, _pane) = app_with_unsaved(&[], 0);
    run_command(&mut app, "q");

    assert!(!app.quit_blocked_by_editor());
    assert!(app.should_quit);

    // And when there never was an editor.
    let mut plain_app = app_with(vec![file("a.rs", 3)]);
    run_command(&mut plain_app, "q");
    assert!(!plain_app.quit_blocked_by_editor());
    assert!(plain_app.should_quit);
}

#[test]
fn should_quit_once_the_buffers_have_been_saved() {
    let (mut app, pane) = app_with_unsaved(&["/repo/a.rs"], 0);
    app.should_quit = true;
    assert!(app.quit_blocked_by_editor());

    pane.borrow_mut().state.modified.clear();
    run_command(&mut app, "q");

    assert!(!app.quit_blocked_by_editor());
    assert!(app.should_quit);
}

#[test]
fn should_wait_for_a_write_in_flight_before_answering_a_quit() {
    // `:w`, `Ctrl-g`, quit: the buffer still counts as unsaved for the
    // moment its write takes. Refusing would be wrong a moment later.
    let (mut app, pane) = app_with_unsaved(&["/repo/a.rs"], 0);
    {
        let mut script = pane.borrow_mut();
        script.state.pending_writes = 1;
        script.pump_outcomes.push_back(PaneOutcome::default());
        script.pump_outcomes.push_back(PaneOutcome {
            saved: vec![PathBuf::from("/repo/a.rs")],
            ..PaneOutcome::default()
        });
    }
    app.should_quit = true;

    assert!(!app.quit_blocked_by_editor());

    assert!(app.should_quit);
    assert!(app.message.is_none());
}

#[test]
fn should_still_refuse_when_the_write_in_flight_leaves_other_buffers_unsaved() {
    let (mut app, pane) = app_with_unsaved(&["/repo/a.rs", "/repo/b.rs"], 0);
    {
        let mut script = pane.borrow_mut();
        script.state.pending_writes = 1;
        script.pump_outcomes.push_back(PaneOutcome {
            saved: vec![PathBuf::from("/repo/a.rs")],
            ..PaneOutcome::default()
        });
    }
    app.should_quit = true;

    assert!(app.quit_blocked_by_editor());

    assert!(error_message(&app).ends_with(": b.rs"), "{:?}", app.message);
}

/// An app whose hidden editor is still writing `/repo/a.rs` (a formatter
/// that takes its time runs first) when a quit is asked for.
fn app_with_a_slow_write() -> (App, FakeHandle) {
    let (mut app, pane) = app_with_unsaved(&["/repo/a.rs"], 0);
    app.editor_pane.write_wait = Duration::from_millis(20);
    pane.borrow_mut().state.pending_writes = 1;
    (app, pane)
}

/// The slow write lands: the next pump reports it.
fn write_lands(pane: &FakeHandle) {
    pane.borrow_mut().pump_outcomes.push_back(PaneOutcome {
        saved: vec![PathBuf::from("/repo/a.rs")],
        ..PaneOutcome::default()
    });
}

fn warning_message(app: &App) -> &str {
    match &app.message {
        Some(message) if message.message_type == MessageType::Warning => &message.content,
        other => panic!("expected a warning in the status line, got {other:?}"),
    }
}

#[test]
fn should_put_a_quit_off_while_the_only_unsaved_buffer_is_being_written() {
    // `:w`, `Ctrl-g`, `:q` with a formatter that takes a second and a
    // half: the buffer is not unsaved, it is on its way to disk. The quit
    // was refused as "1 unsaved editor buffer", which suggested `:q!`.
    let (mut app, pane) = app_with_a_slow_write();
    app.should_quit = true;

    let started = Instant::now();
    assert!(app.quit_blocked_by_editor(), "not yet");

    assert!(started.elapsed() < Duration::from_secs(5), "bounded");
    assert!(!app.should_quit);
    assert!(app.quit_deferred());
    assert_eq!(
        warning_message(&app),
        "Quitting when the editor has written a.rs (any key cancels)"
    );

    // Still writing: nothing happens.
    app.poll_editor_pane();
    assert!(!app.should_quit);

    write_lands(&pane);
    assert!(app.poll_editor_pane(), "the quit is news to the loop");

    assert!(app.should_quit, "carried out when the save landed");
    assert!(!app.quit_deferred());
    let mut ctrl_c_timer = None;
    assert!(app.quit_goes_ahead(&mut ctrl_c_timer));
}

#[test]
fn should_drop_a_quit_that_was_put_off_when_the_user_goes_on_working() {
    let (mut app, pane) = app_with_a_slow_write();
    app.should_quit = true;
    assert!(app.quit_blocked_by_editor());

    assert!(app.cancel_deferred_quit(), "any key");
    assert!(!app.cancel_deferred_quit(), "once");

    write_lands(&pane);
    app.poll_editor_pane();
    assert!(!app.should_quit, "the save landing quits nothing now");
    assert!(!app.quit_deferred());
}

#[test]
fn should_refuse_a_quit_that_was_put_off_when_the_buffer_is_still_unsaved() {
    // The write failed, or the buffer was changed again behind it.
    let (mut app, pane) = app_with_a_slow_write();
    app.should_quit = true;
    assert!(app.quit_blocked_by_editor());

    pane.borrow_mut().state.pending_writes = 0;
    assert!(app.poll_editor_pane());

    assert!(!app.should_quit);
    assert!(!app.quit_deferred());
    assert!(error_message(&app).starts_with("1 unsaved editor buffer"));
}

#[test]
fn should_refuse_outright_when_more_is_unsaved_than_is_being_written() {
    let (mut app, pane) = app_with_unsaved(&["/repo/a.rs", "/repo/b.rs"], 0);
    app.editor_pane.write_wait = Duration::from_millis(20);
    pane.borrow_mut().state.pending_writes = 1;
    app.should_quit = true;

    assert!(app.quit_blocked_by_editor());

    assert!(!app.quit_deferred(), "b.rs would be lost");
    assert!(error_message(&app).starts_with("2 unsaved editor buffers"));

    // And for a scratch buffer, which no write is ever in flight for.
    let (mut app, pane) = app_with_unsaved(&["/repo/a.rs"], 1);
    app.editor_pane.write_wait = Duration::from_millis(20);
    pane.borrow_mut().state.pending_writes = 1;
    app.should_quit = true;
    assert!(app.quit_blocked_by_editor());
    assert!(!app.quit_deferred());
}

#[test]
fn should_discard_the_session_when_the_quit_put_off_was_one_that_does() {
    // `:q` with only reviewed marks unsaved throws the session away and
    // quits. Put off, it still has to do both, in that order, later.
    let (mut app, pane) = app_with_a_slow_write();
    app.dirty = true;
    assert!(!app.session.has_comments());

    run_command(&mut app, "q");

    assert!(!app.should_quit);
    assert!(app.dirty, "nothing is thrown away yet");
    assert!(app.quit_deferred());

    write_lands(&pane);
    app.poll_editor_pane();

    assert!(app.should_quit);
    assert!(!app.dirty, "the session was discarded on the way out");
}

#[test]
fn should_take_a_quit_that_was_put_off_back_when_the_editor_is_opened_again() {
    let (mut app, pane) = app_with_a_slow_write();
    app.should_quit = true;
    assert!(app.quit_blocked_by_editor());

    app.open_editor_pane(&target("/repo/a.rs", None), SCREEN)
        .expect("open");
    write_lands(&pane);
    app.poll_editor_pane();

    assert!(!app.should_quit);
    assert_eq!(app.input_mode, InputMode::Editor);
}

// --- a crash of the editor ends the editor, not the review ---------------

#[test]
fn should_go_back_to_the_review_and_say_so_when_the_editor_crashes_on_a_key() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().state.modified = vec![PathBuf::from("/repo/a.rs")];
    pane.borrow_mut().panic_in.push("key");

    let taken = offer(&mut app, plain('x'));
    let refreshed = app.finish_editor_pane_return();

    assert_eq!(taken, Some(true));
    assert!(refreshed);
    assert_eq!(app.input_mode, InputMode::Normal);
    let message = error_message(&app);
    assert!(
        message.starts_with("The embedded editor crashed and was closed"),
        "{message}"
    );
    assert!(message.contains("not saved are lost"), "{message}");
    assert!(
        message.contains("the fake editor panicked in key"),
        "{message}"
    );
    assert!(!app.editor_pane_available());
    assert_eq!(app.editor_pane_state(), None);

    // Files go to `$EDITOR` from here on, and the status line says why.
    let reason = fallback_reason(hand_off(&mut app, &target("/repo/a.rs", None)));
    assert_eq!(
        reason,
        "embedded editor not used: the embedded editor crashed; restart tuicr to try again"
    );
    // What it held unsaved is gone with it: nothing is left to hold a
    // quit back for.
    app.should_quit = true;
    assert!(!app.quit_blocked_by_editor());
}

#[test]
fn should_draw_the_review_again_after_the_editor_crashed_while_drawing() {
    let (mut app, pane) = app_in_editor();
    pane.borrow_mut().panic_in.push("render");

    // The frame the crash happened in is drawn without the editor.
    let terminal = draw(&mut app);
    assert!(!terminal.backend().cursor_visible());

    // The next pass of the event loop.
    app.poll_editor_pane();
    assert!(app.finish_editor_pane_return());

    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(error_message(&app).contains("the fake editor panicked in render"));
    let terminal = draw(&mut app);
    assert!(
        !screen_text(terminal.backend().buffer()).contains(FAKE_BANNER),
        "the review is on screen"
    );
}

#[test]
fn should_say_so_when_the_hidden_editor_crashes_in_the_background() {
    let (mut app, pane) = app_in_editor();
    offer(&mut app, ctrl('g'));
    app.finish_editor_pane_return();
    app.message = None;
    pane.borrow_mut().panic_in.push("pump");

    app.poll_editor_pane();
    assert!(
        app.finish_editor_pane_return(),
        "the message needs painting"
    );

    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(error_message(&app).contains("the fake editor panicked in pump"));
    assert!(!app.finish_editor_pane_return(), "told once");
}

#[test]
fn should_not_quit_behind_a_crash_while_a_quit_waited_for_a_write() {
    let (mut app, pane) = app_with_a_slow_write();
    app.should_quit = true;
    assert!(app.quit_blocked_by_editor());
    pane.borrow_mut().panic_in.push("pump");

    app.poll_editor_pane();
    app.finish_editor_pane_return();
    app.poll_editor_pane();

    assert!(!app.should_quit, "the user is told first");
    assert!(error_message(&app).starts_with("The embedded editor crashed"));
}

// --- what the pane says about its setup ------------------------------------

#[test]
fn should_repeat_the_pane_s_setup_notice_when_the_review_comes_back() {
    // The editor's own status line loses it a moment after a language
    // server has loaded; the review's keeps it until the next message.
    let mut review = Review::new();
    review.open("f.rs");
    review.pane.borrow_mut().state.notice =
        Some("run `tuicr --helix-setup` once: no syntax highlighting yet".to_string());

    review.press(ctrl('g'));

    assert_eq!(
        review.message(),
        "Opened f.rs and reloaded 2 files \u{00b7} \
         run `tuicr --helix-setup` once: no syntax highlighting yet"
    );

    // With nothing missing the status line is what it always was.
    review.pane.borrow_mut().state.notice = None;
    review.open("f.rs");
    review.press(ctrl('g'));
    assert_eq!(review.message(), "Opened f.rs and reloaded 2 files");
}

#[test]
fn should_stop_the_ctrl_c_timer_when_it_holds_the_quit_back() {
    // Ctrl-C twice: the second press asks for the quit and leaves the
    // two-second timer of the first running. The loop clears the status
    // line when that timer runs out, and the refusal went with it.
    let (mut app, _pane) = app_with_unsaved(&["/repo/a.rs"], 0);
    let mut ctrl_c_timer = Some(Instant::now());
    app.should_quit = true;

    assert!(!app.quit_goes_ahead(&mut ctrl_c_timer));

    assert_eq!(ctrl_c_timer, None, "nothing is left to clear the refusal");
    assert!(
        error_message(&app).starts_with("1 unsaved editor buffer"),
        "{}",
        error_message(&app)
    );
    assert!(!app.should_quit);
}

#[test]
fn should_leave_the_ctrl_c_timer_alone_when_the_quit_goes_ahead() {
    let (mut app, _pane) = app_with_unsaved(&[], 0);
    let first_press = Instant::now();
    let mut ctrl_c_timer = Some(first_press);
    app.should_quit = true;

    assert!(app.quit_goes_ahead(&mut ctrl_c_timer));

    assert_eq!(ctrl_c_timer, Some(first_press));
    assert!(app.should_quit);
}

#[test]
fn should_not_discard_review_marks_for_a_quit_it_refuses() {
    // `:q` with only reviewed marks unsaved throws the session away and
    // quits. Refusing the quit after the throwing away would be the worst
    // of both.
    let (mut app, _pane) = app_with_unsaved(&["/repo/a.rs"], 0);
    app.dirty = true;
    assert!(!app.session.has_comments());

    run_command(&mut app, "q");

    assert!(!app.should_quit);
    assert!(app.dirty, "the session was not discarded");
    assert!(error_message(&app).starts_with("1 unsaved editor buffer"));
}

#[test]
fn should_flush_the_editor_on_the_way_out_of_a_forced_quit() {
    let (mut app, pane) = app_with_unsaved(&["/repo/a.rs"], 0);
    pane.borrow_mut()
        .shutdown_errors
        .push("could not write a.rs".to_string());
    run_command(&mut app, "q!");
    assert!(!app.quit_blocked_by_editor());

    // What the event loop does next, before giving the terminal back.
    let errors = app.shutdown_editor_pane();

    assert_eq!(pane.borrow().calls.last(), Some(&PaneCall::Shutdown));
    assert_eq!(errors.len(), 1, "what it could not flush is reported");
}

// --- the option: config and :set ----------------------------------------

#[test]
fn should_use_the_embedded_editor_unless_turned_off() {
    let app = app_with(vec![file("a.rs", 3)]);

    assert!(app.embedded_editor_enabled(), "on by default");
}

#[test]
fn should_apply_the_config_key_without_a_status_message() {
    let mut app = app_with(vec![file("a.rs", 3)]);

    app.init_embedded_editor(false);

    assert!(!app.embedded_editor_enabled());
    assert!(
        app.message.is_none(),
        "startup leaves the status line alone"
    );
}

#[test]
fn should_turn_the_embedded_editor_off_and_on_with_set() {
    let (mut app, pane) = app_with_pane();

    run_command(&mut app, "set noembedded_editor");
    assert!(!app.embedded_editor_enabled());
    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(
        app.message.as_ref().map(|m| m.content.as_str()),
        Some("Embedded editor off: files open in $EDITOR")
    );
    assert_eq!(
        hand_off(&mut app, &target("/repo/a.rs", None)),
        EditorRoute::External { reason: None }
    );

    run_command(&mut app, "set embedded_editor");
    assert!(app.embedded_editor_enabled());
    assert_eq!(
        hand_off(&mut app, &target("/repo/a.rs", None)),
        EditorRoute::Embedded
    );
    assert_eq!(pane.borrow().created_at.len(), 1);
}

#[test]
fn should_toggle_the_embedded_editor_with_set_bang() {
    let (mut app, _pane) = app_with_pane();

    run_command(&mut app, "set embedded_editor!");
    assert!(!app.embedded_editor_enabled());

    run_command(&mut app, "set embedded_editor!");
    assert!(app.embedded_editor_enabled());
    assert_eq!(
        app.message.as_ref().map(|m| m.content.as_str()),
        Some("Embedded editor on: files open inside tuicr (Ctrl-g returns)")
    );
}

#[test]
fn should_warn_when_turned_on_but_there_is_no_editor_to_use() {
    let mut app = app_with(vec![file("a.rs", 3)]);
    app.install_editor_pane_factory(FakePane::failing_factory("helix runtime not found"));
    hand_off(&mut app, &target("/repo/a.rs", None));

    run_command(&mut app, "set embedded_editor");

    let message = app.message.as_ref().expect("message");
    assert_eq!(message.message_type, MessageType::Warning);
    assert_eq!(
        message.content,
        "Embedded editor on, but not usable: helix runtime not found; restart tuicr to try again"
    );
}

#[test]
fn should_keep_the_quit_hold_when_the_embedded_editor_is_turned_off() {
    // Turning the option off closes nothing: what the pane holds unsaved
    // is still there to lose.
    let (mut app, _pane) = app_with_unsaved(&["/repo/a.rs"], 0);

    run_command(&mut app, "set noembedded_editor");
    run_command(&mut app, "q");

    assert!(app.quit_blocked_by_editor());
}

// ── Following a repository switch ────────────────────────────────────────

#[test]
fn should_tell_a_running_editor_where_tuicr_went_and_build_none_for_it() {
    // given: a pane that could be built but has not been
    let (mut app, pane) = app_with_pane();

    // when / then: nothing to tell, and telling builds nothing
    assert!(!app.editor_pane_working_dir_changed(Path::new("/elsewhere")));
    assert!(pane.borrow().created_at.is_empty());

    // given: the editor was used and is hidden again
    app.open_editor_pane(&target("/repo/a.rs", None), SCREEN)
        .expect("the fake pane opens anything");
    app.leave_editor_mode();

    // when
    let repaint = app.editor_pane_working_dir_changed(Path::new("/elsewhere"));

    // then: it was told, and a hidden pane asks for no repaint
    assert!(!repaint);
    assert!(
        pane.borrow()
            .calls
            .contains(&PaneCall::WorkingDir(PathBuf::from("/elsewhere")))
    );
}
