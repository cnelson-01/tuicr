//! Branch operations: the branch picker, checkout with a dirty-tree guard
//! (or stash-and-switch), branch creation at a commit, and aborting a
//! conflicted operation. After anything moves HEAD the VCS is reopened so
//! the header, the log, and the review's branch context see the new state.

use super::{App, InputMode};
use crate::vcs::detect_vcs;
use crate::vcs::traits::{BranchInfo, RefKind};

/// The branch picker modal: every local and remote-tracking branch, filtered
/// by what the user types, with the cursor on a row of the filtered view.
#[derive(Debug)]
pub struct BranchPicker {
    pub branches: Vec<BranchInfo>,
    pub filter: String,
    /// Row within [`BranchPicker::visible`], not into `branches`.
    pub cursor: usize,
    /// Mode restored when the picker closes.
    pub return_mode: InputMode,
    /// Indices into `branches` picked with Tab, in pick order, at most two:
    /// the pair a compare runs on, or the set the log shows.
    pub picked: Vec<usize>,
}

impl BranchPicker {
    /// Indices into `branches` that match the filter, case-insensitively,
    /// keeping the list's order (local first, then remote, each by name).
    pub fn visible(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        self.branches
            .iter()
            .enumerate()
            .filter(|(_, b)| needle.is_empty() || b.name.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn selected(&self) -> Option<&BranchInfo> {
        let visible = self.visible();
        visible.get(self.cursor).map(|&i| &self.branches[i])
    }

    fn clamp_cursor(&mut self) {
        let n = self.visible().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    /// Index into `branches` of the row under the cursor.
    fn cursor_index(&self) -> Option<usize> {
        self.visible().get(self.cursor).copied()
    }

    /// Tab: pick or unpick the row under the cursor. A third pick replaces
    /// the oldest so the set stays a pair.
    pub fn toggle_pick(&mut self, idx: usize) {
        if let Some(pos) = self.picked.iter().position(|&i| i == idx) {
            self.picked.remove(pos);
        } else {
            self.picked.push(idx);
            if self.picked.len() > 2 {
                self.picked.remove(0);
            }
        }
    }

    /// 1-based pick order of a branch, for the row marker.
    pub fn pick_order(&self, idx: usize) -> Option<usize> {
        self.picked.iter().position(|&i| i == idx).map(|p| p + 1)
    }

    /// The names a compare or log acts on: the picks, else the cursor row.
    pub fn picked_names(&self) -> Vec<String> {
        let indices: Vec<usize> = if self.picked.is_empty() {
            self.cursor_index().into_iter().collect()
        } else {
            self.picked.clone()
        };
        indices
            .into_iter()
            .filter_map(|i| self.branches.get(i))
            .map(|b| b.name.clone())
            .collect()
    }
}

impl App {
    /// Open the branch picker over the current screen. From the command
    /// line the picker returns to whatever mode `:` was typed in.
    pub fn open_branch_picker(&mut self) {
        let return_mode = match self.input_mode {
            InputMode::Command => self.command_return_mode,
            InputMode::CommitSelect => InputMode::CommitSelect,
            InputMode::BranchPicker => return,
            _ => InputMode::Normal,
        };
        match self.vcs.list_branches() {
            Ok(branches) => {
                let cursor = branches.iter().position(|b| b.is_current).unwrap_or(0);
                self.branch_picker = Some(BranchPicker {
                    branches,
                    filter: String::new(),
                    cursor,
                    return_mode,
                    picked: Vec::new(),
                });
                self.command_buffer.clear();
                self.input_mode = InputMode::BranchPicker;
            }
            Err(e) => self.set_error(format!("Failed to list branches: {e}")),
        }
    }

    pub fn close_branch_picker(&mut self) {
        if let Some(picker) = self.branch_picker.take() {
            self.input_mode = picker.return_mode;
        } else {
            self.input_mode = InputMode::Normal;
        }
    }

    pub fn branch_picker_down(&mut self) {
        if let Some(picker) = self.branch_picker.as_mut() {
            let n = picker.visible().len();
            if n > 0 {
                picker.cursor = (picker.cursor + 1) % n;
            }
        }
    }

    pub fn branch_picker_up(&mut self) {
        if let Some(picker) = self.branch_picker.as_mut() {
            let n = picker.visible().len();
            if n > 0 {
                picker.cursor = (picker.cursor + n - 1) % n;
            }
        }
    }

    pub fn branch_picker_push(&mut self, c: char) {
        if let Some(picker) = self.branch_picker.as_mut() {
            picker.filter.push(c);
            picker.cursor = 0;
        }
    }

    pub fn branch_picker_pop(&mut self) {
        if let Some(picker) = self.branch_picker.as_mut() {
            picker.filter.pop();
            picker.clamp_cursor();
        }
    }

    pub fn branch_picker_clear(&mut self) {
        if let Some(picker) = self.branch_picker.as_mut() {
            picker.filter.clear();
            picker.clamp_cursor();
        }
    }

    /// Tab: pick or unpick the highlighted branch for a compare / log.
    pub fn branch_picker_toggle_pick(&mut self) {
        if let Some(picker) = self.branch_picker.as_mut()
            && let Some(idx) = picker.cursor_index()
        {
            picker.toggle_pick(idx);
        }
    }

    /// Ctrl-d: compare the picks as `first...second` (what the second has
    /// that the first does not). One pick, or none, compares the current
    /// branch against it (or the cursor row).
    pub fn branch_picker_compare(&mut self) {
        let Some(picker) = self.branch_picker.as_ref() else {
            return;
        };
        let mut names = picker.picked_names();
        if names.len() < 2 {
            let current = self
                .vcs_info
                .branch_name
                .clone()
                .unwrap_or_else(|| "HEAD".to_string());
            names.insert(0, current);
        }
        let (Some(base), Some(other)) = (names.first().cloned(), names.get(1).cloned()) else {
            return;
        };
        if base == other {
            self.set_message(format!(
                "{base} is the current branch; pick another to compare"
            ));
            return;
        }
        self.close_branch_picker();
        self.compare_pair(&base, &other);
    }

    /// Ctrl-l: show the picked branches (or the cursor row) in the log pane.
    pub fn branch_picker_log(&mut self) {
        let Some(picker) = self.branch_picker.as_ref() else {
            return;
        };
        let names = picker.picked_names();
        if names.is_empty() {
            return;
        }
        self.close_branch_picker();
        let label = names.join(", ");
        self.commit_filter = Some(crate::vcs::CommitQuery {
            refs: names,
            ..Default::default()
        });
        if self.log_pane.is_none() {
            self.enable_log_pane();
        } else {
            self.refresh_log_rows_keep_cursor();
            self.focused_panel = super::FocusedPanel::CommitSelector;
        }
        let count = self.commit_filter_match_label();
        self.set_message(format!(
            "Log of {label} \u{00b7} {count} \u{00b7} esc clears"
        ));
    }

    /// Enter in the picker: check out the highlighted branch. A remote
    /// branch checks out its local counterpart, creating it as a tracking
    /// branch when it does not exist yet (git's own DWIM). `smart` stashes a
    /// dirty tree around the switch instead of refusing.
    pub fn branch_picker_confirm(&mut self, smart: bool) {
        let Some(branch) = self
            .branch_picker
            .as_ref()
            .and_then(|p| p.selected().cloned())
        else {
            return;
        };
        // A remote row targets the local name: an existing local branch is
        // simply checked out, and when there is none `git checkout <name>`
        // creates it as a tracking branch.
        let target = if branch.is_remote {
            branch
                .name
                .split_once('/')
                .map(|(_, rest)| rest)
                .unwrap_or(&branch.name)
                .to_string()
        } else {
            branch.name.clone()
        };
        self.close_branch_picker();
        if branch.is_current {
            self.set_message(format!("Already on {}", branch.name));
            return;
        }
        self.checkout_target(&target, smart);
    }

    /// Check out a branch name or commit. A dirty working tree refuses the
    /// switch unless `smart`, which stashes (untracked files included),
    /// switches, and pops the stash again.
    pub fn checkout_target(&mut self, target: &str, smart: bool) {
        // Same probe the selector rows use: works on every git backend and
        // honours `.tuicrignore`, so ignored churn does not block a switch
        // (git itself still refuses a checkout that would clobber it).
        let highlighter = self.theme.syntax_highlighter();
        let dirty = match Self::get_change_status_with_ignore(
            self.vcs.as_ref(),
            &self.vcs_info.root_path,
            highlighter,
            self.path_filter.as_deref(),
        ) {
            Ok(status) => status.staged || status.unstaged,
            Err(e) => {
                self.set_error(format!("Failed to read working tree status: {e}"));
                return;
            }
        };
        if dirty && !smart {
            self.set_error(format!(
                "Working tree has changes \u{2014} commit or stash them, or `:checkout! {target}` to stash, switch, and restore"
            ));
            return;
        }
        if dirty
            && let Err(e) = self
                .vcs
                .stash_push(&format!("tuicr: switching to {target}"))
        {
            self.set_error(format!("Stash failed: {e}"));
            return;
        }
        match self.vcs.checkout(target) {
            Ok(()) => {
                let mut message = format!("Switched to {target}");
                if dirty {
                    match self.vcs.stash_pop() {
                        Ok(()) => message.push_str(" (changes restored)"),
                        Err(e) => {
                            self.after_head_moved();
                            self.set_warning(format!(
                                "Switched to {target}, but restoring the stash failed: {e} \u{2014} your changes are in `git stash list`"
                            ));
                            return;
                        }
                    }
                }
                self.after_head_moved();
                self.set_message(message);
            }
            Err(e) => {
                if dirty {
                    // Put the tree back the way it was.
                    let _ = self.vcs.stash_pop();
                }
                let err = e.to_string();
                self.set_error(format!("Checkout failed: {}", err.trim()));
            }
        }
    }

    /// The checkout target for the selector's cursor row: the commit's own
    /// local branch when it is a tip, otherwise the commit itself (detached).
    /// `None` for pseudo rows and the overflow row.
    pub fn cursor_checkout_target(&self) -> Option<String> {
        let commit = self.commit_list.get(self.commit_list_cursor)?;
        if Self::is_special_commit(commit) {
            return None;
        }
        Some(
            commit
                .refs
                .iter()
                .find(|r| r.kind == RefKind::Local)
                .map(|r| r.name.clone())
                .or_else(|| commit.branch_name.clone())
                .unwrap_or_else(|| commit.id.clone()),
        )
    }

    /// The commit id under the selector's cursor, for `:branch` prompts.
    pub fn cursor_commit_id(&self) -> Option<String> {
        self.commit_list
            .get(self.commit_list_cursor)
            .filter(|c| !Self::is_special_commit(c))
            .map(|c| c.id.clone())
    }

    /// `n` in a selector: open the command line pre-filled with `branch `
    /// so the user types the name; the branch starts at `start`.
    pub fn start_new_branch_prompt(&mut self, start: Option<String>) {
        self.pending_branch_start = start;
        self.enter_command_mode();
        self.command_buffer = "branch ".to_string();
    }

    /// `:branch <name>`: create and switch to a branch at the pending start
    /// point (the commit `n` was pressed on) or HEAD.
    pub fn create_branch(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.set_error("Usage: :branch <name>");
            return;
        }
        let start = self.pending_branch_start.take();
        match self.vcs.create_branch(name, start.as_deref()) {
            Ok(()) => {
                self.after_head_moved();
                let at = start
                    .as_deref()
                    .map(|id| format!(" at {}", &id[..7.min(id.len())]))
                    .unwrap_or_default();
                self.set_message(format!("Created and switched to {name}{at}"));
            }
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("Branch creation failed: {}", err.trim()));
            }
        }
    }

