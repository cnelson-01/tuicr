//! IDE-style commit graph for the fullscreen commit selector.
//!
//! Assigns each commit to a colored lane ("track") and produces per-row
//! drawing cells for a graph gutter rendered left of the commit hash.
//! The layout walks the commit list top-to-bottom (newest first, as the
//! topo-ordered log provides it): each lane carries an edge from a child
//! somewhere above toward the parent commit it is waiting for. Edges
//! converge at that parent's row; merge commits fork extra lanes for
//! their additional parents.
//!
//! When the caller names the main track (`main` / `master` and its
//! remote-tracking branch), lane 0 belongs to it and to nothing else, so
//! the main line is always the leftmost track and every other branch hangs
//! off its right side. The lane holds the first-parent chain from the
//! track's tip and, when the local branch has diverged from the remote one,
//! the local branch's own commits stacked on top of it. See
//! [`main_track_for`].

use std::collections::{HashMap, HashSet};

use ratatui::text::Span;

use crate::app::{STAGED_SELECTION_ID, UNSTAGED_SELECTION_ID};
use crate::theme::Theme;
use crate::ui::styles;
use crate::vcs::{CommitInfo, MainTrack};

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

/// Palette slot of the main track, the same wherever its tip sits in the
/// list.
const MAIN_TRACK_COLOR: usize = 0;

/// Allocate the leftmost free lane at or right of `first` (or grow) for an
/// edge awaiting `id`, in the next unused colour.
fn alloc_lane(
    lanes: &mut Vec<Option<Lane>>,
    next_color: &mut usize,
    id: String,
    first: usize,
) -> usize {
    let color = *next_color;
    *next_color += 1;
    place_lane(lanes, id, first, color)
}

/// [`alloc_lane`] with the colour given.
fn place_lane(lanes: &mut Vec<Option<Lane>>, id: String, first: usize, color: usize) -> usize {
    let lane = Lane {
        awaiting: id,
        color,
    };
    if let Some(slot) = lanes.iter().skip(first).position(Option::is_none) {
        lanes[first + slot] = Some(lane);
        first + slot
    } else {
        lanes.push(Some(lane));
        lanes.len() - 1
    }
}

/// The main track to lay `commits` out around, or `None` to lay them out
/// without a reserved lane.
///
/// `track` is what the backend reports (`VcsBackend::main_track`). The
/// lane is held when one of its commits is in the list, and also while they
/// may still page in (`may_page_in`: an unfiltered log with more history to
/// load), so the tracks do not all shift right the moment one arrives. A
/// list that will never show them (a filter on another branch) keeps its
/// full width.
pub fn main_track_for<'a>(
    track: Option<&'a MainTrack>,
    commits: &[CommitInfo],
    may_page_in: bool,
) -> Option<&'a MainTrack> {
    let track = track?;
    (may_page_in || commits.iter().any(|c| track.names(&c.id))).then_some(track)
}

/// Ids on the main track: `tip` and its first-parent ancestors, as far as
/// the list goes.
fn main_track_ids<'a>(commits: &'a [CommitInfo], tip: &str) -> HashSet<&'a str> {
    let by_id: HashMap<&str, &CommitInfo> = commits.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut ids = HashSet::new();
    let mut next = by_id.get(tip).copied();
    while let Some(commit) = next {
        if !ids.insert(commit.id.as_str()) {
            break;
        }
        next = commit
            .parent_ids
            .first()
            .and_then(|parent| by_id.get(parent.as_str()).copied());
    }
    ids
}

