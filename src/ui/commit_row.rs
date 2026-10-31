//! Shared commit-row rendering used by both the fullscreen review-target
//! selector and the inline commit selector shown above the diff. Keeps the
//! row layout (cursor arrow, range bar, checkbox, reviewed marker, hash,
//! branch chip, summary, author/date) consistent across surfaces.

use chrono::{DateTime, Utc};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{MERGE_SELECTION_ID, STAGED_SELECTION_ID, UNSTAGED_SELECTION_ID};
use crate::theme::Theme;
use crate::ui::styles;
use crate::ui::text_utils::truncate_or_pad;
use crate::vcs::CommitInfo;
use crate::vcs::traits::RefKind;

pub const CURSOR_GLYPH: &str = "\u{25b8}"; // ▸
pub const RANGE_BAR_GLYPH: &str = "\u{258c}"; // ▌
pub const SELECTED_BOX_GLYPH: &str = "\u{25a3}"; // ▣
pub const UNSELECTED_BOX_GLYPH: &str = "\u{25a2}"; // ▢
pub const REVIEWED_GLYPH: &str = "\u{2713}"; // ✓
pub const REVIEWED_LABEL: &str = "✓ ";

/// Per-list column widths so author/date land at the same x across every
/// row. Branch column gets `[branch_name]` padded to width including
/// brackets and a trailing space; rows without a branch render the same
/// number of blanks. Defaults match a ~120-column terminal; wide terminals
/// grow the flexible columns via [`CommitRowLayout::fit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitRowLayout {
    pub branch_width: usize,
    pub summary_width: usize,
    pub author_width: usize,
}

impl Default for CommitRowLayout {
    fn default() -> Self {
        Self {
            branch_width: 16,
            summary_width: 50,
            author_width: 12,
        }
    }
}

// Sane maxima for the flexible columns on very wide terminals.
const MAX_SUMMARY_WIDTH: usize = 100;
const MAX_BRANCH_WIDTH: usize = 24;
const MAX_AUTHOR_WIDTH: usize = 20;
// cursor(2) + range bar(2) + checkbox(2) + hash(8) + time slot(~12)
const FIXED_ROW_WIDTH: usize = 26;

impl CommitRowLayout {
    /// Size the columns to the rows that will be drawn: the branch column
    /// is exactly as wide as the widest row's label chips (capped so a wall
    /// of labels cannot squeeze the summary below its floor, and empty when
    /// no row has any), the author grows to its maximum, and the summary
    /// takes every remaining column so nothing is left unused on the right.
    pub fn fit_rows(commits: &[CommitInfo], total_width: usize, graph_width: usize) -> Self {
        let needed_branch = commits.iter().map(chip_columns).max().unwrap_or(0);
        let base = Self::default();
        let available = total_width.saturating_sub(FIXED_ROW_WIDTH + graph_width);
        let branch_cap = MAX_BRANCH_WIDTH.max(available * 2 / 5);
        let branch_width = needed_branch.min(branch_cap);
        let mut remaining = available.saturating_sub(branch_width);

        // Floors first so narrow terminals degrade like the fixed layout.
        let mut author_width = base.author_width.min(remaining);
        remaining -= author_width;
        let mut summary_width = base.summary_width.min(remaining);
        remaining -= summary_width;
        // Then the author up to its maximum, and everything else to the summary.
        let author_extra = MAX_AUTHOR_WIDTH.saturating_sub(author_width).min(remaining);
        author_width += author_extra;
        remaining -= author_extra;
        summary_width += remaining;

        Self {
            branch_width,
            summary_width: summary_width.max(base.summary_width),
            author_width: author_width.max(base.author_width),
        }
    }

    /// Distribute `total_width` (the row area) across the flexible
    /// columns without looking at the rows: floors are the defaults, spare
    /// width grows the summary first, then the branch chip, then the
    /// author, up to sane maxima. [`fit_rows`](Self::fit_rows) is the
    /// content-aware version the renderers use.
    pub fn fit(total_width: usize, graph_width: usize) -> Self {
        let mut layout = Self::default();
        let used = FIXED_ROW_WIDTH
            + graph_width
            + layout.branch_width
            + layout.summary_width
            + layout.author_width;
        let mut extra = total_width.saturating_sub(used);

        fn grow(width: &mut usize, max: usize, extra: &mut usize) {
            let inc = max.saturating_sub(*width).min(*extra);
            *width += inc;
            *extra -= inc;
        }
        grow(&mut layout.summary_width, MAX_SUMMARY_WIDTH, &mut extra);
        grow(&mut layout.branch_width, MAX_BRANCH_WIDTH, &mut extra);
        grow(&mut layout.author_width, MAX_AUTHOR_WIDTH, &mut extra);
        layout
    }
}

