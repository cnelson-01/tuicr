//! The changes window (`<leader>t`, `:changes`): an IDE's Commit tool
//! window docked where the file tree sits. It lists the working tree's
//! status in four groups (conflicts, staged, unstaged, untracked), stages
//! and unstages from the list, discards, commits, and previews the file
//! under the cursor in the diff pane by loading the staged or unstaged
//! review and jumping to it.
//!
//! Submodules are first-class: a submodule row carries its state (a moved
//! commit with ahead / behind counts, modified tracked content, untracked
//! content) and the entries inside a dirty submodule hang under it. Every
//! operation on a nested row runs git inside that submodule, and `c` on a
//! nested row commits there; a nested file previews through
//! [`DiffSource::Submodule`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::layout::Rect;

use super::{
    App, CHANGES_PREVIEW_DELAY, ConfirmAction, DiffSource, DiscardTarget, FocusedPanel, InputMode,
    SubmoduleDiffSource,
};
use crate::error::Result;
use crate::vcs::traits::VcsType;
use crate::vcs::{ChangeArea, StatusEntry};

/// One line of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesRow {
    /// A group heading with how many root entries it holds.
    Group { area: ChangeArea, count: usize },
    /// An index into [`ChangesPane::entries`].
    Entry(usize),
}

#[derive(Debug, Default)]
pub struct ChangesPane {
    /// Status rows straight from the VCS, in display order.
    pub entries: Vec<StatusEntry>,
    /// The lines on screen: headings and the entries folds leave visible.
    pub rows: Vec<ChangesRow>,
    pub cursor: usize,
    pub scroll_offset: usize,
    /// Rows the last render had.
    pub viewport_height: usize,
    /// Groups folded with `h` / Space on the heading.
    pub folded: HashSet<ChangeArea>,
    /// Submodule worktrees (relative to the root) whose nested rows are hidden.
    pub collapsed: HashSet<PathBuf>,
    /// Body rect from the last render, for mouse hit-testing.
    pub inner_area: Option<Rect>,
    /// Why the list is empty, when it is an error rather than a clean tree.
    pub error: Option<String>,
    /// Last cursor move; the preview fires once the cursor has rested.
    pub preview_at: Option<Instant>,
    /// `show_file_list` when the window opened, restored when it closes.
    pub show_file_list_before: bool,
}

impl ChangesPane {
    /// Root entries per area, for the title.
    pub fn counts(&self) -> Vec<(ChangeArea, usize)> {
        ChangeArea::ALL
            .iter()
            .map(|area| {
                (
                    *area,
                    self.entries
                        .iter()
                        .filter(|e| e.depth == 0 && e.area == *area)
                        .count(),
                )
            })
            .filter(|(_, n)| *n > 0)
            .collect()
    }

    pub fn entry_at(&self, row: usize) -> Option<&StatusEntry> {
        match self.rows.get(row)? {
            ChangesRow::Entry(idx) => self.entries.get(*idx),
            ChangesRow::Group { .. } => None,
        }
    }

    pub fn cursor_entry(&self) -> Option<&StatusEntry> {
        self.entry_at(self.cursor)
    }

    pub fn cursor_group(&self) -> Option<ChangeArea> {
        match self.rows.get(self.cursor)? {
            ChangesRow::Group { area, .. } => Some(*area),
            ChangesRow::Entry(_) => None,
        }
    }

    /// Whether any entry sits inside the submodule at `worktree`.
    pub fn has_nested(&self, worktree: &Path) -> bool {
        self.entries.iter().any(|e| e.repo == worktree)
    }

    /// Rebuild `rows` from `entries` honoring folds, keeping the cursor on
    /// the row it was on where that row survives.
    pub fn rebuild_rows(&mut self) {
        let anchor = self.rows.get(self.cursor).cloned();
        self.rows = build_rows(&self.entries, &self.folded, &self.collapsed);
        self.cursor = anchor
            .and_then(|row| self.rows.iter().position(|r| *r == row))
            .unwrap_or(self.cursor)
            .min(self.rows.len().saturating_sub(1));
    }

    /// Where the window lists entries belonging to `worktree` (the
    /// submodule row they hang under, or the group heading at the root).
    fn row_of_entry(&self, repo: &Path, area: ChangeArea, path: &Path) -> Option<usize> {
        self.rows.iter().position(|row| match row {
            ChangesRow::Entry(idx) => self
                .entries
                .get(*idx)
                .is_some_and(|e| e.repo == repo && e.area == area && e.path == path),
            ChangesRow::Group { .. } => false,
        })
    }
}

