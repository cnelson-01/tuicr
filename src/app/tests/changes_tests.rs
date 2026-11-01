//! The Changes view (`<leader>C`): opening and closing, the pane's own
//! cursor, lazy per-entry previews, staging and discard operations, the
//! refresh after every operation, the diff watch, the `:` commands and the
//! key maps. Everything runs against a scripted `VcsBackend` whose
//! mutators move entries between groups, so a refresh sees the result the
//! way it would with real git. Rendering the pane is `ui::changes_pane`'s
//! job; the only render assertion here is the header chunk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::*;
use crate::forge::traits::{ForgeRepository, PrSessionKey};
use crate::handler::{
    handle_changes_pane_action, handle_command_action, handle_commit_select_action,
    handle_confirm_action, handle_diff_action, handle_mouse_event,
};
use crate::input::{Action, map_changes_pane_mode, map_key_to_action, map_log_pane_mode};
use crate::model::FileStatus;
use crate::vcs::traits::VcsType;
use crate::vcs::{
    ChangeBulkOp, ChangeDiffKey, ChangeEntry, ChangeEntryKind, ChangeGroup, ChangeSet,
    ChangeStatus, SubmoduleChange,
};

/// Points `load_or_create_session` / `ensure_ephemeral_session_file` at a
/// throwaway directory for the life of a test, so nothing here ever touches
/// the real reviews directory.
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

// ---------------------------------------------------------------------------
// The scripted backend
// ---------------------------------------------------------------------------

/// Shared handles into the mock, so a test can edit the change set under a
/// running `App` (an external `git add`) and read the call log.
#[derive(Clone, Default)]
struct Handles {
    set: Arc<Mutex<ChangeSet>>,
    diffs: Arc<Mutex<HashMap<ChangeDiffKey, Vec<DiffFile>>>>,
    submodules: Arc<Mutex<HashMap<PathBuf, ChangeSet>>>,
    working_tree: Arc<Mutex<Vec<DiffFile>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

/// A `VcsBackend` for the Changes view: answers from a stored `ChangeSet`,
/// logs every call, and has each mutator move the entry the way git would,
/// so the refresh that follows an operation sees the move. `run_git` stays
/// at the trait default, so `refresh_repo_status` yields `None`.
struct ChangesMock {
    info: VcsInfo,
    handles: Handles,
}

impl ChangesMock {
    fn log(&self, call: impl Into<String>) {
        self.handles.calls.lock().unwrap().push(call.into());
    }

    fn mutate(&self, f: impl FnOnce(&mut ChangeSet)) {
        let mut set = self.handles.set.lock().unwrap();
        f(&mut set);
        set.sort();
    }
}

fn key_label(key: &ChangeDiffKey) -> String {
    format!(
        "{} {}",
        key.group.short_label(),
        key.display_path().display()
    )
}

fn sub_label(submodule: Option<&Path>) -> String {
    match submodule {
        Some(sub) => format!("Some({})", sub.display()),
        None => "None".to_string(),
    }
}

fn move_entry(
    set: &mut ChangeSet,
    from: ChangeGroup,
    to: ChangeGroup,
    path: &Path,
    status: Option<ChangeStatus>,
) {
    let group = set.group_mut(from);
    let Some(pos) = group.iter().position(|entry| entry.path == path) else {
        return;
    };
    let mut entry = group.remove(pos);
    if let Some(status) = status {
        entry.status = status;
    }
    if set.find(to, path).is_none() {
        set.push(to, entry);
    }
}

impl VcsBackend for ChangesMock {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, _highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        self.log("get_working_tree_diff");
        let files = self.handles.working_tree.lock().unwrap().clone();
        if files.is_empty() {
            Err(TuicrError::NoChanges)
        } else {
            Ok(files)
        }
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

    fn get_change_set(&self) -> Result<ChangeSet> {
        self.log("get_change_set");
        Ok(self.handles.set.lock().unwrap().clone())
    }

    fn get_submodule_change_set(&self, submodule: &Path) -> Result<ChangeSet> {
        self.log(format!("get_submodule_change_set {}", submodule.display()));
        self.handles
            .submodules
            .lock()
            .unwrap()
            .get(submodule)
            .cloned()
            .ok_or_else(|| {
                TuicrError::VcsCommand(format!("no submodule at {}", submodule.display()))
            })
    }

    fn get_change_entry_diff(
        &self,
        key: &ChangeDiffKey,
        entry: &ChangeEntry,
        _highlighter: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        self.log(format!("get_change_entry_diff {}", key_label(key)));
        let scripted = self.handles.diffs.lock().unwrap().get(key).cloned();
        Ok(scripted.unwrap_or_else(|| vec![preview_file(key, entry)]))
    }

    fn stage_change_entry(&self, key: &ChangeDiffKey, entry: &ChangeEntry) -> Result<()> {
        self.log(format!("stage_change_entry {}", key_label(key)));
        if key.submodule.is_none() {
            let path = entry.path.clone();
            self.mutate(|set| match key.group {
                ChangeGroup::Untracked => move_entry(
                    set,
                    ChangeGroup::Untracked,
                    ChangeGroup::Staged,
                    &path,
                    Some(ChangeStatus::Added),
                ),
                ChangeGroup::Unstaged => {
                    move_entry(set, ChangeGroup::Unstaged, ChangeGroup::Staged, &path, None)
                }
                ChangeGroup::Conflicted => move_entry(
                    set,
                    ChangeGroup::Conflicted,
                    ChangeGroup::Staged,
                    &path,
                    Some(ChangeStatus::Modified),
                ),
                ChangeGroup::Staged => {}
            });
        }
        Ok(())
    }

    fn unstage_change_entry(&self, key: &ChangeDiffKey, entry: &ChangeEntry) -> Result<()> {
        self.log(format!("unstage_change_entry {}", key_label(key)));
        if key.submodule.is_none() {
            let path = entry.path.clone();
            let added = entry.status == ChangeStatus::Added;
            self.mutate(|set| {
                if added {
                    move_entry(
                        set,
                        ChangeGroup::Staged,
                        ChangeGroup::Untracked,
                        &path,
                        Some(ChangeStatus::Untracked),
                    );
                } else {
                    move_entry(set, ChangeGroup::Staged, ChangeGroup::Unstaged, &path, None);
                }
            });
        }
        Ok(())
    }

    fn discard_change_entry(&self, key: &ChangeDiffKey, entry: &ChangeEntry) -> Result<()> {
        self.log(format!("discard_change_entry {}", key_label(key)));
        if key.submodule.is_none() {
            let path = entry.path.clone();
            self.mutate(|set| set.group_mut(key.group).retain(|e| e.path != path));
        }
        Ok(())
    }

    fn discard_change_group(
        &self,
        group: ChangeGroup,
        entries: &[ChangeEntry],
        submodule: Option<&Path>,
    ) -> Result<usize> {
        self.log(format!(
            "discard_change_group {} {} {}",
            group.short_label(),
            entries.len(),
            sub_label(submodule)
        ));
        if submodule.is_none() {
            self.mutate(|set| set.group_mut(group).clear());
        }
        Ok(0)
    }

