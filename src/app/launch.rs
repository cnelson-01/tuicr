//! Launching an `App` on a repository, and replacing it with one on another.
//!
//! `main.rs` resolves the CLI and the config once into [`LaunchSettings`].
//! [`App::finish_launch`] applies them to a fresh `App`, so the app made at
//! startup and the one made by the repo switcher are set up by the same
//! code and cannot drift apart.
//!
//! [`App::switch_repo`] is the repo switcher's whole effect. It behaves like
//! quitting tuicr and starting it again in the other repository with no
//! arguments: a second `App` is built beside the running one and swapped in
//! only once it exists. Nothing is reset field by field, so nothing of the
//! old repository (worker results, caches, pickers, PR mode, selections)
//! can leak into the new one, and a target that cannot be opened leaves the
//! running app exactly as it was. What does survive a switch is listed in
//! one place, [`App::carry_process_state_from`].

use std::path::{Path, PathBuf};

use super::repos::{abbreviate_home, home_dir, repo_name};
use super::{
    App, AppStartupOptions, CommitOrder, CommitSelectionStart, DiffViewMode, FocusedPanel,
    MAX_FILE_LIST_PCT, MAX_SBS_SPLIT_PCT, MIN_FILE_LIST_PCT, MIN_LOG_PANE_HEIGHT,
    MIN_SBS_SPLIT_PCT, MessageType, WrapStyle,
};
use crate::config::{AppConfig, CommentTypeConfig};
use crate::forge::remote_comments::PrCommentsVisibility;
use crate::persistence::recent_repos;
use crate::vcs::{DiffWhitespaceMode, GitBackendPreference};

/// Hide the file list by default on terminals narrower than this.
pub const MIN_WIDTH_FOR_FILE_LIST: u16 = 100;

/// What `main.rs` resolved once from the CLI and the config: everything a
/// fresh `App` is given that does not depend on the repository it opens.
pub struct LaunchSettings {
    pub config: Option<AppConfig>,
    pub output_to_stdout: bool,
    pub keyboard_enhancement_supported: bool,
    pub mouse_enabled: bool,
    pub git_backend_preference: GitBackendPreference,
    pub diff_whitespace_mode: DiffWhitespaceMode,
    pub commit_order: CommitOrder,
    pub commit_selection: CommitSelectionStart,
    pub pr_comments_visibility: Option<PrCommentsVisibility>,
    /// The recent-repositories file, or `None` to keep no list.
    pub recent_repos_path: Option<PathBuf>,
    /// Whether a main loop is polling the status probe (`status_probe.rs`).
    pub background_status_probe: bool,
}

impl Default for LaunchSettings {
    /// A launch with no config file and nothing on the command line. No
    /// recent-repositories file and no background probe: what a test wants.
    fn default() -> Self {
        Self {
            config: None,
            output_to_stdout: false,
            keyboard_enhancement_supported: false,
            mouse_enabled: true,
            git_backend_preference: GitBackendPreference::Libgit2,
            diff_whitespace_mode: DiffWhitespaceMode::Normal,
            commit_order: CommitOrder::default(),
            commit_selection: CommitSelectionStart::default(),
            pr_comments_visibility: None,
            recent_repos_path: None,
            background_status_probe: false,
        }
    }
}

impl LaunchSettings {
    /// The configured comment types, for `App::new`.
    pub fn comment_types(&self) -> Option<Vec<CommentTypeConfig>> {
        self.config
            .as_ref()
            .and_then(|cfg| cfg.comment_types.clone())
    }

