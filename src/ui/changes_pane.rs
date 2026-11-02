//! The changes window docked in the file tree's place: the working tree's
//! status in four groups, each entry with its checkbox (staged or not), git's
//! status letter and its path. Submodule rows carry their state after the
//! name; the entries inside a dirty submodule hang under it, indented.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, ChangesRow};
use crate::theme::Theme;
use crate::ui::styles;
use crate::vcs::{ChangeArea, StatusEntry};

const EXPANDED_GLYPH: &str = "\u{25bc}"; // ▼
const COLLAPSED_GLYPH: &str = "\u{25b6}"; // ▶
const STAGED_BOX: &str = "\u{25a3}"; // ▣
const UNSTAGED_BOX: &str = "\u{25a2}"; // ▢
const CONFLICT_MARK: &str = "\u{2716}"; // ✖

pub(super) fn render_changes_pane(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.changes_pane_focused();
    let theme = &app.theme;
    let Some(pane) = app.changes_pane.as_mut() else {
        return;
    };

    let mut title = " Changes".to_string();
    for (area, count) in pane.counts() {
        title.push_str(&format!(" \u{00b7} {count} {}", area.noun()));
    }
    title.push(' ');
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(styles::panel_style(theme))
        .border_style(styles::border_style(theme, focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    pane.inner_area = Some(inner);
    let height = inner.height as usize;
    pane.viewport_height = height;

    // Keep the cursor inside the viewport whatever moved it.
    let mut offset = pane.scroll_offset;
    if height > 0 {
        if pane.cursor < offset {
            offset = pane.cursor;
        } else if pane.cursor >= offset + height {
            offset = pane.cursor + 1 - height;
        }
    }
    let offset = offset.min(pane.rows.len().saturating_sub(1));
    pane.scroll_offset = offset;

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    if pane.rows.is_empty() {
        let text = match pane.error.as_deref() {
            Some(error) => format!(" {error}"),
            None => " Nothing to commit \u{00b7} working tree clean".to_string(),
        };
        lines.push(Line::from(Span::styled(
            text,
            Style::default().fg(theme.fg_dim),
        )));
    }
    for (row_idx, row) in pane.rows.iter().enumerate().skip(offset).take(height) {
        let mut line = match row {
            ChangesRow::Group { area, count } => {
                group_line(theme, *area, *count, pane.folded.contains(area))
            }
            ChangesRow::Entry(idx) => {
                let entry = &pane.entries[*idx];
                let collapsed = entry.is_submodule() && pane.collapsed.contains(&entry.worktree());
                let nested = entry.is_submodule() && pane.has_nested(&entry.worktree());
                entry_line(theme, entry, nested, collapsed)
            }
        };
        if row_idx == pane.cursor {
            line = line.style(styles::selected_style(theme));
        }
        lines.push(line);
    }

    frame.render_widget(
        Paragraph::new(lines).style(styles::panel_style(theme)),
        inner,
    );
}

fn group_line(theme: &Theme, area: ChangeArea, count: usize, folded: bool) -> Line<'static> {
    let glyph = if folded {
        COLLAPSED_GLYPH
    } else {
        EXPANDED_GLYPH
    };
    let label_style = match area {
        ChangeArea::Conflict => Style::default()
            .fg(theme.diff_del)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().add_modifier(Modifier::BOLD),
    };
    Line::from(vec![
        Span::styled(format!("{glyph} "), styles::dir_icon_style(theme)),
        Span::styled(area.label().to_string(), label_style),
        Span::styled(
            format!(" \u{00b7} {count}"),
            Style::default().fg(theme.fg_dim),
        ),
    ])
}

fn entry_line(theme: &Theme, entry: &StatusEntry, nested: bool, collapsed: bool) -> Line<'static> {
    let indent = "  ".repeat(entry.depth + 1);
    let (mark, mark_style) = match entry.area {
        ChangeArea::Staged => (STAGED_BOX, styles::reviewed_style(theme)),
        ChangeArea::Conflict => (CONFLICT_MARK, Style::default().fg(theme.diff_del)),
        ChangeArea::Unstaged | ChangeArea::Untracked => {
            (UNSTAGED_BOX, styles::pending_style(theme))
        }
    };
    let glyph = entry.glyph();
    let glyph_style = match entry.area {
        ChangeArea::Conflict => Style::default()
            .fg(theme.diff_del)
            .add_modifier(Modifier::BOLD),
        _ if entry.is_submodule() => styles::remote_branch_style(theme),
        ChangeArea::Untracked => Style::default().fg(theme.fg_dim),
        _ => styles::file_status_style(theme, glyph),
    };
    let shown = if entry.area == ChangeArea::Conflict {
        entry.code.clone()
    } else {
        glyph.to_string()
    };

    let mut spans = vec![
        Span::raw(indent),
        Span::styled(format!("{mark} "), mark_style),
        Span::styled(format!("{shown} "), glyph_style),
    ];

    // Directory part dim, file name plain, as an IDE lists a flat change set.
    let path = entry.path.display().to_string();
    match path.rfind('/') {
        Some(cut) if cut + 1 < path.len() => {
            spans.push(Span::styled(
                path[..=cut].to_string(),
                Style::default().fg(theme.fg_dim),
            ));
            spans.push(Span::raw(path[cut + 1..].to_string()));
        }
        _ => spans.push(Span::raw(path)),
    }
    if let Some(orig) = entry.orig_path.as_ref() {
        spans.push(Span::styled(
            format!(" \u{2190} {}", orig.display()),
            Style::default().fg(theme.fg_dim),
        ));
    }

    if let Some(state) = entry.submodule.as_ref() {
        let mut tokens: Vec<String> = Vec::new();
        if state.ahead > 0 {
            tokens.push(format!("\u{2191}{}", state.ahead));
        }
        if state.behind > 0 {
            tokens.push(format!("\u{2193}{}", state.behind));
        }
        if state.commit_changed && state.ahead == 0 && state.behind == 0 {
            tokens.push("commit".to_string());
        }
        if state.modified {
            tokens.push("modified".to_string());
        }
        if state.untracked {
            tokens.push("untracked".to_string());
        }
        if !tokens.is_empty() {
            spans.push(Span::styled(
                format!("  {}", tokens.join(" ")),
                Style::default().fg(theme.fg_dim),
            ));
        }
        if nested {
            let glyph = if collapsed {
                COLLAPSED_GLYPH
            } else {
                EXPANDED_GLYPH
            };
            spans.push(Span::styled(
                format!(" {glyph}"),
                styles::dir_icon_style(theme),
            ));
        }
    }
    Line::from(spans)
}