pub struct CommitRowSpec<'a> {
    pub commit: &'a CommitInfo,
    pub is_cursor: bool,
    pub is_selected: bool,
    /// The row's diff is what the review shows right now (log pane);
    /// drawn with the range bar like the inline selector's selected range.
    pub is_loaded: bool,
    pub is_reviewed: bool,
    pub theme: &'a Theme,
    /// Pre-rendered commit-graph gutter cells (two chars per lane), shared
    /// width across all rows of a list. `None` hides the gutter (inline
    /// selector, lists without parent data).
    pub graph: Option<Vec<Span<'static>>>,
    /// Column widths shared by every row of the list.
    pub layout: CommitRowLayout,
}

/// Label chips for the branch column: every ref on the commit (local
/// branches, then remotes, then tags), each styled by kind, and a `HEAD`
/// chip on a detached head. The current branch's chip is bold. Chips that
/// do not fit are dropped behind an ellipsis and the column is padded to
/// `width` so the summary stays aligned.
/// What a label chip stands for, so the layout can measure chips without a
/// theme and the renderer can style them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChipKind {
    /// Detached HEAD marker.
    Head,
    Local,
    Remote,
    Tag,
}

/// The chips a commit row shows in its branch column, in order: the
/// checked-out branch as `[HEAD → name]` (or `[HEAD]` when detached), other
/// local branches `[name]`, remote-tracking branches `[origin/name]`, and
/// tags `<name>`, so kind reads without colour too.
fn chip_labels(commit: &CommitInfo) -> Vec<(String, ChipKind)> {
    let mut chips = Vec::new();
    if commit.is_head && !commit.refs.iter().any(|r| r.kind == RefKind::Head) {
        chips.push(("[HEAD]".to_string(), ChipKind::Head));
    }
    if commit.refs.is_empty() {
        if let Some(name) = &commit.branch_name {
            chips.push((format!("[{name}]"), ChipKind::Local));
        }
    } else {
        for label in &commit.refs {
            let (text, kind) = match label.kind {
                RefKind::Head => (format!("[HEAD \u{2192} {}]", label.name), ChipKind::Head),
                RefKind::Local => (format!("[{}]", label.name), ChipKind::Local),
                RefKind::Remote => (format!("[{}]", label.name), ChipKind::Remote),
                RefKind::Tag => (format!("<{}>", label.name), ChipKind::Tag),
            };
            chips.push((text, kind));
        }
    }
    chips
}

/// Columns the commit's chips need to show in full: each chip plus the
/// space after it, so the summary never butts against the last label.
pub fn chip_columns(commit: &CommitInfo) -> usize {
    chip_labels(commit)
        .iter()
        .map(|(text, _)| text.chars().count() + 1)
        .sum()
}

fn ref_chip_spans<'a>(spec: &CommitRowSpec<'a>, width: usize) -> Vec<Span<'a>> {
    let theme = spec.theme;
    let commit = spec.commit;
    let head_style = styles::branch_style(theme).add_modifier(Modifier::BOLD);
    let chips: Vec<(String, Style)> = chip_labels(commit)
        .into_iter()
        .map(|(text, kind)| {
            let style = match kind {
                ChipKind::Head => head_style,
                ChipKind::Local => styles::branch_style(theme),
                ChipKind::Remote => Style::default().fg(theme.fg_dim),
                ChipKind::Tag => styles::pseudo_commit_tag_style(theme),
            };
            (text, style)
        })
        .collect();

    let mut spans: Vec<Span<'a>> = Vec::with_capacity(chips.len() + 1);
    let mut used = 0usize;
    for (i, (text, style)) in chips.iter().enumerate() {
        let text_width = text.chars().count();
        let remaining = width.saturating_sub(used);
        if text_width < remaining {
            spans.push(Span::styled(text.clone(), *style));
            spans.push(Span::raw(" "));
            used += text_width + 1;
        } else if i == 0 {
            // The first chip always shows, cut to the column with an
            // ellipsis and still followed by its space.
            let room = width.saturating_sub(1);
            let chip: String = if room == 0 {
                String::new()
            } else {
                let mut cut: String = text.chars().take(room - 1).collect();
                cut.push('\u{2026}');
                cut
            };
            spans.push(Span::styled(truncate_or_pad(&chip, room), *style));
            if width > 0 {
                spans.push(Span::raw(" "));
            }
            used = width;
            break;
        } else {
            if remaining >= 1 {
                spans.push(Span::styled(
                    "\u{2026}".to_string(),
                    Style::default().fg(theme.fg_dim),
                ));
                used += 1;
            }
            break;
        }
    }
    if used < width {
        spans.push(Span::raw(" ".repeat(width - used)));
    }
    spans
}

