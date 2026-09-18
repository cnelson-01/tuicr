//! Log filter: `/` in the log pane or the fullscreen selector opens a
//! one-line prompt whose text becomes a [`CommitQuery`] (free words match
//! the message, `author:` and `path:` narrow further). While a filter is
//! active every history fetch — the initial rows and the overflow row's
//! paging — goes through the VCS search instead of the plain recent list.

use super::{App, InputMode};
use crate::error::Result;
use crate::vcs::{CommitInfo, CommitQuery};

impl App {
    /// Whether the filter prompt is open.
    pub fn commit_filter_editing(&self) -> bool {
        self.commit_filter_draft.is_some()
    }

    /// `/`: open the prompt, pre-filled with the active filter so it can be
    /// edited rather than retyped.
    pub fn begin_commit_filter(&mut self) {
        let current = self
            .commit_filter
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        self.commit_filter_draft = Some(current);
    }

    pub fn commit_filter_insert_char(&mut self, ch: char) {
        if let Some(draft) = self.commit_filter_draft.as_mut() {
            draft.push(ch);
        }
    }

    pub fn commit_filter_delete_char(&mut self) {
        if let Some(draft) = self.commit_filter_draft.as_mut() {
            draft.pop();
        }
    }

    pub fn commit_filter_delete_word(&mut self) {
        if let Some(draft) = self.commit_filter_draft.as_mut() {
            let trimmed = draft.trim_end().len();
            draft.truncate(trimmed);
            let cut = draft.rfind(' ').map(|i| i + 1).unwrap_or(0);
            draft.truncate(cut);
        }
    }

    pub fn commit_filter_clear_draft(&mut self) {
        if let Some(draft) = self.commit_filter_draft.as_mut() {
            draft.clear();
        }
    }

    /// Enter: parse the draft into the active filter (an empty draft clears
    /// it) and reload the rows on screen.
    pub fn apply_commit_filter(&mut self) {
        let Some(draft) = self.commit_filter_draft.take() else {
            return;
        };
        let next = CommitQuery::parse(&draft);
        let changed = next != self.commit_filter;
        self.commit_filter = next;
        if changed {
            self.refresh_filtered_rows();
        }
        match self.commit_filter.as_ref() {
            Some(query) => {
                let label = self.commit_filter_match_label();
                self.set_message(format!("Filter: {query} \u{00b7} {label}"));
            }
            None => self.set_message("Filter cleared"),
        }
    }

    /// Esc while editing: drop the draft, keep the active filter.
    pub fn cancel_commit_filter(&mut self) {
        self.commit_filter_draft = None;
    }

    /// Esc with a filter active: clear it and reload. Returns whether there
    /// was a filter to clear, so callers can fall through to their usual
    /// Esc behavior otherwise.
    pub fn clear_commit_filter(&mut self) -> bool {
        if self.commit_filter.take().is_none() {
            return false;
        }
        self.refresh_filtered_rows();
        self.set_message("Filter cleared");
        true
    }

    fn refresh_filtered_rows(&mut self) {
        if self.input_mode == InputMode::CommitSelect {
            self.refresh_commit_selector_rows();
        } else {
            self.refresh_log_rows_keep_cursor();
        }
    }

    /// History page `offset..offset+limit`, through the search when a
    /// filter is active. Every list that pages history uses this so the
    /// overflow row keeps paging the *filtered* log.
    pub(in crate::app) fn fetch_history(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<CommitInfo>> {
        match self.commit_filter.as_ref() {
            Some(query) => self.vcs.search_commits(query, offset, limit),
            None => self.vcs.get_recent_commits(offset, limit),
        }
    }

    /// The active filter as typed, for titles and footers.
    pub fn commit_filter_summary(&self) -> Option<String> {
        self.commit_filter.as_ref().map(ToString::to_string)
    }

    /// `12 matches` / `200+ matches` for whichever list is on screen: the
    /// log pane's own rows in Normal mode, the selector's otherwise.
    pub fn commit_filter_match_label(&self) -> String {
        let (rows, has_more): (&[CommitInfo], bool) = match self.log_pane.as_ref() {
            Some(pane) if self.input_mode != InputMode::CommitSelect => {
                (&pane.list.commits, pane.list.has_more)
            }
            _ => (&self.commit_list, self.has_more_commit),
        };
        let n = rows.iter().filter(|c| !Self::is_special_commit(c)).count();
        let plus = if has_more { "+" } else { "" };
        let noun = if n == 1 && !has_more {
            "match"
        } else {
            "matches"
        };
        format!("{n}{plus} {noun}")
    }
}
