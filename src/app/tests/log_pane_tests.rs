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

    fn list_branches(&self) -> Result<Vec<crate::vcs::traits::BranchInfo>> {
        Ok(["main", "master", "dev", "origin/main"]
            .iter()
            .map(|name| crate::vcs::traits::BranchInfo {
                name: name.to_string(),
                tip_id: "abc".to_string(),
                is_current: *name == "main",
                is_remote: name.starts_with("origin/"),
                upstream: None,
                ahead: 0,
                behind: 0,
                upstream_gone: false,
            })
            .collect())
    }

    /// Filtered pages are named after the query so tests can tell them apart.
    fn search_commits(
        &self,
        query: &crate::vcs::CommitQuery,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<CommitInfo>> {
        let tag = query.to_string().replace(' ', "_");
        Ok((offset..offset + limit)
            .map(|i| commit(&format!("{tag}-{i}")))
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
pub(super) fn build_review_app(review: &[&str]) -> App {
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
    assert_eq!(app.log_pane_height, Some(MIN_LOG_PANE_HEIGHT));
    app.adjust_log_pane_height(-10);
    assert_eq!(app.log_pane_height, Some(MIN_LOG_PANE_HEIGHT));
    app.adjust_log_pane_height(5);
    assert_eq!(app.log_pane_height, Some(MIN_LOG_PANE_HEIGHT + 5));
}

#[test]
fn default_height_is_a_third_of_the_window_up_to_twelve_rows() {
    let mut app = build_review_app(&["a"]);
    assert_eq!(app.log_pane_height, None);
    assert_eq!(app.log_pane_rows(22), 7);
    assert_eq!(app.log_pane_rows(60), DEFAULT_LOG_PANE_HEIGHT);
    // the diff keeps six rows even in a tiny window
    assert_eq!(app.log_pane_rows(8), MIN_LOG_PANE_HEIGHT);

    // a set height wins, still leaving the diff its six
    app.set_log_pane_height(20);
    assert_eq!(app.log_pane_rows(60), 20);
    assert_eq!(app.log_pane_rows(22), 16);
}

#[test]
fn resizing_starts_from_the_height_on_screen() {
    let mut app = build_review_app(&["a"]);
    app.enable_log_pane();
    // the window-sized default rendered 7 rows (5 inside the borders)
    app.log_pane.as_mut().unwrap().inner_area = Some(ratatui::layout::Rect::new(0, 1, 80, 5));
    app.adjust_log_pane_height(1);
    assert_eq!(app.log_pane_height, Some(8));
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

#[test]
fn autoshow_is_on_by_default_except_without_local_history() {
    let mut app = build_review_app(&["a"]);
    assert!(app.log_pane_wanted);

    // `--all-files` has no diff to sit above
    app.is_pristine_mode = true;
    assert!(!app.poll_log_pane_autoshow());
    app.is_pristine_mode = false;

    // PR and plain-file reviews have no local history
    app.vcs_info.vcs_type = VcsType::File;
    assert!(!app.poll_log_pane_autoshow());
    assert!(app.log_pane.is_none());

    app.vcs_info.vcs_type = VcsType::Git;
    assert!(app.poll_log_pane_autoshow());
}

#[test]
fn log_filter_prompt_applies_pages_and_clears() {
    let mut app = build_review_app(&["a"]);
    app.enable_log_pane();
    assert_eq!(pane(&app).commits[0].id, "c0");

    // typing a query and applying it reloads the pane from the search
    app.begin_commit_filter();
    assert!(app.commit_filter_editing());
    for ch in "fix author:kim".chars() {
        app.commit_filter_insert_char(ch);
    }
    app.apply_commit_filter();
    assert!(!app.commit_filter_editing());
    assert_eq!(
        app.commit_filter_summary().as_deref(),
        Some("fix author:kim")
    );
    // the rows arrive from a background search; stand in for its answer
    assert!(app.commit_filter_loading());
    let query = app.commit_filter.clone().expect("filter");
    let rows = app.vcs.search_commits(&query, 0, 10).expect("search");
    app.commit_filter_rx = None;
    app.install_filtered_rows(rows, true);
    assert_eq!(pane(&app).commits[0].id, "fix_author:kim-0");
    assert!(app.commit_filter_match_label().ends_with("+ matches"));

    // paging through the overflow row stays on the filtered log
    let first_page = pane(&app).commits.len();
    app.log_pane_go_bottom();
    app.log_pane_confirm().expect("paging");
    assert!(pane(&app).commits.len() > first_page);
    assert!(
        pane(&app).commits[first_page]
            .id
            .starts_with("fix_author:kim-")
    );

    // reopening the prompt is pre-filled; Esc keeps the filter
    app.begin_commit_filter();
    assert_eq!(app.commit_filter_draft.as_deref(), Some("fix author:kim"));
    app.cancel_commit_filter();
    assert!(app.commit_filter.is_some());

    // clearing goes back to the plain history; a second clear is a no-op
    assert!(app.clear_commit_filter());
    assert_eq!(pane(&app).commits[0].id, "c0");
    assert!(app.commit_filter_summary().is_none());
    assert!(!app.clear_commit_filter());

    // an empty prompt also clears
    app.begin_commit_filter();
    app.commit_filter_insert_char('x');
    app.apply_commit_filter();
    assert!(app.commit_filter.is_some());
    app.begin_commit_filter();
    app.commit_filter_clear_draft();
    app.apply_commit_filter();
    assert!(app.commit_filter.is_none());
}

#[test]
fn live_filter_debounces_typing_and_installs_rows_behind_special_ones() {
    let mut app = build_review_app(&["a"]);
    app.enable_log_pane();
    app.log_pane.as_mut().unwrap().list.commits.insert(0, {
        let mut staged = commit(STAGED_SELECTION_ID);
        staged.summary = "Staged changes".to_string();
        staged
    });

    // typing schedules a search but nothing fires before the pause
    app.begin_commit_filter();
    app.commit_filter_insert_char('f');
    assert!(app.commit_filter_typed_at.is_some());
    assert!(!app.poll_commit_filter());
    assert!(app.commit_filter_rx.is_none());

    // an answer for the current generation lands behind the pseudo row and
    // keeps the cursor on its commit
    app.log_pane.as_mut().unwrap().list.cursor = 1; // on "c0"
    app.commit_filter_generation += 1;
    let rows = vec![commit("x1"), commit("c0"), commit("x2")];
    app.install_filtered_rows(rows, false);
    let ids: Vec<&str> = pane(&app).commits.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [STAGED_SELECTION_ID, "x1", "c0", "x2"]);
    assert_eq!(pane(&app).cursor, 2, "cursor followed c0");
    assert!(!pane(&app).has_more);

    // a stale answer (older generation) is ignored
    let (tx, rx) = std::sync::mpsc::channel();
    app.commit_filter_rx = Some(rx);
    tx.send(CommitFilterAnswer {
        generation: app.commit_filter_generation - 1,
        query: None,
        result: Ok((vec![commit("stale")], false)),
    })
    .unwrap();
    assert!(!app.poll_commit_filter());
    assert_eq!(pane(&app).commits[1].id, "x1");

    // a current one is installed and clears the busy flag
    tx.send(CommitFilterAnswer {
        generation: app.commit_filter_generation,
        query: None,
        result: Ok((vec![commit("fresh")], true)),
    })
    .unwrap();
    assert!(app.commit_filter_loading());
    assert!(app.poll_commit_filter());
    assert!(!app.commit_filter_loading());
    assert_eq!(pane(&app).commits[1].id, "fresh");
    assert!(pane(&app).has_more);
}

#[test]
fn compare_arguments_complete_branch_names_and_cycle() {
    use crate::handler::handle_command_action;
    use crate::input::Action;

    let mut app = build_review_app(&["a"]);
    app.enter_command_mode();
    app.command_buffer = "compare ma".to_string();

    // the first press lands on the best match, later ones cycle
    handle_command_action(&mut app, Action::CompleteCommand);
    assert_eq!(app.command_buffer, "compare main");
    handle_command_action(&mut app, Action::CompleteCommand);
    assert_eq!(app.command_buffer, "compare master");
    handle_command_action(&mut app, Action::CompleteCommand);
    assert_eq!(app.command_buffer, "compare origin/main");
    handle_command_action(&mut app, Action::CompleteCommandReverse);
    assert_eq!(app.command_buffer, "compare master");

    // a second argument completes on its own
    app.command_buffer = "compare master ori".to_string();
    app.command_completion = None;
    handle_command_action(&mut app, Action::CompleteCommand);
    assert_eq!(app.command_buffer, "compare master origin/main");

    // commands that do not take a branch still complete command names
    app.command_buffer = "set lo".to_string();
    app.command_completion = None;
    handle_command_action(&mut app, Action::CompleteCommand);
    assert!(
        app.command_buffer.starts_with("set log"),
        "{}",
        app.command_buffer
    );

    // no branch matches leaves the buffer alone and says so
    app.command_buffer = "checkout zzz".to_string();
    app.command_completion = None;
    handle_command_action(&mut app, Action::CompleteCommand);
    assert_eq!(app.command_buffer, "checkout zzz");
    assert!(app.message.is_some());
}

#[test]
fn leader_g_opens_then_focuses_then_hides() {
    let mut app = build_review_app(&["a"]);
    assert!(app.log_pane.is_none());

    // hidden: open and focus
    app.toggle_log_pane();
    assert!(app.log_pane.is_some());
    assert_eq!(app.focused_panel, FocusedPanel::CommitSelector);

    // focus elsewhere, then the key comes back to the pane instead of hiding it
    app.focused_panel = FocusedPanel::Diff;
    app.toggle_log_pane();
    assert!(app.log_pane.is_some());
    assert_eq!(app.focused_panel, FocusedPanel::CommitSelector);

    // focused: hide
    app.toggle_log_pane();
    assert!(app.log_pane.is_none());
    assert_eq!(app.focused_panel, FocusedPanel::Diff);
}

#[test]
fn single_commit_details_name_author_parents_and_refs() {
    let mut app = build_review_app(&["abc1234def"]);
    let commit = &mut app.review_commits[0];
    commit.author = "Kim".to_string();
    commit.parent_ids = vec!["1111111aaaa".to_string(), "2222222bbbb".to_string()];
    commit.refs = vec![
        crate::vcs::traits::RefLabel {
            name: "main".to_string(),
            kind: crate::vcs::traits::RefKind::Head,
        },
        crate::vcs::traits::RefLabel {
            name: "v1".to_string(),
            kind: crate::vcs::traits::RefKind::Tag,
        },
    ];
    let details = app.single_commit_details().expect("one commit on screen");
    assert!(details.starts_with("Kim · "), "{details}");
    assert!(details.contains("merge of 1111111 + 2222222"), "{details}");
    assert!(details.ends_with("HEAD \u{2192} main, <v1>"), "{details}");

    let mut wide = build_review_app(&["a", "b"]);
    wide.commit_selection_range = Some((0, 1));
    assert!(wide.single_commit_details().is_none());
}
