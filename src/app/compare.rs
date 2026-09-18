//! `:compare A B` and the branch picker's compare: load a revision range as
//! the review without leaving the TUI, the way `tuicr -r A...B` does at
//! startup.

use super::*;

/// Turn `:compare` arguments into a revision expression: two names become
/// `A...B` (what B has that A does not, from their merge base), one name is
/// compared against HEAD the same way, and anything already containing `..`
/// is used as typed.
pub(crate) fn compare_spec(args: &str) -> Option<String> {
    let words: Vec<&str> = args.split_whitespace().collect();
    match words.as_slice() {
        [] => None,
        [one] if one.contains("..") => Some((*one).to_string()),
        [one] => Some(format!("HEAD...{one}")),
        [a, b, ..] => Some(format!("{a}...{b}")),
    }
}

impl App {
    /// `:compare <base> <other>`, `:compare <other>`, or `:compare A..B`.
    pub fn compare_command(&mut self, args: &str) {
        let Some(spec) = compare_spec(args) else {
            self.set_error("Usage: :compare <base> <other>  (or :compare A..B, :compare <other>)");
            return;
        };
        self.compare_revisions(&spec);
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
        self.reset_persisted_session_tracking();

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

#[cfg(test)]
mod tests {
    use super::compare_spec;

    #[test]
    fn compare_spec_builds_merge_base_ranges() {
        assert_eq!(compare_spec("").as_deref(), None);
        assert_eq!(
            compare_spec("master origin/master").as_deref(),
            Some("master...origin/master")
        );
        assert_eq!(compare_spec("feature").as_deref(), Some("HEAD...feature"));
        assert_eq!(compare_spec("main..dev").as_deref(), Some("main..dev"));
        assert_eq!(compare_spec("a...b").as_deref(), Some("a...b"));
        // extra words are ignored rather than rejected
        assert_eq!(compare_spec("a b c").as_deref(), Some("a...b"));
    }
}
