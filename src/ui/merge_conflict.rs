//! Fullscreen tri-pane merge conflict screen: ours | result | theirs.
//!
//! The outer panes are attributed to the branch and commit their content
//! comes from; the middle pane previews the resolved file as resolutions
//! are picked. Common lines render across all three panes; each conflict
//! renders a header row spanning the full width followed by body rows
//! aligned and padded to the block's display height (geometry shared with
//! `ConflictBlock::display_height`, so scroll math and rendering agree).
//! The side rejected by a resolution is dimmed; kept sides stay tinted.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, MergeFileState, MergeView};
use crate::theme::Theme;
use crate::ui::status_bar;
use crate::ui::styles;
use crate::ui::text_utils::truncate_or_pad;
use crate::vcs::traits::ConflictCommit;
use crate::vcs::{ConflictResolution, ConflictSegment};

const LINENO_WIDTH: usize = 4;
const DIVIDER: &str = " \u{2502} ";

pub(super) fn render_merge_conflict(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // top bar
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    // Body rows = inner height minus the pane-title row; the scroll math in
    // App reads this back, so refresh it before building rows.
    let body_inner_height = (chunks[1].height.saturating_sub(2)).saturating_sub(1) as usize;
    if let Some(view) = app.merge_view.as_mut() {
        view.viewport_height = body_inner_height;
    }

    let theme = &app.theme;
    let Some(view) = app.merge_view.as_ref() else {
        return;
    };

    render_top_bar(frame, view, theme, chunks[0]);

    let Some(file) = view.files.get(view.current_file) else {
        return;
    };

    let staged_suffix = if file.staged {
        " \u{00b7} staged \u{2713}"
    } else {
        ""
    };
    let body_block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " {} \u{00b7} file {}/{}{} ",
            file.path.display(),
            view.current_file + 1,
            view.files.len(),
            staged_suffix,
        ))
        .border_style(styles::border_style(theme, true))
        .style(styles::panel_style(theme));
    let inner = body_block.inner(chunks[1]);
    frame.render_widget(body_block, chunks[1]);

    if inner.height == 0 {
        render_footer(frame, app, chunks[2]);
        return;
    }

    let pane_width = pane_width(inner.width as usize);
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    lines.push(pane_title_row(view, theme, pane_width));

    let body_height = inner.height.saturating_sub(1) as usize;
    let rows = build_rows(
        file,
        view.current_conflict,
        theme,
        pane_width,
        view.scroll_offset,
        body_height,
    );
    lines.extend(rows);

    frame.render_widget(
        Paragraph::new(lines).style(styles::panel_style(theme)),
        inner,
    );

    render_footer(frame, app, chunks[2]);
}

/// Width of one content pane: total minus two 3-char dividers, in thirds.
fn pane_width(inner_width: usize) -> usize {
    inner_width.saturating_sub(2 * DIVIDER.chars().count()) / 3
}

fn render_top_bar(frame: &mut Frame, view: &MergeView, theme: &Theme, area: Rect) {
    let strip_style = Style::default().bg(theme.status_bar_bg).fg(theme.fg_dim);
    let brand_style = Style::default()
        .bg(theme.status_bar_bg)
        .fg(theme.fg_primary)
        .add_modifier(Modifier::BOLD);

    let total_conflicts: usize = view.files.iter().map(|f| f.file.conflict_count()).sum();
    let unresolved: usize = view
        .files
        .iter()
        .map(MergeFileState::unresolved_count)
        .sum();
    let right = format!(
        " {} resolved \u{00b7} {} unresolved ",
        total_conflicts - unresolved,
        unresolved
    );

    let left_spans = vec![
        Span::styled(" tuicr  ", brand_style),
        Span::styled(" Merge conflicts ", strip_style),
    ];
    let right_span = Span::styled(right.clone(), strip_style);
    let spans = status_bar::build_right_aligned_spans(
        left_spans,
        right_span,
        right.len(),
        area.width as usize,
    );
    frame.render_widget(Paragraph::new(Line::from(spans)).style(strip_style), area);
}

/// `ours · main · c713b11 Tune greeting…` — branch label plus the commit
/// the pane's content comes from, truncated to the pane.
fn side_title(side: &str, label: &str, commit: Option<&ConflictCommit>) -> String {
    match commit {
        Some(commit) => format!(
            " {side} \u{00b7} {label} \u{00b7} {} {}",
            commit.short_id, commit.summary
        ),
        None => format!(" {side} \u{00b7} {label}"),
    }
}

