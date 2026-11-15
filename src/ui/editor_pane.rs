//! Fullscreen embedded editor (`InputMode::Editor`).
//!
//! The pane draws its own frame, status line included, so tuicr adds no
//! header or status bar around it: the editor gets every cell.

use ratatui::{
    Frame,
    layout::Position,
    style::Style,
    widgets::{Paragraph, Wrap},
};

use crate::app::App;

pub(super) fn render_editor_pane(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let cursor = app.draw_editor_pane(area, frame.buffer_mut());
    if !app.editor_pane_ready() {
        // Editor mode is only entered with a pane, and the next tick leaves
        // it if the pane is gone; say so rather than show a blank screen.
        let notice = Paragraph::new("The embedded editor is not running. Press Ctrl-g to go back.")
            .style(Style::default().fg(app.theme.fg_secondary))
            .wrap(Wrap { trim: true });
        frame.render_widget(notice, area);
        return;
    }
    // A frame that sets no cursor position has its cursor hidden by
    // ratatui, which is what a pane without a cursor wants. The shape is
    // written by the event loop (`App::take_cursor_style_change`).
    if let Some(cursor) = cursor {
        frame.set_cursor_position(Position::new(cursor.x, cursor.y));
    }
}
