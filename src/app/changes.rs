//! The Changes view, an IDE's Commit / Local Changes tool window: one tree
//! of every local change (conflicts, staged, unstaged, unversioned), each
//! entry previewed in the diff pane, staged or unstaged with Space, and
//! submodules as entries that open to their own changes.
//!
//! The pane borrows the file tree's column, focus value
//! (`FocusedPanel::FileList`) and mouse rectangles, so layout, splitter drags
//! and the focus cycle need no new cases. It does *not* borrow
//! `file_list_state`: every diff install and cursor move rewrites that
//! (`apply_diff_files`, `jump_to_file`, `update_current_file_from_cursor`),
//! so the pane keeps its own `ListState`. `diff_files` holds only the
//! selected entry's diff; `DiffSource::Changes(key)` names it so reloads and
//! the diff watch re-fetch exactly that entry.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ratatui::widgets::ListState;

use super::diff_load::diff_files_fingerprint;
use super::{App, ConfirmAction, DiffSource, DiscardTarget, FocusedPanel, InputMode};
use crate::error::TuicrError;
use crate::model::DiffFile;
use crate::model::review::SessionDiffSource;
use crate::vcs::traits::VcsType;
use crate::vcs::{
    ChangeBulkOp, ChangeDiffKey, ChangeEntry, ChangeEntryKind, ChangeGroup, ChangeSet, ChangeStatus,
};

/// A submodule the user expanded: its own change set, or why it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmoduleExpansion {
    Loaded(ChangeSet),
    Failed(String),
}

#[derive(Debug, Default)]
pub struct ChangesView {
    /// Top-level entries, `.tuicrignore` and `--file` filter applied, sorted.
    pub set: ChangeSet,
    /// Collapsed group headers, keyed by (owning submodule, group).
    pub collapsed_groups: HashSet<(Option<PathBuf>, ChangeGroup)>,
    /// Collapsed directory rows (grouping on), keyed by (submodule, group, dir).
    pub collapsed_dirs: HashSet<(Option<PathBuf>, ChangeGroup, String)>,
    /// Expanded dirty submodules by superproject-relative path.
    pub submodules: HashMap<PathBuf, SubmoduleExpansion>,
    /// `f`: group entries by directory instead of the flat list.
    pub group_by_directory: bool,
    /// Per-entry diffs already fetched; cleared on every refresh.
    pub diff_cache: HashMap<ChangeDiffKey, Vec<DiffFile>>,
    /// The entry whose diff the pane shows.
    pub selected: Option<ChangeDiffKey>,
    /// `set.fingerprint()` at install time, compared by the diff watch.
    pub fingerprint: u64,
    /// Flattened rows, rebuilt by [`ChangesView::rebuild_rows`] after every
    /// state change. Renderer, handlers and mouse hit-testing all read this
    /// one list, so indices agree by construction.
    pub rows: Vec<ChangesRow>,
    /// The pane's own cursor and scroll offset (not `App::file_list_state`).
    pub list: ListState,
    /// Set during render, like `FileListState`.
    pub viewport_height: usize,
    pub viewport_width: usize,
    pub max_content_width: usize,
    /// Horizontal pan (`Left` / `Right` on a non-expandable row).
    pub scroll_x: usize,
}

impl ChangesView {
    pub fn rebuild_rows(&mut self) {
        self.rows = build_changes_rows(self);
    }

    pub fn selected_idx(&self) -> usize {
        self.list.selected().unwrap_or(0)
    }

    pub fn select(&mut self, idx: usize) {
        self.list
            .select(Some(idx.min(self.rows.len().saturating_sub(1))));
    }

    pub fn selected_row(&self) -> Option<&ChangesRow> {
        self.rows.get(self.selected_idx())
    }
}

/// One rendered row of the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesRow {
    Group {
        submodule: Option<PathBuf>,
        group: ChangeGroup,
        count: usize,
        depth: usize,
        expanded: bool,
    },
    Directory {
        submodule: Option<PathBuf>,
        group: ChangeGroup,
        /// Repo-relative directory path (`src/app`).
        path: String,
        /// Last component (`app`).
        label: String,
        depth: usize,
        expanded: bool,
    },
    Entry {
        key: ChangeDiffKey,
        entry: ChangeEntry,
        depth: usize,
        /// `Some(expanded)` for a dirty submodule, `None` otherwise.
        expanded: Option<bool>,
    },
    /// Dim informational row (a failed submodule probe or expansion); not
    /// selectable.
    Note { text: String, depth: usize },
    Uninitialized {
        submodule: Option<PathBuf>,
        path: PathBuf,
        depth: usize,
    },
}

/// Cells per indent level. Shared by the renderer and the mouse hit-test.
pub const CHANGES_INDENT: usize = 2;

impl ChangesRow {
    pub fn depth(&self) -> usize {
        match self {
            ChangesRow::Group { depth, .. }
            | ChangesRow::Directory { depth, .. }
            | ChangesRow::Entry { depth, .. }
            | ChangesRow::Note { depth, .. }
            | ChangesRow::Uninitialized { depth, .. } => *depth,
        }
    }

    pub fn selectable(&self) -> bool {
        !matches!(self, ChangesRow::Note { .. })
    }

    pub fn entry_key(&self) -> Option<&ChangeDiffKey> {
        match self {
            ChangesRow::Entry { key, .. } => Some(key),
            _ => None,
        }
    }

    /// Column (before `scroll_x`) of the `▶`/`▼` glyph, for rows that have
    /// one: groups, directories and dirty submodules.
    pub fn expander_cell(&self) -> Option<usize> {
        match self {
            ChangesRow::Group { depth, .. } | ChangesRow::Directory { depth, .. } => {
                Some(depth * CHANGES_INDENT)
            }
            ChangesRow::Entry {
                depth,
                expanded: Some(_),
                ..
            } => Some(depth * CHANGES_INDENT),
            _ => None,
        }
    }

    /// Column (before `scroll_x`) of the checkbox: indent, expander cell,
    /// space, checkbox. `None` for rows without one (groups use the whole
    /// row; conflicted entries have no checkbox).
    pub fn checkbox_cell(&self) -> Option<usize> {
        match self {
            ChangesRow::Entry { depth, key, .. } if key.group != ChangeGroup::Conflicted => {
                Some(depth * CHANGES_INDENT + 2)
            }
            _ => None,
        }
    }

    /// Column where the glyph starts; a click left of this on an entry means
    /// "toggle staging" (unless it hit the expander cell).
    pub fn glyph_cell(&self) -> usize {
        self.depth() * CHANGES_INDENT + 4
    }
}

/// Flatten the view into rows. Pure, so the renderer (Part C) and the
/// handlers (Part B) agree on indices by construction.
pub fn build_changes_rows(view: &ChangesView) -> Vec<ChangesRow> {
    let mut rows = Vec::new();
    push_set(&mut rows, view, &view.set, None, 0);
    rows
}

fn push_set(
    rows: &mut Vec<ChangesRow>,
    view: &ChangesView,
    set: &ChangeSet,
    submodule: Option<&Path>,
    depth: usize,
) {
    let owner = submodule.map(Path::to_path_buf);
    for group in ChangeGroup::ALL {
        let entries = set.group(group);
        if entries.is_empty() {
            continue;
        }
        let expanded = !view.collapsed_groups.contains(&(owner.clone(), group));
        rows.push(ChangesRow::Group {
            submodule: owner.clone(),
            group,
            count: entries.len(),
            depth,
            expanded,
        });
        if !expanded {
            continue;
        }
        if view.group_by_directory {
            push_grouped(rows, view, entries, submodule, group, depth + 1);
        } else {
            for entry in entries {
                push_entry(rows, view, entry, submodule, group, depth + 1);
            }
        }
    }
    for path in &set.uninitialized_submodules {
        rows.push(ChangesRow::Uninitialized {
            submodule: owner.clone(),
            path: path.clone(),
            depth,
        });
    }
}