    /// Abort the conflicted merge / cherry-pick / rebase / revert, closing the
    /// merge view if it is open.
    pub fn abort_operation(&mut self) {
        match self.vcs.abort_merge_operation() {
            Ok(op) => {
                if let Some(view) = self.merge_view.take() {
                    self.input_mode = view.return_mode;
                }
                self.after_head_moved();
                self.set_message(format!("Aborted {op}"));
            }
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("Abort failed: {}", err.trim()));
            }
        }
    }

    /// HEAD moved (checkout, new branch, abort): reopen the VCS so the
    /// cached branch name and head commit are current, then refresh every
    /// commit list on screen and the working-tree diff if that is what is
    /// being reviewed.
    pub(in crate::app) fn after_head_moved(&mut self) {
        match detect_vcs(
            self.vcs_open_options.git_backend_preference,
            self.vcs_open_options.diff_whitespace_mode,
        ) {
            Ok(vcs) => {
                self.vcs_info = vcs.info().clone();
                self.vcs = vcs;
            }
            Err(e) => self.set_warning(format!("Reopening the repository failed: {e}")),
        }
        if self.input_mode == InputMode::CommitSelect
            || (self.input_mode == InputMode::Command
                && self.command_return_mode == InputMode::CommitSelect)
        {
            self.refresh_commit_selector_rows();
        }
        self.refresh_log_rows_keep_cursor();
        if self.diff_source.includes_worktree_changes()
            && let Err(e) = self.reload_diff_files()
        {
            self.set_warning(format!("Reload after checkout failed: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, is_remote: bool, is_current: bool) -> BranchInfo {
        BranchInfo {
            name: name.to_string(),
            tip_id: "abc".to_string(),
            is_current,
            is_remote,
            upstream: None,
            ahead: 0,
            behind: 0,
            upstream_gone: false,
        }
    }

    fn picker(names: &[(&str, bool)]) -> BranchPicker {
        BranchPicker {
            branches: names
                .iter()
                .map(|(n, remote)| branch(n, *remote, false))
                .collect(),
            filter: String::new(),
            cursor: 0,
            return_mode: InputMode::Normal,
            picked: Vec::new(),
        }
    }

    #[test]
    fn picks_stay_a_pair_in_pick_order_and_fall_back_to_the_cursor() {
        let mut p = picker(&[("main", false), ("dev", false), ("origin/main", true)]);
        // nothing picked: the cursor row stands in
        p.cursor = 1;
        assert_eq!(p.picked_names(), ["dev"]);
        // picks accumulate in order and toggle off again
        p.toggle_pick(0);
        p.toggle_pick(2);
        assert_eq!(p.picked_names(), ["main", "origin/main"]);
        assert_eq!(p.pick_order(2), Some(2));
        p.toggle_pick(0);
        assert_eq!(p.picked_names(), ["origin/main"]);
        // a third pick pushes the oldest out
        p.toggle_pick(0);
        p.toggle_pick(1);
        assert_eq!(p.picked_names(), ["main", "dev"]);
        assert_eq!(p.pick_order(2), None);
    }

    #[test]
    fn visible_filters_case_insensitively_and_keeps_order() {
        let mut p = picker(&[
            ("main", false),
            ("feature/log", false),
            ("origin/main", true),
            ("origin/feature/log", true),
        ]);
        assert_eq!(p.visible(), vec![0, 1, 2, 3]);
        p.filter = "LOG".to_string();
        assert_eq!(p.visible(), vec![1, 3]);
        assert_eq!(p.selected().map(|b| b.name.as_str()), Some("feature/log"));
        p.filter = "nothing".to_string();
        assert!(p.visible().is_empty());
        assert!(p.selected().is_none());
    }

    #[test]
    fn cursor_clamps_when_the_filter_shrinks_the_list() {
        let mut p = picker(&[("main", false), ("dev", false), ("origin/dev", true)]);
        p.cursor = 2;
        p.filter = "dev".to_string();
        p.clamp_cursor();
        assert_eq!(p.cursor, 1);
        assert_eq!(p.selected().map(|b| b.name.as_str()), Some("origin/dev"));
    }
}
