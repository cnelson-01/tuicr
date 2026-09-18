//! The persistent log pane keeps its own list state; these tests pin the
//! contract that its actions never leak into the selector's flat fields
//! (the inline selector's state) and vice versa.

use std::time::{Duration, Instant};

use crate::app::*;
use crate::model::FileStatus;
use crate::vcs::traits::VcsType;

/// A VCS whose history is endless: page `offset..offset+limit` always exists,
/// so the overflow row can be exercised.
struct EndlessVcs {
    info: VcsInfo,
}

impl VcsBackend for EndlessVcs {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, _highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        Err(TuicrError::NoChanges)
    }

    fn fetch_context_lines(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
        _start_line: u32,
        _end_line: u32,
    ) -> Result<Vec<DiffLine>> {
        Ok(Vec::new())
    }

    fn file_line_count(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
    ) -> Result<u32> {
        Ok(0)
    }

    fn get_recent_commits(&self, offset: usize, limit: usize) -> Result<Vec<CommitInfo>> {
        Ok((offset..offset + limit)
            .map(|i| commit(&format!("c{i}")))
            .collect())
    }
}

fn commit(id: &str) -> CommitInfo {
    CommitInfo {
        id: id.to_string(),
        short_id: id.to_string(),
        branch_name: None,
        parent_ids: Vec::new(),
        refs: Vec::new(),
        is_head: false,
        summary: format!("commit {id}"),
        body: None,
        author: "tester".to_string(),
        time: Utc::now(),
    }
}

/// An app in Normal mode reviewing `review` commits, with the inline
/// selector state the review load would have left behind.
fn build_review_app(review: &[&str]) -> App {
    let vcs_info = VcsInfo {
        root_path: PathBuf::from("/tmp"),
        head_commit: "head".to_string(),
        branch_name: Some("main".to_string()),
        vcs_type: VcsType::Git,
    };
    let session = ReviewSession::new(
        vcs_info.root_path.clone(),
        vcs_info.head_commit.clone(),
        vcs_info.branch_name.clone(),
        SessionDiffSource::CommitRange,
    );
    let review_commits: Vec<CommitInfo> = review.iter().map(|id| commit(id)).collect();
    let mut app = App::build(
        Box::new(EndlessVcs {
            info: vcs_info.clone(),
        }),
        vcs_info,
        Theme::dark(),
        None,
        false,
        Vec::new(),
        session,
        DiffSource::CommitRange(review.iter().map(|s| s.to_string()).collect()),
        InputMode::Normal,
        review_commits.clone(),
        None,
        None,
    )
    .expect("failed to build test app");
    app.review_commits = review_commits;
    app.visible_commit_count = app.commit_list.len();
    app.commit_selection_range = Some((0, review.len().saturating_sub(1)));
    app.show_commit_selector = true;
    app.focused_panel = FocusedPanel::Diff;
    app
}

fn pane(app: &App) -> &LogListState {
    &app.log_pane.as_ref().expect("log pane open").list
}

#[test]
fn enable_loads_history_without_touching_inline_state() {
    // given a two-commit review with the inline selector showing it
    let mut app = build_review_app(&["a", "b"]);
    assert!(app.has_inline_commit_selector());

    // when
    app.enable_log_pane();

    // then the pane holds VCS history and the inline fields are untouched
    let ids: Vec<&str> = pane(&app).commits.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids[..3], ["c0", "c1", "c2"]);
    assert!(pane(&app).has_more, "an endless log can page further");
    let inline: Vec<&str> = app.commit_list.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(inline, ["a", "b"]);
    assert_eq!(app.commit_selection_range, Some((0, 1)));
    assert_eq!(app.focused_panel, FocusedPanel::CommitSelector);
    // and the inline selector yields to the pane
    assert!(!app.has_inline_commit_selector());
    assert!(app.has_commit_pane());
    assert!(app.log_pane_focused());
}

#[test]
fn enable_lands_cursor_on_the_loaded_review() {
    // given the review on screen is commit c4
    let mut app = build_review_app(&["c4"]);
    // when
    app.enable_log_pane();
    // then
    assert_eq!(pane(&app).cursor, 4);
}