fn pane_title_row<'a>(view: &MergeView, theme: &Theme, pane_width: usize) -> Line<'a> {
    let title_style = Style::default()
        .fg(theme.fg_secondary)
        .add_modifier(Modifier::BOLD);
    let divider = Span::styled(DIVIDER, Style::default().fg(theme.fg_dim));
    Line::from(vec![
        Span::styled(
            truncate_or_pad(
                &side_title("ours", &view.ours_label, view.ours_commit.as_ref()),
                pane_width,
            ),
            title_style,
        ),
        divider.clone(),
        Span::styled(truncate_or_pad(" result", pane_width), title_style),
        divider,
        Span::styled(
            truncate_or_pad(
                &side_title("theirs", &view.theirs_label, view.theirs_commit.as_ref()),
                pane_width,
            ),
            title_style,
        ),
    ])
}

/// Build the visible window of body rows for the current file.
fn build_rows<'a>(
    file: &MergeFileState,
    current_conflict: usize,
    theme: &Theme,
    pane_width: usize,
    scroll_offset: usize,
    height: usize,
) -> Vec<Line<'a>> {
    if let Some(err) = &file.load_error {
        return vec![Line::from(Span::styled(
            format!("  Cannot display this file: {err}"),
            styles::error_inline_style(theme),
        ))];
    }
    if file.file.conflict_count() == 0 {
        return vec![Line::from(Span::styled(
            "  No conflict markers in this file \u{00b7} press w to mark it resolved",
            Style::default().fg(theme.fg_dim),
        ))];
    }

    let full_width = pane_width * 3 + 2 * DIVIDER.chars().count();
    let divider_style = Style::default().fg(theme.fg_dim);
    let mut rows: Vec<Line> = Vec::with_capacity(height);
    let mut row_idx = 0usize;
    let mut ours_lineno = 1usize;
    let mut result_lineno = 1usize;
    let mut theirs_lineno = 1usize;
    let mut conflict_idx = 0usize;
    let window_end = scroll_offset + height;

    for segment in &file.file.segments {
        if row_idx >= window_end {
            break;
        }
        match segment {
            ConflictSegment::Common(segment_lines) => {
                for line in segment_lines {
                    if row_idx >= scroll_offset && row_idx < window_end {
                        let text_style = Style::default().fg(theme.fg_primary);
                        rows.push(Line::from(vec![
                            side_span(line, Some(ours_lineno), pane_width, text_style),
                            Span::styled(DIVIDER, divider_style),
                            side_span(line, Some(result_lineno), pane_width, text_style),
                            Span::styled(DIVIDER, divider_style),
                            side_span(line, Some(theirs_lineno), pane_width, text_style),
                        ]));
                    }
                    ours_lineno += 1;
                    result_lineno += 1;
                    theirs_lineno += 1;
                    row_idx += 1;
                }
            }
            ConflictSegment::Conflict(block) => {
                let resolution = file
                    .resolutions
                    .get(conflict_idx)
                    .copied()
                    .unwrap_or_default();
                if row_idx >= scroll_offset && row_idx < window_end {
                    rows.push(conflict_header_row(
                        conflict_idx,
                        file.file.conflict_count(),
                        resolution,
                        conflict_idx == current_conflict,
                        theme,
                        full_width,
                    ));
                }
                row_idx += 1;

                let result_lines = block.resolved_lines(resolution);
                let body_len = block.display_height(resolution) - 1;
                let keep_ours = matches!(
                    resolution,
                    ConflictResolution::Unresolved
                        | ConflictResolution::Ours
                        | ConflictResolution::Both
                );
                let keep_theirs = matches!(
                    resolution,
                    ConflictResolution::Unresolved
                        | ConflictResolution::Theirs
                        | ConflictResolution::Both
                );
                let ours_style = conflict_side_style(theme, theme.diff_del_bg, keep_ours);
                let theirs_style = conflict_side_style(theme, theme.diff_add_bg, keep_theirs);
                let result_style = Style::default()
                    .fg(theme.fg_primary)
                    .bg(theme.section_highlight_bg());

                for i in 0..body_len {
                    if row_idx >= scroll_offset && row_idx < window_end {
                        let left = match block.ours.get(i) {
                            Some(line) => {
                                side_span(line, Some(ours_lineno + i), pane_width, ours_style)
                            }
                            None => Span::raw(" ".repeat(pane_width)),
                        };
                        let middle = match &result_lines {
                            Some(lines) => match lines.get(i) {
                                Some(line) => side_span(
                                    line,
                                    Some(result_lineno + i),
                                    pane_width,
                                    result_style,
                                ),
                                None => Span::raw(" ".repeat(pane_width)),
                            },
                            None if i == 0 => side_span(
                                "\u{2039}unresolved \u{2014} o/t/b to pick\u{203a}",
                                None,
                                pane_width,
                                Style::default()
                                    .fg(theme.fg_dim)
                                    .add_modifier(Modifier::ITALIC),
                            ),
                            None => Span::raw(" ".repeat(pane_width)),
                        };
                        let right = match block.theirs.get(i) {
                            Some(line) => {
                                side_span(line, Some(theirs_lineno + i), pane_width, theirs_style)
                            }
                            None => Span::raw(" ".repeat(pane_width)),
                        };
                        rows.push(Line::from(vec![
                            left,
                            Span::styled(DIVIDER, divider_style),
                            middle,
                            Span::styled(DIVIDER, divider_style),
                            right,
                        ]));
                    }
                    row_idx += 1;
                }
                ours_lineno += block.ours.len();
                theirs_lineno += block.theirs.len();
                result_lineno += result_lines.map(|lines| lines.len()).unwrap_or(0);
                conflict_idx += 1;
            }
        }
    }

    rows
}