/// Headings and entries in display order. Nested entries follow their
/// submodule row and vanish with it when the submodule is collapsed or its
/// group folded.
fn build_rows(
    entries: &[StatusEntry],
    folded: &HashSet<ChangeArea>,
    collapsed: &HashSet<PathBuf>,
) -> Vec<ChangesRow> {
    let mut rows = Vec::new();
    for area in ChangeArea::ALL {
        let count = entries
            .iter()
            .filter(|e| e.depth == 0 && e.area == area)
            .count();
        if count == 0 {
            continue;
        }
        rows.push(ChangesRow::Group { area, count });
        if folded.contains(&area) {
            continue;
        }
        // Nested entries follow their root entry, so one flag says whether
        // the root entry they belong to is in this group, and one prefix
        // which collapsed submodule's subtree is being skipped.
        let mut parent_in_group = false;
        let mut hidden_under: Option<PathBuf> = None;
        for (idx, entry) in entries.iter().enumerate() {
            if entry.depth == 0 {
                parent_in_group = entry.area == area;
                hidden_under = None;
            }
            if !parent_in_group {
                continue;
            }
            if let Some(prefix) = hidden_under.as_ref() {
                if entry.repo.starts_with(prefix) {
                    continue;
                }
                hidden_under = None;
            }
            rows.push(ChangesRow::Entry(idx));
            if entry.is_submodule() {
                let worktree = entry.worktree();
                if collapsed.contains(&worktree) {
                    hidden_under = Some(worktree);
                }
            }
        }
    }
    rows
}

impl App {
    pub fn changes_pane_active(&self) -> bool {
        self.changes_pane.is_some()
    }

    pub fn changes_pane_focused(&self) -> bool {
        self.changes_pane.is_some() && self.focused_panel == FocusedPanel::Changes
    }

    pub fn changes_pane_inner_area(&self) -> Option<Rect> {
        self.changes_pane.as_ref().and_then(|pane| pane.inner_area)
    }

    /// The pane that lives in the left column: the changes window while it
    /// is open, the file tree otherwise.
    pub fn left_panel(&self) -> FocusedPanel {
        if self.changes_pane.is_some() {
            FocusedPanel::Changes
        } else {
            FocusedPanel::FileList
        }
    }

    /// Only git has an index to show. `--file` and `--all-files` reviews
    /// have no working tree review to preview into either.
    pub fn changes_pane_supported(&self) -> bool {
        self.vcs_info.vcs_type == VcsType::Git && !self.is_pristine_mode
    }

    /// Open the window (reading the status) and give it focus.
    pub fn enable_changes_pane(&mut self) {
        if self.changes_pane.is_some() {
            return;
        }
        if !self.changes_pane_supported() {
            self.set_message("The changes window needs a git working tree");
            return;
        }
        let mut pane = ChangesPane {
            show_file_list_before: self.show_file_list,
            ..ChangesPane::default()
        };
        match self.vcs.working_tree_status() {
            Ok(entries) => pane.entries = entries,
            Err(e) => pane.error = Some(e.to_string()),
        }
        pane.rebuild_rows();
        // Land on the first file rather than the heading above it.
        if matches!(pane.rows.first(), Some(ChangesRow::Group { .. })) && pane.rows.len() > 1 {
            pane.cursor = 1;
        }
        self.changes_pane = Some(pane);
        self.changes_pane_wanted = true;
        self.show_file_list = true;
        self.focused_panel = FocusedPanel::Changes;
    }

    /// Close the window. The file tree comes back as it was; focus returns
    /// to the diff when the window had it.
    pub fn disable_changes_pane(&mut self) {
        let Some(pane) = self.changes_pane.take() else {
            return;
        };
        self.changes_pane_wanted = false;
        self.show_file_list = pane.show_file_list_before;
        if self.focused_panel == FocusedPanel::Changes {
            self.focused_panel = FocusedPanel::Diff;
        }
    }