fn push_entry(
    rows: &mut Vec<ChangesRow>,
    view: &ChangesView,
    entry: &ChangeEntry,
    submodule: Option<&Path>,
    group: ChangeGroup,
    depth: usize,
) {
    let key = ChangeDiffKey {
        group,
        path: entry.path.clone(),
        submodule: submodule.map(Path::to_path_buf),
    };
    let full = key.display_path();
    let expanded = entry
        .expandable()
        .then(|| view.submodules.contains_key(&full));
    rows.push(ChangesRow::Entry {
        key,
        entry: entry.clone(),
        depth,
        expanded,
    });
    if let Some(err) = entry.submodule().and_then(|sm| sm.probe_error.as_deref()) {
        rows.push(ChangesRow::Note {
            text: format!("could not read submodule: {err}"),
            depth: depth + 1,
        });
    }
    if expanded == Some(true) {
        match view.submodules.get(&full) {
            Some(SubmoduleExpansion::Loaded(inner)) => {
                push_set(rows, view, inner, Some(&full), depth + 1);
            }
            Some(SubmoduleExpansion::Failed(err)) => rows.push(ChangesRow::Note {
                text: format!("could not read submodule: {err}"),
                depth: depth + 1,
            }),
            None => {}
        }
    }
}

/// Directory grouping: one `Directory` row per path component, emitted the
/// first time an ancestor is seen (entries are sorted by path, so the
/// children of a directory are contiguous). Submodule and embedded-repo
/// entries sit at the group level under their own path.
fn push_grouped(
    rows: &mut Vec<ChangesRow>,
    view: &ChangesView,
    entries: &[ChangeEntry],
    submodule: Option<&Path>,
    group: ChangeGroup,
    depth: usize,
) {
    let owner = submodule.map(Path::to_path_buf);
    let mut seen: HashSet<String> = HashSet::new();
    for entry in entries {
        let nested = matches!(entry.kind, ChangeEntryKind::File);
        let mut ancestors: Vec<String> = Vec::new();
        if nested {
            let mut current = entry.path.parent();
            while let Some(parent) = current {
                if parent != Path::new("") {
                    ancestors.push(parent.to_string_lossy().into_owned());
                }
                current = parent.parent();
            }
            ancestors.reverse();
        }
        let mut visible = true;
        let mut level = depth;
        for dir in &ancestors {
            if !visible {
                break;
            }
            let expanded = !view
                .collapsed_dirs
                .contains(&(owner.clone(), group, dir.clone()));
            if seen.insert(dir.clone()) {
                let label = Path::new(dir)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dir.clone());
                rows.push(ChangesRow::Directory {
                    submodule: owner.clone(),
                    group,
                    path: dir.clone(),
                    label,
                    depth: level,
                    expanded,
                });
            }
            visible = expanded;
            level += 1;
        }
        if visible {
            push_entry(rows, view, entry, submodule, group, level);
        }
    }
}

impl App {
    /// The pane is on screen and owns the diff: the view exists *and* the
    /// review is `DiffSource::Changes`. Loading any other review leaves the
    /// view state behind and the left column goes back to the file tree.
    pub fn changes_active(&self) -> bool {
        self.changes.is_some() && matches!(self.diff_source, DiffSource::Changes(_))
    }

    /// The pane has keyboard focus.
    pub fn changes_focused(&self) -> bool {
        self.changes_active() && self.focused_panel == FocusedPanel::FileList
    }

    /// The cached rows (empty when the pane is not active).
    pub fn changes_rows(&self) -> &[ChangesRow] {
        match &self.changes {
            Some(view) if self.changes_active() => &view.rows,
            _ => &[],
        }
    }

    /// Row under the pane cursor.
    pub fn changes_selected_row(&self) -> Option<&ChangesRow> {
        let view = self.changes.as_ref().filter(|_| self.changes_active())?;
        view.selected_row()
    }

    pub fn changes_selected_entry(&self) -> Option<(ChangeDiffKey, ChangeEntry)> {
        match self.changes_selected_row() {
            Some(ChangesRow::Entry { key, entry, .. }) => Some((key.clone(), entry.clone())),
            _ => None,
        }
    }

    /// The entry the diff pane previews, for hunk-op gating and for file
    /// ops issued from the diff pane.
    pub fn changes_previewed_entry(&self) -> Option<(ChangeDiffKey, ChangeEntry)> {
        let DiffSource::Changes(Some(key)) = &self.diff_source else {
            return None;
        };
        let view = self.changes.as_ref()?;
        let set = match &key.submodule {
            None => &view.set,
            Some(sub) => match view.submodules.get(sub) {
                Some(SubmoduleExpansion::Loaded(inner)) => inner,
                _ => return None,
            },
        };
        set.find(key.group, &key.path)
            .map(|entry| (key.clone(), entry.clone()))
    }

    /// Row index under a screen row of the left column (mouse), using the
    /// pane's own offset.
    pub fn changes_row_at_screen_row(&self, screen_row: u16) -> Option<usize> {
        let inner = self.file_list_inner_area?;
        let view = self.changes.as_ref().filter(|_| self.changes_active())?;
        if screen_row < inner.y || screen_row >= inner.y + inner.height {
            return None;
        }
        let idx = view.list.offset() + (screen_row - inner.y) as usize;
        (idx < view.rows.len()).then_some(idx)
    }

    // ---- Opening and closing ---------------------------------------------

    /// `<leader>C`: hidden → open and focus; open → focus (and re-read);
    /// focused → close.
    pub fn toggle_changes_view(&mut self) {
        if !self.changes_active() {
            self.open_changes_view();
        } else if self.focused_panel != FocusedPanel::FileList || !self.show_file_list {
            self.focus_changes_view();
        } else {
            self.close_changes_view();
        }
    }

    /// `:changes` toggles; `:set changes` / `:set nochanges` set.
    pub fn set_changes_visible(&mut self, visible: bool) {
        match (visible, self.changes_active()) {
            (true, false) => self.open_changes_view(),
            (true, true) => self.focus_changes_view(),
            (false, true) => self.close_changes_view(),
            (false, false) => self.set_message("The Changes view is not open"),
        }
    }

    /// The view is open but something else has focus: show and focus the
    /// pane and re-read the change set, so the tree is honest even without
    /// the diff watch.
    fn focus_changes_view(&mut self) {
        self.show_file_list = true;
        self.focused_panel = FocusedPanel::FileList;
        self.refresh_changes_view(None);
    }