    /// Startup options for a launch with no CLI arguments: the review
    /// target selector. `repo_root` opens the repository containing that
    /// directory; `None` opens the one around the working directory.
    /// `main.rs` overrides the CLI-driven fields with struct update syntax.
    pub fn startup_options<'a>(&'a self, repo_root: Option<&'a Path>) -> AppStartupOptions<'a> {
        let config = self.config.as_ref();
        AppStartupOptions {
            revisions: None,
            working_tree: false,
            path_filter: None,
            file_path: None,
            all_files: false,
            show_pr_checks: config.and_then(|cfg| cfg.show_pr_checks).unwrap_or(false),
            show_pr_comments: config.and_then(|cfg| cfg.show_pr_comments).unwrap_or(true),
            pr_comments_visibility: self.pr_comments_visibility,
            git_backend_preference: self.git_backend_preference,
            diff_whitespace_mode: self.diff_whitespace_mode.clone(),
            commit_selection: self.commit_selection,
            pr_target: None,
            repo_url_override: None,
            repo_root,
        }
    }
}

impl App {
    /// Apply everything the CLI and the config decide about a fresh app.
    /// `terminal_width` is the terminal's width in columns when it is known.
    pub fn apply_launch_settings(
        &mut self,
        settings: &LaunchSettings,
        terminal_width: Option<u16>,
    ) {
        self.supports_keyboard_enhancement = settings.keyboard_enhancement_supported;
        if let Some(cfg) = settings.config.as_ref() {
            if let Some(forge_cfg) = cfg.forge.clone() {
                self.forge_config = forge_cfg;
            }
            if let Some(leader) = cfg.leader {
                self.leader_key = leader;
            }
            self.comment_vim_enabled = cfg.comment_vim.unwrap_or(false);
            self.q_quits = cfg.q_quits.unwrap_or(false);
            self.editor_override = cfg.editor.clone();
            if let Some(w) = cfg.comment_tab_width {
                self.comment_tab_width = w;
            }
            if let Some(username) = cfg
                .username
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                self.username = username.to_string();
            }
        }

        // Commit selector presentation/behavior prefs. `commit_selection`
        // also fed `App::new` for the initial `-r`/PR range; keep it on the
        // app so re-selections during the session (target selector, PR
        // reload) honor it.
        self.commit_order = settings.commit_order;
        self.commit_selection_start = settings.commit_selection;
        self.mouse_enabled = settings.mouse_enabled;
        // With a main loop polling the worker, the selector need not wait on
        // `git status` to open.
        self.background_status_probe = settings.background_status_probe;
        self.recent_repos_path = settings.recent_repos_path.clone();

