//! Local changes, IDE "Commit tool window" style: stage / unstage the
//! file or hunk under the cursor, discard a hunk or file behind a
//! confirmation, commit (or amend) through the comment composer, and stash.
//!
//! Hunk operations hand git the hunk's coordinates and the diff it was
//! displayed from ([`HunkScope`]); the backend re-reads that diff and applies
//! just the matching hunk, so the rendered (tab-expanded, highlighted) lines
//! never have to be turned back into a patch.

use std::path::{Path, PathBuf};

use super::{App, ConfirmAction, DiffSource, FileTreeItem, FocusedPanel, InputMode};
use crate::error::TuicrError;
use crate::vcs::{
    ChangeDiffKey, ChangeEntry, ChangeEntryKind, ChangeGroup, ChangeStatus, HunkScope, StashEntry,
};

/// What a pending "Discard?" confirmation will throw away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscardTarget {
    Hunk {
        path: PathBuf,
        scope: HunkScope,
        old_start: u32,
        new_start: u32,
    },
    File {
        path: PathBuf,
    },
    /// One row of the Changes view (a file, a submodule, or an entry inside
    /// an expanded submodule).
    Entry {
        key: ChangeDiffKey,
        entry: ChangeEntry,
    },
    /// A whole group of the Changes view (`X` on a group row).
    Group {
        group: ChangeGroup,
        submodule: Option<PathBuf>,
        entries: Vec<ChangeEntry>,
    },
}

/// The first seven characters of an object id, for prompts.
fn short7(id: &str) -> &str {
    &id[..7.min(id.len())]
}

impl DiscardTarget {
    /// The confirmation question.
    pub fn prompt(&self) -> String {
        match self {
            Self::Hunk {
                path, new_start, ..
            } => format!(
                "Discard the hunk at {}:{new_start}? This cannot be undone.",
                path.display()
            ),
            Self::File { path } => format!(
                "Discard all changes to {}? This cannot be undone.",
                path.display()
            ),
            Self::Entry { key, entry } => Self::entry_prompt(key, entry),
            Self::Group { group, entries, .. } => {
                let n = entries.len();
                match group {
                    ChangeGroup::Staged => format!(
                        "Discard all {n} staged entries? Index and working tree go back to HEAD. This cannot be undone."
                    ),
                    ChangeGroup::Unstaged => format!(
                        "Discard all {n} unstaged entries? Working tree goes back to the index. This cannot be undone."
                    ),
                    ChangeGroup::Untracked => format!(
                        "Delete all {n} untracked files? Embedded repositories are kept. This cannot be undone."
                    ),
                    ChangeGroup::Conflicted => "Resolve conflicts with :merge".to_string(),
                }
            }
        }
    }

    /// Extra lines under the question (`render_confirm_dialog_with_detail`
    /// when non-empty): the submodule pointer move and what happens to the
    /// files inside it. For a group, one line per submodule the loop would
    /// check out, clone or clean inside, and per entry it skips. Empty for
    /// hunks and files.
    pub fn detail(&self) -> Vec<String> {
        match self {
            Self::Entry { key, entry } => Self::entry_detail(key, entry),
            Self::Group {
                group,
                submodule,
                entries,
            } => Self::group_detail(*group, submodule.as_deref(), entries),
            _ => Vec::new(),
        }
    }

    fn entry_detail(key: &ChangeDiffKey, entry: &ChangeEntry) -> Vec<String> {
        let ChangeEntryKind::Submodule(sm) = &entry.kind else {
            return Vec::new();
        };
        let path = key.display_path();
        let mut lines = Vec::new();
        match entry.status {
            ChangeStatus::Added | ChangeStatus::Deleted => {}
            _ if sm.pointer_moved(key.group) => {
                let (old, new) = sm.sides(key.group);
                let (old, new) = (old.unwrap_or(""), new.unwrap_or(""));
                lines.push(match key.group {
                    ChangeGroup::Staged => {
                        format!("pointer {} → {}", short7(new), short7(old))
                    }
                    _ => format!("checkout {} → {}", short7(new), short7(old)),
                });
                lines.push(if sm.dirty() {
                    format!(
                        "modified and untracked files inside {} are thrown away",
                        path.display()
                    )
                } else {
                    format!(
                        "modified and untracked files inside {} are kept",
                        path.display()
                    )
                });
            }
            _ => {
                if sm.modified_content {
                    lines.push("modified content".to_string());
                }
                if sm.untracked_content {
                    lines.push("untracked content".to_string());
                }
            }
        }
        lines
    }