    fn change_bulk_op(&self, op: ChangeBulkOp, submodule: Option<&Path>) -> Result<()> {
        self.log(format!("change_bulk_op {op:?} {}", sub_label(submodule)));
        if submodule.is_some() {
            return Ok(());
        }
        self.mutate(|set| {
            let stage_tracked = |set: &mut ChangeSet| {
                for entry in std::mem::take(&mut set.unstaged) {
                    if set.find(ChangeGroup::Staged, &entry.path).is_none() {
                        set.push(ChangeGroup::Staged, entry);
                    }
                }
            };
            let stage_untracked = |set: &mut ChangeSet| {
                for mut entry in std::mem::take(&mut set.untracked) {
                    if entry.is_embedded_repo() {
                        set.push(ChangeGroup::Untracked, entry);
                        continue;
                    }
                    entry.status = ChangeStatus::Added;
                    set.push(ChangeGroup::Staged, entry);
                }
            };
            match op {
                ChangeBulkOp::StageTracked => stage_tracked(set),
                ChangeBulkOp::StageUntracked => stage_untracked(set),
                ChangeBulkOp::StageAll => {
                    stage_tracked(set);
                    stage_untracked(set);
                }
                ChangeBulkOp::UnstageAll => {
                    for mut entry in std::mem::take(&mut set.staged) {
                        if entry.status == ChangeStatus::Added {
                            entry.status = ChangeStatus::Untracked;
                            set.push(ChangeGroup::Untracked, entry);
                        } else if set.find(ChangeGroup::Unstaged, &entry.path).is_none() {
                            set.push(ChangeGroup::Unstaged, entry);
                        }
                    }
                }
            }
        });
        Ok(())
    }

    fn commit(&self, _message: &str, _amend: bool) -> Result<String> {
        self.log("commit");
        self.mutate(|set| set.staged.clear());
        Ok("abc1234".to_string())
    }

    fn stash_push(&self, _message: &str) -> Result<()> {
        self.log("stash_push");
        self.mutate(|set| *set = ChangeSet::default());
        Ok(())
    }

    fn stage_file(&self, path: &Path) -> Result<()> {
        self.log(format!("stage_file {}", path.display()));
        Ok(())
    }

    fn unstage_file(&self, path: &Path) -> Result<()> {
        self.log(format!("unstage_file {}", path.display()));
        Ok(())
    }

    fn discard_file(&self, path: &Path) -> Result<()> {
        self.log(format!("discard_file {}", path.display()));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const OLD_ID: &str = "1111111aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEW_ID: &str = "2222222bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn file(path: &str, status: ChangeStatus) -> ChangeEntry {
    ChangeEntry::file(path, status)
}

fn key(group: ChangeGroup, path: &str) -> ChangeDiffKey {
    ChangeDiffKey::new(group, path)
}

fn gitlink(path: &str, status: ChangeStatus, change: SubmoduleChange) -> ChangeEntry {
    ChangeEntry {
        path: PathBuf::from(path),
        old_path: None,
        status,
        kind: ChangeEntryKind::Submodule(change),
    }
}

/// An unstaged pointer move: the checkout is one commit ahead of the index.
fn moved_pointer() -> SubmoduleChange {
    SubmoduleChange {
        head_id: Some(OLD_ID.to_string()),
        index_id: Some(OLD_ID.to_string()),
        workdir_id: Some(NEW_ID.to_string()),
        ahead: 1,
        registered: true,
        ..Default::default()
    }
}

/// Modified tracked content inside the submodule, pointer unchanged.
fn dirty_only() -> SubmoduleChange {
    SubmoduleChange {
        head_id: Some(OLD_ID.to_string()),
        index_id: Some(OLD_ID.to_string()),
        workdir_id: Some(OLD_ID.to_string()),
        modified_content: true,
        registered: true,
        ..Default::default()
    }
}

/// `f` staged and unstaged, `g` unstaged, `n.txt` untracked. Rows:
/// 0 Staged, 1 f, 2 Unstaged, 3 f, 4 g, 5 Unversioned files, 6 n.txt.
fn sample_set() -> ChangeSet {
    let mut set = ChangeSet::default();
    set.push(ChangeGroup::Staged, file("f", ChangeStatus::Modified));
    set.push(ChangeGroup::Unstaged, file("f", ChangeStatus::Modified));
    set.push(ChangeGroup::Unstaged, file("g", ChangeStatus::Modified));
    set.push(
        ChangeGroup::Untracked,
        file("n.txt", ChangeStatus::Untracked),
    );
    set.sort();
    set
}

const ROW_STAGED_F: usize = 1;
const ROW_UNSTAGED_GROUP: usize = 2;
const ROW_UNSTAGED_F: usize = 3;
const ROW_UNSTAGED_G: usize = 4;
const ROW_UNTRACKED_N: usize = 6;

/// Only a dirty submodule `lib`, with `a.txt` modified inside it.
fn dirty_submodule_set() -> (ChangeSet, ChangeSet) {
    let mut set = ChangeSet::default();
    set.push(
        ChangeGroup::Unstaged,
        gitlink("lib", ChangeStatus::Modified, dirty_only()),
    );
    let mut inner = ChangeSet::default();
    inner.push(ChangeGroup::Unstaged, file("a.txt", ChangeStatus::Modified));
    (set, inner)
}

fn one_line_hunk(text: &str) -> DiffHunk {
    DiffHunk {
        header: "@@ -0,0 +1 @@".to_string(),
        lines: vec![DiffLine {
            origin: LineOrigin::Addition,
            content: text.to_string(),
            raw_content: None,
            old_lineno: None,
            new_lineno: Some(1),
            highlighted_spans: None,
        }],
        old_start: 0,
        old_count: 0,
        new_start: 1,
        new_count: 1,
    }
}

/// The diff the mock synthesizes for an entry: one added line naming the
/// side, so the Staged and Unstaged previews of one path hash differently.
fn preview_file(key: &ChangeDiffKey, entry: &ChangeEntry) -> DiffFile {
    let path = key.display_path();
    let hunks = vec![one_line_hunk(&format!(
        "{} {}",
        key.group.short_label(),
        path.display()
    ))];
    DiffFile {
        old_path: Some(path.clone()),
        new_path: Some(path),
        status: entry.status.as_file_status(),
        content_hash: DiffFile::compute_content_hash(&hunks),
        hunks,
        is_binary: false,
        is_too_large: false,
        is_commit_message: false,
    }
}

fn plain_file(path: &str) -> DiffFile {
    let hunks = vec![one_line_hunk(path)];
    DiffFile {
        old_path: Some(PathBuf::from(path)),
        new_path: Some(PathBuf::from(path)),
        status: FileStatus::Modified,
        content_hash: DiffFile::compute_content_hash(&hunks),
        hunks,
        is_binary: false,
        is_too_large: false,
        is_commit_message: false,
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
        time: chrono::Utc::now(),
    }
}

fn pull_request_source() -> DiffSource {
    DiffSource::PullRequest(Box::new(PullRequestDiffSource {
        key: PrSessionKey::new(
            ForgeRepository::github("github.com", "agavra", "tuicr"),
            125,
            "abcdef0123".to_string(),
        ),
        base_sha: "0000".to_string(),
        title: "test pr".to_string(),
        url: "https://github.com/agavra/tuicr/pull/125".to_string(),
        head_ref_name: "feat".to_string(),
        base_ref_name: "main".to_string(),
        state: "OPEN".to_string(),
        closed: false,
        merged: false,
    }))
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    app: App,
    handles: Handles,
    _reviews: TestReviewsDir,
    _root: tempfile::TempDir,
}

impl Harness {
    fn calls(&self) -> Vec<String> {
        self.handles.calls.lock().unwrap().clone()
    }

    fn clear_calls(&self) {
        self.handles.calls.lock().unwrap().clear();
    }

    fn set_change_set(&self, set: ChangeSet) {
        let mut stored = self.handles.set.lock().unwrap();
        *stored = set;
        stored.sort();
    }

    fn message(&self) -> String {
        self.app
            .message
            .as_ref()
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }

    fn selected_idx(&self) -> usize {
        self.app.changes.as_ref().expect("view").selected_idx()
    }

    fn selected_key(&self) -> Option<ChangeDiffKey> {
        self.app.changes_selected_entry().map(|(key, _)| key)
    }

    fn previewed_key(&self) -> Option<ChangeDiffKey> {
        match &self.app.diff_source {
            DiffSource::Changes(key) => key.clone(),
            _ => None,
        }
    }

    fn pane(&mut self, action: Action) {
        handle_changes_pane_action(&mut self.app, action);
    }

    fn command(&mut self, cmd: &str) {
        self.app.enter_command_mode();
        self.app.command_buffer = cmd.to_string();
        handle_command_action(&mut self.app, Action::SubmitInput);
    }

    fn fetches(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("get_change_entry_diff"))
            .collect()
    }
}

fn build(
    set: ChangeSet,
    vcs_type: VcsType,
    diff_source: DiffSource,
    input_mode: InputMode,
) -> Harness {
    let reviews = TestReviewsDir::new();
    let root = tempfile::tempdir().expect("tempdir");
    let vcs_info = VcsInfo {
        root_path: root.path().to_path_buf(),
        head_commit: "abc1234def".to_string(),
        branch_name: Some("main".to_string()),
        vcs_type,
    };
    let handles = Handles::default();
    *handles.set.lock().unwrap() = set;
    let session_source = match &diff_source {
        DiffSource::StagedAndUnstaged => SessionDiffSource::StagedAndUnstaged,
        DiffSource::PullRequest(_) => SessionDiffSource::PullRequest,
        _ => SessionDiffSource::CommitRange,
    };
    let session = ReviewSession::new(
        vcs_info.root_path.clone(),
        vcs_info.head_commit.clone(),
        vcs_info.branch_name.clone(),
        session_source,
    );
    let app = App::build(
        Box::new(ChangesMock {
            info: vcs_info.clone(),
            handles: handles.clone(),
        }),
        vcs_info,
        Theme::dark(),
        None,
        false,
        Vec::new(),
        session,
        diff_source,
        input_mode,
        Vec::new(),
        None,
        None,
    )
    .expect("failed to build test app");
    Harness {
        app,
        handles,
        _reviews: reviews,
        _root: root,
    }
}

fn open(set: ChangeSet) -> Harness {
    let mut h = build(
        set,
        VcsType::Git,
        DiffSource::StagedAndUnstaged,
        InputMode::Normal,
    );
    h.app.open_changes_view();
    h
}

/// Delivers a diff-watch event the way `diff_watch_tests.rs` does: on a
/// fresh channel marked as the in-flight reload, then one poll.
fn deliver(app: &mut App, request: DiffWatchReloadRequest, event: DiffWatchReloadEvent) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(event).unwrap();
    app.diff_watch_reload = Some(DiffWatchReload { request, rx });
    app.poll_diff_watch_changes()
}

fn watch_request(app: &App) -> DiffWatchReloadRequest {
    DiffWatchReloadRequest {
        diff_source: app.diff_source.clone(),
        commit_selection_range: None,
    }
}

fn click(app: &mut App, x: u16, y: u16) {
    handle_mouse_event(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn pane_rects(app: &mut App) -> Rect {
    let inner = Rect::new(1, 2, 30, 20);
    app.file_list_area = Some(Rect::new(0, 1, 32, 22));
    app.file_list_inner_area = Some(inner);
    inner
}

fn key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut text = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

// ---------------------------------------------------------------------------
// Opening and closing
// ---------------------------------------------------------------------------

#[test]
fn open_changes_view_shows_the_first_entry_diff_and_focuses_the_pane() {
    let h = open(sample_set());

    assert!(h.app.changes_active());
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert_eq!(h.app.focused_panel, FocusedPanel::FileList);
    assert!(h.app.show_file_list);
    assert_eq!(h.selected_idx(), ROW_STAGED_F);
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Staged, "f")));
    assert_eq!(h.app.diff_files.len(), 1);
    assert_eq!(
        h.calls(),
        vec!["get_change_set", "get_change_entry_diff staged f"]
    );
    assert!(h.message().contains("space stages"));
}

