//! Find in files: `:find <text>` searches the working tree with `rg` (or
//! `git grep` when ripgrep is not installed) and shows the hits in place of
//! the diff. Enter jumps to the hit when its file is in the loaded diff and
//! opens `$EDITOR` at the line otherwise; `e` always opens the editor.

use std::path::{Path, PathBuf};

use super::{App, InputMode};
use crate::editor::EditorTarget;
use crate::error::{Result, TuicrError};
use crate::model::LineSide;
use crate::process::{CommandOutputErrorKind, run_command_output};

/// Hits beyond this are dropped and the view says so.
pub const FIND_MAX_HITS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindHit {
    /// Repository-relative path, as the diff's display paths are.
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct FindState {
    pub query: String,
    pub hits: Vec<FindHit>,
    pub selected: usize,
    pub scroll_offset: usize,
    /// Rows available, refreshed each render.
    pub viewport_height: usize,
    /// `rg` or `git grep`, for the title.
    pub tool: &'static str,
    pub truncated: bool,
}

/// `path:line:column:text` as `rg --column` and `git grep --column` print
/// it. Paths may contain `:`, so the split is the first `:N:M:` run.
pub(crate) fn parse_grep_line(line: &str) -> Option<FindHit> {
    for (idx, _) in line.match_indices(':') {
        let rest = &line[idx + 1..];
        let mut fields = rest.splitn(3, ':');
        let (Some(line_no), Some(column), Some(text)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let (Ok(line_no), Ok(column)) = (line_no.parse::<u32>(), column.parse::<u32>()) {
            return Some(FindHit {
                path: PathBuf::from(&line[..idx]),
                line: line_no,
                column,
                text: text.to_string(),
            });
        }
    }
    None
}

/// Fixed-string, smart-case search of `root`. Exit status 1 from either
/// tool means "no matches", not failure.
fn run_search(root: &Path, query: &str) -> Result<(Vec<FindHit>, &'static str)> {
    let case_insensitive = query.chars().all(|c| !c.is_uppercase());
    let mut rg_args = vec![
        "--line-number",
        "--column",
        "--no-heading",
        "--color",
        "never",
        "--fixed-strings",
    ];
    if case_insensitive {
        rg_args.push("--ignore-case");
    }
    rg_args.extend(["-e", query]);
    let (output, tool) = match run_command_output("rg", Some(root), &rg_args) {
        Ok(output) => (output, "rg"),
        Err(err) if err.kind == CommandOutputErrorKind::Unsuccessful && err.status == Some(1) => {
            (String::new(), "rg")
        }
        Err(err) if err.kind == CommandOutputErrorKind::NotFound => {
            let mut args = vec!["grep", "-n", "--column", "-I", "-F"];
            if case_insensitive {
                args.push("-i");
            }
            args.extend(["-e", query, "--"]);
            match run_command_output("git", Some(root), &args) {
                Ok(output) => (output, "git grep"),
                Err(err)
                    if err.kind == CommandOutputErrorKind::Unsuccessful
                        && err.status == Some(1) =>
                {
                    (String::new(), "git grep")
                }
                Err(err) => {
                    return Err(TuicrError::VcsCommand(format!(
                        "git grep failed: {}",
                        err.stderr.trim()
                    )));
                }
            }
        }
        Err(err) => {
            return Err(TuicrError::VcsCommand(format!(
                "rg failed: {}",
                err.stderr.trim()
            )));
        }
    };
    let hits: Vec<FindHit> = output
        .lines()
        .filter_map(parse_grep_line)
        .take(FIND_MAX_HITS + 1)
        .collect();
    Ok((hits, tool))
}

impl App {
    /// `:find <text>` / `:grep <text>`.
    pub fn find_in_files(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            self.set_error("Usage: :find <text>");
            return;
        }
        match run_search(&self.vcs_info.root_path, query) {
            Ok((hits, tool)) => self.open_find_results(query, hits, tool),
            Err(e) => self.set_error(format!("Find failed: {e}")),
        }
    }

    /// Show `hits` for `query`; with none, just say so and stay put.
    pub fn open_find_results(&mut self, query: &str, mut hits: Vec<FindHit>, tool: &'static str) {
        let truncated = hits.len() > FIND_MAX_HITS;
        hits.truncate(FIND_MAX_HITS);
        self.find = FindState {
            query: query.to_string(),
            hits,
            selected: 0,
            scroll_offset: 0,
            viewport_height: self.find.viewport_height,
            tool,
            truncated,
        };
        if self.find.hits.is_empty() {
            self.set_message(format!("No matches for \"{query}\""));
            return;
        }
        self.input_mode = InputMode::FindResults;
    }

    /// `:find` with no text: bring the last results back, or prompt.
    pub fn reopen_find_results(&mut self) {
        if self.find.hits.is_empty() {
            self.start_find_prompt();
        } else {
            self.input_mode = InputMode::FindResults;
        }
    }

    /// `<leader>F`: the command line, pre-filled with `find `.
    pub fn start_find_prompt(&mut self) {
        if self.input_mode == InputMode::FindResults {
            self.input_mode = InputMode::Normal;
        }
        self.enter_command_mode();
        self.command_buffer = "find ".to_string();
    }

    pub fn exit_find_results(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    pub fn find_select_down(&mut self, n: usize) {
        let last = self.find.hits.len().saturating_sub(1);
        self.find.selected = (self.find.selected + n.max(1)).min(last);
    }

    pub fn find_select_up(&mut self, n: usize) {
        self.find.selected = self.find.selected.saturating_sub(n.max(1));
    }

    pub fn find_select_first(&mut self) {
        self.find.selected = 0;
    }

    pub fn find_select_last(&mut self) {
        self.find.selected = self.find.hits.len().saturating_sub(1);
    }

    pub fn find_page(&mut self, down: bool, half: bool) {
        let rows = self.find.viewport_height.max(2);
        let n = if half { rows / 2 } else { rows };
        if down {
            self.find_select_down(n);
        } else {
            self.find_select_up(n);
        }
    }

    /// Enter: jump to the hit in the loaded diff when its file is there,
    /// else open `$EDITOR` at the line. `e` (`force_editor`) skips the diff.
    pub fn find_open_selected(&mut self, force_editor: bool) {
        let Some(hit) = self.find.hits.get(self.find.selected).cloned() else {
            return;
        };
        self.exit_find_results();
        let in_diff = self
            .diff_files
            .iter()
            .position(|file| file.display_path() == &hit.path);
        match in_diff {
            Some(idx) if !force_editor => {
                self.jump_to_file(idx);
                self.go_to_source_line(hit.line, LineSide::New);
                self.set_message(format!("{}:{}", hit.path.display(), hit.line));
            }
            _ => {
                self.pending_editor_target = Some(EditorTarget {
                    path: self.vcs_info.root_path.join(&hit.path),
                    line: Some(hit.line),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_grep_lines_including_paths_with_colons() {
        let hit = parse_grep_line("src/app/mod.rs:12:5:    pub fn x() {").expect("hit");
        assert_eq!(hit.path, PathBuf::from("src/app/mod.rs"));
        assert_eq!((hit.line, hit.column), (12, 5));
        assert_eq!(hit.text, "    pub fn x() {");

        let odd = parse_grep_line("notes/a:b.md:3:1:text: with colons").expect("hit");
        assert_eq!(odd.path, PathBuf::from("notes/a:b.md"));
        assert_eq!((odd.line, odd.column), (3, 1));
        assert_eq!(odd.text, "text: with colons");

        assert!(parse_grep_line("no numbers here").is_none());
        assert!(parse_grep_line("").is_none());
    }
}