#[test]
fn moves_and_picks_stay_inside_the_pane() {
    let mut app = build_review_app(&["a", "b"]);
    app.enable_log_pane();
    app.commit_list_cursor = 1; // inline cursor, must survive

    // when moving and picking in the pane
    app.log_pane_move_down(2);
    app.log_pane_toggle_pick();

    // then the pane advanced and picked; the flat fields did not move
    assert_eq!(pane(&app).cursor, 3, "pick auto-advances past row 2");
    assert!(pane(&app).picks.contains(&2));
    assert_eq!(app.commit_list_cursor, 1);
    assert!(app.commit_picks.is_empty());
    assert_eq!(app.commit_selection_range, Some((0, 1)));
    assert!(
        app.log_pane.as_ref().unwrap().preview_at.is_some(),
        "a cursor move schedules a preview"
    );
}

#[test]
fn confirm_on_the_overflow_row_pages_history_into_the_pane() {
    let mut app = build_review_app(&["a"]);
    app.enable_log_pane();
    let first_page = pane(&app).commits.len();

    // when Enter lands on the "show more" row
    app.log_pane_go_bottom();
    assert_eq!(
        pane(&app).cursor,
        first_page,
        "cursor sits on the overflow row"
    );
    app.log_pane_confirm().expect("paging must not fail");

    // then the pane grew and the inline list is still the review
    assert!(pane(&app).commits.len() > first_page);
    let inline: Vec<&str> = app.commit_list.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(inline, ["a"]);
}

#[test]
fn preview_waits_for_the_cursor_to_rest_and_skips_picks_and_loaded_rows() {
    let mut app = build_review_app(&["c0"]);
    app.enable_log_pane();

    // a fresh move has not rested yet
    app.log_pane_move_down(1);
    assert!(!app.poll_log_preview());
    assert!(app.log_pane.as_ref().unwrap().preview_at.is_some());

    // with picks present the rested preview is dropped, not fired
    app.log_pane_toggle_pick();
    app.log_pane.as_mut().unwrap().preview_at = Some(Instant::now() - Duration::from_secs(1));
    assert!(!app.poll_log_preview());
    assert!(app.log_pane.as_ref().unwrap().preview_at.is_none());

    // the row already on screen is not reloaded either
    app.log_pane.as_mut().unwrap().list.picks.clear();
    app.log_pane_go_top();
    assert_eq!(pane(&app).commits[0].id, "c0");
    app.log_pane.as_mut().unwrap().preview_at = Some(Instant::now() - Duration::from_secs(1));
    assert!(!app.poll_log_preview());
}

#[test]
fn disable_restores_the_inline_selector_and_diff_focus() {
    let mut app = build_review_app(&["a", "b"]);
    app.enable_log_pane();

    // when
    app.disable_log_pane();

    // then
    assert!(app.log_pane.is_none());
    assert!(!app.log_pane_wanted);
    assert_eq!(app.focused_panel, FocusedPanel::Diff);
    assert!(app.has_inline_commit_selector());
}

#[test]
fn height_is_floored_at_the_minimum() {
    let mut app = build_review_app(&["a"]);
    app.set_log_pane_height(1);
    assert_eq!(app.log_pane_height, MIN_LOG_PANE_HEIGHT);
    app.adjust_log_pane_height(-10);
    assert_eq!(app.log_pane_height, MIN_LOG_PANE_HEIGHT);
    app.adjust_log_pane_height(5);
    assert_eq!(app.log_pane_height, MIN_LOG_PANE_HEIGHT + 5);
}

#[test]
fn autoshow_opens_in_normal_mode_without_stealing_focus() {
    let mut app = build_review_app(&["a"]);
    app.log_pane_wanted = true;

    // not while the fullscreen selector is up
    app.input_mode = InputMode::CommitSelect;
    assert!(!app.poll_log_pane_autoshow());
    assert!(app.log_pane.is_none());

    // once a review is on screen, and the diff keeps focus
    app.input_mode = InputMode::Normal;
    assert!(app.poll_log_pane_autoshow());
    assert!(app.log_pane.is_some());
    assert_eq!(app.focused_panel, FocusedPanel::Diff);
    // and only once
    assert!(!app.poll_log_pane_autoshow());
}