    /// What the group loop does beyond "index / working tree": a submodule
    /// row is checked out at the restored pointer (its dirt thrown away
    /// when the pointer moved), cloned when its directory is gone, and the
    /// entries `discard_change_group` skips are named as skipped.
    fn group_detail(
        group: ChangeGroup,
        submodule: Option<&Path>,
        entries: &[ChangeEntry],
    ) -> Vec<String> {
        let mut lines = Vec::new();
        for entry in entries {
            let shown = match submodule {
                Some(sub) => sub.join(&entry.path),
                None => entry.path.clone(),
            };
            let shown = shown.display();
            if entry.skipped_by_group_discard(group) {
                let why = if entry.is_embedded_repo() {
                    "an embedded repository"
                } else if entry.status == ChangeStatus::Conflicted {
                    "conflicted"
                } else if entry.status == ChangeStatus::Deleted {
                    "not in .gitmodules"
                } else {
                    "a submodule with uncommitted changes inside it"
                };
                lines.push(format!("{shown} is skipped: {why}"));
                continue;
            }
            let Some(sm) = entry.submodule() else {
                continue;
            };
            match entry.status {
                ChangeStatus::Added => {}
                ChangeStatus::Deleted => {
                    lines.push(format!("submodule {shown} is cloned or checked out again"));
                }
                _ if sm.pointer_moved(group) => {
                    let (old, new) = sm.sides(group);
                    let (old, new) = (short7(old.unwrap_or("")), short7(new.unwrap_or("")));
                    lines.push(if sm.dirty() {
                        format!(
                            "submodule {shown}: checkout {new} → {old}; modified and untracked files inside it are thrown away"
                        )
                    } else {
                        format!("submodule {shown}: checkout {new} → {old}")
                    });
                }
                _ => {}
            }
        }
        lines
    }

    /// The question for one Changes-view entry (§1.6 of the design: one
    /// sentence per entry kind and status, always naming the path).
    fn entry_prompt(key: &ChangeDiffKey, entry: &ChangeEntry) -> String {
        let path = key.display_path();
        let path = path.display();
        match &entry.kind {
            ChangeEntryKind::File => match entry.status {
                ChangeStatus::Untracked => {
                    format!("Delete the untracked file {path}? This cannot be undone.")
                }
                ChangeStatus::Added if key.group == ChangeGroup::Staged => format!(
                    "Delete the new file {path}? It exists only in the index and the working tree. This cannot be undone."
                ),
                // Unstaged `A`: an intent-to-add; `git reset -- path` keeps
                // the file.
                ChangeStatus::Added => format!(
                    "Drop the intent-to-add for {path}? The file is kept and becomes untracked."
                ),
                ChangeStatus::Renamed | ChangeStatus::Copied => {
                    let old = entry
                        .old_path
                        .as_ref()
                        .map(|old| match &key.submodule {
                            Some(sub) => sub.join(old),
                            None => old.clone(),
                        })
                        .unwrap_or_default();
                    let old = old.display();
                    // Staged: `checkout HEAD -- old`; unstaged: `checkout -- old`.
                    let source = match key.group {
                        ChangeGroup::Staged => "HEAD",
                        _ => "the index",
                    };
                    if entry.status == ChangeStatus::Renamed {
                        format!(
                            "Undo the rename {old} → {path}? {path} is deleted and {old} restored from {source}. This cannot be undone."
                        )
                    } else {
                        format!(
                            "Undo the copy {old} → {path}? {path} is deleted. This cannot be undone."
                        )
                    }
                }
                // Staged: `checkout HEAD -- path` (both halves); unstaged:
                // `checkout -- path` (the staged half is kept).
                _ if key.group == ChangeGroup::Staged => format!(
                    "Discard all changes to {path}? Index and working tree go back to HEAD. This cannot be undone."
                ),
                _ => format!(
                    "Discard the unstaged changes to {path}? The working tree goes back to the index; staged changes are kept. This cannot be undone."
                ),
            },
            ChangeEntryKind::Submodule(sm) => match entry.status {
                ChangeStatus::Added => {
                    format!("Unstage the new submodule {path}? Its directory is kept.")
                }
                ChangeStatus::Deleted => {
                    let target = match key.group {
                        ChangeGroup::Staged => sm.head_id.as_deref(),
                        _ => sm.index_id.as_deref(),
                    }
                    .unwrap_or("");
                    format!(
                        "Restore submodule {path} at {}? It is cloned or checked out again. This cannot be undone.",
                        short7(target)
                    )
                }
                _ if sm.pointer_moved(key.group) => {
                    let (old, _) = sm.sides(key.group);
                    let old = short7(old.unwrap_or(""));
                    match key.group {
                        ChangeGroup::Staged => format!(
                            "Reset submodule {path} to {old} and check it out there? This cannot be undone."
                        ),
                        _ => format!(
                            "Check submodule {path} out at {old} (the commit recorded in the index)? This cannot be undone."
                        ),
                    }
                }
                _ if sm.dirty() => format!(
                    "Throw away the modified and untracked files inside {path}? This cannot be undone."
                ),
                _ => format!(
                    "Discard all changes to {path}? Index and working tree go back to HEAD. This cannot be undone."
                ),
            },
            ChangeEntryKind::EmbeddedRepo => {
                format!("{path} is a git repository; delete it by hand")
            }
        }
    }
}

