use crate::app::*;
use crate::model::FileStatus;
use crate::vcs::traits::{BranchInfo, RefKind, RefLabel, VcsType};

struct DummyVcs {
    info: VcsInfo,
    range_diff: Vec<DiffFile>,
    branches: Vec<BranchInfo>,
}

impl VcsBackend for DummyVcs {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, _highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        Err(TuicrError::NoChanges)
    }

    fn get_commit_range_diff(
        &self,
        _revision_range: &crate::vcs::traits::ResolvedRevisionRange<'_>,
        _highlighter: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        Ok(self.range_diff.clone())
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

    fn list_branches(&self) -> Result<Vec<BranchInfo>> {
        Ok(self.branches.clone())
    }
}

fn build_app(commit_list: Vec<CommitInfo>) -> App {
    build_app_with_range_diff(commit_list, Vec::new())
}

fn build_app_with_range_diff(commit_list: Vec<CommitInfo>, range_diff: Vec<DiffFile>) -> App {
    build_app_with(commit_list, range_diff, Vec::new())
}

fn build_app_with(
    commit_list: Vec<CommitInfo>,
    range_diff: Vec<DiffFile>,
    branches: Vec<BranchInfo>,
) -> App {
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
        SessionDiffSource::WorkingTree,
    );

    App::build(
        Box::new(DummyVcs {
            info: vcs_info.clone(),
            range_diff,
            branches,
        }),
        vcs_info,
        Theme::dark(),
        None,
        false,
        Vec::new(),
        session,
        DiffSource::WorkingTree,
        InputMode::CommitSelect,
        commit_list,
        None,
        None,
    )
    .expect("failed to build test app")
}

fn normal_commit(id: &str) -> CommitInfo {
    CommitInfo {
        id: id.to_string(),
        short_id: id.to_string(),
        branch_name: None,
        parent_ids: Vec::new(),
        refs: Vec::new(),
        is_head: false,
        summary: "Test commit".to_string(),
        body: None,
        author: "Test".to_string(),
        time: Utc::now(),
    }
}

fn install_merge_view(app: &mut App) {
    let content = "top\n<<<<<<< HEAD\nours1\n=======\ntheirs1\n>>>>>>> x\nmid\n<<<<<<< HEAD\nours2\n=======\ntheirs2\n>>>>>>> x\n";
    let file = crate::vcs::parse_conflict_file(content);
    let resolutions = vec![ConflictResolution::Unresolved; file.conflict_count()];
    app.merge_view = Some(MergeView {
        files: vec![MergeFileState {
            path: PathBuf::from("a.txt"),
            file,
            resolutions,
            dirty: false,
            staged: false,
            load_error: None,
        }],
        current_file: 0,
        current_conflict: 0,
        scroll_offset: 0,
        viewport_height: 20,
        return_mode: InputMode::CommitSelect,
        discard_armed: false,
        ours_label: "main".to_string(),
        theirs_label: "feature".to_string(),
        ours_commit: None,
        theirs_commit: None,
    });
    app.input_mode = InputMode::MergeConflict;
}

#[test]
fn commit_limit_scales_with_terminal_rows_within_bounds() {
    // small/unknown terminals keep the historical page size
    assert_eq!(commit_limit_for_rows(0), VISIBLE_COMMIT_COUNT);
    assert_eq!(commit_limit_for_rows(12), VISIBLE_COMMIT_COUNT);
    // a fullscreen terminal fills its body
    assert_eq!(commit_limit_for_rows(40), 34);
    assert_eq!(commit_limit_for_rows(60), 54);
    // bounded above
    assert_eq!(commit_limit_for_rows(10_000), MAX_INITIAL_COMMITS);
}

#[test]
fn fill_commit_viewport_reveals_loaded_commits_up_to_capacity() {
    // given 30 loaded commits with only 10 visible
    let commits: Vec<CommitInfo> = (0..30).map(|i| normal_commit(&format!("c{i}"))).collect();
    let mut app = build_app(commits);
    app.visible_commit_count = 10;

    // when the terminal grows to fit ~24 rows of body
    app.fill_commit_viewport(30);

    // then visibility grows toward capacity (page-at-a-time)
    assert!(
        app.visible_commit_count >= 24,
        "got {}",
        app.visible_commit_count
    );
    assert!(app.visible_commit_count <= 30);

    // and a huge resize reveals everything that exists (DummyVcs has
    // no further pages)
    app.fill_commit_viewport(200);
    assert_eq!(app.visible_commit_count, 30);
    assert!(!app.has_more_commit);
}