    /// `<leader>t`, like `<leader>g` for the log: hidden opens and focuses
    /// the window, open-but-unfocused focuses it, focused hides it.
    pub fn toggle_changes_pane(&mut self) {
        if self.changes_pane.is_none() {
            self.set_changes_pane_visible(true);
        } else if self.focused_panel != FocusedPanel::Changes {
            self.focused_panel = FocusedPanel::Changes;
            self.set_message("Changes focused \u{00b7} esc returns to the diff");
        } else {
            self.set_changes_pane_visible(false);
        }
    }

    pub fn set_changes_pane_visible(&mut self, visible: bool) {
        if visible {
            self.enable_changes_pane();
            if self.changes_pane.is_some() {
                self.set_message(self.changes_summary_message());
            }
        } else {
            self.disable_changes_pane();
            self.set_message("Changes window: hidden");
        }
    }

    /// `show_changes = true`: open the window once a review is on screen,
    /// the way the log pane auto-opens, without stealing focus.
    pub fn poll_changes_pane_autoshow(&mut self) -> bool {
        if !self.changes_pane_wanted
            || self.changes_pane.is_some()
            || self.input_mode != InputMode::Normal
            || !self.changes_pane_supported()
        {
            return false;
        }
        self.enable_changes_pane();
        self.focused_panel = FocusedPanel::Diff;
        self.changes_pane.is_some()
    }

    fn changes_summary_message(&self) -> String {
        let Some(pane) = self.changes_pane.as_ref() else {
            return String::new();
        };
        let counts = pane.counts();
        if counts.is_empty() {
            return "Changes: nothing to commit, working tree clean".to_string();
        }
        let parts: Vec<String> = counts
            .iter()
            .map(|(area, n)| format!("{n} {}", area.noun()))
            .collect();
        format!("Changes: {}", parts.join(" \u{00b7} "))
    }

    /// Re-read the status (after an operation, a history change, or a
    /// diff-watch tick), keeping the cursor on the same entry where it
    /// still exists, else on the same row.
    pub fn refresh_changes_pane(&mut self) {
        let Some(pane) = self.changes_pane.as_ref() else {
            return;
        };
        let anchor = pane
            .cursor_entry()
            .map(|e| (e.repo.clone(), e.area, e.path.clone()));
        let result = self.vcs.working_tree_status();
        let pane = self.changes_pane.as_mut().expect("checked above");
        match result {
            Ok(entries) => {
                pane.entries = entries;
                pane.error = None;
            }
            Err(e) => {
                pane.entries.clear();
                pane.error = Some(e.to_string());
            }
        }
        pane.rebuild_rows();
        if let Some((repo, area, path)) = anchor
            && let Some(row) = pane.row_of_entry(&repo, area, &path)
        {
            pane.cursor = row;
        }
        pane.scroll_offset = pane.scroll_offset.min(pane.cursor);
    }

    // Movement

    fn changes_set_cursor(&mut self, cursor: usize) {
        if let Some(pane) = self.changes_pane.as_mut() {
            pane.cursor = cursor.min(pane.rows.len().saturating_sub(1));
            pane.preview_at = Some(Instant::now());
        }
    }

    pub fn changes_move_down(&mut self, n: usize) {
        let Some(pane) = self.changes_pane.as_ref() else {
            return;
        };
        self.changes_set_cursor(pane.cursor.saturating_add(n.max(1)));
    }

    pub fn changes_move_up(&mut self, n: usize) {
        let Some(pane) = self.changes_pane.as_ref() else {
            return;
        };
        self.changes_set_cursor(pane.cursor.saturating_sub(n.max(1)));
    }

    pub fn changes_go_top(&mut self) {
        self.changes_set_cursor(0);
    }

    pub fn changes_go_bottom(&mut self) {
        self.changes_set_cursor(usize::MAX);
    }

    pub fn changes_page(&mut self, down: bool, half: bool) {
        let rows = self
            .changes_pane
            .as_ref()
            .map_or(0, |pane| pane.viewport_height)
            .max(2);
        let n = if half { rows / 2 } else { rows };
        if down {
            self.changes_move_down(n);
        } else {
            self.changes_move_up(n);
        }
    }

    /// Which row a screen row of the window body shows, if any.
    pub fn changes_row_at_screen_row(&self, screen_row: u16) -> Option<usize> {
        let pane = self.changes_pane.as_ref()?;
        let inner = pane.inner_area?;
        if screen_row < inner.y || screen_row >= inner.y + inner.height {
            return None;
        }
        let row = pane.scroll_offset + usize::from(screen_row - inner.y);
        (row < pane.rows.len()).then_some(row)
    }

