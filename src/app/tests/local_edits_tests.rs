//! Lines edited in `$EDITOR` (baseline snapshot, refresh on reload) and
//! `:local`, driven through the App entry points main.rs and the `:` commands
//! use.

use crate::app::*;
use crate::editor::EditorTarget;
use crate::model::{DiffFile, DiffHunk, DiffLine, FileStatus, LineOrigin};
use crate::vcs::traits::{ResolvedRevisionRange, VcsBackend, VcsInfo, VcsType};
use std::path::PathBuf;

/// Redirects persisted-session storage to a temp dir for the current thread,
/// so `:local`'s session lookups never see another test's reviews.
struct TestReviewsDir {
    _dir: tempfile::TempDir,
}

impl TestReviewsDir {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("failed to create test reviews dir");
        crate::persistence::storage::set_test_reviews_dir(Some(dir.path().to_path_buf()));
        Self { _dir: dir }
    }
}

impl Drop for TestReviewsDir {
    fn drop(&mut self) {
        crate::persistence::storage::set_test_reviews_dir(None);
    }
}

/// Serves `files` for every kind of diff and knows the commits in `commits`.
struct FakeVcs {
    info: VcsInfo,
    files: Vec<DiffFile>,
    commits: Vec<CommitInfo>,
}

impl VcsBackend for FakeVcs {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, _hl: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        Ok(self.files.clone())
    }

    fn get_commit_range_diff(
        &self,
        _range: &ResolvedRevisionRange<'_>,
        _hl: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        Ok(self.files.clone())
    }

    fn get_working_tree_with_commits_diff(
        &self,
        _commit_ids: &[String],
        _hl: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        Ok(self.files.clone())
    }

    /// Unstaged edits only, so `:local` adds just the Unstaged row.
    fn get_change_status(&self) -> Result<VcsChangeStatus> {
        Ok(VcsChangeStatus {
            staged: false,
            unstaged: true,
        })
    }

    fn get_commits_info(&self, ids: &[String]) -> Result<Vec<CommitInfo>> {
        Ok(ids
            .iter()
            .filter_map(|id| self.commits.iter().find(|c| &c.id == id).cloned())
            .collect())
    }

    fn fetch_context_lines(
        &self,
        _path: &Path,
        _status: FileStatus,
        _ref_commit: Option<&str>,
        _start: u32,
        _end: u32,
    ) -> Result<Vec<DiffLine>> {
        Ok(Vec::new())
    }

    fn file_line_count(
        &self,
        _path: &Path,
        _status: FileStatus,
        _ref_commit: Option<&str>,
    ) -> Result<u32> {
        Ok(0)
    }
}

/// One all-added hunk over `lines` lines of `path`.
fn added_file(path: &str, lines: u32) -> DiffFile {
    let hunks = vec![DiffHunk {
        header: format!("@@ -0,0 +1,{lines} @@"),
        lines: (1..=lines)
            .map(|n| DiffLine {
                origin: LineOrigin::Addition,
                content: format!("line {n}"),
                old_lineno: None,
                new_lineno: Some(n),
                raw_content: None,
                highlighted_spans: None,
            })
            .collect(),
        old_start: 0,
        old_count: 0,
        new_start: 1,
        new_count: lines,
    }];
    let content_hash = DiffFile::compute_content_hash(&hunks);
    DiffFile {
        old_path: None,
        new_path: Some(PathBuf::from(path)),
        status: FileStatus::Added,
        hunks,
        is_binary: false,
        is_too_large: false,
        is_commit_message: false,
        content_hash,
    }
}

fn commit(id: &str) -> CommitInfo {
    CommitInfo {
        id: id.to_string(),
        short_id: id.chars().take(7).collect(),
        branch_name: None,
        parent_ids: Vec::new(),
        refs: Vec::new(),
        is_head: false,
        summary: format!("commit {id}"),
        body: None,
        author: "Test".to_string(),
        time: Utc::now(),
    }
}

