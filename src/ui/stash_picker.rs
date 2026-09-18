//! The stash picker modal: `git stash list` with pop / apply / drop.

use ratatui::{
    Frame,
    layout::Alignment,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::app::App;
use crate::ui::styles;
use crate::ui::submit_modals::{centered_rect, modal_anchor};

pub(super) fn render_stash_picker(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let Some(picker) = app.stash_picker.as_ref() else {
        return;
    };
    let area = centered_rect(60, 50, modal_anchor(app, frame.area()));
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Stashes ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .style(styles::popup_style(theme))
        .border_style(styles::border_style(theme, true));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = usize::from(inner.height).saturating_sub(2);
    let offset = if rows == 0 {
        0
    } else {
        picker.cursor.saturating_sub(rows - 1)
    };

    let mut lines: Vec<Line> = Vec::with_capacity(rows + 2);
    for (row, entry) in picker.entries.iter().enumerate().skip(offset).take(rows) {
        let is_cursor = row == picker.cursor;
        let style = if is_cursor {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if is_cursor { ">" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} stash@{{{}}} ", entry.index),
                if is_cursor {
                    style
                } else {
                    styles::hash_style(theme)
                },
            ),
            Span::styled(entry.message.clone(), style),
        ]));
    }
    while lines.len() < rows {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "\u{21b5} pop \u{00b7} a apply \u{00b7} d drop \u{00b7} esc close",
        Style::default().fg(theme.fg_secondary),
    )));

    frame.render_widget(
        Paragraph::new(lines).style(styles::popup_style(theme)),
        inner,
    );
}
