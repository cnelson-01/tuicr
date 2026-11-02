//! The changes window drives the working tree through the VCS: these tests
//! pin which worktree each key acts in (the root or a submodule), how the
//! rows fold, and how the window shares the left column with the tree.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::log_pane_tests::build_review_app;
use crate::app::*;
use crate::input::Action;
use crate::model::FileStatus;
use crate::vcs::traits::VcsType;
use crate::vcs::{ChangeArea, StatusEntry, SubmoduleState};

/// A VCS whose status is a shared table the test edits between calls, and
/// which records every worktree operation it is asked for.
struct StatusVcs {
    info: VcsInfo,
    entries: Arc<Mutex<Vec<StatusEntry>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl StatusVcs {
    fn record(&self, call: String) -> Result<()> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

fn diff_file(path: &str) -> DiffFile {
    DiffFile {
        old_path: Some(PathBuf::from(path)),
        new_path: Some(PathBuf::from(path)),
        status: FileStatus::Modified,
        hunks: Vec::new(),
        is_binary: false,
        is_too_large: false,
        is_commit_message: false,
        content_hash: 0,
    }
}

impl VcsBackend for StatusVcs {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, _highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        Err(TuicrError::NoChanges)
    }

    /// The root's staged files, so a preview has something to jump to.
    fn get_staged_diff(&self, _highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        let files: Vec<DiffFile> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.depth == 0 && e.area == ChangeArea::Staged && !e.is_submodule())
            .map(|e| diff_file(&e.path.display().to_string()))
            .collect();
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

    fn working_tree_status(&self) -> Result<Vec<StatusEntry>> {
        Ok(self.entries.lock().unwrap().clone())
    }

    fn stage_in(&self, repo: &Path, paths: &[PathBuf]) -> Result<()> {
        self.record(format!("stage {} {:?}", repo.display(), paths))
    }

    fn unstage_in(&self, repo: &Path, paths: &[PathBuf]) -> Result<()> {
        self.record(format!("unstage {} {:?}", repo.display(), paths))
    }

    fn stage_all_in(&self, repo: &Path) -> Result<()> {
        self.record(format!("stage-all {}", repo.display()))
    }

    fn unstage_all_in(&self, repo: &Path) -> Result<()> {
        self.record(format!("unstage-all {}", repo.display()))
    }

    fn discard_in(&self, repo: &Path, path: &Path) -> Result<()> {
        self.record(format!("discard {} {}", repo.display(), path.display()))
    }

    fn commit_in(&self, repo: &Path, message: &str, amend: bool) -> Result<String> {
        self.record(format!(
            "commit {} {message:?} amend={amend}",
            repo.display()
        ))?;
        Ok("abc1234".to_string())
    }

    fn head_commit_message_in(&self, repo: &Path) -> Result<String> {
        Ok(format!("previous in {}\n", repo.display()))
    }

    fn submodule_checkout(&self, repo: &Path, path: &Path, to_head: bool) -> Result<()> {
        self.record(format!(
            "submodule-checkout {} {} to_head={to_head}",
            repo.display(),
            path.display()
        ))
    }

    fn commit(&self, message: &str, amend: bool) -> Result<String> {
        self.record(format!("commit-root {message:?} amend={amend}"))?;
        Ok("def5678".to_string())
    }

    fn head_commit_message(&self) -> Result<String> {
        Ok("previous at the root\n".to_string())
    }
}

fn entry(repo: &str, area: ChangeArea, path: &str, depth: usize) -> StatusEntry {
    StatusEntry {
        repo: PathBuf::from(repo),
        area,
        path: PathBuf::from(path),
        orig_path: None,
        code: if area == ChangeArea::Untracked {
            "?".to_string()
        } else {
            "M".to_string()
        },
        submodule: None,
        depth,
    }
}

fn submodule(repo: &str, area: ChangeArea, path: &str, depth: usize) -> StatusEntry {
    StatusEntry {
        submodule: Some(SubmoduleState {
            commit_changed: true,
            modified: true,
            untracked: false,
            ahead: 2,
            behind: 0,
        }),
        ..entry(repo, area, path, depth)
    }
}

/// Staged: a.rs. Unstaged: the submodule lib/sub (with x.c staged and
/// y.c unstaged inside it), z.rs. Untracked: new.txt.
fn sample() -> Vec<StatusEntry> {
    vec![
        entry("", ChangeArea::Staged, "a.rs", 0),
        submodule("", ChangeArea::Unstaged, "lib/sub", 0),
        entry("lib/sub", ChangeArea::Staged, "x.c", 1),
        entry("lib/sub", ChangeArea::Unstaged, "y.c", 1),
        entry("", ChangeArea::Unstaged, "z.rs", 0),
        entry("", ChangeArea::Untracked, "new.txt", 0),
    ]
}

struct Harness {
    app: App,
    entries: Arc<Mutex<Vec<StatusEntry>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Harness {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn pane(&self) -> &ChangesPane {
        self.app.changes_pane.as_ref().expect("changes pane open")
    }

    /// The label of the row under the cursor.
    fn cursor_label(&self) -> String {
        let pane = self.pane();
        match &pane.rows[pane.cursor] {
            ChangesRow::Group { area, .. } => area.label().to_string(),
            ChangesRow::Entry(idx) => pane.entries[*idx].worktree().display().to_string(),
        }
    }

    fn move_to(&mut self, label: &str) {
        let pane = self.pane();
        let row = pane
            .rows
            .iter()
            .position(|row| match row {
                ChangesRow::Group { area, .. } => area.label() == label,
                ChangesRow::Entry(idx) => pane.entries[*idx].worktree() == Path::new(label),
            })
            .unwrap_or_else(|| panic!("no row {label}"));
        self.app.changes_go_top();
        self.app.changes_move_down(row);
        assert_eq!(self.cursor_label(), label);
    }
}

fn harness(entries: Vec<StatusEntry>) -> Harness {
    let mut app = build_review_app(&["a"]);
    let entries = Arc::new(Mutex::new(entries));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let info = VcsInfo {
        root_path: PathBuf::from("/tmp/no-such-repo"),
        head_commit: "head".to_string(),
        branch_name: Some("main".to_string()),
        vcs_type: VcsType::Git,
    };
    app.vcs_info = info.clone();
    app.vcs = Box::new(StatusVcs {
        info,
        entries: entries.clone(),
        calls: calls.clone(),
    });
    app.diff_source = DiffSource::WorkingTree;
    app.show_file_list = false;
    Harness {
        app,
        entries,
        calls,
    }
}

#[test]
fn opening_lists_groups_and_lands_on_the_first_file() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    assert!(h.app.changes_pane_active());
    assert_eq!(h.app.focused_panel, FocusedPanel::Changes);
    assert!(h.app.show_file_list, "the window needs the left column");
    let pane = h.pane();
    let labels: Vec<String> = (0..pane.rows.len())
        .map(|row| match &pane.rows[row] {
            ChangesRow::Group { area, count } => format!("{}:{count}", area.label()),
            ChangesRow::Entry(idx) => pane.entries[*idx].worktree().display().to_string(),
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            "Staged:1",
            "a.rs",
            "Unstaged:2",
            "lib/sub",
            "lib/sub/x.c",
            "lib/sub/y.c",
            "z.rs",
            "Untracked:1",
            "new.txt",
        ]
    );
    assert_eq!(h.cursor_label(), "a.rs");
    assert_eq!(
        pane.counts(),
        vec![
            (ChangeArea::Staged, 1),
            (ChangeArea::Unstaged, 2),
            (ChangeArea::Untracked, 1)
        ]
    );
}

#[test]
fn closing_restores_the_file_list_and_focus() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.app.disable_changes_pane();
    assert!(!h.app.changes_pane_active());
    assert!(!h.app.show_file_list, "the tree was hidden before");
    assert_eq!(h.app.focused_panel, FocusedPanel::Diff);
}