#[test]
fn selector_action_ids_use_picks_oldest_first() {
    // given a selector list with a pseudo row and all rows picked
    let mut app = build_app(vec![
        App::staged_commit_entry(),
        normal_commit("aaa"),
        normal_commit("bbb"),
    ]);
    app.commit_picks.extend([0, 1, 2]);
    // when / then: pseudo rows drop out, order flips to oldest-first
    assert_eq!(
        app.selector_action_commit_ids(),
        vec!["bbb".to_string(), "aaa".to_string()]
    );
    // and without picks the cursor row is used
    app.commit_picks.clear();
    app.commit_list_cursor = 1;
    assert_eq!(app.selector_action_commit_ids(), vec!["aaa".to_string()]);
    // pseudo cursor rows produce no ids
    app.commit_list_cursor = 0;
    assert!(app.selector_action_commit_ids().is_empty());
}

#[test]
fn selector_picks_support_non_contiguous_commits() {
    // given commits interleaved across branches
    let mut app = build_app(vec![
        normal_commit("newest"),
        normal_commit("other-branch"),
        normal_commit("mine"),
    ]);

    // when picking rows 0 and 2 with space (skipping row 1)
    app.toggle_commit_selection_and_advance(); // picks 0, cursor -> 1
    app.commit_list_cursor = 2;
    app.toggle_commit_selection_and_advance(); // picks 2

    // then the action set is exactly those two, oldest first
    assert_eq!(
        app.selector_action_commit_ids(),
        vec!["mine".to_string(), "newest".to_string()]
    );

    // and space on a picked row unpicks it
    app.commit_list_cursor = 0;
    app.toggle_commit_selection_and_advance();
    assert_eq!(app.selector_action_commit_ids(), vec!["mine".to_string()]);
}

#[test]
fn confirm_refuses_non_contiguous_picks_for_review() {
    // given a gap in the picks
    let mut app = build_app(vec![
        normal_commit("a"),
        normal_commit("b"),
        normal_commit("c"),
    ]);
    app.commit_picks.extend([0, 2]);
    // when confirming
    app.confirm_commit_selection()
        .expect("confirm should not error");
    // then we stay in the selector with a hint instead of reviewing a
    // range that would silently include the unpicked middle commit
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert!(app.message.is_some());
}

#[test]
fn merge_resolve_marks_dirty_and_advances_to_next_unresolved() {
    // given a merge view with two unresolved conflicts
    let mut app = build_app(vec![]);
    install_merge_view(&mut app);

    // when the first conflict takes ours
    app.merge_resolve_current(ConflictResolution::Ours);

    // then the resolution is recorded, the file is dirty, and focus
    // sweeps forward to the remaining unresolved conflict
    let view = app.merge_view.as_ref().unwrap();
    let file = view.current_file_state().unwrap();
    assert_eq!(file.resolutions[0], ConflictResolution::Ours);
    assert!(file.dirty);
    assert_eq!(view.current_conflict, 1);
    assert_eq!(file.unresolved_count(), 1);
}

#[test]
fn merge_unresolve_restores_conflict() {
    // given
    let mut app = build_app(vec![]);
    install_merge_view(&mut app);
    app.merge_resolve_current(ConflictResolution::Theirs);

    // when the user moves back and undoes the pick
    app.merge_prev_conflict();
    app.merge_resolve_current(ConflictResolution::Unresolved);

    // then
    let view = app.merge_view.as_ref().unwrap();
    let file = view.current_file_state().unwrap();
    assert_eq!(file.resolutions[0], ConflictResolution::Unresolved);
    assert_eq!(file.unresolved_count(), 2);
}

#[test]
fn merge_close_requires_second_esc_when_dirty() {
    // given unsaved resolutions
    let mut app = build_app(vec![]);
    install_merge_view(&mut app);
    app.merge_resolve_current(ConflictResolution::Ours);

    // when Esc is pressed once
    app.close_merge_view();

    // then the view stays open with the discard warning armed
    assert_eq!(app.input_mode, InputMode::MergeConflict);
    assert!(app.merge_view.as_ref().unwrap().discard_armed);

    // and a second Esc returns to the originating mode
    app.close_merge_view();
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert!(app.merge_view.is_none());
}

#[test]
fn merge_prompt_triggers_only_when_everything_is_resolved_and_staged() {
    // given a merge view with two conflicts
    let mut app = build_app(vec![]);
    install_merge_view(&mut app);
    assert!(!app.merge_all_resolved_and_staged());

    // when everything is resolved, written, and staged
    {
        let view = app.merge_view.as_mut().unwrap();
        let file = view.files.get_mut(0).unwrap();
        file.resolutions = vec![ConflictResolution::Ours, ConflictResolution::Theirs];
        file.dirty = false;
        file.staged = true;
    }
    // then the prompt condition holds (DummyVcs reports no conflicts)
    assert!(app.merge_all_resolved_and_staged());
}