#[test]
fn open_changes_view_from_the_selector_lands_in_normal_mode() {
    let mut h = build(
        sample_set(),
        VcsType::Git,
        DiffSource::CommitRange(vec!["c1".to_string()]),
        InputMode::CommitSelect,
    );

    handle_commit_select_action(&mut h.app, Action::OpenChangesView);

    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert!(h.app.changes_active());
    assert_eq!(h.app.focused_panel, FocusedPanel::FileList);
    assert_eq!(
        h.app.session.diff_source,
        SessionDiffSource::StagedAndUnstaged,
        "a commit-range session is replaced by the working-tree one"
    );
}

#[test]
fn log_pane_autoshow_after_opening_from_the_selector_keeps_the_pane_focused() {
    let mut h = build(
        sample_set(),
        VcsType::Git,
        DiffSource::CommitRange(vec!["c1".to_string()]),
        InputMode::CommitSelect,
    );
    h.app.log_pane_wanted = true;
    h.app.log_pane = None;

    handle_commit_select_action(&mut h.app, Action::OpenChangesView);
    assert!(h.app.changes_focused());

    // The main loop opens the log pane on the first Normal-mode tick; the
    // Changes pane the user just asked for must keep the keys.
    assert!(h.app.poll_log_pane_autoshow());
    assert!(h.app.log_pane.is_some());
    assert!(h.app.changes_focused());
    assert_eq!(
        map_changes_pane_mode(
            key_event(KeyCode::Char('j'), KeyModifiers::NONE),
            ';',
            false
        ),
        Action::CursorDown(1)
    );
}

#[test]
fn changes_command_from_the_selector_returns_to_normal_not_the_selector() {
    let mut h = build(
        sample_set(),
        VcsType::Git,
        DiffSource::CommitRange(vec!["c1".to_string()]),
        InputMode::CommitSelect,
    );

    h.command("changes");

    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert!(h.app.changes_active());
}

#[test]
fn open_changes_view_on_hg_loads_the_flat_working_tree_review() {
    let mut h = build(
        ChangeSet::default(),
        VcsType::Mercurial,
        DiffSource::StagedAndUnstaged,
        InputMode::Normal,
    );
    *h.handles.working_tree.lock().unwrap() = vec![plain_file("w.txt")];

    h.app.open_changes_view();

    assert!(h.app.changes.is_none());
    assert!(!h.app.changes_active());
    assert_eq!(h.app.diff_source, DiffSource::WorkingTree);
    assert_eq!(h.app.diff_files.len(), 1);
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert_eq!(h.app.focused_panel, FocusedPanel::FileList);
    assert_eq!(
        h.message(),
        "No index on hg: showing the working copy as a flat list"
    );
    assert!(!h.calls().iter().any(|c| c == "get_change_set"));
}

#[test]
fn open_changes_view_on_jj_names_the_colocated_case() {
    let mut h = build(
        ChangeSet::default(),
        VcsType::Jujutsu,
        DiffSource::StagedAndUnstaged,
        InputMode::Normal,
    );

    h.app.open_changes_view();

    assert_eq!(h.app.diff_source, DiffSource::WorkingTree);
    assert!(h.app.diff_files.is_empty(), "a clean working copy is empty");
    assert_eq!(
        h.message(),
        "No index on jj: showing the working copy as a flat list (colocated jj included)"
    );
}

#[test]
fn open_changes_view_in_pr_mode_warns() {
    let mut h = build(
        sample_set(),
        VcsType::Git,
        pull_request_source(),
        InputMode::Normal,
    );

    h.app.open_changes_view();

    assert!(!h.app.changes_active());
    assert!(matches!(h.app.diff_source, DiffSource::PullRequest(_)));
    assert!(h.message().contains("leave the PR review first"));
    assert!(h.calls().is_empty());
}

#[test]
fn open_changes_view_in_pristine_mode_warns() {
    let mut h = build(
        sample_set(),
        VcsType::Git,
        DiffSource::StagedAndUnstaged,
        InputMode::Normal,
    );
    h.app.is_pristine_mode = true;

    h.app.open_changes_view();

    assert!(!h.app.changes_active());
    assert_eq!(h.message(), "The Changes view needs a repository checkout");
    assert!(h.calls().is_empty());
}

