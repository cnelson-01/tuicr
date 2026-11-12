use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    AnnotatedLine, App, DiffSource, ExpandDirection, FocusedPanel, GAP_EXPAND_BATCH, GapId,
    InputMode, LocalEditMark,
};
use crate::model::{DiffLine, FileStatus, LineOrigin, LineRange, LineSide};
use crate::theme::Theme;
use crate::ui::comment_panel;
use crate::ui::diff_view::{
    apply_horizontal_scroll, apply_local_edit_mark, comment_box_row, comment_type_presentation,
    cursor_indicator, cursor_indicator_spaced, diff_stat_title, hunk_header_text_and_style,
    paint_cursor_line_highlight, paint_visual_selection_overlay, populate_row_to_annotation,
    render_expander_line, render_hidden_lines, scroll_comment_input_into_view, skip_comment_box,
};
use crate::ui::styles;
use crate::ui::text_utils::{
    apply_search_highlight_pairs, apply_search_highlight_spans, apply_search_highlight_text,
    truncate_or_pad, truncate_or_pad_pairs_by_chars, truncate_or_pad_spans, wrap_spans,
};
use crate::vcs::git::calculate_gap;

/// How a code row joins its two columns across the divider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SbsLink {
    /// Unchanged on both sides: the plain divider.
    #[default]
    None,
    /// A removed line beside the line that replaced it.
    Both,
    /// A removed line with nothing beside it. `fold` is set on the first
    /// such row of a change block, where the new side closes.
    OldOnly { fold: bool },
    /// An added line with nothing beside it; `fold` likewise, for the old
    /// side.
    NewOnly { fold: bool },
}

#[derive(Clone, Default)]
struct SbsRowMeta {
    left_content: Vec<Span<'static>>,
    right_content: Vec<Span<'static>>,
    left_prefix: Vec<Span<'static>>,
    right_prefix: Vec<Span<'static>>,
    left_pad_style: Style,
    right_pad_style: Style,
    /// Background of each side's line-number gutter when the row's colour
    /// runs through it (IDE change colours); `None` leaves it plain.
    left_gutter_bg: Option<Color>,
    right_gutter_bg: Option<Color>,
    /// The colour each side's connector band leans toward.
    left_accent: Color,
    right_accent: Color,
    link: SbsLink,
}

/// What one column shows on a changed row, which decides its colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellKind {
    /// A new line with nothing removed beside it.
    Added,
    /// A removed line with nothing added beside it.
    Removed,
    /// The old side of a removed line and the line that replaced it.
    ModifiedOld,
    /// The new side of that pair.
    ModifiedNew,
}

/// Colours of one changed cell: its code, the padding after it, its gutter
/// and its change marker.
struct CellPaint {
    /// Row background, laid under syntax colours and the padding.
    bg: Color,
    /// Text without syntax colours, and the padding.
    plain: Style,
    /// Background of the changed words of a modified line.
    word_bg: Color,
    /// Colour the connector band leans toward.
    accent: Color,
    marker: &'static str,
    marker_style: Style,
    /// Line-number style. Carries the row background when the row's colour
    /// runs through the gutter.
    gutter: Style,
    gutter_bg: Option<Color>,
}

/// Colours for a changed cell. Git colours (`ide` off) go by side: the old
/// column red, the new one green, each with a `▌` marker beside a plain
/// gutter. IDE change colours go by what happened to the line: a modified
/// pair takes the theme's third diff colour on both sides, a line that was
/// only added stays green, one that was only removed turns neutral; the row
/// colour runs through the gutter in place of the marker, so a change reads
/// as one band from edge to edge.
fn cell_paint(theme: &Theme, ide: bool, kind: CellKind, syntax: bool) -> CellPaint {
    let new_side = matches!(kind, CellKind::Added | CellKind::ModifiedNew);
    let dim = styles::dim_style(theme);
    if !ide {
        let (bg, plain_bg, accent) = if new_side {
            (theme.syntax_add_bg, theme.diff_add_bg, theme.diff_add)
        } else {
            (theme.syntax_del_bg, theme.diff_del_bg, theme.diff_del)
        };
        let bg = if syntax { bg } else { plain_bg };
        return CellPaint {
            bg,
            plain: Style::default().fg(accent).bg(bg),
            word_bg: theme.intraline_bg(new_side, syntax),
            accent,
            marker: "▌",
            marker_style: Style::default().fg(accent).bg(plain_bg),
            gutter: dim,
            gutter_bg: None,
        };
    }
    let (bg, word_bg, accent) = match kind {
        CellKind::Added => (
            if syntax {
                theme.syntax_add_bg
            } else {
                theme.diff_add_bg
            },
            theme.intraline_bg(true, syntax),
            theme.diff_add,
        ),
        CellKind::Removed => (theme.diff_gone_bg(), theme.diff_gone_bg(), theme.fg_dim),
        CellKind::ModifiedOld | CellKind::ModifiedNew => (
            theme.diff_mod_bg(),
            theme.diff_mod_word_bg(),
            theme.diff_edit,
        ),
    };
    let plain = Style::default().fg(theme.diff_context).bg(bg);
    CellPaint {
        bg,
        plain,
        word_bg,
        accent,
        marker: " ",
        marker_style: plain,
        gutter: dim.bg(bg),
        gutter_bg: Some(bg),
    }
}

type WrappedRows = Vec<Vec<Span<'static>>>;

/// Each side's wrapped rows at its own column width (the divider can sit
/// off-centre). An empty side is one blank row.
fn sbs_wrapped_sides(
    meta: &SbsRowMeta,
    left_width: usize,
    right_width: usize,
) -> (WrappedRows, WrappedRows) {
    let side = |content: &[Span<'static>], width: usize| {
        if content.is_empty() {
            vec![Vec::new()]
        } else {
            wrap_spans(content, width)
        }
    };
    (
        side(&meta.left_content, left_width),
        side(&meta.right_content, right_width),
    )
}

fn sbs_row_height(
    lines: &[Line],
    sbs_meta: &std::collections::HashMap<usize, SbsRowMeta>,
    idx: usize,
    wrap: bool,
    (left_width, right_width): (usize, usize),
    viewport_width: usize,
) -> usize {
    if !wrap || left_width == 0 || right_width == 0 {
        return 1;
    }
    match sbs_meta.get(&idx) {
        Some(meta) => {
            let (left_rows, right_rows) = sbs_wrapped_sides(meta, left_width, right_width);
            left_rows.len().max(right_rows.len())
        }
        None => lines
            .get(idx)
            .map_or(1, |line| wrap_spans(&line.spans, viewport_width).len()),
    }
}

/// A code cell's spans. `paint` is the cell's change colours, `None` for an
/// unchanged line.
fn content_spans_for_diff_line(
    theme: &Theme,
    dl: &DiffLine,
    paint: Option<&CellPaint>,
    search: Option<(&crate::ui::text_utils::SearchPattern, Style)>,
    emphasis: Option<&[std::ops::Range<usize>]>,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = if let Some(ref h) = dl.highlighted_spans {
        h.iter()
            .map(|(s, t)| {
                let style = match paint {
                    Some(paint) => s.bg(paint.bg),
                    None => *s,
                };
                Span::styled(t.clone(), style)
            })
            .collect()
    } else {
        let base = paint.map_or_else(|| styles::diff_context_style(theme), |p| p.plain);
        vec![Span::styled(dl.content.clone(), base)]
    };
    if let (Some(ranges), Some(paint)) = (emphasis, paint) {
        spans = crate::ui::intraline::emphasize(spans, ranges, paint.word_bg);
    }
    match search {
        Some((needle, hl)) => apply_search_highlight_spans(spans, needle, hl),
        None => spans,
    }
}

fn searched_cell_spans(
    pairs: &[(Style, String)],
    width: usize,
    pad_style: Style,
    search: Option<(&crate::ui::text_utils::SearchPattern, Style)>,
) -> Vec<Span<'static>> {
    if let Some((needle, hl)) = search
        && let Some(highlighted) = apply_search_highlight_pairs(pairs, needle, hl)
    {
        return truncate_or_pad_spans(&highlighted, width, pad_style);
    }
    truncate_or_pad_spans(pairs, width, pad_style)
}

fn plain_cell_spans(
    content: &str,
    style: Style,
    width: usize,
    search: Option<(&crate::ui::text_utils::SearchPattern, Style)>,
) -> Vec<Span<'static>> {
    if let Some((needle, hl)) = search
        && let Some(highlighted) = apply_search_highlight_text(content, style, needle, hl)
    {
        return truncate_or_pad_pairs_by_chars(&highlighted, width, style);
    }
    vec![Span::styled(truncate_or_pad(content, width), style)]
}

fn pad_spans_to_width(
    mut spans: Vec<Span<'static>>,
    width: usize,
    pad_style: Style,
) -> Vec<Span<'static>> {
    let cur: usize = spans.iter().map(|s| s.content.width()).sum();
    if cur < width {
        spans.push(Span::styled(" ".repeat(width - cur), pad_style));
    }
    spans
}

fn pan_spans(
    spans: &[Span<'static>],
    scroll_x: usize,
    width: usize,
    pad_style: Style,
) -> Vec<Span<'static>> {
    let mut skipped = 0;
    let mut visible_width = 0;
    let mut visible = Vec::new();
    for span in spans {
        let mut text = String::new();
        for ch in span.content.chars() {
            let ch_width = ch.width().unwrap_or(if ch == '\t' { 1 } else { 0 });
            if skipped < scroll_x {
                skipped += ch_width;
            } else if visible_width + ch_width <= width {
                text.push(ch);
                visible_width += ch_width;
            } else {
                if !text.is_empty() {
                    visible.push(Span::styled(text, span.style));
                }
                return pad_spans_to_width(visible, width, pad_style);
            }
        }
        if !text.is_empty() {
            visible.push(Span::styled(text, span.style));
        }
        if visible_width == width {
            break;
        }
    }
    pad_spans_to_width(visible, width, pad_style)
}

fn pan_sbs_row(
    meta: &SbsRowMeta,
    scroll_x: usize,
    left_width: usize,
    right_width: usize,
) -> Line<'static> {
    let mut spans = meta.left_prefix.clone();
    spans.extend(pan_spans(
        &meta.left_content,
        scroll_x,
        left_width,
        meta.left_pad_style,
    ));
    spans.extend(meta.right_prefix.clone());
    spans.extend(pan_spans(
        &meta.right_content,
        scroll_x,
        right_width,
        meta.right_pad_style,
    ));
    Line::from(spans)
}

/// Repaint the new side of a row for a line changed in `$EDITOR`. The
/// metadata is enough: code rows are always drawn from it.
fn mark_local_edit(meta: &mut SbsRowMeta, mark: Option<LocalEditMark>, theme: &Theme, ide: bool) {
    let Some(mark) = mark else {
        return;
    };
    if let Some(marker) = meta.right_prefix.last_mut() {
        apply_local_edit_mark(marker, &mut meta.right_content, mark, theme);
    }
    if mark == LocalEditMark::Edited {
        meta.right_pad_style = styles::diff_edit_span_style(meta.right_pad_style, theme);
        // With the row colour running through the gutter, the edit tint
        // does too: the number sits between the divider and the marker.
        if ide {
            meta.right_gutter_bg = Some(theme.diff_edit_bg);
            let number = meta.right_prefix.len().checked_sub(2);
            if let Some(number) = number.and_then(|i| meta.right_prefix.get_mut(i)) {
                number.style = number.style.bg(theme.diff_edit_bg);
            }
        }
    }
}

struct SideSpec {
    lineno: Option<u32>,
    /// Blame label to overlay on the number's blank lead (new side only).
    blame: Option<String>,
    marker: &'static str,
    marker_style: Style,
    /// Line-number style: dim, over the row colour when it runs through
    /// the gutter.
    gutter: Style,
}

impl SideSpec {
    /// A side with no line on this row.
    fn blank(theme: &Theme) -> Self {
        Self {
            lineno: None,
            blame: None,
            marker: " ",
            marker_style: Style::default(),
            gutter: styles::dim_style(theme),
        }
    }
}

fn sbs_row_prefixes(
    theme: &Theme,
    indicator: &'static str,
    left: SideSpec,
    right: SideSpec,
    lw: usize,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let dim = styles::dim_style(theme);
    let old_num = left
        .lineno
        .map(|n| format!("{n:>lw$}"))
        .unwrap_or_else(|| " ".repeat(lw));
    let new_num = right
        .lineno
        .map(|n| format!("{n:>lw$}"))
        .unwrap_or_else(|| " ".repeat(lw));
    let new_num =
        crate::app::overlay_label(new_num, right.blame.as_deref(), crate::app::BLAME_WIDTH - 1);

    let left_prefix = vec![
        Span::styled(indicator, styles::current_line_indicator_style(theme)),
        Span::styled(format!("{old_num} "), left.gutter),
        Span::styled(left.marker.to_string(), left.marker_style),
    ];
    let right_prefix = vec![
        Span::styled(" │ ", dim),
        Span::styled(format!("{new_num} "), right.gutter),
        Span::styled(right.marker.to_string(), right.marker_style),
    ];
    (left_prefix, right_prefix)
}

/// Move the `▶` caret onto the active side for the cursor row. The per-line
/// builders always place the caret in the far-left slot (old side). When the
/// cursor's effective side is `New`, blank that slot and redraw the caret in
/// the divider's trailing space, just left of the new line number, so it
/// points at the side a comment would attach to. Run as a post-render overlay
/// so it's independent of the wrapped/unwrapped row-build paths.
fn paint_sbs_active_side_caret(
    frame: &mut Frame,
    inner: Rect,
    app: &App,
    lw: usize,
    content_width: usize,
    row_heights: &[usize],
) {
    // Only the New side needs moving; Old keeps the left-slot caret.
    if !matches!(app.get_line_at_cursor(), Some((_, LineSide::New))) {
        return;
    }
    let scroll_offset = app.diff_state.scroll_offset;
    let cursor_line = app.diff_state.cursor_line;
    if cursor_line < scroll_offset {
        return;
    }
    let logical_offset = cursor_line - scroll_offset;
    if logical_offset >= app.diff_state.visible_line_count.max(1) {
        return;
    }
    // First visual row of the cursor's logical line (wrap-aware).
    let visual_row: u16 = if app.diff_state.wrap_lines {
        (0..logical_offset)
            .map(|i| row_heights.get(i).copied().unwrap_or(1) as u16)
            .sum()
    } else {
        logical_offset as u16
    };
    if visual_row >= inner.height {
        return;
    }
    let y = inner.y + visual_row;
    let style = styles::current_line_indicator_style(&app.theme);

    // Blank the far-left caret slot.
    frame.buffer_mut()[(inner.x, y)].set_char(' ');

    // Caret goes in the divider's trailing space: after the left gutter
    // (`sbs_left_gutter`) and the left content column, `" │ "` occupies three
    // cells and the third (index +2) is the space just left of the new lineno.
    let caret_x = inner.x + crate::app::sbs_left_gutter(lw) + content_width as u16 + 2;
    if caret_x < inner.x + inner.width {
        let cell = &mut frame.buffer_mut()[(caret_x, y)];
        cell.set_char('▶');
        if let Some(fg) = style.fg {
            cell.set_fg(fg);
        }
    }
}

/// Mark clipped content while panning without wrap: `‹` in a column's
/// change-marker slot (just left of the code, so no text is hidden) when
/// text is scrolled off its left edge, `›` on its last cell when the line
/// runs past the right edge. An IDE shows a
/// scrollbar; a TUI needs the cue on the row itself.
#[allow(clippy::too_many_arguments)]
fn paint_sbs_clip_markers(
    frame: &mut Frame,
    inner: Rect,
    theme: &Theme,
    sbs_meta: &std::collections::HashMap<usize, SbsRowMeta>,
    scroll_offset: usize,
    scroll_x: usize,
    lw: usize,
    widths: (usize, usize),
) {
    let (left_w, right_w) = widths;
    if left_w < 2 || right_w < 2 {
        return;
    }
    let left_x = inner.x + crate::app::sbs_left_gutter(lw);
    let right_x = inner.x + crate::app::sbs_overhead(lw) + left_w as u16;
    let fg = theme.diff_hunk_header;
    let mut mark = |x: u16, y: u16, glyph: char| {
        if x < inner.x + inner.width {
            let cell = &mut frame.buffer_mut()[(x, y)];
            // The `‹` goes on the `▌` change-marker slot: keep a changed
            // line's red/green so it still reads as a deletion/addition.
            if cell.symbol() != "▌" {
                cell.set_fg(fg);
            }
            cell.set_char(glyph);
        }
    };
    for row in 0..inner.height {
        let Some(meta) = sbs_meta.get(&(scroll_offset + row as usize)) else {
            continue;
        };
        let y = inner.y + row;
        for (content, x, w) in [
            (&meta.left_content, left_x, left_w),
            (&meta.right_content, right_x, right_w),
        ] {
            let len: usize = content.iter().map(|s| s.content.width()).sum();
            if scroll_x > 0 && len > 0 {
                mark(x - 1, y, '‹');
            }
            if len > scroll_x + w {
                mark(x + w as u16 - 1, y, '›');
            }
        }
    }
}

