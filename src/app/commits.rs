use super::*;

impl App {
    /// The commit-selection range a fresh multi-commit review opens with,
    /// honoring the `initial_commit_selection` config. `review_commits` is stored
    /// newest-first, so the oldest commit is the last index — that stays true
    /// regardless of the `commit_order` display setting (which is presentation
    /// only). Returns `None` for an empty list.
    pub(in crate::app) fn initial_commit_range(
        start: CommitSelectionStart,
        n: usize,
    ) -> Option<(usize, usize)> {
        match (start, n) {
            (_, 0) => None,
            (CommitSelectionStart::Oldest, n) => Some((n - 1, n - 1)),
            (CommitSelectionStart::All, n) => Some((0, n - 1)),
        }
    }

    pub(in crate::app) fn is_strict_commit_selection(
        range: Option<(usize, usize)>,
        total: usize,
    ) -> bool {
        range.is_some_and(|(start, end)| {
            total > 0 && start <= end && end < total && (start > 0 || end + 1 < total)
        })
    }

    pub(in crate::app) fn apply_pr_commit_selector(
        &mut self,
        commits: Vec<crate::forge::traits::PullRequestCommit>,
        review_metadata: crate::forge::traits::PullRequestReviewMetadata,
    ) -> Option<String> {
        self.pr_last_reviewed_commit_index = None;
        if commits.len() <= 1 {
            return None;
        }

        let since_last_review = commits_since_last_review_selection(&commits, &review_metadata);
        self.set_pr_last_reviewed_commit_from_metadata(&commits, &review_metadata);

        self.pr_commits = commits.clone();
        let mapped: Vec<CommitInfo> = commits.iter().map(pr_commit_to_commit_info).collect();
        self.range_diff_files = Some(self.diff_files.clone());
        self.commit_list = mapped.clone();
        self.commit_list_cursor = 0;
        self.commit_list_scroll_offset = 0;
        self.visible_commit_count = mapped.len();
        self.has_more_commit = false;
        self.show_commit_selector = true;

        let mut range = (0, mapped.len() - 1);
        let mut auto_scoped_since_last_review = false;
        let mut since_last_review_message = None;
        // Restore any persisted range scoped to this head SHA. If the
        // restored range exceeds the current commit count (e.g., the PR
        // was rebased), fall back to "all". A valid persisted range wins over
        // both the `initial_commit_selection = oldest` opt-in and the
        // since-last-review auto-scoping; `oldest` in turn takes precedence
        // over since-last-review.
        if let Some(persisted) = self.session.commit_selection_range
            && persisted.1 < mapped.len()
            && persisted.0 <= persisted.1
        {
            range = persisted;
        } else if self.commit_selection_start == CommitSelectionStart::Oldest {
            if let Some(oldest) =
                Self::initial_commit_range(CommitSelectionStart::Oldest, mapped.len())
            {
                range = oldest;
            }
        } else if self.session.commit_selection_range.is_none()
            && let Some(selection) = since_last_review.as_ref()
        {
            if let Some(selected_range) = selection.range {
                range = selected_range;
                auto_scoped_since_last_review = true;
            }
            since_last_review_message = Some(selection.message.clone());
        }

        self.commit_selection_range = Some(range);
        self.review_commits = mapped;

        if let Some(message) = since_last_review_message {
            if auto_scoped_since_last_review
                && Self::is_strict_commit_selection(Some(range), self.pr_commits.len())
            {
                self.focused_panel = FocusedPanel::CommitSelector;
                self.commit_list_cursor = self.pr_commits.len().saturating_sub(1);
                self.commit_list_scroll_offset = self.commit_list_cursor.saturating_sub(5);
            }
            return Some(message);
        }
        None
    }

    pub fn commit_list_idx_at_screen_row(&self, screen_row: u16) -> Option<usize> {
        let inner = self.commit_list_inner_area?;
        if screen_row < inner.y || screen_row >= inner.y + inner.height {
            return None;
        }
        let rel = (screen_row - inner.y) as usize;
        // Display row: the scroll offset is kept in display space for the
        // inline pane (see the renderer), so this is the on-screen row index.
        let display_idx = self.commit_list_scroll_offset + rel;
        match self.input_mode {
            InputMode::CommitSelect => {
                let total = self.visible_commit_count + usize::from(self.can_show_more_commits());
                (display_idx < total).then_some(display_idx)
            }
            // Inline commit selector: map the display row back to a data index
            // into `review_commits` (newest-first storage) for ascending order.
            _ => {
                let total = self.review_commits.len();
                (display_idx < total).then_some(self.commit_data_index(display_idx))
            }
        }
    }

    /// Whether the inline commit selector renders oldest-first. Presentation
    /// only — `review_commits` is always stored newest-first.
    pub fn commits_ascending(&self) -> bool {
        matches!(self.commit_order, CommitOrder::Ascending)
    }

    /// Convert between a data index into `review_commits` and its on-screen
    /// display row (and back — the mapping is its own inverse). Identity in
    /// descending order; mirrored (`n-1-i`) in ascending order.
    pub fn commit_data_index(&self, index: usize) -> usize {
        let n = self.review_commits.len();
        if self.commits_ascending() && n > 0 {
            n - 1 - index.min(n - 1)
        } else {
            index
        }
    }

    /// Status-bar description of the current inline commit selection, or `None`
    /// when the whole range is selected (the caller shows the plain total).
    /// A single selected commit reports its 1-based display position — so the
    /// value changes as `(` / `)` cycle — while a multi-commit subrange reports
    /// the selected count.
    pub fn commit_selection_summary(&self) -> Option<String> {
        let total = self.review_commits.len();
        let (start, end) = self.commit_selection_range?;
        let selected = end.saturating_sub(start) + 1;
        if total <= 1 || selected >= total {
            return None;
        }
        if start == end {
            Some(format!(
                "commit {}/{}",
                self.commit_data_index(start) + 1,
                total
            ))
        } else {
            Some(format!("{selected} of {total} commits"))
        }
    }