pub fn render_commit_row<'a>(spec: &CommitRowSpec<'a>) -> Line<'a> {
    let theme = spec.theme;

    let row_text_style = if spec.is_cursor {
        styles::selected_style(theme)
    } else if spec.is_selected {
        Style::default().fg(theme.fg_secondary)
    } else {
        Style::default().fg(theme.fg_primary)
    };

    let mut spans: Vec<Span<'a>> = Vec::with_capacity(11);
    spans.push(Span::styled(
        if spec.is_cursor {
            format!("{CURSOR_GLYPH} ")
        } else {
            "  ".to_string()
        },
        styles::selection_marker_style(theme, row_text_style),
    ));
    spans.push(Span::styled(
        if spec.is_selected || spec.is_loaded {
            format!("{RANGE_BAR_GLYPH} ")
        } else {
            "  ".to_string()
        },
        styles::range_bar_style(theme),
    ));
    spans.push(Span::styled(
        if spec.is_selected {
            format!("{SELECTED_BOX_GLYPH} ")
        } else {
            format!("{UNSELECTED_BOX_GLYPH} ")
        },
        if spec.is_selected {
            styles::reviewed_style(theme)
        } else {
            styles::pending_style(theme)
        },
    ));
    spans.push(Span::styled(
        if spec.is_reviewed {
            REVIEWED_LABEL.to_string()
        } else {
            " ".repeat(REVIEWED_LABEL.chars().count())
        },
        if spec.is_reviewed {
            styles::reviewed_style(theme)
        } else {
            row_text_style
        },
    ));

    // Graph gutter sits between the checkbox and the hash so the track
    // column lines up across every row (pseudo rows get blank cells).
    if let Some(graph) = &spec.graph {
        spans.extend(graph.iter().cloned());
    }

    if spec.commit.id == MERGE_SELECTION_ID {
        spans.push(Span::styled(
            " \u{00b7} conflicts \u{00b7} ",
            styles::error_inline_style(theme),
        ));
        spans.push(Span::styled(spec.commit.summary.clone(), row_text_style));
        return Line::from(spans);
    }

    if spec.commit.id == STAGED_SELECTION_ID || spec.commit.id == UNSTAGED_SELECTION_ID {
        let tag = if spec.commit.id == STAGED_SELECTION_ID {
            " \u{00b7} staged \u{00b7}   "
        } else {
            " \u{00b7} unstaged \u{00b7} "
        };
        spans.push(Span::styled(tag, styles::pseudo_commit_tag_style(theme)));
        spans.push(Span::styled(spec.commit.summary.clone(), row_text_style));
        return Line::from(spans);
    }

    spans.push(Span::styled(
        format!("{} ", spec.commit.short_id),
        styles::hash_style(theme),
    ));

    // Branch column: always layout.branch_width cells. With a branch we
    // render `[<name>]` padded out with trailing spaces; without one we
    // render the same number of blanks so the summary column starts at the
    // same x.
    let layout = spec.layout;
    spans.extend(ref_chip_spans(spec, layout.branch_width));

    spans.push(Span::styled(
        truncate_or_pad(&spec.commit.summary, layout.summary_width),
        row_text_style,
    ));

    let when = format_relative_short(&spec.commit.time);
    spans.push(Span::styled(
        format!(
            "  {} \u{00b7} {}",
            truncate_or_pad(&spec.commit.author, layout.author_width),
            when
        ),
        Style::default().fg(theme.fg_secondary),
    ));

    Line::from(spans)
}

