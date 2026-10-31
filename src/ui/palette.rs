//! Command palette modal and the which-key popup, both drawn from the action
//! catalog (`crate::input::catalog`).

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, InputMode};
use crate::input::catalog::{self, ENTRIES, Scope};
use crate::ui::styles;
use crate::ui::submit_modals::{centered_rect, modal_anchor};

pub(super) fn render_palette(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let Some(palette) = app.palette.as_ref() else {
        return;
    };
    let area = centered_rect(70, 75, modal_anchor(app, frame.area()));
    frame.render_widget(Clear, area);
    let title = format!(
        " Actions \u{00b7} {} of {} ",
        palette.matches.len(),
        ENTRIES.len()
    );
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .style(styles::popup_style(theme))
        .border_style(styles::border_style(theme, true));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 3 || inner.width < 20 {
        return;
    }

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    lines.push(Line::from(vec![
        Span::styled(" > ", Style::default().fg(theme.border_focused)),
        Span::styled(
            palette.query.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2588}", Style::default().fg(theme.fg_secondary)),
    ]));
    lines.push(Line::from(""));

    let rows = (inner.height as usize).saturating_sub(2);
    let offset = palette.cursor.saturating_sub(rows.saturating_sub(1));
    let leader = app.leader_key;
    for (row, &idx) in palette.matches.iter().enumerate().skip(offset).take(rows) {
        let entry = &ENTRIES[idx];
        let selected = row == palette.cursor;
        let keys = catalog::display_keys(entry.keys, leader);
        let scope = if entry.scope == Scope::Global || entry.scope == palette.scope {
            String::new()
        } else {
            format!(" [{}]", entry.scope.label())
        };
        let group = format!("{:<13}", entry.group);
        let left_width = 3 + group.width() + entry.desc.width() + scope.width();
        let pad = width.saturating_sub(left_width + keys.width() + 1).max(1);
        let base = if selected {
            styles::selected_style(theme).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if selected { " \u{25b6} " } else { "   " };
        lines.push(Line::from(vec![
            Span::styled(marker, base.fg(theme.border_focused)),
            Span::styled(group, base.fg(theme.fg_dim)),
            Span::styled(entry.desc, base),
            Span::styled(scope, base.fg(theme.fg_dim)),
            Span::styled(" ".repeat(pad), base),
            Span::styled(keys, base.fg(theme.help_indicator)),
            Span::styled(" ", base),
        ]));
    }
    if palette.matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "   no action matches",
            Style::default().fg(theme.fg_dim),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).style(styles::popup_style(theme)),
        inner,
    );
}

/// After a prefix key, list what can follow it in a box above the status
/// bar, in as many columns as it takes.
pub(super) fn render_which_key(frame: &mut Frame, app: &App, status_area: Rect) {
    let Some(prefix) = app.pending_prefix else {
        return;
    };
    if app.input_mode != InputMode::Normal {
        return;
    }
    let entries = catalog::continuations(prefix);
    if entries.is_empty() || status_area.y < 4 {
        return;
    }
    let theme = &app.theme;
    let key_w = entries.iter().map(|(k, _)| k.width()).max().unwrap_or(1);
    let desc_w = entries
        .iter()
        .map(|(_, d)| d.width())
        .max()
        .unwrap_or(1)
        .min(40);
    let col_w = key_w + 2 + desc_w + 3;
    let max_rows = (status_area.y as usize).saturating_sub(3).clamp(1, 14);
    let cols_fit = ((status_area.width as usize).saturating_sub(2) / col_w).max(1);
    let mut rows = entries.len().div_ceil(cols_fit).min(max_rows).max(1);
    let cols = entries.len().div_ceil(rows).min(cols_fit);
    rows = entries.len().div_ceil(cols).min(max_rows);
    let width = (cols * col_w + 2).min(status_area.width as usize) as u16;
    let height = rows as u16 + 2;
    let area = Rect {
        x: status_area.x + status_area.width.saturating_sub(width),
        y: status_area.y.saturating_sub(height),
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" {} ", prefix.title(app.leader_key)))
        .borders(Borders::ALL)
        .style(styles::popup_style(theme))
        .border_style(styles::border_style(theme, true));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut spans = Vec::new();
        for c in 0..cols {
            let Some((key, desc)) = entries.get(c * rows + r) else {
                continue;
            };
            let desc: String = if desc.width() > desc_w {
                let mut cut: String = desc.chars().take(desc_w.saturating_sub(1)).collect();
                cut.push('\u{2026}');
                cut
            } else {
                desc.to_string()
            };
            spans.push(Span::styled(
                format!(" {key:>key_w$} "),
                Style::default()
                    .fg(theme.help_indicator)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(format!(" {desc:<desc_w$}  ")));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(lines).style(styles::popup_style(theme)),
        inner,
    );
}
