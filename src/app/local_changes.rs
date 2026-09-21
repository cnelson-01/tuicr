//! Local changes, JetBrains "Commit tool window" style: stage / unstage the
//! file or hunk under the cursor, discard a hunk or file behind a
//! confirmation, commit (or amend) through the comment composer, and stash.
//!
//! Hunk operations hand git the hunk's coordinates and the diff it was
//! displayed from ([`HunkScope`]); the backend re-reads that diff and applies
//! just the matching hunk, so the rendered (tab-expanded, highlighted) lines
//! never have to be turned back into a patch.

use std::path::PathBuf;

use super::{App, ConfirmAction, DiffSource, FileTreeItem, FocusedPanel, InputMode};
use crate::error::TuicrError;
use crate::vcs::{HunkScope, StashEntry};

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
        match self.diff_source {
            DiffSource::WorkingTree | DiffSource::StagedAndUnstaged => Some(HunkScope::WorkingTree),
            DiffSource::Unstaged => Some(HunkScope::Unstaged),
            DiffSource::Staged => Some(HunkScope::Staged),
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

    /// `s`: stage the focused file.
    pub fn stage_file_at_cursor(&mut self) {
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
        let Some(scope) = self.require_local_scope("Unstaging a hunk") else {
            return;
        };
        if scope != HunkScope::Staged {
            self.set_message(
                "Unstage hunks from the staged view (`:commits`, then Staged changes); u unstages the whole file",
            );
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

    /// `X`: ask before discarding the hunk under the cursor.
    pub fn request_discard_hunk(&mut self) {
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
        };
        match result {
            Ok(message) => self.after_local_change(message),
            Err(e) => self.set_error(format!("Discard failed: {e}")),
        }
    }

    pub fn cancel_discard(&mut self) {
        self.pending_discard = None;
        self.exit_confirm_mode();
    }

    /// Reload the review after the index or working tree changed. The diff
    /// may have emptied out (everything staged away from the unstaged view,
    /// or discarded), which `reload_diff_files` reports as `NoChanges`.
    fn after_local_change(&mut self, message: String) {
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
        self.refresh_log_rows_keep_cursor();
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
