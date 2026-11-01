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

use super::{App, DiffSource, FocusedPanel};
use crate::model::DiffFile;
use crate::vcs::{
    ChangeBulkOp, ChangeDiffKey, ChangeEntry, ChangeEntryKind, ChangeGroup, ChangeSet,
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

    // ---- Part B replaces every body below -------------------------------

    /// `<leader>C`: hidden → open and focus; open → focus (and re-read);
    /// focused → close.
    pub fn toggle_changes_view(&mut self) {
        self.set_warning("Changes view: not implemented yet");
    }
    /// `:changes` toggles; `:set changes` / `:set nochanges` set.
    pub fn set_changes_visible(&mut self, _visible: bool) {
        self.set_warning("Changes view: not implemented yet");
    }
    /// Opens the view (always `InputMode::Normal`), or the degraded flat
    /// working-tree review on hg / jj.
    pub fn open_changes_view(&mut self) {
        self.set_warning("Changes view: not implemented yet");
    }
    pub fn close_changes_view(&mut self) {}
    /// `r`, `:e`, and every local-change operation: re-read the change set,
    /// drop cached diffs, keep the selection, re-show its diff.
    pub fn refresh_changes_view(&mut self, _message: Option<String>) {}
    pub fn changes_move(&mut self, _delta: isize) {}
    pub fn changes_select(&mut self, _idx: usize) {}
    pub fn changes_select_first(&mut self) {}
    pub fn changes_select_last(&mut self) {}
    /// `}` / `{` from the diff: next / previous entry row.
    pub fn changes_move_entry(&mut self, _delta: isize) {}
    pub fn changes_toggle_stage_at_cursor(&mut self) {}
    pub fn changes_stage_at_cursor(&mut self) {}
    pub fn changes_unstage_at_cursor(&mut self) {}
    /// `s` / `u` / `:discard` issued from the diff pane: act on the
    /// previewed entry.
    pub fn changes_stage_previewed(&mut self) {}
    pub fn changes_unstage_previewed(&mut self) {}
    pub fn changes_request_discard_previewed(&mut self) {}
    pub fn changes_bulk(&mut self, _op: ChangeBulkOp) {}
    pub fn changes_request_discard_at_cursor(&mut self) {}
    pub fn changes_expand_at_cursor(&mut self) {}
    pub fn changes_collapse_at_cursor(&mut self) {}
    pub fn changes_expand_all(&mut self) {}
    pub fn changes_collapse_all(&mut self) {}
    pub fn changes_toggle_grouping(&mut self) {}
    pub fn changes_scroll_left(&mut self, _cols: usize) {}
    pub fn changes_scroll_right(&mut self, _cols: usize) {}
    pub fn changes_viewport_scroll(&mut self, _delta: isize) {}
    /// Mouse: select `idx`; `col` is the column inside the pane (after
    /// adding `scroll_x`) so the expander / checkbox cells can be told apart.
    pub fn changes_click(&mut self, _idx: usize, _col: usize) {}
    /// Diff watch landing: install a fresher change set. `true` when the
    /// screen changed. Not called until the watch forwards a change set
    /// (`poll_diff_watch_reload_events` drops it for now).
    #[allow(dead_code)]
    pub(in crate::app) fn changes_apply_watched_set(&mut self, _set: ChangeSet) -> bool {
        false
    }
}
