use crate::app::*;
use crate::input::Action;
use crate::model::{DiffFile, DiffHunk, DiffLine, FileStatus, LineOrigin};
use crate::vcs::traits::{VcsBackend, VcsInfo, VcsType};
use std::path::PathBuf;

struct StubVcs(VcsInfo);
impl VcsBackend for StubVcs {
    fn info(&self) -> &VcsInfo {
        &self.0
    }
    fn get_working_tree_diff(
        &self,
        _hl: &crate::syntax::SyntaxHighlighter,
    ) -> crate::error::Result<Vec<DiffFile>> {
        Ok(Vec::new())
    }
    fn fetch_context_lines(
        &self,
        _path: &std::path::Path,
        _status: FileStatus,
        _ref_commit: Option<&str>,
        _start: u32,
        _end: u32,
    ) -> crate::error::Result<Vec<DiffLine>> {
        Ok(Vec::new())
    }
    fn file_line_count(
        &self,
        _path: &std::path::Path,
        _status: FileStatus,
        _ref_commit: Option<&str>,
    ) -> crate::error::Result<u32> {
        Ok(0)
    }
}

fn line(origin: LineOrigin, content: &str) -> DiffLine {
    DiffLine {
        origin,
        content: content.to_string(),
        old_lineno: (origin != LineOrigin::Addition).then_some(1),
        new_lineno: (origin != LineOrigin::Deletion).then_some(1),
        raw_content: None,
        highlighted_spans: None,
    }
}

/// Two hunks. The first holds two changes split by a context line: a
/// replaced line (deletion + addition), then a lone addition. The second
/// holds one deletion.
fn file(path: &str) -> DiffFile {
    use LineOrigin::{Addition, Context, Deletion};
    let hunks = vec![
        DiffHunk {
            header: "@@ -1,4 +1,5 @@".to_string(),
            lines: vec![
                line(Context, "needle one"),
                line(Deletion, "b"),
                line(Addition, "B"),
                line(Context, "c needle"),
                line(Addition, "d"),
                line(Context, "e"),
            ],
            old_start: 1,
            old_count: 4,
            new_start: 1,
            new_count: 5,
        },
        DiffHunk {
            header: "@@ -20,2 +21,1 @@".to_string(),
            lines: vec![line(Deletion, "x"), line(Context, "y")],
            old_start: 20,
            old_count: 2,
            new_start: 21,
            new_count: 1,
        },
    ];
    let content_hash = DiffFile::compute_content_hash(&hunks);
    DiffFile {
        old_path: None,
        new_path: Some(PathBuf::from(path)),
        status: FileStatus::Modified,
        hunks,
        is_binary: false,
        is_too_large: false,
        is_commit_message: false,
        content_hash,
    }
}

fn app_with(files: Vec<DiffFile>) -> App {
    let vcs_info = VcsInfo {
        root_path: PathBuf::from("/tmp"),
        head_commit: "head".into(),
        branch_name: Some("main".into()),
        vcs_type: VcsType::Git,
    };
    let session = ReviewSession::new(
        vcs_info.root_path.clone(),
        vcs_info.head_commit.clone(),
        vcs_info.branch_name.clone(),
        SessionDiffSource::WorkingTree,
    );
    App::build(
        Box::new(StubVcs(vcs_info.clone())),
        vcs_info,
        crate::theme::Theme::dark(),
        None,
        false,
        files,
        session,
        DiffSource::WorkingTree,
        InputMode::Normal,
        Vec::new(),
        None,
        None,
    )
    .expect("build app")
}

/// Rendered row showing `line_idx` of a hunk, in either layout.
fn row(app: &App, file: usize, hunk: usize, line_idx: usize) -> usize {
    app.line_annotations
        .iter()
        .position(|annotation| match *annotation {
            AnnotatedLine::DiffLine {
                file_idx,
                hunk_idx,
                line_idx: idx,
                ..
            } => (file_idx, hunk_idx, idx) == (file, hunk, line_idx),
            AnnotatedLine::SideBySideLine {
                file_idx,
                hunk_idx,
                del_line_idx,
                add_line_idx,
                ..
            } => {
                (file_idx, hunk_idx) == (file, hunk)
                    && (del_line_idx == Some(line_idx) || add_line_idx == Some(line_idx))
            }
            _ => false,
        })
        .expect("row for diff line")
}

fn message(app: &App) -> Option<String> {
    app.message.as_ref().map(|m| m.content.clone())
}