/// Join each change block across the divider, the way an IDE's diff gutter
/// does. A removed line and its replacement share a band: the old side's
/// colour up to the centre, the new side's from there on. Where one side has
/// no line, the other's colour stops at the centre, so the blank rows read as
/// padding and not as blank lines.
///
/// The first of those blank rows is where the block folds. There the longer
/// side's band steps across the divider and thins into a rule along the top
/// of the blank row, as wide as the shorter side: the gap between two of its
/// lines that the rest of the block collapses into.
///
/// Only block elements are used, which terminals draw edge to edge.
#[allow(clippy::too_many_arguments)]
fn paint_sbs_connectors(
    frame: &mut Frame,
    inner: Rect,
    theme: &Theme,
    sbs_meta: &std::collections::HashMap<usize, SbsRowMeta>,
    scroll_offset: usize,
    row_heights: &[usize],
    lw: usize,
    (left_width, right_width): (usize, usize),
) {
    let right_edge = inner.x + inner.width;
    // The three divider cells: `x`, the centre `x + 1`, and `x + 2`.
    let x = inner.x + crate::app::sbs_left_gutter(lw) + left_width as u16;
    if x + 2 >= right_edge {
        return;
    }
    // A rule spans its side's gutter and column. The old side's leaves the
    // cursor-indicator cell alone.
    let old_rule = (inner.x + 1)..x;
    let new_side_end = inner.x + crate::app::sbs_overhead(lw) + (left_width + right_width) as u16;
    let new_rule = (x + 3)..new_side_end.min(right_edge);

    let bottom = inner.y + inner.height;
    let buf = frame.buffer_mut();
    let mut y = inner.y;
    for (i, &height) in row_heights.iter().enumerate() {
        let rows = y..(y + height as u16).min(bottom);
        y = rows.end;
        let Some(meta) = sbs_meta.get(&(scroll_offset + i)) else {
            continue;
        };
        // Each side's colour follows its own row background, so a line tinted
        // as a local edit gets a band to match.
        let band = |pad: Style, accent: Color| match pad.bg {
            Some(bg) => theme.sbs_link_color_toward(bg, accent),
            None => accent,
        };
        let old = band(meta.left_pad_style, meta.left_accent);
        let new = band(meta.right_pad_style, meta.right_accent);
        for (k, y) in rows.enumerate() {
            // A wrapped line folds on its first row only.
            let first_row = k == 0;
            match meta.link {
                SbsLink::None => {}
                SbsLink::Both => {
                    buf[(x, y)].set_bg(old);
                    buf[(x + 1, y)].set_char('▌').set_fg(old).set_bg(new);
                    buf[(x + 2, y)].set_bg(new);
                }
                SbsLink::OldOnly { fold: true } if first_row => {
                    buf[(x, y)].set_bg(old);
                    buf[(x + 1, y)].set_char('▛').set_fg(old);
                    buf[(x + 2, y)].set_char('▀').set_fg(old);
                    for rule_x in new_rule.clone() {
                        buf[(rule_x, y)].set_char('▔').set_fg(old);
                    }
                }
                SbsLink::OldOnly { .. } => {
                    buf[(x, y)].set_bg(old);
                    buf[(x + 1, y)].set_char('▌').set_fg(old);
                }
                SbsLink::NewOnly { fold: true } if first_row => {
                    for rule_x in old_rule.clone() {
                        buf[(rule_x, y)].set_char('▔').set_fg(new);
                    }
                    buf[(x, y)].set_char('▀').set_fg(new);
                    buf[(x + 1, y)].set_char('▜').set_fg(new);
                    buf[(x + 2, y)].set_bg(new);
                }
                SbsLink::NewOnly { .. } => {
                    buf[(x + 1, y)].set_char('▐').set_fg(new);
                    buf[(x + 2, y)].set_bg(new);
                }
            }
        }
    }
}

/// Put each side's row colour back in its gutter where the cursor-line tint
/// painted over it. The IDE change colours have no `▌` marker to say what
/// happened to a line, so on the cursor row the gutters keep saying it while
/// the code between them takes the tint. Rows whose gutter has no colour of
/// its own (unchanged lines, colours by side) are left as they are.
#[allow(clippy::too_many_arguments)]
fn repaint_sbs_cursor_row_gutters(
    frame: &mut Frame,
    inner: Rect,
    theme: &Theme,
    sbs_meta: &std::collections::HashMap<usize, SbsRowMeta>,
    scroll_offset: usize,
    row_heights: &[usize],
    lw: usize,
    left_width: usize,
) {
    let right_edge = inner.x + inner.width;
    let divider = inner.x + crate::app::sbs_left_gutter(lw) + left_width as u16;
    // Line number, the space after it, and the marker cell.
    let gutter = lw as u16 + 2;
    let old_gutter = (inner.x + 1).min(right_edge)..(inner.x + 1 + gutter).min(right_edge);
    let new_gutter = (divider + 3).min(right_edge)..(divider + 3 + gutter).min(right_edge);

    let bottom = inner.y + inner.height;
    let buf = frame.buffer_mut();
    let mut y = inner.y;
    for (i, &height) in row_heights.iter().enumerate() {
        let rows = y..(y + height as u16).min(bottom);
        y = rows.end;
        let Some(meta) = sbs_meta.get(&(scroll_offset + i)) else {
            continue;
        };
        for y in rows {
            for (cells, bg) in [
                (&old_gutter, meta.left_gutter_bg),
                (&new_gutter, meta.right_gutter_bg),
            ] {
                let Some(bg) = bg else {
                    continue;
                };
                for x in cells.clone() {
                    let cell = &mut buf[(x, y)];
                    if cell.bg == theme.cursor_line_bg {
                        cell.set_bg(bg);
                    }
                }
            }
        }
    }
}

/// Continuation-row prefixes of a wrapped line: blank in place of the line
/// numbers (same width, so columns stay aligned) with the center divider
/// preserved. A gutter the row colour runs through keeps that colour.
fn sbs_blank_prefixes(
    theme: &Theme,
    lw: usize,
    meta: &SbsRowMeta,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let dim = styles::dim_style(theme);
    let gutter = |bg: Option<Color>| {
        let style = bg.map_or_else(Style::default, |bg| Style::default().bg(bg));
        Span::styled(" ".repeat(lw + 2), style)
    };
    let left = vec![Span::raw(" "), gutter(meta.left_gutter_bg)];
    let right = vec![Span::styled(" │ ", dim), gutter(meta.right_gutter_bg)];
    (left, right)
}

/// Cursor info for the inline comment input box in side-by-side view:
/// (cursor_logical_line, cursor_column, box_start_line, box_end_line)
type SideBySideCursorInfo = (usize, u16, usize, usize, usize);

/// Context for rendering side-by-side diff lines
struct SideBySideContext<'a> {
    app: &'a App,
    theme: &'a Theme,
    /// Old (left) column content width.
    left_width: usize,
    /// New (right) column content width.
    right_width: usize,
    panel_width: usize,
    current_line_idx: usize,
    lineno_width: usize,
    // Comment input state for inline editing
    comment_input_mode: bool,
    comment_line: Option<(u32, LineSide)>,
    comment_type: crate::model::CommentType,
    comment_buffer: &'a str,
    comment_cursor: usize,
    comment_line_range: Option<LineRange>,
    editing_comment_id: Option<&'a str>,
    current_file_idx: usize,
    // RefCell so deeply-nested rendering helpers can push without each
    // intermediate function needing a `&mut Vec` parameter threaded through.
    comment_bars: std::cell::RefCell<Vec<crate::ui::diff_view::CommentBarAnchor>>,
    sbs_meta: std::cell::RefCell<std::collections::HashMap<usize, SbsRowMeta>>,
    // Only fully build spans for diff lines whose `line_idx` falls in this
    // half-open range; off-screen rows push `Line::default()` placeholders.
    visible_start: usize,
    visible_end: usize,
    search_style: Style,
}

impl SideBySideContext<'_> {
    fn is_visible(&self, line_idx: usize) -> bool {
        line_idx >= self.visible_start && line_idx < self.visible_end
    }

    /// Same question for a multi-row comment box: does any of it land on screen?
    fn box_visible(&self, top: usize, rows: usize) -> bool {
        crate::ui::diff_view::comment_box_visible(top, rows, (self.visible_start, self.visible_end))
    }

    fn search_for(
        &self,
        line_idx: usize,
    ) -> Option<(&crate::ui::text_utils::SearchPattern, Style)> {
        let needle = self.app.search_paint_at(line_idx)?;
        Some((needle, self.search_style))
    }

    /// Whether changes take the IDE change colours (`:set idecolors`).
    fn ide(&self) -> bool {
        self.app.ide_change_colors
    }

    fn cell_paint(&self, kind: CellKind, line: &DiffLine) -> CellPaint {
        cell_paint(
            self.theme,
            self.ide(),
            kind,
            line.highlighted_spans.is_some(),
        )
    }

    fn display_lineno(&self, source_line: Option<u32>, line_idx: usize) -> Option<u32> {
        source_line.map(|line| {
            if self.app.relative_line_numbers {
                crate::ui::diff_view::relative_line_number(line, line_idx, self.current_line_idx)
            } else {
                line
            }
        })
    }
}

