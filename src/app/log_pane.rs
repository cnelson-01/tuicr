//! Persistent log pane: the commit graph docked above the diff while
//! reviewing, with IDE-style live preview of the highlighted commit.
//!
//! The fullscreen selector and the inline commit pane both drive `App`'s
//! flat commit-list fields (`commit_list`, `commit_list_cursor`, ...), and
//! every review load rewrites those fields with the new inline state. The
//! log pane therefore owns a [`LogListState`] of its own and swaps it into
//! the flat fields only for the duration of one action
//! ([`App::with_log_state`]), so the existing movement, pick, expand and
//! history-op code drives the log unchanged and a review load can never
//! clobber it.

use std::time::Instant;

use ratatui::layout::Rect;

use super::{
    App, DEFAULT_LOG_PANE_HEIGHT, FocusedPanel, InputMode, LOG_PREVIEW_DELAY, LogListState,
    LogPane, MERGE_SELECTION_ID, MIN_LOG_PANE_HEIGHT, initial_commit_limit,
};
use crate::error::Result;
use crate::vcs::CommitInfo;
use crate::vcs::traits::VcsType;

impl App {
    /// Whether the log pane is on screen.
    pub fn log_pane_active(&self) -> bool {
        self.log_pane.is_some()
    }

    /// Whether the log pane holds keyboard focus.
    pub fn log_pane_focused(&self) -> bool {
        self.log_pane.is_some() && self.focused_panel == FocusedPanel::CommitSelector
    }

    /// A commit pane of either kind sits above the diff: the persistent log
    /// or the inline range selector. Focus cycling and `<leader>k` use this.
    pub fn has_commit_pane(&self) -> bool {
        self.log_pane.is_some() || self.has_inline_commit_selector()
    }

    /// Body rect of the log pane from the last render, for mouse hit-testing.
    pub fn log_pane_inner_area(&self) -> Option<Rect> {
        self.log_pane.as_ref().and_then(|pane| pane.inner_area)
    }

    fn swap_log_state(&mut self) {
        let Some(pane) = self.log_pane.as_mut() else {
            return;
        };
        std::mem::swap(&mut pane.list.commits, &mut self.commit_list);
        std::mem::swap(&mut pane.list.cursor, &mut self.commit_list_cursor);
        std::mem::swap(
            &mut pane.list.scroll_offset,
            &mut self.commit_list_scroll_offset,
        );
        std::mem::swap(
            &mut pane.list.viewport_height,
            &mut self.commit_list_viewport_height,
        );
        std::mem::swap(&mut pane.list.picks, &mut self.commit_picks);
        std::mem::swap(&mut pane.list.visible_count, &mut self.visible_commit_count);
        std::mem::swap(&mut pane.list.has_more, &mut self.has_more_commit);
    }

