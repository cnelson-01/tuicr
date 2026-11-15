//! tuicr's input events (crossterm) in the editor's own vocabulary.
//!
//! The editor never sees a key release, never sees the chord tuicr keeps
//! for itself (`Ctrl-g`), and gets mouse positions counted from the corner
//! of its pane.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MediaKeyCode, ModifierKeyCode, MouseButton,
    MouseEvent, MouseEventKind,
};
use helix_embed as hx;
use ratatui::layout::Rect;

use super::pane::is_return_chord;

/// The key as the editor names it. `None` for a release and for tuicr's
/// return chord, which the editor must not act on.
///
/// `BackTab` becomes Shift-Tab, the only spelling the editor's keymap has
/// for it. Modifiers the editor has no bit for (Hyper, Meta) are dropped.
pub(super) fn key_event(key: KeyEvent) -> Option<hx::KeyEvent> {
    if key.kind == KeyEventKind::Release || is_return_chord(&key) {
        return None;
    }
    let mut modifiers = key_modifiers(key.modifiers);
    let code = match key.code {
        KeyCode::BackTab => {
            modifiers |= hx::KeyModifiers::SHIFT;
            hx::KeyCode::Tab
        }
        KeyCode::Backspace => hx::KeyCode::Backspace,
        KeyCode::Enter => hx::KeyCode::Enter,
        KeyCode::Left => hx::KeyCode::Left,
        KeyCode::Right => hx::KeyCode::Right,
        KeyCode::Up => hx::KeyCode::Up,
        KeyCode::Down => hx::KeyCode::Down,
        KeyCode::Home => hx::KeyCode::Home,
        KeyCode::End => hx::KeyCode::End,
        KeyCode::PageUp => hx::KeyCode::PageUp,
        KeyCode::PageDown => hx::KeyCode::PageDown,
        KeyCode::Tab => hx::KeyCode::Tab,
        KeyCode::Delete => hx::KeyCode::Delete,
        KeyCode::Insert => hx::KeyCode::Insert,
        KeyCode::F(n) => hx::KeyCode::F(n),
        KeyCode::Char(c) => hx::KeyCode::Char(c),
        KeyCode::Null => hx::KeyCode::Null,
        KeyCode::Esc => hx::KeyCode::Esc,
        KeyCode::CapsLock => hx::KeyCode::CapsLock,
        KeyCode::ScrollLock => hx::KeyCode::ScrollLock,
        KeyCode::NumLock => hx::KeyCode::NumLock,
        KeyCode::PrintScreen => hx::KeyCode::PrintScreen,
        KeyCode::Pause => hx::KeyCode::Pause,
        KeyCode::Menu => hx::KeyCode::Menu,
        KeyCode::KeypadBegin => hx::KeyCode::KeypadBegin,
        KeyCode::Media(media) => hx::KeyCode::Media(media_key(media)),
        KeyCode::Modifier(modifier) => hx::KeyCode::Modifier(modifier_key(modifier)),
    };
    Some(hx::KeyEvent { code, modifiers })
}

fn key_modifiers(modifiers: KeyModifiers) -> hx::KeyModifiers {
    let mut out = hx::KeyModifiers::NONE;
    for (ours, theirs) in [
        (KeyModifiers::SHIFT, hx::KeyModifiers::SHIFT),
        (KeyModifiers::CONTROL, hx::KeyModifiers::CONTROL),
        (KeyModifiers::ALT, hx::KeyModifiers::ALT),
        (KeyModifiers::SUPER, hx::KeyModifiers::SUPER),
    ] {
        if modifiers.contains(ours) {
            out |= theirs;
        }
    }
    out
}