pub(super) fn render_side_by_side_diff(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focused_panel == FocusedPanel::Diff;

    // When the diff is the only pane, drop the frame entirely — including its
    // title row. The file name and stats are folded into the top header line
    // instead (see status_bar::render_header) so there's a single header line.
    let sole = app.is_diff_sole_pane();
    let mut block = Block::default()
        .borders(if sole { Borders::NONE } else { Borders::ALL })
        .style(styles::panel_style(&app.theme))
        .border_style(styles::border_style(&app.theme, focused));
    if !sole {
        block = block
            .title(crate::ui::diff_view::diff_title(app, area.width))
            .title_top(diff_stat_title(app).right_aligned());
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Update viewport height for scroll calculations
    app.diff_state.viewport_height = inner.height as usize;
    app.diff_inner_area = Some(inner);

    // Reset comment input annotation offset (will be set if a comment input box is rendered)
    app.comment_input_annotation_offset = None;

    let lw = app.lineno_width();
    let (left_width, right_width) = app.sbs_column_widths(inner.width);

    // Determine if we're in line comment mode (not file-level)
    let comment_input_mode = app.input_mode == InputMode::Comment
        && !app.comment_is_file_level
        && !app.comment_is_review_level;

    let (visible_start, visible_end) = crate::ui::diff_view::diff_visible_range(app, inner);

    let ctx = SideBySideContext {
        app,
        theme: &app.theme,
        left_width,
        right_width,
        panel_width: inner.width as usize,
        current_line_idx: app.diff_state.cursor_line,
        lineno_width: lw,
        comment_input_mode,
        comment_line: app.comment_line,
        comment_type: app.comment_type.clone(),
        comment_buffer: &app.comment_buffer,
        comment_cursor: app.comment_cursor,
        comment_line_range: app.comment_line_range.map(|(r, _)| r),
        editing_comment_id: app.editing_comment_id.as_deref(),
        current_file_idx: app.diff_state.current_file_idx,
        comment_bars: std::cell::RefCell::new(Vec::new()),
        sbs_meta: std::cell::RefCell::new(std::collections::HashMap::new()),
        visible_start,
        visible_end,
        search_style: styles::search_match_style(&app.theme),
    };

    // Build all diff lines for side-by-side view
    let mut lines: Vec<Line> = Vec::new();
    let mut line_idx: usize = 0;

    // Track cursor position for IME when in Comment mode
    let mut comment_cursor_logical_line: Option<usize> = None;
    let mut comment_cursor_column: u16 = 0;
    // Track the full extent of the comment input box so we can auto-scroll
    // the viewport to keep it visible while the user types.
    let mut comment_input_box_range: Option<(usize, usize)> = None;
    let mut annotation_offset: Option<(usize, usize, usize)> = None;

    let is_review_comment_mode =
        app.input_mode == InputMode::Comment && app.comment_is_review_level;

    crate::ui::pr_info_panel::append_pr_info_section(
        app,
        &mut lines,
        &mut line_idx,
        ctx.current_line_idx,
    );

    // The `═══ Review Comments ═══` label is redundant in single-file
    // view -- see the matching guard in `src/ui/diff_unified.rs`.
    if app.show_review_comments_header() {
        let general_indicator = cursor_indicator_spaced(line_idx, ctx.current_line_idx);
        lines.push(Line::from(vec![
            Span::styled(
                general_indicator,
                styles::current_line_indicator_style(&app.theme),
            ),
            Span::styled(
                crate::ui::diff_view::REVIEW_COMMENTS_HEADER_PREFIX,
                styles::file_header_style(&app.theme),
            ),
            Span::styled(
                crate::ui::diff_view::HEADER_RULE,
                styles::file_header_style(&app.theme),
            ),
        ]));
        line_idx += 1;
    }

    for summary in &app.forge_review_summaries {
        let summary_lines = comment_panel::format_remote_review_summary_lines(
            &app.theme,
            summary,
            app.forge_kind(),
        );
        for mut summary_line in summary_lines {
            let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
            summary_line.spans.insert(
                0,
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
            );
            lines.push(summary_line);
            line_idx += 1;
        }
    }

    for comment in &app.session.review_comments {
        let is_being_edited =
            app.editing_comment_id.as_ref() == Some(&comment.id) && is_review_comment_mode;

        if is_being_edited {
            let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
                &app.theme,
                comment_type_presentation(app, &app.comment_type),
                &app.comment_buffer,
                app.comment_cursor,
                None,
                true,
                ctx.panel_width.saturating_sub(1),
                app.comment_vim_mode_label()
                    .as_ref()
                    .map(|(t, w)| (t.as_str(), *w)),
                app.supports_keyboard_enhancement,
            );
            comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
            comment_cursor_column = 1 + cursor_info.column;
            comment_input_box_range =
                Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
            let annotations_replaced = App::comment_display_lines(comment, inner.width as usize);
            annotation_offset = Some((line_idx, input_lines.len(), annotations_replaced));

            for mut input_line in input_lines {
                let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
                input_line.spans.insert(
                    0,
                    Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                );
                lines.push(input_line);
                line_idx += 1;
            }
        } else {
            let rows = App::comment_display_lines(comment, ctx.panel_width);
            if !ctx.box_visible(line_idx, rows) {
                skip_comment_box(&mut lines, &mut line_idx, rows);
                continue;
            }
            let comment_lines = comment_panel::format_comment_lines(
                &app.theme,
                comment_type_presentation(app, &comment.comment_type),
                &comment.content,
                None,
                ctx.panel_width.saturating_sub(1),
                (comment.author != app.username).then_some(comment.author.as_str()),
            );
            for mut comment_line in comment_lines {
                let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
                comment_line.spans.insert(
                    0,
                    Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                );
                lines.push(comment_line);
                line_idx += 1;
            }
        }
    }

    // Render remote review-level threads (general MR notes, line: None).
    {
        use crate::forge::remote_comments::{PrCommentsVisibility, RemoteCommentSide};
        let _ = RemoteCommentSide::Right; // ensure import is used
        let visibility = app.session.remote_comments_visibility;
        if !matches!(visibility, PrCommentsVisibility::Hide) {
            for thread in &app.forge_review_threads {
                if thread.line.is_some() {
                    continue; // inline threads are rendered in-diff
                }
                let Some(muted) = visibility.render_decision(thread) else {
                    continue;
                };
                let thread_lines = comment_panel::format_remote_thread_lines(
                    &app.theme,
                    thread,
                    muted,
                    app.forge_kind(),
                );
                for mut comment_line in thread_lines {
                    let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
                    comment_line.spans.insert(
                        0,
                        Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                    );
                    lines.push(comment_line);
                    line_idx += 1;
                }
            }
        }
    }

    if is_review_comment_mode && app.editing_comment_id.is_none() {
        let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
            &app.theme,
            comment_type_presentation(app, &app.comment_type),
            &app.comment_buffer,
            app.comment_cursor,
            None,
            false,
            ctx.panel_width.saturating_sub(1),
            app.comment_vim_mode_label()
                .as_ref()
                .map(|(t, w)| (t.as_str(), *w)),
            app.supports_keyboard_enhancement,
        );
        comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
        comment_cursor_column = 1 + cursor_info.column;
        comment_input_box_range = Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
        annotation_offset = Some((line_idx, input_lines.len(), 0));

        for mut input_line in input_lines {
            let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
            input_line.spans.insert(
                0,
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
            );
            lines.push(input_line);
            line_idx += 1;
        }
    }

    crate::ui::pr_info_panel::append_issue_comments_section(
        app,
        &mut lines,
        &mut line_idx,
        ctx.current_line_idx,
        ctx.panel_width.saturating_sub(1),
        (ctx.visible_start, ctx.visible_end),
    );

    for (file_idx, file) in app.diff_files.iter().enumerate() {
        // Single-file view: hide everything except the cursor's file. See
        // src/ui/diff_unified.rs for the matching guard.
        if app.is_single_file_view && file_idx != app.diff_state.current_file_idx {
            continue;
        }
        // See the matching filter guard in src/ui/diff_unified.rs.
        if !app.file_passes_filter(file) {
            continue;
        }
        let path = file.display_path();
        let is_reviewed = app.session.is_file_reviewed(path);

        if !app.is_single_file_view {
            let indicator = cursor_indicator_spaced(line_idx, ctx.current_line_idx);
            let header_text = crate::ui::diff_view::file_header_prefix_text(app, file);
            lines.push(Line::from(vec![
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                Span::styled(header_text, styles::file_header_style(&app.theme)),
                Span::styled(
                    crate::ui::diff_view::HEADER_RULE,
                    styles::file_header_style(&app.theme),
                ),
            ]));
            line_idx += 1;
        }

        // Reviewed files normally collapse in continuous view. A summary jump
        // may reveal one target body without changing its reviewed marker.
        if app.should_collapse_file(file_idx) {
            continue;
        }
        if is_reviewed && app.is_single_file_view {
            let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
            lines.push(Line::from(vec![
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                Span::styled(
                    crate::ui::diff_view::REVIEWED_BANNER_TEXT,
                    Style::default()
                        .fg(app.theme.fg_secondary)
                        .add_modifier(Modifier::DIM),
                ),
            ]));
            line_idx += 1;
        }

        // Check if we're editing/adding a file-level comment for this file
        let is_file_comment_mode = app.input_mode == InputMode::Comment
            && app.comment_is_file_level
            && file_idx == app.diff_state.current_file_idx;

        // Show file-level comments
        if let Some(review) = app.session.files.get(path) {
            for comment in &review.file_comments {
                if !app.comment_visible(comment) {
                    continue;
                }
                // Skip rendering this comment if it's being edited
                let is_being_edited =
                    app.editing_comment_id.as_ref() == Some(&comment.id) && is_file_comment_mode;

                if is_being_edited {
                    // Render the inline input instead
                    let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
                        &app.theme,
                        comment_type_presentation(app, &app.comment_type),
                        &app.comment_buffer,
                        app.comment_cursor,
                        None,
                        true,
                        ctx.panel_width.saturating_sub(1),
                        app.comment_vim_mode_label()
                            .as_ref()
                            .map(|(t, w)| (t.as_str(), *w)),
                        app.supports_keyboard_enhancement,
                    );
                    comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
                    comment_cursor_column = 1 + cursor_info.column;
                    comment_input_box_range =
                        Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
                    let annotations_replaced =
                        App::comment_display_lines(comment, inner.width as usize);
                    annotation_offset = Some((line_idx, input_lines.len(), annotations_replaced));

                    for mut input_line in input_lines {
                        let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
                        input_line.spans.insert(
                            0,
                            Span::styled(
                                indicator,
                                styles::current_line_indicator_style(&app.theme),
                            ),
                        );
                        lines.push(input_line);
                        line_idx += 1;
                    }
                } else {
                    let rows = App::comment_display_lines(comment, ctx.panel_width);
                    if !ctx.box_visible(line_idx, rows) {
                        skip_comment_box(&mut lines, &mut line_idx, rows);
                        continue;
                    }
                    let comment_lines = comment_panel::format_comment_lines(
                        &app.theme,
                        comment_type_presentation(app, &comment.comment_type),
                        &comment.content,
                        None,
                        ctx.panel_width.saturating_sub(1),
                        (comment.author != app.username).then_some(comment.author.as_str()),
                    );
                    for mut comment_line in comment_lines {
                        let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
                        comment_line.spans.insert(
                            0,
                            Span::styled(
                                indicator,
                                styles::current_line_indicator_style(&app.theme),
                            ),
                        );
                        lines.push(comment_line);
                        line_idx += 1;
                    }
                }
            }
        }

        // Render inline input for new file-level comment
        if is_file_comment_mode && app.editing_comment_id.is_none() {
            let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
                &app.theme,
                comment_type_presentation(app, &app.comment_type),
                &app.comment_buffer,
                app.comment_cursor,
                None,
                false,
                ctx.panel_width.saturating_sub(1),
                app.comment_vim_mode_label()
                    .as_ref()
                    .map(|(t, w)| (t.as_str(), *w)),
                app.supports_keyboard_enhancement,
            );
            comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
            comment_cursor_column = 1 + cursor_info.column;
            comment_input_box_range =
                Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
            annotation_offset = Some((line_idx, input_lines.len(), 0));

            for mut input_line in input_lines {
                let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
                input_line.spans.insert(
                    0,
                    Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                );
                lines.push(input_line);
                line_idx += 1;
            }
        }

        if file.is_too_large || file.is_binary || file.hunks.is_empty() {
            let indicator = cursor_indicator_spaced(line_idx, ctx.current_line_idx);
            lines.push(Line::from(vec![
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                Span::styled(
                    crate::ui::diff_view::binary_or_empty_label(file),
                    styles::dim_style(&app.theme),
                ),
            ]));
            line_idx += 1;
        } else {
            let line_comments = app
                .session
                .files
                .get(path)
                .map(|r| &r.line_comments)
                .unwrap_or(&crate::ui::diff_view::EMPTY_LINE_COMMENTS);

            for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
                // Calculate and render gap before this hunk
                let prev_hunk = if hunk_idx > 0 {
                    file.hunks.get(hunk_idx - 1)
                } else {
                    None
                };
                let gap = calculate_gap(
                    prev_hunk.map(|h| (&h.new_start, &h.new_count)),
                    hunk.new_start,
                );

                let gap_id = GapId { file_idx, hunk_idx };

                if gap > 0 && app.should_render_gap_before_hunk(file_idx, hunk_idx) {
                    let top_lines = app.expanded_top.get(&gap_id);
                    let bot_lines = app.expanded_bottom.get(&gap_id);
                    let top_len = top_lines.map_or(0, |v| v.len());
                    let bot_len = bot_lines.map_or(0, |v| v.len());
                    let remaining = (gap as usize).saturating_sub(top_len + bot_len);
                    let is_top_of_file = hunk_idx == 0;

                    // Render top expanded lines
                    if let Some(top) = top_lines {
                        for expanded_line in top {
                            if !ctx.is_visible(line_idx) {
                                lines.push(Line::default());
                                line_idx += 1;
                                continue;
                            }
                            render_sbs_expanded_context_line(
                                &mut lines,
                                &mut line_idx,
                                expanded_line,
                                file_idx,
                                &ctx,
                            );
                        }
                    }

                    // Render expanders / hidden lines
                    if remaining > 0 {
                        if is_top_of_file {
                            if remaining > GAP_EXPAND_BATCH {
                                render_hidden_lines(
                                    &mut lines,
                                    &mut line_idx,
                                    ctx.current_line_idx,
                                    remaining,
                                    &app.theme,
                                );
                            }
                            render_expander_line(
                                &mut lines,
                                &mut line_idx,
                                ctx.current_line_idx,
                                ExpandDirection::Up,
                                remaining,
                                &app.theme,
                            );
                        } else if remaining >= GAP_EXPAND_BATCH {
                            render_expander_line(
                                &mut lines,
                                &mut line_idx,
                                ctx.current_line_idx,
                                ExpandDirection::Down,
                                remaining,
                                &app.theme,
                            );
                            render_hidden_lines(
                                &mut lines,
                                &mut line_idx,
                                ctx.current_line_idx,
                                remaining,
                                &app.theme,
                            );
                            render_expander_line(
                                &mut lines,
                                &mut line_idx,
                                ctx.current_line_idx,
                                ExpandDirection::Up,
                                remaining,
                                &app.theme,
                            );
                        } else {
                            render_expander_line(
                                &mut lines,
                                &mut line_idx,
                                ctx.current_line_idx,
                                ExpandDirection::Both,
                                remaining,
                                &app.theme,
                            );
                        }
                    }

                    // Render bottom expanded lines
                    if let Some(bot) = bot_lines {
                        for expanded_line in bot {
                            if !ctx.is_visible(line_idx) {
                                lines.push(Line::default());
                                line_idx += 1;
                                continue;
                            }
                            render_sbs_expanded_context_line(
                                &mut lines,
                                &mut line_idx,
                                expanded_line,
                                file_idx,
                                &ctx,
                            );
                        }
                    }
                }

                // Hunk header
                let is_hunk_reviewed = app.is_hunk_reviewed(file_idx, hunk_idx);
                let (hunk_header_text, hunk_header_style) =
                    hunk_header_text_and_style(&app.theme, hunk, is_hunk_reviewed);
                let indicator = cursor_indicator_spaced(line_idx, ctx.current_line_idx);
                lines.push(Line::from(vec![
                    Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                    Span::styled(hunk_header_text, hunk_header_style),
                ]));
                line_idx += 1;
                if app.should_collapse_hunk(file_idx, hunk_idx) {
                    continue;
                }

                // Process diff lines in side-by-side format
                let (new_line_idx, cursor_info) = render_hunk_lines_side_by_side(
                    &hunk.lines,
                    line_comments,
                    &ctx,
                    (file_idx, hunk_idx),
                    line_idx,
                    &mut lines,
                );
                line_idx = new_line_idx;
                if let Some((line, col, box_start, box_end, annotations_replaced)) = cursor_info {
                    comment_cursor_logical_line = Some(line);
                    comment_cursor_column = col;
                    comment_input_box_range = Some((box_start, box_end));
                    let box_len = box_end - box_start + 1;
                    annotation_offset = Some((box_start, box_len, annotations_replaced));
                }
            }
        }

        // End-of-file gap (after all hunks, not for deleted files)
        if file.status != FileStatus::Deleted
            && matches!(
                app.diff_source,
                DiffSource::WorkingTree
                    | DiffSource::Unstaged
                    | DiffSource::StagedAndUnstaged
                    | DiffSource::StagedUnstagedAndCommits(_)
                    | DiffSource::CommitRange(_)
                    | DiffSource::PullRequest(_)
            )
            && let Some(last_hunk) = file.hunks.last()
        {
            let eof_start = last_hunk.new_start + last_hunk.new_count;
            if let Some(&total) = app.file_line_count_cache.get(&file_idx)
                && eof_start <= total
            {
                let gap = (total - eof_start + 1) as usize;
                let eof_gap_id = GapId {
                    file_idx,
                    hunk_idx: file.hunks.len(),
                };
                let top_lines = app.expanded_top.get(&eof_gap_id);
                let bot_lines = app.expanded_bottom.get(&eof_gap_id);
                let top_len = top_lines.map_or(0, |v| v.len());
                let bot_len = bot_lines.map_or(0, |v| v.len());
                let remaining = gap.saturating_sub(top_len + bot_len);

                // Render top expanded lines (↓ direction)
                if let Some(top) = top_lines {
                    for expanded_line in top {
                        render_sbs_expanded_context_line(
                            &mut lines,
                            &mut line_idx,
                            expanded_line,
                            file_idx,
                            &ctx,
                        );
                    }
                }

                // Expander / hidden lines
                if remaining > 0 {
                    render_expander_line(
                        &mut lines,
                        &mut line_idx,
                        ctx.current_line_idx,
                        ExpandDirection::Down,
                        remaining,
                        &app.theme,
                    );
                    if remaining > GAP_EXPAND_BATCH {
                        render_hidden_lines(
                            &mut lines,
                            &mut line_idx,
                            ctx.current_line_idx,
                            remaining,
                            &app.theme,
                        );
                    }
                }

                // Render bottom expanded lines
                if let Some(bot) = bot_lines {
                    for expanded_line in bot {
                        render_sbs_expanded_context_line(
                            &mut lines,
                            &mut line_idx,
                            expanded_line,
                            file_idx,
                            &ctx,
                        );
                    }
                }
            }
        }

        // Spacing between files
        let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
        lines.push(Line::from(Span::styled(
            indicator,
            styles::current_line_indicator_style(&app.theme),
        )));
        line_idx += 1;
    }

    let comment_bars = {
        let mut bars = ctx.comment_bars.borrow_mut();
        std::mem::take(&mut *bars)
    };
    let sbs_meta = {
        let mut m = ctx.sbs_meta.borrow_mut();
        std::mem::take(&mut *m)
    };
    drop(ctx);
    app.comment_input_annotation_offset = annotation_offset;

    // Auto-scroll so the comment input box stays visible while the user types.
    let wrap = app.diff_state.wrap_lines;
    let viewport_width = inner.width as usize;
    scroll_comment_input_into_view(
        &mut app.diff_state.scroll_offset,
        comment_input_box_range,
        comment_cursor_logical_line,
        inner.height as usize,
        lines.len(),
        |idx| {
            sbs_row_height(
                &lines,
                &sbs_meta,
                idx,
                wrap,
                (left_width, right_width),
                viewport_width,
            )
        },
    );

    let visible_lines_unscrolled: Vec<Line> = lines
        .into_iter()
        .skip(app.diff_state.scroll_offset)
        .take(inner.height as usize)
        .collect();

    // Calculate the width of each line for max_content_width and visible line count
    let line_widths: Vec<usize> = visible_lines_unscrolled
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.width())
                .sum::<usize>()
        })
        .collect();

    let max_content_width = sbs_meta
        .values()
        .flat_map(|meta| [&meta.left_content, &meta.right_content])
        .map(|spans| spans.iter().map(|span| span.content.width()).sum::<usize>())
        .max()
        .unwrap_or(0);

    app.sync_viewport_width(inner.width as usize);
    app.diff_state.max_content_width = max_content_width;

    let scroll_offset = app.diff_state.scroll_offset;
    let visible_lines_unscrolled_for_overlay = visible_lines_unscrolled.clone();
    // Single pass: wrap each logical line once, producing both the visual
    // rows to render and the per-line height used by every row-mapping
    // consumer below, so the two can't disagree.
    let (row_heights, wrapped_lines): (Vec<usize>, Option<Vec<Line>>) = if wrap
        && left_width > 0
        && right_width > 0
    {
        let mut heights = Vec::with_capacity(visible_lines_unscrolled_for_overlay.len());
        let mut out: Vec<Line> = Vec::new();
        for (i, line) in visible_lines_unscrolled_for_overlay.iter().enumerate() {
            let logical_idx = scroll_offset + i;
            match sbs_meta.get(&logical_idx) {
                Some(m) => {
                    let (left_rows, right_rows) = sbs_wrapped_sides(m, left_width, right_width);
                    let n = left_rows.len().max(right_rows.len());
                    heights.push(n);
                    let (left_prefix_blank, right_prefix_blank) =
                        sbs_blank_prefixes(&app.theme, lw, m);
                    let empty_row: Vec<Span> = Vec::new();
                    for k in 0..n {
                        let left_content_row = left_rows.get(k).unwrap_or(&empty_row).clone();
                        let right_content_row = right_rows.get(k).unwrap_or(&empty_row).clone();
                        let left_padded =
                            pad_spans_to_width(left_content_row, left_width, m.left_pad_style);
                        let right_padded =
                            pad_spans_to_width(right_content_row, right_width, m.right_pad_style);
                        let (left_prefix, right_prefix) = if k == 0 {
                            (m.left_prefix.clone(), m.right_prefix.clone())
                        } else {
                            (left_prefix_blank.clone(), right_prefix_blank.clone())
                        };
                        let mut spans = left_prefix;
                        spans.extend(left_padded);
                        spans.extend(right_prefix);
                        spans.extend(right_padded);
                        out.push(Line::from(spans));
                    }
                }
                None => {
                    let rows = wrap_spans(&line.spans, viewport_width);
                    heights.push(rows.len());
                    out.extend(rows.into_iter().map(Line::from));
                }
            }
        }
        (heights, Some(out))
    } else {
        (vec![1; visible_lines_unscrolled_for_overlay.len()], None)
    };
    app.diff_state.visible_line_count = populate_row_to_annotation(
        &mut app.diff_row_to_annotation,
        &row_heights,
        viewport_width,
        inner.height as usize,
        wrap,
        scroll_offset,
    );

    let max_scroll_x = max_content_width.saturating_sub(left_width.min(right_width));
    if app.diff_state.scroll_x > max_scroll_x {
        app.diff_state.scroll_x = max_scroll_x;
    }
    if app.diff_state.wrap_lines {
        app.diff_state.scroll_x = 0;
    }

    let scroll_x = app.diff_state.scroll_x;
    let visible_lines: Vec<Line> = match wrapped_lines {
        Some(out) => out,
        // Code rows are always rebuilt from their metadata so both columns
        // clip the same way at any pan offset (the `‹` / `›` markers mark
        // the cut). File and hunk headers and expanders stay put: they are
        // labels, not code, and half a header reads as garbage.
        None => visible_lines_unscrolled
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                if comment_box_row(&line).is_some() {
                    return line;
                }
                if let Some(meta) = sbs_meta.get(&(scroll_offset + i)) {
                    return pan_sbs_row(meta, scroll_x, left_width, right_width);
                }
                let pinned = matches!(
                    app.line_annotations.get(scroll_offset + i),
                    Some(
                        AnnotatedLine::FileHeader { .. }
                            | AnnotatedLine::HunkHeader { .. }
                            | AnnotatedLine::Expander { .. }
                            | AnnotatedLine::HiddenLines { .. }
                            | AnnotatedLine::ReviewedBanner { .. }
                    )
                );
                if scroll_x == 0 || pinned {
                    line
                } else {
                    apply_horizontal_scroll(line, scroll_x)
                }
            })
            .collect(),
    };

    let overlay_ctx = crate::ui::diff_view::DiffOverlayPaint {
        inner,
        visible_lines_unscrolled: &visible_lines_unscrolled_for_overlay,
        line_widths: &line_widths,
        row_heights: &row_heights,
        wrap_lines: app.diff_state.wrap_lines,
        viewport_width: inner.width as usize,
        scroll_x,
        scroll_offset: app.diff_state.scroll_offset,
        theme: &app.theme,
        comment_bars: &comment_bars,
        fixed_gutters: true,
    };

    // Section-marker row tint (hunk headers + expand/hidden stubs).
    crate::ui::diff_view::paint_section_highlight(frame, &overlay_ctx);

    let diff = Paragraph::new(visible_lines).style(styles::panel_style(&app.theme));
    frame.render_widget(diff, inner);

    paint_cursor_line_highlight(
        frame,
        inner,
        &visible_lines_unscrolled_for_overlay,
        &row_heights,
        app,
    );

    // The cursor row's gutters keep their change colour under the tint.
    repaint_sbs_cursor_row_gutters(
        frame,
        inner,
        &app.theme,
        &sbs_meta,
        scroll_offset,
        &row_heights,
        lw,
        left_width,
    );

    // Change-block connectors in the divider: after the cursor-line tint, so
    // a band runs unbroken through the cursor row, and before the caret that
    // shares its last cell.
    paint_sbs_connectors(
        frame,
        inner,
        &app.theme,
        &sbs_meta,
        scroll_offset,
        &row_heights,
        lw,
        (left_width, right_width),
    );

    // Move the `▶` caret onto the active side for the cursor row.
    paint_sbs_active_side_caret(frame, inner, app, lw, left_width, &row_heights);

    if !app.diff_state.wrap_lines {
        paint_sbs_clip_markers(
            frame,
            inner,
            &app.theme,
            &sbs_meta,
            scroll_offset,
            scroll_x,
            lw,
            (left_width, right_width),
        );
    }

    // Painted last so the cell overlay wins over cursor-line bg on overlap.
    if let Some(sel) = app.visual_selection {
        paint_visual_selection_overlay(frame, inner, app, sel, &app.theme);
    }

    // File-section header rules extended to the full viewport width.
    crate::ui::diff_view::paint_file_header_fill(frame, &overlay_ctx);

    // Comment-box overlays painted last so the box + bar always win on their
    // single cells.
    crate::ui::diff_view::paint_comment_box_bar(frame, &overlay_ctx);
    crate::ui::diff_view::paint_comment_box_right_border(frame, &overlay_ctx);

    // Calculate screen position for comment cursor if in Comment mode
    if let Some(cursor_logical_line) = comment_cursor_logical_line {
        let scroll_offset = app.diff_state.scroll_offset;
        let visible_lines_count = app.diff_state.visible_line_count.max(1);

        // Check if the cursor line is visible (after scrolling)
        if cursor_logical_line >= scroll_offset
            && cursor_logical_line < scroll_offset + visible_lines_count
        {
            // Calculate screen row - need to account for wrapping
            let logical_offset = cursor_logical_line - scroll_offset;

            let mut visual_row: u16 = 0;
            let viewport_width = inner.width as usize;

            if app.diff_state.wrap_lines && viewport_width > 0 {
                for i in 0..logical_offset {
                    visual_row += row_heights.get(i).copied().unwrap_or(1) as u16;
                }
            } else {
                visual_row = logical_offset as u16;
            }

            let screen_col = inner.x + comment_cursor_column;
            let screen_row_abs = inner.y + visual_row;

            app.comment_cursor_screen_pos = Some((screen_col, screen_row_abs));
        }
    }
}