#[test]
fn merge_commit_prompt_no_returns_to_merge_view() {
    // given the commit prompt is up
    let mut app = build_app(vec![]);
    install_merge_view(&mut app);
    app.enter_confirm_mode(ConfirmAction::ContinueMergeOperation);
    assert_eq!(app.input_mode, InputMode::Confirm);

    // when the user declines
    crate::handler::handle_confirm_action(&mut app, crate::input::Action::ConfirmNo);

    // then we are back in the merge view, not quitting
    assert_eq!(app.input_mode, InputMode::MergeConflict);
    assert!(!app.should_quit);
    assert!(app.merge_view.is_some());
}

#[test]
fn merge_commit_prompt_yes_surfaces_backend_error() {
    // given a backend that cannot continue (DummyVcs default)
    let mut app = build_app(vec![]);
    install_merge_view(&mut app);
    app.enter_confirm_mode(ConfirmAction::ContinueMergeOperation);

    // when the user confirms
    crate::handler::handle_confirm_action(&mut app, crate::input::Action::ConfirmYes);

    // then the failure is surfaced and the merge view stays open
    assert_eq!(app.input_mode, InputMode::MergeConflict);
    assert!(app.merge_view.is_some());
    assert!(app.message.is_some(), "an error message should be shown");
}

#[test]
fn merge_reload_resets_resolutions_from_new_content() {
    // given a merge view with picks already made
    let mut app = build_app(vec![]);
    install_merge_view(&mut app);
    app.merge_resolve_current(ConflictResolution::Ours);

    // when the file is reloaded with one conflict hand-resolved
    let edited = "top\nhand resolved\nmid\n<<<<<<< HEAD\nours2\n=======\ntheirs2\n>>>>>>> x\n";
    app.merge_apply_reloaded_content(edited);

    // then only the remaining marker is a conflict, unresolved again
    let view = app.merge_view.as_ref().unwrap();
    let file = view.current_file_state().unwrap();
    assert_eq!(file.file.conflict_count(), 1);
    assert_eq!(file.resolutions, vec![ConflictResolution::Unresolved]);
    assert!(!file.dirty);
    assert!(!file.staged);
    assert_eq!(view.current_conflict, 0);
}

#[test]
fn merge_edit_queues_editor_at_focused_conflict() {
    // given a merge view whose file lives in a real directory
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = build_app(vec![]);
    app.vcs_info.root_path = dir.path().to_path_buf();
    install_merge_view(&mut app);

    // when the user asks to edit
    app.merge_edit_current_file();

    // then current picks are written and the editor targets the
    // first conflict's marker line
    let target = app.take_pending_editor_target().expect("editor target");
    assert_eq!(target.path, dir.path().join("a.txt"));
    assert_eq!(target.line, Some(2));
    let written = std::fs::read_to_string(dir.path().join("a.txt")).expect("written file");
    assert!(
        written.starts_with("top\n<<<<<<< HEAD\n"),
        "got: {written:?}"
    );
}

#[test]
fn merge_close_without_changes_returns_immediately() {
    // given
    let mut app = build_app(vec![]);
    install_merge_view(&mut app);
    // when
    app.close_merge_view();
    // then
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert!(app.merge_view.is_none());
}

#[test]
fn special_commit_count_counts_leading_special_entries() {
    let app = build_app(vec![
        App::staged_commit_entry(),
        App::unstaged_commit_entry(),
        normal_commit("abc123"),
    ]);

    assert_eq!(app.special_commit_count(), 2);
}

#[test]
fn special_commit_count_ignores_non_leading_special_entries() {
    let app = build_app(vec![normal_commit("abc123"), App::staged_commit_entry()]);

    assert_eq!(app.special_commit_count(), 0);
}

#[test]
fn toggle_commit_selection_from_all_selected_selects_only_cursor() {
    for cursor in 0..3 {
        let mut app = build_app(vec![
            normal_commit("abc123"),
            normal_commit("def456"),
            normal_commit("789abc"),
        ]);
        app.commit_selection_range = Some((0, 2));
        app.commit_list_cursor = cursor;

        app.toggle_commit_selection();

        assert_eq!(app.commit_selection_range, Some((cursor, cursor)));
    }
}

#[test]
fn toggle_commit_selection_keeps_partial_range_shrink_behavior() {
    let mut app = build_app(vec![
        normal_commit("abc123"),
        normal_commit("def456"),
        normal_commit("789abc"),
    ]);
    app.commit_selection_range = Some((0, 1));
    app.commit_list_cursor = 0;

    app.toggle_commit_selection();

    assert_eq!(app.commit_selection_range, Some((1, 1)));
}