    /// Mouse click: move the cursor there and open it (a heading folds).
    pub fn changes_click_row(&mut self, row: usize) {
        self.changes_set_cursor(row);
        if self
            .changes_pane
            .as_ref()
            .is_some_and(|p| p.cursor_group().is_some())
        {
            self.changes_toggle_fold();
        } else {
            self.changes_preview_now();
        }
    }

    // Folding

    fn changes_toggle_fold(&mut self) {
        let Some(pane) = self.changes_pane.as_mut() else {
            return;
        };
        if let Some(area) = pane.cursor_group() {
            if !pane.folded.remove(&area) {
                pane.folded.insert(area);
            }
            pane.rebuild_rows();
        } else if let Some(entry) = pane.cursor_entry().filter(|e| e.is_submodule()) {
            let worktree = entry.worktree();
            if !pane.has_nested(&worktree) {
                return;
            }
            if !pane.collapsed.remove(&worktree) {
                pane.collapsed.insert(worktree);
            }
            pane.rebuild_rows();
        }
    }

    /// `h`: fold the heading or submodule under the cursor; on a plain
    /// entry, jump to its heading (or its submodule row).
    pub fn changes_collapse(&mut self) {
        let Some(pane) = self.changes_pane.as_ref() else {
            return;
        };
        let on_group = pane.cursor_group().is_some();
        let on_open_submodule = pane
            .cursor_entry()
            .filter(|e| e.is_submodule())
            .is_some_and(|e| {
                pane.has_nested(&e.worktree()) && !pane.collapsed.contains(&e.worktree())
            });
        if on_group || on_open_submodule {
            self.changes_toggle_fold();
            return;
        }
        // Walk up to the enclosing submodule row or the heading.
        let Some(entry) = pane.cursor_entry() else {
            return;
        };
        let (repo, depth) = (entry.repo.clone(), entry.depth);
        let target = (0..pane.cursor).rev().find(|&row| match &pane.rows[row] {
            ChangesRow::Group { .. } => depth == 0,
            ChangesRow::Entry(idx) => {
                pane.entries[*idx].is_submodule()
                    && pane.entries[*idx].depth + 1 == depth
                    && pane.entries[*idx].worktree() == repo
            }
        });
        if let Some(row) = target {
            self.changes_set_cursor(row);
        }
    }

    /// `l`: unfold the heading or submodule under the cursor.
    pub fn changes_expand(&mut self) {
        let Some(pane) = self.changes_pane.as_ref() else {
            return;
        };
        let folded_group = pane
            .cursor_group()
            .is_some_and(|a| pane.folded.contains(&a));
        let collapsed_sub = pane
            .cursor_entry()
            .filter(|e| e.is_submodule())
            .is_some_and(|e| pane.collapsed.contains(&e.worktree()));
        if folded_group || collapsed_sub {
            self.changes_toggle_fold();
        }
    }

    pub fn changes_expand_all(&mut self) {
        if let Some(pane) = self.changes_pane.as_mut() {
            pane.folded.clear();
            pane.collapsed.clear();
            pane.rebuild_rows();
        }
    }

    pub fn changes_collapse_all(&mut self) {
        if let Some(pane) = self.changes_pane.as_mut() {
            pane.folded = ChangeArea::ALL.into_iter().collect();
            pane.rebuild_rows();
            pane.cursor = 0;
        }
    }

    // Preview

    /// Fire a pending preview once the cursor has rested. Returns whether
    /// anything changed on screen.
    pub fn poll_changes_preview(&mut self) -> bool {
        let Some(pane) = self.changes_pane.as_ref() else {
            return false;
        };
        let Some(at) = pane.preview_at else {
            return false;
        };
        if at.elapsed() < CHANGES_PREVIEW_DELAY
            || self.input_mode != InputMode::Normal
            || self.focused_panel != FocusedPanel::Changes
        {
            return false;
        }
        self.changes_pane
            .as_mut()
            .expect("checked above")
            .preview_at = None;
        self.changes_preview_now()
    }