    /// Open the review target selector on a specific tab.
    ///
    /// `Local` loads the recent-commits list (same as the historical commit
    /// selector). `PullRequests` switches the tab; the actual fetch is
    /// triggered lazily through `on_target_tab_entered`.
    pub fn enter_target_selector(&mut self, initial_tab: TargetTab) -> Result<()> {
        // Save inline selection state if we have review commits
        if !self.review_commits.is_empty() {
            self.saved_inline_selection = self.commit_selection_range;
        }

        let highlighter = self.theme.syntax_highlighter();
        let change_status = Self::get_change_status_with_ignore(
            self.vcs.as_ref(),
            &self.vcs_info.root_path,
            highlighter,
            self.path_filter.as_deref(),
        )?;
        let has_staged_changes = change_status.staged;
        let has_unstaged_changes = change_status.unstaged;

        let commit_limit = initial_commit_limit();
        let commits = self.vcs.get_recent_commits(0, commit_limit)?;
        let no_local_targets = commits.is_empty() && !has_staged_changes && !has_unstaged_changes;
        // Allow opening the selector on the Pull Requests tab even when there
        // are no local commits or changes — the PR tab is the user's reason
        // for being here.
        if no_local_targets && initial_tab == TargetTab::Local {
            self.set_message("No commits or staged/unstaged changes found");
            return Ok(());
        }

        // Check if there might be more commits
        self.has_more_commit = commits.len() >= commit_limit;
        self.commit_list = commits;
        if has_staged_changes {
            self.commit_list.insert(0, Self::staged_commit_entry());
        }
        if has_unstaged_changes {
            self.commit_list.insert(0, Self::unstaged_commit_entry());
        }
        self.commit_list_cursor = 0;
        self.commit_list_scroll_offset = 0;
        self.commit_selection_range = None;
        self.commit_picks.clear();
        self.visible_commit_count = self.commit_list.len();
        self.input_mode = InputMode::CommitSelect;

        // Reset the PR tab to Idle each time the selector is opened so the
        // fetch happens lazily on first visit.
        self.pr_tab = PullRequestsTab::new(self.forge_repository.clone());
        self.pr_filter_draft = None;
        self.pr_load_rx = None;

        self.target_tab = initial_tab;
        if initial_tab == TargetTab::PullRequests {
            self.on_target_tab_entered();
        }
        Ok(())
    }

    pub fn exit_commit_select_mode(&mut self) -> Result<()> {
        self.input_mode = InputMode::Normal;

        // If we have review commits, restore the inline selector state
        if !self.review_commits.is_empty() {
            self.commit_list = self.review_commits.clone();
            self.commit_selection_range = self.saved_inline_selection;
            self.commit_list_cursor = 0;
            self.commit_list_scroll_offset = 0;
            self.visible_commit_count = self.review_commits.len();
            self.has_more_commit = false;
            self.saved_inline_selection = None;

            // Reload diff for the restored selection
            if self.commit_selection_range.is_some() {
                self.reload_inline_selection_for_source()?;
            }
            return Ok(());
        }

        // If we were viewing commits, try to go back to working tree
        if matches!(
            self.diff_source,
            DiffSource::CommitRange(_) | DiffSource::StagedUnstagedAndCommits(_)
        ) {
            let highlighter = self.theme.syntax_highlighter();
            match Self::get_working_tree_diff_with_ignore(
                self.vcs.as_ref(),
                &self.vcs_info.root_path,
                highlighter,
                self.path_filter.as_deref(),
            ) {
                Ok(diff_files) => {
                    self.diff_files = diff_files;
                    self.diff_source = DiffSource::StagedAndUnstaged;

                    // Update session for new files
                    for file in &self.diff_files {
                        self.session.add_diff_file(file);
                    }

                    self.sort_files_by_directory(true);
                    self.expand_all_dirs();
                }
                Err(_) => {
                    self.set_message("No staged or unstaged changes");
                }
            }
        }

        Ok(())
    }

    /// Switch to the next/previous tab in the review target selector.
    /// With only two tabs, forward and reverse are equivalent; the `_forward`
    /// arg is kept so callers can pass the natural direction without a cast.
    /// Triggers the lazy PR fetch the first time the PR tab is entered.
    pub fn cycle_target_tab(&mut self, _forward: bool) {
        let next = match self.target_tab {
            TargetTab::Local => TargetTab::PullRequests,
            TargetTab::PullRequests => TargetTab::Local,
        };
        self.target_tab = next;
        if next == TargetTab::PullRequests {
            self.on_target_tab_entered();
        } else {
            // Returning to Local: clear any half-typed PR filter draft.
            self.pr_filter_draft = None;
        }
    }

    /// Entry-point hook called when the PR tab becomes visible.
    /// Triggers the first network call lazily.
    fn on_target_tab_entered(&mut self) {
        if let Some((repo, scope)) = self.pr_tab.start_initial_load() {
            let override_repo = self.repo_url_override.clone();
            let skip_resolution = self.canonical_resolved;
            self.spawn_pr_initial_load(repo, override_repo, skip_resolution, scope);
        }
    }

    /// Whether the inline commit selector panel should be displayed.
    pub fn has_inline_commit_selector(&self) -> bool {
        self.show_commit_selector && self.has_review_commits()
    }

    /// Whether the current review has a multi-commit selection that `(` / `)`
    /// can cycle through. Unlike [`has_inline_commit_selector`], this ignores
    /// pane visibility so cycling still works while the pane is hidden (the
    /// status bar shows the `{n}/{total} commits` count as feedback).
    pub fn has_review_commits(&self) -> bool {
        self.review_commits.len() > 1 && !matches!(&self.diff_source, DiffSource::WorkingTree)
    }

    /// Toggle the inline commit selector's visibility. When hiding it while it
    /// is focused, move focus back to the diff so input keeps flowing.
    pub fn toggle_commit_selector(&mut self) {
        let visible = !self.show_commit_selector;
        self.show_commit_selector = visible;
        if !visible && self.focused_panel == FocusedPanel::CommitSelector {
            self.focused_panel = FocusedPanel::Diff;
        }
        let status = if visible { "visible" } else { "hidden" };
        self.set_message(format!("Commit selector: {status}"));
    }

    // Commit selection methods

    pub fn commit_select_up(&mut self) {
        if self.commit_list_cursor > 0 {
            self.commit_list_cursor -= 1;
            // Scroll up if cursor goes above visible area
            if self.commit_list_cursor < self.commit_list_scroll_offset {
                self.commit_list_scroll_offset = self.commit_list_cursor;
            }
        }
    }

    pub fn commit_select_down(&mut self) {
        let max_cursor = if self.can_show_more_commits() {
            self.visible_commit_count
        } else {
            self.visible_commit_count.saturating_sub(1)
        };

        if self.commit_list_cursor < max_cursor {
            self.commit_list_cursor += 1;
            // Scroll down if cursor goes below visible area
            if self.commit_list_viewport_height > 0
                && self.commit_list_cursor
                    >= self.commit_list_scroll_offset + self.commit_list_viewport_height
            {
                self.commit_list_scroll_offset =
                    self.commit_list_cursor - self.commit_list_viewport_height + 1;
            }
        }
    }