#[test]
fn closing_the_view_restores_the_working_tree_review() {
    let mut h = open(sample_set());
    *h.handles.working_tree.lock().unwrap() = vec![plain_file("w.txt")];
    h.clear_calls();

    h.app.close_changes_view();

    assert!(h.app.changes.is_none());
    assert_eq!(h.app.diff_source, DiffSource::StagedAndUnstaged);
    assert_eq!(h.calls(), vec!["get_working_tree_diff"]);
    assert_eq!(h.app.diff_files.len(), 1);
}

#[test]
fn toggle_changes_view_cycles_open_focus_close() {
    let mut h = build(
        sample_set(),
        VcsType::Git,
        DiffSource::StagedAndUnstaged,
        InputMode::Normal,
    );

    h.app.toggle_changes_view();
    assert!(h.app.changes_active());
    assert_eq!(h.app.focused_panel, FocusedPanel::FileList);

    h.app.focused_panel = FocusedPanel::Diff;
    h.clear_calls();
    h.app.toggle_changes_view();
    assert!(h.app.changes_active());
    assert_eq!(h.app.focused_panel, FocusedPanel::FileList);
    assert_eq!(
        h.calls().first().map(String::as_str),
        Some("get_change_set"),
        "refocusing re-reads the change set"
    );

    h.app.toggle_changes_view();
    assert!(!h.app.changes_active());
    assert_eq!(h.app.diff_source, DiffSource::StagedAndUnstaged);
}

#[test]
fn set_changes_visible_false_closes() {
    let mut h = open(sample_set());

    h.app.set_changes_visible(false);
    assert!(!h.app.changes_active());

    h.app.set_changes_visible(true);
    assert!(h.app.changes_active());
}

#[test]
fn loading_another_review_keeps_the_view_state_for_the_next_open() {
    let mut h = open(sample_set());
    h.pane(Action::ChangesToggleGrouping);
    h.app.diff_source = DiffSource::CommitRange(vec!["c1".to_string()]);
    assert!(!h.app.changes_active());
    assert!(h.app.changes_rows().is_empty());

    h.app.open_changes_view();

    assert!(h.app.changes_active());
    assert!(h.app.changes.as_ref().expect("view").group_by_directory);
}

// ---------------------------------------------------------------------------
// Cursor and previews
// ---------------------------------------------------------------------------

#[test]
fn moving_the_cursor_onto_an_entry_loads_its_diff_once_and_caches_it() {
    let mut h = open(sample_set());

    h.pane(Action::CursorDown(1));
    h.pane(Action::CursorDown(1));
    h.pane(Action::CursorDown(1));
    h.pane(Action::CursorUp(1));

    assert_eq!(h.selected_key(), Some(key(ChangeGroup::Unstaged, "f")));
    assert_eq!(
        h.fetches(),
        vec![
            "get_change_entry_diff staged f",
            "get_change_entry_diff unstaged f",
            "get_change_entry_diff unstaged g",
        ],
        "one read per distinct key; going back is a cache hit"
    );
}

#[test]
fn moving_the_cursor_does_not_snap_back_after_the_preview_installs() {
    let mut h = open(sample_set());

    h.pane(Action::CursorDown(1));
    assert_eq!(h.selected_idx(), ROW_UNSTAGED_GROUP);
    h.pane(Action::CursorDown(1));

    assert_eq!(h.selected_idx(), ROW_UNSTAGED_F);
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Unstaged, "f")));
}

#[test]
fn group_rows_leave_the_preview_as_it_is() {
    let mut h = open(sample_set());

    h.pane(Action::CursorUp(1));

    assert_eq!(h.selected_idx(), 0);
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Staged, "f")));
    assert_eq!(h.app.diff_files.len(), 1);
}

#[test]
fn g_and_capital_g_move_to_the_first_and_last_rows() {
    let mut h = open(sample_set());

    h.pane(Action::GoToBottom);
    assert_eq!(h.selected_idx(), ROW_UNTRACKED_N);
    assert_eq!(
        h.previewed_key(),
        Some(key(ChangeGroup::Untracked, "n.txt"))
    );

    h.pane(Action::GoToTop);
    assert_eq!(h.selected_idx(), 0);
}

#[test]
fn preview_registers_the_file_without_invalidating_its_review() {
    let mut h = open(sample_set());
    let path = PathBuf::from("f");
    {
        let review = h
            .app
            .session
            .files
            .get_mut(&path)
            .expect("the preview registered f with the session");
        review.reviewed = true;
        review.reviewed_hunks.insert("hunk-key".to_string());
    }

    h.app.changes_select(ROW_UNSTAGED_F);
    h.app.changes_select(ROW_STAGED_F);

    let review = &h.app.session.files[&path];
    assert!(review.reviewed);
    assert!(review.reviewed_hunks.contains("hunk-key"));
}

#[test]
fn next_file_from_the_diff_moves_to_the_next_entry() {
    let mut h = open(sample_set());
    h.app.focused_panel = FocusedPanel::Diff;

    handle_diff_action(&mut h.app, Action::NextFile);
    assert_eq!(h.selected_idx(), ROW_UNSTAGED_F);
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Unstaged, "f")));

    handle_diff_action(&mut h.app, Action::PrevFile);
    assert_eq!(h.selected_idx(), ROW_STAGED_F);
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Staged, "f")));
}

#[test]
fn rows_are_rebuilt_only_on_state_change() {
    let mut set = sample_set();
    set.push(
        ChangeGroup::Unstaged,
        file("src/h.rs", ChangeStatus::Modified),
    );
    set.sort();
    let mut h = open(set);

    let first = h.app.changes_rows().to_vec();
    let second = h.app.changes_rows().to_vec();
    assert_eq!(first, second);
    assert!(
        !first
            .iter()
            .any(|row| matches!(row, ChangesRow::Directory { .. }))
    );

    h.pane(Action::ChangesToggleGrouping);

    assert!(
        h.app
            .changes_rows()
            .iter()
            .any(|row| matches!(row, ChangesRow::Directory { path, .. } if path == "src"))
    );
    assert_eq!(
        h.selected_key(),
        Some(key(ChangeGroup::Staged, "f")),
        "the cursor stays on the same entry across the rebuild"
    );
}

#[test]
fn tuicrignore_hides_ignored_entries_from_the_changes_view() {
    let mut set = sample_set();
    set.push(
        ChangeGroup::Unstaged,
        file("vendor/x.js", ChangeStatus::Modified),
    );
    set.sort();
    let mut h = build(
        set,
        VcsType::Git,
        DiffSource::StagedAndUnstaged,
        InputMode::Normal,
    );
    std::fs::write(h.app.vcs_info.root_path.join(".tuicrignore"), "vendor/\n")
        .expect("write .tuicrignore");

    h.app.open_changes_view();

    assert!(!h.app.changes_rows().iter().any(|row| {
        row.entry_key()
            .is_some_and(|k| k.path == Path::new("vendor/x.js"))
    }));
    assert_eq!(h.app.changes.as_ref().expect("view").set.unstaged.len(), 2);
}

#[test]
fn header_names_the_previewed_side() {
    let mut h = open(sample_set());
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("terminal");

    terminal
        .draw(|f| crate::ui::render(f, &mut h.app))
        .expect("draw");

    let text = buffer_text(terminal.backend().buffer());
    assert!(
        text.contains("changes \u{00b7} staged"),
        "header: {}",
        text.lines().next().unwrap_or_default()
    );
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

#[test]
fn space_on_an_unstaged_entry_stages_it_and_refreshes() {
    let mut h = open(sample_set());
    h.app.changes_select(ROW_UNSTAGED_G);
    h.clear_calls();

    h.pane(Action::ChangesToggleStage);

    let calls = h.calls();
    assert_eq!(
        &calls[..2],
        ["stage_change_entry unstaged g", "get_change_set"]
    );
    assert!(calls.contains(&"get_change_entry_diff staged g".to_string()));
    assert_eq!(h.selected_key(), Some(key(ChangeGroup::Staged, "g")));
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Staged, "g")));
    assert_eq!(h.message(), "Staged g");
}