fn build_app(root: &Path, files: Vec<DiffFile>, source: DiffSource) -> App {
    let vcs_info = VcsInfo {
        root_path: root.to_path_buf(),
        head_commit: "bbbbbbbbbb".into(),
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
        Box::new(FakeVcs {
            info: vcs_info.clone(),
            files: files.clone(),
            commits: vec![commit("aaaaaaaaaa"), commit("bbbbbbbbbb")],
        }),
        vcs_info,
        crate::theme::Theme::dark(),
        None,
        false,
        files,
        session,
        source,
        InputMode::Normal,
        Vec::new(),
        None,
        None,
    )
    .expect("build app")
}

fn target(root: &Path, rel: &str) -> EditorTarget {
    EditorTarget {
        path: root.join(rel),
        line: None,
        label: rel.to_string(),
    }
}

fn lines(set: &std::collections::BTreeSet<u32>) -> Vec<u32> {
    set.iter().copied().collect()
}

#[test]
fn reload_recomputes_lines_edited_since_the_editor_opened() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("f.rs"), "a\nb\nc\nd\n").unwrap();
    let mut app = build_app(root, vec![added_file("f.rs", 4)], DiffSource::WorkingTree);

    app.snapshot_local_edit_baseline(&target(root, "f.rs"));
    // What the editor saved: line 2 changed, a line appended, `c` deleted.
    std::fs::write(root.join("f.rs"), "a\nB\nd\ne\n").unwrap();
    app.reload_diff_files().expect("reload");

    let edits = app.local_edits.get(Path::new("f.rs")).expect("tracked");
    assert_eq!(lines(&edits.changed), vec![2, 4]);
    assert_eq!(lines(&edits.removed), vec![3]);
    assert_eq!(app.local_edit_mark(0, Some(2)), Some(LocalEditMark::Edited));
    assert_eq!(
        app.local_edit_mark(0, Some(3)),
        Some(LocalEditMark::RemovedAbove)
    );
    assert_eq!(app.local_edit_mark(0, Some(1)), None);
    assert_eq!(
        app.local_edit_summary(&target(root, "f.rs")).as_deref(),
        Some("2 lines edited, 1 deletion")
    );

    // A second editor launch keeps the first baseline.
    app.snapshot_local_edit_baseline(&target(root, "f.rs"));
    app.refresh_local_edits();
    assert_eq!(
        lines(&app.local_edits.get(Path::new("f.rs")).unwrap().changed),
        vec![2, 4]
    );

    // Putting the file back drops it.
    std::fs::write(root.join("f.rs"), "a\nb\nc\nd\n").unwrap();
    app.reload_diff_files().expect("reload");
    assert!(app.local_edits.get(Path::new("f.rs")).is_none());
}

#[test]
fn a_file_missing_at_launch_has_an_empty_baseline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let mut app = build_app(root, vec![added_file("new.rs", 2)], DiffSource::WorkingTree);

    app.snapshot_local_edit_baseline(&target(root, "new.rs"));
    std::fs::write(root.join("new.rs"), "x\ny\n").unwrap();
    app.refresh_local_edits();

    let edits = app.local_edits.get(Path::new("new.rs")).expect("tracked");
    assert_eq!(lines(&edits.changed), vec![1, 2]);
}

#[test]
fn edits_show_only_where_the_new_side_is_the_working_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("f.rs"), "a\n").unwrap();
    let mut app = build_app(root, vec![added_file("f.rs", 1)], DiffSource::WorkingTree);
    app.snapshot_local_edit_baseline(&target(root, "f.rs"));
    std::fs::write(root.join("f.rs"), "A\n").unwrap();
    app.refresh_local_edits();
    assert_eq!(app.local_edit_mark(0, Some(1)), Some(LocalEditMark::Edited));

    // A commit's new side is the commit, not the file on disk.
    app.diff_source = DiffSource::CommitRange(vec!["aaaaaaaaaa".into()]);
    assert_eq!(app.local_edit_mark(0, Some(1)), None);

    app.diff_source = DiffSource::StagedUnstagedAndCommits(vec!["aaaaaaaaaa".into()]);
    assert_eq!(app.local_edit_mark(0, Some(1)), Some(LocalEditMark::Edited));

    // Narrowing that review's inline selector to the commit alone puts the
    // commit back on the new side.
    app.review_commits = vec![
        App::unstaged_commit_entry(),
        App::staged_commit_entry(),
        commit("aaaaaaaaaa"),
    ];
    app.commit_selection_range = Some((2, 2));
    assert_eq!(app.local_edit_mark(0, Some(1)), None);
    app.commit_selection_range = Some((0, 2));
    assert_eq!(app.local_edit_mark(0, Some(1)), Some(LocalEditMark::Edited));

    app.clear_local_edits();
    assert_eq!(app.local_edit_mark(0, Some(1)), None);
    assert!(app.local_edits.is_empty());
}