#[test]
fn initial_commit_range_all_selects_full_span() {
    assert_eq!(
        App::initial_commit_range(CommitSelectionStart::All, 4),
        Some((0, 3))
    );
}

#[test]
fn initial_commit_range_oldest_selects_last_index() {
    // review_commits is stored newest-first, so the oldest commit is the last
    // index regardless of the display order.
    assert_eq!(
        App::initial_commit_range(CommitSelectionStart::Oldest, 4),
        Some((3, 3))
    );
}

#[test]
fn initial_commit_range_empty_is_none() {
    assert_eq!(
        App::initial_commit_range(CommitSelectionStart::Oldest, 0),
        None
    );
    assert_eq!(
        App::initial_commit_range(CommitSelectionStart::All, 0),
        None
    );
}

#[test]
fn commit_data_index_is_identity_when_descending() {
    let mut app = build_app(vec![
        normal_commit("a"),
        normal_commit("b"),
        normal_commit("c"),
    ]);
    app.review_commits = app.commit_list.clone();
    app.commit_order = CommitOrder::Descending;
    for i in 0..3 {
        assert_eq!(app.commit_data_index(i), i);
    }
}

#[test]
fn commit_data_index_mirrors_and_round_trips_when_ascending() {
    let mut app = build_app(vec![
        normal_commit("a"),
        normal_commit("b"),
        normal_commit("c"),
    ]);
    app.review_commits = app.commit_list.clone();
    app.commit_order = CommitOrder::Ascending;
    assert_eq!(app.commit_data_index(0), 2);
    assert_eq!(app.commit_data_index(2), 0);
    // The mapping is its own inverse (data <-> display row).
    for i in 0..3 {
        assert_eq!(app.commit_data_index(app.commit_data_index(i)), i);
    }
}

#[test]
fn toggle_commit_selector_flips_visibility_and_drops_focus() {
    let mut app = build_app(vec![normal_commit("a"), normal_commit("b")]);
    app.show_commit_selector = true;
    app.focused_panel = FocusedPanel::CommitSelector;

    app.toggle_commit_selector();
    assert!(!app.show_commit_selector);
    // Hiding the focused pane returns focus to the diff.
    assert_eq!(app.focused_panel, FocusedPanel::Diff);

    app.toggle_commit_selector();
    assert!(app.show_commit_selector);
}

#[test]
fn has_review_commits_ignores_visibility_but_requires_multiple_non_worktree() {
    let mut app = build_app(vec![normal_commit("a"), normal_commit("b")]);
    app.review_commits = app.commit_list.clone();
    app.diff_source = DiffSource::CommitRange(vec!["a".to_string(), "b".to_string()]);

    app.show_commit_selector = false;
    assert!(app.has_review_commits());
    assert!(!app.has_inline_commit_selector());

    // Working-tree reviews never cycle commits.
    app.diff_source = DiffSource::WorkingTree;
    assert!(!app.has_review_commits());
}

#[test]
fn commit_selection_summary_shows_position_for_single_and_count_for_range() {
    let mut app = build_app(vec![
        normal_commit("a"),
        normal_commit("b"),
        normal_commit("c"),
    ]);
    app.review_commits = app.commit_list.clone();

    // Whole range selected -> no summary (caller shows the plain total).
    app.commit_selection_range = Some((0, 2));
    assert_eq!(app.commit_selection_summary(), None);

    // Single commit, descending: position == data index + 1, so cycling moves it.
    app.commit_order = CommitOrder::Descending;
    app.commit_selection_range = Some((0, 0));
    assert_eq!(
        app.commit_selection_summary().as_deref(),
        Some("commit 1/3")
    );
    app.commit_selection_range = Some((2, 2));
    assert_eq!(
        app.commit_selection_summary().as_deref(),
        Some("commit 3/3")
    );

    // Single commit, ascending: display position is mirrored (top row == 1).
    app.commit_order = CommitOrder::Ascending;
    app.commit_selection_range = Some((0, 0)); // newest -> bottom row
    assert_eq!(
        app.commit_selection_summary().as_deref(),
        Some("commit 3/3")
    );
    app.commit_selection_range = Some((2, 2)); // oldest -> top row
    assert_eq!(
        app.commit_selection_summary().as_deref(),
        Some("commit 1/3")
    );

    // Multi-commit subrange -> selected count.
    app.commit_order = CommitOrder::Descending;
    app.commit_selection_range = Some((0, 1));
    assert_eq!(
        app.commit_selection_summary().as_deref(),
        Some("2 of 3 commits")
    );
}