#[test]
fn space_on_a_staged_entry_unstages_it() {
    let mut h = open(sample_set());
    h.clear_calls();

    h.pane(Action::ChangesToggleStage);

    assert_eq!(h.calls()[0], "unstage_change_entry staged f");
    assert!(h.app.changes.as_ref().expect("view").set.staged.is_empty());
    assert_eq!(
        h.selected_key(),
        Some(key(ChangeGroup::Unstaged, "f")),
        "the selection follows the path into its new group"
    );
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Unstaged, "f")));
}

#[test]
fn s_and_u_in_the_pane_are_explicit_stage_and_unstage() {
    let mut h = open(sample_set());
    h.clear_calls();

    h.pane(Action::StageFile);
    assert!(h.calls().is_empty(), "s on a staged entry is a no-op");
    assert_eq!(h.message(), "f is staged already");

    h.app.changes_select(ROW_UNTRACKED_N);
    h.clear_calls();
    h.pane(Action::UnstageFile);
    assert!(h.calls().is_empty());
    assert_eq!(h.message(), "n.txt is not tracked");

    h.pane(Action::StageFile);
    assert_eq!(h.calls()[0], "stage_change_entry unversioned n.txt");
    assert_eq!(h.selected_key(), Some(key(ChangeGroup::Staged, "n.txt")));

    h.clear_calls();
    h.pane(Action::UnstageFile);
    assert_eq!(h.calls()[0], "unstage_change_entry staged n.txt");
    assert_eq!(h.selected_key(), Some(key(ChangeGroup::Untracked, "n.txt")));
}

#[test]
fn space_on_a_group_row_runs_the_groups_bulk_op() {
    let mut h = open(sample_set());

    h.app.changes_select(ROW_UNSTAGED_GROUP);
    h.clear_calls();
    h.pane(Action::ChangesToggleStage);
    assert_eq!(h.calls()[0], "change_bulk_op StageTracked None");

    // Staged: f, g; Unversioned files: n.txt.
    let unversioned = h
        .app
        .changes_rows()
        .iter()
        .position(|row| {
            matches!(
                row,
                ChangesRow::Group {
                    group: ChangeGroup::Untracked,
                    ..
                }
            )
        })
        .expect("unversioned group row");
    h.app.changes_select(unversioned);
    h.clear_calls();
    h.pane(Action::ChangesToggleStage);
    assert_eq!(h.calls()[0], "change_bulk_op StageUntracked None");

    h.app.changes_select(0);
    h.clear_calls();
    h.pane(Action::ChangesToggleStage);
    assert_eq!(h.calls()[0], "change_bulk_op UnstageAll None");
    assert!(h.app.changes.as_ref().expect("view").set.staged.is_empty());
}

#[test]
fn space_on_an_inner_submodule_group_runs_in_that_submodule() {
    let (set, inner) = dirty_submodule_set();
    let mut h = open(set);
    h.handles
        .submodules
        .lock()
        .unwrap()
        .insert(PathBuf::from("lib"), inner);
    h.pane(Action::ChangesExpand);
    let inner_group = h
        .app
        .changes_rows()
        .iter()
        .position(|row| {
            matches!(row, ChangesRow::Group { submodule: Some(sub), .. } if sub == Path::new("lib"))
        })
        .expect("nested group row");
    h.app.changes_select(inner_group);
    h.clear_calls();

    h.pane(Action::ChangesToggleStage);

    assert_eq!(h.calls()[0], "change_bulk_op StageTracked Some(lib)");
}

#[test]
fn enter_on_a_dirty_submodule_entry_warns_it_cannot_be_staged() {
    let (set, _) = dirty_submodule_set();
    let mut h = open(set);
    h.clear_calls();

    h.pane(Action::ChangesToggleStage);

    assert!(h.calls().is_empty());
    assert_eq!(
        h.message(),
        "lib has uncommitted changes inside it; commit there first (l expands it)"
    );
}

#[test]
fn a_stages_tracked_capital_a_stages_everything_capital_u_unstages_everything() {
    let mut h = open(sample_set());

    h.clear_calls();
    h.pane(Action::ChangesStageAllTracked);
    assert_eq!(h.calls()[0], "change_bulk_op StageTracked None");
    assert_eq!(h.message(), "Staged every tracked change");

    h.clear_calls();
    h.pane(Action::ChangesStageAll);
    assert_eq!(h.calls()[0], "change_bulk_op StageAll None");

    h.clear_calls();
    h.pane(Action::ChangesUnstageAll);
    assert_eq!(h.calls()[0], "change_bulk_op UnstageAll None");
    assert!(h.app.changes_active());
}

#[test]
fn after_local_change_refreshes_the_tree_instead_of_reloading_the_whole_diff() {
    let mut h = open(sample_set());
    h.app.changes_select(ROW_UNSTAGED_G);
    h.clear_calls();

    h.pane(Action::ChangesToggleStage);

    assert!(
        !h.calls().iter().any(|c| c == "get_working_tree_diff"),
        "calls: {:?}",
        h.calls()
    );
    assert!(matches!(h.app.diff_source, DiffSource::Changes(Some(_))));
    assert_eq!(h.app.diff_files.len(), 1);
}

#[test]
fn stash_from_the_pane_empties_the_tree() {
    let mut h = open(sample_set());
    h.clear_calls();

    h.pane(Action::ChangesStash);

    assert_eq!(&h.calls()[..2], ["stash_push", "get_change_set"]);
    assert!(h.app.changes_rows().is_empty());
    assert_eq!(h.app.diff_source, DiffSource::Changes(None));
    assert!(h.app.diff_files.is_empty());
}

// ---------------------------------------------------------------------------
// Submodules
// ---------------------------------------------------------------------------

#[test]
fn l_on_a_dirty_submodule_loads_its_inner_change_set_and_nests_rows() {
    let (set, inner) = dirty_submodule_set();
    let mut h = open(set);
    h.handles
        .submodules
        .lock()
        .unwrap()
        .insert(PathBuf::from("lib"), inner);
    h.clear_calls();

    h.pane(Action::ChangesExpand);

    assert_eq!(h.calls(), vec!["get_submodule_change_set lib"]);
    let rows = h.app.changes_rows().to_vec();
    assert!(rows.iter().any(|row| matches!(
        row,
        ChangesRow::Group { submodule: Some(sub), group: ChangeGroup::Unstaged, depth: 2, .. }
            if sub == Path::new("lib")
    )));
    let inner_key = ChangeDiffKey::in_submodule(ChangeGroup::Unstaged, "lib", "a.txt");
    let inner_idx = rows
        .iter()
        .position(|row| row.entry_key() == Some(&inner_key))
        .expect("nested entry row");
    assert_eq!(
        h.selected_idx(),
        ROW_STAGED_F,
        "expanding keeps the cursor on the submodule"
    );

    h.app.changes_select(inner_idx);

    assert_eq!(h.previewed_key(), Some(inner_key));
    assert!(
        h.calls()
            .contains(&"get_change_entry_diff unstaged lib/a.txt".to_string())
    );
}

#[test]
fn h_on_an_expanded_submodule_drops_the_expansion_so_refresh_does_not_refetch_it() {
    let (set, inner) = dirty_submodule_set();
    let mut h = open(set);
    h.handles
        .submodules
        .lock()
        .unwrap()
        .insert(PathBuf::from("lib"), inner);
    h.pane(Action::ChangesExpand);
    assert_eq!(h.app.changes_rows().len(), 4);

    h.pane(Action::ChangesCollapse);

    assert_eq!(h.app.changes_rows().len(), 2);
    assert!(h.app.changes.as_ref().expect("view").submodules.is_empty());
    h.clear_calls();
    h.app.refresh_changes_view(None);
    assert!(
        !h.calls()
            .iter()
            .any(|c| c.starts_with("get_submodule_change_set")),
        "calls: {:?}",
        h.calls()
    );
}

