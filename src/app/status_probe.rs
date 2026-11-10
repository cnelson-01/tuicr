//! Working-tree status for the selector, without the wait.
//!
//! The selector's "Unstaged changes" / "Staged changes" rows come from a
//! `git status`, which takes about 100 ms on a 20k-file tree and used to
//! run before the selector could open. With background probing on, the
//! selector opens on the status tuicr last saw and a worker reads it again;
//! when the answer differs, the two rows are fixed in place under the
//! cursor.

use std::sync::mpsc::TryRecvError;

use super::{App, InputMode, STAGED_SELECTION_ID, TargetTab, UNSTAGED_SELECTION_ID};
use crate::vcs::detect_vcs;
use crate::vcs::traits::{VcsChangeStatus, VcsType};

impl App {
    /// A status just read from the repository. It supersedes anything a
    /// worker is still fetching, so that is dropped.
    pub(in crate::app) fn remember_change_status(&mut self, status: VcsChangeStatus) {
        self.last_change_status = Some(status);
        self.status_probe_rx = None;
        self.status_probe_pending = None;
    }

    /// The status the selector may open on right away, or `None` when it
    /// has to be read first: background probing is off (tests, anything
    /// without the main loop to poll the worker), nothing has been read
    /// yet, or the answer needs more than `git status` (a path filter or a
    /// `.tuicrignore` means diffing to see what survives).
    pub(in crate::app) fn remembered_change_status(&self) -> Option<VcsChangeStatus> {
        if !self.background_status_probe
            || self.vcs_info.vcs_type != VcsType::Git
            || self.path_filter.is_some()
            || crate::tuicrignore::has_tuicrignore(&self.vcs_info.root_path)
        {
            return None;
        }
        self.last_change_status
    }

    /// Read the status again on a worker thread with its own VCS handle.
    /// A probe already in flight is left to finish.
    pub(in crate::app) fn spawn_status_probe(&mut self) {
        if self.status_probe_rx.is_some() {
            return;
        }
        let options = self.vcs_open_options.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.status_probe_rx = Some(rx);
        self.status_probe_pending = None;
        std::thread::spawn(move || {
            let result = detect_vcs(options.git_backend_preference, options.diff_whitespace_mode)
                .and_then(|vcs| vcs.get_change_status());
            let _ = tx.send(result);
        });
    }

    /// Main-loop tick: take a finished probe and bring the selector's rows
    /// in line with it. Returns whether the screen changed.
    pub fn poll_status_probe(&mut self) -> bool {
        if let Some(rx) = self.status_probe_rx.as_ref() {
            match rx.try_recv() {
                Ok(answer) => {
                    self.status_probe_rx = None;
                    if let Ok(status) = answer {
                        self.last_change_status = Some(status);
                        self.status_probe_pending = Some(status);
                    }
                }
                Err(TryRecvError::Disconnected) => self.status_probe_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        // The answer waits while something sits over the selector (the
        // command line, a picker) and is applied once its rows are back.
        let Some(status) = self.status_probe_pending else {
            return false;
        };
        if self.input_mode != InputMode::CommitSelect || self.target_tab != TargetTab::Local {
            return false;
        }
        self.status_probe_pending = None;
        self.sync_selector_change_rows(status)
    }

    /// Make the selector's unstaged / staged rows match `status`, keeping
    /// the cursor and the picks on the rows they were on. Returns whether
    /// anything changed.
    pub(in crate::app) fn sync_selector_change_rows(&mut self, status: VcsChangeStatus) -> bool {
        let special = self
            .commit_list
            .iter()
            .take_while(|c| Self::is_special_commit(c))
            .count();
        let shows = |id: &str| self.commit_list[..special].iter().any(|c| c.id == id);
        if shows(UNSTAGED_SELECTION_ID) == status.unstaged
            && shows(STAGED_SELECTION_ID) == status.staged
        {
            return false;
        }

        let on_expand_row = self.is_on_expand_row();
        let cursor_id = self
            .commit_list
            .get(self.commit_list_cursor)
            .map(|c| c.id.clone());
        let picked: Vec<String> = self
            .commit_picks
            .iter()
            .filter_map(|&i| self.commit_list.get(i))
            .map(|c| c.id.clone())
            .collect();

        // Same order every loader builds: conflicts, unstaged, staged.
        let mut rows: Vec<_> = self.commit_list[..special]
            .iter()
            .filter(|c| Self::is_merge_conflicts_commit(c))
            .cloned()
            .collect();
        if status.unstaged {
            rows.push(Self::unstaged_commit_entry());
        }
        if status.staged {
            rows.push(Self::staged_commit_entry());
        }
        let now_special = rows.len();
        rows.extend(self.commit_list.drain(special..));
        self.commit_list = rows;
        self.visible_commit_count = (self.visible_commit_count + now_special)
            .saturating_sub(special)
            .min(self.commit_list.len());

        let position = |id: &String| self.commit_list.iter().position(|c| &c.id == id);
        self.commit_list_cursor = if on_expand_row {
            self.visible_commit_count
        } else {
            // A row that went away leaves the cursor at the top.
            cursor_id.as_ref().and_then(position).unwrap_or(0)
        };
        self.commit_picks = picked.iter().filter_map(position).collect();
        self.commit_list_scroll_offset =
            self.commit_list_scroll_offset.min(self.commit_list_cursor);
        true
    }
}
