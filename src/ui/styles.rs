use ratatui::style::{Color, Modifier, Style};

use crate::model::LineOrigin;
use crate::theme::Theme;

pub fn selected_style(theme: &Theme) -> Style {
    Style::default().bg(theme.bg_highlight).fg(theme.fg_primary)
}

pub fn dim_style(theme: &Theme) -> Style {
    Style::default().fg(theme.fg_dim)
}

pub fn diff_add_style(theme: &Theme) -> Style {
    Style::default().fg(theme.diff_add).bg(theme.diff_add_bg)
}

pub fn diff_del_style(theme: &Theme) -> Style {
    Style::default().fg(theme.diff_del).bg(theme.diff_del_bg)
}

pub fn diff_context_style(theme: &Theme) -> Style {
    Style::default().fg(theme.diff_context)
}

/// Gutter marker for a line changed in `$EDITOR` this session.
pub fn diff_edit_marker_style(theme: &Theme) -> Style {
    Style::default().fg(theme.diff_edit).bg(theme.diff_edit_bg)
}

/// A span on a line changed in `$EDITOR`: the add / context background gives
/// way to `diff_edit_bg`, and added-word emphasis to its edit-coloured
/// counterpart. The foreground (syntax colours) stays, and so does a search
/// match background.
pub fn diff_edit_span_style(style: Style, theme: &Theme) -> Style {
    match style.bg {
        None | Some(Color::Reset) => style.bg(theme.diff_edit_bg),
        Some(bg) if bg == theme.diff_add_bg || bg == theme.syntax_add_bg => {
            style.bg(theme.diff_edit_bg)
        }
        Some(bg)
            if bg == theme.intraline_bg(true, true) || bg == theme.intraline_bg(true, false) =>
        {
            style.bg(theme.edit_intraline_bg())
        }
        Some(_) => style,
    }
}

/// Live background a syntax-highlighted diff span should show for a line of
/// the given origin. `DiffLine.highlighted_spans` bakes in whatever
/// `syntax_add_bg`/`syntax_del_bg` was active when the diff was loaded (or
/// last rehighlighted); this lets renderers override that per-frame from the
/// currently active theme instead of trusting the cached span's own
/// background, so add/del backgrounds never lag behind the active theme even
/// if a rehighlight was skipped or missed. `None` for context lines, which
/// never carry a baked-in background (`syntax::apply_diff_background`
/// intentionally excludes them).
pub fn diff_syntax_bg(theme: &Theme, origin: LineOrigin) -> Option<Color> {
    match origin {
        LineOrigin::Addition => Some(theme.syntax_add_bg),
        LineOrigin::Deletion => Some(theme.syntax_del_bg),
        LineOrigin::Context => None,
    }
}

/// Apply `diff_syntax_bg` to `style`, leaving it untouched when there is no
/// live override for `origin` (context lines).
pub fn patch_highlighted_span_bg(style: Style, theme: &Theme, origin: LineOrigin) -> Style {
    match diff_syntax_bg(theme, origin) {
        Some(bg) => style.bg(bg),
        None => style,
    }
}

pub fn expanded_context_style(theme: &Theme) -> Style {
    Style::default().fg(theme.expanded_context_fg)
}

pub fn diff_hunk_header_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.fg_dim)
        .bg(theme.section_highlight_bg())
}

pub fn file_header_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.fg_primary)
        .add_modifier(Modifier::BOLD)
}

pub fn reviewed_style(theme: &Theme) -> Style {
    Style::default().fg(theme.reviewed)
}

pub fn pending_style(theme: &Theme) -> Style {
    Style::default().fg(theme.pending)
}

pub fn border_style(theme: &Theme, focused: bool) -> Style {
    if focused {
        Style::default().fg(theme.border_focused)
    } else {
        Style::default().fg(theme.border_unfocused)
    }
}