#[test]
fn refresh_re_reads_an_expanded_submodule() {
    let (set, inner) = dirty_submodule_set();
    let mut h = open(set);
    h.handles
        .submodules
        .lock()
        .unwrap()
        .insert(PathBuf::from("lib"), inner);
    h.pane(Action::ChangesExpand);
    h.clear_calls();

    h.app.refresh_changes_view(None);

    assert!(
        h.calls()
            .contains(&"get_submodule_change_set lib".to_string())
    );
    assert_eq!(h.app.changes_rows().len(), 4);
}

#[test]
fn failed_submodule_expansion_shows_a_note_row() {
    let (set, _) = dirty_submodule_set();
    let mut h = open(set);

    h.pane(Action::ChangesExpand);

    assert!(h.app.changes_rows().iter().any(|row| matches!(
        row,
        ChangesRow::Note { text, .. } if text.starts_with("could not read submodule")
    )));
}

#[test]
fn h_on_a_collapsed_entry_moves_to_its_group_row() {
    let mut h = open(sample_set());
    h.app.changes_select(ROW_UNSTAGED_G);

    h.pane(Action::ChangesCollapse);

    assert_eq!(h.selected_idx(), ROW_UNSTAGED_GROUP);
    h.pane(Action::ChangesCollapse);
    assert!(h.app.changes_rows().iter().any(|row| matches!(
        row,
        ChangesRow::Group {
            group: ChangeGroup::Unstaged,
            expanded: false,
            ..
        }
    )));
    assert_eq!(h.app.changes_rows().len(), 5);
}

#[test]
fn o_and_capital_o_fold_every_group() {
    let mut h = open(sample_set());

    h.pane(Action::CollapseAll);
    assert_eq!(h.app.changes_rows().len(), 3);
    assert!(h.app.changes_selected_entry().is_none());

    h.pane(Action::ExpandAll);
    assert_eq!(h.app.changes_rows().len(), 7);
}

// ---------------------------------------------------------------------------
// Discard
// ---------------------------------------------------------------------------

#[test]
fn discard_on_a_gitlink_entry_prompts_with_the_target_commit_then_runs() {
    let mut set = ChangeSet::default();
    set.push(
        ChangeGroup::Unstaged,
        gitlink("lib", ChangeStatus::Modified, moved_pointer()),
    );
    let mut h = open(set);

    h.pane(Action::ChangesDiscard);

    assert_eq!(h.app.input_mode, InputMode::Confirm);
    let target = h.app.pending_discard.clone().expect("pending discard");
    assert!(matches!(
        &target,
        DiscardTarget::Entry { key, .. } if *key == key_for_lib()
    ));
    assert!(
        target.prompt().contains("1111111"),
        "prompt: {}",
        target.prompt()
    );
    assert_eq!(
        target.detail(),
        vec![
            "checkout 2222222 \u{2192} 1111111".to_string(),
            "modified and untracked files inside lib are kept".to_string(),
        ]
    );

    h.clear_calls();
    handle_confirm_action(&mut h.app, Action::ConfirmYes);

    assert_eq!(
        &h.calls()[..2],
        ["discard_change_entry unstaged lib", "get_change_set"]
    );
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert!(h.app.pending_discard.is_none());
}

fn key_for_lib() -> ChangeDiffKey {
    key(ChangeGroup::Unstaged, "lib")
}

#[test]
fn discard_on_a_dirty_only_submodule_prompts_about_its_files() {
    let (set, _) = dirty_submodule_set();
    let mut h = open(set);

    h.pane(Action::ChangesDiscard);

    assert_eq!(h.app.input_mode, InputMode::Confirm);
    let target = h.app.pending_discard.clone().expect("pending discard");
    assert_eq!(
        target.prompt(),
        "Throw away the modified and untracked files inside lib? This cannot be undone."
    );
    assert_eq!(target.detail(), vec!["modified content".to_string()]);
}

#[test]
fn discard_on_a_group_row_prompts_with_the_count_and_runs_discard_change_group() {
    let mut h = open(sample_set());
    h.app.changes_select(ROW_UNSTAGED_GROUP);

    h.pane(Action::ChangesDiscard);

    assert_eq!(h.app.input_mode, InputMode::Confirm);
    let prompt = h.app.pending_discard.as_ref().expect("pending").prompt();
    assert!(
        prompt.starts_with("Discard all 2 unstaged entries?"),
        "{prompt}"
    );

    h.clear_calls();
    handle_confirm_action(&mut h.app, Action::ConfirmYes);

    assert_eq!(
        &h.calls()[..2],
        ["discard_change_group unstaged 2 None", "get_change_set"]
    );
    assert_eq!(h.message(), "Discarded 2 unstaged entries");
    assert!(
        h.app
            .changes
            .as_ref()
            .expect("view")
            .set
            .unstaged
            .is_empty()
    );
}

#[test]
fn discard_on_an_embedded_repo_only_hints() {
    let mut set = ChangeSet::default();
    set.push(
        ChangeGroup::Untracked,
        ChangeEntry {
            path: PathBuf::from("nested"),
            old_path: None,
            status: ChangeStatus::Untracked,
            kind: ChangeEntryKind::EmbeddedRepo,
        },
    );
    let mut h = open(set);
    h.clear_calls();

    h.pane(Action::ChangesDiscard);

    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert!(h.app.pending_discard.is_none());
    assert_eq!(h.message(), "nested is a git repository; delete it by hand");

    h.pane(Action::ChangesToggleStage);
    assert!(h.calls().is_empty());
    assert_eq!(
        h.message(),
        "nested is a git repository; add it with git submodule add, or remove its .git"
    );
}

#[test]
fn discard_on_a_conflict_group_points_at_merge() {
    let mut set = ChangeSet::default();
    set.push(
        ChangeGroup::Conflicted,
        file("README.md", ChangeStatus::Conflicted),
    );
    let mut h = open(set);

    h.app.changes_select(0);
    h.pane(Action::ChangesDiscard);
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert_eq!(h.message(), "Resolve conflicts with :merge");

    h.app.changes_select(1);
    h.pane(Action::ChangesDiscard);
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert_eq!(h.message(), "Resolve README.md first (:merge)");
}

// ---------------------------------------------------------------------------
// Operations issued from the diff pane
// ---------------------------------------------------------------------------

#[test]
fn s_u_and_discard_from_the_diff_pane_use_the_previewed_entry() {
    let mut h = open(sample_set());
    h.app.changes_select(ROW_UNSTAGED_G);
    h.app.focused_panel = FocusedPanel::Diff;
    h.clear_calls();

    handle_diff_action(&mut h.app, Action::StageFile);
    assert_eq!(h.calls()[0], "stage_change_entry unstaged g");
    assert!(!h.calls().iter().any(|c| c.starts_with("stage_file")));
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Staged, "g")));

    h.clear_calls();
    handle_diff_action(&mut h.app, Action::UnstageFile);
    assert_eq!(h.calls()[0], "unstage_change_entry staged g");
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Unstaged, "g")));

    h.command("discard");
    assert_eq!(h.app.input_mode, InputMode::Confirm);
    assert!(matches!(
        h.app.pending_discard.as_ref(),
        Some(DiscardTarget::Entry { key, .. }) if *key == self::key(ChangeGroup::Unstaged, "g")
    ));
}

#[test]
fn hunk_staging_and_x_on_an_untracked_preview_are_refused_before_any_prompt() {
    let mut h = open(sample_set());
    h.app.changes_select(ROW_UNTRACKED_N);
    h.app.focused_panel = FocusedPanel::Diff;
    h.clear_calls();

    handle_diff_action(&mut h.app, Action::StageHunk);
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert_eq!(
        h.message(),
        "Untracked files stage whole: press s in the Changes pane"
    );

    handle_diff_action(&mut h.app, Action::DiscardHunk);
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert!(h.app.pending_discard.is_none());

    handle_diff_action(&mut h.app, Action::UnstageHunk);
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert!(h.calls().is_empty(), "calls: {:?}", h.calls());
}

