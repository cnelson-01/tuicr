//! Pane sizing and density: the side-by-side splitter, the file tree width,
//! zen mode, and horizontal pans of the diff. The keyboard counterparts of
//! dragging a JetBrains splitter or hiding every tool window.

use super::{
    App, DEFAULT_FILE_LIST_PCT, DEFAULT_SBS_SPLIT_PCT, DiffSource, DiffViewMode,
    DiffWhitespaceMode, FocusedPanel, MAX_FILE_LIST_PCT, MAX_SBS_SPLIT_PCT, MIN_FILE_LIST_PCT,
    MIN_SBS_SPLIT_PCT, Splitter, ZenSaved,
};

/// Percentage points one splitter key press moves.
pub const SPLIT_STEP_PCT: i16 = 5;

impl App {
    /// `:set split N` — the old side's share of the side-by-side width.
    pub fn set_sbs_split(&mut self, pct: u16) {
        self.sbs_split_pct = pct.clamp(MIN_SBS_SPLIT_PCT, MAX_SBS_SPLIT_PCT);
        self.diff_state.scroll_x = self.diff_state.scroll_x.min(self.max_pan());
        let note = if self.diff_view_mode == DiffViewMode::SideBySide {
            ""
        } else {
            " (applies to side-by-side view, :diff)"
        };
        self.set_message(format!(
            "Split: old {}% · new {}%{note}",
            self.sbs_split_pct,
            100 - self.sbs_split_pct
        ));
    }

    /// `<leader><` / `<leader>>` — move the side-by-side divider.
    pub fn adjust_sbs_split(&mut self, delta: i16) {
        let next = (i32::from(self.sbs_split_pct) + i32::from(delta)).max(0) as u16;
        self.set_sbs_split(next);
    }

    /// `:set treewidth N` — the file tree's share of the content width.
    pub fn set_file_list_pct(&mut self, pct: u16) {
        self.file_list_pct = pct.clamp(MIN_FILE_LIST_PCT, MAX_FILE_LIST_PCT);
        if !self.show_file_list {
            self.show_file_list = true;
        }
        self.set_message(format!("File tree width: {}%", self.file_list_pct));
    }

    /// `<leader>[` / `<leader>]` — narrow or widen the file tree.
    pub fn adjust_file_list_pct(&mut self, delta: i16) {
        let next = (i32::from(self.file_list_pct) + i32::from(delta)).max(0) as u16;
        self.set_file_list_pct(next);
    }

    /// `<leader>=` — put the splitter and tree width back to their defaults.
    pub fn reset_layout(&mut self) {
        self.sbs_split_pct = DEFAULT_SBS_SPLIT_PCT;
        self.file_list_pct = DEFAULT_FILE_LIST_PCT;
        self.set_message("Layout reset: split 50/50, file tree 20%");
    }

    /// Move a splitter to the screen column `x` (mouse drag).
    pub fn drag_splitter_to(&mut self, splitter: Splitter, x: u16) {
        match splitter {
            Splitter::FileList => {
                let Some(content) = self.content_area else {
                    return;
                };
                if content.width == 0 {
                    return;
                }
                let rel = x.saturating_sub(content.x) as u32 + 1;
                let pct = (rel * 100 / u32::from(content.width)) as u16;
                self.file_list_pct = pct.clamp(MIN_FILE_LIST_PCT, MAX_FILE_LIST_PCT);
            }
            Splitter::SideBySide => {
                let Some(inner) = self.diff_inner_area else {
                    return;
                };
                let lw = self.lineno_width();
                let available = inner.width.saturating_sub(super::sbs_overhead(lw));
                if available == 0 {
                    return;
                }
                let start = inner.x + super::sbs_left_gutter(lw);
                let rel = x.saturating_sub(start) as u32;
                let pct = (rel * 100 / u32::from(available)) as u16;
                self.sbs_split_pct = pct.clamp(MIN_SBS_SPLIT_PCT, MAX_SBS_SPLIT_PCT);
            }
        }
    }