/// A file that lives only inside a commit, never in the working tree.
fn commit_only_file(path: &Path, hunks: Vec<DiffHunk>) -> DiffFile {
    DiffFile {
        old_path: None,
        new_path: Some(path.to_path_buf()),
        status: FileStatus::Modified,
        hunks,
        is_binary: false,
        is_too_large: false,
        is_commit_message: false,
        content_hash: 7,
    }
}

fn one_line_hunk() -> DiffHunk {
    DiffHunk {
        header: "@@ -1,1 +1,1 @@".to_string(),
        lines: vec![DiffLine {
            origin: LineOrigin::Addition,
            content: "let x = 1;".to_string(),
            old_lineno: None,
            new_lineno: Some(1),
            raw_content: None,
            highlighted_spans: None,
        }],
        old_start: 1,
        old_count: 1,
        new_start: 1,
        new_count: 1,
    }
}

/// The index of `path` in the loaded diff. A single-commit selection also gets
/// a synthetic commit-message row, so the real file is not always first.
fn loaded_file_idx(app: &App, path: &Path) -> usize {
    app.diff_files
        .iter()
        .position(|file| file.display_path() == path)
        .expect("the commit's file should be loaded")
}

/// Marking a file reviewed with `r` must work for a file that only exists in a
/// selected commit, not just for staged and unstaged ones.
///
/// `toggle_reviewed_for_file_idx` looks the file up in `session.files` and
/// returns silently when it is absent. Only files that were registered with
/// `session.add_diff_file` are in there. Staged and unstaged files get
/// registered by the initial working-tree load, so they toggle. Files reached
/// by narrowing the inline commit pane never did.
#[test]
fn should_mark_a_commit_only_file_reviewed_after_narrowing_the_commit_pane() {
    let mut app = build_app(vec![normal_commit("c2"), normal_commit("c1")]);
    app.review_commits = app.commit_list.clone();
    let path = PathBuf::from("src/only_in_commit.rs");
    app.commit_diff_cache
        .insert((0, 0), vec![commit_only_file(&path, Vec::new())]);

    // Narrow the pane to c2 alone and load its diff.
    app.commit_selection_range = Some((0, 0));
    app.reload_inline_selection()
        .expect("reload should succeed");

    // when: the user presses `r` on it
    app.toggle_reviewed_for_file_idx(loaded_file_idx(&app, &path), true);

    // then
    assert!(
        app.session.is_file_reviewed(&path),
        "pressing r on a commit-only file must mark it reviewed"
    );
}

/// The hunk-level mark (`R`) has the same dependency on session registration as
/// the file-level one, so it failed in the same place for the same reason.
#[test]
fn should_mark_a_hunk_reviewed_in_a_commit_only_file_after_narrowing_the_commit_pane() {
    let mut app = build_app(vec![normal_commit("c2"), normal_commit("c1")]);
    app.review_commits = app.commit_list.clone();
    let path = PathBuf::from("src/only_in_commit.rs");
    app.commit_diff_cache
        .insert((0, 0), vec![commit_only_file(&path, vec![one_line_hunk()])]);

    app.commit_selection_range = Some((0, 0));
    app.reload_inline_selection()
        .expect("reload should succeed");
    let file_idx = loaded_file_idx(&app, &path);

    // when: the user puts the cursor on the hunk header and presses `R`
    let header_line = app
        .hunk_header_line(file_idx, 0)
        .expect("the hunk should have a header line");
    app.diff_state.cursor_line = header_line;
    app.toggle_hunk_reviewed();

    // then
    assert!(
        app.is_hunk_reviewed(file_idx, 0),
        "pressing R on a hunk in a commit-only file must mark it reviewed"
    );
}

/// Selecting every commit takes a different route to its files: the
/// whole-range copy in `range_diff_files`, not the subrange cache. So it needs
/// its own test. The two above would pass with that wider route still broken.
#[test]
fn should_mark_a_commit_only_file_reviewed_when_every_commit_is_selected() {
    let mut app = build_app(vec![normal_commit("c2"), normal_commit("c1")]);
    app.review_commits = app.commit_list.clone();
    let path = PathBuf::from("src/only_in_commit.rs");
    app.range_diff_files = Some(vec![commit_only_file(&path, Vec::new())]);

    app.commit_selection_range = Some((0, 1));
    app.reload_inline_selection()
        .expect("reload should succeed");

    // when
    app.toggle_reviewed_for_file_idx(loaded_file_idx(&app, &path), true);

    // then
    assert!(
        app.session.is_file_reviewed(&path),
        "pressing r must work when the whole commit range is selected"
    );
}