#[test]
fn n_steps_each_change_across_files_in_both_layouts() {
    for mode in [DiffViewMode::Unified, DiffViewMode::SideBySide] {
        let mut app = app_with(vec![file("a.rs"), file("b.rs")]);
        app.diff_view_mode = mode;
        app.rebuild_annotations();
        app.diff_state.cursor_line = 0;

        // Context between the two edits of hunk 0 makes them two changes;
        // the all-files view runs straight on into b.rs.
        let expected = [
            row(&app, 0, 0, 1),
            row(&app, 0, 0, 4),
            row(&app, 0, 1, 0),
            row(&app, 1, 0, 1),
            row(&app, 1, 0, 4),
            row(&app, 1, 1, 0),
        ];
        for &target in &expected {
            app.next_change();
            assert_eq!(app.diff_state.cursor_line, target, "{mode:?}");
        }
        assert_eq!(app.diff_state.current_file_idx, 1);

        app.next_change();
        assert_eq!(app.diff_state.cursor_line, row(&app, 1, 1, 0));
        assert_eq!(message(&app).as_deref(), Some("No more changes"));

        for &target in expected.iter().rev().skip(1) {
            app.prev_change();
            assert_eq!(app.diff_state.cursor_line, target, "{mode:?}");
        }
        assert_eq!(app.diff_state.current_file_idx, 0);

        app.prev_change();
        assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 1));
        assert_eq!(message(&app).as_deref(), Some("No earlier changes"));
    }
}

#[test]
fn n_from_inside_a_change_skips_the_rest_of_it() {
    let mut app = app_with(vec![file("a.rs")]);
    // The addition half of the replaced line, in the unified layout.
    app.diff_state.cursor_line = row(&app, 0, 0, 2);
    app.primed_walk_next = true;

    app.next_change();

    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 4));
    assert!(!app.primed_walk_next);
}

#[test]
fn shift_n_goes_to_the_start_of_the_current_change_first() {
    let mut app = app_with(vec![file("a.rs")]);

    // Inside a change, below its first row: that change's first row.
    app.diff_state.cursor_line = row(&app, 0, 0, 2);
    app.prev_change();
    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 1));

    // On a change's first row: the previous change.
    app.diff_state.cursor_line = row(&app, 0, 1, 0);
    app.prev_change();
    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 4));

    // On context after a change: that change.
    app.diff_state.cursor_line = row(&app, 0, 0, 5);
    app.prev_change();
    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 4));
}

#[test]
fn n_crosses_files_in_single_file_view() {
    let mut app = app_with(vec![file("a.rs"), file("b.rs")]);
    app.is_single_file_view = true;
    app.diff_state.current_file_idx = 0;
    app.rebuild_annotations();
    app.diff_state.cursor_line = *app.change_positions().last().expect("a.rs changes");

    // Past a.rs's last change, `n` lands on b.rs's first change.
    app.next_change();
    assert_eq!(app.diff_state.current_file_idx, 1);
    let first = *app.change_positions().first().expect("b.rs changes");
    assert_eq!(app.diff_state.cursor_line, first);

    // And `N` from there goes back to a.rs's last change.
    app.prev_change();
    assert_eq!(app.diff_state.current_file_idx, 0);
    let last = *app.change_positions().last().expect("a.rs changes");
    assert_eq!(app.diff_state.cursor_line, last);

    // No file before a.rs: stay put.
    app.diff_state.cursor_line = *app.change_positions().first().expect("a.rs changes");
    app.prev_change();
    assert_eq!(app.diff_state.current_file_idx, 0);
    assert_eq!(message(&app).as_deref(), Some("No earlier changes"));
}

#[test]
fn n_steps_search_matches_until_the_highlight_is_cleared() {
    let mut app = app_with(vec![file("a.rs")]);
    app.search_buffer = "needle".to_string();
    assert!(app.search_in_diff_from_cursor());
    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 0));

    // Highlighted: `n` / `N` are search, messages included.
    crate::handler::handle_diff_action(&mut app, Action::NextChange);
    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 3));
    crate::handler::handle_diff_action(&mut app, Action::NextChange);
    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 0));
    assert_eq!(
        message(&app).as_deref(),
        Some("search hit BOTTOM, continuing at TOP")
    );
    crate::handler::handle_diff_action(&mut app, Action::PrevChange);
    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 3));

    // Esc clears the highlight; `n` / `N` step changes again.
    crate::handler::handle_diff_action(&mut app, Action::ClearSearchHighlight);
    crate::handler::handle_diff_action(&mut app, Action::NextChange);
    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 4));
    crate::handler::handle_diff_action(&mut app, Action::PrevChange);
    assert_eq!(app.diff_state.cursor_line, row(&app, 0, 0, 1));
}
