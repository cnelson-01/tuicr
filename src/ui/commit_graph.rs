//! JetBrains-style commit graph for the fullscreen commit selector.
//!
//! Assigns each commit to a colored lane ("track") and produces per-row
//! drawing cells for a graph gutter rendered left of the commit hash.
//! The layout walks the commit list top-to-bottom (newest first, as the
//! topo-ordered log provides it): each lane carries an edge from a child
//! somewhere above toward the parent commit it is waiting for. Edges
//! converge at that parent's row; merge commits fork extra lanes for
//! their additional parents.

use ratatui::text::Span;

use crate::app::{STAGED_SELECTION_ID, UNSTAGED_SELECTION_ID};
use crate::theme::Theme;
use crate::ui::styles;
use crate::vcs::CommitInfo;

/// Cap on rendered lanes; topology beyond this is clipped, not wrapped.
pub const MAX_GRAPH_LANES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphGlyph {
    Blank,
    /// `●` — the commit's own node.
    Node,
    /// `│` — an edge passing through this row.
    Vertical,
    /// `╯` — edge from above ends here, bending toward a node on its left.
    MergeIntoLeft,
    /// `╰` — edge from above ends here, bending toward a node on its right.
    MergeIntoRight,
    /// `╮` — new edge (extra merge parent) starts here, node on its left.
    ForkFromLeft,
    /// `╭` — new edge (extra merge parent) starts here, node on its right.
    ForkFromRight,
    /// `┤` — an edge ends and a new one starts in the same column, node left.
    EndAndForkLeft,
    /// `├` — an edge ends and a new one starts in the same column, node right.
    EndAndForkRight,
    /// `┼` — a passing edge crossed by this row's horizontal connector.
    Cross,
    /// `─` — horizontal connector through an otherwise empty column.
    Horizontal,
}