/// Render a single expanded context line in side-by-side mode
fn render_sbs_expanded_context_line(
    lines: &mut Vec<Line<'_>>,
    line_idx: &mut usize,
    expanded_line: &crate::model::DiffLine,
    file_idx: usize,
    ctx: &SideBySideContext,
) {
    let theme = ctx.theme;
    let lw = ctx.lineno_width;
    let indicator = cursor_indicator(*line_idx, ctx.current_line_idx);
    let old_line_num = ctx
        .display_lineno(expanded_line.old_lineno, *line_idx)
        .map(|n| format!("{n:>lw$} "))
        .unwrap_or_else(|| " ".repeat(lw + 1));
    let new_line_num = ctx
        .display_lineno(expanded_line.new_lineno, *line_idx)
        .map(|n| format!("{n:>lw$} "))
        .unwrap_or_else(|| " ".repeat(lw + 1));
    let ec_style = styles::expanded_context_style(theme);
    let cell = |width| {
        plain_cell_spans(
            &expanded_line.content,
            ec_style,
            width,
            ctx.search_for(*line_idx),
        )
    };
    let mut line_spans = vec![
        Span::styled(indicator, styles::current_line_indicator_style(theme)),
        Span::styled(old_line_num.clone(), ec_style),
        Span::styled(" ", ec_style),
    ];
    line_spans.extend(cell(ctx.left_width));
    line_spans.extend([
        Span::styled(" │ ", styles::dim_style(theme)),
        Span::styled(new_line_num.clone(), ec_style),
        Span::styled(" ", ec_style),
    ]);
    line_spans.extend(cell(ctx.right_width));
    lines.push(Line::from(line_spans));

    let dim = styles::dim_style(theme);
    let left_prefix = vec![
        Span::styled(indicator, styles::current_line_indicator_style(theme)),
        Span::styled(old_line_num, ec_style),
        Span::styled(" ", ec_style),
    ];
    let right_prefix = vec![
        Span::styled(" │ ", dim),
        Span::styled(new_line_num, ec_style),
        Span::styled(" ", ec_style),
    ];
    let mut content = vec![Span::styled(expanded_line.content.clone(), ec_style)];
    if let Some((needle, hl)) = ctx.search_for(*line_idx) {
        content = apply_search_highlight_spans(content, needle, hl);
    }
    let mut meta = SbsRowMeta {
        left_content: content.clone(),
        right_content: content,
        left_prefix,
        right_prefix,
        left_pad_style: ec_style,
        right_pad_style: ec_style,
        ..SbsRowMeta::default()
    };
    mark_local_edit(
        &mut meta,
        ctx.app.local_edit_mark(file_idx, expanded_line.new_lineno),
        theme,
        ctx.ide(),
    );
    ctx.sbs_meta.borrow_mut().insert(*line_idx, meta);
    *line_idx += 1;
}

/// Process and render all diff lines in a hunk for side-by-side view
/// Returns (new_line_idx, optional cursor info for inline comment input)
fn render_hunk_lines_side_by_side(
    hunk_lines: &[crate::model::DiffLine],
    line_comments: &std::collections::HashMap<u32, Vec<crate::model::Comment>>,
    ctx: &SideBySideContext,
    (file_idx, hunk_idx): (usize, usize),
    mut line_idx: usize,
    lines: &mut Vec<Line>,
) -> (usize, Option<SideBySideCursorInfo>) {
    let mut i = 0;
    let mut cursor_info_out: Option<SideBySideCursorInfo> = None;

    // A commit message is a synthetic "added" file; its lines are Context so
    // the unified view renders them neutrally. In side-by-side that would
    // duplicate the message across both columns, so render it right-side only
    // as an addition instead.
    let is_commit_msg = ctx
        .app
        .diff_files
        .get(file_idx)
        .is_some_and(|f| f.is_commit_message);

    while i < hunk_lines.len() {
        let diff_line = &hunk_lines[i];

        match diff_line.origin {
            LineOrigin::Context if is_commit_msg => {
                let (new_line_idx, cursor_info) = render_commit_message_line_side_by_side(
                    diff_line,
                    line_comments,
                    ctx,
                    file_idx,
                    line_idx,
                    lines,
                );
                line_idx = new_line_idx;
                if cursor_info.is_some() {
                    cursor_info_out = cursor_info;
                }
                i += 1;
            }
            LineOrigin::Context => {
                let (new_line_idx, cursor_info) = render_context_line_side_by_side(
                    diff_line,
                    line_comments,
                    ctx,
                    file_idx,
                    line_idx,
                    lines,
                );
                line_idx = new_line_idx;
                if cursor_info.is_some() {
                    cursor_info_out = cursor_info;
                }
                i += 1;
            }
            LineOrigin::Deletion => {
                let (new_line_idx, lines_processed, cursor_info) =
                    render_deletion_addition_pair_side_by_side(
                        hunk_lines,
                        i,
                        line_comments,
                        ctx,
                        (file_idx, hunk_idx),
                        line_idx,
                        lines,
                    );
                line_idx = new_line_idx;
                if cursor_info.is_some() {
                    cursor_info_out = cursor_info;
                }
                i = lines_processed;
            }
            LineOrigin::Addition => {
                // A run of additions with nothing removed (additions that
                // follow deletions are paired above). The old side folds at
                // the run's first line.
                let starts_block = i == 0 || hunk_lines[i - 1].origin != LineOrigin::Addition;
                let (new_line_idx, cursor_info) = render_standalone_addition_side_by_side(
                    diff_line,
                    starts_block,
                    line_comments,
                    ctx,
                    file_idx,
                    line_idx,
                    lines,
                );
                line_idx = new_line_idx;
                if cursor_info.is_some() {
                    cursor_info_out = cursor_info;
                }
                i += 1;
            }
        }
    }
    (line_idx, cursor_info_out)
}

/// Render a context line (appears on both sides)
/// Returns (new_line_idx, optional cursor info for inline comment input)
fn render_context_line_side_by_side(
    diff_line: &crate::model::DiffLine,
    line_comments: &std::collections::HashMap<u32, Vec<crate::model::Comment>>,
    ctx: &SideBySideContext,
    file_idx: usize,
    mut line_idx: usize,
    lines: &mut Vec<Line>,
) -> (usize, Option<SideBySideCursorInfo>) {
    if ctx.is_visible(line_idx) {
        let w = ctx.lineno_width;
        let old_line_num = ctx
            .display_lineno(diff_line.old_lineno, line_idx)
            .map(|n| format!("{n:>w$}"))
            .unwrap_or_else(|| " ".repeat(w));
        let new_line_num = ctx
            .display_lineno(diff_line.new_lineno, line_idx)
            .map(|n| format!("{n:>w$}"))
            .unwrap_or_else(|| " ".repeat(w));
        let new_line_num = ctx
            .app
            .blame_overlay_idx(file_idx, diff_line.new_lineno, new_line_num);

        let indicator = cursor_indicator(line_idx, ctx.current_line_idx);

        let mut spans = vec![
            Span::styled(indicator, styles::current_line_indicator_style(ctx.theme)),
            Span::styled(format!("{old_line_num} "), styles::dim_style(ctx.theme)),
            Span::styled(" ".to_string(), styles::diff_context_style(ctx.theme)),
        ];

        let search = ctx.search_for(line_idx);
        let content_cell = |width| {
            if let Some(ref highlighted) = diff_line.highlighted_spans {
                searched_cell_spans(
                    highlighted,
                    width,
                    styles::diff_context_style(ctx.theme),
                    search,
                )
            } else {
                plain_cell_spans(
                    &diff_line.content,
                    styles::diff_context_style(ctx.theme),
                    width,
                    search,
                )
            }
        };

        // Left side content - use syntax highlighting if available
        spans.extend(content_cell(ctx.left_width));

        // Separator
        spans.push(Span::styled(" │ ", styles::dim_style(ctx.theme)));
        spans.push(Span::styled(
            format!("{new_line_num} "),
            styles::dim_style(ctx.theme),
        ));
        spans.push(Span::styled(
            " ".to_string(),
            styles::diff_context_style(ctx.theme),
        ));

        // Right side content - use same highlighting
        spans.extend(content_cell(ctx.right_width));

        lines.push(Line::from(spans));

        let content = content_spans_for_diff_line(ctx.theme, diff_line, None, search, None);
        let ctx_style = styles::diff_context_style(ctx.theme);
        let (lp, rp) = sbs_row_prefixes(
            ctx.theme,
            indicator,
            SideSpec {
                lineno: ctx.display_lineno(diff_line.old_lineno, line_idx),
                marker_style: ctx_style,
                ..SideSpec::blank(ctx.theme)
            },
            SideSpec {
                lineno: ctx.display_lineno(diff_line.new_lineno, line_idx),
                marker_style: ctx_style,
                ..SideSpec::blank(ctx.theme)
            },
            w,
        );
        let mut meta = SbsRowMeta {
            left_content: content.clone(),
            right_content: content,
            left_prefix: lp,
            right_prefix: rp,
            left_pad_style: ctx_style,
            right_pad_style: ctx_style,
            ..SbsRowMeta::default()
        };
        mark_local_edit(
            &mut meta,
            ctx.app.local_edit_mark(file_idx, diff_line.new_lineno),
            ctx.theme,
            ctx.ide(),
        );
        ctx.sbs_meta.borrow_mut().insert(line_idx, meta);
    } else {
        lines.push(Line::default());
    }
    line_idx += 1;

    // Add comments if any
    let mut cursor_info_out: Option<SideBySideCursorInfo> = None;
    if let Some(new_ln) = diff_line.new_lineno {
        let (new_line_idx, cursor_info) = add_comments_to_line(
            new_ln,
            line_comments,
            LineSide::New,
            ctx,
            file_idx,
            line_idx,
            lines,
        );
        line_idx = new_line_idx;
        cursor_info_out = cursor_info;
        if let Some(file) = ctx.app.diff_files.get(file_idx) {
            line_idx = add_remote_threads_to_line(
                new_ln,
                LineSide::New,
                ctx,
                file.display_path(),
                line_idx,
                lines,
            );
        }
    }

    (line_idx, cursor_info_out)
}