pub fn panel_style(theme: &Theme) -> Style {
    Style::default().bg(theme.panel_bg).fg(theme.fg_primary)
}

pub fn popup_style(theme: &Theme) -> Style {
    panel_style(theme)
}

pub fn status_bar_style(theme: &Theme) -> Style {
    Style::default()
        .bg(theme.status_bar_bg)
        .fg(theme.fg_primary)
}

pub fn mode_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.mode_fg)
        .bg(theme.mode_bg)
        .add_modifier(Modifier::BOLD)
}

pub fn file_status_style(theme: &Theme, status: char) -> Style {
    let color = match status {
        'A' => theme.file_added,
        'M' => theme.file_modified,
        'D' => theme.file_deleted,
        'R' => theme.file_renamed,
        _ => theme.fg_secondary,
    };
    Style::default().fg(color)
}

/// Override only the marker foreground, preserving the caller's legacy style.
pub fn selection_marker_style(theme: &Theme, fallback: Style) -> Style {
    match theme.selection_marker {
        Some(color) => fallback.fg(color),
        None => fallback,
    }
}

pub fn current_line_indicator_style(theme: &Theme) -> Style {
    selection_marker_style(theme, Style::default().fg(theme.border_focused))
}

pub fn hash_style(theme: &Theme) -> Style {
    Style::default().fg(theme.cursor_color)
}

pub fn branch_style(theme: &Theme) -> Style {
    Style::default().fg(theme.branch_name)
}

/// Remote-tracking branches (`origin/main`): the theme's renamed-file
/// colour, purple in most themes like an IDE's remote labels, so they
/// stand apart from local branches. A theme that already spends that colour
/// on local branches gets its added-file colour instead.
pub fn remote_branch_style(theme: &Theme) -> Style {
    let color = if theme.file_renamed == theme.branch_name {
        theme.file_added
    } else {
        theme.file_renamed
    };
    Style::default().fg(color)
}

pub fn dir_icon_style(theme: &Theme) -> Style {
    Style::default().fg(theme.diff_hunk_header)
}

pub fn comment_type_style(_theme: &Theme, color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub fn comment_border_style(theme: &Theme, _color: Color) -> Style {
    // Match the file-header separator look so the comment box reads as a
    // structural divider rather than as colour-coded chrome. The comment-
    // type colour still lives on the [NOTE]/[ISSUE]/... label inside.
    file_header_style(theme)
}

/// Fixed palette used to tint comment chrome by author. Excludes red/green so
/// the colour never collides with diff add/del semantics. Cyan/yellow/magenta/
/// blue/light-magenta/light-cyan give us six visually distinct slots — enough
/// for a handful of agents alongside the human reviewer.
const AUTHOR_PALETTE: &[Color] = &[
    Color::Cyan,
    Color::Yellow,
    Color::Magenta,
    Color::Blue,
    Color::LightMagenta,
    Color::LightCyan,
];

/// Deterministic palette colour for a given author name. Always returns
/// a colour; callers gate visibility separately via [`author_accent`].
/// Hashes via FNV-1a so the mapping is platform-independent and survives
/// across runs.
pub fn author_color_for(author: &str) -> Color {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in author.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    AUTHOR_PALETTE[(hash as usize) % AUTHOR_PALETTE.len()]
}

/// `Some(colour)` when `author` differs from the viewer (so the comment
/// chrome should advertise authorship), `None` when the comment is the
/// viewer's own.
pub fn author_accent(viewer: &str, author: &str) -> Option<Color> {
    if author == viewer {
        None
    } else {
        Some(author_color_for(author))
    }
}

/// Border style for a comment box, tinted by author when the comment is not
/// from the current viewer. Falls back to the neutral header style otherwise.
pub fn comment_border_style_for_author(theme: &Theme, viewer: &str, author: &str) -> Style {
    match author_accent(viewer, author) {
        Some(color) => Style::default().fg(color).add_modifier(Modifier::BOLD),
        None => file_header_style(theme),
    }
}

pub fn visual_selection_style(theme: &Theme) -> Style {
    Style::default().bg(theme.bg_highlight)
}

pub fn search_match_style(theme: &Theme) -> Style {
    Style::default().bg(theme.search_match_bg)
}

pub fn help_indicator_style(theme: &Theme) -> Style {
    Style::default().fg(theme.help_indicator).bg(theme.panel_bg)
}

pub fn range_bar_style(theme: &Theme) -> Style {
    current_line_indicator_style(theme)
}

pub fn error_inline_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.message_error_fg)
        .add_modifier(Modifier::BOLD)
}