/// Compact relative time used in selector rows: `5m`, `3h`, `2d`, `6w`, `4mo`,
/// `2y`, or `just now`. Mirrors `format_relative_time` in `selector.rs` but
/// without the trailing "ago" so rows stay tight.
pub fn format_relative_short(time: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let delta = now.signed_duration_since(*time);
    if delta.num_seconds() < 60 {
        return "just now".to_string();
    }
    let mins = delta.num_minutes();
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = delta.num_hours();
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = delta.num_days();
    if days < 7 {
        return format!("{days}d");
    }
    if days < 30 {
        return format!("{}w", days / 7);
    }
    if days < 365 {
        return format!("{}mo", days / 30);
    }
    format!("{}y", days / 365)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn commit(id: &str, summary: &str, branch: Option<&str>) -> CommitInfo {
        CommitInfo {
            id: id.to_string(),
            short_id: id[..7.min(id.len())].to_string(),
            branch_name: branch.map(|s| s.to_string()),
            parent_ids: Vec::new(),
            refs: Vec::new(),
            is_head: false,
            summary: summary.to_string(),
            body: None,
            author: "alice".to_string(),
            time: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn should_render_cursor_arrow_when_is_cursor() {
        // given
        let theme = Theme::dark();
        let c = commit("abc1234", "Add feature", Some("main"));
        // when
        let line = render_commit_row(&CommitRowSpec {
            commit: &c,
            is_cursor: true,
            is_selected: false,
            is_loaded: false,
            is_reviewed: false,
            theme: &theme,
            graph: None,
            layout: CommitRowLayout::default(),
        });
        // then
        let text = line_text(&line);
        assert!(text.starts_with(CURSOR_GLYPH), "got: {text:?}");
    }

    #[test]
    fn should_render_range_bar_when_selected() {
        // given
        let theme = Theme::dark();
        let c = commit("abc1234", "Add feature", None);
        // when
        let line = render_commit_row(&CommitRowSpec {
            commit: &c,
            is_cursor: false,
            is_selected: true,
            is_loaded: false,
            is_reviewed: false,
            theme: &theme,
            graph: None,
            layout: CommitRowLayout::default(),
        });
        // then
        let text = line_text(&line);
        assert!(text.contains(RANGE_BAR_GLYPH), "got: {text:?}");
        assert!(text.contains(SELECTED_BOX_GLYPH), "got: {text:?}");
    }

    #[test]
    fn should_render_empty_box_when_not_selected() {
        // given
        let theme = Theme::dark();
        let c = commit("abc1234", "Add feature", None);
        // when
        let line = render_commit_row(&CommitRowSpec {
            commit: &c,
            is_cursor: false,
            is_selected: false,
            is_loaded: false,
            is_reviewed: false,
            theme: &theme,
            graph: None,
            layout: CommitRowLayout::default(),
        });
        // then
        let text = line_text(&line);
        assert!(!text.contains(RANGE_BAR_GLYPH), "got: {text:?}");
        assert!(text.contains(UNSELECTED_BOX_GLYPH), "got: {text:?}");
    }

    #[test]
    fn should_render_reviewed_marker_when_commit_was_already_reviewed() {
        // given
        let theme = Theme::dark();
        let c = commit("abc1234", "Add feature", None);
        // when
        let line = render_commit_row(&CommitRowSpec {
            commit: &c,
            is_cursor: false,
            is_selected: false,
            is_loaded: false,
            is_reviewed: true,
            theme: &theme,
            graph: None,
            layout: CommitRowLayout::default(),
        });
        // then
        let text = line_text(&line);
        assert!(text.contains(REVIEWED_LABEL), "got: {text:?}");
    }

    #[test]
    fn should_render_pseudo_commit_with_tag_and_drop_metadata() {
        // given
        let theme = Theme::dark();
        let c = commit(STAGED_SELECTION_ID, "Staged changes", None);
        // when
        let line = render_commit_row(&CommitRowSpec {
            commit: &c,
            is_cursor: false,
            is_selected: false,
            is_loaded: false,
            is_reviewed: false,
            theme: &theme,
            graph: None,
            layout: CommitRowLayout::default(),
        });
        // then
        let text = line_text(&line);
        assert!(text.contains("staged"), "got: {text:?}");
        assert!(text.contains("Staged changes"), "got: {text:?}");
        assert!(!text.contains("alice"), "should drop author: {text:?}");
    }

    #[test]
    fn should_render_branch_chip_when_present() {
        // given
        let theme = Theme::dark();
        let c = commit("abc1234", "Add feature", Some("feat/foo"));
        // when
        let line = render_commit_row(&CommitRowSpec {
            commit: &c,
            is_cursor: false,
            is_selected: false,
            is_loaded: false,
            is_reviewed: false,
            theme: &theme,
            graph: None,
            layout: CommitRowLayout::default(),
        });
        // then
        let text = line_text(&line);
        assert!(text.contains("[feat/foo]"), "got: {text:?}");
    }

    #[test]
    fn should_render_graph_gutter_between_checkbox_and_hash() {
        // given
        let theme = Theme::dark();
        let c = commit("abc1234", "Add feature", None);
        // when
        let line = render_commit_row(&CommitRowSpec {
            commit: &c,
            is_cursor: false,
            is_selected: false,
            is_loaded: false,
            is_reviewed: false,
            theme: &theme,
            graph: Some(vec![Span::raw("\u{25cf} ")]),
            layout: CommitRowLayout::default(),
        });
        // then the node glyph lands immediately before the hash
        let text = line_text(&line);
        assert!(text.contains("\u{25cf} abc1234"), "got: {text:?}");
    }

    #[test]
    fn should_keep_default_layout_on_narrow_terminals() {
        // given terminals at or below the historical break-even width
        // (fixed columns + defaults + a 2-lane gutter = 108 cells)
        for width in [60, 100, 108] {
            let layout = CommitRowLayout::fit(width, 4);
            // then the defaults are the floor — columns never shrink
            assert_eq!(layout, CommitRowLayout::default(), "width {width}");
        }
    }

    #[test]
    fn should_grow_summary_first_then_branch_then_author_when_wide() {
        // given a fullscreen-wide terminal
        let layout = CommitRowLayout::fit(220, 4);
        // then summary takes the spare width first, capped at its maximum
        assert_eq!(layout.summary_width, 100);
        assert!(layout.branch_width > CommitRowLayout::default().branch_width);
        // and nothing exceeds the sane maxima
        assert!(layout.branch_width <= 24);
        assert!(layout.author_width <= 20);
    }

    #[test]
    fn fit_rows_sizes_the_branch_column_to_the_widest_labels() {
        // given rows whose chips need more than the fixed cap
        let mut tagged = commit("abc1234", "Add feature", Some("main"));
        tagged.refs = vec![
            crate::vcs::traits::RefLabel {
                name: "main".to_string(),
                kind: RefKind::Local,
            },
            crate::vcs::traits::RefLabel {
                name: "origin/main".to_string(),
                kind: RefKind::Remote,
            },
            crate::vcs::traits::RefLabel {
                name: "v1.2.3".to_string(),
                kind: RefKind::Tag,
            },
        ];
        let plain = commit("def5678", "Plain", None);
        let needed = chip_columns(&tagged);
        assert_eq!(needed, "[main] [origin/main] <v1.2.3> ".len());

        // when the terminal is wide
        let layout = CommitRowLayout::fit_rows(&[tagged.clone(), plain.clone()], 200, 4);

        // then the chips fit in full and no width is left over on the right
        assert_eq!(layout.branch_width, needed);
        assert_eq!(layout.author_width, 20);
        assert_eq!(
            FIXED_ROW_WIDTH + 4 + layout.branch_width + layout.summary_width + layout.author_width,
            200
        );

        // and rows without any label get no branch column at all
        let bare = CommitRowLayout::fit_rows(&[plain], 200, 4);
        assert_eq!(bare.branch_width, 0);
        assert!(bare.summary_width > layout.summary_width);

        // while a narrow terminal keeps the floors instead of shrinking
        let narrow = CommitRowLayout::fit_rows(&[tagged], 80, 4);
        assert!(narrow.summary_width >= CommitRowLayout::default().summary_width);
        assert!(narrow.author_width >= CommitRowLayout::default().author_width);
    }

    #[test]
    fn should_cap_all_columns_at_maxima_on_ultrawide() {
        // given an absurdly wide terminal
        let layout = CommitRowLayout::fit(500, 0);
        // then
        assert_eq!(layout.summary_width, 100);
        assert_eq!(layout.branch_width, 24);
        assert_eq!(layout.author_width, 20);
    }

    #[test]
    fn should_format_short_relative_time_buckets() {
        // given
        let now = Utc::now();
        // when / then
        assert_eq!(format_relative_short(&now), "just now");
        assert_eq!(
            format_relative_short(&(now - chrono::Duration::minutes(5))),
            "5m"
        );
        assert_eq!(
            format_relative_short(&(now - chrono::Duration::hours(3))),
            "3h"
        );
        assert_eq!(
            format_relative_short(&(now - chrono::Duration::days(2))),
            "2d"
        );
        assert_eq!(
            format_relative_short(&(now - chrono::Duration::days(20))),
            "2w"
        );
        assert_eq!(
            format_relative_short(&(now - chrono::Duration::days(60))),
            "2mo"
        );
        assert_eq!(
            format_relative_short(&(now - chrono::Duration::days(800))),
            "2y"
        );
    }
}