/// Compute the lane layout for a selector commit list.
///
/// Requires children to appear before their parents (the backends emit
/// date-ordered logs with that topological guarantee, like `git log
/// --date-order`). Lists with no parent information at all (hg/jj,
/// PR commit subsets) produce an empty graph so the gutter collapses.
///
/// `main` (from [`main_track_for`]) reserves lane 0 for the main track: its
/// tip and that commit's first-parent ancestors, with the stacked local
/// commits above them. The lane stays empty where none of those are, and
/// while they have not been paged in yet.
///
/// The stacked commits sit on the lane as a run of their own. The last of
/// them hands its parent edge to a side lane, since that parent is further
/// down the line than the commits the lane is about to carry:
///
/// ```text
/// ●     local tip
/// ●─╮   oldest local commit; its parent is the fork point
///   │ ● some other branch
/// ● │   remote tip
/// ● │
/// ●─╯   fork point
/// ```
///
/// That needs the local commits listed above the remote tip. In an order
/// that has the remote tip first, the lane is taken when they arrive and
/// they get an ordinary track.
pub fn compute_commit_graph(commits: &[CommitInfo], main: Option<&MainTrack>) -> CommitGraph {
    if commits.iter().all(|c| c.parent_ids.is_empty()) {
        return CommitGraph {
            rows: vec![GraphRow::default(); commits.len()],
            lane_count: 0,
        };
    }

    let main_ids = main.map(|track| main_track_ids(commits, &track.tip));
    let stacked: HashSet<&str> = main
        .map(|track| track.stacked.iter().map(String::as_str).collect())
        .unwrap_or_default();
    // Lanes left of this index are never handed out or trimmed.
    let reserved = usize::from(main_ids.is_some());
    let mut lanes: Vec<Option<Lane>> = vec![None; reserved];
    let mut next_color = reserved;
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

        let on_main_line = main_ids
            .as_ref()
            .is_some_and(|ids| ids.contains(commit.id.as_str()));
        // A stacked commit takes the main lane while the lane is its to
        // take: empty (the local tip) or waiting for it (the rest).
        let on_main_stack = !on_main_line
            && stacked.contains(commit.id.as_str())
            && lanes[0].as_ref().is_none_or(|l| l.awaiting == commit.id);
        let on_main_track = on_main_line || on_main_stack;
        let (node_lane, node_color) = if on_main_track {
            // Whatever waited for this commit bends into the main lane.
            (0, MAIN_TRACK_COLOR)
        } else {
            match incoming.first() {
                Some(&(lane, color)) => (lane, color),
                // Branch tip: open a fresh track.
                None => {
                    let lane = alloc_lane(&mut lanes, &mut next_color, commit.id.clone(), reserved);
                    let color = lanes[lane].as_ref().expect("lane just allocated").color;
                    (lane, color)
                }
            }
        };

        // The node's track continues toward the first parent; the other
        // converging edges end on this row.
        let ends: Vec<(usize, usize)> = incoming
            .iter()
            .copied()
            .filter(|&(lane, _)| lane != node_lane)
            .collect();
        let mut parents = commit.parent_ids.iter();
        let first_parent = parents.next();
        // The bottom of the stack: its parent is not the next commit the
        // main lane carries, so the edge leaves for a lane of its own.
        let detour = first_parent
            .filter(|p| on_main_stack && !stacked.contains(p.as_str()))
            .cloned();
        lanes[node_lane] = first_parent.filter(|_| detour.is_none()).map(|p| Lane {
            awaiting: p.clone(),
            color: node_color,
        });
        for &(lane, _) in &ends {
            lanes[lane] = None;
        }

        // Extra parents of a merge commit fork new tracks (which may reuse
        // a lane freed this very row — drawn as a combined end+fork glyph).
        let mut forks: Vec<(usize, usize)> = Vec::new();
        if let Some(parent) = detour {
            let lane = place_lane(&mut lanes, parent, reserved, MAIN_TRACK_COLOR);
            forks.push((lane, MAIN_TRACK_COLOR));
        }
        forks.extend(parents.map(|p| {
            let lane = alloc_lane(&mut lanes, &mut next_color, p.clone(), reserved);
            let color = lanes[lane].as_ref().expect("lane just allocated").color;
            (lane, color)
        }));

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

        while lanes.len() > reserved && matches!(lanes.last(), Some(None)) {
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
            refs: Vec::new(),
            is_head: false,
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
        let graph = compute_commit_graph(&commits, None);
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
        let graph = compute_commit_graph(&commits, None);
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
        let graph = compute_commit_graph(&commits, None);
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
        let graph = compute_commit_graph(&commits, None);
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
        let graph = compute_commit_graph(&commits, None);
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
    fn should_keep_main_on_the_leftmost_lane_under_a_newer_branch() {
        // given a feature branch two commits ahead of main's tip `m2`
        let commits = vec![
            commit("f2", &["f1"]),
            commit("f1", &["m2"]),
            commit("m2", &["m1"]),
            commit("m1", &[]),
        ];
        // when main's tip is named
        let graph = compute_commit_graph(&commits, Some(&MainTrack::line("m2")));
        // then lane 0 stays empty above the tip and the branch bends into it
        assert_eq!(graph.lane_count, 2);
        assert_eq!(
            glyphs(&graph.rows[0]),
            vec![GraphGlyph::Blank, GraphGlyph::Node]
        );
        assert_eq!(
            glyphs(&graph.rows[1]),
            vec![GraphGlyph::Blank, GraphGlyph::Node]
        );
        assert_eq!(
            glyphs(&graph.rows[2]),
            vec![GraphGlyph::Node, GraphGlyph::MergeIntoLeft]
        );
        assert_eq!(glyphs(&graph.rows[3]), vec![GraphGlyph::Node]);
        // and without the tip the same history is a single lane
        assert_eq!(compute_commit_graph(&commits, None).lane_count, 1);
    }

    #[test]
    fn should_follow_first_parents_for_the_main_track() {
        // given main = merge(m1, side) with the side branch listed first
        let commits = vec![
            commit("merge", &["m1", "s1"]),
            commit("s1", &["base"]),
            commit("m1", &["base"]),
            commit("base", &[]),
        ];
        // when
        let graph = compute_commit_graph(&commits, Some(&MainTrack::line("merge")));
        // then the first-parent line holds lane 0 and the side branch lane 1
        assert_eq!(
            glyphs(&graph.rows[0]),
            vec![GraphGlyph::Node, GraphGlyph::ForkFromLeft]
        );
        assert_eq!(
            glyphs(&graph.rows[1]),
            vec![GraphGlyph::Vertical, GraphGlyph::Node]
        );
        assert_eq!(
            glyphs(&graph.rows[2]),
            vec![GraphGlyph::Node, GraphGlyph::Vertical]
        );
        assert_eq!(
            glyphs(&graph.rows[3]),
            vec![GraphGlyph::Node, GraphGlyph::MergeIntoLeft]
        );
        // and the whole main track shares one colour, apart from the side's
        let main_color = graph.rows[0].cells[0].color;
        assert_eq!(graph.rows[2].cells[0].color, main_color);
        assert_eq!(graph.rows[3].cells[0].color, main_color);
        assert_ne!(graph.rows[1].cells[1].color, main_color);
    }

    #[test]
    fn should_never_hand_the_main_lane_to_another_branch() {
        // given two unrelated tips above main, one of them finished (its
        // lane freed) before main's tip appears
        let commits = vec![
            commit("a2", &["a1"]),
            commit("a1", &[]),
            commit("b1", &["m1"]),
            commit("m2", &["m1"]),
            commit("m1", &[]),
        ];
        // when
        let graph = compute_commit_graph(&commits, Some(&MainTrack::line("m2")));
        // then every non-main node sits right of lane 0
        for row in &graph.rows[..3] {
            assert_eq!(row.cells[0].glyph, GraphGlyph::Blank, "{:?}", glyphs(row));
        }
        assert_eq!(graph.rows[3].cells[0].glyph, GraphGlyph::Node);
        assert_eq!(graph.rows[4].cells[0].glyph, GraphGlyph::Node);
    }

    /// Local `l2 → l1` and remote `r2 → r1` both fork from `base`.
    fn diverged() -> MainTrack {
        MainTrack {
            tip: "r2".to_string(),
            stacked: vec!["l2".to_string(), "l1".to_string()],
        }
    }

    #[test]
    fn should_stack_a_diverged_local_branch_on_the_remote_line() {
        // given the local commits listed first, then a branch `x` cut from
        // the remote tip, then the remote line
        let commits = vec![
            commit("l2", &["l1"]),
            commit("l1", &["base"]),
            commit("x", &["r2"]),
            commit("r2", &["r1"]),
            commit("r1", &["base"]),
            commit("base", &[]),
        ];
        // when
        let graph = compute_commit_graph(&commits, Some(&diverged()));
        // then the local commits sit on the main lane
        assert_eq!(glyphs(&graph.rows[0]), vec![GraphGlyph::Node]);
        // and the oldest hands its edge to a side lane
        assert_eq!(
            glyphs(&graph.rows[1]),
            vec![GraphGlyph::Node, GraphGlyph::ForkFromLeft]
        );
        // which runs past the other branch with the main lane empty
        assert_eq!(
            glyphs(&graph.rows[2]),
            vec![GraphGlyph::Blank, GraphGlyph::Vertical, GraphGlyph::Node]
        );
        // the remote line then takes the main lane, under the local commits
        assert_eq!(
            glyphs(&graph.rows[3]),
            vec![
                GraphGlyph::Node,
                GraphGlyph::Cross,
                GraphGlyph::MergeIntoLeft
            ]
        );
        assert_eq!(
            glyphs(&graph.rows[4]),
            vec![GraphGlyph::Node, GraphGlyph::Vertical]
        );
        // and the side lane comes back in at the fork point
        assert_eq!(
            glyphs(&graph.rows[5]),
            vec![GraphGlyph::Node, GraphGlyph::MergeIntoLeft]
        );
        // all of it in the main track's colour
        let main_color = graph.rows[0].cells[0].color;
        for (row, lane) in [
            (1, 0),
            (1, 1),
            (2, 1),
            (3, 0),
            (4, 0),
            (4, 1),
            (5, 0),
            (5, 1),
        ] {
            assert_eq!(
                graph.rows[row].cells[lane].color, main_color,
                "row {row} lane {lane}"
            );
        }
        assert_ne!(graph.rows[2].cells[2].color, main_color);
    }

    #[test]
    fn should_stack_a_local_merge_of_the_remote_on_the_remote_line() {
        // given local `m` = merge(l1, r1), not pushed: the remote tip is its
        // second parent
        let commits = vec![
            commit("m", &["l1", "r1"]),
            commit("l1", &["base"]),
            commit("r1", &["base"]),
            commit("base", &[]),
        ];
        let track = MainTrack {
            tip: "r1".to_string(),
            stacked: vec!["m".to_string(), "l1".to_string()],
        };
        // when
        let graph = compute_commit_graph(&commits, Some(&track));
        // then the merge forks toward the remote tip from the main lane
        assert_eq!(
            glyphs(&graph.rows[0]),
            vec![GraphGlyph::Node, GraphGlyph::ForkFromLeft]
        );
        // the stack ends, its edge leaving for the fork point across the
        // merge's
        assert_eq!(
            glyphs(&graph.rows[1]),
            vec![
                GraphGlyph::Node,
                GraphGlyph::Cross,
                GraphGlyph::ForkFromLeft
            ]
        );
        // the remote tip takes the lane, the merge's edge arriving
        assert_eq!(
            glyphs(&graph.rows[2]),
            vec![
                GraphGlyph::Node,
                GraphGlyph::MergeIntoLeft,
                GraphGlyph::Vertical
            ]
        );
        assert_eq!(graph.rows[3].cells[0].glyph, GraphGlyph::Node);
        assert_eq!(graph.rows[3].cells[2].glyph, GraphGlyph::MergeIntoLeft);
    }

    #[test]
    fn should_give_the_local_branch_its_own_lane_when_the_remote_tip_is_listed_first() {
        // given an order with the remote line on top (a backend that does
        // not reorder)
        let commits = vec![
            commit("r2", &["r1"]),
            commit("r1", &["base"]),
            commit("l2", &["l1"]),
            commit("l1", &["base"]),
            commit("base", &[]),
        ];
        // when
        let graph = compute_commit_graph(&commits, Some(&diverged()));
        // then the remote line holds the main lane and the local commits
        // run beside it, no edge lost
        assert_eq!(glyphs(&graph.rows[0]), vec![GraphGlyph::Node]);
        assert_eq!(glyphs(&graph.rows[1]), vec![GraphGlyph::Node]);
        assert_eq!(
            glyphs(&graph.rows[2]),
            vec![GraphGlyph::Vertical, GraphGlyph::Node]
        );
        assert_eq!(
            glyphs(&graph.rows[3]),
            vec![GraphGlyph::Vertical, GraphGlyph::Node]
        );
        assert_eq!(
            glyphs(&graph.rows[4]),
            vec![GraphGlyph::Node, GraphGlyph::MergeIntoLeft]
        );
    }

    #[test]
    fn should_keep_the_stack_on_the_main_lane_before_the_remote_tip_has_loaded() {
        // given a first page holding only the local commits
        let commits = vec![commit("l2", &["l1"]), commit("l1", &["base"])];
        let track = diverged();
        // when
        let held = main_track_for(Some(&track), &commits, true);
        let graph = compute_commit_graph(&commits, held);
        // then they are already where they stay once the rest pages in
        assert_eq!(glyphs(&graph.rows[0]), vec![GraphGlyph::Node]);
        assert_eq!(
            glyphs(&graph.rows[1]),
            vec![GraphGlyph::Node, GraphGlyph::ForkFromLeft]
        );
    }

    #[test]
    fn should_hold_the_main_lane_while_the_tip_is_still_to_load() {
        // given a page of branch commits; main's tip `m9` is further down
        let commits = vec![commit("f2", &["f1"]), commit("f1", &["f0"])];
        // when the lane is held for it
        let track = MainTrack::line("m9");
        let held = main_track_for(Some(&track), &commits, true);
        let graph = compute_commit_graph(&commits, held);
        // then the branch already sits on lane 1, where it stays once
        // main's history pages in
        assert_eq!(graph.lane_count, 2);
        for row in &graph.rows {
            assert_eq!(glyphs(row), vec![GraphGlyph::Blank, GraphGlyph::Node]);
        }
    }

    #[test]
    fn should_not_hold_a_lane_for_a_tip_the_list_will_never_show() {
        // given a filtered list with no more history that lacks main's tip
        let commits = vec![commit("f2", &["f1"]), commit("f1", &[])];
        // then no lane is reserved
        let absent = MainTrack::line("m9");
        assert_eq!(main_track_for(Some(&absent), &commits, false), None);
        // while a loaded tip always reserves it, as does a stacked commit
        let loaded = MainTrack::line("f1");
        assert_eq!(
            main_track_for(Some(&loaded), &commits, false),
            Some(&loaded)
        );
        let stacked = MainTrack {
            tip: "m9".to_string(),
            stacked: vec!["f2".to_string()],
        };
        assert_eq!(
            main_track_for(Some(&stacked), &commits, false),
            Some(&stacked)
        );
        // and no track never does
        assert_eq!(main_track_for(None, &commits, true), None);
    }

    #[test]
    fn should_skip_pseudo_rows_without_breaking_alignment() {
        // given a staged pseudo entry above a linear history
        let mut staged = commit(STAGED_SELECTION_ID, &[]);
        staged.summary = "Staged changes".to_string();
        let commits = vec![staged, commit("b", &["a"]), commit("a", &[])];
        // when
        let graph = compute_commit_graph(&commits, None);
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
        let graph = compute_commit_graph(&commits, None);
        // when
        let spans = graph_row_spans(&graph.rows[0], 3, &theme);
        // then each lane renders two characters, missing lanes pad blank
        assert_eq!(spans.len(), 3);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "\u{25cf}     ");
    }
}