#[test]
fn hunk_ops_on_a_submodule_preview_are_refused() {
    let (set, _) = dirty_submodule_set();
    let mut h = open(set);
    h.app.focused_panel = FocusedPanel::Diff;
    h.clear_calls();

    handle_diff_action(&mut h.app, Action::StageHunk);

    assert_eq!(
        h.message(),
        "Submodules stage whole: press s in the Changes pane"
    );
    assert!(h.calls().is_empty());
}

// ---------------------------------------------------------------------------
// Commit
// ---------------------------------------------------------------------------

#[test]
fn c_opens_the_commit_composer_from_the_changes_pane() {
    let mut h = open(sample_set());

    h.pane(Action::ChangesCommit);

    assert_eq!(h.app.input_mode, InputMode::Comment);
    let composer = h.app.commit_composer.as_ref().expect("composer");
    assert!(!composer.amend);
}

#[test]
fn committing_refreshes_the_view() {
    let mut h = open(sample_set());
    h.pane(Action::ChangesCommit);
    h.app.comment_buffer = "feat: stage things".to_string();
    h.clear_calls();

    h.app.save_comment();

    assert_eq!(&h.calls()[..2], ["commit", "get_change_set"]);
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert!(h.app.changes_active());
    assert!(h.app.changes.as_ref().expect("view").set.staged.is_empty());
    assert_eq!(
        h.selected_key(),
        Some(key(ChangeGroup::Unstaged, "f")),
        "f left the index with the commit; the cursor follows its unstaged half"
    );
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

#[test]
fn reload_diff_files_refreshes_the_tree_and_drops_the_cache() {
    let mut h = open(sample_set());
    h.clear_calls();

    let result = h.app.reload_diff_files().expect("reload");

    assert_eq!(result, (1, 0));
    assert_eq!(
        h.calls(),
        vec!["get_change_set", "get_change_entry_diff staged f"]
    );

    let mut empty = open(ChangeSet::default());
    assert_eq!(empty.app.diff_source, DiffSource::Changes(None));
    assert_eq!(empty.app.reload_diff_files().expect("reload"), (0, 0));
}

#[test]
fn refresh_keeps_the_selection_when_the_entry_disappears() {
    let mut h = open(sample_set());
    h.app.changes_select(ROW_UNSTAGED_G);
    let mut set = sample_set();
    set.unstaged.retain(|entry| entry.path != Path::new("g"));
    h.set_change_set(set);

    h.app.refresh_changes_view(None);

    assert_eq!(h.selected_key(), Some(key(ChangeGroup::Untracked, "n.txt")));
    assert_eq!(
        h.previewed_key(),
        Some(key(ChangeGroup::Untracked, "n.txt"))
    );

    h.set_change_set(ChangeSet::default());
    h.app.refresh_changes_view(None);

    assert_eq!(h.app.diff_source, DiffSource::Changes(None));
    assert!(h.app.diff_files.is_empty());
    assert!(h.app.changes_rows().is_empty());
}

#[test]
fn r_re_reads_the_set_and_summarizes_it() {
    let mut h = open(sample_set());
    h.clear_calls();

    h.pane(Action::ChangesRefresh);

    assert_eq!(
        h.calls(),
        vec!["get_change_set", "get_change_entry_diff staged f"]
    );
    assert_eq!(
        h.message(),
        "Refreshed \u{00b7} 1 staged \u{00b7} 2 unstaged \u{00b7} 1 unversioned"
    );
}

// ---------------------------------------------------------------------------
// Diff watch
// ---------------------------------------------------------------------------

#[test]
fn diff_watch_tick_installs_a_newer_change_set() {
    let mut h = open(sample_set());
    let request = watch_request(&h.app);
    let mut newer = sample_set();
    newer.push(
        ChangeGroup::Untracked,
        file("extra.txt", ChangeStatus::Untracked),
    );
    newer.sort();

    let redraw = deliver(
        &mut h.app,
        request.clone(),
        DiffWatchReloadEvent::Done {
            request,
            result: Ok(None),
            commits: None,
            change_status: None,
            change_set: Some(newer),
        },
    );

    assert!(redraw);
    assert!(h.app.changes_rows().iter().any(|row| {
        row.entry_key()
            .is_some_and(|k| k.path == Path::new("extra.txt"))
    }));
    assert_eq!(h.selected_key(), Some(key(ChangeGroup::Staged, "f")));
    assert_eq!(h.app.diff_files.len(), 1, "the preview is untouched");

    // The entry moved groups: the tree lands, the diff half (read for the
    // old key) is discarded, and the preview now names the new key.
    let request = watch_request(&h.app);
    let mut moved = sample_set();
    moved.staged.clear();
    let stale_preview = {
        let mut file = plain_file("f");
        file.content_hash = 99;
        file
    };
    let redraw = deliver(
        &mut h.app,
        request.clone(),
        DiffWatchReloadEvent::Done {
            request,
            result: Ok(Some(vec![stale_preview])),
            commits: None,
            change_status: None,
            change_set: Some(moved),
        },
    );

    assert!(redraw);
    assert_eq!(h.selected_key(), Some(key(ChangeGroup::Unstaged, "f")));
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Unstaged, "f")));
    assert!(
        !h.app.diff_files.iter().any(|f| f.content_hash == 99),
        "the diff half for the old key must not land"
    );
}

#[test]
fn diff_watch_tick_with_the_same_set_changes_nothing() {
    let mut h = open(sample_set());
    let request = watch_request(&h.app);

    let redraw = deliver(
        &mut h.app,
        request.clone(),
        DiffWatchReloadEvent::Done {
            request,
            result: Ok(None),
            commits: None,
            change_status: None,
            change_set: Some(sample_set()),
        },
    );

    assert!(!redraw);
}

#[test]
fn diff_watch_tick_installs_the_previewed_entry_in_place() {
    let mut h = open(sample_set());
    let request = watch_request(&h.app);
    let fresh = {
        let mut file = plain_file("f");
        file.content_hash = 77;
        file
    };

    let redraw = deliver(
        &mut h.app,
        request.clone(),
        DiffWatchReloadEvent::Done {
            request,
            result: Ok(Some(vec![fresh])),
            commits: None,
            change_status: None,
            change_set: None,
        },
    );

    assert!(redraw);
    assert_eq!(h.app.diff_files[0].content_hash, 77);
    assert_eq!(h.previewed_key(), Some(key(ChangeGroup::Staged, "f")));
    assert_eq!(h.selected_idx(), ROW_STAGED_F);
    assert!(
        h.app.session.files[&PathBuf::from("f")].content_hash != Some(77),
        "a watched preview registers through ensure_file, never add_diff_file"
    );
}

#[test]
fn diff_watch_never_installs_review_commits_while_the_view_is_open() {
    let mut h = open(sample_set());
    let request = watch_request(&h.app);

    deliver(
        &mut h.app,
        request.clone(),
        DiffWatchReloadEvent::Done {
            request,
            result: Ok(None),
            commits: Some(vec![commit("c1"), commit("c2")]),
            change_status: Some(VcsChangeStatus {
                staged: true,
                unstaged: true,
            }),
            change_set: None,
        },
    );

    assert!(h.app.review_commits.is_empty());
    assert!(!h.app.has_inline_commit_selector());
    assert!(h.app.changes_active());
}

// ---------------------------------------------------------------------------
// Key maps and commands
// ---------------------------------------------------------------------------