    /// Load the review the cursor entry belongs to (staged / unstaged, at
    /// the root or inside its submodule) and put the diff cursor on the
    /// file. Conflicts are left to Enter, which opens the merge view.
    fn changes_preview_now(&mut self) -> bool {
        let Some(entry) = self
            .changes_pane
            .as_ref()
            .and_then(|p| p.cursor_entry())
            .cloned()
        else {
            return false;
        };
        if entry.area == ChangeArea::Conflict {
            return false;
        }
        let staged = entry.area == ChangeArea::Staged;
        let wanted = if entry.repo.as_os_str().is_empty() {
            if staged {
                DiffSource::Staged
            } else {
                DiffSource::Unstaged
            }
        } else {
            DiffSource::Submodule(Box::new(SubmoduleDiffSource {
                path: entry.repo.clone(),
                staged,
            }))
        };
        let mut changed = false;
        if self.diff_source != wanted {
            let result = match &wanted {
                DiffSource::Staged => self.load_staged_selection(),
                DiffSource::Unstaged => self.load_unstaged_selection(),
                DiffSource::Submodule(sub) => {
                    self.load_submodule_selection(sub.path.clone(), sub.staged)
                }
                _ => Ok(()),
            };
            match result {
                Ok(()) => changed = true,
                Err(e) => {
                    self.set_error(format!("Failed to load the diff: {}", e.to_string().trim()));
                    return false;
                }
            }
            // The load may have found nothing (and said so) and left the
            // old review in place: then there is no file to jump to.
            if self.diff_source != wanted {
                return changed;
            }
        }
        let target = self
            .diff_files
            .iter()
            .position(|f| f.display_path() == &entry.path)
            .or_else(|| {
                // An untracked directory (`dir/`) lists as its files.
                self.diff_files
                    .iter()
                    .position(|f| f.display_path().starts_with(&entry.path))
            });
        match target {
            Some(idx) => {
                if self.diff_state.current_file_idx != idx || !changed {
                    self.jump_to_file(idx);
                }
                true
            }
            None => {
                if entry.is_submodule() {
                    self.set_message(format!(
                        "{}: no diff for the submodule pointer here",
                        entry.worktree().display()
                    ));
                }
                changed
            }
        }
    }

    /// Enter: open the entry's diff and move to it; fold a heading or a
    /// submodule with nested rows; open the merge view from a conflict.
    pub fn changes_open(&mut self) {
        let Some(pane) = self.changes_pane.as_ref() else {
            return;
        };
        if pane.cursor_group().is_some() {
            self.changes_toggle_fold();
            return;
        }
        let Some(entry) = pane.cursor_entry().cloned() else {
            return;
        };
        if entry.area == ChangeArea::Conflict {
            if !entry.repo.as_os_str().is_empty() {
                self.set_message("Resolve conflicts inside the submodule with its own tuicr");
                return;
            }
            if let Err(e) = self.open_merge_view(InputMode::Normal) {
                self.set_error(format!("Failed to open the merge view: {e}"));
            }
            return;
        }
        if entry.is_submodule() && pane.has_nested(&entry.worktree()) {
            self.changes_toggle_fold();
            return;
        }
        if let Some(pane) = self.changes_pane.as_mut() {
            pane.preview_at = None;
        }
        if self.changes_preview_now() {
            self.focused_panel = FocusedPanel::Diff;
        }
    }

    // Staging

    fn changes_cursor_target(&mut self, verb: &str) -> Option<StatusEntry> {
        let entry = self
            .changes_pane
            .as_ref()
            .and_then(|p| p.cursor_entry())
            .cloned();
        if entry.is_none() {
            self.set_warning(format!("Move the cursor onto a file to {verb}"));
        }
        entry
    }

    /// After an operation: re-read the status, reload the diff on screen,
    /// and say what happened.
    fn after_changes_op(&mut self, result: Result<()>, done: String, failed: &str) {
        match result {
            Ok(()) => self.after_local_change(done),
            Err(e) => self.set_error(format!("{failed}: {}", e.to_string().trim())),
        }
    }

    /// Space: the IDE checkbox. A staged entry is unstaged, anything else
    /// staged; a heading folds.
    pub fn changes_toggle_stage(&mut self) {
        let on_group = self
            .changes_pane
            .as_ref()
            .is_some_and(|p| p.cursor_group().is_some());
        if on_group {
            self.changes_toggle_fold();
            return;
        }
        let Some(entry) = self.changes_cursor_target("stage") else {
            return;
        };
        match entry.area {
            ChangeArea::Staged => self.changes_unstage(),
            ChangeArea::Conflict => {
                self.set_message("Enter opens the merge view; s marks the conflict resolved")
            }
            _ => self.changes_stage(),
        }
    }