/// Leaving a comment needs the same session registration the review marks need,
/// because `add_comment_to_session` looks the file up the same way. It fails
/// loudly where `r` and `R` fail silently, so this asserts on the error rather
/// than on a missing mark.
#[test]
fn should_comment_on_a_commit_only_file_after_narrowing_the_commit_pane() {
    use crate::model::comment::{CommentType, LineSide};
    use crate::review_store::{AddCommentRequest, CommentTarget, add_comment_to_session};

    let mut app = build_app(vec![normal_commit("c2"), normal_commit("c1")]);
    app.review_commits = app.commit_list.clone();
    let path = PathBuf::from("src/only_in_commit.rs");
    app.commit_diff_cache
        .insert((0, 0), vec![commit_only_file(&path, vec![one_line_hunk()])]);

    app.commit_selection_range = Some((0, 0));
    app.reload_inline_selection()
        .expect("reload should succeed");

    // when: the user writes a line comment on the commit-only file
    let saved = add_comment_to_session(
        &mut app.session,
        AddCommentRequest {
            target: CommentTarget::Line {
                path: path.clone(),
                line: 1,
                side: LineSide::New,
            },
            content: "this landed two commits ago".to_string(),
            comment_type: CommentType::from_id("note"),
            author: "user".to_string(),
            commit_id: None,
        },
    );

    // then
    assert!(
        saved.is_ok(),
        "commenting on a commit-only file must not fail: {:?}",
        saved.err()
    );
    assert_eq!(
        app.session
            .files
            .get(&path)
            .expect("the file should be in the session")
            .line_comments
            .get(&1)
            .map(Vec::len),
        Some(1),
        "the comment must be stored against the file"
    );
}

#[test]
fn is_diff_sole_pane_only_when_no_other_panes_visible() {
    let mut app = build_app(vec![normal_commit("a"), normal_commit("b")]);
    app.review_commits = app.commit_list.clone();
    app.diff_source = DiffSource::CommitRange(vec!["a".to_string(), "b".to_string()]);

    // File list visible -> not sole.
    app.show_file_list = true;
    app.show_commit_selector = false;
    assert!(!app.is_diff_sole_pane());

    // File list hidden but commit selector visible -> not sole.
    app.show_file_list = false;
    app.show_commit_selector = true;
    assert!(app.has_inline_commit_selector());
    assert!(!app.is_diff_sole_pane());

    // Both hidden -> diff is the sole pane.
    app.show_commit_selector = false;
    assert!(app.is_diff_sole_pane());
}

#[test]
fn review_comments_header_hidden_while_empty() {
    let mut app = build_app(vec![normal_commit("a")]);
    app.is_single_file_view = false;

    // Empty section -> no header.
    assert!(!app.has_review_section_content());
    assert!(!app.show_review_comments_header());

    // A local review comment gives the section content -> header shows.
    app.session.review_comments.push(crate::model::Comment::new(
        "review-level".to_string(),
        crate::model::CommentType::from_id("note"),
        None,
    ));
    assert!(app.has_review_section_content());
    assert!(app.show_review_comments_header());

    // Single-file view always hides the header.
    app.is_single_file_view = true;
    assert!(!app.show_review_comments_header());
}

fn commit_with_body(id: &str, body: &str) -> CommitInfo {
    CommitInfo {
        body: Some(body.to_string()),
        ..normal_commit(id)
    }
}

#[test]
fn should_show_the_commit_message_when_one_commit_is_chosen_in_the_target_selector() {
    let path = PathBuf::from("src/only_in_commit.rs");
    let mut app = build_app_with_range_diff(
        vec![commit_with_body("c1", "why this change was made")],
        vec![commit_only_file(&path, vec![one_line_hunk()])],
    );
    app.commit_list_cursor = 0;
    app.commit_selection_range = Some((0, 0));

    // when: the user presses Enter on that one commit
    app.confirm_commit_selection()
        .expect("confirming one commit should load its diff");

    // then
    let message = app
        .diff_files
        .iter()
        .find(|file| file.is_commit_message)
        .expect("a single-commit review should carry its commit message");
    let rendered: Vec<&str> = message.hunks[0]
        .lines
        .iter()
        .map(|line| line.content.as_str())
        .collect();
    assert!(
        rendered.contains(&"why this change was made"),
        "the message body should be readable, got {rendered:?}"
    );
}

fn commit_message_path(short_id: &str) -> PathBuf {
    PathBuf::from(format!("Commit Message ({short_id})"))
}

fn commit_message_file(app: &App) -> Option<&DiffFile> {
    app.diff_files.iter().find(|file| file.is_commit_message)
}