/// The comment composer is doubling as a commit message editor: for a new
/// commit, an amend, or a reword of `reword`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitComposer {
    pub amend: bool,
    pub reword: Option<String>,
    /// Mode to hand back to when the composer closes (the fullscreen
    /// selector when a reword started there).
    pub return_mode: InputMode,
}

/// The stash picker modal (`:stashes`).
#[derive(Debug)]
pub struct StashPicker {
    pub entries: Vec<StashEntry>,
    pub cursor: usize,
    pub return_mode: InputMode,
}

impl App {
    /// Which git diff the review on screen was built from, for hunk
    /// operations. `None` for commit ranges and PRs.
    pub fn hunk_scope(&self) -> Option<HunkScope> {
        match &self.diff_source {
            DiffSource::WorkingTree | DiffSource::StagedAndUnstaged => Some(HunkScope::WorkingTree),
            DiffSource::Unstaged => Some(HunkScope::Unstaged),
            DiffSource::Staged => Some(HunkScope::Staged),
            DiffSource::Changes(Some(key)) => Some(key.group.hunk_scope()),
            DiffSource::Changes(None) => Some(HunkScope::WorkingTree),
            _ => None,
        }
    }

    /// The file the user means: the tree's selection when the tree has
    /// focus, otherwise the file under the diff cursor.
    fn focused_file_idx(&self) -> Option<usize> {
        if self.focused_panel == FocusedPanel::FileList {
            match self.get_selected_tree_item() {
                Some(FileTreeItem::File { file_idx, .. }) => Some(file_idx),
                _ => None,
            }
        } else {
            self.current_file_path()
                .map(|_| self.diff_state.current_file_idx)
        }
    }

    fn focused_file_path(&self) -> Option<PathBuf> {
        self.focused_file_idx()
            .and_then(|idx| self.diff_files.get(idx))
            .map(|file| file.display_path().clone())
    }

    fn require_local_scope(&mut self, verb: &str) -> Option<HunkScope> {
        let scope = self.hunk_scope();
        if scope.is_none() {
            self.set_error(format!(
                "{verb} needs a working-tree review (staged / unstaged changes)"
            ));
        }
        scope
    }

