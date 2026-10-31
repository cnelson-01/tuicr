//! The git console (`:console`): one row per command with time, result,
//! duration, and the command line, followed by its output for writes and
//! failures.

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::app::App;
use crate::ui::styles;

pub(super) fn render_console(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let Some(view) = app.console.as_ref() else {
        return;
    };
    let entries = crate::git_console::snapshot();
    let shown: Vec<_> = entries
        .iter()
        .filter(|e| !view.mutating_only || e.mutating)
        .collect();
    let filter = if view.mutating_only {
        "changes only"
    } else {
        "every command"
    };
    let title = format!(" Git console \u{00b7} {} \u{00b7} {filter} ", shown.len());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .style(styles::panel_style(theme))
        .border_style(styles::border_style(theme, true));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for entry in &shown {
        let (mark, mark_style) = if entry.ok {
            ("\u{2713}", Style::default().fg(theme.diff_add))
        } else {
            ("\u{2717}", Style::default().fg(theme.diff_del))
        };
        let status = match (entry.ok, entry.status) {
            (true, _) => String::new(),
            (false, Some(code)) => format!(" exit {code}"),
            (false, None) => " not started".to_string(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", entry.at.format("%H:%M:%S")),
                Style::default().fg(theme.fg_dim),
            ),
            Span::styled(mark, mark_style.add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" {:>5}ms ", entry.duration.as_millis()),
                Style::default().fg(theme.fg_dim),
            ),
            Span::styled(
                entry.command.clone(),
                if entry.mutating {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg_secondary)
                },
            ),
            Span::styled(status, mark_style),
        ]));
        if entry.mutating || !entry.ok {
            for out in entry.output.iter().filter(|l| !l.trim().is_empty()) {
                lines.push(Line::from(Span::styled(
                    format!("                   {out}"),
                    Style::default().fg(theme.fg_dim),
                )));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " Nothing yet. Git commands tuicr runs show up here (m shows reads too).",
            Style::default().fg(theme.fg_dim),
        )));
    }

    let height = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(height);
    let scroll_from_bottom = view.scroll_from_bottom.min(max_scroll);
    let top = max_scroll - scroll_from_bottom;
    if let Some(view) = app.console.as_mut() {
        view.scroll_from_bottom = scroll_from_bottom;
        view.viewport_height = height;
    }
    let visible: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
    frame.render_widget(
        Paragraph::new(visible).style(styles::panel_style(&app.theme)),
        inner,
    );
}