/// Render paired deletions and additions side-by-side
/// Returns (line_idx, skip_count, optional cursor info for inline comment input)
fn render_deletion_addition_pair_side_by_side(
    hunk_lines: &[crate::model::DiffLine],
    start_idx: usize,
    line_comments: &std::collections::HashMap<u32, Vec<crate::model::Comment>>,
    ctx: &SideBySideContext,
    (file_idx, hunk_idx): (usize, usize),
    mut line_idx: usize,
    lines: &mut Vec<Line>,
) -> (usize, usize, Option<SideBySideCursorInfo>) {
    // Find the range of consecutive deletions
    let mut del_end = start_idx + 1;
    while del_end < hunk_lines.len() && hunk_lines[del_end].origin == LineOrigin::Deletion {
        del_end += 1;
    }

    // Find the range of consecutive additions following the deletions
    let add_start = del_end;
    let mut add_end = add_start;
    while add_end < hunk_lines.len() && hunk_lines[add_end].origin == LineOrigin::Addition {
        add_end += 1;
    }

    let del_count = del_end - start_idx;
    let add_count = add_end - add_start;
    let mut cursor_info_out: Option<SideBySideCursorInfo> = None;

    // The block's rows, laid out the way `rebuild_annotations` paired its
    // lines: by likeness where it recorded an answer, else by position.
    let assign = ctx.app.sbs_pairing.get(&(file_idx, hunk_idx, start_idx));
    let rows =
        crate::ui::intraline::BlockRows::new(del_count, add_count, assign.map(Vec::as_slice));

    // Render each pair of deletion/addition
    for row in 0..rows.count() {
        let (del_offset, add_offset) = rows.row(row);
        let del_opt = del_offset.map(|o| &hunk_lines[start_idx + o]);
        let add_opt = add_offset.map(|o| &hunk_lines[add_start + o]);
        if ctx.is_visible(line_idx) {
            let indicator = cursor_indicator(line_idx, ctx.current_line_idx);

            let mut spans = vec![Span::styled(
                indicator,
                styles::current_line_indicator_style(ctx.theme),
            )];

            // Left side (deletion)
            if let Some(del_line) = del_opt {
                add_deletion_spans(
                    ctx.theme,
                    &mut spans,
                    del_line,
                    ctx.left_width,
                    ctx.lineno_width,
                    ctx.display_lineno(del_line.old_lineno, line_idx),
                    ctx.search_for(line_idx),
                );
            } else {
                add_empty_column_spans(&mut spans, ctx.left_width, ctx.lineno_width);
            }

            spans.push(Span::styled(" │ ", styles::dim_style(ctx.theme)));

            // Right side (addition)
            if let Some(add_line) = add_opt {
                add_addition_spans(
                    ctx.theme,
                    &mut spans,
                    add_line,
                    ctx.right_width,
                    ctx.lineno_width,
                    ctx.display_lineno(add_line.new_lineno, line_idx),
                    ctx.search_for(line_idx),
                );
            } else {
                add_empty_column_spans(&mut spans, ctx.right_width, ctx.lineno_width);
            }

            lines.push(Line::from(spans));

            // Word-level emphasis for a deletion/addition pair: just the
            // tokens that changed get the stronger background.
            let emphasis = match (del_opt, add_opt) {
                (Some(dl), Some(al)) => crate::ui::intraline::changed_ranges(
                    &crate::ui::intraline::rendered_text(dl),
                    &crate::ui::intraline::rendered_text(al),
                ),
                _ => None,
            };

            let w = ctx.lineno_width;
            let search = ctx.search_for(line_idx);
            let (del_kind, add_kind) = match (del_opt, add_opt) {
                (Some(_), Some(_)) => (CellKind::ModifiedOld, CellKind::ModifiedNew),
                _ => (CellKind::Removed, CellKind::Added),
            };
            let left = del_opt.map(|dl| {
                let paint = ctx.cell_paint(del_kind, dl);
                let content = content_spans_for_diff_line(
                    ctx.theme,
                    dl,
                    Some(&paint),
                    search,
                    emphasis.as_ref().map(|(old, _)| old.as_slice()),
                );
                (content, paint, ctx.display_lineno(dl.old_lineno, line_idx))
            });
            let right = add_opt.map(|al| {
                let paint = ctx.cell_paint(add_kind, al);
                let content = content_spans_for_diff_line(
                    ctx.theme,
                    al,
                    Some(&paint),
                    search,
                    emphasis.as_ref().map(|(_, new)| new.as_slice()),
                );
                (content, paint, ctx.display_lineno(al.new_lineno, line_idx))
            });
            let side_spec = |side: &Option<(Vec<Span<'static>>, CellPaint, Option<u32>)>,
                             blame: Option<String>| match side {
                Some((_, paint, lineno)) => SideSpec {
                    lineno: *lineno,
                    blame,
                    marker: paint.marker,
                    marker_style: paint.marker_style,
                    gutter: paint.gutter,
                },
                None => SideSpec::blank(ctx.theme),
            };
            let (lp, rp) = sbs_row_prefixes(
                ctx.theme,
                indicator,
                side_spec(&left, None),
                side_spec(
                    &right,
                    ctx.app
                        .blame_label_for(file_idx, add_opt.and_then(|al| al.new_lineno)),
                ),
                w,
            );
            let mut meta = SbsRowMeta {
                left_prefix: lp,
                right_prefix: rp,
                // The first row of a run of one-sided rows is where the
                // other side closes.
                link: match (del_opt, add_opt) {
                    (Some(_), Some(_)) => SbsLink::Both,
                    (Some(_), None) => SbsLink::OldOnly {
                        fold: rows.starts_one_sided_run(row),
                    },
                    (None, Some(_)) => SbsLink::NewOnly {
                        fold: rows.starts_one_sided_run(row),
                    },
                    (None, None) => SbsLink::None,
                },
                ..SbsRowMeta::default()
            };
            if let Some((content, paint, _)) = left {
                meta.left_content = content;
                meta.left_pad_style = paint.plain;
                meta.left_gutter_bg = paint.gutter_bg;
                meta.left_accent = paint.accent;
            }
            if let Some((content, paint, _)) = right {
                meta.right_content = content;
                meta.right_pad_style = paint.plain;
                meta.right_gutter_bg = paint.gutter_bg;
                meta.right_accent = paint.accent;
            }
            mark_local_edit(
                &mut meta,
                add_opt.and_then(|al| ctx.app.local_edit_mark(file_idx, al.new_lineno)),
                ctx.theme,
                ctx.ide(),
            );
            ctx.sbs_meta.borrow_mut().insert(line_idx, meta);
        } else {
            lines.push(Line::default());
        }
        line_idx += 1;

        // Add comments for deletion
        if let Some(del_line) = del_opt
            && let Some(old_ln) = del_line.old_lineno
        {
            let (new_line_idx, cursor_info) = add_comments_to_line(
                old_ln,
                line_comments,
                LineSide::Old,
                ctx,
                file_idx,
                line_idx,
                lines,
            );
            line_idx = new_line_idx;
            if cursor_info.is_some() {
                cursor_info_out = cursor_info;
            }
            if let Some(file) = ctx.app.diff_files.get(file_idx) {
                line_idx = add_remote_threads_to_line(
                    old_ln,
                    LineSide::Old,
                    ctx,
                    file.display_path(),
                    line_idx,
                    lines,
                );
            }
        }

        // Add comments for addition
        if let Some(add_line) = add_opt
            && let Some(new_ln) = add_line.new_lineno
        {
            let (new_line_idx, cursor_info) = add_comments_to_line(
                new_ln,
                line_comments,
                LineSide::New,
                ctx,
                file_idx,
                line_idx,
                lines,
            );
            line_idx = new_line_idx;
            if cursor_info.is_some() {
                cursor_info_out = cursor_info;
            }
            if let Some(file) = ctx.app.diff_files.get(file_idx) {
                line_idx = add_remote_threads_to_line(
                    new_ln,
                    LineSide::New,
                    ctx,
                    file.display_path(),
                    line_idx,
                    lines,
                );
            }
        }
    }

    (line_idx, add_end, cursor_info_out)
}

/// Render a standalone addition (no matching deletion)
/// Returns (new_line_idx, optional cursor info for inline comment input)
fn render_standalone_addition_side_by_side(
    diff_line: &crate::model::DiffLine,
    starts_block: bool,
    line_comments: &std::collections::HashMap<u32, Vec<crate::model::Comment>>,
    ctx: &SideBySideContext,
    file_idx: usize,
    mut line_idx: usize,
    lines: &mut Vec<Line>,
) -> (usize, Option<SideBySideCursorInfo>) {
    if ctx.is_visible(line_idx) {
        let indicator = cursor_indicator(line_idx, ctx.current_line_idx);

        let mut spans = vec![Span::styled(
            indicator,
            styles::current_line_indicator_style(ctx.theme),
        )];
        add_empty_column_spans(&mut spans, ctx.left_width, ctx.lineno_width);
        spans.push(Span::styled(" │ ", styles::dim_style(ctx.theme)));
        add_addition_spans(
            ctx.theme,
            &mut spans,
            diff_line,
            ctx.right_width,
            ctx.lineno_width,
            ctx.display_lineno(diff_line.new_lineno, line_idx),
            ctx.search_for(line_idx),
        );

        lines.push(Line::from(spans));

        let w = ctx.lineno_width;
        let paint = ctx.cell_paint(CellKind::Added, diff_line);
        let right_content = content_spans_for_diff_line(
            ctx.theme,
            diff_line,
            Some(&paint),
            ctx.search_for(line_idx),
            None,
        );
        let (lp, rp) = sbs_row_prefixes(
            ctx.theme,
            indicator,
            SideSpec::blank(ctx.theme),
            SideSpec {
                lineno: ctx.display_lineno(diff_line.new_lineno, line_idx),
                blame: None,
                marker: paint.marker,
                marker_style: paint.marker_style,
                gutter: paint.gutter,
            },
            w,
        );
        let mut meta = SbsRowMeta {
            right_content,
            left_prefix: lp,
            right_prefix: rp,
            right_pad_style: paint.plain,
            right_gutter_bg: paint.gutter_bg,
            right_accent: paint.accent,
            link: SbsLink::NewOnly { fold: starts_block },
            ..SbsRowMeta::default()
        };
        mark_local_edit(
            &mut meta,
            ctx.app.local_edit_mark(file_idx, diff_line.new_lineno),
            ctx.theme,
            ctx.ide(),
        );
        ctx.sbs_meta.borrow_mut().insert(line_idx, meta);
    } else {
        lines.push(Line::default());
    }
    line_idx += 1;

    // Add comments if any
    let mut cursor_info_out: Option<SideBySideCursorInfo> = None;
    if let Some(new_ln) = diff_line.new_lineno {
        let (new_line_idx, cursor_info) = add_comments_to_line(
            new_ln,
            line_comments,
            LineSide::New,
            ctx,
            file_idx,
            line_idx,
            lines,
        );
        line_idx = new_line_idx;
        cursor_info_out = cursor_info;
        if let Some(file) = ctx.app.diff_files.get(file_idx) {
            line_idx = add_remote_threads_to_line(
                new_ln,
                LineSide::New,
                ctx,
                file.display_path(),
                line_idx,
                lines,
            );
        }
    }

    (line_idx, cursor_info_out)
}

/// Render a commit-message line in side-by-side mode. The commit message is a
/// synthetic "added" file, but visually it is prose, not code: delta renders it
/// as a full-width block, not confined to a diff column. So we emit a single
/// full-width, left-aligned, neutrally-styled line with no column split, diff
/// coloring, or per-column line numbers. It is deliberately NOT inserted into
/// `sbs_meta`, so the wrap path falls through to the full-width wrapping branch.
fn render_commit_message_line_side_by_side(
    diff_line: &crate::model::DiffLine,
    line_comments: &std::collections::HashMap<u32, Vec<crate::model::Comment>>,
    ctx: &SideBySideContext,
    file_idx: usize,
    mut line_idx: usize,
    lines: &mut Vec<Line>,
) -> (usize, Option<SideBySideCursorInfo>) {
    let ctx_style = styles::diff_context_style(ctx.theme);

    if ctx.is_visible(line_idx) {
        let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
        let mut spans = vec![Span::styled(
            indicator,
            styles::current_line_indicator_style(ctx.theme),
        )];
        // Git-style two-space indent, then the message text at full width.
        spans.push(Span::styled("  ".to_string(), ctx_style));
        spans.push(Span::styled(diff_line.content.clone(), ctx_style));

        lines.push(Line::from(spans));
    } else {
        lines.push(Line::default());
    }
    line_idx += 1;

    let mut cursor_info_out: Option<SideBySideCursorInfo> = None;
    if let Some(new_ln) = diff_line.new_lineno {
        let (new_line_idx, cursor_info) = add_comments_to_line(
            new_ln,
            line_comments,
            LineSide::New,
            ctx,
            file_idx,
            line_idx,
            lines,
        );
        line_idx = new_line_idx;
        cursor_info_out = cursor_info;
        if let Some(file) = ctx.app.diff_files.get(file_idx) {
            line_idx = add_remote_threads_to_line(
                new_ln,
                LineSide::New,
                ctx,
                file.display_path(),
                line_idx,
                lines,
            );
        }
    }

    (line_idx, cursor_info_out)
}

/// Add deletion line spans to the spans vector
fn add_deletion_spans(
    theme: &Theme,
    spans: &mut Vec<Span>,
    diff_line: &crate::model::DiffLine,
    content_width: usize,
    lw: usize,
    display_lineno: Option<u32>,
    search: Option<(&crate::ui::text_utils::SearchPattern, Style)>,
) {
    let line_num = display_lineno
        .map(|n| format!("{n:>lw$}"))
        .unwrap_or_else(|| " ".repeat(lw));

    spans.push(Span::styled(
        format!("{line_num} "),
        styles::dim_style(theme),
    ));
    spans.push(Span::styled("▌".to_string(), styles::diff_del_style(theme)));

    // Use syntax highlighting if available
    if let Some(ref highlighted) = diff_line.highlighted_spans {
        let syntax_pad_style = Style::default().fg(theme.diff_del).bg(theme.syntax_del_bg);
        let patched: Vec<(Style, String)> = highlighted
            .iter()
            .map(|(s, t)| {
                (
                    styles::patch_highlighted_span_bg(*s, theme, LineOrigin::Deletion),
                    t.clone(),
                )
            })
            .collect();
        let content_spans = searched_cell_spans(&patched, content_width, syntax_pad_style, search);
        spans.extend(content_spans);
    } else {
        spans.extend(plain_cell_spans(
            &diff_line.content,
            styles::diff_del_style(theme),
            content_width,
            search,
        ));
    }
}

/// Add addition line spans to the spans vector
fn add_addition_spans(
    theme: &Theme,
    spans: &mut Vec<Span>,
    diff_line: &crate::model::DiffLine,
    content_width: usize,
    lw: usize,
    display_lineno: Option<u32>,
    search: Option<(&crate::ui::text_utils::SearchPattern, Style)>,
) {
    let line_num = display_lineno
        .map(|n| format!("{n:>lw$}"))
        .unwrap_or_else(|| " ".repeat(lw));

    spans.push(Span::styled(
        format!("{line_num} "),
        styles::dim_style(theme),
    ));
    spans.push(Span::styled("▌".to_string(), styles::diff_add_style(theme)));

    // Use syntax highlighting if available
    if let Some(ref highlighted) = diff_line.highlighted_spans {
        let syntax_pad_style = Style::default().fg(theme.diff_add).bg(theme.syntax_add_bg);
        let patched: Vec<(Style, String)> = highlighted
            .iter()
            .map(|(s, t)| {
                (
                    styles::patch_highlighted_span_bg(*s, theme, LineOrigin::Addition),
                    t.clone(),
                )
            })
            .collect();
        let content_spans = searched_cell_spans(&patched, content_width, syntax_pad_style, search);
        spans.extend(content_spans);
    } else {
        spans.extend(plain_cell_spans(
            &diff_line.content,
            styles::diff_add_style(theme),
            content_width,
            search,
        ));
    }
}

/// Add empty column spans (for when one side has no content)
fn add_empty_column_spans(spans: &mut Vec<Span>, content_width: usize, lw: usize) {
    // line_num(lw) + space(1) + prefix(1) + content
    spans.push(Span::styled(
        " ".repeat(lw + 1 + 1 + content_width),
        Style::default(),
    ));
}

/// Add comments for a specific line.
/// Returns (new_line_idx, optional cursor info for inline comment input)
/// Render remote review threads anchored at this `(file, line, side)`
/// position into the side-by-side rendering. Mirrors the unified-view
/// helper but uses the side-by-side cursor indicator path.
fn add_remote_threads_to_line(
    line_num: u32,
    side: LineSide,
    ctx: &SideBySideContext,
    file_path: &std::path::Path,
    mut line_idx: usize,
    lines: &mut Vec<Line>,
) -> usize {
    use crate::forge::remote_comments::{PrCommentsVisibility, RemoteCommentSide};
    let visibility = ctx.app.session.remote_comments_visibility;
    if matches!(visibility, PrCommentsVisibility::Hide) {
        return line_idx;
    }
    let target_path = file_path.to_string_lossy();
    for thread in &ctx.app.forge_review_threads {
        let Some(muted) = visibility.render_decision(thread) else {
            continue;
        };
        if thread.path != *target_path {
            continue;
        }
        let Some(thread_line) = thread.line else {
            continue;
        };
        if thread_line != line_num {
            continue;
        }
        let matches_side = matches!(
            (thread.side, side),
            (RemoteCommentSide::Right, LineSide::New) | (RemoteCommentSide::Left, LineSide::Old)
        );
        if !matches_side {
            continue;
        }
        let thread_lines = comment_panel::format_remote_thread_lines(
            ctx.theme,
            thread,
            muted,
            ctx.app.forge_kind(),
        );
        let box_top_row = line_idx;
        for mut comment_line in thread_lines {
            let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
            comment_line.spans.insert(
                0,
                Span::styled(indicator, styles::current_line_indicator_style(ctx.theme)),
            );
            lines.push(comment_line);
            line_idx += 1;
        }
        crate::ui::diff_view::push_comment_bar(
            &mut ctx.comment_bars.borrow_mut(),
            box_top_row,
            Some(crate::model::LineRange::single(thread_line)),
        );
    }
    line_idx
}

fn add_comments_to_line(
    line_num: u32,
    line_comments: &std::collections::HashMap<u32, Vec<crate::model::Comment>>,
    side: LineSide,
    ctx: &SideBySideContext,
    file_idx: usize,
    mut line_idx: usize,
    lines: &mut Vec<Line>,
) -> (usize, Option<SideBySideCursorInfo>) {
    // Check if we're adding/editing a comment on this line and side
    let is_line_comment_mode = ctx.comment_input_mode
        && file_idx == ctx.current_file_idx
        && ctx.comment_line == Some((line_num, side));
    let mut cursor_info_out: Option<SideBySideCursorInfo> = None;

    if let Some(comments) = line_comments.get(&line_num) {
        for comment in comments {
            let comment_side = comment.side.unwrap_or(LineSide::New);
            if ((side == LineSide::Old && comment_side == LineSide::Old)
                || (side == LineSide::New && comment_side != LineSide::Old))
                && ctx.app.comment_visible(comment)
            {
                // Check if this comment is being edited
                let is_being_edited =
                    is_line_comment_mode && ctx.editing_comment_id == Some(comment.id.as_str());

                if is_being_edited {
                    // Render inline input instead
                    let line_range = ctx
                        .comment_line_range
                        .or_else(|| Some(LineRange::single(line_num)));
                    let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
                        ctx.theme,
                        comment_type_presentation(ctx.app, &ctx.comment_type),
                        ctx.comment_buffer,
                        ctx.comment_cursor,
                        line_range,
                        true,
                        ctx.panel_width.saturating_sub(1),
                        ctx.app
                            .comment_vim_mode_label()
                            .as_ref()
                            .map(|(t, w)| (t.as_str(), *w)),
                        ctx.app.supports_keyboard_enhancement,
                    );
                    let box_top_row = line_idx;
                    let box_end = line_idx + input_lines.len().saturating_sub(1);
                    let annotations_replaced = App::comment_display_lines(comment, ctx.panel_width);
                    cursor_info_out = Some((
                        line_idx + cursor_info.line_offset,
                        1 + cursor_info.column,
                        line_idx,
                        box_end,
                        annotations_replaced,
                    ));

                    for mut input_line in input_lines {
                        let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
                        input_line.spans.insert(
                            0,
                            Span::styled(
                                indicator,
                                styles::current_line_indicator_style(ctx.theme),
                            ),
                        );
                        lines.push(input_line);
                        line_idx += 1;
                    }
                    crate::ui::diff_view::push_comment_bar(
                        &mut ctx.comment_bars.borrow_mut(),
                        box_top_row,
                        line_range,
                    );
                } else {
                    let line_range = comment
                        .line_range
                        .or_else(|| Some(LineRange::single(line_num)));
                    let box_top_row = line_idx;
                    let rows = App::comment_display_lines(comment, ctx.panel_width);
                    // The bar is recorded either way: it is painted above the
                    // box, so it can be on screen while the box itself is not.
                    if !ctx.box_visible(line_idx, rows) {
                        skip_comment_box(lines, &mut line_idx, rows);
                    } else {
                        let comment_lines = comment_panel::format_comment_lines(
                            ctx.theme,
                            comment_type_presentation(ctx.app, &comment.comment_type),
                            &comment.content,
                            line_range,
                            ctx.panel_width.saturating_sub(1),
                            (comment.author != ctx.app.username).then_some(comment.author.as_str()),
                        );
                        for mut comment_line in comment_lines {
                            let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
                            comment_line.spans.insert(
                                0,
                                Span::styled(
                                    indicator,
                                    styles::current_line_indicator_style(ctx.theme),
                                ),
                            );
                            lines.push(comment_line);
                            line_idx += 1;
                        }
                    }
                    crate::ui::diff_view::push_comment_bar(
                        &mut ctx.comment_bars.borrow_mut(),
                        box_top_row,
                        line_range,
                    );
                }
            }
        }
    }

    // Render inline input for new line comment
    if is_line_comment_mode && ctx.editing_comment_id.is_none() {
        let line_range = ctx
            .comment_line_range
            .or_else(|| Some(LineRange::single(line_num)));
        let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
            ctx.theme,
            comment_type_presentation(ctx.app, &ctx.comment_type),
            ctx.comment_buffer,
            ctx.comment_cursor,
            line_range,
            false,
            ctx.panel_width.saturating_sub(1),
            ctx.app
                .comment_vim_mode_label()
                .as_ref()
                .map(|(t, w)| (t.as_str(), *w)),
            ctx.app.supports_keyboard_enhancement,
        );
        let box_top_row = line_idx;
        let box_end = line_idx + input_lines.len().saturating_sub(1);
        cursor_info_out = Some((
            line_idx + cursor_info.line_offset,
            1 + cursor_info.column,
            line_idx,
            box_end,
            0,
        ));

        for mut input_line in input_lines {
            let indicator = cursor_indicator(line_idx, ctx.current_line_idx);
            input_line.spans.insert(
                0,
                Span::styled(indicator, styles::current_line_indicator_style(ctx.theme)),
            );
            lines.push(input_line);
            line_idx += 1;
        }
        crate::ui::diff_view::push_comment_bar(
            &mut ctx.comment_bars.borrow_mut(),
            box_top_row,
            line_range,
        );
    }

    (line_idx, cursor_info_out)
}