#[test]
fn leader_t_opens_then_focuses_then_hides() {
    let mut h = harness(sample());
    h.app.toggle_changes_pane();
    assert!(h.app.changes_pane_active());
    assert_eq!(h.app.focused_panel, FocusedPanel::Changes);
    h.app.focused_panel = FocusedPanel::Diff;
    h.app.toggle_changes_pane();
    assert!(h.app.changes_pane_active());
    assert_eq!(h.app.focused_panel, FocusedPanel::Changes);
    h.app.toggle_changes_pane();
    assert!(!h.app.changes_pane_active());
    assert_eq!(h.app.focused_panel, FocusedPanel::Diff);
}

#[test]
fn space_stages_or_unstages_in_the_entry_s_own_worktree() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.move_to("lib/sub/y.c");
    h.app.changes_toggle_stage();
    h.move_to("lib/sub/x.c");
    h.app.changes_toggle_stage();
    h.move_to("a.rs");
    h.app.changes_toggle_stage();
    h.move_to("new.txt");
    h.app.changes_toggle_stage();
    assert_eq!(
        h.calls(),
        vec![
            "stage lib/sub [\"y.c\"]",
            "unstage lib/sub [\"x.c\"]",
            "unstage  [\"a.rs\"]",
            "stage  [\"new.txt\"]",
        ]
    );
}