        // Config-driven defaults
        if let Some(cfg) = settings.config.as_ref() {
            self.show_pr_checks = cfg.show_pr_checks.unwrap_or(false);
            self.show_pr_comments = cfg.show_pr_comments.unwrap_or(true);
            self.initial_comments_visibility = settings.pr_comments_visibility;
            self.set_compact_folders(cfg.compact_folders.unwrap_or(false));
            if cfg.show_file_list == Some(false) {
                self.show_file_list = false;
                self.focused_panel = FocusedPanel::Diff;
            }
            // Start with the commit selector pane hidden. `(` / `)` still cycle the
            // selected commit while hidden; `<leader>s` / `:set commits!` reveal it.
            if cfg.show_commits == Some(false) {
                self.show_commit_selector = false;
                if self.focused_panel == FocusedPanel::CommitSelector {
                    self.focused_panel = FocusedPanel::Diff;
                }
            }
            if let Some(show) = cfg.show_log {
                self.log_pane_wanted = show;
            }
            if let Some(show) = cfg.show_changes {
                self.changes_pane_wanted = show;
            }
            if let Some(rows) = cfg.log_height {
                self.log_pane_height = Some(
                    u16::try_from(rows)
                        .unwrap_or(u16::MAX)
                        .max(MIN_LOG_PANE_HEIGHT),
                );
            }
            // Start with files already marked reviewed hidden. `init_` rather than
            // `set_` so startup leaves no status message behind; `H` /
            // `:set reviewed!` toggle from here.
            if cfg.show_reviewed == Some(false) {
                self.init_show_reviewed(false);
            }
            // Pristine mode has no diff, so side-by-side would render two
            // identical panes. Honor the config for every other mode.
            if cfg.diff_view.as_deref() == Some("side-by-side") && !self.is_pristine_mode {
                self.diff_view_mode = DiffViewMode::SideBySide;
            }
            if let Some(wrap) = cfg.wrap {
                self.diff_state.wrap_lines = wrap;
            }
            if let Some(pct) = cfg.split {
                self.sbs_split_pct =
                    (pct.min(100) as u16).clamp(MIN_SBS_SPLIT_PCT, MAX_SBS_SPLIT_PCT);
            }
            if let Some(pct) = cfg.tree_width {
                self.file_list_pct =
                    (pct.min(100) as u16).clamp(MIN_FILE_LIST_PCT, MAX_FILE_LIST_PCT);
            }
            match cfg.wrap_style.as_deref() {
                Some("flow") => self.wrap_style = WrapStyle::Flow,
                Some("gutter") => self.wrap_style = WrapStyle::Gutter,
                _ => {}
            }
            self.relative_line_numbers = cfg.relative_line_numbers.unwrap_or(false);
            self.ide_change_colors = cfg.ide_colors.unwrap_or(true);
            // Open in single-file view when the user opts in. Pristine
            // `--all-files` already turned it on inside `App::new`, so we
            // only toggle if it's still off.
            if cfg.single_file_view == Some(true) && !self.is_single_file_view {
                self.toggle_single_file_view();
            }
            self.export = cfg.resolved_export();
            if cfg.cursor_line == Some(false) {
                self.cursor_line_highlight = false;
            }
            if cfg.search_highlight == Some(false) {
                self.search_highlight_enabled = false;
            }
            if let Some(scroll_offset) = cfg.scroll_offset {
                self.scroll_offset = scroll_offset;
            }
            if let Some(lines) = cfg.mouse_scroll_lines {
                self.mouse_scroll_lines = lines;
            }
            if let Some(interval_ms) = cfg.review_watch_interval_ms {
                self.set_review_watch_interval_ms(interval_ms as u64);
            }
            if let Some(interval_ms) = cfg.diff_watch_interval_ms {
                self.set_diff_watch_interval_ms(interval_ms as u64);
            }
        }