#[test]
fn no_baseline_outside_the_repo_or_in_a_submodule_or_merge_review() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("f.rs"), "a\n").unwrap();
    std::fs::write(dir.path().join("snapshot.rs"), "a\n").unwrap();
    let mut app = build_app(&root, vec![added_file("f.rs", 1)], DiffSource::WorkingTree);

    // A PR snapshot in the temp dir.
    app.snapshot_local_edit_baseline(&EditorTarget {
        path: dir.path().join("snapshot.rs"),
        line: None,
        label: "snapshot.rs".into(),
    });
    assert!(app.local_edits.is_empty());

    app.input_mode = InputMode::MergeConflict;
    app.snapshot_local_edit_baseline(&target(&root, "f.rs"));
    assert!(app.local_edits.is_empty());

    app.input_mode = InputMode::Normal;
    app.diff_source = DiffSource::Submodule(Box::new(SubmoduleDiffSource {
        path: PathBuf::from("sub"),
        staged: false,
    }));
    app.snapshot_local_edit_baseline(&target(&root, "f.rs"));
    assert!(app.local_edits.is_empty());
}

#[test]
fn local_toggles_a_commit_range_with_the_working_tree() {
    let _reviews = TestReviewsDir::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let ids = vec!["aaaaaaaaaa".to_string(), "bbbbbbbbbb".to_string()];
    let mut app = build_app(
        dir.path(),
        vec![added_file("a.rs", 1), added_file("b.rs", 1)],
        DiffSource::CommitRange(ids.clone()),
    );
    let b_idx = app
        .diff_files
        .iter()
        .position(|f| f.display_path() == Path::new("b.rs"))
        .expect("b.rs loaded");
    app.jump_to_file(b_idx);

    app.toggle_compare_with_local();
    assert_eq!(
        app.diff_source,
        DiffSource::StagedUnstagedAndCommits(ids.clone())
    );
    assert_eq!(
        app.session.diff_source,
        SessionDiffSource::StagedUnstagedAndCommits
    );
    assert!(App::is_unstaged_commit(&app.review_commits[0]));
    assert!(!app.review_commits.iter().any(App::is_staged_commit));
    assert_eq!(
        app.current_file_path().map(PathBuf::as_path),
        Some(Path::new("b.rs"))
    );
    assert_eq!(
        app.message.as_ref().map(|m| m.content.as_str()),
        Some("Comparing aaaaaaa..bbbbbbb with local (working tree)")
    );

    app.toggle_compare_with_local();
    assert_eq!(app.diff_source, DiffSource::CommitRange(ids));
    assert_eq!(app.session.diff_source, SessionDiffSource::CommitRange);
    assert!(!app.review_commits.iter().any(App::is_special_commit));
}

#[test]
fn local_is_refused_where_there_is_no_range_to_compare() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = build_app(
        dir.path(),
        vec![added_file("a.rs", 1)],
        DiffSource::WorkingTree,
    );

    app.toggle_compare_with_local();
    assert_eq!(app.diff_source, DiffSource::WorkingTree);
    assert_eq!(
        app.message.as_ref().map(|m| m.content.as_str()),
        Some("Already comparing with the working tree")
    );

    let submodule = DiffSource::Submodule(Box::new(SubmoduleDiffSource {
        path: PathBuf::from("sub"),
        staged: true,
    }));
    app.diff_source = submodule.clone();
    app.toggle_compare_with_local();
    assert_eq!(app.diff_source, submodule);
    let message = app.message.as_ref().expect("error shown");
    assert_eq!(message.message_type, MessageType::Error);
    assert!(message.content.contains("not available"));
}
