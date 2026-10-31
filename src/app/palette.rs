//! Command palette (`Ctrl-p`, `:palette`): fuzzy-search the action catalog,
//! see each action's keys, and run it. The palette performs an action by
//! focusing the pane it belongs to and replaying its keys through the normal
//! input path, so a palette run and a typed key can never behave differently.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, FocusedPanel, InputMode};
use crate::input::catalog::{self, ENTRIES, Run, Scope};

#[derive(Debug, Clone)]
pub struct Palette {
    pub query: String,
    /// Row within `matches`.
    pub cursor: usize,
    /// Indices into [`ENTRIES`], best match first.
    pub matches: Vec<usize>,
    /// Scope of the pane the palette was opened from, ranked first.
    pub scope: Scope,
    pub return_mode: InputMode,
}

impl App {
    /// The catalog scope of whatever has focus right now.
    pub fn current_scope(&self) -> Scope {
        match self.input_mode {
            // The fullscreen selector takes the log pane's commit keys.
            InputMode::CommitSelect => return Scope::Log,
            InputMode::BranchPicker => return Scope::Branches,
            InputMode::StashPicker => return Scope::Stashes,
            InputMode::MergeConflict => return Scope::Merge,
            _ => {}
        }
        match self.focused_panel {
            FocusedPanel::FileList => Scope::Tree,
            FocusedPanel::CommitSelector if self.log_pane.is_some() => Scope::Log,
            _ => Scope::Diff,
        }
    }

    pub fn open_palette(&mut self) {
        let mut palette = Palette {
            query: String::new(),
            cursor: 0,
            matches: Vec::new(),
            scope: self.current_scope(),
            return_mode: self.input_mode,
        };
        palette.matches = rank(&palette.query, palette.scope, self.leader_key);
        self.palette = Some(palette);
        self.input_mode = InputMode::Palette;
    }

    pub fn close_palette(&mut self) {
        if let Some(palette) = self.palette.take() {
            self.input_mode = palette.return_mode;
        }
    }

    fn refilter_palette(&mut self) {
        let leader = self.leader_key;
        if let Some(palette) = self.palette.as_mut() {
            palette.matches = rank(&palette.query, palette.scope, leader);
            palette.cursor = 0;
        }
    }

    pub fn palette_push(&mut self, c: char) {
        if let Some(palette) = self.palette.as_mut() {
            palette.query.push(c);
        }
        self.refilter_palette();
    }

    pub fn palette_pop(&mut self) {
        if let Some(palette) = self.palette.as_mut() {
            palette.query.pop();
        }
        self.refilter_palette();
    }

    pub fn palette_clear(&mut self) {
        if let Some(palette) = self.palette.as_mut() {
            palette.query.clear();
        }
        self.refilter_palette();
    }

    pub fn palette_move(&mut self, delta: isize) {
        if let Some(palette) = self.palette.as_mut() {
            let n = palette.matches.len();
            if n == 0 {
                return;
            }
            palette.cursor = (palette.cursor as isize + delta).rem_euclid(n as isize) as usize;
        }
    }

    /// Run the highlighted entry: close the palette, put focus where the
    /// entry's keys work, and queue the keys (or the `:` command) as input.
    pub fn palette_run(&mut self) {
        let Some(idx) = self
            .palette
            .as_ref()
            .and_then(|p| p.matches.get(p.cursor).copied())
        else {
            self.close_palette();
            return;
        };
        self.close_palette();
        let entry = ENTRIES[idx];
        let leader = self.leader_key;
        match entry.run {
            Run::Command(cmd) => {
                self.inject_text(&format!(":{cmd}"));
                self.injected_keys
                    .push_back(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            Run::Prompt(prefill) => self.inject_text(&format!(":{prefill}")),
            Run::Keys(keys) => {
                self.focus_scope(entry.scope);
                self.injected_keys.extend(catalog::parse_keys(keys, leader));
            }
        }
    }

    fn inject_text(&mut self, text: &str) {
        self.injected_keys.extend(text.chars().map(|c| {
            let mods = if c.is_ascii_uppercase() {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            KeyEvent::new(KeyCode::Char(c), mods)
        }));
    }

    /// Move focus to the pane whose keys an entry replays.
    fn focus_scope(&mut self, scope: Scope) {
        if self.input_mode != InputMode::Normal {
            return;
        }
        match scope {
            Scope::Diff => self.focused_panel = FocusedPanel::Diff,
            Scope::Tree => {
                self.show_file_list = true;
                self.focused_panel = FocusedPanel::FileList;
            }
            Scope::Log => {
                if self.log_pane.is_none() {
                    self.enable_log_pane();
                }
                self.focused_panel = FocusedPanel::CommitSelector;
            }
            Scope::Global | Scope::Branches | Scope::Stashes | Scope::Merge => {}
        }
    }
}

/// Catalog indices matching `query`, best first. With an empty query every
/// entry is listed: the focused scope, then global actions, then the rest.
fn rank(query: &str, scope: Scope, leader: char) -> Vec<usize> {
    let scope_bonus = |s: Scope| {
        if s == scope {
            60
        } else if s == Scope::Global {
            30
        } else {
            0
        }
    };
    let mut scored: Vec<(i32, usize)> = ENTRIES
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let haystack = format!(
                "{} {} {} {}",
                entry.desc,
                entry.group,
                catalog::display_keys(entry.keys, leader),
                entry.scope.label()
            );
            let score = if query.trim().is_empty() {
                0
            } else {
                catalog::match_score(query, &haystack)?
            };
            Some((score + scope_bonus(entry.scope), i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}