    /// Space in a selector. The fullscreen selector toggles the cursor row
    /// in the pick set — picks may be non-contiguous, so single commits
    /// from other branches can be pulled in without dragging their
    /// neighbours along. Newly picking a row advances the cursor so
    /// repeated Space still sweeps a run. The inline selector keeps the
    /// contiguous-range behavior (PR subsets need contiguity).
    pub fn toggle_commit_selection_and_advance(&mut self) {
        if self.input_mode == InputMode::CommitSelect {
            let cursor = self.commit_list_cursor;
            if cursor >= self.commit_list.len() {
                return;
            }
            if !self.commit_picks.insert(cursor) {
                self.commit_picks.remove(&cursor);
                return;
            }
            self.commit_select_down();
            return;
        }

        let cursor = self.commit_list_cursor;
        let was_selected = self.is_commit_selected(cursor);
        self.toggle_commit_selection();
        let now_selected = self.is_commit_selected(cursor);
        if was_selected || !now_selected {
            return;
        }
        if let Some((_, end)) = self.commit_selection_range {
            while self.commit_list_cursor <= end {
                let before = self.commit_list_cursor;
                self.commit_select_down();
                if self.commit_list_cursor == before {
                    return;
                }
            }
        }
    }

    // Selector history operations (cherry-pick / rebase)

    /// Commits the selector action applies to: the picked rows when any
    /// exist, else the cursor row — pseudo rows excluded, oldest first
    /// (picks iterate descending index; larger index = older commit).
    pub(in crate::app) fn selector_action_commit_ids(&self) -> Vec<String> {
        let indices: Vec<usize> = if self.commit_picks.is_empty() {
            vec![self.commit_list_cursor]
        } else {
            self.commit_picks.iter().rev().copied().collect()
        };
        indices
            .into_iter()
            .filter_map(|i| self.commit_list.get(i))
            .filter(|c| !Self::is_special_commit(c))
            .map(|c| c.id.clone())
            .collect()
    }

    /// Cherry-pick the selected commits (or the cursor commit) onto the
    /// current branch. Conflicts open the merge view automatically.
    pub fn selector_cherry_pick(&mut self) {
        let ids = self.selector_action_commit_ids();
        if ids.is_empty() {
            self.set_message("Move the cursor onto a commit to cherry-pick");
            return;
        }
        match self.vcs.cherry_pick(&ids) {
            Ok(()) => {
                self.refresh_commit_selector_rows();
                self.set_message(format!(
                    "Cherry-picked {} commit{} onto {}",
                    ids.len(),
                    if ids.len() == 1 { "" } else { "s" },
                    self.vcs_info.branch_name.as_deref().unwrap_or("HEAD"),
                ));
            }
            Err(err) => self.handle_history_op_failure("cherry-pick", err),
        }
    }

    /// Rebase the current branch onto the commit under the cursor.
    /// Conflicts open the merge view automatically.
    pub fn selector_rebase_onto_cursor(&mut self) {
        let commit_id = self
            .commit_list
            .get(self.commit_list_cursor)
            .filter(|c| !Self::is_special_commit(c))
            .map(|c| c.id.clone());
        let Some(commit_id) = commit_id else {
            self.set_message("Move the cursor onto a commit to rebase onto");
            return;
        };
        match self.vcs.rebase_onto(&commit_id) {
            Ok(()) => {
                self.refresh_commit_selector_rows();
                self.set_message(format!(
                    "Rebased {} onto {}",
                    self.vcs_info.branch_name.as_deref().unwrap_or("HEAD"),
                    &commit_id[..7.min(commit_id.len())],
                ));
            }
            Err(err) => self.handle_history_op_failure("rebase", err),
        }
    }

    /// Squash the selected commits (space-selected range) into one commit.
    pub fn selector_squash(&mut self) {
        let ids = self.selector_action_commit_ids();
        if ids.len() < 2 {
            self.set_message("Select at least two commits to squash (space, then s)");
            return;
        }
        match self.vcs.squash_commits(&ids) {
            Ok(()) => {
                let count = ids.len();
                self.refresh_commit_selector_rows();
                self.set_message(format!("Squashed {count} commits into one"));
            }
            Err(err) => {
                let err = err.to_string();
                self.set_error(format!("Squash failed: {}", err.trim()));
            }
        }
    }

    /// A failed history operation either stopped on conflicts (open the
    /// merge view so the user can resolve, then `--continue` from a shell)
    /// or genuinely failed (surface the VCS error).
    fn handle_history_op_failure(&mut self, op: &str, err: TuicrError) {
        if let Ok(Some(_)) = self.vcs.get_merge_conflicts() {
            if let Err(open_err) = self.open_merge_view(InputMode::CommitSelect) {
                self.set_error(format!("{op} stopped on conflicts: {open_err}"));
                return;
            }
            self.set_message(format!(
                "{op} stopped on conflicts \u{2014} resolve + w, then `git {op} --continue`"
            ));
        } else {
            let err = err.to_string();
            let err = err.trim();
            self.set_error(format!("{op} failed: {err}"));
        }
    }

    /// Re-probe the repository and rebuild the selector rows (pseudo rows
    /// included). Used after history operations and when leaving the merge
    /// view, where HEAD or the conflict state may have changed.
    fn refresh_commit_selector_rows(&mut self) {
        let commit_limit = initial_commit_limit();
        let commits = self
            .vcs
            .get_recent_commits(0, commit_limit)
            .unwrap_or_default();
        let highlighter = self.theme.syntax_highlighter();
        let change_status = Self::get_change_status_with_ignore(
            self.vcs.as_ref(),
            &self.vcs_info.root_path,
            highlighter,
            self.path_filter.as_deref(),
        )
        .unwrap_or_default();
        let merge_conflicts = self.vcs.get_merge_conflicts().ok().flatten();

        self.has_more_commit = commits.len() >= commit_limit;
        let mut commit_list = commits;
        if change_status.staged {
            commit_list.insert(0, Self::staged_commit_entry());
        }
        if change_status.unstaged {
            commit_list.insert(0, Self::unstaged_commit_entry());
        }
        if let Some(info) = &merge_conflicts {
            commit_list.insert(0, Self::merge_commit_entry(info));
        }
        self.commit_list = commit_list;
        self.commit_list_cursor = 0;
        self.commit_list_scroll_offset = 0;
        self.commit_selection_range = None;
        self.commit_picks.clear();
        self.visible_commit_count = self.commit_list.len();
    }

    // Merge conflict view methods