fn media_key(key: MediaKeyCode) -> hx::MediaKeyCode {
    match key {
        MediaKeyCode::Play => hx::MediaKeyCode::Play,
        MediaKeyCode::Pause => hx::MediaKeyCode::Pause,
        MediaKeyCode::PlayPause => hx::MediaKeyCode::PlayPause,
        MediaKeyCode::Reverse => hx::MediaKeyCode::Reverse,
        MediaKeyCode::Stop => hx::MediaKeyCode::Stop,
        MediaKeyCode::FastForward => hx::MediaKeyCode::FastForward,
        MediaKeyCode::Rewind => hx::MediaKeyCode::Rewind,
        MediaKeyCode::TrackNext => hx::MediaKeyCode::TrackNext,
        MediaKeyCode::TrackPrevious => hx::MediaKeyCode::TrackPrevious,
        MediaKeyCode::Record => hx::MediaKeyCode::Record,
        MediaKeyCode::LowerVolume => hx::MediaKeyCode::LowerVolume,
        MediaKeyCode::RaiseVolume => hx::MediaKeyCode::RaiseVolume,
        MediaKeyCode::MuteVolume => hx::MediaKeyCode::MuteVolume,
    }
}

fn modifier_key(key: ModifierKeyCode) -> hx::ModifierKeyCode {
    match key {
        ModifierKeyCode::LeftShift => hx::ModifierKeyCode::LeftShift,
        ModifierKeyCode::LeftControl => hx::ModifierKeyCode::LeftControl,
        ModifierKeyCode::LeftAlt => hx::ModifierKeyCode::LeftAlt,
        ModifierKeyCode::LeftSuper => hx::ModifierKeyCode::LeftSuper,
        ModifierKeyCode::LeftHyper => hx::ModifierKeyCode::LeftHyper,
        ModifierKeyCode::LeftMeta => hx::ModifierKeyCode::LeftMeta,
        ModifierKeyCode::RightShift => hx::ModifierKeyCode::RightShift,
        ModifierKeyCode::RightControl => hx::ModifierKeyCode::RightControl,
        ModifierKeyCode::RightAlt => hx::ModifierKeyCode::RightAlt,
        ModifierKeyCode::RightSuper => hx::ModifierKeyCode::RightSuper,
        ModifierKeyCode::RightHyper => hx::ModifierKeyCode::RightHyper,
        ModifierKeyCode::RightMeta => hx::ModifierKeyCode::RightMeta,
        ModifierKeyCode::IsoLevel3Shift => hx::ModifierKeyCode::IsoLevel3Shift,
        ModifierKeyCode::IsoLevel5Shift => hx::ModifierKeyCode::IsoLevel5Shift,
    }
}

/// The mouse event with its position counted from the top-left corner of
/// `area`, the rectangle the pane is drawn in.
///
/// An event outside the pane is not the editor's: `None`. The exceptions
/// are a drag and a button release, which belong to the press that started
/// inside; they are moved to the nearest cell of the pane, so a selection
/// dragged past the edge still ends.
pub(super) fn mouse_event(mouse: MouseEvent, area: Rect) -> Option<hx::MouseEvent> {
    if area.is_empty() {
        return None;
    }
    let kind = match mouse.kind {
        MouseEventKind::Down(button) => hx::MouseEventKind::Down(mouse_button(button)),
        MouseEventKind::Up(button) => hx::MouseEventKind::Up(mouse_button(button)),
        MouseEventKind::Drag(button) => hx::MouseEventKind::Drag(mouse_button(button)),
        MouseEventKind::Moved => hx::MouseEventKind::Moved,
        MouseEventKind::ScrollDown => hx::MouseEventKind::ScrollDown,
        MouseEventKind::ScrollUp => hx::MouseEventKind::ScrollUp,
        MouseEventKind::ScrollLeft => hx::MouseEventKind::ScrollLeft,
        MouseEventKind::ScrollRight => hx::MouseEventKind::ScrollRight,
    };
    let inside = area.contains(ratatui::layout::Position::new(mouse.column, mouse.row));
    let follows_a_press = matches!(mouse.kind, MouseEventKind::Up(_) | MouseEventKind::Drag(_));
    if !inside && !follows_a_press {
        return None;
    }
    let column = mouse.column.clamp(area.left(), area.right() - 1) - area.left();
    let row = mouse.row.clamp(area.top(), area.bottom() - 1) - area.top();
    Some(hx::MouseEvent {
        kind,
        column,
        row,
        modifiers: key_modifiers(mouse.modifiers),
    })
}