    /// With the Changes view open, the diff previews one entry of the tree;
    /// hunk and line operations on an untracked, submodule, embedded-repo
    /// or conflicted preview have no git counterpart, so they are refused
    /// before any prompt. `None` means the operation may go ahead.
    fn changes_hunk_op_refusal(&self) -> Option<&'static str> {
        if !self.changes_active() {
            return None;
        }
        let (key, entry) = self.changes_previewed_entry()?;
        // An inner entry's preview carries the superproject-prefixed path;
        // the hunk commands run in the superproject, where the submodule
        // is one gitlink and `git diff -- sub/file` prints nothing.
        if key.submodule.is_some() {
            return Some(
                "Hunks inside a submodule: stage or discard the whole file with s / X in the Changes pane",
            );
        }
        match &entry.kind {
            ChangeEntryKind::Submodule(_) | ChangeEntryKind::EmbeddedRepo => {
                Some("Submodules stage whole: press s in the Changes pane")
            }
            ChangeEntryKind::File => {
                if key.group == ChangeGroup::Conflicted || entry.status == ChangeStatus::Conflicted
                {
                    Some("Resolve conflicts with :merge")
                } else if key.group == ChangeGroup::Untracked
                    || entry.status == ChangeStatus::Untracked
                {
                    Some("Untracked files stage whole: press s in the Changes pane")
                } else {
                    None
                }
            }
        }
    }

    /// `s`: stage the focused file.
    pub fn stage_file_at_cursor(&mut self) {
        if self.changes_active() {
            return self.changes_stage_previewed();
        }
        let Some(_) = self.require_local_scope("Staging") else {
            return;
        };
        let Some(path) = self.focused_file_path() else {
            self.set_warning("Move the cursor onto a file to stage");
            return;
        };
        match self.vcs.stage_file(&path) {
            Ok(()) => self.after_local_change(format!("Staged {}", path.display())),
            Err(e) => self.set_error(format!("Failed to stage {}: {e}", path.display())),
        }
    }

    /// `u`: unstage the focused file.
    pub fn unstage_file_at_cursor(&mut self) {
        if self.changes_active() {
            return self.changes_unstage_previewed();
        }
        let Some(_) = self.require_local_scope("Unstaging") else {
            return;
        };
        let Some(path) = self.focused_file_path() else {
            self.set_warning("Move the cursor onto a file to unstage");
            return;
        };
        match self.vcs.unstage_file(&path) {
            Ok(()) => self.after_local_change(format!("Unstaged {}", path.display())),
            Err(e) => self.set_error(format!("Failed to unstage {}: {e}", path.display())),
        }
    }

    /// The hunk under the diff cursor as (path, old_start, new_start).
    fn hunk_coordinates_at_cursor(&self) -> Option<(PathBuf, u32, u32)> {
        let (file_idx, hunk_idx) = self.hunk_at_cursor()?;
        let file = self.diff_files.get(file_idx)?;
        let hunk = file.hunks.get(hunk_idx)?;
        Some((file.display_path().clone(), hunk.old_start, hunk.new_start))
    }

    /// `S`: stage the hunk under the cursor.
    pub fn stage_hunk_at_cursor(&mut self) {
        if let Some(refusal) = self.changes_hunk_op_refusal() {
            self.set_warning(refusal);
            return;
        }
        let Some(scope) = self.require_local_scope("Staging a hunk") else {
            return;
        };
        if scope == HunkScope::Staged {
            self.set_message("This view shows staged hunks already; U unstages one");
            return;
        }
        let Some((path, old_start, new_start)) = self.hunk_coordinates_at_cursor() else {
            self.set_warning("Move the cursor into a hunk to stage it");
            return;
        };
        match self.vcs.stage_hunk(&path, scope, old_start, new_start) {
            Ok(()) => {
                self.after_local_change(format!("Staged hunk at {}:{new_start}", path.display()))
            }
            Err(e) => self.set_error(format!("Failed to stage hunk: {e}")),
        }
    }

    /// `U`: unstage the hunk under the cursor (staged view only, since a
    /// hunk's line numbers only mean something in the diff it came from).
    pub fn unstage_hunk_at_cursor(&mut self) {
        if let Some(refusal) = self.changes_hunk_op_refusal() {
            self.set_warning(refusal);
            return;
        }
        let Some(scope) = self.require_local_scope("Unstaging a hunk") else {
            return;
        };
        if scope != HunkScope::Staged {
            self.set_message(if self.changes_active() {
                "Unstage hunks from the Staged entry of this path in the Changes pane; u unstages the whole file"
            } else {
                "Unstage hunks from the staged view (`:commits`, then Staged changes); u unstages the whole file"
            });
            return;
        }
        let Some((path, old_start, new_start)) = self.hunk_coordinates_at_cursor() else {
            self.set_warning("Move the cursor into a hunk to unstage it");
            return;
        };
        match self.vcs.unstage_hunk(&path, old_start, new_start) {
            Ok(()) => {
                self.after_local_change(format!("Unstaged hunk at {}:{new_start}", path.display()))
            }
            Err(e) => self.set_error(format!("Failed to unstage hunk: {e}")),
        }
    }

    /// The changed lines under the visual selection, grouped by hunk:
    /// `(file_idx, hunk_idx) → lines`. Side-by-side rows contribute both
    /// their deletion and their addition.
    fn visual_line_picks(
        &self,
    ) -> std::collections::BTreeMap<(usize, usize), crate::vcs::traits::LinePick> {
        use crate::model::LineOrigin;
        let mut picks: std::collections::BTreeMap<(usize, usize), crate::vcs::traits::LinePick> =
            Default::default();
        let Some(sel) = self.get_visual_selection() else {
            return picks;
        };
        let (start, end) = sel.ordered();
        for idx in start.annotation_idx..=end.annotation_idx {
            let (file_idx, hunk_idx, line_idxs) = match self.line_annotations.get(idx) {
                Some(super::AnnotatedLine::DiffLine {
                    file_idx,
                    hunk_idx,
                    line_idx,
                    ..
                }) => (*file_idx, *hunk_idx, vec![*line_idx]),
                Some(super::AnnotatedLine::SideBySideLine {
                    file_idx,
                    hunk_idx,
                    del_line_idx,
                    add_line_idx,
                    ..
                }) => (
                    *file_idx,
                    *hunk_idx,
                    del_line_idx.iter().chain(add_line_idx).copied().collect(),
                ),
                _ => continue,
            };
            let Some(hunk) = self
                .diff_files
                .get(file_idx)
                .and_then(|f| f.hunks.get(hunk_idx))
            else {
                continue;
            };
            for line_idx in line_idxs {
                let Some(line) = hunk.lines.get(line_idx) else {
                    continue;
                };
                let pick = picks.entry((file_idx, hunk_idx)).or_default();
                match (line.origin, line.old_lineno, line.new_lineno) {
                    (LineOrigin::Deletion, Some(old), _) => {
                        pick.old.insert(old);
                    }
                    (LineOrigin::Addition, _, Some(new)) => {
                        pick.new.insert(new);
                    }
                    _ => {}
                }
            }
        }
        picks.retain(|_, pick| !pick.is_empty());
        picks
    }

    /// `S` / `U` in visual mode: stage (or, in the staged view, unstage)
    /// just the selected lines. Hunks are applied bottom-up so the ones
    /// above keep the coordinates they are looked up by.
    pub fn stage_selected_lines(&mut self, unstage: bool) {
        if let Some(refusal) = self.changes_hunk_op_refusal() {
            self.exit_visual_mode();
            self.set_warning(refusal);
            return;
        }
        let verb = if unstage { "Unstaging" } else { "Staging" };
        let Some(scope) = self.require_local_scope(&format!("{verb} lines")) else {
            return;
        };
        if unstage != (scope == HunkScope::Staged) {
            self.set_message(if !unstage {
                "These lines are staged already; U unstages them"
            } else if self.changes_active() {
                "Unstage lines from the Staged entry of this path in the Changes pane"
            } else {
                "Unstage lines from the staged view (`:commits`, then Staged changes)"
            });
            return;
        }
        let picks = self.visual_line_picks();
        if picks.is_empty() {
            self.set_warning("No changed lines in the selection");
            return;
        }
        let mut done = 0usize;
        for ((file_idx, hunk_idx), pick) in picks.iter().rev() {
            let Some(file) = self.diff_files.get(*file_idx) else {
                continue;
            };
            let Some(hunk) = file.hunks.get(*hunk_idx) else {
                continue;
            };
            let path = file.display_path().clone();
            let result = if unstage {
                self.vcs
                    .unstage_lines(&path, hunk.old_start, hunk.new_start, pick)
            } else {
                self.vcs
                    .stage_lines(&path, scope, hunk.old_start, hunk.new_start, pick)
            };
            match result {
                Ok(()) => done += pick.old.len() + pick.new.len(),
                Err(e) => {
                    self.exit_visual_mode();
                    self.set_error(format!("{verb} lines failed: {e}"));
                    if done > 0 {
                        self.after_local_change(format!("{verb} stopped after {done} lines"));
                    }
                    return;
                }
            }
        }
        self.exit_visual_mode();
        let past = if unstage { "Unstaged" } else { "Staged" };
        self.after_local_change(format!(
            "{past} {done} line{}",
            if done == 1 { "" } else { "s" }
        ));
    }

    /// `X`: ask before discarding the hunk under the cursor.
    pub fn request_discard_hunk(&mut self) {
        if let Some(refusal) = self.changes_hunk_op_refusal() {
            self.set_warning(refusal);
            return;
        }
        let Some(scope) = self.require_local_scope("Discarding a hunk") else {
            return;
        };
        let Some((path, old_start, new_start)) = self.hunk_coordinates_at_cursor() else {
            self.set_warning("Move the cursor into a hunk to discard it");
            return;
        };
        self.pending_discard = Some(DiscardTarget::Hunk {
            path,
            scope,
            old_start,
            new_start,
        });
        self.enter_confirm_mode(ConfirmAction::DiscardChanges);
    }

    /// `:discard`: ask before throwing away every change to the focused file.
    pub fn request_discard_file(&mut self) {
        if self.changes_active() {
            return self.changes_request_discard_previewed();
        }
        let Some(_) = self.require_local_scope("Discarding a file") else {
            return;
        };
        let Some(path) = self.focused_file_path() else {
            self.set_warning("Move the cursor onto a file to discard");
            return;
        };
        self.pending_discard = Some(DiscardTarget::File { path });
        self.enter_confirm_mode(ConfirmAction::DiscardChanges);
    }

    /// `y` on the discard prompt.
    pub fn confirm_discard(&mut self) {
        self.exit_confirm_mode();
        let Some(target) = self.pending_discard.take() else {
            return;
        };
        let result = match &target {
            DiscardTarget::Hunk {
                path,
                scope,
                old_start,
                new_start,
            } => self
                .vcs
                .discard_hunk(path, *scope, *old_start, *new_start)
                .map(|()| format!("Discarded hunk at {}:{new_start}", path.display())),
            DiscardTarget::File { path } => self
                .vcs
                .discard_file(path)
                .map(|()| format!("Discarded changes to {}", path.display())),
            DiscardTarget::Entry { key, entry } => self
                .vcs
                .discard_change_entry(key, entry)
                .map(|()| format!("Discarded {}", key.display_path().display())),
            DiscardTarget::Group {
                group,
                submodule,
                entries,
            } => self
                .vcs
                .discard_change_group(*group, entries, submodule.as_deref())
                .map(|skipped| match skipped {
                    0 => format!(
                        "Discarded {} {} entries",
                        entries.len(),
                        group.short_label()
                    ),
                    n => format!(
                        "Discarded {} {} entries, {n} skipped",
                        entries.len() - n,
                        group.short_label()
                    ),
                }),
        };
        match result {
            Ok(message) => self.after_local_change(message),
            Err(e) => {
                // A group loop stops at the first failure with the entries
                // before it already discarded; the tree must say so.
                if self.changes_active() {
                    self.refresh_changes_view(None);
                }
                self.set_error(format!("Discard failed: {e}"));
            }
        }
    }

    pub fn cancel_discard(&mut self) {
        self.pending_discard = None;
        self.exit_confirm_mode();
    }

    /// Reload the review after the index or working tree changed. The diff
    /// may have emptied out (everything staged away from the unstaged view,
    /// or discarded), which `reload_diff_files` reports as `NoChanges`.
    /// With the Changes view open, `reload_diff_files` refreshes the tree
    /// instead, log rows included.
    pub(in crate::app) fn after_local_change(&mut self, message: String) {
        let changes_view = self.changes_active();
        match self.reload_diff_files() {
            Ok(_) => {}
            Err(TuicrError::NoChanges) => {
                self.diff_files.clear();
                self.diff_state = Default::default();
                self.file_list_state = Default::default();
                self.clear_expanded_gaps();
                self.rebuild_annotations();
            }
            Err(e) => {
                self.set_error(format!("{message}, but reloading the diff failed: {e}"));
                return;
            }
        }
        if !changes_view {
            self.refresh_log_rows_keep_cursor();
        }
        self.set_message(message);
    }

    // Commit

    /// `:commit` / `:commit!`: open the comment composer as the commit
    /// message editor. Amending pre-fills the current HEAD message.
    pub fn start_commit_composer(&mut self, amend: bool) {
        if self.hunk_scope().is_none() {
            self.set_error("Commit needs a working-tree review (staged / unstaged changes)");
            return;
        }
        self.enter_review_comment_mode();
        self.commit_composer = Some(CommitComposer {
            amend,
            reword: None,
            return_mode: InputMode::Normal,
        });
        if amend && let Ok(message) = self.vcs.head_commit_message() {
            self.comment_buffer = message.trim_end().to_string();
            self.comment_cursor = self.comment_buffer.len();
        }
        let what = if amend { "Amend" } else { "Commit" };
        self.set_message(format!(
            "{what}: type the message, then Enter / Ctrl-s to commit, Esc to cancel"
        ));
    }

    /// The composer's save, when it is editing a commit message.
    pub(in crate::app) fn commit_from_composer(&mut self, composer: CommitComposer) {
        let message = self.comment_buffer.trim().to_string();
        self.exit_comment_mode();
        if composer.return_mode == InputMode::CommitSelect {
            self.input_mode = InputMode::CommitSelect;
        }
        match composer.reword {
            Some(id) => self.reword_commit(&id, &message),
            None => self.commit_with_message(&message, composer.amend),
        }
    }

    pub fn commit_with_message(&mut self, message: &str, amend: bool) {
        if message.trim().is_empty() {
            self.set_error("Commit message cannot be empty");
            return;
        }
        match self.vcs.commit(message, amend) {
            Ok(id) => {
                let verb = if amend { "Amended" } else { "Committed" };
                let summary = message.lines().next().unwrap_or_default();
                self.after_local_change(format!("{verb} {id}: {summary}"));
            }
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("Commit failed: {}", err.trim()));
            }
        }
    }

    // Stash

    /// `:stash [message]`: stash the working tree (untracked included).
    pub fn stash_push_current(&mut self, message: Option<&str>) {
        // `git stash push` on a clean tree says "No local changes to save"
        // and exits 0; with the Changes view open the tree is known.
        if self.changes_view().is_some_and(|view| view.set.is_empty()) {
            self.set_message("Nothing to stash");
            return;
        }
        let message = message
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| "tuicr stash".to_string());
        match self.vcs.stash_push(&message) {
            Ok(()) => self.after_local_change(format!("Stashed: {message}")),
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("Stash failed: {}", err.trim()));
            }
        }
    }

    /// `:stash pop`: restore the newest stash.
    pub fn stash_pop_newest(&mut self) {
        match self.vcs.stash_pop() {
            Ok(()) => self.after_local_change("Stash popped".to_string()),
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("Stash pop failed: {}", err.trim()));
            }
        }
    }

    pub fn open_stash_picker(&mut self) {
        let return_mode = match self.input_mode {
            InputMode::Command => self.command_return_mode,
            InputMode::StashPicker => return,
            _ => InputMode::Normal,
        };
        match self.vcs.stash_list() {
            Ok(entries) if entries.is_empty() => self.set_message("No stashes"),
            Ok(entries) => {
                self.stash_picker = Some(StashPicker {
                    entries,
                    cursor: 0,
                    return_mode,
                });
                self.command_buffer.clear();
                self.input_mode = InputMode::StashPicker;
            }
            Err(e) => self.set_error(format!("Failed to list stashes: {e}")),
        }
    }

    pub fn close_stash_picker(&mut self) {
        if let Some(picker) = self.stash_picker.take() {
            self.input_mode = picker.return_mode;
        } else {
            self.input_mode = InputMode::Normal;
        }
    }

    pub fn stash_picker_down(&mut self) {
        if let Some(picker) = self.stash_picker.as_mut()
            && !picker.entries.is_empty()
        {
            picker.cursor = (picker.cursor + 1) % picker.entries.len();
        }
    }

    pub fn stash_picker_up(&mut self) {
        if let Some(picker) = self.stash_picker.as_mut()
            && !picker.entries.is_empty()
        {
            let n = picker.entries.len();
            picker.cursor = (picker.cursor + n - 1) % n;
        }
    }

    fn stash_picker_selected(&self) -> Option<StashEntry> {
        let picker = self.stash_picker.as_ref()?;
        picker.entries.get(picker.cursor).cloned()
    }

    /// Enter (pop) or `a` (apply, keeping the stash).
    /// `v` in the stash picker: load the stash's changes as the review
    /// (an IDE's "Show Diff"), leaving the stash in place. Untracked files
    /// stashed with it are not part of this diff.
    pub fn stash_picker_view(&mut self) {
        let Some(entry) = self.stash_picker_selected() else {
            return;
        };
        self.close_stash_picker();
        let stash = format!("stash@{{{}}}", entry.index);
        match self.open_revision_range(&format!("{stash}^..{stash}")) {
            Ok(_) => self.set_message(format!(
                "Viewing {stash}: {} \u{00b7} :stashes to pop or apply it",
                entry.message
            )),
            Err(e) => self.set_error(format!("Can't show {stash}: {}", e.to_string().trim())),
        }
    }

    pub fn stash_picker_apply(&mut self, pop: bool) {
        let Some(entry) = self.stash_picker_selected() else {
            return;
        };
        self.close_stash_picker();
        match self.vcs.stash_apply(entry.index, pop) {
            Ok(()) => {
                let verb = if pop { "Popped" } else { "Applied" };
                self.after_local_change(format!(
                    "{verb} stash@{{{}}}: {}",
                    entry.index, entry.message
                ));
            }
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("Stash apply failed: {}", err.trim()));
            }
        }
    }

    /// `d`: ask before dropping the highlighted stash.
    pub fn request_drop_stash(&mut self) {
        if self.stash_picker_selected().is_none() {
            return;
        }
        self.enter_confirm_mode(ConfirmAction::DropStash);
    }

    /// `y` on the drop prompt: drop it and refresh the picker.
    pub fn confirm_drop_stash(&mut self) {
        let Some(entry) = self.stash_picker_selected() else {
            self.exit_confirm_mode();
            return;
        };
        match self.vcs.stash_drop(entry.index) {
            Ok(()) => {
                self.set_message(format!("Dropped stash@{{{}}}", entry.index));
                match self.vcs.stash_list() {
                    Ok(entries) if !entries.is_empty() => {
                        if let Some(picker) = self.stash_picker.as_mut() {
                            picker.cursor = picker.cursor.min(entries.len() - 1);
                            picker.entries = entries;
                        }
                        self.input_mode = InputMode::StashPicker;
                        self.pending_confirm = None;
                    }
                    _ => {
                        self.pending_confirm = None;
                        self.close_stash_picker();
                    }
                }
            }
            Err(e) => {
                self.pending_confirm = None;
                self.input_mode = InputMode::StashPicker;
                self.set_error(format!("Stash drop failed: {e}"));
            }
        }
    }

    pub fn cancel_drop_stash(&mut self) {
        self.pending_confirm = None;
        self.input_mode = InputMode::StashPicker;
    }
}