    pub fn changes_stage(&mut self) {
        let Some(entry) = self.changes_cursor_target("stage") else {
            return;
        };
        if entry.area == ChangeArea::Staged {
            self.set_message(format!("{} is staged already", entry.worktree().display()));
            return;
        }
        let result = self
            .vcs
            .stage_in(&entry.repo, std::slice::from_ref(&entry.path));
        self.after_changes_op(
            result,
            format!("Staged {}", entry.worktree().display()),
            "Failed to stage",
        );
    }

    pub fn changes_unstage(&mut self) {
        let Some(entry) = self.changes_cursor_target("unstage") else {
            return;
        };
        if entry.area != ChangeArea::Staged {
            self.set_message(format!("{} is not staged", entry.worktree().display()));
            return;
        }
        let result = self
            .vcs
            .unstage_in(&entry.repo, std::slice::from_ref(&entry.path));
        self.after_changes_op(
            result,
            format!("Unstaged {}", entry.worktree().display()),
            "Failed to unstage",
        );
    }

    /// The worktree the cursor is in: a nested row's submodule, else the
    /// root (a heading or an empty window means the root).
    fn changes_cursor_repo(&self) -> PathBuf {
        self.changes_pane
            .as_ref()
            .and_then(|p| p.cursor_entry())
            .map(|e| e.repo.clone())
            .unwrap_or_default()
    }

    fn worktree_label(repo: &Path) -> String {
        if repo.as_os_str().is_empty() {
            "the working tree".to_string()
        } else {
            repo.display().to_string()
        }
    }

    /// `a`: stage everything in the cursor's worktree, untracked included.
    pub fn changes_stage_all(&mut self) {
        let repo = self.changes_cursor_repo();
        let result = self.vcs.stage_all_in(&repo);
        self.after_changes_op(
            result,
            format!("Staged everything in {}", Self::worktree_label(&repo)),
            "Failed to stage",
        );
    }

    /// `A`: empty the cursor's worktree's index back to HEAD.
    pub fn changes_unstage_all(&mut self) {
        let repo = self.changes_cursor_repo();
        let result = self.vcs.unstage_all_in(&repo);
        self.after_changes_op(
            result,
            format!("Unstaged everything in {}", Self::worktree_label(&repo)),
            "Failed to unstage",
        );
    }

    /// `X`: ask before discarding the entry. A submodule row goes back to
    /// the recorded commit; a file to HEAD; a new file is deleted.
    pub fn changes_request_discard(&mut self) {
        let Some(entry) = self.changes_cursor_target("discard") else {
            return;
        };
        if entry.area == ChangeArea::Conflict {
            self.set_message(
                "Resolve the conflict from the merge view (Enter), or abort the operation",
            );
            return;
        }
        self.pending_discard = Some(if entry.is_submodule() {
            DiscardTarget::Submodule {
                repo: entry.repo.clone(),
                path: entry.path.clone(),
                to_head: entry.area == ChangeArea::Staged,
            }
        } else {
            DiscardTarget::Path {
                repo: entry.repo.clone(),
                path: entry.path.clone(),
                untracked: entry.area == ChangeArea::Untracked,
            }
        });
        self.enter_confirm_mode(ConfirmAction::DiscardChanges);
    }

    /// `c` / `C`: commit (amend) the cursor's worktree through the message
    /// composer. A nested row commits inside its submodule.
    pub fn changes_commit(&mut self, amend: bool) {
        let repo = self.changes_cursor_repo();
        let has_staged = self.changes_pane.as_ref().is_some_and(|p| {
            p.entries
                .iter()
                .any(|e| e.repo == repo && e.area == ChangeArea::Staged)
        });
        if !has_staged && !amend {
            self.set_warning(format!(
                "Nothing staged in {} \u{00b7} Space or s stages a file, a stages everything",
                Self::worktree_label(&repo)
            ));
            return;
        }
        self.start_commit_composer_in(&repo, amend);
    }

    /// The VCS a Submodule review reads context lines through.
    pub(in crate::app) fn submodule_review_vcs(&self) -> Option<&dyn crate::vcs::VcsBackend> {
        match &self.diff_source {
            DiffSource::Submodule(_) => self.submodule_vcs.as_deref(),
            _ => None,
        }
    }