    /// Which splitter, if any, sits under screen cell `(x, y)`: the file
    /// tree's right border, or the side-by-side `│` divider in the diff body.
    pub fn splitter_at(&self, x: u16, y: u16) -> Option<Splitter> {
        if let Some(tree) = self.file_list_area
            && self.show_file_list
            && y >= tree.y
            && y < tree.y + tree.height
            && x + 1 == tree.x + tree.width
        {
            return Some(Splitter::FileList);
        }
        if self.diff_view_mode == DiffViewMode::SideBySide
            && let Some(inner) = self.diff_inner_area
            && y >= inner.y
            && y < inner.y + inner.height
        {
            let lw = self.lineno_width();
            let (left_w, _) = self.sbs_column_widths(inner.width);
            // `" │ "`: the glyph sits one cell past the left column; accept
            // the cells either side so the grab target is three wide.
            let divider = inner.x + super::sbs_left_gutter(lw) + left_w as u16 + 1;
            if x + 1 >= divider && x <= divider + 1 {
                return Some(Splitter::SideBySide);
            }
        }
        None
    }

    /// `<leader>z` — hide every panel but the diff (JetBrains "Hide All Tool
    /// Windows"); a second press restores exactly what was open.
    pub fn toggle_zen(&mut self) {
        if let Some(saved) = self.zen_saved.take() {
            self.show_file_list = saved.show_file_list;
            self.show_commit_selector = saved.show_commit_selector;
            if saved.log_pane_open {
                self.enable_log_pane();
            }
            self.focused_panel = FocusedPanel::Diff;
            self.set_message("Zen mode off");
            return;
        }
        self.zen_saved = Some(ZenSaved {
            show_file_list: self.show_file_list,
            log_pane_open: self.log_pane.is_some(),
            show_commit_selector: self.show_commit_selector,
        });
        self.show_file_list = false;
        self.show_commit_selector = false;
        self.disable_log_pane();
        self.focused_panel = FocusedPanel::Diff;
        self.set_message("Zen mode: diff only · <leader>z restores the panels");
    }

    pub fn zen_active(&self) -> bool {
        self.zen_saved.is_some()
    }

    /// Visible content width of the narrowest diff column, the unit a
    /// half-screen pan moves by.
    fn pan_viewport_width(&self) -> usize {
        let width = self.diff_state.viewport_width;
        if self.diff_view_mode == DiffViewMode::SideBySide {
            let (l, r) = self.sbs_column_widths(width as u16);
            l.min(r)
        } else {
            width.saturating_sub(super::unified_gutter(self.lineno_width()) as usize)
        }
    }

    fn max_pan(&self) -> usize {
        self.diff_state
            .max_content_width
            .saturating_sub(self.pan_viewport_width())
    }

    /// `zH` / `zL` — pan half a column width left or right.
    pub fn pan_half_screen(&mut self, right: bool) {
        if self.diff_state.wrap_lines {
            self.set_message("Wrap is on · :set wrap! to pan instead");
            return;
        }
        let step = (self.pan_viewport_width() / 2).max(1);
        if right {
            self.scroll_right(step);
        } else {
            self.scroll_left(step);
        }
    }

    /// `zs` — pan back to the start of the line.
    pub fn pan_home(&mut self) {
        self.diff_state.scroll_x = 0;
    }

    /// `ze` — pan so the end of the longest line on screen is visible.
    pub fn pan_end(&mut self) {
        if self.diff_state.wrap_lines {
            return;
        }
        self.diff_state.scroll_x = self.max_pan();
    }

    /// `:set ignorews` / `<leader>w` — compare lines ignoring whitespace
    /// (JetBrains' "Ignore whitespaces" diff option). Reopens the backend
    /// with the new mode and reloads the diff.
    pub fn set_ignore_whitespace(&mut self, ignore: bool) {
        if matches!(self.diff_source, DiffSource::PullRequest(_)) {
            self.set_message("Ignore-whitespace applies to local diffs, not PR reviews");
            return;
        }
        let mode = if ignore {
            DiffWhitespaceMode::IgnoreAll
        } else {
            DiffWhitespaceMode::Normal
        };
        self.vcs_open_options.diff_whitespace_mode = mode;
        match crate::vcs::detect_vcs(self.vcs_open_options.git_backend_preference, mode) {
            Ok(vcs) => {
                self.vcs_info = vcs.info().clone();
                self.vcs = vcs;
            }
            Err(e) => {
                self.set_warning(format!("Reopening the repository failed: {e}"));
                return;
            }
        }
        let state = if ignore { "ignored" } else { "compared" };
        match self.reload_diff_files() {
            Ok((count, _)) => {
                self.set_message(format!("Whitespace {state} · {count} files"));
            }
            Err(e) => self.set_warning(format!("Reload failed: {e}")),
        }
    }

    pub fn ignores_whitespace(&self) -> bool {
        self.vcs_open_options.diff_whitespace_mode.ignores_all()
    }
}
