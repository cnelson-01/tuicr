//! The branch picker modal: local branches, then remote-tracking branches,
//! filtered as the user types, with tracking state on the right.

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

pub(super) fn render_branch_picker(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let Some(picker) = app.branch_picker.as_ref() else {
        return;
    };
    let area = centered_rect(60, 70, modal_anchor(app, frame.area()));
    frame.render_widget(Clear, area);

    let picked = picker.picked.len();
    let title = if picked > 0 {
        format!(" Branches \u{00b7} {picked} picked ")
    } else {
        " Branches ".to_string()
    };
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .style(styles::popup_style(theme))
        .border_style(styles::border_style(theme, true));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible = picker.visible();
    // Rows available for branches: the filter line and the hint line sit at
    // the bottom, with a blank line between.
    let rows = usize::from(inner.height).saturating_sub(3);
    let offset = if rows == 0 {
        0
    } else {
        picker.cursor.saturating_sub(rows - 1)
    };

    let mut lines: Vec<Line> = Vec::with_capacity(rows + 3);
    for (row, &idx) in visible.iter().enumerate().skip(offset).take(rows) {
        let branch = &picker.branches[idx];
        let is_cursor = row == picker.cursor;
        let name_style = if is_cursor {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else if branch.is_remote {
            Style::default().fg(theme.fg_dim)
        } else if branch.is_current {
            styles::branch_style(theme).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if is_cursor {
            ">"
        } else if branch.is_current {
            "*"
        } else {
            " "
        };
        let pick = match picker.pick_order(idx) {
            Some(order) => format!("\u{25c6}{order}"),
            None => "  ".to_string(),
        };
        let mut spans = vec![
            Span::styled(format!("{marker} "), name_style),
            Span::styled(pick, styles::branch_style(theme)),
            Span::styled(format!(" {}", branch.name), name_style),
        ];
        let mut tracking = String::new();
        if let Some(upstream) = &branch.upstream {
            tracking.push_str(&format!("  \u{2192} {upstream}"));
            if branch.upstream_gone {
                tracking.push_str(" (gone)");
            }
            if branch.ahead > 0 {
                tracking.push_str(&format!(" \u{2191}{}", branch.ahead));
            }
            if branch.behind > 0 {
                tracking.push_str(&format!(" \u{2193}{}", branch.behind));
            }
        }
        if !tracking.is_empty() {
            spans.push(Span::styled(tracking, Style::default().fg(theme.fg_dim)));
        }
        lines.push(Line::from(spans));
    }
    if visible.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no branches match",
            Style::default().fg(theme.fg_dim),
        )));
    }
    while lines.len() < rows {
        lines.push(Line::from(""));
    }

    lines.push(Line::from(vec![
        Span::styled("  / ", Style::default().fg(theme.fg_secondary)),
        Span::styled(
            picker.filter.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2588}", Style::default().fg(theme.fg_secondary)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "type to filter \u{00b7} tab pick \u{00b7} ctrl-d compare \u{00b7} ctrl-l log \u{00b7} \u{21b5} checkout \u{00b7} ctrl-s stash+switch \u{00b7} esc",
        Style::default().fg(theme.fg_secondary),
    )));

    frame.render_widget(
        Paragraph::new(lines).style(styles::popup_style(theme)),
        inner,
    );
}