pub fn pseudo_commit_tag_style(theme: &Theme) -> Style {
    Style::default().fg(theme.file_modified)
}

/// Fixed palette for commit-graph tracks. Like [`AUTHOR_PALETTE`], plain
/// ANSI slots so every theme/terminal palette renders distinct tracks;
/// red is excluded to avoid reading as an error/deletion.
const GRAPH_TRACK_PALETTE: &[Color] = &[
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Blue,
    Color::Green,
    Color::LightMagenta,
    Color::LightCyan,
    Color::LightBlue,
];

/// Style for a commit-graph track by palette slot (wraps around).
pub fn graph_track_style(_theme: &Theme, color_index: usize) -> Style {
    Style::default().fg(GRAPH_TRACK_PALETTE[color_index % GRAPH_TRACK_PALETTE.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edited_spans_trade_add_colours_for_edit_ones() {
        let theme = Theme::dark();
        let bg = |bg: Color| diff_edit_span_style(Style::default().bg(bg), &theme).bg;
        assert_eq!(bg(theme.syntax_add_bg), Some(theme.diff_edit_bg));
        // Added-word emphasis stays emphasised, in the edit colour.
        let emphasis = theme.edit_intraline_bg();
        assert_ne!(emphasis, theme.diff_edit_bg);
        assert_eq!(bg(theme.intraline_bg(true, true)), Some(emphasis));
        assert_eq!(bg(theme.intraline_bg(true, false)), Some(emphasis));
        // A search match keeps its own background.
        assert_eq!(bg(theme.search_match_bg), Some(theme.search_match_bg));
    }

    #[test]
    fn remote_branches_never_share_the_local_branch_colour() {
        // given themes whose renamed-file colour differs from, and matches,
        // their branch colour
        for theme in [Theme::dark(), Theme::github_dark_colorblind()] {
            // then remote-tracking branches still read apart from local ones
            assert_ne!(
                remote_branch_style(&theme).fg,
                branch_style(&theme).fg,
                "{:?}",
                theme.branch_name
            );
        }
    }

    #[test]
    fn selection_marker_changes_only_marker_foregrounds() {
        let mut theme = Theme::github_dark();
        let legacy_pointer = selected_style(&theme);
        assert_eq!(
            selection_marker_style(&theme, legacy_pointer),
            legacy_pointer
        );
        assert_eq!(
            current_line_indicator_style(&theme).fg,
            Some(theme.border_focused)
        );
        assert_eq!(range_bar_style(&theme).fg, Some(theme.border_focused));
        let unchanged = (
            border_style(&theme, true),
            hash_style(&theme),
            selected_style(&theme),
            visual_selection_style(&theme),
        );

        theme.selection_marker = Some(Color::Rgb(36, 45, 56));
        assert_eq!(
            current_line_indicator_style(&theme).fg,
            theme.selection_marker
        );
        assert_eq!(range_bar_style(&theme).fg, theme.selection_marker);
        assert_eq!(
            selection_marker_style(&theme, legacy_pointer).bg,
            legacy_pointer.bg
        );
        assert_eq!(
            unchanged,
            (
                border_style(&theme, true),
                hash_style(&theme),
                selected_style(&theme),
                visual_selection_style(&theme)
            )
        );
    }
}