    /// Opens the view (always `InputMode::Normal`), or the degraded flat
    /// working-tree review on hg / jj.
    pub fn open_changes_view(&mut self) {
        // From the selector, `C` is the act of leaving the PR review, like
        // Enter on a local target: the session and the source are replaced
        // below. From the review itself, say how to get there.
        if matches!(self.diff_source, DiffSource::PullRequest(_))
            && self.input_mode != InputMode::CommitSelect
        {
            self.set_warning(
                "The Changes view works on the local checkout; leave the PR review first (:commits, then C)",
            );
            return;
        }
        if self.is_pristine_mode || self.vcs_info.vcs_type == VcsType::File {
            self.set_warning("The Changes view needs a repository checkout");
            return;
        }
        if self.vcs_info.vcs_type != VcsType::Git {
            self.open_flat_working_copy_review();
            return;
        }
        let set = match self.vcs.get_change_set() {
            Ok(set) => self.filter_change_set(set, None),
            Err(TuicrError::UnsupportedOperation(reason)) => {
                self.set_warning(format!("Changes view unavailable: {reason}"));
                return;
            }
            Err(e) => {
                self.set_error(format!("Changes view unavailable: {e}"));
                return;
            }
        };
        self.ensure_working_tree_session(SessionDiffSource::StagedAndUnstaged);
        self.leave_commit_review();

        // Grouping and collapsed sets survive a close-by-loading-another-review
        // (`changes_active()` false while `changes` is still `Some`); fetched
        // data (expansions, cached diffs) does not.
        let previous = self.changes.take();
        let mut view = ChangesView {
            fingerprint: set.fingerprint(),
            set,
            ..Default::default()
        };
        if let Some(previous) = previous {
            view.group_by_directory = previous.group_by_directory;
            view.collapsed_groups = previous.collapsed_groups;
            view.collapsed_dirs = previous.collapsed_dirs;
        }
        view.rebuild_rows();
        self.changes = Some(view);

        self.diff_source = DiffSource::Changes(None);
        self.diff_files.clear();
        self.diff_state = Default::default();
        self.file_list_state = Default::default();
        self.clear_expanded_gaps();
        self.rebuild_annotations();
        self.input_mode = InputMode::Normal;
        self.show_file_list = true;
        self.focused_panel = FocusedPanel::FileList;
        match self.changes_first_entry_row() {
            Some(idx) => self.changes_select(idx),
            None => self.changes_select_first(),
        }
        self.refresh_repo_status();
        self.set_message("Changes · space stages · c commits · <leader>C closes");
    }

    /// hg / jj have no index, so the honest fallback is the plain working
    /// copy as the file tree already shows it.
    fn open_flat_working_copy_review(&mut self) {
        let highlighter = self.theme.syntax_highlighter();
        let fetched = Self::get_working_tree_diff_with_ignore(
            self.vcs.as_ref(),
            &self.vcs_info.root_path,
            highlighter,
            self.path_filter.as_deref(),
        );
        let files = match fetched {
            Ok(files) => files,
            Err(TuicrError::NoChanges) => Vec::new(),
            Err(e) => {
                self.set_error(format!("Failed to load the working copy: {e}"));
                return;
            }
        };
        self.ensure_working_tree_session(SessionDiffSource::WorkingTree);
        self.leave_commit_review();
        self.diff_source = DiffSource::WorkingTree;
        self.input_mode = InputMode::Normal;
        if files.is_empty() {
            self.clear_review_diff();
        } else {
            self.apply_diff_files(files);
        }
        self.show_file_list = true;
        self.focused_panel = FocusedPanel::FileList;
        let colocated = if self.vcs_info.vcs_type == VcsType::Jujutsu {
            " (colocated jj included)"
        } else {
            ""
        };
        self.set_message(format!(
            "No index on {}: showing the working copy as a flat list{colocated}",
            self.vcs_info.vcs_type
        ));
    }

    /// Closes the view: the left column goes back to the file tree over the
    /// combined working-tree review (or an empty diff when the tree is
    /// clean). Focus is left where it was.
    pub fn close_changes_view(&mut self) {
        let was_active = self.changes_active();
        self.changes = None;
        if !was_active {
            return;
        }
        self.diff_source = DiffSource::StagedAndUnstaged;
        let highlighter = self.theme.syntax_highlighter();
        let fetched = Self::get_working_tree_diff_with_ignore(
            self.vcs.as_ref(),
            &self.vcs_info.root_path,
            highlighter,
            self.path_filter.as_deref(),
        );
        match fetched {
            Ok(files) => {
                self.apply_diff_files(files);
            }
            Err(TuicrError::NoChanges) => self.clear_review_diff(),
            Err(e) => {
                self.clear_review_diff();
                self.set_error(format!("Failed to load the working tree: {e}"));
                return;
            }
        }
        self.set_message("Changes view closed");
    }

    /// Keeps the current session when it already belongs to the working
    /// tree (reviewed marks and comments carry over); otherwise loads or
    /// creates the `wanted` one.
    fn ensure_working_tree_session(&mut self, wanted: SessionDiffSource) {
        if matches!(
            self.session.diff_source,
            SessionDiffSource::WorkingTree
                | SessionDiffSource::Staged
                | SessionDiffSource::Unstaged
                | SessionDiffSource::StagedAndUnstaged
        ) {
            return;
        }
        self.session = Self::load_or_create_session(&self.vcs_info, wanted);
        if let Err(e) = self.reset_persisted_session_tracking() {
            self.set_warning(format!("Failed to track the review session: {e}"));
        }
    }

    /// The inline commit selector must never appear over the view.
    fn leave_commit_review(&mut self) {
        self.review_commits.clear();
        self.commit_selection_range = None;
        self.saved_inline_selection = None;
    }

    /// An empty diff pane, the way `after_local_change` leaves one.
    fn clear_review_diff(&mut self) {
        self.diff_files.clear();
        self.diff_state = Default::default();
        self.file_list_state = Default::default();
        self.clear_expanded_gaps();
        self.rebuild_annotations();
    }

    /// `.tuicrignore` and the `--file` path filter, then sort. `prefix` is
    /// the submodule path of an inner set, so ignore rules see
    /// superproject-relative paths.
    fn filter_change_set(&self, mut set: ChangeSet, prefix: Option<&Path>) -> ChangeSet {
        Self::filter_change_set_with(
            &mut set,
            &self.vcs_info.root_path,
            self.path_filter.as_deref(),
            prefix,
        );
        set
    }

    pub(in crate::app) fn filter_change_set_with(
        set: &mut ChangeSet,
        root_path: &Path,
        path_filter: Option<&str>,
        prefix: Option<&Path>,
    ) {
        let full = |path: &Path| match prefix {
            Some(prefix) => prefix.join(path),
            None => path.to_path_buf(),
        };
        let candidates: Vec<PathBuf> = set.paths().iter().map(|p| full(p)).collect();
        let mut kept = crate::tuicrignore::filter_paths(root_path, candidates);
        if let Some(filter) = path_filter {
            kept = Self::filter_paths_by_pathspec(kept, filter);
        }
        let kept: HashSet<PathBuf> = kept.into_iter().collect();
        set.retain_paths(|path| kept.contains(&full(path)));
        set.sort();
    }

    // ---- Selection and preview -------------------------------------------

    pub(in crate::app) fn changes_view(&self) -> Option<&ChangesView> {
        self.changes.as_ref().filter(|_| self.changes_active())
    }

    fn changes_view_mut(&mut self) -> Option<&mut ChangesView> {
        if !self.changes_active() {
            return None;
        }
        self.changes.as_mut()
    }

    fn changes_first_entry_row(&self) -> Option<usize> {
        self.changes_rows()
            .iter()
            .position(|row| matches!(row, ChangesRow::Entry { .. }))
    }

