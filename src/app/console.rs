//! `:console` — the git commands tuicr ran (JetBrains' Git Console), newest
//! at the bottom. `m` switches between changes only and every command,
//! `c` clears the log.

use super::{App, InputMode};

#[derive(Debug, Clone)]
pub struct ConsoleView {
    /// Rows scrolled up from the bottom (0 = following the newest entry).
    pub scroll_from_bottom: usize,
    /// Show only commands that change the repository or a remote.
    pub mutating_only: bool,
    pub return_mode: InputMode,
    /// Rows the last render had, for paging.
    pub viewport_height: usize,
}

impl App {
    pub fn open_console(&mut self) {
        self.console = Some(ConsoleView {
            scroll_from_bottom: 0,
            mutating_only: true,
            return_mode: self.input_mode,
            viewport_height: 0,
        });
        self.input_mode = InputMode::Console;
    }

    pub fn close_console(&mut self) {
        if let Some(view) = self.console.take() {
            self.input_mode = view.return_mode;
        }
    }

    pub fn console_scroll(&mut self, up: isize) {
        if let Some(view) = self.console.as_mut() {
            view.scroll_from_bottom = (view.scroll_from_bottom as isize + up).max(0) as usize;
        }
    }

    pub fn console_page(&mut self, up: bool) {
        let page = self
            .console
            .as_ref()
            .map_or(10, |v| v.viewport_height.saturating_sub(2).max(1)) as isize;
        self.console_scroll(if up { page } else { -page });
    }

    pub fn console_toggle_mutating(&mut self) {
        if let Some(view) = self.console.as_mut() {
            view.mutating_only = !view.mutating_only;
            view.scroll_from_bottom = 0;
        }
    }

    pub fn console_clear(&mut self) {
        crate::git_console::clear();
        if let Some(view) = self.console.as_mut() {
            view.scroll_from_bottom = 0;
        }
    }
}