#[cfg(test)]
mod remote_comments_side_by_side_snapshot_tests {
    //! Render-snapshot tests for inline remote review threads in the
    //! side-by-side diff view. Confirms the badge appears at least once
    //! when a thread is active and is hidden under `:comments hide`.
    use crate::app::{App, DiffSource, DiffViewMode, InputMode, PullRequestDiffSource};
    use crate::error::Result as TuicrResult;
    use crate::error::TuicrError;
    use crate::forge::remote_comments::{
        PrCommentsVisibility, RemoteCommentSide, RemoteReviewComment, RemoteReviewThread,
    };
    use crate::forge::traits::{ForgeRepository, PrSessionKey};
    use crate::model::{
        DiffFile, DiffHunk, DiffLine, FileStatus, LineOrigin, LineSide, ReviewSession,
        SessionDiffSource,
    };
    use crate::syntax::SyntaxHighlighter;
    use crate::theme::Theme;
    use crate::ui::render;
    use crate::vcs::traits::{VcsBackend, VcsChangeStatus, VcsInfo, VcsType};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::path::{Path, PathBuf};

    struct SnapshotVcs {
        info: VcsInfo,
    }

    impl VcsBackend for SnapshotVcs {
        fn info(&self) -> &VcsInfo {
            &self.info
        }
        fn get_working_tree_diff(
            &self,
            _highlighter: &SyntaxHighlighter,
        ) -> TuicrResult<Vec<DiffFile>> {
            Err(TuicrError::NoChanges)
        }
        fn fetch_context_lines(
            &self,
            _file_path: &Path,
            _file_status: FileStatus,
            _ref_commit: Option<&str>,
            _start_line: u32,
            _end_line: u32,
        ) -> TuicrResult<Vec<DiffLine>> {
            Ok(Vec::new())
        }
        fn get_change_status(&self) -> TuicrResult<VcsChangeStatus> {
            Ok(VcsChangeStatus {
                staged: false,
                unstaged: false,
            })
        }
        fn file_line_count(
            &self,
            _file_path: &Path,
            _file_status: FileStatus,
            _ref_commit: Option<&str>,
        ) -> TuicrResult<u32> {
            Ok(0)
        }
    }

    fn repo() -> ForgeRepository {
        ForgeRepository::github("github.com", "agavra", "tuicr")
    }

    fn sample_diff_file() -> DiffFile {
        let lines = vec![
            DiffLine {
                origin: LineOrigin::Context,
                content: "first".to_string(),
                old_lineno: Some(1),
                new_lineno: Some(1),
                raw_content: None,
                highlighted_spans: None,
            },
            DiffLine {
                origin: LineOrigin::Addition,
                content: "second".to_string(),
                old_lineno: None,
                new_lineno: Some(2),
                raw_content: None,
                highlighted_spans: None,
            },
        ];
        let hunk = DiffHunk {
            header: "@@ -1,1 +1,2 @@".to_string(),
            lines,
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 2,
        };
        let hunks = vec![hunk];
        let content_hash = DiffFile::compute_content_hash(&hunks);
        DiffFile {
            old_path: Some(PathBuf::from("src/lib.rs")),
            new_path: Some(PathBuf::from("src/lib.rs")),
            status: FileStatus::Modified,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash,
        }
    }

    fn thread() -> RemoteReviewThread {
        RemoteReviewThread {
            id: "T".to_string(),
            path: "src/lib.rs".to_string(),
            line: Some(2),
            side: RemoteCommentSide::Right,
            is_resolved: false,
            is_outdated: false,
            comments: vec![RemoteReviewComment {
                id: "C".to_string(),
                author: Some("alice".to_string()),
                body: "sbs hello".to_string(),
                url: "https://example.com".to_string(),
                ..Default::default()
            }],
        }
    }

    fn make_pr_app() -> App {
        make_pr_app_with(vec![sample_diff_file()])
    }

    fn make_pr_app_with(diff_files: Vec<DiffFile>) -> App {
        let pr = PullRequestDiffSource {
            key: PrSessionKey::new(repo(), 125, "headsha".to_string()),
            base_sha: "basesha".to_string(),
            title: "test pr".to_string(),
            url: "https://example.com".to_string(),
            head_ref_name: "feat".to_string(),
            base_ref_name: "main".to_string(),
            state: "OPEN".to_string(),
            closed: false,
            merged: false,
        };
        let vcs_info = VcsInfo {
            root_path: PathBuf::from("forge:github.com/agavra/tuicr"),
            head_commit: "headsha".to_string(),
            branch_name: Some("feat".to_string()),
            vcs_type: VcsType::File,
        };
        let mut session = ReviewSession::new(
            vcs_info.root_path.clone(),
            "headsha".to_string(),
            Some("feat".to_string()),
            SessionDiffSource::PullRequest,
        );
        session.pr_session_key = Some(pr.key.clone());
        let mut app = App::build(
            Box::new(SnapshotVcs {
                info: vcs_info.clone(),
            }),
            vcs_info,
            Theme::dark(),
            None,
            false,
            diff_files,
            session,
            DiffSource::PullRequest(Box::new(pr)),
            InputMode::Normal,
            Vec::new(),
            None,
            None,
        )
        .expect("build app");
        app.diff_view_mode = DiffViewMode::SideBySide;
        app
    }

