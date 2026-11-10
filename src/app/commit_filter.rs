//! Log filter: `/` in the log pane or the fullscreen selector opens a
//! one-line prompt whose text becomes a [`CommitQuery`] (free words match
//! the message, `author:` and `path:` narrow further). While a filter is
//! active every history fetch — the initial rows and the overflow row's
//! paging — goes through the VCS search instead of the plain recent list.

use std::time::{Duration, Instant};

use super::{App, CommitFilterAnswer, InputMode, initial_commit_limit};
use crate::error::Result;
use crate::vcs::{CommitInfo, CommitQuery, detect_vcs};

/// How long the prompt must rest before its text is searched.
const LIVE_FILTER_DELAY: Duration = Duration::from_millis(200);

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
            self.schedule_live_filter();
        }
    }

    pub fn commit_filter_delete_char(&mut self) {
        if let Some(draft) = self.commit_filter_draft.as_mut() {
            draft.pop();
            self.schedule_live_filter();
        }
    }

    pub fn commit_filter_delete_word(&mut self) {
        if let Some(draft) = self.commit_filter_draft.as_mut() {
            let trimmed = draft.trim_end().len();
            draft.truncate(trimmed);
            let cut = draft.rfind(' ').map(|i| i + 1).unwrap_or(0);
            draft.truncate(cut);
            self.schedule_live_filter();
        }
    }

    pub fn commit_filter_clear_draft(&mut self) {
        if let Some(draft) = self.commit_filter_draft.as_mut() {
            draft.clear();
            self.schedule_live_filter();
        }
    }

    /// Note a prompt edit; the text is searched once it has rested for
    /// [`LIVE_FILTER_DELAY`], so the rows follow the typing without a
    /// search per keystroke.
    fn schedule_live_filter(&mut self) {
        self.commit_filter_typed_at = Some(Instant::now());
    }

    /// Enter: the draft becomes the active filter (an empty draft clears
    /// it). The rows are already live-filtered, or a search for exactly this
    /// text is in flight; otherwise one is started now.
    pub fn apply_commit_filter(&mut self) {
        let Some(draft) = self.commit_filter_draft.take() else {
            return;
        };
        self.commit_filter_typed_at = None;
        let next = CommitQuery::parse(&draft);
        self.commit_filter = next.clone();
        if self.commit_filter_shown != next {
            self.launch_commit_search(next);
        }
        match self.commit_filter.as_ref() {
            Some(query) => {
                let label = self.commit_filter_match_label();
                self.set_message(format!("Filter: {query} \u{00b7} {label}"));
            }
            None => self.set_message("Filter cleared"),
        }
    }

    /// Esc while editing: drop the draft and put the rows back to the
    /// active filter if live typing had moved them away from it.
    pub fn cancel_commit_filter(&mut self) {
        self.commit_filter_draft = None;
        self.commit_filter_typed_at = None;
        if self.commit_filter_shown != self.commit_filter {
            let committed = self.commit_filter.clone();
            self.launch_commit_search(committed);
        }
    }

    /// Esc with a filter active: clear it and reload. Returns whether there
    /// was a filter to clear, so callers can fall through to their usual
    /// Esc behavior otherwise.
    pub fn clear_commit_filter(&mut self) -> bool {
        if self.commit_filter.take().is_none() {
            return false;
        }
        self.commit_filter_rx = None;
        self.commit_filter_shown = None;
        self.refresh_filtered_rows();
        self.set_message("Filter cleared");
        true
    }

    /// Whether a search is in flight (the title shows it).
    pub fn commit_filter_loading(&self) -> bool {
        self.commit_filter_rx.is_some()
    }

    fn refresh_filtered_rows(&mut self) {
        if self.input_mode == InputMode::CommitSelect {
            self.refresh_commit_selector_rows();
        } else {
            self.refresh_log_rows_keep_cursor();
        }
    }

    /// Run `query` (or the plain recent list for `None`) on a worker thread
    /// with its own VCS handle. Only the newest launch's answer is used.
    fn launch_commit_search(&mut self, query: Option<CommitQuery>) {
        self.commit_filter_generation += 1;
        let generation = self.commit_filter_generation;
        let limit = initial_commit_limit();
        let options = self.vcs_open_options.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.commit_filter_rx = Some(rx);
        self.commit_filter_shown = query.clone();
        std::thread::spawn(move || {
            let result = detect_vcs(options.git_backend_preference, options.diff_whitespace_mode)
                .and_then(|vcs| match &query {
                    Some(q) => vcs.search_commits(q, 0, limit),
                    None => vcs.get_recent_commits(0, limit),
                })
                .map(|rows| {
                    let has_more = rows.len() >= limit;
                    (rows, has_more)
                });
            let _ = tx.send(CommitFilterAnswer {
                generation,
                query,
                result,
            });
        });
    }

    /// Main-loop tick: fire a rested live search, and install a finished
    /// one. Returns whether the screen changed.
    pub fn poll_commit_filter(&mut self) -> bool {
        let mut changed = false;
        if let Some(typed_at) = self.commit_filter_typed_at
            && typed_at.elapsed() >= LIVE_FILTER_DELAY
            && let Some(draft) = self.commit_filter_draft.clone()
        {
            self.commit_filter_typed_at = None;
            let query = CommitQuery::parse(&draft);
            if self.commit_filter_shown != query {
                self.launch_commit_search(query);
                changed = true;
            }
        }
        let answer = match self.commit_filter_rx.as_ref() {
            Some(rx) => rx.try_recv().ok(),
            None => None,
        };
        let Some(answer) = answer else {
            return changed;
        };
        if answer.generation != self.commit_filter_generation {
            return changed;
        }
        self.commit_filter_rx = None;
        match answer.result {
            Ok((rows, has_more)) => self.install_filtered_rows(rows, has_more),
            Err(e) => self.set_error(format!("Search failed: {e}")),
        }
        true
    }

    /// Put searched history rows on screen behind the existing conflicts /
    /// unstaged / staged pseudo rows, keeping the cursor on its commit.
    pub(in crate::app) fn install_filtered_rows(
        &mut self,
        history: Vec<CommitInfo>,
        has_more: bool,
    ) {
        self.main_track = self.vcs.main_track();
        if self.input_mode == InputMode::CommitSelect {
            let special: Vec<CommitInfo> = self
                .commit_list
                .iter()
                .take_while(|c| Self::is_special_commit(c))
                .cloned()
                .collect();
            let mut rows = special;
            rows.extend(history);
            self.has_more_commit = has_more;
            self.commit_list = rows;
            self.commit_list_cursor = 0;
            self.commit_list_scroll_offset = 0;
            self.commit_picks.clear();
            self.visible_commit_count = self.commit_list.len();
        } else if let Some(pane) = self.log_pane.as_mut() {
            let anchor = pane
                .list
                .commits
                .get(pane.list.cursor)
                .map(|c| c.id.clone());
            let mut rows: Vec<CommitInfo> = pane
                .list
                .commits
                .iter()
                .take_while(|c| Self::is_special_commit(c))
                .cloned()
                .collect();
            rows.extend(history);
            pane.list.cursor = anchor
                .and_then(|id| rows.iter().position(|row| row.id == id))
                .unwrap_or(0)
                .min(rows.len().saturating_sub(1));
            pane.list.scroll_offset = pane.list.scroll_offset.min(pane.list.cursor);
            pane.list.visible_count = rows.len();
            pane.list.has_more = has_more;
            pane.list.commits = rows;
            pane.list.picks.clear();
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
        let busy = if self.commit_filter_loading() {
            " \u{2026}"
        } else {
            ""
        };
        format!("{n}{plus} {noun}{busy}")
    }
}