    /// Open the fullscreen merge conflict screen. Shows a message and stays
    /// in the current mode when the repository has no conflicted paths.
    pub fn open_merge_view(&mut self, return_mode: InputMode) -> Result<()> {
        let Some(info) = self.vcs.get_merge_conflicts()? else {
            self.set_message("No merge conflicts detected");
            return Ok(());
        };

        let root = self.vcs_info.root_path.clone();
        let files: Vec<MergeFileState> = info
            .paths
            .iter()
            .map(|path| match std::fs::read_to_string(root.join(path)) {
                Ok(content) => {
                    let file = crate::vcs::parse_conflict_file(&content);
                    let resolutions = vec![ConflictResolution::Unresolved; file.conflict_count()];
                    MergeFileState {
                        path: path.clone(),
                        file,
                        resolutions,
                        dirty: false,
                        staged: false,
                        load_error: None,
                    }
                }
                Err(err) => MergeFileState {
                    path: path.clone(),
                    file: ConflictFile::default(),
                    resolutions: Vec::new(),
                    dirty: false,
                    staged: false,
                    load_error: Some(err.to_string()),
                },
            })
            .collect();

        let mut view = MergeView {
            files,
            current_file: 0,
            current_conflict: 0,
            scroll_offset: 0,
            viewport_height: 0,
            return_mode,
            discard_armed: false,
            ours_label: info.ours_label,
            theirs_label: info.theirs_label,
            ours_commit: info.ours_commit,
            theirs_commit: info.theirs_commit,
        };
        view.scroll_to_current_conflict();
        self.merge_view = Some(view);
        self.input_mode = InputMode::MergeConflict;
        Ok(())
    }

    /// Esc from the merge view. Unsaved resolutions require a second Esc.
    pub fn close_merge_view(&mut self) {
        let Some(view) = self.merge_view.as_mut() else {
            self.input_mode = InputMode::Normal;
            return;
        };
        if view.files.iter().any(|f| f.dirty) && !view.discard_armed {
            view.discard_armed = true;
            self.set_message("Unsaved conflict resolutions \u{2014} press Esc again to discard");
            return;
        }
        let return_mode = view.return_mode;
        self.input_mode = return_mode;
        self.merge_view = None;
        if return_mode == InputMode::CommitSelect {
            self.refresh_commit_selector_rows();
        }
    }

    /// Any key other than Esc cancels a pending discard warning.
    pub fn merge_disarm_discard(&mut self) {
        if let Some(view) = self.merge_view.as_mut() {
            view.discard_armed = false;
        }
    }

    pub fn merge_half_page(&self) -> usize {
        self.merge_view
            .as_ref()
            .map(|v| (v.viewport_height / 2).max(1))
            .unwrap_or(1)
    }

    pub fn merge_full_page(&self) -> usize {
        self.merge_view
            .as_ref()
            .map(|v| v.viewport_height.max(1))
            .unwrap_or(1)
    }

    pub fn merge_scroll_down(&mut self, n: usize) {
        if let Some(view) = self.merge_view.as_mut() {
            view.scroll_offset = (view.scroll_offset + n).min(view.max_scroll());
        }
    }

    pub fn merge_scroll_up(&mut self, n: usize) {
        if let Some(view) = self.merge_view.as_mut() {
            view.scroll_offset = view.scroll_offset.saturating_sub(n);
        }
    }

    pub fn merge_scroll_to_top(&mut self) {
        if let Some(view) = self.merge_view.as_mut() {
            view.scroll_offset = 0;
        }
    }

    pub fn merge_scroll_to_bottom(&mut self) {
        if let Some(view) = self.merge_view.as_mut() {
            view.scroll_offset = view.max_scroll();
        }
    }

    pub fn merge_next_conflict(&mut self) {
        let message = {
            let Some(view) = self.merge_view.as_mut() else {
                return;
            };
            let count = view
                .current_file_state()
                .map(|f| f.file.conflict_count())
                .unwrap_or(0);
            if count == 0 {
                Some("No conflicts in this file".to_string())
            } else if view.current_conflict + 1 >= count {
                Some("Already at the last conflict".to_string())
            } else {
                view.current_conflict += 1;
                view.scroll_to_current_conflict();
                None
            }
        };
        if let Some(msg) = message {
            self.set_message(msg);
        }
    }

    pub fn merge_prev_conflict(&mut self) {
        let message = {
            let Some(view) = self.merge_view.as_mut() else {
                return;
            };
            let count = view
                .current_file_state()
                .map(|f| f.file.conflict_count())
                .unwrap_or(0);
            if count == 0 {
                Some("No conflicts in this file".to_string())
            } else if view.current_conflict == 0 {
                Some("Already at the first conflict".to_string())
            } else {
                view.current_conflict -= 1;
                view.scroll_to_current_conflict();
                None
            }
        };
        if let Some(msg) = message {
            self.set_message(msg);
        }
    }

    pub fn merge_next_file(&mut self) {
        if let Some(view) = self.merge_view.as_mut()
            && view.current_file + 1 < view.files.len()
        {
            view.current_file += 1;
            view.current_conflict = 0;
            view.scroll_offset = 0;
            view.scroll_to_current_conflict();
        }
    }

    pub fn merge_prev_file(&mut self) {
        if let Some(view) = self.merge_view.as_mut()
            && view.current_file > 0
        {
            view.current_file -= 1;
            view.current_conflict = 0;
            view.scroll_offset = 0;
            view.scroll_to_current_conflict();
        }
    }

    /// Apply a resolution to the focused conflict, then advance to the next
    /// unresolved conflict in the file so repeated picks sweep forward.
    pub fn merge_resolve_current(&mut self, resolution: ConflictResolution) {
        let message = {
            let Some(view) = self.merge_view.as_mut() else {
                return;
            };
            let current_conflict = view.current_conflict;
            let Some(file) = view.files.get_mut(view.current_file) else {
                return;
            };
            if file.resolutions.is_empty() {
                Some("No conflicts to resolve in this file".to_string())
            } else {
                let idx = current_conflict.min(file.resolutions.len() - 1);
                if file.resolutions[idx] != resolution {
                    file.resolutions[idx] = resolution;
                    file.dirty = true;
                    file.staged = false;
                }
                if resolution == ConflictResolution::Unresolved {
                    None
                } else if let Some(next) = file
                    .resolutions
                    .iter()
                    .enumerate()
                    .skip(idx + 1)
                    .find(|(_, r)| matches!(r, ConflictResolution::Unresolved))
                    .map(|(i, _)| i)
                {
                    view.current_conflict = next;
                    view.scroll_to_current_conflict();
                    None
                } else if file.unresolved_count() == 0 {
                    Some("All conflicts resolved \u{00b7} press w to write".to_string())
                } else {
                    None
                }
            }
        };
        if let Some(msg) = message {
            self.set_message(msg);
        }
    }