fn mouse_button(button: MouseButton) -> hx::MouseButton {
    match button {
        MouseButton::Left => hx::MouseButton::Left,
        MouseButton::Right => hx::MouseButton::Right,
        MouseButton::Middle => hx::MouseButton::Middle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn hx_key(code: hx::KeyCode, modifiers: hx::KeyModifiers) -> Option<hx::KeyEvent> {
        Some(hx::KeyEvent { code, modifiers })
    }

    #[test]
    fn should_pass_a_plain_character_and_its_modifiers_through() {
        assert_eq!(
            key_event(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            hx_key(hx::KeyCode::Char('x'), hx::KeyModifiers::NONE)
        );
        assert_eq!(
            key_event(key(
                KeyCode::Char('w'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            hx_key(
                hx::KeyCode::Char('w'),
                hx::KeyModifiers::CONTROL | hx::KeyModifiers::ALT
            )
        );
        // An upper-case letter arrives with Shift, as the editor's own
        // terminal layer reports it; the editor drops the Shift itself.
        assert_eq!(
            key_event(key(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            hx_key(hx::KeyCode::Char('A'), hx::KeyModifiers::SHIFT)
        );
        assert_eq!(
            key_event(key(KeyCode::Char('s'), KeyModifiers::SUPER)),
            hx_key(hx::KeyCode::Char('s'), hx::KeyModifiers::SUPER)
        );
    }

    #[test]
    fn should_drop_a_key_release_and_keep_a_repeat() {
        let released = KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        let repeated =
            KeyEvent::new_with_kind(KeyCode::Char('j'), KeyModifiers::NONE, KeyEventKind::Repeat);

        assert_eq!(key_event(released), None);
        assert_eq!(
            key_event(repeated),
            hx_key(hx::KeyCode::Char('j'), hx::KeyModifiers::NONE)
        );
    }

    #[test]
    fn should_never_hand_the_return_chord_to_the_editor() {
        assert_eq!(
            key_event(key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            None
        );
        // With a lock key on it is the same chord.
        assert_eq!(
            key_event(KeyEvent::new_with_kind_and_state(
                KeyCode::Char('g'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
                KeyEventState::CAPS_LOCK,
            )),
            None
        );
        // Its neighbours are the editor's.
        assert_eq!(
            key_event(key(KeyCode::Char('g'), KeyModifiers::NONE)),
            hx_key(hx::KeyCode::Char('g'), hx::KeyModifiers::NONE)
        );
        assert_eq!(
            key_event(key(
                KeyCode::Char('g'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            hx_key(
                hx::KeyCode::Char('g'),
                hx::KeyModifiers::CONTROL | hx::KeyModifiers::ALT
            )
        );
    }

    #[test]
    fn should_turn_back_tab_into_shift_tab() {
        // crossterm reports it with or without the Shift bit, depending on
        // the keyboard protocol in use.
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            assert_eq!(
                key_event(key(KeyCode::BackTab, modifiers)),
                hx_key(hx::KeyCode::Tab, hx::KeyModifiers::SHIFT)
            );
        }
        assert_eq!(
            key_event(key(KeyCode::BackTab, KeyModifiers::CONTROL)),
            hx_key(
                hx::KeyCode::Tab,
                hx::KeyModifiers::SHIFT | hx::KeyModifiers::CONTROL
            )
        );
        assert_eq!(
            key_event(key(KeyCode::Tab, KeyModifiers::NONE)),
            hx_key(hx::KeyCode::Tab, hx::KeyModifiers::NONE)
        );
    }

    #[test]
    fn should_name_the_special_keys_as_the_editor_does() {
        let cases = [
            (KeyCode::Esc, hx::KeyCode::Esc),
            (KeyCode::Enter, hx::KeyCode::Enter),
            (KeyCode::Backspace, hx::KeyCode::Backspace),
            (KeyCode::Delete, hx::KeyCode::Delete),
            (KeyCode::Insert, hx::KeyCode::Insert),
            (KeyCode::Left, hx::KeyCode::Left),
            (KeyCode::Right, hx::KeyCode::Right),
            (KeyCode::Up, hx::KeyCode::Up),
            (KeyCode::Down, hx::KeyCode::Down),
            (KeyCode::Home, hx::KeyCode::Home),
            (KeyCode::End, hx::KeyCode::End),
            (KeyCode::PageUp, hx::KeyCode::PageUp),
            (KeyCode::PageDown, hx::KeyCode::PageDown),
            (KeyCode::F(5), hx::KeyCode::F(5)),
            (KeyCode::Null, hx::KeyCode::Null),
            (
                KeyCode::Media(MediaKeyCode::PlayPause),
                hx::KeyCode::Media(hx::MediaKeyCode::PlayPause),
            ),
            (
                KeyCode::Modifier(ModifierKeyCode::RightAlt),
                hx::KeyCode::Modifier(hx::ModifierKeyCode::RightAlt),
            ),
        ];
        for (ours, theirs) in cases {
            assert_eq!(
                key_event(key(ours, KeyModifiers::NONE)),
                hx_key(theirs, hx::KeyModifiers::NONE),
                "{ours:?}"
            );
        }
    }

    #[test]
    fn should_drop_modifiers_the_editor_has_no_name_for() {
        assert_eq!(
            key_event(key(
                KeyCode::Char('k'),
                KeyModifiers::HYPER | KeyModifiers::META | KeyModifiers::ALT
            )),
            hx_key(hx::KeyCode::Char('k'), hx::KeyModifiers::ALT)
        );
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    const AREA: Rect = Rect {
        x: 10,
        y: 5,
        width: 40,
        height: 12,
    };

    #[test]
    fn should_count_mouse_positions_from_the_pane_s_corner() {
        let click = mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 10, 5), AREA)
            .expect("the corner is inside");
        assert_eq!((click.column, click.row), (0, 0));
        assert_eq!(click.kind, hx::MouseEventKind::Down(hx::MouseButton::Left));

        let last = mouse_event(mouse(MouseEventKind::ScrollDown, 49, 16), AREA)
            .expect("the last cell is inside");
        assert_eq!((last.column, last.row), (39, 11));
        assert_eq!(last.kind, hx::MouseEventKind::ScrollDown);

        let modified = mouse_event(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 12,
                row: 9,
                modifiers: KeyModifiers::ALT,
            },
            AREA,
        )
        .expect("inside");
        assert_eq!((modified.column, modified.row), (2, 4));
        assert_eq!(modified.modifiers, hx::KeyModifiers::ALT);
        assert_eq!(
            modified.kind,
            hx::MouseEventKind::Down(hx::MouseButton::Right)
        );
    }

    #[test]
    fn should_keep_presses_and_scrolls_outside_the_pane_from_the_editor() {
        for (column, row) in [(9, 5), (10, 4), (50, 10), (20, 17), (0, 0)] {
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::ScrollUp,
                MouseEventKind::Moved,
            ] {
                assert_eq!(
                    mouse_event(mouse(kind, column, row), AREA),
                    None,
                    "{kind:?} at {column},{row}"
                );
            }
        }
    }

    #[test]
    fn should_end_a_drag_that_left_the_pane_on_its_nearest_cell() {
        let dragged = mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), 3, 30), AREA)
            .expect("a drag is followed out of the pane");
        assert_eq!((dragged.column, dragged.row), (0, 11));

        let released = mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 70, 2), AREA)
            .expect("so is the release");
        assert_eq!((released.column, released.row), (39, 0));
    }

    #[test]
    fn should_have_no_mouse_before_the_pane_was_drawn() {
        assert_eq!(
            mouse_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
                Rect::default()
            ),
            None
        );
    }
}