    /// Run `f` with the log list swapped into the selector fields, then swap
    /// back. Everything `f` does to the flat fields lands in the log pane;
    /// the inline selector's state is untouched.
    pub fn with_log_state<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.swap_log_state();
        let result = f(self);
        self.swap_log_state();
        result
    }

    /// Selector rows straight from the VCS: history across local and
    /// remote-tracking branches plus the conflicts / unstaged / staged pseudo rows on top. Returns the
    /// rows and whether more history can be paged in. Shared with the
    /// fullscreen selector's post-operation refresh.
    pub(in crate::app) fn log_rows_from_vcs(&self) -> (Vec<CommitInfo>, bool) {
        let commit_limit = initial_commit_limit();
        let commits = self.fetch_history(0, commit_limit).unwrap_or_default();
        let highlighter = self.theme.syntax_highlighter();
        let change_status = Self::get_change_status_with_ignore(
            self.vcs.as_ref(),
            &self.vcs_info.root_path,
            highlighter,
            self.path_filter.as_deref(),
        )
        .unwrap_or_default();
        let merge_conflicts = self.vcs.get_merge_conflicts().ok().flatten();

        let has_more = commits.len() >= commit_limit;
        let mut rows = commits;
        if change_status.staged {
            rows.insert(0, Self::staged_commit_entry());
        }
        if change_status.unstaged {
            rows.insert(0, Self::unstaged_commit_entry());
        }
        if let Some(info) = &merge_conflicts {
            rows.insert(0, Self::merge_commit_entry(info));
        }
        (rows, has_more)
    }

    /// Open the log pane, landing the cursor on the review that is on
    /// screen, and give it focus.
    pub fn enable_log_pane(&mut self) {
        if self.log_pane.is_some() {
            return;
        }
        let (rows, has_more) = self.log_rows_from_vcs();
        let mut list = LogListState {
            visible_count: rows.len(),
            has_more,
            commits: rows,
            ..Default::default()
        };
        if let Some(pos) = self
            .review_commits
            .iter()
            .find(|c| !Self::is_special_commit(c))
            .and_then(|c| list.commits.iter().position(|row| row.id == c.id))
        {
            list.cursor = pos;
        }
        self.log_pane = Some(LogPane {
            list,
            ..Default::default()
        });
        self.log_pane_wanted = true;
        self.focused_panel = FocusedPanel::CommitSelector;
    }

    /// Close the log pane. Focus returns to the diff when the pane had it.
    pub fn disable_log_pane(&mut self) {
        if self.log_pane.take().is_none() {
            return;
        }
        self.log_pane_wanted = false;
        if self.focused_panel == FocusedPanel::CommitSelector {
            self.focused_panel = FocusedPanel::Diff;
        }
    }

    /// `<leader>g`, like an IDE tool-window key: hidden opens and focuses
    /// the pane, open-but-unfocused focuses it, focused hides it. So from
    /// anywhere in the review one press lands in the log and Esc (or the
    /// same key again) leaves it.
    pub fn toggle_log_pane(&mut self) {
        if self.log_pane.is_none() {
            self.set_log_pane_visible(true);
        } else if self.focused_panel != FocusedPanel::CommitSelector {
            self.focused_panel = FocusedPanel::CommitSelector;
            self.set_message("Log pane focused \u{00b7} esc returns to the diff");
        } else {
            self.set_log_pane_visible(false);
        }
    }

    pub fn set_log_pane_visible(&mut self, visible: bool) {
        if visible {
            self.enable_log_pane();
        } else {
            self.disable_log_pane();
        }
        let status = if visible { "visible" } else { "hidden" };
        self.set_message(format!("Log pane: {status}"));
    }

    /// Set the pane height in rows (borders included), floored at the
    /// smallest useful size. The renderer caps it so the diff keeps room.
    pub fn set_log_pane_height(&mut self, rows: u16) {
        let rows = rows.max(MIN_LOG_PANE_HEIGHT);
        self.log_pane_height = Some(rows);
        self.set_message(format!("Log pane height: {rows}"));
    }

    /// `<leader>+` / `<leader>-`: grow or shrink from the height on screen,
    /// so the first press after the window-sized default moves one row.
    pub fn adjust_log_pane_height(&mut self, delta: i16) {
        let current = self
            .log_pane
            .as_ref()
            .and_then(|pane| pane.inner_area)
            .map(|inner| inner.height + 2)
            .or(self.log_pane_height)
            .unwrap_or(DEFAULT_LOG_PANE_HEIGHT);
        let next = (i32::from(current) + i32::from(delta)).max(i32::from(MIN_LOG_PANE_HEIGHT));
        self.set_log_pane_height(next as u16);
    }

    /// Rows the pane takes out of a main area `area_height` tall. Until a
    /// height is set it takes a third of the area, at most the default, so
    /// a short terminal keeps most of its rows for the diff; either way the
    /// diff keeps at least six.
    pub fn log_pane_rows(&self, area_height: u16) -> u16 {
        let wanted = self
            .log_pane_height
            .unwrap_or_else(|| (area_height / 3).min(DEFAULT_LOG_PANE_HEIGHT));
        let max = area_height.saturating_sub(6).max(MIN_LOG_PANE_HEIGHT);
        wanted.clamp(MIN_LOG_PANE_HEIGHT, max).min(area_height)
    }

    /// Whether the review has local history for the pane to show: not a
    /// PR or plain-file review, and not `--all-files`, which has no diff.
    pub fn log_pane_supported(&self) -> bool {
        self.vcs_info.vcs_type != VcsType::File && !self.is_pristine_mode
    }

    /// Open the pane once a review is on screen, as an IDE keeps its Git
    /// tool window open (`show_log = false` opts out). The startup selector
    /// runs first, and the pane belongs to Normal mode, so this is polled
    /// from the main loop rather than done at startup.
    pub fn poll_log_pane_autoshow(&mut self) -> bool {
        if !self.log_pane_wanted
            || self.log_pane.is_some()
            || self.input_mode != InputMode::Normal
            || !self.log_pane_supported()
        {
            return false;
        }
        self.enable_log_pane();
        // Nobody pressed a key for this: keep the diff focused.
        self.focused_panel = FocusedPanel::Diff;
        true
    }

    // Movement and picks. Each runs inside `with_log_state` so the selector's
    // own cursor / scroll / pick code drives the log.

    pub fn log_pane_move_down(&mut self, n: usize) {
        self.with_log_state(|app| {
            for _ in 0..n.max(1) {
                app.commit_select_down();
            }
        });
        self.schedule_log_preview();
    }

    pub fn log_pane_move_up(&mut self, n: usize) {
        self.with_log_state(|app| {
            for _ in 0..n.max(1) {
                app.commit_select_up();
            }
        });
        self.schedule_log_preview();
    }

    pub fn log_pane_go_top(&mut self) {
        self.with_log_state(|app| {
            app.commit_list_cursor = 0;
            app.commit_list_scroll_offset = 0;
        });
        self.schedule_log_preview();
    }

    pub fn log_pane_go_bottom(&mut self) {
        self.with_log_state(|app| {
            let last = if app.can_show_more_commits() {
                app.visible_commit_count
            } else {
                app.visible_commit_count.saturating_sub(1)
            };
            app.commit_list_cursor = last;
        });
        self.schedule_log_preview();
    }

    /// Page by the pane height (or half of it).
    pub fn log_pane_page(&mut self, down: bool, half: bool) {
        let rows = self
            .log_pane
            .as_ref()
            .map(|pane| pane.list.viewport_height)
            .unwrap_or(0)
            .max(2);
        let n = if half { rows / 2 } else { rows };
        if down {
            self.log_pane_move_down(n);
        } else {
            self.log_pane_move_up(n);
        }
    }

    /// Space: toggle the cursor row in the pick set, or page in more history
    /// when the cursor sits on the overflow row.
    pub fn log_pane_toggle_pick(&mut self) {
        let result = self.with_log_state(|app| {
            if app.is_on_expand_row() {
                app.expand_commit()
            } else {
                app.toggle_commit_selection_and_advance();
                Ok(())
            }
        });
        if let Err(e) = result {
            self.set_error(format!("Failed to load commits: {e}"));
        }
    }

    /// Mouse click on a pane row: move the cursor there (which previews it)
    /// or page in more history from the overflow row.
    pub fn log_pane_click_row(&mut self, screen_row: usize) {
        let result = self.with_log_state(|app| {
            let idx = app.commit_list_scroll_offset + screen_row;
            let max_cursor = if app.can_show_more_commits() {
                app.visible_commit_count
            } else {
                app.visible_commit_count.saturating_sub(1)
            };
            if idx > max_cursor {
                return Ok(());
            }
            app.commit_list_cursor = idx;
            if app.is_on_expand_row() {
                app.expand_commit()
            } else {
                Ok(())
            }
        });
        if let Err(e) = result {
            self.set_error(format!("Failed to load commits: {e}"));
        }
        self.schedule_log_preview();
    }

    /// Enter: load the picks (or the cursor commit) as the review, keeping
    /// the log list intact around the load. The conflicts row opens the merge
    /// view and the overflow row pages in more history instead.
    pub fn log_pane_confirm(&mut self) -> Result<()> {
        if let Some(pane) = self.log_pane.as_mut() {
            pane.preview_at = None;
        }
        let on_special = self.with_log_state(|app| {
            if app.is_on_expand_row() {
                Some(app.expand_commit())
            } else if app.cursor_on_merge_row() {
                Some(app.open_merge_view(InputMode::Normal))
            } else {
                None
            }
        });
        if let Some(result) = on_special {
            return result;
        }

        self.swap_log_state();
        let log = LogListState {
            commits: self.commit_list.clone(),
            cursor: self.commit_list_cursor,
            scroll_offset: self.commit_list_scroll_offset,
            viewport_height: self.commit_list_viewport_height,
            picks: self.commit_picks.clone(),
            visible_count: self.visible_commit_count,
            has_more: self.has_more_commit,
        };
        let result = self.confirm_commit_selection();
        // A review load rewrote the selector fields with the new inline
        // state; that is exactly what the inline selector wants, so leave it
        // there and put the log back. If nothing was loaded the fields still
        // hold the log: swap it back out so the inline state survives too.
        let still_log = self.commit_list.len() == log.commits.len()
            && self
                .commit_list
                .iter()
                .zip(&log.commits)
                .all(|(a, b)| a.id == b.id);
        if still_log {
            self.swap_log_state();
        } else if let Some(pane) = self.log_pane.as_mut() {
            pane.list = log;
            if result.is_ok() {
                // The picks are now the review on screen; the loaded marker
                // shows them, so a fresh pick set is less confusing.
                pane.list.picks.clear();
            }
        }
        result
    }

    /// History operations (`p` / `b` / `s`) run through the selector's own
    /// code against the log list; their post-op refresh lands in the pane.
    pub fn log_pane_history_op(&mut self, op: impl FnOnce(&mut Self)) {
        self.with_log_state(op);
    }

    /// Note a cursor move; the preview fires once the cursor has rested.
    pub fn schedule_log_preview(&mut self) {
        if let Some(pane) = self.log_pane.as_mut() {
            pane.preview_at = Some(Instant::now());
        }
    }

    /// Fire a pending preview once the cursor has rested for
    /// [`LOG_PREVIEW_DELAY`]. Only a bare cursor (no picks) previews, and only
    /// when its row is not already the review on screen. Returns whether
    /// anything changed.
    pub fn poll_log_preview(&mut self) -> bool {
        let Some(pane) = self.log_pane.as_ref() else {
            return false;
        };
        let Some(at) = pane.preview_at else {
            return false;
        };
        if at.elapsed() < LOG_PREVIEW_DELAY || self.input_mode != InputMode::Normal {
            return false;
        }
        let pane = self.log_pane.as_mut().expect("checked above");
        pane.preview_at = None;
        if !pane.list.picks.is_empty() {
            return false;
        }
        let Some(commit) = pane.list.commits.get(pane.list.cursor) else {
            return false;
        };
        if commit.id == MERGE_SELECTION_ID {
            return false;
        }
        let already_loaded =
            self.review_commits.len() == 1 && self.review_commits[0].id == commit.id;
        if already_loaded {
            return false;
        }
        if let Err(e) = self.log_pane_confirm() {
            self.set_error(format!("Failed to load commit: {e}"));
        }
        true
    }

    /// Re-read the log rows from the VCS (after a history operation, a
    /// merge resolution, or a diff-watch tick) keeping the cursor on the
    /// same commit where it still exists.
    pub fn refresh_log_rows_keep_cursor(&mut self) {
        // Whatever moved the log (a commit, a checkout, a stage) moved the
        // working tree's status too.
        self.refresh_changes_pane();
        self.refresh_log_rows_only();
    }

    /// `refresh_log_rows_keep_cursor` for callers that re-read the status
    /// themselves.
    pub(in crate::app) fn refresh_log_rows_only(&mut self) {
        let Some(pane) = self.log_pane.as_ref() else {
            return;
        };
        let anchor = pane
            .list
            .commits
            .get(pane.list.cursor)
            .map(|c| c.id.clone());
        let (rows, has_more) = self.log_rows_from_vcs();
        self.commit_filter_shown = self.commit_filter.clone();
        let pane = self.log_pane.as_mut().expect("checked above");
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