    /// Write the current file's resolutions to the working tree; stage it
    /// when every conflict is resolved (the VCS "mark resolved" gesture).
    pub fn merge_save_current_file(&mut self) -> Result<()> {
        let root = self.vcs_info.root_path.clone();
        let load_error = self
            .merge_view
            .as_ref()
            .and_then(MergeView::current_file_state)
            .and_then(|f| f.load_error.clone());
        if let Some(err) = load_error {
            self.set_error(format!("Cannot write this file: {err}"));
            return Ok(());
        }

        let (path, unresolved) = {
            let Some(view) = self.merge_view.as_mut() else {
                return Ok(());
            };
            let Some(file) = view.files.get_mut(view.current_file) else {
                return Ok(());
            };
            let content = apply_resolutions(&file.file, &file.resolutions);
            std::fs::write(root.join(&file.path), content)?;
            file.dirty = false;
            (file.path.clone(), file.unresolved_count())
        };

        if unresolved > 0 {
            self.set_message(format!(
                "Wrote {} \u{00b7} {unresolved} conflict{} remaining",
                path.display(),
                if unresolved == 1 { "" } else { "s" }
            ));
            return Ok(());
        }

        match self.vcs.stage_file(&path) {
            Ok(()) => {
                if let Some(view) = self.merge_view.as_mut()
                    && let Some(file) = view.files.get_mut(view.current_file)
                {
                    file.staged = true;
                }
                if self.merge_all_resolved_and_staged() {
                    // Hand off to the Y/N prompt; the dialog renders on
                    // top of the merge view.
                    self.enter_confirm_mode(ConfirmAction::ContinueMergeOperation);
                } else {
                    self.set_message(format!(
                        "Wrote {} \u{00b7} resolved and staged",
                        path.display()
                    ));
                }
            }
            Err(err) => {
                self.set_error(format!(
                    "Wrote {} but staging failed: {err}",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    /// Hand the current conflicted file to `$EDITOR`: write the picks made
    /// so far (unresolved conflicts keep their markers), then queue the
    /// editor at the focused conflict's line. The main loop performs the
    /// terminal hand-off and calls [`Self::merge_reload_current_file`]
    /// when the editor exits.
    pub fn merge_edit_current_file(&mut self) {
        enum Outcome {
            Queue(EditorTarget),
            WriteFailed(String),
            NotEditable,
        }

        let root = self.vcs_info.root_path.clone();
        let outcome = {
            let Some(view) = self.merge_view.as_mut() else {
                return;
            };
            let current_conflict = view.current_conflict;
            let Some(file) = view.files.get_mut(view.current_file) else {
                return;
            };
            if file.load_error.is_some() {
                Outcome::NotEditable
            } else {
                let content = crate::vcs::apply_resolutions(&file.file, &file.resolutions);
                match std::fs::write(root.join(&file.path), content) {
                    Ok(()) => {
                        file.dirty = false;
                        file.staged = false;
                        let line = crate::vcs::conflict::conflict_output_line(
                            &file.file,
                            &file.resolutions,
                            current_conflict,
                        );
                        Outcome::Queue(EditorTarget {
                            path: root.join(&file.path),
                            line: line.map(|l| l as u32),
                        })
                    }
                    Err(err) => Outcome::WriteFailed(format!("{}: {err}", file.path.display())),
                }
            }
        };
        match outcome {
            Outcome::Queue(target) => self.pending_editor_target = Some(target),
            Outcome::WriteFailed(err) => self.set_error(format!("Cannot write {err}")),
            Outcome::NotEditable => self.set_error("Cannot edit this file"),
        }
    }

    /// Re-parse the current file after an external edit: markers still
    /// present become unresolved conflicts again; everything else is
    /// settled content.
    pub fn merge_reload_current_file(&mut self) {
        let root = self.vcs_info.root_path.clone();
        let path = self
            .merge_view
            .as_ref()
            .and_then(MergeView::current_file_state)
            .map(|f| f.path.clone());
        let Some(path) = path else {
            return;
        };
        match std::fs::read_to_string(root.join(&path)) {
            Ok(content) => {
                self.merge_apply_reloaded_content(&content);
                let remaining = self
                    .merge_view
                    .as_ref()
                    .and_then(MergeView::current_file_state)
                    .map(|f| f.file.conflict_count())
                    .unwrap_or(0);
                if remaining == 0 {
                    self.set_message(format!(
                        "Reloaded {} \u{00b7} no conflicts remain \u{00b7} press w to stage",
                        path.display()
                    ));
                } else {
                    self.set_message(format!(
                        "Reloaded {} \u{00b7} {remaining} conflict{} remaining",
                        path.display(),
                        if remaining == 1 { "" } else { "s" }
                    ));
                }
            }
            Err(err) => self.set_error(format!("Reload failed: {err}")),
        }
    }

    /// Pure half of [`Self::merge_reload_current_file`], split for tests.
    pub(in crate::app) fn merge_apply_reloaded_content(&mut self, content: &str) {
        let Some(view) = self.merge_view.as_mut() else {
            return;
        };
        let Some(file) = view.files.get_mut(view.current_file) else {
            return;
        };
        file.file = crate::vcs::parse_conflict_file(content);
        file.resolutions = vec![ConflictResolution::Unresolved; file.file.conflict_count()];
        file.dirty = false;
        file.staged = false;
        view.current_conflict = view
            .current_conflict
            .min(file.resolutions.len().saturating_sub(1));
        view.scroll_to_current_conflict();
    }

    /// True once every file in the merge view is conflict-free, written,
    /// and staged — confirmed against the VCS, which is authoritative
    /// (paths conflicted outside this view would still block).
    pub(in crate::app) fn merge_all_resolved_and_staged(&self) -> bool {
        let view_done = self.merge_view.as_ref().is_some_and(|view| {
            view.files.iter().all(|f| {
                f.load_error.is_none() && f.unresolved_count() == 0 && !f.dirty && f.staged
            })
        });
        view_done && matches!(self.vcs.get_merge_conflicts(), Ok(None))
    }

    /// Yes-path of the commit prompt: commit/continue the operation, then
    /// leave the merge view. On failure, return to the merge view with the
    /// error surfaced.
    pub fn confirm_continue_merge_operation(&mut self) {
        match self.vcs.continue_merge_operation() {
            Ok(op) => {
                self.close_merge_view();
                self.set_message(format!("\u{2713} {op} committed"));
            }
            Err(err) => {
                self.input_mode = InputMode::MergeConflict;
                self.set_error(format!("Commit failed: {err}"));
            }
        }
    }

    // Check if cursor is on the commit expand row
    pub fn is_on_expand_row(&self) -> bool {
        self.can_show_more_commits() && self.commit_list_cursor == self.visible_commit_count
    }

    pub fn can_show_more_commits(&self) -> bool {
        self.visible_commit_count < self.commit_list.len() || self.has_more_commit
    }

    // Expand the commit list to show more commits
    pub fn expand_commit(&mut self) -> Result<()> {
        if !self.grow_commit_list()? {
            self.set_message("No more commits");
        }
        Ok(())
    }

    /// Silent page-grow shared by the expand row and viewport filling.
    /// Returns whether anything new became visible.
    fn grow_commit_list(&mut self) -> Result<bool> {
        if self.visible_commit_count < self.commit_list.len() {
            self.visible_commit_count =
                (self.visible_commit_count + self.commit_page_size).min(self.commit_list.len());
            return Ok(true);
        }

        if !self.has_more_commit {
            return Ok(false);
        }

        let offset = self.loaded_history_commit_count();
        let limit = self.commit_page_size;

        let new_commits = self.vcs.get_recent_commits(offset, limit)?;

        if new_commits.len() < limit {
            self.has_more_commit = false;
        }
        if new_commits.is_empty() {
            return Ok(false);
        }

        self.commit_list.extend(new_commits);
        self.visible_commit_count = self.commit_list.len();
        Ok(true)
    }

    /// Load enough commits to fill a selector body of `terminal_rows`
    /// total height. Called on terminal resize so growing the window
    /// (e.g. full-screening) reveals more history instead of dead space.
    pub fn fill_commit_viewport(&mut self, terminal_rows: usize) {
        if self.input_mode != InputMode::CommitSelect {
            return;
        }
        let capacity = commit_limit_for_rows(terminal_rows);
        let mut guard = 0usize;
        while self.visible_commit_count < capacity
            && self.can_show_more_commits()
            && guard < MAX_INITIAL_COMMITS
        {
            match self.grow_commit_list() {
                Ok(true) => guard += 1,
                _ => break,
            }
        }
    }

    pub fn toggle_commit_selection(&mut self) {
        let cursor = self.commit_list_cursor;
        if cursor >= self.commit_list.len() {
            return;
        }

        match self.commit_selection_range {
            None => {
                // No selection yet - select just this commit
                self.commit_selection_range = Some((cursor, cursor));
            }
            Some((start, end)) => {
                let all_commits_selected =
                    self.commit_list.len() > 1 && start == 0 && end == self.commit_list.len() - 1;
                if all_commits_selected {
                    self.commit_selection_range = Some((cursor, cursor));
                    return;
                }

                if cursor >= start && cursor <= end {
                    // Cursor is within the range - shrink or deselect
                    if start == end {
                        // Only one commit selected, deselect all
                        self.commit_selection_range = None;
                    } else if cursor == start {
                        // At start edge - shrink from start
                        self.commit_selection_range = Some((start + 1, end));
                    } else if cursor == end {
                        // At end edge - shrink from end
                        self.commit_selection_range = Some((start, end - 1));
                    } else {
                        // In the middle - deselect cursor and everything after it
                        self.commit_selection_range = Some((start, cursor - 1));
                    }
                } else {
                    // Cursor is outside the range - extend to include it
                    let new_start = start.min(cursor);
                    let new_end = end.max(cursor);
                    self.commit_selection_range = Some((new_start, new_end));
                }
            }
        }
    }

    /// Check if a commit at the given index is selected. The fullscreen
    /// selector renders the pick set; the inline selector keeps the
    /// contiguous range.
    pub fn is_commit_selected(&self, index: usize) -> bool {
        if self.input_mode == InputMode::CommitSelect {
            return self.commit_picks.contains(&index);
        }
        match self.commit_selection_range {
            Some((start, end)) => index >= start && index <= end,
            None => false,
        }
    }

    /// The set of commit SHAs currently selected in the inline commit
    /// selector. `None` when there is no selector / no selection (working
    /// tree, staged, etc.) — in that case every comment is visible. When
    /// the full range is selected the set contains every commit SHA, so
    /// all comments (including single-commit-scoped ones) show. When a
    /// strict subset is selected, only comments whose `commit_id` is in
    /// the set (or `None`) are visible.
    pub(in crate::app) fn selected_commit_set(&self) -> Option<std::collections::HashSet<String>> {
        let (start, end) = self.commit_selection_range?;
        if start > end || self.review_commits.is_empty() {
            return Some(std::collections::HashSet::new());
        }
        let end = end.min(self.review_commits.len().saturating_sub(1));
        let set: std::collections::HashSet<String> = (start..=end)
            .filter_map(|i| self.review_commits.get(i))
            .filter(|c| !Self::is_special_commit(c))
            .map(|c| c.id.clone())
            .collect();
        Some(set)
    }

    /// Whether a comment should be visible given the current commit
    /// selection. A comment with `commit_id == None` (legacy or made
    /// against the full cumulative diff) is always visible. Otherwise it
    /// is visible only when its commit is in the selected set.
    ///
    /// Allocates the commit set on every call. Callers in hot paths
    /// (per-comment-per-frame renderers, height calculation) should
    /// compute [`selected_commit_set`] once and use
    /// [`comment_visible_with`] instead.
    pub fn comment_visible(&self, comment: &crate::model::Comment) -> bool {
        Self::comment_visible_with(comment, self.selected_commit_set().as_ref())
    }

    /// Pure visibility check against a precomputed commit set — no
    /// allocation. `set == None` means "no selector", so every comment
    /// is visible. This is the shared predicate all filtering sites
    /// should converge on so height math and rendering never drift.
    pub fn comment_visible_with(
        comment: &crate::model::Comment,
        commit_set: Option<&std::collections::HashSet<String>>,
    ) -> bool {
        match (&comment.commit_id, commit_set) {
            (None, _) => true,
            (Some(_), None) => true,
            (Some(sha), Some(set)) => set.contains(sha),
        }
    }

    /// The single commit SHA to stamp on a *new* comment, when the inline
    /// selector shows exactly one commit. `None` otherwise (full range,
    /// multi-commit subset, or no selector) — those comments get
    /// `commit_id = None` so they stay visible across selections.
    pub(in crate::app) fn commit_id_for_new_comment(&self) -> Option<String> {
        let (start, end) = self.commit_selection_range?;
        if start != end {
            return None;
        }
        self.review_commits
            .get(start)
            .filter(|c| !Self::is_special_commit(c))
            .map(|c| c.id.clone())
    }

    pub(in crate::app) fn set_pr_last_reviewed_commit_from_metadata(
        &mut self,
        commits: &[crate::forge::traits::PullRequestCommit],
        review_metadata: &crate::forge::traits::PullRequestReviewMetadata,
    ) {
        self.pr_last_reviewed_commit_index = if commits.len() > 1 {
            commits_since_last_review_selection(commits, review_metadata)
                .map(|selection| selection.reviewed_index)
        } else {
            None
        };
    }

    pub(in crate::app) fn mark_pr_commits_reviewed_through(&mut self, commit_id: &str) {
        if !matches!(&self.diff_source, DiffSource::PullRequest(_)) {
            return;
        }
        if let Some(index) = self
            .pr_commits
            .iter()
            .position(|commit| commit.oid == commit_id)
        {
            self.pr_last_reviewed_commit_index = Some(index);
        }
    }

    /// Check if a PR commit is covered by the viewer's latest submitted review.
    pub fn is_commit_reviewed_by_viewer(&self, index: usize) -> bool {
        matches!(&self.diff_source, DiffSource::PullRequest(_))
            && index < self.review_commits.len()
            && self
                .pr_last_reviewed_commit_index
                .is_some_and(|reviewed_index| index >= reviewed_index)
    }

    /// Cycle inline commit selector to the next individual commit (`)` key).
    /// all → last, i → i+1, last → all
    pub fn cycle_commit_next(&mut self) {
        if self.review_commits.is_empty() {
            return;
        }
        let n = self.review_commits.len();
        let all_selected = Some((0, n - 1));

        if self.commit_selection_range == all_selected {
            // all → last
            self.commit_selection_range = Some((n - 1, n - 1));
            self.commit_list_cursor = n - 1;
        } else if let Some((i, j)) = self.commit_selection_range {
            if i == j {
                // Single commit selected
                if i == n - 1 {
                    // last → all
                    self.commit_selection_range = all_selected;
                } else {
                    // i → i+1
                    self.commit_selection_range = Some((i + 1, i + 1));
                    self.commit_list_cursor = i + 1;
                }
            } else {
                // Multi-commit subrange → select last of that range
                self.commit_selection_range = Some((j, j));
                self.commit_list_cursor = j;
            }
        } else {
            // None selected → select all
            self.commit_selection_range = all_selected;
        }
    }

    /// Cycle inline commit selector to the previous individual commit (`(` key).
    /// all → first, i → i-1, first → all
    pub fn cycle_commit_prev(&mut self) {
        if self.review_commits.is_empty() {
            return;
        }
        let n = self.review_commits.len();
        let all_selected = Some((0, n - 1));

        if self.commit_selection_range == all_selected {
            // all → first
            self.commit_selection_range = Some((0, 0));
            self.commit_list_cursor = 0;
        } else if let Some((i, j)) = self.commit_selection_range {
            if i == j {
                // Single commit selected
                if i == 0 {
                    // first → all
                    self.commit_selection_range = all_selected;
                } else {
                    // i → i-1
                    self.commit_selection_range = Some((i - 1, i - 1));
                    self.commit_list_cursor = i - 1;
                }
            } else {
                // Multi-commit subrange → select first of that range
                self.commit_selection_range = Some((i, i));
                self.commit_list_cursor = i;
            }
        } else {
            // None selected → select all
            self.commit_selection_range = all_selected;
        }
    }

    pub fn confirm_commit_selection(&mut self) -> Result<()> {
        let selection = match self.commit_selection_range {
            Some((start, end)) => format!(
                "range={start}..={end}, rows={}",
                end.saturating_sub(start) + 1
            ),
            None => "range=none, rows=0".to_string(),
        };
        crate::profile::time_with(
            "commit_select.confirm_selection",
            || self.confirm_commit_selection_inner(),
            |result| format!("{selection}, {}", profile_unit_result(result)),
        )
    }

    fn confirm_commit_selection_inner(&mut self) -> Result<()> {
        // With no picks, Enter reviews the cursor commit on its own.
        let picks: Vec<usize> = if self.commit_picks.is_empty() {
            vec![self.commit_list_cursor]
        } else {
            self.commit_picks.iter().copied().collect()
        };

        // Collect picked entries in order from oldest to newest (picks
        // iterate ascending index = newest first, so reverse).
        let selected_commits: Vec<CommitInfo> = picks
            .iter()
            .rev()
            .filter_map(|i| self.commit_list.get(*i))
            .cloned()
            .collect();

        if selected_commits.is_empty() {
            self.set_message("Select at least one commit (space)");
            return Ok(());
        }

        if selected_commits.iter().any(Self::is_merge_conflicts_commit) {
            return self.open_merge_view(InputMode::CommitSelect);
        }

        // Reviewing materializes one diff from the oldest pick's parent to
        // the newest pick, so gaps would silently include unpicked commits.
        // History ops have no such constraint (`p` cherry-picks any set).
        let min = *picks.first().expect("picks not empty");
        let max = *picks.last().expect("picks not empty");
        if max - min + 1 != picks.len() {
            self.set_message(
                "Review needs a contiguous selection \u{2014} p cherry-picks the exact set",
            );
            return Ok(());
        }

        let selected_staged = selected_commits.iter().any(Self::is_staged_commit);
        let selected_unstaged = selected_commits.iter().any(Self::is_unstaged_commit);
        let selected_ids: Vec<String> = selected_commits
            .iter()
            .filter(|c| !Self::is_special_commit(c))
            .map(|c| c.id.clone())
            .collect();

        if (selected_staged || selected_unstaged) && !selected_ids.is_empty() {
            return self.load_staged_unstaged_and_commits_selection(selected_ids, selected_commits);
        }

        if selected_staged && selected_unstaged {
            return self.load_staged_and_unstaged_selection();
        }

        if selected_staged {
            return self.load_staged_selection();
        }

        if selected_unstaged {
            return self.load_unstaged_selection();
        }

        // Get the diff for the selected commits
        let highlighter = self.theme.syntax_highlighter();
        let diff_files = Self::get_commit_range_diff_with_ignore(
            self.vcs.as_ref(),
            &self.vcs_info.root_path,
            &ResolvedRevisionRange::from_commit_ids(&selected_ids, RevisionDiffTarget::CommitList),
            highlighter,
            self.path_filter.as_deref(),
        )?;

        if diff_files.is_empty() {
            self.set_message("No changes in selected commits");
            return Ok(());
        }

        // Update session with the newest commit as base
        let newest_commit_id = selected_ids.last().unwrap().clone();
        let loaded_session = load_latest_session_for_context(
            &self.vcs_info.root_path,
            self.vcs_info.branch_name.as_deref(),
            &newest_commit_id,
            SessionDiffSource::CommitRange,
            Some(selected_ids.as_slice()),
        )
        .ok()
        .and_then(|found| found.map(|(_path, session)| session));

        let mut session = loaded_session.unwrap_or_else(|| {
            let mut session = ReviewSession::new(
                self.vcs_info.root_path.clone(),
                newest_commit_id,
                self.vcs_info.branch_name.clone(),
                SessionDiffSource::CommitRange,
            );
            session.commit_range = Some(selected_ids.clone());
            session
        });

        if session.commit_range.is_none() {
            session.commit_range = Some(selected_ids.clone());
            session.updated_at = chrono::Utc::now();
        }

        self.session = session;

        // Add files to session
        for file in &diff_files {
            self.session.add_diff_file(file);
        }
        self.reset_persisted_session_tracking();

        // Update app state
        self.diff_files = diff_files;
        self.diff_source = DiffSource::CommitRange(selected_ids);
        self.input_mode = InputMode::Normal;

        // Reset navigation state
        self.diff_state = DiffState::default();
        self.file_list_state = FileListState::default();

        // Set up inline commit selector for multi-commit reviews (newest-first display order)
        self.pr_commits.clear();
        self.pr_last_reviewed_commit_index = None;
        self.review_commits = selected_commits.iter().rev().cloned().collect();
        self.range_diff_files = Some(self.diff_files.clone());
        self.commit_list = self.review_commits.clone();
        let range =
            Self::initial_commit_range(self.commit_selection_start, self.review_commits.len());
        self.commit_selection_range = range;
        self.commit_list_cursor = range.map(|(start, _)| start).unwrap_or(0);
        self.commit_list_scroll_offset = 0;
        self.visible_commit_count = self.review_commits.len();
        self.has_more_commit = false;
        self.show_commit_selector = self.review_commits.len() > 1;
        self.commit_diff_cache.clear();
        self.saved_inline_selection = None;
        self.commit_picks.clear();

        // `initial_commit_selection = oldest` opens scoped to a single commit; narrow
        // the loaded diff to it. Otherwise finalize the full-range diff.
        if Self::is_strict_commit_selection(self.commit_selection_range, self.review_commits.len())
        {
            self.reload_inline_selection()?;
        } else {
            self.sort_files_by_directory(true);
            self.expand_all_dirs();
            self.rebuild_annotations();
        }

        Ok(())
    }

    /// The diff for commit rows `start..=end`, when it is already in memory.
    /// `None` means it has to be fetched.
    ///
    /// There are two caches. `range_diff_files` holds the every-row selection,
    /// loaded once when the review opened, so it is checked first.
    /// `commit_diff_cache` fills up with narrower selections as the user
    /// cycles through them.
    fn cached_selection_diff(&self, start: usize, end: usize) -> Option<Vec<DiffFile>> {
        // `end + 1 == len`, not `end == len - 1`: an empty list underflows.
        let whole_range = start == 0 && end + 1 == self.review_commits.len();
        if whole_range && let Some(files) = &self.range_diff_files {
            return Some(files.clone());
        }
        self.commit_diff_cache.get(&(start, end)).cloned()
    }

    /// Reload the diff for the currently selected inline commit subrange.
    pub fn reload_inline_selection(&mut self) -> Result<()> {
        let Some((start, end)) = self.commit_selection_range else {
            self.set_message("Select at least one commit");
            return Ok(());
        };

        // Each branch decides only where the files come from. The install runs
        // once, below, so no branch can forget a step of it. Session
        // registration went missing that way.
        let diff_files = match self.cached_selection_diff(start, end) {
            Some(files) => files,
            // Load diff for selected subrange. `source_for_commit_subrange`
            // holds the one copy of "which diff does this selection mean",
            // shared with `narrowed_fetch_source` so a reload and the selector
            // can never disagree about it. An empty result is not an error
            // here: a subrange can legitimately contain no changes.
            None => {
                let fetch_source =
                    Self::source_for_commit_subrange(&self.review_commits, start, end);
                let highlighter = self.theme.syntax_highlighter();
                let fetched = match Self::fetch_diff_files_for_source(
                    self.vcs.as_ref(),
                    &self.vcs_info.root_path,
                    &fetch_source,
                    highlighter,
                    self.path_filter.as_deref(),
                ) {
                    Ok(files) => files,
                    Err(TuicrError::NoChanges) => Vec::new(),
                    Err(e) => return Err(e),
                };
                self.commit_diff_cache.insert((start, end), fetched.clone());
                fetched
            }
        };

        self.diff_files = diff_files;

        // Register the files in the session. `r`, `R` and the comment path all
        // look a file up here, so a file reachable only through a narrowed
        // commit selection could not be marked reviewed or commented on.
        //
        // Hunk marks are preserved rather than pruned, for the same reason
        // `reload_pr_inline_selection` preserves them: a narrowed selection is
        // a partial view of a wider review, and hunks it does not show are
        // still reviewed in that wider scope.
        Self::register_diff_files(&mut self.session, &self.diff_files, true);

        // Reset navigation, rebuild file tree + annotations
        let wrap = self.diff_state.wrap_lines;
        self.diff_state = DiffState::default();
        self.diff_state.wrap_lines = wrap;
        self.file_list_state = FileListState::default();
        self.expanded_top.clear();
        self.expanded_bottom.clear();
        self.insert_commit_message_if_single();
        self.sort_files_by_directory(true);
        self.expand_all_dirs();
        self.rebuild_annotations();

        Ok(())
    }

    /// Reload the inline commit subrange diff via the backend appropriate for
    /// the current review. PR reviews route through the forge `compare` API
    /// (persisting the narrowed range so it survives a restart); local reviews
    /// reload straight from the VCS. Callers that adjust the selection (toggle,
    /// `(`/`)` cycling, restore-on-exit) must go through here so PR reviews
    /// don't fall into the VCS path — the forge backend has no commit-range
    /// diff support and would error with "Commit range diff not supported".
    pub fn reload_inline_selection_for_source(&mut self) -> Result<()> {
        if matches!(self.diff_source, DiffSource::PullRequest(_)) {
            self.persist_pr_commit_selection_range();
            self.reload_pr_inline_selection();
            Ok(())
        } else {
            self.reload_inline_selection()
        }
    }
}