#[test]
fn map_changes_pane_mode_binds_the_pane_keys_and_falls_through() {
    let map = |code: KeyCode, modifiers: KeyModifiers| {
        map_changes_pane_mode(key_event(code, modifiers), ';', false)
    };
    let none = KeyModifiers::NONE;

    assert_eq!(map(KeyCode::Char(' '), none), Action::ChangesToggleStage);
    assert_eq!(map(KeyCode::Enter, none), Action::ChangesToggleStage);
    assert_eq!(map(KeyCode::Char('s'), none), Action::StageFile);
    assert_eq!(map(KeyCode::Char('u'), none), Action::UnstageFile);
    assert_eq!(
        map(KeyCode::Char('a'), none),
        Action::ChangesStageAllTracked
    );
    assert_eq!(
        map(KeyCode::Char('A'), KeyModifiers::SHIFT),
        Action::ChangesStageAll
    );
    assert_eq!(
        map(KeyCode::Char('U'), KeyModifiers::SHIFT),
        Action::ChangesUnstageAll
    );
    assert_eq!(
        map(KeyCode::Char('X'), KeyModifiers::SHIFT),
        Action::ChangesDiscard
    );
    assert_eq!(map(KeyCode::Char('c'), none), Action::ChangesCommit);
    assert_eq!(
        map(KeyCode::Char('C'), KeyModifiers::SHIFT),
        Action::ChangesAmend
    );
    assert_eq!(map(KeyCode::Char('t'), none), Action::ChangesStash);
    assert_eq!(map(KeyCode::Char('r'), none), Action::ChangesRefresh);
    assert_eq!(map(KeyCode::Char('f'), none), Action::ChangesToggleGrouping);
    assert_eq!(map(KeyCode::Char('l'), none), Action::ChangesExpand);
    assert_eq!(map(KeyCode::Right, none), Action::ChangesExpand);
    assert_eq!(map(KeyCode::Char('h'), none), Action::ChangesCollapse);
    assert_eq!(map(KeyCode::Left, none), Action::ChangesCollapse);
    assert_eq!(map(KeyCode::Char('e'), none), Action::EditFile);
    assert_eq!(map(KeyCode::Esc, none), Action::ExitMode);
    // Fall-through to the normal map.
    assert_eq!(map(KeyCode::Char('j'), none), Action::CursorDown(1));
    assert_eq!(map(KeyCode::Char(':'), none), Action::EnterCommandMode);
    assert_eq!(map(KeyCode::Char('?'), none), Action::ToggleHelp);
    assert_eq!(map(KeyCode::Char('o'), none), Action::ExpandAll);
    assert_eq!(map(KeyCode::Tab, none), Action::ToggleFocus);
    assert_eq!(map(KeyCode::Char(';'), none), Action::PendingLeaderCommand);
    assert_eq!(
        map(KeyCode::Char('p'), KeyModifiers::CONTROL),
        Action::OpenPalette
    );
}

#[test]
fn commit_select_map_binds_capital_c_to_open_changes_view() {
    let c = key_event(KeyCode::Char('C'), KeyModifiers::SHIFT);
    assert_eq!(
        map_key_to_action(c, InputMode::CommitSelect, ';'),
        Action::OpenChangesView
    );
    assert_eq!(
        map_log_pane_mode(c, ';', false),
        Action::OpenChangesView,
        "the palette replays the same key in the log pane"
    );
}

#[test]
fn commands_changes_set_changes_and_set_nochanges_dispatch() {
    let mut h = build(
        sample_set(),
        VcsType::Git,
        DiffSource::StagedAndUnstaged,
        InputMode::Normal,
    );

    h.command("changes");
    assert!(h.app.changes_active());
    assert_eq!(h.app.input_mode, InputMode::Normal);

    h.command("set nochanges");
    assert!(!h.app.changes_active());

    h.command("set changes");
    assert!(h.app.changes_active());

    h.command("set changes!");
    assert!(
        !h.app.changes_active(),
        ":set changes! toggles a focused pane closed"
    );
}

#[test]
fn edit_from_the_pane_opens_the_previewed_entry() {
    let mut h = open(sample_set());
    std::fs::write(h.app.vcs_info.root_path.join("f"), "one\n").expect("write f");

    h.pane(Action::EditFile);

    let target = h.app.pending_editor_target.as_ref().expect("editor target");
    assert_eq!(target.path, h.app.vcs_info.root_path.join("f"));

    let mut empty = open(ChangeSet::default());
    empty.pane(Action::EditFile);
    assert!(empty.app.pending_editor_target.is_none());
    assert!(empty.message().contains("Select an entry"));
}

#[test]
fn esc_in_the_pane_focuses_the_diff_and_keeps_the_pane() {
    let mut h = open(sample_set());

    h.pane(Action::ExitMode);

    assert_eq!(h.app.focused_panel, FocusedPanel::Diff);
    assert!(h.app.show_file_list);
    assert!(h.app.changes_active());
}

// ---------------------------------------------------------------------------
// Mouse
// ---------------------------------------------------------------------------

#[test]
fn mouse_click_on_the_checkbox_cell_toggles_staging() {
    let mut h = open(sample_set());
    let inner = pane_rects(&mut h.app);
    h.clear_calls();

    click(&mut h.app, inner.x + 4, inner.y + ROW_UNSTAGED_G as u16);

    let calls = h.calls();
    assert_eq!(calls[0], "get_change_entry_diff unstaged g");
    assert!(calls.contains(&"stage_change_entry unstaged g".to_string()));
    assert_eq!(h.app.focused_panel, FocusedPanel::FileList);
}

#[test]
fn mouse_click_on_the_name_only_selects() {
    let mut h = open(sample_set());
    let inner = pane_rects(&mut h.app);
    h.clear_calls();

    click(&mut h.app, inner.x + 8, inner.y + ROW_UNSTAGED_G as u16);

    assert_eq!(h.calls(), vec!["get_change_entry_diff unstaged g"]);
    assert_eq!(h.selected_idx(), ROW_UNSTAGED_G);
}

#[test]
fn mouse_click_on_a_group_row_folds_it() {
    let mut h = open(sample_set());
    let inner = pane_rects(&mut h.app);

    click(&mut h.app, inner.x + 5, inner.y + ROW_UNSTAGED_GROUP as u16);

    assert_eq!(h.selected_idx(), ROW_UNSTAGED_GROUP);
    assert_eq!(h.app.changes_rows().len(), 5);
}

#[test]
fn mouse_click_on_the_expander_cell_expands_a_dirty_submodule() {
    let (set, inner_set) = dirty_submodule_set();
    let mut h = open(set);
    h.handles
        .submodules
        .lock()
        .unwrap()
        .insert(PathBuf::from("lib"), inner_set);
    let inner = pane_rects(&mut h.app);
    h.clear_calls();

    click(&mut h.app, inner.x + 2, inner.y + 1);

    assert!(
        h.calls()
            .contains(&"get_submodule_change_set lib".to_string())
    );
    assert_eq!(h.app.changes_rows().len(), 4);
}

#[test]
fn mouse_click_on_a_note_row_is_ignored() {
    let mut set = ChangeSet::default();
    set.push(
        ChangeGroup::Unstaged,
        gitlink(
            "lib",
            ChangeStatus::Modified,
            SubmoduleChange {
                probe_error: Some("boom".to_string()),
                registered: true,
                ..Default::default()
            },
        ),
    );
    let mut h = open(set);
    assert!(matches!(h.app.changes_rows()[2], ChangesRow::Note { .. }));
    let inner = pane_rects(&mut h.app);
    h.clear_calls();

    click(&mut h.app, inner.x + 4, inner.y + 2);

    assert!(h.calls().is_empty());
    assert_eq!(h.selected_idx(), 1);
}

#[test]
fn wheel_over_the_pane_scrolls_its_viewport() {
    let mut h = open(sample_set());
    let area = pane_rects(&mut h.app);
    h.app.changes.as_mut().expect("view").viewport_height = 3;

    handle_mouse_event(
        &mut h.app,
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: area.x + 2,
            row: area.y + 1,
            modifiers: KeyModifiers::NONE,
        },
    );

    let view = h.app.changes.as_ref().expect("view");
    assert!(view.list.offset() > 0);
    assert!(view.selected_idx() >= view.list.offset());
}
