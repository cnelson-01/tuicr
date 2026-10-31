//! The interactive rebase editor modal: one row per commit, oldest first,
//! with its action, and a footer saying what a run would do.

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
use crate::vcs::traits::RebaseAction;

pub(super) fn render_rebase_editor(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let Some(editor) = app.rebase_editor.as_ref() else {
        return;
    };
    let area = centered_rect(80, 80, modal_anchor(app, frame.area()));
    frame.render_widget(Clear, area);
    let title = format!(
        " Interactive rebase \u{00b7} {} commit{} onto {} ",
        editor.rows.len(),
        if editor.rows.len() == 1 { "" } else { "s" },
        editor.base_label
    );
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .style(styles::popup_style(theme))
        .border_style(styles::border_style(theme, true));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 4 {
        return;
    }

    let action_style = |a: RebaseAction| match a {
        RebaseAction::Pick => Style::default().fg(theme.fg_secondary),
        RebaseAction::Reword => Style::default().fg(theme.comment_suggestion),
        RebaseAction::Squash | RebaseAction::Fixup => Style::default().fg(theme.file_modified),
        RebaseAction::Drop => Style::default().fg(theme.diff_del),
    };

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        "  oldest first: git applies top to bottom",
        Style::default().fg(theme.fg_dim),
    ))];
    let rows_room = (inner.height as usize).saturating_sub(5);
    let offset = editor.cursor.saturating_sub(rows_room.saturating_sub(1));
    for (i, row) in editor.rows.iter().enumerate().skip(offset).take(rows_room) {
        let selected = i == editor.cursor;
        let base = if selected {
            styles::selected_style(theme)
        } else {
            Style::default()
        };
        let marker = if selected { " \u{25b6} " } else { "   " };
        let subject = match (&editor.editing, selected) {
            (Some(buf), true) => format!("{buf}\u{2588}"),
            _ => row
                .message
                .as_deref()
                .and_then(|m| m.lines().next())
                .map(|s| format!("{s} \u{270e}"))
                .unwrap_or_else(|| row.subject.clone()),
        };
        let indent = if row.action.melds() {
            "  \u{2514} "
        } else {
            ""
        };
        let mut subject_style = base;
        if row.action == RebaseAction::Drop {
            subject_style = subject_style
                .fg(theme.fg_dim)
                .add_modifier(Modifier::CROSSED_OUT);
        }
        lines.push(Line::from(vec![
            Span::styled(marker, base.fg(theme.border_focused)),
            Span::styled(
                format!("{:<7}", row.action.verb()),
                base.patch(action_style(row.action))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {} ", row.short_id), base.fg(theme.fg_dim)),
            Span::styled(indent.to_string(), base.fg(theme.fg_dim)),
            Span::styled(subject, subject_style),
        ]));
    }
    while lines.len() < rows_room + 1 {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(""));
    let status = match (editor.problem(), editor.summary(&app.rebase_original)) {
        (Some(problem), _) => Span::styled(
            format!("  Can't run: {problem}"),
            Style::default().fg(theme.diff_del),
        ),
        (None, Some(summary)) => Span::styled(
            format!("  \u{21b5} runs: {summary}"),
            Style::default().fg(theme.diff_add),
        ),
        (None, None) => Span::styled("  No changes yet", Style::default().fg(theme.fg_dim)),
    };
    lines.push(Line::from(status));
    let keys = if editor.editing.is_some() {
        "  type the new subject \u{00b7} ctrl-u clear \u{00b7} \u{21b5} keep \u{00b7} esc cancel"
    } else {
        "  p/r/s/f/d pick reword squash fixup drop \u{00b7} J/K move \u{00b7} u reset \u{00b7} \u{21b5} run \u{00b7} esc"
    };
    lines.push(Line::from(Span::styled(
        keys,
        Style::default().fg(theme.fg_secondary),
    )));
    frame.render_widget(
        Paragraph::new(lines).style(styles::popup_style(theme)),
        inner,
    );
}