    /// The nearest selectable row to `idx`, looking in the direction of
    /// travel first so `j` / `k` step over Note rows, then the other way.
    fn selectable_near(rows: &[ChangesRow], idx: usize, forward: bool) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        let idx = idx.min(rows.len() - 1);
        let ahead = || (idx..rows.len()).find(|&i| rows[i].selectable());
        let behind = || (0..=idx).rev().find(|&i| rows[i].selectable());
        if forward {
            ahead().or_else(behind)
        } else {
            behind().or_else(ahead)
        }
    }

    /// The set an entry key addresses: the superproject's, or an expanded
    /// submodule's.
    fn changes_set_for(&self, submodule: Option<&Path>) -> Option<&ChangeSet> {
        let view = self.changes.as_ref()?;
        match submodule {
            None => Some(&view.set),
            Some(sub) => match view.submodules.get(sub) {
                Some(SubmoduleExpansion::Loaded(inner)) => Some(inner),
                _ => None,
            },
        }
    }

    /// Moves the cursor to `idx` (clamped, Note rows skipped) and previews
    /// the entry under it.
    pub fn changes_select(&mut self, idx: usize) {
        let Some(view) = self.changes_view_mut() else {
            return;
        };
        let forward = idx >= view.selected_idx();
        let Some(target) = Self::selectable_near(&view.rows, idx, forward) else {
            view.select(0);
            return;
        };
        view.select(target);
        self.changes_follow_selection();
    }

    /// `j` / `k` with a count: move the cursor by `delta` rows.
    pub fn changes_move(&mut self, delta: isize) {
        let Some(view) = self.changes_view() else {
            return;
        };
        if view.rows.is_empty() {
            return;
        }
        let last = view.rows.len() as isize - 1;
        let target = (view.selected_idx() as isize + delta).clamp(0, last);
        self.changes_select(target as usize);
    }

    /// `g`: the first selectable row.
    pub fn changes_select_first(&mut self) {
        self.changes_select(0);
    }

    /// `G`: the last selectable row.
    pub fn changes_select_last(&mut self) {
        let Some(view) = self.changes_view() else {
            return;
        };
        let Some(last) = view.rows.len().checked_sub(1) else {
            return;
        };
        self.changes_select(last);
    }

    /// `}` / `{` from the diff: next / previous entry row.
    pub fn changes_move_entry(&mut self, delta: isize) {
        let Some(view) = self.changes_view() else {
            return;
        };
        let current = view.selected_idx();
        let is_entry = |i: usize| matches!(view.rows[i], ChangesRow::Entry { .. });
        let target = if delta > 0 {
            (current + 1..view.rows.len()).find(|&i| is_entry(i))
        } else {
            (0..current.min(view.rows.len()))
                .rev()
                .find(|&i| is_entry(i))
        };
        if let Some(target) = target {
            self.changes_select(target);
        }
    }

    /// The cursor landed on a row: an entry is previewed; any other row
    /// leaves the diff as it is.
    fn changes_follow_selection(&mut self) {
        if let Some((key, entry)) = self.changes_selected_entry() {
            self.changes_show_entry(key, entry);
        }
    }

    /// Previews `entry`: the cached diff when there is one, else a fresh
    /// read. A read error keeps the diff on screen.
    fn changes_show_entry(&mut self, key: ChangeDiffKey, entry: ChangeEntry) {
        let already = matches!(&self.diff_source, DiffSource::Changes(Some(current)) if *current == key)
            && self
                .changes
                .as_ref()
                .is_some_and(|view| view.selected.as_ref() == Some(&key));
        if already {
            return;
        }
        let cached = self
            .changes
            .as_ref()
            .and_then(|view| view.diff_cache.get(&key).cloned());
        let files = match cached {
            Some(files) => files,
            None => {
                let highlighter = self.theme.syntax_highlighter();
                match self.vcs.get_change_entry_diff(&key, &entry, highlighter) {
                    Ok(files) => Self::filter_ignored_diff_files(&self.vcs_info.root_path, files),
                    Err(TuicrError::NoChanges) => Vec::new(),
                    Err(e) => {
                        self.set_error(format!(
                            "Failed to load the diff of {}: {e}",
                            key.display_path().display()
                        ));
                        return;
                    }
                }
            }
        };
        self.install_preview(key, files);
    }

    /// Puts one entry's diff in the pane. Registers the file with the
    /// session through `ensure_file` (insert-only), never `add_diff_file`:
    /// the Staged and Unstaged previews of one path hash differently, and
    /// invalidating would flip `reviewed` on every cursor move. Does not go
    /// through `apply_diff_files` either, which would `jump_to_file` and
    /// rewrite `file_list_state`.
    fn install_preview(&mut self, key: ChangeDiffKey, files: Vec<DiffFile>) {
        for file in &files {
            self.session.ensure_file(file);
        }
        if let Some(view) = self.changes.as_mut() {
            view.selected = Some(key.clone());
            view.diff_cache.insert(key.clone(), files.clone());
        }
        self.diff_source = DiffSource::Changes(Some(key));
        self.diff_files = files;
        self.clear_expanded_gaps();
        self.populate_file_line_count_cache();
        // Cursor to the top of the one-file preview; viewport size and the
        // wrap setting are the user's, not the preview's.
        self.diff_state.cursor_line = 0;
        self.diff_state.scroll_offset = 0;
        self.diff_state.scroll_x = 0;
        self.diff_state.current_file_idx = 0;
        self.rebuild_annotations();
    }

    /// Nothing to preview (clean tree, or the previewed entry is gone).
    fn changes_clear_preview(&mut self) {
        if let Some(view) = self.changes.as_mut() {
            view.selected = None;
        }
        self.diff_source = DiffSource::Changes(None);
        self.diff_files.clear();
        self.clear_expanded_gaps();
        self.diff_state.cursor_line = 0;
        self.diff_state.scroll_offset = 0;
        self.diff_state.current_file_idx = 0;
        self.rebuild_annotations();
    }

    /// Watch landing: the previewed entry moved to another group. The diff
    /// half of this tick was read for the old key, so the stale check drops
    /// it and the next tick fetches the new one; until then the pane shows
    /// the cached diff when there is one, else nothing.
    fn changes_repoint_preview(&mut self, key: ChangeDiffKey) {
        if matches!(&self.diff_source, DiffSource::Changes(Some(current)) if *current == key) {
            if let Some(view) = self.changes.as_mut() {
                view.selected = Some(key);
            }
            return;
        }
        let cached = self
            .changes
            .as_ref()
            .and_then(|view| view.diff_cache.get(&key).cloned());
        match cached {
            Some(files) => self.install_preview(key, files),
            None => {
                self.changes_clear_preview();
                if let Some(view) = self.changes.as_mut() {
                    view.selected = Some(key.clone());
                }
                self.diff_source = DiffSource::Changes(Some(key));
            }
        }
    }

    /// Rows were rebuilt under the cursor (collapse, grouping, refresh):
    /// keep the same row when it still exists (an entry by key, following
    /// a stage / unstage into its new group; a group or directory by
    /// identity), else the nearest entry row; then show the previewed entry
    /// again, or nothing when it is gone.
    fn changes_reselect(
        &mut self,
        prev_row: Option<ChangesRow>,
        prev_idx: usize,
        prev_key: Option<ChangeDiffKey>,
        refetch: bool,
    ) {
        let Some(view) = self.changes.as_mut() else {
            return;
        };
        let rows = &view.rows;
        let idx = match &prev_row {
            Some(ChangesRow::Entry { key, .. }) => relocate_key(view, key)
                .and_then(|moved| rows.iter().position(|row| row.entry_key() == Some(&moved))),
            Some(other) => rows.iter().position(|row| same_row(row, other)),
            None => None,
        };
        let idx = idx.or_else(|| {
            let n = rows.len();
            let is_entry = |i: usize| matches!(rows[i], ChangesRow::Entry { .. });
            (prev_idx.min(n.saturating_sub(1))..n)
                .find(|&i| is_entry(i))
                .or_else(|| (0..prev_idx.min(n)).rev().find(|&i| is_entry(i)))
                .or_else(|| Self::selectable_near(rows, prev_idx, false))
        });
        view.select(idx.unwrap_or(0));

        let show = match self.changes_selected_entry() {
            Some(shown) => Some(shown),
            None => {
                let view = self.changes.as_ref().expect("view checked above");
                prev_key
                    .as_ref()
                    .and_then(|key| relocate_key(view, key))
                    .and_then(|key| entry_for(view, &key).map(|entry| (key, entry)))
            }
        };
        match show {
            Some((key, entry)) if refetch => self.changes_show_entry(key, entry),
            Some((key, _)) => self.changes_repoint_preview(key),
            None => self.changes_clear_preview(),
        }
    }

    /// Rows changed shape but the data did not (collapse, expand, grouping).
    fn changes_rebuild_keep_selection(&mut self) {
        let Some(view) = self.changes.as_mut() else {
            return;
        };
        let prev_row = view.selected_row().cloned();
        let prev_idx = view.selected_idx();
        let prev_key = view.selected.clone();
        view.rebuild_rows();
        self.changes_reselect(prev_row, prev_idx, prev_key, true);
    }

    // ---- Staging ------------------------------------------------------------

    /// Space / Enter: the entry's checkbox, the group's checkbox, a
    /// directory's fold, or the hint on an uninitialized submodule.
    pub fn changes_toggle_stage_at_cursor(&mut self) {
        let Some(row) = self.changes_selected_row().cloned() else {
            return;
        };
        match row {
            ChangesRow::Entry { key, entry, .. } => match key.group {
                ChangeGroup::Staged => self.changes_unstage_entry(key, entry),
                ChangeGroup::Unstaged | ChangeGroup::Untracked => {
                    self.changes_stage_entry(key, entry)
                }
                ChangeGroup::Conflicted => self.changes_open_merge_view(),
            },
            ChangesRow::Group {
                group, submodule, ..
            } => self.changes_group_checkbox(group, submodule),
            ChangesRow::Directory { .. } => self.changes_toggle_collapse_at_cursor(),
            ChangesRow::Uninitialized {
                path, submodule, ..
            } => self.changes_uninitialized_hint(&path, submodule.as_deref()),
            ChangesRow::Note { .. } => {}
        }
    }

    /// `s`: stage the entry (a conflicted entry is marked resolved; the
    /// backend refuses while markers remain). On a group row the key keeps
    /// its verb: the Unstaged and Unversioned groups are staged whole, the
    /// Staged group is left alone (Space is the toggle).
    pub fn changes_stage_at_cursor(&mut self) {
        let Some(row) = self.changes_selected_row().cloned() else {
            return;
        };
        match row {
            ChangesRow::Entry { key, entry, .. } => self.changes_stage_rules(key, entry),
            ChangesRow::Group {
                group, submodule, ..
            } => match group {
                ChangeGroup::Unstaged | ChangeGroup::Untracked => {
                    self.changes_group_checkbox(group, submodule)
                }
                ChangeGroup::Staged => {
                    self.set_message("Everything in this group is staged already")
                }
                ChangeGroup::Conflicted => self.set_message("Resolve conflicts with :merge"),
            },
            ChangesRow::Directory { .. } => self.changes_toggle_collapse_at_cursor(),
            ChangesRow::Uninitialized {
                path, submodule, ..
            } => self.changes_uninitialized_hint(&path, submodule.as_deref()),
            ChangesRow::Note { .. } => {}
        }
    }

    /// `u`: unstage the entry. On a group row the key keeps its verb: the
    /// Staged group is unstaged whole, the others have nothing staged.
    pub fn changes_unstage_at_cursor(&mut self) {
        let Some(row) = self.changes_selected_row().cloned() else {
            return;
        };
        match row {
            ChangesRow::Entry { key, entry, .. } => self.changes_unstage_rules(key, entry),
            ChangesRow::Group {
                group, submodule, ..
            } => match group {
                ChangeGroup::Staged => self.changes_group_checkbox(group, submodule),
                ChangeGroup::Conflicted => self.set_message("Resolve conflicts with :merge"),
                ChangeGroup::Unstaged | ChangeGroup::Untracked => self.set_message(format!(
                    "Nothing in {} is staged; Space or s stages the group",
                    group.label()
                )),
            },
            ChangesRow::Directory { .. } => self.changes_toggle_collapse_at_cursor(),
            ChangesRow::Uninitialized {
                path, submodule, ..
            } => self.changes_uninitialized_hint(&path, submodule.as_deref()),
            ChangesRow::Note { .. } => {}
        }
    }

    /// `s` issued from the diff pane: stage the previewed entry.
    pub fn changes_stage_previewed(&mut self) {
        let Some((key, entry)) = self.changes_previewed_entry() else {
            self.set_warning("Nothing is previewed; move onto an entry in the Changes pane");
            return;
        };
        self.changes_stage_rules(key, entry);
    }

    /// `u` issued from the diff pane: unstage the previewed entry.
    pub fn changes_unstage_previewed(&mut self) {
        let Some((key, entry)) = self.changes_previewed_entry() else {
            self.set_warning("Nothing is previewed; move onto an entry in the Changes pane");
            return;
        };
        self.changes_unstage_rules(key, entry);
    }

    /// `:discard` issued from the diff pane: ask about the previewed entry.
    pub fn changes_request_discard_previewed(&mut self) {
        let Some((key, entry)) = self.changes_previewed_entry() else {
            self.set_warning("Nothing is previewed; move onto an entry in the Changes pane");
            return;
        };
        self.changes_request_discard_entry(key, entry);
    }

    fn changes_stage_rules(&mut self, key: ChangeDiffKey, entry: ChangeEntry) {
        if key.group == ChangeGroup::Staged {
            self.set_message(format!(
                "{} is staged already",
                key.display_path().display()
            ));
            return;
        }
        self.changes_stage_entry(key, entry);
    }

    fn changes_unstage_rules(&mut self, key: ChangeDiffKey, entry: ChangeEntry) {
        let shown = key.display_path();
        match key.group {
            ChangeGroup::Staged => self.changes_unstage_entry(key, entry),
            ChangeGroup::Conflicted => {
                self.set_message(format!("Resolve {} first (:merge)", shown.display()))
            }
            ChangeGroup::Untracked => {
                self.set_message(format!("{} is not tracked", shown.display()))
            }
            // The same path may be staged as well; unstage that side.
            ChangeGroup::Unstaged => {
                let staged = self
                    .changes_set_for(key.submodule.as_deref())
                    .and_then(|set| set.find(ChangeGroup::Staged, &key.path).cloned());
                match staged {
                    Some(staged) => {
                        let key = ChangeDiffKey {
                            group: ChangeGroup::Staged,
                            ..key
                        };
                        self.changes_unstage_entry(key, staged);
                    }
                    None => self.set_message(format!("Nothing staged for {}", shown.display())),
                }
            }
        }
    }

    fn changes_stage_entry(&mut self, key: ChangeDiffKey, entry: ChangeEntry) {
        let shown = key.display_path();
        let shown = shown.display();
        if entry.is_embedded_repo() {
            self.set_warning(format!(
                "{shown} is a git repository; add it with git submodule add, or remove its .git"
            ));
            return;
        }
        // A probe that failed leaves the kind unknown; let the backend decide.
        if let Some(sm) = entry.submodule()
            && sm.probe_error.is_none()
            && !entry.stageable(key.group)
        {
            self.set_warning(format!(
                "{shown} has uncommitted changes inside it; commit there first (l expands it)"
            ));
            return;
        }
        match self.vcs.stage_change_entry(&key, &entry) {
            Ok(()) => self.after_local_change(format!("Staged {shown}")),
            Err(e) => self.set_error(format!("Failed to stage {shown}: {e}")),
        }
    }

    fn changes_unstage_entry(&mut self, key: ChangeDiffKey, entry: ChangeEntry) {
        let shown = key.display_path();
        let shown = shown.display();
        if entry.is_embedded_repo() {
            self.set_warning(format!("{shown} is a git repository; it is not staged"));
            return;
        }
        match self.vcs.unstage_change_entry(&key, &entry) {
            Ok(()) => self.after_local_change(format!("Unstaged {shown}")),
            Err(e) => self.set_error(format!("Failed to unstage {shown}: {e}")),
        }
    }

    /// The group row's checkbox: stage or unstage everything in it, or open
    /// the merge view for conflicts. A nested group runs in its submodule.
    fn changes_group_checkbox(&mut self, group: ChangeGroup, submodule: Option<PathBuf>) {
        match group.bulk_op() {
            Some(op) => self.changes_bulk_in(op, submodule),
            None => self.changes_open_merge_view(),
        }
    }

    fn changes_open_merge_view(&mut self) {
        if let Err(e) = self.open_merge_view(InputMode::Normal) {
            self.set_error(format!("Failed to open merge view: {e}"));
        }
    }

    /// `a` / `A` / `U`: whole-repository bulk operations.
    pub fn changes_bulk(&mut self, op: ChangeBulkOp) {
        self.changes_bulk_in(op, None);
    }

    fn changes_bulk_in(&mut self, op: ChangeBulkOp, submodule: Option<PathBuf>) {
        // `A` leaves embedded repositories alone; name them, as the docs say.
        let skipped: Vec<String> = match op {
            ChangeBulkOp::StageAll | ChangeBulkOp::StageUntracked => self
                .changes_set_for(submodule.as_deref())
                .map(|set| {
                    set.untracked
                        .iter()
                        .filter(|entry| entry.is_embedded_repo())
                        .map(|entry| entry.path.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        match self.vcs.change_bulk_op(op, submodule.as_deref()) {
            Ok(()) => {
                let mut message = match &submodule {
                    Some(sub) => format!("{} in {}", op.done_message(), sub.display()),
                    None => op.done_message().to_string(),
                };
                if !skipped.is_empty() {
                    message.push_str(&format!(
                        "; skipped embedded repo{} {}",
                        if skipped.len() == 1 { "" } else { "s" },
                        skipped.join(", ")
                    ));
                }
                self.after_local_change(message);
            }
            Err(e) => {
                let what = match op {
                    ChangeBulkOp::StageTracked => "Staging tracked changes",
                    ChangeBulkOp::StageUntracked => "Staging untracked files",
                    ChangeBulkOp::StageAll => "Staging everything",
                    ChangeBulkOp::UnstageAll => "Unstaging everything",
                };
                self.set_error(format!("{what} failed: {e}"));
            }
        }
    }

    fn changes_uninitialized_hint(&mut self, path: &Path, submodule: Option<&Path>) {
        let command = match submodule {
            Some(sub) => format!(
                "git -C {} submodule update --init -- {}",
                sub.display(),
                path.display()
            ),
            None => format!("git submodule update --init -- {}", path.display()),
        };
        self.set_message(format!("Not checked out · {command}"));
    }

    // ---- Discard --------------------------------------------------------------

    /// `X`: ask before discarding the entry or the whole group under the
    /// cursor (the prompts are `DiscardTarget::prompt`).
    pub fn changes_request_discard_at_cursor(&mut self) {
        let Some(row) = self.changes_selected_row().cloned() else {
            return;
        };
        match row {
            ChangesRow::Entry { key, entry, .. } => self.changes_request_discard_entry(key, entry),
            ChangesRow::Group {
                group, submodule, ..
            } => self.changes_request_discard_group(group, submodule),
            ChangesRow::Directory { .. } | ChangesRow::Uninitialized { .. } => {
                self.set_message("Move the cursor onto an entry or a group row to discard")
            }
            ChangesRow::Note { .. } => {}
        }
    }

    fn changes_request_discard_entry(&mut self, key: ChangeDiffKey, entry: ChangeEntry) {
        let shown = key.display_path();
        let shown = shown.display();
        if entry.is_embedded_repo() {
            self.set_message(format!("{shown} is a git repository; delete it by hand"));
            return;
        }
        if key.group == ChangeGroup::Conflicted || entry.status == ChangeStatus::Conflicted {
            self.set_message(format!("Resolve {shown} first (:merge)"));
            return;
        }
        if let Some(sm) = entry.submodule()
            && entry.status == ChangeStatus::Deleted
            && !sm.registered
        {
            self.set_message(format!(
                "{shown} was removed and is not in .gitmodules; restore it by hand"
            ));
            return;
        }
        self.pending_discard = Some(DiscardTarget::Entry { key, entry });
        self.enter_confirm_mode(ConfirmAction::DiscardChanges);
    }

    fn changes_request_discard_group(&mut self, group: ChangeGroup, submodule: Option<PathBuf>) {
        if group == ChangeGroup::Conflicted {
            self.set_message("Resolve conflicts with :merge");
            return;
        }
        let Some(entries) = self
            .changes_set_for(submodule.as_deref())
            .map(|set| set.group(group).to_vec())
        else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        self.pending_discard = Some(DiscardTarget::Group {
            group,
            submodule,
            entries,
        });
        self.enter_confirm_mode(ConfirmAction::DiscardChanges);
    }

    // ---- Expand / collapse ----------------------------------------------------

    /// `l` / `→`: expand the group, directory or dirty submodule under the
    /// cursor; pan right on anything else.
    pub fn changes_expand_at_cursor(&mut self) {
        let Some(row) = self.changes_selected_row().cloned() else {
            return;
        };
        match row {
            ChangesRow::Group {
                submodule,
                group,
                expanded,
                ..
            } => {
                if !expanded && let Some(view) = self.changes.as_mut() {
                    view.collapsed_groups.remove(&(submodule, group));
                    self.changes_rebuild_keep_selection();
                }
            }
            ChangesRow::Directory {
                submodule,
                group,
                path,
                expanded,
                ..
            } => {
                if !expanded && let Some(view) = self.changes.as_mut() {
                    view.collapsed_dirs.remove(&(submodule, group, path));
                    self.changes_rebuild_keep_selection();
                }
            }
            ChangesRow::Entry {
                key,
                expanded: Some(false),
                ..
            } => self.changes_load_submodule(key.display_path()),
            ChangesRow::Entry {
                expanded: Some(true),
                ..
            } => {}
            ChangesRow::Entry { .. } | ChangesRow::Uninitialized { .. } => {
                self.changes_scroll_right(4)
            }
            ChangesRow::Note { .. } => {}
        }
    }

    /// Reads a dirty submodule's own change set and nests it under its row.
    fn changes_load_submodule(&mut self, full: PathBuf) {
        let expansion = match self.vcs.get_submodule_change_set(&full) {
            Ok(inner) => SubmoduleExpansion::Loaded(self.filter_change_set(inner, Some(&full))),
            Err(e) => SubmoduleExpansion::Failed(e.to_string()),
        };
        if let Some(view) = self.changes.as_mut() {
            view.submodules.insert(full, expansion);
        }
        self.changes_rebuild_keep_selection();
    }

    /// `h` / `←`: collapse the row under the cursor (an expanded submodule
    /// drops its inner set, so `r` does not re-read it), or move to the
    /// parent row when it is already collapsed; pan left on a panned entry.
    pub fn changes_collapse_at_cursor(&mut self) {
        let Some(row) = self.changes_selected_row().cloned() else {
            return;
        };
        let scroll_x = self.changes.as_ref().map_or(0, |view| view.scroll_x);
        match row {
            ChangesRow::Group {
                submodule,
                group,
                expanded: true,
                ..
            } => {
                if let Some(view) = self.changes.as_mut() {
                    view.collapsed_groups.insert((submodule, group));
                }
                self.changes_rebuild_keep_selection();
            }
            ChangesRow::Directory {
                submodule,
                group,
                path,
                expanded: true,
                ..
            } => {
                if let Some(view) = self.changes.as_mut() {
                    view.collapsed_dirs.insert((submodule, group, path));
                }
                self.changes_rebuild_keep_selection();
            }
            ChangesRow::Entry {
                key,
                expanded: Some(true),
                ..
            } => {
                let full = key.display_path();
                if let Some(view) = self.changes.as_mut() {
                    view.submodules.remove(&full);
                    view.diff_cache
                        .retain(|cached, _| cached.submodule.as_deref() != Some(full.as_path()));
                }
                // The preview may have been an inner entry; it is gone now.
                if matches!(&self.diff_source, DiffSource::Changes(Some(k)) if k.submodule.as_deref() == Some(full.as_path()))
                {
                    self.changes_clear_preview();
                }
                self.changes_rebuild_keep_selection();
            }
            ChangesRow::Entry { expanded: None, .. } if scroll_x > 0 => self.changes_scroll_left(4),
            _ => self.changes_select_parent(),
        }
    }

    /// The nearest selectable row above with a smaller depth.
    fn changes_select_parent(&mut self) {
        let Some(view) = self.changes_view() else {
            return;
        };
        let idx = view.selected_idx();
        let depth = view.rows.get(idx).map_or(0, ChangesRow::depth);
        let parent = (0..idx.min(view.rows.len()))
            .rev()
            .find(|&i| view.rows[i].depth() < depth && view.rows[i].selectable());
        if let Some(parent) = parent {
            self.changes_select(parent);
        }
    }

    /// Mouse on a group / directory row, or Space on a directory: fold it
    /// the other way.
    fn changes_toggle_collapse_at_cursor(&mut self) {
        let expanded = match self.changes_selected_row() {
            Some(ChangesRow::Group { expanded, .. })
            | Some(ChangesRow::Directory { expanded, .. }) => *expanded,
            Some(ChangesRow::Entry {
                expanded: Some(expanded),
                ..
            }) => *expanded,
            _ => return,
        };
        if expanded {
            self.changes_collapse_at_cursor();
        } else {
            self.changes_expand_at_cursor();
        }
    }

    /// `o`: expand every group and directory (submodule expansions untouched).
    pub fn changes_expand_all(&mut self) {
        let Some(view) = self.changes_view_mut() else {
            return;
        };
        view.collapsed_groups.clear();
        view.collapsed_dirs.clear();
        self.changes_rebuild_keep_selection();
        self.set_message("All groups expanded");
    }

    /// `O`: collapse every group and directory on screen.
    pub fn changes_collapse_all(&mut self) {
        let Some(view) = self.changes_view_mut() else {
            return;
        };
        let ChangesView {
            rows,
            collapsed_groups,
            collapsed_dirs,
            ..
        } = view;
        for row in rows.iter() {
            match row {
                ChangesRow::Group {
                    submodule, group, ..
                } => {
                    collapsed_groups.insert((submodule.clone(), *group));
                }
                ChangesRow::Directory {
                    submodule,
                    group,
                    path,
                    ..
                } => {
                    collapsed_dirs.insert((submodule.clone(), *group, path.clone()));
                }
                _ => {}
            }
        }
        self.changes_rebuild_keep_selection();
        self.set_message("All groups collapsed");
    }

    /// `f`: flat list ↔ group by directory.
    pub fn changes_toggle_grouping(&mut self) {
        let Some(view) = self.changes_view_mut() else {
            return;
        };
        view.group_by_directory = !view.group_by_directory;
        let grouped = view.group_by_directory;
        self.changes_rebuild_keep_selection();
        self.set_message(if grouped {
            "Changes grouped by directory"
        } else {
            "Changes as a flat list"
        });
    }

    // ---- Scrolling and mouse --------------------------------------------------

    /// `←` / `h` on a panned entry: pan the rows back left.
    pub fn changes_scroll_left(&mut self, cols: usize) {
        if let Some(view) = self.changes_view_mut() {
            view.scroll_x = view.scroll_x.saturating_sub(cols);
        }
    }

    /// `→` / `l` on an entry that cannot expand: pan the rows right, as far
    /// as the widest row (measured by the renderer) allows.
    pub fn changes_scroll_right(&mut self, cols: usize) {
        if let Some(view) = self.changes_view_mut() {
            let max = view.max_content_width.saturating_sub(view.viewport_width);
            view.scroll_x = view.scroll_x.saturating_add(cols).min(max);
        }
    }

    /// Wheel: move the viewport, keeping the selection inside it (mirrors
    /// `file_list_viewport_scroll_*`).
    pub fn changes_viewport_scroll(&mut self, delta: isize) {
        let Some(view) = self.changes_view_mut() else {
            return;
        };
        let viewport = view.viewport_height.max(1);
        let max_offset = view.rows.len().saturating_sub(viewport);
        let offset = view.list.offset();
        let new_offset = if delta >= 0 {
            offset.saturating_add(delta as usize).min(max_offset)
        } else {
            offset.saturating_sub(delta.unsigned_abs())
        };
        *view.list.offset_mut() = new_offset;
        let selected = view.selected_idx();
        let last_visible = (new_offset + viewport).saturating_sub(1);
        let target = if selected < new_offset {
            Some(new_offset)
        } else if selected > last_visible {
            Some(last_visible)
        } else {
            None
        };
        if let Some(target) = target {
            self.changes_select(target);
        }
    }

    /// Mouse: select `idx`; `col` is the column inside the pane (after
    /// adding `scroll_x`) so the expander / checkbox cells can be told apart.
    pub fn changes_click(&mut self, idx: usize, col: usize) {
        let Some(row) = self.changes_rows().get(idx).cloned() else {
            return;
        };
        if !row.selectable() {
            return;
        }
        self.changes_select(idx);
        // The renderer pans everything but the indent, so while panned the
        // indent cells sit over content that was never on screen: a click
        // there only selects.
        let scroll_x = self.changes.as_ref().map_or(0, |view| view.scroll_x);
        if scroll_x > 0 && col.saturating_sub(scroll_x) < row.depth() * CHANGES_INDENT {
            return;
        }
        match &row {
            ChangesRow::Group { .. } | ChangesRow::Directory { .. } => {
                self.changes_toggle_collapse_at_cursor()
            }
            ChangesRow::Entry {
                expanded: Some(_), ..
            } if row.expander_cell() == Some(col) => self.changes_toggle_collapse_at_cursor(),
            ChangesRow::Entry { .. } if col < row.glyph_cell() => {
                self.changes_toggle_stage_at_cursor()
            }
            _ => {}
        }
    }

    // ---- Refresh and the diff watch -------------------------------------------

    /// `r`, `:e`, and every local-change operation: re-read the change set,
    /// drop cached diffs, keep the selection, re-show its diff.
    pub fn refresh_changes_view(&mut self, message: Option<String>) {
        match self.refresh_changes_view_inner() {
            Ok(true) => {
                if let Some(message) = message {
                    self.set_message(message);
                }
            }
            Ok(false) => {}
            Err(e) => self.set_error(format!("Failed to read the change set: {e}")),
        }
    }

    /// `r`: refresh and say what the tree holds.
    pub fn changes_refresh(&mut self) {
        match self.refresh_changes_view_inner() {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                self.set_error(format!("Failed to read the change set: {e}"));
                return;
            }
        }
        let Some(view) = self.changes_view() else {
            return;
        };
        let set = &view.set;
        let summary = format!(
            "Refreshed · {} staged · {} unstaged · {} unversioned",
            set.staged.len(),
            set.unstaged.len(),
            set.untracked.len()
        );
        self.set_message(summary);
    }

    /// `Ok(true)` when the set was re-read and installed, `Ok(false)` when
    /// the view is not active. A set that cannot be read is the caller's
    /// error to show, so a success message never papers over a stale tree.
    pub(in crate::app) fn refresh_changes_view_inner(&mut self) -> Result<bool, TuicrError> {
        if !self.changes_active() {
            return Ok(false);
        }
        let set = self.filter_change_set(self.vcs.get_change_set()?, None);
        // Expanded submodules: re-read the ones that are still dirty, drop
        // the rest and every failed expansion.
        let expanded: Vec<PathBuf> = self
            .changes
            .as_ref()
            .map(|view| {
                let mut paths: Vec<PathBuf> = view
                    .submodules
                    .iter()
                    .filter(|(_, expansion)| matches!(expansion, SubmoduleExpansion::Loaded(_)))
                    .map(|(path, _)| path.clone())
                    .collect();
                paths.sort_by_key(|path| path.components().count());
                paths
            })
            .unwrap_or_default();
        let mut submodules: HashMap<PathBuf, SubmoduleExpansion> = HashMap::new();
        for full in expanded {
            if !expandable_in(&set, &submodules, &full) {
                continue;
            }
            let expansion = match self.vcs.get_submodule_change_set(&full) {
                Ok(inner) => SubmoduleExpansion::Loaded(self.filter_change_set(inner, Some(&full))),
                Err(e) => SubmoduleExpansion::Failed(e.to_string()),
            };
            submodules.insert(full, expansion);
        }

        let Some(view) = self.changes.as_mut() else {
            return Ok(false);
        };
        let prev_row = view.selected_row().cloned();
        let prev_idx = view.selected_idx();
        // Cleared so the reselect fetches afresh instead of short-circuiting
        // on "already shown".
        let prev_key = view.selected.take();
        view.diff_cache.clear();
        view.fingerprint = set.fingerprint();
        view.set = set;
        view.submodules = submodules;
        view.rebuild_rows();
        self.changes_reselect(prev_row, prev_idx, prev_key, true);
        self.refresh_log_rows_keep_cursor();
        self.refresh_repo_status();
        Ok(true)
    }

    /// Diff watch landing: install a fresher change set. `true` when the
    /// screen changed. The diff half of the same tick follows separately;
    /// nothing is re-read here.
    pub(in crate::app) fn changes_apply_watched_set(&mut self, set: ChangeSet) -> bool {
        if !self.changes_active() {
            return false;
        }
        let set = self.filter_change_set(set, None);
        let fingerprint = set.fingerprint();
        let Some(view) = self.changes.as_mut() else {
            return false;
        };
        if view.fingerprint == fingerprint {
            return false;
        }
        let prev_row = view.selected_row().cloned();
        let prev_idx = view.selected_idx();
        let prev_key = view.selected.clone();
        // Keep the expansions whose submodule is still dirty; their inner
        // sets are re-read by `r` or the next operation.
        let keep: Vec<PathBuf> = view
            .submodules
            .keys()
            .filter(|path| expandable_in(&set, &view.submodules, path))
            .cloned()
            .collect();
        view.submodules.retain(|path, _| keep.contains(path));
        view.fingerprint = fingerprint;
        view.set = set;
        view.rebuild_rows();
        self.changes_reselect(prev_row, prev_idx, prev_key, false);
        self.refresh_log_rows_keep_cursor();
        true
    }

    /// Diff watch landing for the previewed entry: install it without
    /// moving the cursor. `true` when the screen changed.
    pub(in crate::app) fn changes_apply_watched_diff(&mut self, files: Vec<DiffFile>) -> bool {
        let DiffSource::Changes(Some(key)) = &self.diff_source else {
            return false;
        };
        let key = key.clone();
        if diff_files_fingerprint(&files) == diff_files_fingerprint(&self.diff_files) {
            return false;
        }
        let cursor = self.diff_state.cursor_line;
        let offset = self.diff_state.scroll_offset;
        self.install_preview(key, files);
        self.diff_state.cursor_line = cursor.min(self.total_lines().saturating_sub(1));
        self.diff_state.scroll_offset = offset.min(self.max_scroll_offset());
        self.ensure_cursor_visible();
        self.update_current_file_from_cursor();
        true
    }

    // ---- Editor and commit ----------------------------------------------------

    /// `e` / `:edit` in the pane: `$EDITOR` on the previewed entry.
    pub fn changes_edit_previewed(&mut self) {
        if self.diff_files.is_empty() {
            self.set_warning("Select an entry in the Changes pane to open it in the editor");
            return;
        }
        self.queue_editor_for_file_idx(0, None);
    }

    /// `c` / `C`: the comment box becomes the commit message editor. The
    /// commit is the superproject's even with the cursor inside an expanded
    /// submodule, and the prompt says so.
    pub fn changes_start_commit_composer(&mut self, amend: bool) {
        let inside = match self.changes_selected_row() {
            Some(ChangesRow::Entry { key, .. }) => key.submodule.clone(),
            Some(ChangesRow::Group { submodule, .. })
            | Some(ChangesRow::Directory { submodule, .. })
            | Some(ChangesRow::Uninitialized { submodule, .. }) => submodule.clone(),
            _ => None,
        };
        self.start_commit_composer(amend);
        if self.input_mode == InputMode::Comment
            && let Some(sub) = inside
        {
            let what = if amend { "Amend" } else { "Commit" };
            self.set_message(format!(
                "{what} the superproject, not {}: type the message, then Enter / Ctrl-s to commit, Esc to cancel",
                sub.display()
            ));
        }
    }
}

/// Rows with the same identity, ignoring counts and fold state.
fn same_row(a: &ChangesRow, b: &ChangesRow) -> bool {
    match (a, b) {
        (
            ChangesRow::Group {
                submodule: sa,
                group: ga,
                ..
            },
            ChangesRow::Group {
                submodule: sb,
                group: gb,
                ..
            },
        ) => sa == sb && ga == gb,
        (
            ChangesRow::Directory {
                submodule: sa,
                group: ga,
                path: pa,
                ..
            },
            ChangesRow::Directory {
                submodule: sb,
                group: gb,
                path: pb,
                ..
            },
        ) => sa == sb && ga == gb && pa == pb,
        (ChangesRow::Entry { key: ka, .. }, ChangesRow::Entry { key: kb, .. }) => ka == kb,
        (
            ChangesRow::Uninitialized {
                submodule: sa,
                path: pa,
                ..
            },
            ChangesRow::Uninitialized {
                submodule: sb,
                path: pb,
                ..
            },
        ) => sa == sb && pa == pb,
        _ => false,
    }
}

/// Where `key`'s entry is now: the same group, or the group a stage /
/// unstage moved it to. `None` when it is gone (or its submodule is no
/// longer expanded).
fn relocate_key(view: &ChangesView, key: &ChangeDiffKey) -> Option<ChangeDiffKey> {
    let set = match &key.submodule {
        None => &view.set,
        Some(sub) => match view.submodules.get(sub) {
            Some(SubmoduleExpansion::Loaded(inner)) => inner,
            _ => return None,
        },
    };
    let group = set.group_of(&key.path, key.group)?;
    Some(ChangeDiffKey {
        group,
        path: key.path.clone(),
        submodule: key.submodule.clone(),
    })
}

fn entry_for(view: &ChangesView, key: &ChangeDiffKey) -> Option<ChangeEntry> {
    let set = match &key.submodule {
        None => &view.set,
        Some(sub) => match view.submodules.get(sub) {
            Some(SubmoduleExpansion::Loaded(inner)) => inner,
            _ => return None,
        },
    };
    set.find(key.group, &key.path).cloned()
}

/// Whether `full` (superproject-relative) still names a dirty submodule
/// entry: in `top`, or in the loaded set of the nearest expanded ancestor.
fn expandable_in(
    top: &ChangeSet,
    submodules: &HashMap<PathBuf, SubmoduleExpansion>,
    full: &Path,
) -> bool {
    let mut owner: Option<(&Path, &ChangeSet)> = None;
    for (path, expansion) in submodules {
        if let SubmoduleExpansion::Loaded(inner) = expansion
            && path.as_path() != full
            && full.starts_with(path)
            && owner.is_none_or(|(best, _)| path.components().count() > best.components().count())
        {
            owner = Some((path.as_path(), inner));
        }
    }
    let (set, relative) = match owner {
        Some((path, inner)) => (inner, full.strip_prefix(path).unwrap_or(full)),
        None => (top, full),
    };
    [ChangeGroup::Staged, ChangeGroup::Unstaged]
        .iter()
        .any(|group| {
            set.find(*group, relative)
                .is_some_and(ChangeEntry::expandable)
        })
}