#[test]
fn the_window_re_reads_the_status_after_an_operation_keeping_the_cursor() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.move_to("z.rs");
    // The stage "happens": z.rs moves to the staged group.
    {
        let mut entries = h.entries.lock().unwrap();
        entries.retain(|e| e.path != Path::new("z.rs"));
        entries.insert(1, entry("", ChangeArea::Staged, "z.rs", 0));
    }
    h.app.changes_stage();
    assert_eq!(h.calls(), vec!["stage  [\"z.rs\"]"]);
    // z.rs is now staged: the cursor is no longer on it (it moved group);
    // the window landed on the row where z.rs was, the next unstaged file.
    let pane = h.pane();
    assert_eq!(pane.counts()[0], (ChangeArea::Staged, 2));
    assert!(pane.cursor < pane.rows.len());
}

#[test]
fn stage_all_targets_the_cursor_s_worktree() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.move_to("lib/sub/x.c");
    h.app.changes_stage_all();
    h.move_to("Unstaged");
    h.app.changes_unstage_all();
    h.move_to("lib/sub");
    h.app.changes_stage_all();
    assert_eq!(
        h.calls(),
        vec!["stage-all lib/sub", "unstage-all ", "stage-all "]
    );
}

#[test]
fn discard_asks_first_and_knows_submodules_and_untracked_files() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.move_to("lib/sub");
    h.app.changes_request_discard();
    assert_eq!(h.app.input_mode, InputMode::Confirm);
    assert_eq!(
        h.app.pending_discard,
        Some(DiscardTarget::Submodule {
            repo: PathBuf::new(),
            path: PathBuf::from("lib/sub"),
            to_head: false,
        })
    );
    h.app.confirm_discard();
    assert_eq!(h.app.input_mode, InputMode::Normal);

    h.move_to("new.txt");
    h.app.changes_request_discard();
    let prompt = h.app.pending_discard.as_ref().unwrap().prompt();
    assert!(
        prompt.starts_with("Delete the untracked new.txt"),
        "{prompt}"
    );
    h.app.confirm_discard();

    h.move_to("lib/sub/y.c");
    h.app.changes_request_discard();
    h.app.cancel_discard();
    assert_eq!(
        h.calls(),
        vec![
            "submodule-checkout  lib/sub to_head=false",
            "discard  new.txt",
        ]
    );
    assert!(h.app.pending_discard.is_none());
}

#[test]
fn commit_from_a_nested_row_lands_inside_the_submodule() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.move_to("lib/sub/y.c");
    h.app.changes_commit(false);
    assert_eq!(h.app.input_mode, InputMode::Comment);
    let composer = h.app.commit_composer.clone().expect("composer open");
    assert_eq!(composer.repo, PathBuf::from("lib/sub"));
    h.app.comment_buffer = "inside the submodule".to_string();
    h.app.save_comment();
    assert_eq!(
        h.calls(),
        vec!["commit lib/sub \"inside the submodule\" amend=false"]
    );
    assert_eq!(h.app.input_mode, InputMode::Normal);

    // The root commits through the plain `commit`; amend pre-fills HEAD's message.
    h.move_to("a.rs");
    h.app.changes_commit(true);
    let composer = h.app.commit_composer.clone().expect("composer open");
    assert_eq!(composer.repo, PathBuf::new());
    assert!(composer.amend);
    assert_eq!(h.app.comment_buffer, "previous at the root");
    h.app.save_comment();
    assert_eq!(
        h.calls().last().unwrap(),
        "commit-root \"previous at the root\" amend=true"
    );
}