impl GraphGlyph {
    fn as_char(self) -> char {
        match self {
            GraphGlyph::Blank => ' ',
            GraphGlyph::Node => '\u{25cf}',            // ●
            GraphGlyph::Vertical => '\u{2502}',        // │
            GraphGlyph::MergeIntoLeft => '\u{256f}',   // ╯
            GraphGlyph::MergeIntoRight => '\u{2570}',  // ╰
            GraphGlyph::ForkFromLeft => '\u{256e}',    // ╮
            GraphGlyph::ForkFromRight => '\u{256d}',   // ╭
            GraphGlyph::EndAndForkLeft => '\u{2524}',  // ┤
            GraphGlyph::EndAndForkRight => '\u{251c}', // ├
            GraphGlyph::Cross => '\u{253c}',           // ┼
            GraphGlyph::Horizontal => '\u{2500}',      // ─
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GraphCell {
    pub glyph: GraphGlyph,
    /// Palette slot of the track that owns the glyph.
    pub color: usize,
    /// Whether the row's horizontal connector continues through the filler
    /// column to the right of this glyph.
    pub fill: bool,
}

/// One commit's worth of graph gutter. Pseudo rows (staged/unstaged/merge
/// entries) carry no cells and render as blanks of the shared gutter width.
#[derive(Debug, Clone, Default)]
pub struct GraphRow {
    pub cells: Vec<GraphCell>,
}

#[derive(Debug, Clone, Default)]
pub struct CommitGraph {
    /// One row per entry of the input list, index-aligned.
    pub rows: Vec<GraphRow>,
    /// Widest lane usage across all rows (uncapped).
    pub lane_count: usize,
}

impl CommitGraph {
    /// Rendered gutter width in lanes (0 = no gutter).
    pub fn gutter_lanes(&self) -> usize {
        self.lane_count.min(MAX_GRAPH_LANES)
    }
}

fn is_pseudo_commit(commit: &CommitInfo) -> bool {
    commit.id == STAGED_SELECTION_ID
        || commit.id == UNSTAGED_SELECTION_ID
        || commit.id == crate::app::MERGE_SELECTION_ID
}

#[derive(Debug, Clone)]
struct Lane {
    awaiting: String,
    color: usize,
}

/// Allocate the leftmost free lane (or grow) for an edge awaiting `id`.
fn alloc_lane(lanes: &mut Vec<Option<Lane>>, next_color: &mut usize, id: String) -> usize {
    let color = *next_color;
    *next_color += 1;
    let lane = Lane {
        awaiting: id,
        color,
    };
    if let Some(slot) = lanes.iter().position(Option::is_none) {
        lanes[slot] = Some(lane);
        slot
    } else {
        lanes.push(Some(lane));
        lanes.len() - 1
    }
}

/// Compute the lane layout for a selector commit list.
///
/// Requires children to appear before their parents (the backends emit
/// date-ordered logs with that topological guarantee, like `git log
/// --date-order`). Lists with no parent information at all (hg/jj,
/// PR commit subsets) produce an empty graph so the gutter collapses.
pub fn compute_commit_graph(commits: &[CommitInfo]) -> CommitGraph {
    if commits.iter().all(|c| c.parent_ids.is_empty()) {
        return CommitGraph {
            rows: vec![GraphRow::default(); commits.len()],
            lane_count: 0,
        };
    }

    let mut lanes: Vec<Option<Lane>> = Vec::new();
    let mut next_color = 0usize;
    let mut rows = Vec::with_capacity(commits.len());
    let mut lane_count = 0usize;

    for commit in commits {
        if is_pseudo_commit(commit) {
            rows.push(GraphRow::default());
            continue;
        }

        // Lanes whose edge terminates at this commit, with their colors
        // captured before the lanes are freed below.
        let incoming: Vec<(usize, usize)> = lanes
            .iter()
            .enumerate()
            .filter_map(|(i, lane)| {
                lane.as_ref()
                    .filter(|l| l.awaiting == commit.id)
                    .map(|l| (i, l.color))
            })
            .collect();

        let (node_lane, node_color) = match incoming.first() {
            Some(&(lane, color)) => (lane, color),
            // Branch tip: open a fresh track.
            None => {
                let lane = alloc_lane(&mut lanes, &mut next_color, commit.id.clone());
                let color = lanes[lane].as_ref().expect("lane just allocated").color;
                (lane, color)
            }
        };

        // The node's track continues toward the first parent; the other
        // converging edges end on this row.
        let mut parents = commit.parent_ids.iter();
        lanes[node_lane] = parents.next().map(|p| Lane {
            awaiting: p.clone(),
            color: node_color,
        });
        for &(lane, _) in incoming.iter().skip(1) {
            lanes[lane] = None;
        }

        // Extra parents of a merge commit fork new tracks (which may reuse
        // a lane freed this very row — drawn as a combined end+fork glyph).
        let forks: Vec<(usize, usize)> = parents
            .map(|p| {
                let lane = alloc_lane(&mut lanes, &mut next_color, p.clone());
                let color = lanes[lane].as_ref().expect("lane just allocated").color;
                (lane, color)
            })
            .collect();

        let ends: Vec<(usize, usize)> = incoming.iter().skip(1).copied().collect();
        let span_min = ends
            .iter()
            .chain(forks.iter())
            .map(|&(lane, _)| lane)
            .min()
            .unwrap_or(node_lane)
            .min(node_lane);
        let span_max = ends
            .iter()
            .chain(forks.iter())
            .map(|&(lane, _)| lane)
            .max()
            .unwrap_or(node_lane)
            .max(node_lane);

        let mut cells = Vec::with_capacity(lanes.len());
        for (i, slot) in lanes.iter().enumerate() {
            let end_color = ends.iter().find(|&&(lane, _)| lane == i).map(|&(_, c)| c);
            let fork_color = forks.iter().find(|&&(lane, _)| lane == i).map(|&(_, c)| c);
            let (glyph, color) = if i == node_lane {
                (GraphGlyph::Node, node_color)
            } else if let (Some(_), Some(fc)) = (end_color, fork_color) {
                let glyph = if i > node_lane {
                    GraphGlyph::EndAndForkLeft
                } else {
                    GraphGlyph::EndAndForkRight
                };
                (glyph, fc)
            } else if let Some(ec) = end_color {
                let glyph = if i > node_lane {
                    GraphGlyph::MergeIntoLeft
                } else {
                    GraphGlyph::MergeIntoRight
                };
                (glyph, ec)
            } else if let Some(fc) = fork_color {
                let glyph = if i > node_lane {
                    GraphGlyph::ForkFromLeft
                } else {
                    GraphGlyph::ForkFromRight
                };
                (glyph, fc)
            } else if let Some(lane) = slot {
                let glyph = if i > span_min && i < span_max {
                    GraphGlyph::Cross
                } else {
                    GraphGlyph::Vertical
                };
                (glyph, lane.color)
            } else if i > span_min && i < span_max {
                (GraphGlyph::Horizontal, node_color)
            } else {
                (GraphGlyph::Blank, 0)
            };
            cells.push(GraphCell {
                glyph,
                color,
                fill: i >= span_min && i < span_max,
            });
        }

        lane_count = lane_count.max(cells.len());
        rows.push(GraphRow { cells });

        while matches!(lanes.last(), Some(None)) {
            lanes.pop();
        }
    }

    CommitGraph { rows, lane_count }
}

/// Render one graph row as spans, two characters per lane, padded (or
/// clipped) to `gutter_lanes` so every selector row shares the same width.
pub fn graph_row_spans(row: &GraphRow, gutter_lanes: usize, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(gutter_lanes);
    for i in 0..gutter_lanes {
        match row.cells.get(i) {
            Some(cell) => {
                let filler = if cell.fill { '\u{2500}' } else { ' ' };
                spans.push(Span::styled(
                    format!("{}{}", cell.glyph.as_char(), filler),
                    styles::graph_track_style(theme, cell.color),
                ));
            }
            None => spans.push(Span::raw("  ")),
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn commit(id: &str, parents: &[&str]) -> CommitInfo {
        CommitInfo {
            id: id.to_string(),
            short_id: id[..7.min(id.len())].to_string(),
            branch_name: None,
            parent_ids: parents.iter().map(|p| p.to_string()).collect(),
            summary: format!("commit {id}"),
            body: None,
            author: "tester".to_string(),
            time: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        }
    }

    fn glyphs(row: &GraphRow) -> Vec<GraphGlyph> {
        row.cells.iter().map(|c| c.glyph).collect()
    }

    #[test]
    fn should_layout_linear_history_on_single_lane() {
        // given a -> b -> c (newest first)
        let commits = vec![commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])];
        // when
        let graph = compute_commit_graph(&commits);
        // then
        assert_eq!(graph.lane_count, 1);
        for row in &graph.rows {
            assert_eq!(glyphs(row), vec![GraphGlyph::Node]);
        }
    }

    #[test]
    fn should_hide_graph_when_no_parent_information() {
        // given commits without parent data (hg/jj, PR subsets)
        let commits = vec![commit("c", &[]), commit("b", &[]), commit("a", &[])];
        // when
        let graph = compute_commit_graph(&commits);
        // then the gutter collapses instead of opening a lane per commit
        assert_eq!(graph.lane_count, 0);
        assert_eq!(graph.gutter_lanes(), 0);
        assert_eq!(graph.rows.len(), 3);
    }

    #[test]
    fn should_converge_two_branch_tips_at_their_common_parent() {
        // given two tips d and c sharing parent b
        let commits = vec![
            commit("d", &["b"]),
            commit("c", &["b"]),
            commit("b", &["a"]),
            commit("a", &[]),
        ];
        // when
        let graph = compute_commit_graph(&commits);
        // then both edges run until b, where the second lane bends into b
        assert_eq!(graph.lane_count, 2);
        assert_eq!(glyphs(&graph.rows[0]), vec![GraphGlyph::Node]);
        assert_eq!(
            glyphs(&graph.rows[1]),
            vec![GraphGlyph::Vertical, GraphGlyph::Node]
        );
        assert_eq!(
            glyphs(&graph.rows[2]),
            vec![GraphGlyph::Node, GraphGlyph::MergeIntoLeft]
        );
        assert_eq!(glyphs(&graph.rows[3]), vec![GraphGlyph::Node]);
        // distinct tracks get distinct colors
        assert_ne!(graph.rows[1].cells[0].color, graph.rows[1].cells[1].color);
    }

    #[test]
    fn should_fork_extra_lane_for_merge_commit_second_parent() {
        // given m = merge(a, b); a and b both descend from base
        let commits = vec![
            commit("m", &["a", "b"]),
            commit("a", &["base"]),
            commit("b", &["base"]),
            commit("base", &[]),
        ];
        // when
        let graph = compute_commit_graph(&commits);
        // then the merge row forks a second track which rejoins at base
        assert_eq!(
            glyphs(&graph.rows[0]),
            vec![GraphGlyph::Node, GraphGlyph::ForkFromLeft]
        );
        assert!(graph.rows[0].cells[0].fill, "connector fills toward fork");
        assert_eq!(
            glyphs(&graph.rows[1]),
            vec![GraphGlyph::Node, GraphGlyph::Vertical]
        );
        assert_eq!(
            glyphs(&graph.rows[2]),
            vec![GraphGlyph::Vertical, GraphGlyph::Node]
        );
        assert_eq!(
            glyphs(&graph.rows[3]),
            vec![GraphGlyph::Node, GraphGlyph::MergeIntoLeft]
        );
    }

    #[test]
    fn should_cross_passing_lane_when_connector_spans_it() {
        // given tips t0 (parent p), t1 (parent x), t2 (parent p): the
        // connector from p to t2's lane crosses t1's track
        let commits = vec![
            commit("t0", &["p"]),
            commit("t1", &["x"]),
            commit("t2", &["p"]),
            commit("p", &[]),
        ];
        // when
        let graph = compute_commit_graph(&commits);
        // then
        assert_eq!(
            glyphs(&graph.rows[3]),
            vec![
                GraphGlyph::Node,
                GraphGlyph::Cross,
                GraphGlyph::MergeIntoLeft
            ]
        );
    }

    #[test]
    fn should_skip_pseudo_rows_without_breaking_alignment() {
        // given a staged pseudo entry above a linear history
        let mut staged = commit(STAGED_SELECTION_ID, &[]);
        staged.summary = "Staged changes".to_string();
        let commits = vec![staged, commit("b", &["a"]), commit("a", &[])];
        // when
        let graph = compute_commit_graph(&commits);
        // then the pseudo row is blank but index-aligned
        assert_eq!(graph.rows.len(), 3);
        assert!(graph.rows[0].cells.is_empty());
        assert_eq!(glyphs(&graph.rows[1]), vec![GraphGlyph::Node]);
    }

    #[test]
    fn should_render_row_spans_padded_to_gutter_width() {
        // given
        let theme = crate::theme::Theme::dark();
        let commits = vec![commit("b", &["a"]), commit("a", &[])];
        let graph = compute_commit_graph(&commits);
        // when
        let spans = graph_row_spans(&graph.rows[0], 3, &theme);
        // then each lane renders two characters, missing lanes pad blank
        assert_eq!(spans.len(), 3);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "\u{25cf}     ");
    }
}