fn conflict_side_style(theme: &Theme, kept_bg: ratatui::style::Color, kept: bool) -> Style {
    if kept {
        Style::default().fg(theme.fg_primary).bg(kept_bg)
    } else {
        Style::default()
            .fg(theme.fg_dim)
            .add_modifier(Modifier::CROSSED_OUT)
    }
}

fn conflict_header_row<'a>(
    conflict_idx: usize,
    conflict_count: usize,
    resolution: ConflictResolution,
    is_current: bool,
    theme: &Theme,
    full_width: usize,
) -> Line<'a> {
    let status = match resolution {
        ConflictResolution::Unresolved => "unresolved",
        ConflictResolution::Ours => "\u{2713} ours",
        ConflictResolution::Theirs => "\u{2713} theirs",
        ConflictResolution::Both => "\u{2713} both",
    };
    let pointer = if is_current { "\u{25b8}" } else { " " };
    let text = format!(
        "{pointer} conflict {}/{} \u{00b7} {status} ",
        conflict_idx + 1,
        conflict_count
    );
    let pad = full_width.saturating_sub(text.chars().count());
    let mut style = Style::default()
        .bg(theme.section_highlight_bg())
        .add_modifier(Modifier::BOLD);
    style = if resolution == ConflictResolution::Unresolved {
        style.fg(theme.message_error_fg)
    } else {
        style.fg(theme.reviewed)
    };
    if is_current {
        style = style.bg(theme.bg_highlight);
    }
    Line::from(Span::styled(
        format!("{text}{}", "\u{2500}".repeat(pad)),
        style,
    ))
}

/// One pane's worth of a row: line number gutter plus truncated content.
/// `lineno: None` renders a blank gutter (e.g. the unresolved placeholder).
fn side_span<'a>(
    line: &str,
    lineno: Option<usize>,
    pane_width: usize,
    text_style: Style,
) -> Span<'a> {
    let gutter = LINENO_WIDTH;
    let content_width = pane_width.saturating_sub(gutter + 1);
    let text = line.replace('\t', "    ");
    let gutter_text = match lineno {
        Some(lineno) => format!("{lineno:>gutter$}"),
        None => " ".repeat(gutter),
    };
    Span::styled(
        format!("{gutter_text} {}", truncate_or_pad(&text, content_width)),
        text_style,
    )
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let hints = " j/k scroll \u{00b7} n/N conflict \u{00b7} }/{ file \u{00b7} o ours \u{00b7} t theirs \u{00b7} b both \u{00b7} u undo \u{00b7} e edit \u{00b7} w write \u{00b7} esc back \u{00b7} q quit";
    let left = vec![Span::styled(hints, Style::default().fg(theme.fg_secondary))];
    let (message_span, message_width) = status_bar::build_message_span(app.message.as_ref(), theme);
    let spans = status_bar::build_right_aligned_spans(
        left,
        message_span,
        message_width,
        area.width as usize,
    );
    let footer = Paragraph::new(Line::from(spans)).style(styles::status_bar_style(theme));
    frame.render_widget(footer, area);
}