#[test]
fn commit_refuses_an_empty_index() {
    let mut h = harness(vec![entry("", ChangeArea::Unstaged, "z.rs", 0)]);
    h.app.enable_changes_pane();
    h.app.changes_commit(false);
    assert_eq!(h.app.input_mode, InputMode::Normal);
    assert!(h.app.commit_composer.is_none());
    assert!(h.calls().is_empty());
}

#[test]
fn folding_hides_rows_and_the_cursor_survives() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.move_to("lib/sub");
    h.app.changes_collapse();
    assert_eq!(h.pane().rows.len(), 7, "the two nested rows are hidden");
    assert_eq!(h.cursor_label(), "lib/sub");
    h.app.changes_expand();
    assert_eq!(h.pane().rows.len(), 9);
    h.move_to("lib/sub/y.c");
    h.app.changes_collapse();
    assert_eq!(
        h.cursor_label(),
        "lib/sub",
        "h on a nested row jumps to its submodule"
    );
    h.move_to("z.rs");
    h.app.changes_collapse();
    assert_eq!(
        h.cursor_label(),
        "Unstaged",
        "h on a root row jumps to its heading"
    );
    h.app.changes_toggle_stage();
    assert_eq!(h.pane().rows.len(), 5, "space on a heading folds it");
    h.app.changes_collapse_all();
    assert_eq!(h.pane().rows.len(), 3);
    h.app.changes_expand_all();
    assert_eq!(h.pane().rows.len(), 9);
}

#[test]
fn enter_loads_the_review_the_entry_belongs_to_and_focuses_the_diff() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.move_to("a.rs");
    h.app.changes_open();
    assert_eq!(h.app.diff_source, DiffSource::Staged);
    assert_eq!(h.app.focused_panel, FocusedPanel::Diff);
    assert_eq!(
        h.app.current_file_path().map(|p| p.display().to_string()),
        Some("a.rs".to_string())
    );
    assert!(h.app.changes_pane_active(), "the window stays open");
}

#[test]
fn tab_cycles_between_the_window_and_the_diff() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.app.show_commit_selector = false;
    crate::handler::handle_changes_pane_action(&mut h.app, Action::ToggleFocus);
    assert_eq!(h.app.focused_panel, FocusedPanel::Diff);
    crate::handler::handle_diff_action(&mut h.app, Action::ToggleFocus);
    assert_eq!(h.app.focused_panel, FocusedPanel::Changes);
    crate::handler::handle_changes_pane_action(&mut h.app, Action::ExitMode);
    assert_eq!(h.app.focused_panel, FocusedPanel::Diff);
    assert!(h.app.changes_pane_active(), "esc only moves focus");
}

#[test]
fn zen_hides_the_window_and_brings_it_back() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.app.toggle_zen();
    assert!(!h.app.changes_pane_active());
    h.app.toggle_zen();
    assert!(h.app.changes_pane_active());
}

#[test]
fn toggling_the_file_list_swaps_the_window_for_the_tree() {
    let mut h = harness(sample());
    h.app.enable_changes_pane();
    h.app.toggle_file_list();
    assert!(!h.app.changes_pane_active());
    assert!(h.app.show_file_list);
}

#[test]
fn an_unsupported_backend_shows_why_the_window_is_empty() {
    let mut h = harness(sample());
    // The stock backend of `build_review_app` has no status.
    h.app.vcs = Box::new(super::log_pane_tests::EndlessVcs {
        info: h.app.vcs_info.clone(),
    });
    h.app.enable_changes_pane();
    let pane = h.pane();
    assert!(pane.rows.is_empty());
    assert!(
        pane.error
            .as_deref()
            .unwrap_or("")
            .contains("not supported")
    );
}

#[test]
fn a_clean_tree_opens_empty_without_an_error() {
    let mut h = harness(Vec::new());
    h.app.enable_changes_pane();
    let pane = h.pane();
    assert!(pane.rows.is_empty());
    assert!(pane.error.is_none());
    assert!(pane.counts().is_empty());
}
