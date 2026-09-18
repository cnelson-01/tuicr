//! `:compare A B` and the branch picker's compare: load a revision range as
//! the review without leaving the TUI, the way `tuicr -r A...B` does at
//! startup.

use super::*;
use crate::vcs::CommitQuery;

impl App {
    /// `:compare <base> <other>` (alias `:select`), `:compare <other>`, or a
    /// typed range like `:compare A..B`.
    pub fn compare_command(&mut self, args: &str) {
        let words: Vec<&str> = args.split_whitespace().collect();
        match words.as_slice() {
            [] => self
                .set_error("Usage: :compare <base> <other>  (or :compare <other>, :compare A..B)"),
            // A typed range is loaded as written: no direction guessing and
            // no log narrowing, since the endpoints may not be refs.
            [one] if one.contains("..") => self.compare_revisions(one),
            [one] => {
                let current = self
                    .vcs_info
                    .branch_name
                    .clone()
                    .unwrap_or_else(|| "HEAD".to_string());
                self.compare_pair(&current, one);
            }
            [base, other, ..] => {
                let (base, other) = ((*base).to_string(), (*other).to_string());
                self.compare_pair(&base, &other);
            }
        }
    }

    /// Compare two refs: load `base...other` (what `other` has since their
    /// merge base). When that side is empty because `base` is the one ahead,
    /// load `other...base` instead and say so. Either way the log pane is
    /// narrowed to just the two refs.
    pub fn compare_pair(&mut self, base: &str, other: &str) {
        if base == other {
            self.set_message(format!("{base} compared with itself: nothing to show"));
            return;
        }
        let forward = format!("{base}...{other}");
        let loaded = match self.open_revision_range(&forward) {
            Ok(count) => Ok((forward, count, false)),
            Err(first) => {
                let backward = format!("{other}...{base}");
                match self.open_revision_range(&backward) {
                    Ok(count) => Ok((backward, count, true)),
                    Err(_) => Err(first),
                }
            }
        };
        match loaded {
            Ok((spec, count, flipped)) => {
                self.narrow_log_to_refs(&[base, other]);
                let note = if flipped {
                    format!(" \u{00b7} {other} has nothing beyond {base}, so this is the reverse")
                } else {
                    String::new()
                };
                self.set_message(format!(
                    "Comparing {spec} \u{00b7} {count} commit{}{note}",
                    if count == 1 { "" } else { "s" }
                ));
            }
            Err(_) => self.set_error(format!(
                "No differences between {base} and {other} in either direction"
            )),
        }
    }

    /// Show only `refs` in the log pane (opening it if hidden) without
    /// moving focus.
    fn narrow_log_to_refs(&mut self, refs: &[&str]) {
        self.commit_filter = Some(CommitQuery {
            refs: refs.iter().map(|r| (*r).to_string()).collect(),
            ..Default::default()
        });
        self.commit_filter_draft = None;
        self.commit_filter_rx = None;
        if self.log_pane.is_none() {
            let focus = self.focused_panel;
            self.enable_log_pane();
            self.focused_panel = focus;
        } else {
            self.refresh_log_rows_keep_cursor();
        }
        self.commit_filter_shown = self.commit_filter.clone();
    }

    /// Load `spec` as the review and report what came up.
    pub fn compare_revisions(&mut self, spec: &str) {
        match self.open_revision_range(spec) {
            Ok(count) => self.set_message(format!(
                "Comparing {spec} \u{00b7} {count} commit{}",
                if count == 1 { "" } else { "s" }
            )),
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("Compare failed: {}", err.trim()));
            }
        }
    }

    /// Load a revision expression (`A..B`, `A...B`, or one revision) as the
    /// review: resolve it, diff it, restore or create its session, and set
    /// up the inline selector over its commits. Returns the commit count.
    pub fn open_revision_range(&mut self, spec: &str) -> Result<usize> {
        let range = self.vcs.resolve_revision_range(spec)?;
        let commit_ids = range.commit_ids.to_vec();
        if commit_ids.is_empty() {
            return Err(TuicrError::VcsCommand(format!(
                "{spec} selects no commits (the two sides are the same)"
            )));
        }
        let highlighter = self.theme.syntax_highlighter();
        let diff_files = Self::get_commit_range_diff_with_ignore(
            self.vcs.as_ref(),
            &self.vcs_info.root_path,
            &range,
            highlighter,
            self.path_filter.as_deref(),
        )?;
        if diff_files.is_empty() {
            return Err(TuicrError::VcsCommand(format!("No changes in {spec}")));
        }
        let review_commits: Vec<CommitInfo> = self
            .vcs
            .get_commits_info(&commit_ids)?
            .into_iter()
            .rev()
            .collect();

        let mut session = Self::load_or_create_commit_range_session(&self.vcs_info, &commit_ids);
        for file in &diff_files {
            session.add_diff_file(file);
        }
        self.session = session;
        self.reset_persisted_session_tracking()?;

        self.diff_files = diff_files;
        self.diff_source = DiffSource::CommitRange(commit_ids);
        self.input_mode = InputMode::Normal;
        self.diff_state = DiffState::default();
        self.file_list_state = FileListState::default();

        // Inline selector over the range's commits, newest first, exactly as
        // a startup `-r` review sets it up. With the log pane open these
        // fields are the inline state; the pane keeps its own list.
        let count = review_commits.len();
        self.pr_commits.clear();
        self.pr_last_reviewed_commit_index = None;
        self.range_diff_files = Some(self.diff_files.clone());
        self.commit_list = review_commits.clone();
        self.review_commits = review_commits;
        let range = Self::initial_commit_range(self.commit_selection_start, count);
        self.commit_selection_range = range;
        self.commit_list_cursor = range.map(|(start, _)| start).unwrap_or(0);
        self.commit_list_scroll_offset = 0;
        self.visible_commit_count = count;
        self.has_more_commit = false;
        self.show_commit_selector = count > 1;
        self.commit_diff_cache.clear();
        self.saved_inline_selection = None;
        self.commit_picks.clear();

        if Self::is_strict_commit_selection(self.commit_selection_range, count) {
            self.reload_inline_selection()?;
        } else {
            self.insert_commit_message_if_single();
            self.sort_files_by_directory(true);
            self.expand_all_dirs();
            self.rebuild_annotations();
        }
        Ok(count)
    }
}
