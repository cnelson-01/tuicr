//! Find-in-files results view: opening, navigation, and where Enter goes.

use crate::app::*;
use crate::model::FileStatus;
use crate::vcs::traits::VcsType;

struct DummyVcs {
    info: VcsInfo,
}

impl VcsBackend for DummyVcs {
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
}

fn build_app() -> App {
    let vcs_info = VcsInfo {
        root_path: PathBuf::from("/repo"),
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
        }),
        vcs_info,
        Theme::dark(),
        None,
        false,
        Vec::new(),
        session,
        DiffSource::WorkingTree,
        InputMode::Normal,
        Vec::new(),
        None,
        None,
    )
    .expect("failed to build test app")
}

fn hit(path: &str, line: u32) -> FindHit {
    FindHit {
        path: PathBuf::from(path),
        line,
        column: 1,
        text: format!("match on {line}"),
    }
}

#[test]
fn opening_results_enters_the_view_and_no_hits_stay_put() {
    let mut app = build_app();

    app.open_find_results("needle", Vec::new(), "rg");
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(app.message.is_some(), "an empty result says so");

    app.open_find_results("needle", vec![hit("a.rs", 3), hit("b.rs", 9)], "rg");
    assert_eq!(app.input_mode, InputMode::FindResults);
    assert_eq!(app.find.query, "needle");
    assert_eq!(app.find.hits.len(), 2);
    assert_eq!(app.find.selected, 0);
    assert!(!app.find.truncated);
}

#[test]
fn results_beyond_the_cap_are_dropped_and_flagged() {
    let mut app = build_app();
    let hits: Vec<FindHit> = (0..FIND_MAX_HITS as u32 + 5)
        .map(|i| hit("big.rs", i + 1))
        .collect();
    app.open_find_results("x", hits, "git grep");
    assert_eq!(app.find.hits.len(), FIND_MAX_HITS);
    assert!(app.find.truncated);
}

#[test]
fn navigation_clamps_to_the_hit_list() {
    let mut app = build_app();
    app.open_find_results(
        "n",
        vec![hit("a.rs", 1), hit("a.rs", 2), hit("a.rs", 3)],
        "rg",
    );
    app.find_select_down(10);
    assert_eq!(app.find.selected, 2);
    app.find_select_up(1);
    assert_eq!(app.find.selected, 1);
    app.find_select_first();
    assert_eq!(app.find.selected, 0);
    app.find_select_up(5);
    assert_eq!(app.find.selected, 0);
    app.find_select_last();
    assert_eq!(app.find.selected, 2);
}

#[test]
fn enter_opens_the_editor_when_the_file_is_not_in_the_diff() {
    // given no diff on screen, so no hit can be jumped to
    let mut app = build_app();
    app.open_find_results("n", vec![hit("src/lib.rs", 42)], "rg");

    // when
    app.find_open_selected(false);

    // then the results view closes and an editor launch is queued at the line
    assert_eq!(app.input_mode, InputMode::Normal);
    let target = app.take_pending_editor_target().expect("editor target");
    assert_eq!(target.path, PathBuf::from("/repo/src/lib.rs"));
    assert_eq!(target.line, Some(42));

    // and the results are still there for `:find` to bring back
    app.reopen_find_results();
    assert_eq!(app.input_mode, InputMode::FindResults);
}

#[test]
fn reopen_without_results_prompts_instead() {
    let mut app = build_app();
    app.reopen_find_results();
    assert_eq!(app.input_mode, InputMode::Command);
    assert_eq!(app.command_buffer, "find ");
}