fn single_commit_review() -> App {
    let mut app = build_app_with_range_diff(
        vec![commit_with_body("c1", "why this change was made")],
        vec![commit_only_file(
            &PathBuf::from("src/only_in_commit.rs"),
            vec![one_line_hunk()],
        )],
    );
    app.commit_selection_range = Some((0, 0));
    app.confirm_commit_selection().unwrap();
    app
}

#[test]
fn should_preserve_commit_message_and_comments_on_manual_reload() {
    let mut app = single_commit_review();
    let message = commit_message_file(&app).unwrap().clone();
    let comment = crate::model::comment::Comment::new(
        "Keep this explanation".into(),
        crate::model::comment::CommentType::None,
        Some(crate::model::comment::LineSide::New),
    );
    app.session
        .files
        .get_mut(message.display_path())
        .unwrap()
        .line_comments
        .insert(3, vec![comment.clone()]);
    app.save_current_session_merging_external().unwrap();

    // Repeated :e must neither drop nor duplicate the synthetic file.
    for _ in 0..2 {
        app.reload_diff_files().unwrap();
        assert_eq!(
            app.diff_files
                .iter()
                .filter(|f| f.is_commit_message)
                .count(),
            1
        );
        assert_eq!(
            commit_message_file(&app).unwrap().content_hash,
            message.content_hash
        );
        assert_eq!(
            app.session.files[message.display_path()].line_comments[&3][0].id,
            comment.id
        );
    }
}

#[test]
fn should_not_treat_commit_message_as_a_watched_diff_change() {
    let app = single_commit_review();
    assert!(commit_message_file(&app).is_some());
    assert!(app.fetch_changed_diff_files().unwrap().is_none());
}