    fn draw(app: &mut App) -> Buffer {
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, app))
            .expect("draw frame");
        terminal.backend().buffer().clone()
    }

    fn body_text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Side-by-side mirror of the unified culling test: the skip/emit wiring
    /// here goes through `ctx.box_visible` and a by-value `line_idx`, so it
    /// needs its own coverage.
    #[test]
    fn should_cull_comment_boxes_outside_the_viewport() {
        use crate::app::AnnotatedLine;
        use crate::model::{Comment, CommentType};

        const NEEDLE: &str = "far-below-the-fold";

        let lines: Vec<DiffLine> = (1..=120)
            .map(|n| DiffLine {
                origin: LineOrigin::Addition,
                content: format!("line {n}"),
                old_lineno: None,
                new_lineno: Some(n),
                raw_content: None,
                highlighted_spans: None,
            })
            .collect();
        let hunks = vec![DiffHunk {
            header: "@@ -0,0 +1,120 @@".to_string(),
            lines,
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: 120,
        }];
        let content_hash = DiffFile::compute_content_hash(&hunks);
        let path = PathBuf::from("src/lib.rs");
        let file = DiffFile {
            old_path: Some(path.clone()),
            new_path: Some(path.clone()),
            status: FileStatus::Modified,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash,
        };

        let mut app = make_pr_app_with(vec![file]);
        app.session
            .get_file_mut(&path)
            .expect("file registered in session")
            .add_line_comment(
                100,
                Comment::new(NEEDLE.to_string(), CommentType::from_id("note"), None),
            );
        app.rebuild_annotations();

        let body = body_text(&draw(&mut app));
        assert!(
            !body.contains(NEEDLE),
            "off-screen comment should not be visible:\n{body}"
        );

        let comment_row = app
            .line_annotations
            .iter()
            .position(|a| matches!(a, AnnotatedLine::LineComment { .. }))
            .expect("comment annotated in the document");
        app.diff_state.scroll_offset = comment_row;
        app.diff_state.cursor_line = comment_row;

        let body = body_text(&draw(&mut app));
        assert!(
            body.contains(NEEDLE),
            "comment scrolled into view should render at its annotated row:\n{body}"
        );
    }

    #[test]
    fn should_render_remote_comment_inline_in_side_by_side_diff() {
        // given
        let mut app = make_pr_app();
        app.forge_review_threads = vec![thread()];
        app.rebuild_annotations();
        let buffer = draw(&mut app);
        // then
        let body = body_text(&buffer);
        assert!(
            body.contains("[github @alice]"),
            "expected badge in side-by-side render:\n{body}"
        );
    }

    #[test]
    fn should_hide_remote_comments_under_comments_hide_in_side_by_side() {
        // given
        let mut app = make_pr_app();
        app.forge_review_threads = vec![thread()];
        app.set_remote_comments_visibility(PrCommentsVisibility::Hide);
        let buffer = draw(&mut app);
        // then
        let body = body_text(&buffer);
        assert!(
            !body.contains("[github @alice"),
            "remote comment leaked under Hide:\n{body}"
        );
    }

    fn diff_file_with_pair(left: &str, right: &str) -> DiffFile {
        let lines = vec![
            DiffLine {
                origin: LineOrigin::Deletion,
                content: left.to_string(),
                old_lineno: Some(1),
                new_lineno: None,
                raw_content: None,
                highlighted_spans: None,
            },
            DiffLine {
                origin: LineOrigin::Addition,
                content: right.to_string(),
                old_lineno: None,
                new_lineno: Some(1),
                raw_content: None,
                highlighted_spans: None,
            },
        ];
        let hunks = vec![DiffHunk {
            header: "@@ -1,1 +1,1 @@".to_string(),
            lines,
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
        }];
        let content_hash = DiffFile::compute_content_hash(&hunks);
        DiffFile {
            old_path: Some(PathBuf::from("src/lib.rs")),
            new_path: Some(PathBuf::from("src/lib.rs")),
            status: FileStatus::Modified,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash,
        }
    }

    fn diff_file_with_standalone_deletion(left: &str) -> DiffFile {
        let lines = vec![DiffLine {
            origin: LineOrigin::Deletion,
            content: left.to_string(),
            old_lineno: Some(1),
            new_lineno: None,
            raw_content: None,
            highlighted_spans: None,
        }];
        let hunks = vec![DiffHunk {
            header: "@@ -1,1 +0,0 @@".to_string(),
            lines,
            old_start: 1,
            old_count: 1,
            new_start: 0,
            new_count: 0,
        }];
        let content_hash = DiffFile::compute_content_hash(&hunks);
        DiffFile {
            old_path: Some(PathBuf::from("src/lib.rs")),
            new_path: Some(PathBuf::from("src/lib.rs")),
            status: FileStatus::Modified,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash,
        }
    }

    fn draw_sbs(app: &mut App, w: u16, h: u16) -> Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::render_side_by_side_diff(frame, app, Rect::new(0, 0, w, h)))
            .expect("draw sbs");
        terminal.backend().buffer().clone()
    }

    fn char_at(buf: &Buffer, x: u16, y: u16) -> String {
        buf[(x, y)].symbol().to_string()
    }

    #[test]
    fn should_wrap_long_line_in_side_by_side_view_when_wrap_enabled() {
        let long_left = "L".repeat(200);
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(&long_left, "short")];
        app.set_diff_wrap(true);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 160, 20);

        let mut rows_with_l = 0u16;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| char_at(&buf, x, y)).collect();
            if row.contains("LLLLLLLLLL") {
                rows_with_l += 1;
            }
        }
        assert!(
            rows_with_l >= 2,
            "expected long left content to span >=2 visual rows, got {rows_with_l}"
        );
    }

    #[test]
    fn should_not_wrap_when_wrap_disabled_in_side_by_side() {
        let long_left = "L".repeat(200);
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(&long_left, "short")];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 160, 20);

        let rows_with_l: u16 = (0..buf.area.height)
            .filter(|&y| {
                (0..buf.area.width)
                    .map(|x| char_at(&buf, x, y))
                    .collect::<String>()
                    .contains("LLLLLLLLLL")
            })
            .count() as u16;
        assert_eq!(
            rows_with_l, 1,
            "wrap-off should produce exactly one row of L, got {rows_with_l}"
        );
    }

    #[test]
    fn should_pan_both_columns_when_wrap_is_disabled() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(
            &format!("0000LEFT{}", "L".repeat(100)),
            &format!("0000RIGHT{}", "R".repeat(100)),
        )];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let _ = draw_sbs(&mut app, 80, 20);
        app.scroll_right(4);
        let buf = draw_sbs(&mut app, 80, 20);

        assert_eq!(app.diff_state.scroll_x, 4);
        let body = body_text(&buf);
        assert!(body.contains("LEFT"), "left column did not pan:\n{body}");
        assert!(body.contains("RIGHT"), "right column did not pan:\n{body}");
        assert!(
            !body.contains("0000LEFT"),
            "left column stayed put:\n{body}"
        );
        assert!(
            !body.contains("0000RIGHT"),
            "right column stayed put:\n{body}"
        );

        let row = (0..buf.area.height)
            .find(|&y| {
                (0..buf.area.width)
                    .map(|x| char_at(&buf, x, y))
                    .collect::<String>()
                    .contains("LEFT")
            })
            .expect("panned diff row");
        let lw = app.lineno_width();
        let content_width = (78 - crate::app::sbs_overhead(lw) as usize) / 2;
        let left_start = 1 + crate::app::sbs_left_gutter(lw);
        let divider = left_start + content_width as u16 + 1;
        let right_start = left_start + content_width as u16 + lw as u16 + 5;
        assert_eq!(char_at(&buf, left_start, row), "L");
        // A changed pair: the divider column holds the seam of its band.
        assert_eq!(char_at(&buf, divider, row), "▌");
        assert_eq!(char_at(&buf, right_start, row), "R");

        app.enter_comment_mode(false, Some((1, LineSide::New)));
        app.comment_buffer = "COMMENT".to_string();
        app.comment_cursor = app.comment_buffer.len();
        let buf = draw_sbs(&mut app, 80, 20);
        let (cursor_x, cursor_y) = app.comment_cursor_screen_pos.expect("comment cursor");
        assert_eq!(app.diff_state.scroll_x, 4);
        assert!(body_text(&buf).contains("COMMENT"));
        assert_eq!(char_at(&buf, cursor_x - 1, cursor_y), "T");
    }

    #[test]
    fn should_align_divider_on_wrapped_rows_in_side_by_side() {
        let long_left = "L".repeat(200);
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(&long_left, "short")];
        app.set_diff_wrap(true);
        app.rebuild_annotations();

        let lw = 1usize;
        let inner_w = 158usize;
        let content_width = (inner_w - crate::app::sbs_overhead(lw) as usize) / 2;
        let divider_x_inner = crate::app::sbs_left_gutter(lw) as usize + content_width;
        let divider_glyph_x = 1 + divider_x_inner + 1;

        let buf = draw_sbs(&mut app, 160, 20);

        let mut rows_with_l: Vec<u16> = Vec::new();
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| char_at(&buf, x, y)).collect();
            if row.contains("LLLLLLLLLL") {
                rows_with_l.push(y);
            }
        }
        assert!(
            rows_with_l.len() >= 2,
            "expected ≥2 wrapped rows, got {}",
            rows_with_l.len()
        );
        // The pair is one changed line on each side, so every wrapped row
        // carries the seam of its band in the divider column.
        for y in &rows_with_l {
            let glyph = char_at(&buf, divider_glyph_x as u16, *y);
            assert_eq!(
                glyph, "▌",
                "expected ▌ at col {divider_glyph_x} on row {y}, got {glyph:?}"
            );
        }
    }

    #[test]
    fn should_pad_shorter_column_on_wrapped_rows_in_side_by_side() {
        let long_left = "L".repeat(200);
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_standalone_deletion(&long_left)];
        app.set_diff_wrap(true);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 160, 20);

        let lw = 1usize;
        let inner_w = 158usize;
        let content_width = (inner_w - crate::app::sbs_overhead(lw) as usize) / 2;
        let divider_glyph_x = 1 + crate::app::sbs_left_gutter(lw) as usize + content_width + 1;
        let right_content_start = divider_glyph_x + 2 + lw + 1 + 1;
        let right_content_end = right_content_start + content_width;

        let mut checked = 0;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| char_at(&buf, x, y)).collect();
            if !row.contains("LLLLLLLLLL") {
                continue;
            }
            checked += 1;
            let right: String = (right_content_start..right_content_end)
                .map(|x| char_at(&buf, x as u16, y))
                .collect();
            // The deletion has no counterpart: its first row draws the fold
            // rule across the new side, the rows under it stay blank.
            if checked == 1 {
                assert!(
                    right.chars().all(|c| c == '▔'),
                    "right column should hold the fold rule on row {y}, got {right:?}"
                );
            } else {
                assert!(
                    right.trim().is_empty(),
                    "right column should be blank on wrapped L row {y}, got {right:?}"
                );
            }
        }
        assert!(
            checked >= 2,
            "expected ≥2 wrapped rows to check, got {checked}"
        );
    }

    fn commit_message_file(message: &str) -> DiffFile {
        let lines: Vec<DiffLine> = message
            .lines()
            .enumerate()
            .map(|(i, line)| DiffLine {
                origin: LineOrigin::Context,
                content: line.to_string(),
                old_lineno: None,
                new_lineno: Some(i as u32 + 1),
                raw_content: None,
                highlighted_spans: None,
            })
            .collect();
        let new_count = lines.len() as u32;
        let hunks = vec![DiffHunk {
            header: String::new(),
            lines,
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count,
        }];
        let content_hash = DiffFile::compute_content_hash(&hunks);
        DiffFile {
            old_path: None,
            new_path: Some(PathBuf::from("Commit Message (abc1234)")),
            status: FileStatus::Added,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: true,
            content_hash,
        }
    }

    #[test]
    fn should_render_commit_message_full_width_in_side_by_side() {
        let mut app = make_pr_app();
        app.diff_files = vec![commit_message_file("COMMITMSG summary line")];
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 160, 20);

        let mut checked = 0;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| char_at(&buf, x, y)).collect();
            let Some(col) = row.find("COMMITMSG") else {
                continue;
            };
            checked += 1;
            // Full-width prose: rendered near the left edge (small indent), not
            // pushed into the right diff column, and with no column divider.
            assert!(
                col < 8,
                "commit message should start near the left edge, got col {col} on row {y}: {row:?}"
            );
            assert!(
                !row.contains(" │ "),
                "commit message row should not have a column divider on row {y}: {row:?}"
            );
        }
        assert_eq!(
            checked, 1,
            "expected the commit message body to render exactly once, got {checked}"
        );
    }

    fn row_containing(buf: &Buffer, needle: &str) -> u16 {
        (0..buf.area.height)
            .find(|&y| {
                (0..buf.area.width)
                    .map(|x| char_at(buf, x, y))
                    .collect::<String>()
                    .contains(needle)
            })
            .unwrap_or_else(|| panic!("no row contains {needle:?}"))
    }

    #[test]
    fn should_move_divider_when_split_changes() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair("OLDSIDE", "NEWSIDE")];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        app.sbs_split_pct = 70;
        let buf = draw_sbs(&mut app, 120, 20);

        let row = row_containing(&buf, "OLDSIDE");
        let lw = app.lineno_width();
        let (left_w, right_w) = app.sbs_column_widths(118);
        assert!(left_w > right_w * 2, "{left_w} vs {right_w}");
        let divider = 1 + crate::app::sbs_left_gutter(lw) + left_w as u16 + 1;
        assert_eq!(char_at(&buf, divider, row), "▌");
        let right_start = divider + 1 + lw as u16 + 3;
        let right_text: String = (right_start..right_start + 7)
            .map(|x| char_at(&buf, x, row))
            .collect();
        assert_eq!(right_text, "NEWSIDE");
    }

    /// One hunk from `(origin, text)` rows, numbered from line 1 on each side.
    fn diff_file_with_lines(rows: &[(LineOrigin, &str)]) -> DiffFile {
        let (mut old_no, mut new_no) = (0u32, 0u32);
        let lines: Vec<DiffLine> = rows
            .iter()
            .map(|&(origin, text)| {
                let old_lineno = (origin != LineOrigin::Addition).then(|| {
                    old_no += 1;
                    old_no
                });
                let new_lineno = (origin != LineOrigin::Deletion).then(|| {
                    new_no += 1;
                    new_no
                });
                DiffLine {
                    origin,
                    content: text.to_string(),
                    old_lineno,
                    new_lineno,
                    raw_content: None,
                    highlighted_spans: None,
                }
            })
            .collect();
        let hunks = vec![DiffHunk {
            header: format!("@@ -1,{old_no} +1,{new_no} @@"),
            lines,
            old_start: 1,
            old_count: old_no,
            new_start: 1,
            new_count: new_no,
        }];
        let content_hash = DiffFile::compute_content_hash(&hunks);
        DiffFile {
            old_path: Some(PathBuf::from("src/lib.rs")),
            new_path: Some(PathBuf::from("src/lib.rs")),
            status: FileStatus::Modified,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash,
        }
    }

    /// The divider's three cells on `row`, and the x of the first.
    fn divider_cells(app: &App, buf: &Buffer, width: u16, row: u16) -> (u16, String) {
        let (left_w, _) = app.sbs_column_widths(width - 2);
        let x = 1 + crate::app::sbs_left_gutter(app.lineno_width()) + left_w as u16;
        (x, (x..x + 3).map(|cx| char_at(buf, cx, row)).collect())
    }

    /// Text of `row` over the old side (gutter and column) and the new side.
    fn side_texts(app: &App, buf: &Buffer, width: u16, row: u16) -> (String, String) {
        let (left_w, right_w) = app.sbs_column_widths(width - 2);
        let (x, _) = divider_cells(app, buf, width, row);
        let new_end = 1 + crate::app::sbs_overhead(app.lineno_width()) + (left_w + right_w) as u16;
        let text = |range: std::ops::Range<u16>| range.map(|cx| char_at(buf, cx, row)).collect();
        (text(2..x), text(x + 3..new_end))
    }

    #[test]
    fn should_join_a_changed_pair_across_the_divider() {
        let mut app = make_pr_app();
        app.ide_change_colors = false;
        app.diff_files = vec![diff_file_with_lines(&[
            (LineOrigin::Context, "BEFORE"),
            (LineOrigin::Deletion, "OLDSIDE"),
            (LineOrigin::Addition, "NEWSIDE"),
        ])];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 120, 20);

        let row = row_containing(&buf, "OLDSIDE");
        let (x, cells) = divider_cells(&app, &buf, 120, row);
        let old = app.theme.sbs_link_color(Some(app.theme.diff_del_bg), false);
        let new = app.theme.sbs_link_color(Some(app.theme.diff_add_bg), true);
        assert_eq!(cells, " ▌ ");
        assert_eq!(buf[(x, row)].bg, old);
        assert_eq!((buf[(x + 1, row)].fg, buf[(x + 1, row)].bg), (old, new));
        assert_eq!(buf[(x + 2, row)].bg, new);
        // Equal sides: nothing folds.
        let (old_text, new_text) = side_texts(&app, &buf, 120, row);
        assert!(!old_text.contains('▔') && !new_text.contains('▔'));

        // An unchanged row keeps the plain divider.
        let context = row_containing(&buf, "BEFORE");
        let (cx, cells) = divider_cells(&app, &buf, 120, context);
        assert_eq!(cells, " │ ");
        assert_ne!(buf[(cx, context)].bg, old);
    }

    #[test]
    fn should_fold_extra_deletions_into_a_rule_on_the_new_side() {
        let mut app = make_pr_app();
        app.ide_change_colors = false;
        app.diff_files = vec![diff_file_with_lines(&[
            (LineOrigin::Deletion, "GONE1"),
            (LineOrigin::Deletion, "GONE2"),
            (LineOrigin::Deletion, "GONE3"),
            (LineOrigin::Addition, "KEPT1"),
            (LineOrigin::Context, "AFTER"),
        ])];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 120, 20);
        let old = app.theme.sbs_link_color(Some(app.theme.diff_del_bg), false);

        // The paired row is a plain band.
        let paired = row_containing(&buf, "GONE1");
        assert_eq!(divider_cells(&app, &buf, 120, paired).1, " ▌ ");

        // The new side ends above GONE2: the old band steps across and
        // becomes a rule over the whole new side.
        let fold = row_containing(&buf, "GONE2");
        let (x, cells) = divider_cells(&app, &buf, 120, fold);
        assert_eq!(cells, " ▛▀");
        assert_eq!(buf[(x, fold)].bg, old);
        assert_eq!(buf[(x + 1, fold)].fg, old);
        let (_, new_text) = side_texts(&app, &buf, 120, fold);
        assert!(new_text.chars().all(|c| c == '▔'), "{new_text:?}");
        assert_eq!(buf[(x + 3, fold)].fg, old);

        // Later rows of the block hug the old side: no second rule.
        let rest = row_containing(&buf, "GONE3");
        assert_eq!(divider_cells(&app, &buf, 120, rest).1, " ▌ ");
        let (_, new_text) = side_texts(&app, &buf, 120, rest);
        assert!(new_text.trim().is_empty(), "{new_text:?}");
        assert_ne!(buf[(x + 2, rest)].bg, old);
    }

    #[test]
    fn should_fold_extra_additions_into_a_rule_on_the_old_side() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_lines(&[
            (LineOrigin::Context, "BEFORE"),
            (LineOrigin::Addition, "FRESH1"),
            (LineOrigin::Addition, "FRESH2"),
            (LineOrigin::Context, "BETWEEN"),
            (LineOrigin::Deletion, "GONE1"),
            (LineOrigin::Addition, "KEPT1"),
            (LineOrigin::Addition, "KEPT2"),
        ])];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 120, 20);
        let new = app.theme.sbs_link_color(Some(app.theme.diff_add_bg), true);

        // A pure insertion folds on its first row.
        let fold = row_containing(&buf, "FRESH1");
        let (x, cells) = divider_cells(&app, &buf, 120, fold);
        assert_eq!(cells, "▀▜ ");
        assert_eq!(buf[(x + 2, fold)].bg, new);
        let (old_text, _) = side_texts(&app, &buf, 120, fold);
        assert!(old_text.chars().all(|c| c == '▔'), "{old_text:?}");
        assert_eq!(buf[(x - 1, fold)].fg, new);
        // The cursor-indicator cell stays clear of the rule.
        assert_ne!(char_at(&buf, 1, fold), "▔");

        let rest = row_containing(&buf, "FRESH2");
        assert_eq!(divider_cells(&app, &buf, 120, rest).1, " ▐ ");
        let (old_text, _) = side_texts(&app, &buf, 120, rest);
        assert!(old_text.trim().is_empty(), "{old_text:?}");

        // After a paired row, the fold lands on the first unpaired addition.
        assert_eq!(
            divider_cells(&app, &buf, 120, row_containing(&buf, "KEPT1")).1,
            " ▌ "
        );
        assert_eq!(
            divider_cells(&app, &buf, 120, row_containing(&buf, "KEPT2")).1,
            "▀▜ "
        );
    }

    #[test]
    fn should_fold_a_wrapped_line_on_its_first_row_only() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_standalone_deletion(&"L".repeat(200))];
        app.set_diff_wrap(true);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 160, 20);

        let rows: Vec<u16> = (0..buf.area.height)
            .filter(|&y| {
                let row: String = (0..buf.area.width).map(|x| char_at(&buf, x, y)).collect();
                row.contains("LLLLLLLLLL")
            })
            .collect();
        assert!(rows.len() >= 2, "expected wrapped rows, got {rows:?}");
        assert_eq!(divider_cells(&app, &buf, 160, rows[0]).1, " ▛▀");
        for &y in &rows[1..] {
            assert_eq!(divider_cells(&app, &buf, 160, y).1, " ▌ ");
        }
    }

    #[test]
    fn should_keep_the_connector_on_the_cursor_row() {
        let mut app = make_pr_app();
        app.ide_change_colors = false;
        app.diff_files = vec![diff_file_with_pair("OLDSIDE", "NEWSIDE")];
        app.set_diff_wrap(false);
        app.rebuild_annotations();
        app.diff_state.cursor_line = app
            .line_annotations
            .iter()
            .position(|a| matches!(a, crate::app::AnnotatedLine::SideBySideLine { .. }))
            .expect("code row");

        let buf = draw_sbs(&mut app, 120, 20);

        let row = row_containing(&buf, "OLDSIDE");
        let (x, _) = divider_cells(&app, &buf, 120, row);
        // The row is tinted as the cursor line; the band is drawn over it.
        assert_eq!(buf[(x - 1, row)].bg, app.theme.cursor_line_bg);
        assert_eq!(
            buf[(x, row)].bg,
            app.theme.sbs_link_color(Some(app.theme.diff_del_bg), false)
        );
        // The caret for the new side sits on the band's last cell.
        assert_eq!(char_at(&buf, x + 2, row), "▶");
        assert_eq!(
            buf[(x + 2, row)].bg,
            app.theme.sbs_link_color(Some(app.theme.diff_add_bg), true)
        );
    }

    #[test]
    fn should_keep_the_row_colour_in_the_gutters_on_the_cursor_row() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair("OLDSIDE", "NEWSIDE")];
        app.set_diff_wrap(false);
        app.rebuild_annotations();
        app.diff_state.cursor_line = app
            .line_annotations
            .iter()
            .position(|a| matches!(a, crate::app::AnnotatedLine::SideBySideLine { .. }))
            .expect("code row");

        let buf = draw_sbs(&mut app, 120, 20);

        let row = row_containing(&buf, "OLDSIDE");
        let (x, _) = divider_cells(&app, &buf, 120, row);
        let lw = app.lineno_width() as u16;
        let modified = app.theme.diff_mod_bg();
        // The code takes the cursor-line tint on both sides.
        assert_eq!(buf[(x - 1, row)].bg, app.theme.cursor_line_bg);
        assert_eq!(buf[(x + 3 + lw + 2, row)].bg, app.theme.cursor_line_bg);
        // No `▌` marker says what happened to the line, so the gutters keep
        // saying it, from the number to the cell before the code.
        for gx in 2..2 + lw + 2 {
            assert_eq!(buf[(gx, row)].bg, modified, "old gutter, x {gx}");
        }
        for gx in x + 3..x + 3 + lw + 2 {
            assert_eq!(buf[(gx, row)].bg, modified, "new gutter, x {gx}");
        }
        // The cursor indicator's own cell is left alone.
        assert_eq!(buf[(1, row)].bg, app.theme.cursor_line_bg);
    }

    #[test]
    fn should_mark_clipped_edges_without_hiding_code() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(
            &format!("AAAA{}", "x".repeat(150)),
            &format!("BBBB{}", "y".repeat(150)),
        )];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 80, 20);
        let row = row_containing(&buf, "AAAA");
        let text: String = (0..80).map(|x| char_at(&buf, x, row)).collect();
        assert_eq!(text.matches('›').count(), 2, "{text}");
        assert!(
            !text.contains('‹'),
            "nothing is clipped on the left yet: {text}"
        );

        app.scroll_right(4);
        let buf = draw_sbs(&mut app, 80, 20);
        let row = row_containing(&buf, "xxx");
        let text: String = (0..80).map(|x| char_at(&buf, x, row)).collect();
        assert_eq!(text.matches('‹').count(), 2, "{text}");
        // The `‹` sits in the change-marker slot, so the first visible code
        // column still shows code.
        let lw = app.lineno_width();
        let left_start = 1 + crate::app::sbs_left_gutter(lw);
        assert_eq!(char_at(&buf, left_start - 1, row), "‹");
        assert_eq!(char_at(&buf, left_start, row), "x");
    }

    #[test]
    fn should_keep_hunk_header_in_place_while_panning() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(&"L".repeat(150), &"R".repeat(150))];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let _ = draw_sbs(&mut app, 80, 20);
        app.scroll_right(20);
        let buf = draw_sbs(&mut app, 80, 20);
        assert!(app.diff_state.scroll_x > 0);
        assert!(
            body_text(&buf).contains("@@ -1,1 +1,1 @@"),
            "hunk header panned away:\n{}",
            body_text(&buf)
        );
    }

    #[test]
    fn should_hit_test_and_drag_the_divider() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair("old", "new")];
        app.rebuild_annotations();
        let _ = draw_sbs(&mut app, 120, 20);

        let inner = app.diff_inner_area.expect("inner area");
        let lw = app.lineno_width();
        let (left_w, _) = app.sbs_column_widths(inner.width);
        let divider = inner.x + crate::app::sbs_left_gutter(lw) + left_w as u16 + 1;
        assert_eq!(
            app.splitter_at(divider, inner.y + 1),
            Some(crate::app::Splitter::SideBySide)
        );
        assert_eq!(app.splitter_at(inner.x + 5, inner.y + 1), None);

        let available = inner.width - crate::app::sbs_overhead(lw);
        let target = inner.x + crate::app::sbs_left_gutter(lw) + available * 3 / 10;
        app.drag_splitter_to(crate::app::Splitter::SideBySide, target);
        assert_eq!(app.sbs_split_pct, 29);
    }

    #[test]
    fn should_emphasize_only_the_changed_tokens_of_a_pair() {
        let mut app = make_pr_app();
        app.ide_change_colors = false;
        app.diff_files = vec![diff_file_with_pair(
            "return alpha * 10 + beta",
            "return alpha * 1000 + beta",
        )];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 120, 20);
        let row = row_containing(&buf, "1000");
        let text: String = (0..120).map(|x| char_at(&buf, x, row)).collect();
        let emph_add = app.theme.intraline_bg(true, false);
        let emph_del = app.theme.intraline_bg(false, false);
        let col_of = |needle: &str, from: usize| {
            text[from..]
                .find(needle)
                .map(|i| text[..from + i].chars().count() as u16)
        };
        let ten = col_of("10 ", 0).expect("old number");
        let thousand = col_of("1000", 0).expect("new number");
        let alpha_new = col_of("alpha", thousand as usize - 12).expect("new alpha");
        assert_eq!(buf[(ten, row)].bg, emph_del);
        assert_eq!(buf[(thousand, row)].bg, emph_add);
        assert_eq!(buf[(thousand + 3, row)].bg, emph_add);
        assert_ne!(
            buf[(alpha_new, row)].bg,
            emph_add,
            "unchanged token emphasized"
        );
    }

    #[test]
    fn should_colour_a_modified_pair_alike_on_both_sides() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(
            "return alpha * 10 + beta",
            "return alpha * 1000 + beta",
        )];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 120, 20);
        let row = row_containing(&buf, "1000");
        let text: String = (0..120).map(|x| char_at(&buf, x, row)).collect();
        let theme = &app.theme;
        let (modified, word) = (theme.diff_mod_bg(), theme.diff_mod_word_bg());
        let col_of = |needle: &str, from: usize| {
            text[from..]
                .find(needle)
                .map(|i| text[..from + i].chars().count() as u16)
                .unwrap_or_else(|| panic!("{needle:?} not in {text:?}"))
        };
        let ten = col_of("10 ", 0);
        let thousand = col_of("1000", 0);
        let alpha_old = col_of("alpha", 0);
        let alpha_new = col_of("alpha", thousand as usize - 12);

        // Both sides share the modified colour; only the changed number is
        // brighter.
        assert_eq!(buf[(alpha_old, row)].bg, modified);
        assert_eq!(buf[(alpha_new, row)].bg, modified);
        assert_eq!(buf[(ten, row)].bg, word);
        assert_eq!(buf[(thousand, row)].bg, word);

        // The colour runs through both line-number gutters, and no `▌`
        // marker stands between a number and its code.
        let (x, cells) = divider_cells(&app, &buf, 120, row);
        assert_eq!(buf[(2, row)].bg, modified, "old gutter");
        assert_eq!(buf[(x + 3, row)].bg, modified, "new gutter");
        assert_eq!(text.matches('▌').count(), 1, "{text:?}");

        // One band across the divider, in one colour.
        let band = theme.sbs_link_color_toward(modified, theme.diff_edit);
        assert_eq!(cells, " ▌ ");
        assert_eq!(buf[(x, row)].bg, band);
        assert_eq!((buf[(x + 1, row)].fg, buf[(x + 1, row)].bg), (band, band));
        assert_eq!(buf[(x + 2, row)].bg, band);
    }

    #[test]
    fn should_colour_one_sided_rows_by_what_happened() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_lines(&[
            (LineOrigin::Context, "BEFORE"),
            (LineOrigin::Deletion, "GONE1"),
            (LineOrigin::Context, "BETWEEN"),
            (LineOrigin::Addition, "FRESH1"),
        ])];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 120, 20);
        let theme = &app.theme;
        let col = |row: u16, needle: &str| {
            let text: String = (0..120).map(|x| char_at(&buf, x, row)).collect();
            text.find(needle)
                .map(|i| text[..i].chars().count() as u16)
                .expect("needle")
        };

        // Removed with nothing beside it: neutral, through the gutter.
        let gone = row_containing(&buf, "GONE1");
        let (x, _) = divider_cells(&app, &buf, 120, gone);
        assert_eq!(buf[(col(gone, "GONE1"), gone)].bg, theme.diff_gone_bg());
        assert_eq!(buf[(2, gone)].bg, theme.diff_gone_bg());
        assert_eq!(
            buf[(x, gone)].bg,
            theme.sbs_link_color_toward(theme.diff_gone_bg(), theme.fg_dim)
        );
        // The blank new side stays plain.
        assert_ne!(buf[(x + 3, gone)].bg, theme.diff_gone_bg());

        // Added with nothing beside it: still green.
        let fresh = row_containing(&buf, "FRESH1");
        assert_eq!(buf[(col(fresh, "FRESH1"), fresh)].bg, theme.diff_add_bg);
        assert_eq!(buf[(x + 3, fresh)].bg, theme.diff_add_bg);
        assert_ne!(buf[(2, fresh)].bg, theme.diff_add_bg);

        // Unchanged rows have no row colour.
        let context = row_containing(&buf, "BETWEEN");
        assert_ne!(buf[(2, context)].bg, theme.diff_gone_bg());
        assert_ne!(buf[(x + 3, context)].bg, theme.diff_add_bg);
    }

    #[test]
    fn should_carry_the_row_colour_through_wrapped_gutters() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(&"L".repeat(200), &"R".repeat(200))];
        app.set_diff_wrap(true);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 160, 20);
        let rows: Vec<u16> = (0..buf.area.height)
            .filter(|&y| {
                let row: String = (0..buf.area.width).map(|x| char_at(&buf, x, y)).collect();
                row.contains("LLLLLLLLLL")
            })
            .collect();
        assert!(rows.len() >= 2, "expected wrapped rows, got {rows:?}");
        let modified = app.theme.diff_mod_bg();
        for &y in &rows {
            let (x, _) = divider_cells(&app, &buf, 160, y);
            assert_eq!(buf[(2, y)].bg, modified, "old gutter, row {y}");
            assert_eq!(buf[(x + 3, y)].bg, modified, "new gutter, row {y}");
        }
    }

    #[test]
    fn should_seat_a_removed_line_beside_the_line_that_replaced_it() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_lines(&[
            (LineOrigin::Context, "BEFORE"),
            (LineOrigin::Deletion, "let total = compute(alpha, beta);"),
            (LineOrigin::Addition, "// widen the sum"),
            (LineOrigin::Addition, "let extra = 1;"),
            (
                LineOrigin::Addition,
                "let total = compute(alpha, beta, extra);",
            ),
        ])];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        // The annotations pair the removed line (hunk line 1) with the last
        // added line (hunk line 4), two rows down.
        let pairs: Vec<(Option<usize>, Option<usize>)> = app
            .line_annotations
            .iter()
            .filter_map(|a| match a {
                crate::app::AnnotatedLine::SideBySideLine {
                    del_line_idx,
                    add_line_idx,
                    ..
                } => Some((*del_line_idx, *add_line_idx)),
                _ => None,
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                (Some(0), Some(0)),
                (None, Some(2)),
                (None, Some(3)),
                (Some(1), Some(4)),
            ]
        );

        // The renderer draws the same rows.
        let buf = draw_sbs(&mut app, 140, 20);
        let old = row_containing(&buf, "beta);");
        assert_eq!(old, row_containing(&buf, "beta, extra);"));
        let comment = row_containing(&buf, "// widen the sum");
        assert_eq!(comment + 2, old);
        // The inserted lines above it fold onto the old side once, at the
        // top; the pair is a full band.
        assert_eq!(divider_cells(&app, &buf, 140, comment).1, "▀▜ ");
        assert_eq!(divider_cells(&app, &buf, 140, comment + 1).1, " ▐ ");
        assert_eq!(divider_cells(&app, &buf, 140, old).1, " ▌ ");
        let (old_text, _) = side_texts(&app, &buf, 140, comment + 1);
        assert!(old_text.trim().is_empty(), "{old_text:?}");
    }

    #[test]
    fn should_colour_by_side_when_ide_colours_are_off() {
        let mut app = make_pr_app();
        app.ide_change_colors = false;
        app.diff_files = vec![diff_file_with_pair("OLDSIDE", "NEWSIDE")];
        app.set_diff_wrap(false);
        app.rebuild_annotations();

        let buf = draw_sbs(&mut app, 120, 20);
        let row = row_containing(&buf, "OLDSIDE");
        let text: String = (0..120).map(|x| char_at(&buf, x, row)).collect();
        let col = |needle: &str| text[..text.find(needle).unwrap()].chars().count() as u16;
        assert_eq!(buf[(col("OLDSIDE"), row)].bg, app.theme.diff_del_bg);
        assert_eq!(buf[(col("NEWSIDE"), row)].bg, app.theme.diff_add_bg);
        // Markers beside plain gutters: one per side, plus the divider's.
        assert_eq!(text.matches('▌').count(), 3, "{text:?}");
        assert_ne!(buf[(2, row)].bg, app.theme.diff_del_bg);
    }

    #[test]
    fn should_keep_file_header_text_after_panning_to_the_end() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(&"L".repeat(300), &"R".repeat(300))];
        app.set_diff_wrap(false);
        app.rebuild_annotations();
        let _ = draw_sbs(&mut app, 80, 20);
        app.pan_end();
        let buf = draw_sbs(&mut app, 80, 20);
        assert!(app.diff_state.scroll_x > 100);
        assert!(
            body_text(&buf).contains("src/lib.rs [M]"),
            "file header text was overwritten:\n{}",
            body_text(&buf)
        );
    }

    #[test]
    fn should_map_mouse_columns_through_the_pan_offset() {
        let mut app = make_pr_app();
        app.diff_files = vec![diff_file_with_pair(
            &format!("{}TARGET", "x".repeat(60)),
            "short",
        )];
        app.set_diff_wrap(false);
        app.rebuild_annotations();
        let _ = draw_sbs(&mut app, 80, 20);
        app.scroll_right(60);
        let buf = draw_sbs(&mut app, 80, 20);
        let row = row_containing(&buf, "TARGET");
        let text: String = (0..80).map(|x| char_at(&buf, x, row)).collect();
        let col = text
            .find("TARGET")
            .map(|i| text[..i].chars().count())
            .unwrap() as u16;
        let point = app.cell_to_sel_point(col, row).expect("a selection point");
        assert_eq!(point.char_offset, 60, "column maps to the panned text");
    }

    fn wrapping_diff_file(count: u32) -> DiffFile {
        let lines: Vec<DiffLine> = (1..=count)
            .map(|i| DiffLine {
                origin: LineOrigin::Addition,
                content: format!("line {i} {}", "x".repeat(200)),
                old_lineno: None,
                new_lineno: Some(i),
                raw_content: None,
                highlighted_spans: None,
            })
            .collect();
        let hunks = vec![DiffHunk {
            header: format!("@@ -0,0 +1,{count} @@"),
            lines,
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: count,
        }];
        let content_hash = DiffFile::compute_content_hash(&hunks);
        DiffFile {
            old_path: None,
            new_path: Some(PathBuf::from("src/lib.rs")),
            status: FileStatus::Added,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash,
        }
    }

    #[test]
    fn should_show_comment_input_at_eof_when_lines_wrap() {
        let mut app = make_pr_app();
        app.diff_files = vec![wrapping_diff_file(40)];
        app.set_diff_wrap(true);
        app.rebuild_annotations();
        let _ = draw_sbs(&mut app, 160, 20);
        app.jump_to_bottom();
        app.enter_comment_mode(false, Some((40, LineSide::New)));
        let buf = draw_sbs(&mut app, 160, 20);
        let body = body_text(&buf);
        assert!(
            body.contains("Type your comment..."),
            "expected the comment input box to be visible in:\n{body}"
        );
    }

    /// Context at old 40-41 / new 50-51, a deleted line 42 replaced by new 52,
    /// then context at old 43 / new 53.
    fn relative_numbering_file() -> DiffFile {
        let line = |origin, content: &str, old_lineno, new_lineno| DiffLine {
            origin,
            content: content.to_string(),
            old_lineno,
            new_lineno,
            raw_content: None,
            highlighted_spans: None,
        };
        let lines = vec![
            line(LineOrigin::Context, "alpha", Some(40), Some(50)),
            line(LineOrigin::Context, "bravo", Some(41), Some(51)),
            line(LineOrigin::Deletion, "charlie", Some(42), None),
            line(LineOrigin::Addition, "delta", None, Some(52)),
            line(LineOrigin::Context, "echo", Some(43), Some(53)),
        ];
        let hunks = vec![DiffHunk {
            header: "@@ -40,4 +50,4 @@".to_string(),
            lines,
            old_start: 40,
            old_count: 4,
            new_start: 50,
            new_count: 4,
        }];
        let content_hash = DiffFile::compute_content_hash(&hunks);
        DiffFile {
            old_path: Some(PathBuf::from("src/lib.rs")),
            new_path: Some(PathBuf::from("src/lib.rs")),
            status: FileStatus::Modified,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash,
        }
    }

    /// Numbers printed on the screen row of `annotation`. Content words in
    /// `relative_numbering_file` have no digits, so these are the gutters.
    fn gutter_numbers(app: &App, buffer: &Buffer, annotation: usize) -> Vec<u32> {
        let inner = app.diff_inner_area.unwrap();
        let row = app
            .diff_row_to_annotation
            .iter()
            .position(|&idx| idx == annotation)
            .unwrap();
        let text: String = (inner.x..inner.x + inner.width)
            .map(|x| buffer[(x, inner.y + row as u16)].symbol().to_string())
            .collect();
        text.split_whitespace()
            .filter_map(|token| token.parse().ok())
            .collect()
    }

    #[test]
    fn should_show_absolute_numbers_on_cursor_row_with_relative_numbers() {
        let mut app = make_pr_app_with(vec![relative_numbering_file()]);
        app.relative_line_numbers = true;
        app.rebuild_annotations();
        let find = |app: &App, old: u32, new: u32| {
            app.line_annotations
                .iter()
                .position(|annotation| {
                    matches!(
                        annotation,
                        crate::app::AnnotatedLine::SideBySideLine { old_lineno, new_lineno, .. }
                            if *old_lineno == Some(old) && *new_lineno == Some(new)
                    )
                })
                .unwrap()
        };
        let alpha = find(&app, 40, 50);
        let bravo = find(&app, 41, 51);
        let changed = find(&app, 42, 52);
        let echo = find(&app, 43, 53);

        // Each gutter shows the cursor row's own old/new number.
        app.diff_state.cursor_line = changed;
        let buffer = draw_sbs(&mut app, 160, 20);
        assert_eq!(gutter_numbers(&app, &buffer, changed), vec![42, 52]);
        assert_eq!(gutter_numbers(&app, &buffer, bravo), vec![1, 1]);
        assert_eq!(gutter_numbers(&app, &buffer, alpha), vec![2, 2]);
        assert_eq!(gutter_numbers(&app, &buffer, echo), vec![1, 1]);

        app.diff_state.cursor_line = bravo;
        let buffer = draw_sbs(&mut app, 160, 20);
        assert_eq!(gutter_numbers(&app, &buffer, bravo), vec![41, 51]);
        assert_eq!(gutter_numbers(&app, &buffer, changed), vec![1, 1]);
    }
}