        // On narrow terminals, start with only the diff panel visible.
        if terminal_width.is_some_and(|width| width < MIN_WIDTH_FOR_FILE_LIST) {
            self.show_file_list = false;
            self.focused_panel = FocusedPanel::Diff;
        }
    }

    /// Everything a fresh app needs after `App::new` and before its first
    /// frame: the settings, the review session file, and the header's
    /// repository status. Returns the warnings worth showing the user. It
    /// prints nothing and draws nothing, so the repo switcher can call it
    /// while the TUI is on screen.
    pub fn finish_launch(
        &mut self,
        settings: &LaunchSettings,
        terminal_width: Option<u16>,
    ) -> Vec<String> {
        let mut warnings = self.vcs.startup_warnings();
        self.apply_launch_settings(settings, terminal_width);
        if let Err(e) = self.ensure_ephemeral_session_file() {
            warnings.push(format!("Failed to initialize review session file: {e}"));
        }
        // Upstream divergence and any operation in progress, for the header.
        self.refresh_repo_status();
        warnings
    }

    /// Replace this app with one opened on the repository containing
    /// `target`, as if tuicr had been quit and started there with no
    /// arguments. Returns whether the switch happened; when it did not, this
    /// app is untouched apart from the message saying why.
    ///
    /// Like quitting, a remote operation or PR submit still running in the
    /// old repository is left to finish on its own, and its result is
    /// dropped with the old app. Unlike `:q`, a review with unsaved changes
    /// is saved first (what `ZZ` does, minus the export), and the switch is
    /// refused if that save fails.
    ///
    /// It never changes the process's working directory: `main.rs` does that
    /// after a successful switch, so tests can switch freely.
    pub fn switch_repo(
        &mut self,
        target: &Path,
        settings: &LaunchSettings,
        terminal_width: Option<u16>,
    ) -> bool {
        if self.output_to_stdout {
            // The caller is waiting for this repository's review, and the
            // `tuicr-session:` line cannot be announced a second time.
            self.set_warning("Cannot switch repositories under --stdout");
            return false;
        }
        let home = home_dir();
        let root = recent_repos::canonical(target);
        // Messages lead with the name and the reason and end with the path,
        // which is the part a narrow status bar can afford to cut.
        let name = repo_name(&root);
        let shown = abbreviate_home(&root, home.as_deref());
        if !root.is_dir() {
            self.forget_recent_repo(&root);
            self.set_warning(format!(
                "{name} no longer exists; removed from the list ({shown})"
            ));
            return false;
        }
        let current = self.current_repo_root().map(recent_repos::canonical);
        if current.as_deref() == Some(root.as_path()) {
            self.set_message(format!("Already in {name}"));
            return false;
        }

        // A review being posted lasts seconds, and its answer is what turns
        // the drafts into submitted comments: leaving now would drop it and
        // the next submit would post them twice.
        if self.pr_submit_rx.is_some() {
            self.set_warning("Not switching: the review is still being submitted");
            return false;
        }

        // Built beside the running app: until it exists, nothing of `self`
        // has changed, and dropping it has no side effect.
        let mut next = match App::new(
            self.theme.clone_sharing_highlighter(),
            settings.comment_types(),
            settings.output_to_stdout,
            settings.startup_options(Some(&root)),
        ) {
            Ok(next) => next,
            Err(e) => {
                self.set_warning(format!("Cannot open {name}: {e} ({shown})"));
                return false;
            }
        };
        // The target was a directory inside the repository already open.
        let next_root = next.current_repo_root().map(recent_repos::canonical);
        if let (Some(next_root), Some(current)) = (next_root.as_deref(), current.as_deref())
            && next_root == current
        {
            self.set_message(format!("Already in {}", repo_name(current)));
            return false;
        }

        // What quitting does for the review being left.
        if self.dirty
            && let Err(e) = self.save_current_session_merging_external()
        {
            self.set_warning(format!("Not switching: saving this review failed: {e}"));
            return false;
        }
        let _ = self.cleanup_empty_ephemeral_sessions();

        // Marking the new session active also replaces this process's
        // active-session entry, so the old one needs no clearing.
        let warnings = next.finish_launch(settings, terminal_width);
        let leaving = self
            .remote_op_running()
            .map(|op| (op, current.as_deref().map(repo_name)));
        next.carry_process_state_from(self);
        // The old app, its worker receivers and its caches are dropped here.
        *self = next;
        self.record_current_repo();

        if let Some(warning) = warnings.first() {
            self.set_warning(warning.clone());
        } else if !self
            .message
            .as_ref()
            .is_some_and(|message| message.message_type != MessageType::Info)
        {
            let name = self.current_repo_root().map(repo_name).unwrap_or(name);
            let mut message = format!("Switched to {name}");
            if let Some((op, old_name)) = leaving {
                let old_name = old_name.unwrap_or_else(|| "the previous repository".to_string());
                message.push_str(&format!(
                    " \u{00b7} the {} in {old_name} is still running",
                    op.name()
                ));
            }
            self.set_message(message);
        }
        self.request_full_redraw();
        true
    }

    /// The only state that survives a repository switch, taken from the app
    /// being replaced. Everything else is either rebuilt from
    /// [`LaunchSettings`] or belongs to the old repository and goes with it.
    /// (The theme is carried too, by the clone `switch_repo` hands
    /// `App::new`.)
    fn carry_process_state_from(&mut self, old: &mut App) {
        // The update badge is about this tuicr, not about a repository.
        self.update_info = old.update_info.take();
        // Windowed editors already open keep being watched for a launch
        // failure.
        self.editor_launches = std::mem::take(&mut old.editor_launches);
        // A fetch-all is about every recent repository, not the one being
        // left: it goes on, and reports to whichever app is on screen.
        self.fetch_all = old.fetch_all.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::render_perf_tests::{app_with, file};

    fn fresh_app() -> App {
        app_with(vec![file("src/a.rs", 4), file("src/b.rs", 4)])
    }

    #[test]
    fn should_apply_config_driven_defaults_to_a_fresh_app() {
        // given
        let mut app = fresh_app();
        let settings = LaunchSettings {
            config: Some(AppConfig {
                show_file_list: Some(false),
                diff_view: Some("side-by-side".to_string()),
                leader: Some(','),
                wrap: Some(true),
                ..AppConfig::default()
            }),
            mouse_enabled: false,
            keyboard_enhancement_supported: true,
            commit_order: CommitOrder::Ascending,
            ..LaunchSettings::default()
        };

        // when
        app.apply_launch_settings(&settings, Some(200));

        // then
        assert!(!app.show_file_list);
        assert_eq!(app.focused_panel, FocusedPanel::Diff);
        assert_eq!(app.diff_view_mode, DiffViewMode::SideBySide);
        assert_eq!(app.leader_key, ',');
        assert!(app.diff_state.wrap_lines);
        assert!(!app.mouse_enabled);
        assert!(app.supports_keyboard_enhancement);
        assert_eq!(app.commit_order, CommitOrder::Ascending);
        assert_eq!(app.recent_repos_path, None);
        assert!(!app.background_status_probe);
    }

    #[test]
    fn should_keep_the_file_list_on_a_wide_terminal_without_config() {
        // given
        let mut app = fresh_app();
        app.show_file_list = true;

        // when
        app.apply_launch_settings(&LaunchSettings::default(), Some(MIN_WIDTH_FOR_FILE_LIST));

        // then
        assert!(app.show_file_list);
    }

    #[test]
    fn should_hide_the_file_list_on_a_narrow_terminal() {
        // given
        let mut app = fresh_app();
        app.show_file_list = true;

        // when
        app.apply_launch_settings(
            &LaunchSettings::default(),
            Some(MIN_WIDTH_FOR_FILE_LIST - 1),
        );

        // then
        assert!(!app.show_file_list);
        assert_eq!(app.focused_panel, FocusedPanel::Diff);
    }

    #[test]
    fn should_leave_the_file_list_alone_when_the_terminal_width_is_unknown() {
        // given
        let mut app = fresh_app();
        app.show_file_list = true;

        // when
        app.apply_launch_settings(&LaunchSettings::default(), None);

        // then
        assert!(app.show_file_list);
    }

    #[test]
    fn should_build_no_argument_startup_options_from_the_config() {
        // given
        let settings = LaunchSettings {
            config: Some(AppConfig {
                show_pr_checks: Some(true),
                show_pr_comments: Some(false),
                ..AppConfig::default()
            }),
            git_backend_preference: GitBackendPreference::Cli,
            commit_selection: CommitSelectionStart::Oldest,
            ..LaunchSettings::default()
        };
        let root = Path::new("/somewhere/else");

        // when
        let options = settings.startup_options(Some(root));

        // then: the selector launch, at the given root
        assert_eq!(options.repo_root, Some(root));
        assert!(options.revisions.is_none() && !options.working_tree);
        assert!(options.file_path.is_none() && !options.all_files);
        assert!(options.pr_target.is_none() && options.path_filter.is_none());
        assert!(options.show_pr_checks);
        assert!(!options.show_pr_comments);
        assert_eq!(options.git_backend_preference, GitBackendPreference::Cli);
        assert_eq!(options.commit_selection, CommitSelectionStart::Oldest);
    }
}