#[test]
fn should_preserve_commit_message_when_watched_diff_changes() {
    let mut app = single_commit_review();
    let message_hash = commit_message_file(&app).unwrap().content_hash;
    let changed = commit_only_file(
        &PathBuf::from("src/newly_visible.rs"),
        vec![one_line_hunk()],
    );
    let request = DiffWatchReloadRequest {
        diff_source: app.diff_source.clone(),
        commit_selection_range: app.commit_selection_range,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(DiffWatchReloadEvent::Done {
        request: request.clone(),
        result: Ok(Some(vec![changed.clone()])),
        commits: None,
        change_status: None,
    })
    .unwrap();
    app.diff_watch_reload = Some(DiffWatchReload { request, rx });

    assert!(app.poll_diff_watch_changes());
    assert_eq!(
        commit_message_file(&app).unwrap().content_hash,
        message_hash
    );
    assert!(
        app.diff_files
            .iter()
            .any(|f| f.display_path() == changed.display_path())
    );
    assert_eq!(app.diff_files.len(), 2);
}

#[test]
fn should_add_and_drop_the_commit_message_as_the_inline_pane_narrows_and_widens() {
    let path = PathBuf::from("src/only_in_commit.rs");
    let mut app = build_app(vec![commit_with_body("c2", "second"), normal_commit("c1")]);
    app.review_commits = app.commit_list.clone();
    app.commit_diff_cache
        .insert((0, 0), vec![commit_only_file(&path, vec![one_line_hunk()])]);
    app.range_diff_files = Some(vec![commit_only_file(&path, vec![one_line_hunk()])]);

    // when: the pane narrows to one commit
    app.commit_selection_range = Some((0, 0));
    app.reload_inline_selection()
        .expect("narrowing should load");

    // then
    assert_eq!(
        commit_message_file(&app).map(|file| file.display_path().clone()),
        Some(commit_message_path("c2")),
        "narrowing to one commit should show that commit's message"
    );

    // when: the pane widens back to both commits
    app.commit_selection_range = Some((0, 1));
    app.reload_inline_selection().expect("widening should load");

    // then
    assert!(
        commit_message_file(&app).is_none(),
        "a two-commit view has no single message to show"
    );
}

#[test]
fn should_not_show_a_commit_message_for_the_unstaged_row() {
    let path = PathBuf::from("src/only_in_commit.rs");
    let mut app = build_app(vec![App::unstaged_commit_entry(), normal_commit("c1")]);
    app.review_commits = app.commit_list.clone();
    app.commit_diff_cache
        .insert((0, 0), vec![commit_only_file(&path, vec![one_line_hunk()])]);

    // when: the pane narrows onto the unstaged row
    app.commit_selection_range = Some((0, 0));
    app.reload_inline_selection()
        .expect("narrowing should load");

    // then
    assert!(
        commit_message_file(&app).is_none(),
        "the unstaged placeholder is not a commit and has no message"
    );
}

#[test]
fn should_comment_on_the_commit_message_of_a_commit_chosen_from_the_target_selector() {
    use crate::model::comment::{CommentType, LineSide};
    use crate::review_store::{AddCommentRequest, CommentTarget, add_comment_to_session};

    let path = PathBuf::from("src/only_in_commit.rs");
    let mut app = build_app_with_range_diff(
        vec![commit_with_body("c1", "why this change was made")],
        vec![commit_only_file(&path, vec![one_line_hunk()])],
    );
    app.commit_list_cursor = 0;
    app.commit_selection_range = Some((0, 0));
    app.confirm_commit_selection()
        .expect("confirming one commit should load its diff");

    // when: the user writes a line comment on the commit message
    let saved = add_comment_to_session(
        &mut app.session,
        AddCommentRequest {
            target: CommentTarget::Line {
                path: commit_message_path("c1"),
                line: 3,
                side: LineSide::New,
            },
            content: "spell out why, not what".to_string(),
            comment_type: CommentType::None,
            author: "user".to_string(),
            commit_id: None,
        },
    );

    // then
    assert!(
        saved.is_ok(),
        "commenting on the commit message should succeed, got {:?}",
        saved.err()
    );
}

#[test]
fn mouse_wheel_moves_commit_picker_cursor_by_configured_lines() {
    let commits = (0..10).map(|i| normal_commit(&format!("c{i}"))).collect();
    let mut app = build_app(commits);
    app.commit_list_inner_area = Some(ratatui::layout::Rect::new(0, 0, 80, 20));
    app.commit_list_cursor = 0;
    let wheel = |app: &mut App, kind| {
        crate::handler::handle_mouse_event(
            app,
            crossterm::event::MouseEvent {
                kind,
                column: 5,
                row: 5,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
    };

    wheel(&mut app, crossterm::event::MouseEventKind::ScrollDown);
    assert_eq!(app.commit_list_cursor, 3, "default is 3 per tick");

    app.mouse_scroll_lines = 1;
    wheel(&mut app, crossterm::event::MouseEventKind::ScrollDown);
    assert_eq!(app.commit_list_cursor, 4);
    wheel(&mut app, crossterm::event::MouseEventKind::ScrollUp);
    assert_eq!(app.commit_list_cursor, 3);
}

fn branch_info(name: &str, is_remote: bool) -> BranchInfo {
    BranchInfo {
        name: name.to_string(),
        tip_id: "abc".to_string(),
        is_current: false,
        is_remote,
        upstream: None,
        ahead: 0,
        behind: 0,
        upstream_gone: false,
    }
}

fn tip_of(id: &str, refs: &[(&str, RefKind)]) -> CommitInfo {
    let mut commit = normal_commit(id);
    commit.refs = refs
        .iter()
        .map(|(name, kind)| RefLabel {
            name: (*name).to_string(),
            kind: *kind,
        })
        .collect();
    commit
}

#[test]
fn checkout_from_a_remote_only_tip_targets_a_new_tracking_branch() {
    // given the cursor on a commit only origin/fix points at
    let remote_tip = tip_of("c1", &[("origin/fix", RefKind::Remote)]);
    let branches = vec![branch_info("main", false), branch_info("origin/fix", true)];
    let app = build_app_with(vec![remote_tip.clone()], Vec::new(), branches);

    // then checkout names the local branch git creates tracking it
    assert_eq!(app.cursor_checkout_target().as_deref(), Some("fix"));

    // but a local `fix` elsewhere, or a second remote's `fix`, would land
    // somewhere else or be ambiguous, so the commit is checked out detached
    for extra in [branch_info("fix", false), branch_info("fork/fix", true)] {
        let branches = vec![branch_info("origin/fix", true), extra];
        let app = build_app_with(vec![remote_tip.clone()], Vec::new(), branches);
        assert_eq!(app.cursor_checkout_target().as_deref(), Some("c1"));
    }

    // and a local branch on the same commit still wins
    let both = tip_of(
        "c2",
        &[("fix", RefKind::Local), ("origin/fix", RefKind::Remote)],
    );
    let app = build_app_with(vec![both], Vec::new(), Vec::new());
    assert_eq!(app.cursor_checkout_target().as_deref(), Some("fix"));
}

#[test]
fn moving_onto_the_overflow_row_pages_in_more_history() {
    // given a selector showing 2 of 5 loaded commits, cursor on the last
    let commits = (0..5).map(|i| normal_commit(&format!("c{i}"))).collect();
    let mut app = build_app(commits);
    app.visible_commit_count = 2;
    app.has_more_commit = false;
    app.commit_list_cursor = 1;

    // when the cursor moves down onto the "show more" row
    app.commit_select_down();

    // then the next page is shown and the cursor sits on a real commit
    assert_eq!(app.commit_list_cursor, 2);
    assert_eq!(app.visible_commit_count, 5);
    assert!(!app.is_on_expand_row());
}
