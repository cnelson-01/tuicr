//! The persistent log pane docked above the diff: the commit graph across
//! local and remote-tracking branches, the pick set, and a marker on the rows whose diff is on
//! screen. Rendering reads the pane's own list state; it never touches the
//! selector's flat fields.

use std::collections::HashSet;

use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;
use crate::ui::commit_graph::{compute_commit_graph, graph_row_spans, main_track_tip_for};
use crate::ui::commit_row::{CommitRowLayout, CommitRowSpec, render_commit_row};
use crate::ui::selector::overflow_row;
use crate::ui::styles;

pub(super) fn render_log_pane(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.log_pane_focused();
    let branch = app.vcs_info.branch_name.as_deref().unwrap_or("HEAD");
    let picked = app
        .log_pane
        .as_ref()
        .map(|pane| pane.list.picks.len())
        .unwrap_or(0);
    let mut title = format!(" Log \u{00b7} {branch}");
    if let Some(summary) = app.commit_filter_summary() {
        title.push_str(&format!(
            " \u{00b7} /{summary} \u{00b7} {}",
            app.commit_filter_match_label()
        ));
    }
    if picked > 0 {
        title.push_str(&format!(" \u{00b7} {picked} picked"));
    }
    title.push(' ');
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(styles::panel_style(&app.theme))
        .border_style(styles::border_style(&app.theme, focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Rows whose diff is on screen carry the range bar, like the inline
    // selector's selected range.
    let loaded: HashSet<&str> = app.review_commits.iter().map(|c| c.id.as_str()).collect();

    let theme = &app.theme;
    let main_tip = app.main_track_tip.as_deref();
    let unfiltered = app.commit_filter_shown.is_none();
    let Some(pane) = app.log_pane.as_mut() else {
        return;
    };
    let height = inner.height as usize;
    pane.list.viewport_height = height;
    pane.inner_area = Some(inner);

    let list = &pane.list;
    let total = list.commits.len();
    let visible_count = list.visible_count.min(total);
    let can_show_more = list.visible_count < total || list.has_more;
    let row_count = visible_count + usize::from(can_show_more);

    // The pane height can change between frames (`:set logheight`, terminal
    // resize), so keep the cursor inside the viewport here rather than
    // trusting the offset the movement code left behind.
    let mut offset = list.scroll_offset;
    if height > 0 {
        if list.cursor < offset {
            offset = list.cursor;
        } else if list.cursor >= offset + height {
            offset = list.cursor + 1 - height;
        }
    }
    let offset = offset.min(row_count.saturating_sub(1));
    pane.list.scroll_offset = offset;
    let list = &pane.list;

    let main_tip = main_track_tip_for(main_tip, &list.commits, unfiltered && list.has_more);
    let graph = compute_commit_graph(&list.commits, main_tip);
    let gutter_lanes = graph.gutter_lanes();
    let layout = CommitRowLayout::fit_rows(&list.commits, inner.width as usize, gutter_lanes * 2);

    let mut items: Vec<Line> = list
        .commits
        .iter()
        .take(visible_count)
        .enumerate()
        .map(|(i, commit)| {
            let row = graph.rows.get(i).cloned().unwrap_or_default();
            render_commit_row(&CommitRowSpec {
                commit,
                is_cursor: i == list.cursor,
                is_selected: list.picks.contains(&i),
                is_loaded: loaded.contains(commit.id.as_str()),
                is_reviewed: false,
                theme,
                graph: (gutter_lanes > 0).then(|| graph_row_spans(&row, gutter_lanes, theme)),
                layout,
            })
        })
        .collect();

    if can_show_more {
        items.push(overflow_row(
            theme,
            list.cursor == visible_count,
            "show more commits",
            gutter_lanes * 2,
        ));
    }

    let visible_items: Vec<Line> = items.into_iter().skip(offset).take(height).collect();
    frame.render_widget(
        Paragraph::new(visible_items).style(styles::panel_style(theme)),
        inner,
    );
}