    /// A `Submodule` review's worktree, relative to the root, for the
    /// `s` / `u` keys in the diff.
    pub(in crate::app) fn submodule_review_path(&self) -> Option<PathBuf> {
        match &self.diff_source {
            DiffSource::Submodule(sub) => Some(sub.path.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(repo: &str, area: ChangeArea, path: &str, depth: usize, sub: bool) -> StatusEntry {
        StatusEntry {
            repo: PathBuf::from(repo),
            area,
            path: PathBuf::from(path),
            orig_path: None,
            code: "M".to_string(),
            submodule: sub.then(crate::vcs::SubmoduleState::default),
            depth,
        }
    }

    fn sample() -> Vec<StatusEntry> {
        vec![
            entry("", ChangeArea::Staged, "a.rs", 0, false),
            entry("", ChangeArea::Unstaged, "lib/sub", 0, true),
            entry("lib/sub", ChangeArea::Staged, "x.c", 1, false),
            entry("lib/sub", ChangeArea::Unstaged, "deep", 1, true),
            entry("lib/sub/deep", ChangeArea::Untracked, "n.txt", 2, false),
            entry("lib/sub", ChangeArea::Untracked, "y.txt", 1, false),
            entry("", ChangeArea::Unstaged, "z.rs", 0, false),
            entry("", ChangeArea::Untracked, "new.txt", 0, false),
        ]
    }

    fn labels(rows: &[ChangesRow], entries: &[StatusEntry]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                ChangesRow::Group { area, count } => format!("{}:{count}", area.label()),
                ChangesRow::Entry(i) => entries[*i].worktree().display().to_string(),
            })
            .collect()
    }

    #[test]
    fn rows_group_root_entries_and_nest_submodule_entries() {
        let entries = sample();
        let rows = build_rows(&entries, &HashSet::new(), &HashSet::new());
        assert_eq!(
            labels(&rows, &entries),
            vec![
                "Staged:1",
                "a.rs",
                "Unstaged:2",
                "lib/sub",
                "lib/sub/x.c",
                "lib/sub/deep",
                "lib/sub/deep/n.txt",
                "lib/sub/y.txt",
                "z.rs",
                "Untracked:1",
                "new.txt",
            ]
        );
    }

    #[test]
    fn folding_a_group_hides_its_entries_and_their_nested_rows() {
        let entries = sample();
        let folded: HashSet<ChangeArea> = [ChangeArea::Unstaged].into_iter().collect();
        let rows = build_rows(&entries, &folded, &HashSet::new());
        assert_eq!(
            labels(&rows, &entries),
            vec!["Staged:1", "a.rs", "Unstaged:2", "Untracked:1", "new.txt"]
        );
    }

    #[test]
    fn collapsing_a_submodule_hides_only_its_subtree() {
        let entries = sample();
        let collapsed: HashSet<PathBuf> = [PathBuf::from("lib/sub/deep")].into_iter().collect();
        let rows = build_rows(&entries, &HashSet::new(), &collapsed);
        assert_eq!(
            labels(&rows, &entries),
            vec![
                "Staged:1",
                "a.rs",
                "Unstaged:2",
                "lib/sub",
                "lib/sub/x.c",
                "lib/sub/deep",
                "lib/sub/y.txt",
                "z.rs",
                "Untracked:1",
                "new.txt",
            ]
        );
        let collapsed: HashSet<PathBuf> = [PathBuf::from("lib/sub")].into_iter().collect();
        let rows = build_rows(&entries, &HashSet::new(), &collapsed);
        assert_eq!(
            labels(&rows, &entries),
            vec![
                "Staged:1",
                "a.rs",
                "Unstaged:2",
                "lib/sub",
                "z.rs",
                "Untracked:1",
                "new.txt",
            ]
        );
    }

    #[test]
    fn rebuild_keeps_the_cursor_on_its_row() {
        let mut pane = ChangesPane {
            entries: sample(),
            ..ChangesPane::default()
        };
        pane.rebuild_rows();
        pane.cursor = 8; // z.rs
        pane.collapsed.insert(PathBuf::from("lib/sub"));
        pane.rebuild_rows();
        assert_eq!(pane.rows[pane.cursor], ChangesRow::Entry(6));
        assert_eq!(
            pane.counts(),
            vec![
                (ChangeArea::Staged, 1),
                (ChangeArea::Unstaged, 2),
                (ChangeArea::Untracked, 1)
            ]
        );
    }
}
