//! The repo switcher (`<leader>r`, `:repos`): a picker over the
//! repositories tuicr was opened in, most recent first, plus `:repo <path>`
//! for one that is not on the list yet.
//!
//! Nothing here switches. Choosing a repository only sets
//! `App::pending_repo_switch`; the main loop takes it between events and
//! calls [`App::switch_repo`](super::App::switch_repo), which replaces the
//! whole app. Opening the picker reads the list file and asks the
//! filesystem whether each path is still a directory, and nothing else: no
//! git runs per row.

use std::path::{Path, PathBuf};

use chrono::Utc;

use super::{App, InputMode};
use crate::input::catalog;
use crate::persistence::recent_repos::{self, RecentRepo};

/// One repository in the switcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRow {
    /// The root as stored in the list (canonical when it was recorded).
    pub path: PathBuf,
    /// The root's last path component.
    pub name: String,
    /// The root with the home directory written as `~`.
    pub display_path: String,
    /// When it was last opened, unix seconds.
    pub last_opened: i64,
    /// Whether the path is still a directory.
    pub exists: bool,
    /// Whether it is the repository on screen.
    pub is_current: bool,
}

impl RepoRow {
    fn new(repo: RecentRepo, current: Option<&Path>, home: Option<&Path>) -> Self {
        Self {
            name: repo_name(&repo.path),
            display_path: abbreviate_home(&repo.path, home),
            last_opened: repo.last_opened,
            exists: repo.path.is_dir(),
            is_current: current == Some(repo.path.as_path()),
            path: repo.path,
        }
    }
}

/// The repo switcher modal: the recent repositories, filtered by what the
/// user types, with the cursor on a row of the filtered view.
#[derive(Debug)]
pub struct RepoPicker {
    /// Most recently opened first.
    pub rows: Vec<RepoRow>,
    pub filter: String,
    /// Row within [`RepoPicker::visible`], not into `rows`.
    pub cursor: usize,
    /// Mode restored when the picker closes.
    pub return_mode: InputMode,
}

impl RepoPicker {
    /// Indices into `rows` that match the filter: best match first, and
    /// among equals the most recently opened. With no filter that is every
    /// row, in the list's order.
    pub fn visible(&self) -> Vec<usize> {
        let mut scored: Vec<(i32, usize)> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(idx, row)| {
                repo_match_score(&self.filter, &row.name, &row.display_path)
                    .map(|score| (score, idx))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, idx)| idx).collect()
    }

    pub fn selected(&self) -> Option<&RepoRow> {
        self.visible().get(self.cursor).map(|&idx| &self.rows[idx])
    }

    /// Where the cursor starts: the most recent repository that is not the
    /// one on screen, so opening the picker and pressing Enter flips between
    /// the last two. A row whose directory is gone is passed over while
    /// another can be opened.
    fn default_cursor(&self) -> usize {
        let visible = self.visible();
        let other = |need_dir: bool| {
            visible.iter().position(|&idx| {
                let row = &self.rows[idx];
                !row.is_current && (row.exists || !need_dir)
            })
        };
        other(true).or_else(|| other(false)).unwrap_or(0)
    }

    fn clamp_cursor(&mut self) {
        let n = self.visible().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    /// After the filter changed: the best match while filtering, the usual
    /// starting row once the filter is empty again.
    fn reset_cursor(&mut self) {
        self.cursor = if self.filter.is_empty() {
            self.default_cursor()
        } else {
            0
        };
    }
}

/// Score a repository against the switcher's filter, or `None` when it does
/// not match. Every space-separated word must match the name or the path,
/// as a substring or, failing that, as a subsequence (`tcr` finds `tuicr`).
/// Each word counts with its best tier, and the tiers do not overlap:
///
/// 1. substring of the name
/// 2. substring of the path
/// 3. subsequence of the name
/// 4. subsequence of the path
///
/// An empty filter matches everything equally, which leaves the list in
/// most-recently-used order.
pub(crate) fn repo_match_score(query: &str, name: &str, display_path: &str) -> Option<i32> {
    const TIER: i32 = 1000;
    let mut total = 0;
    for word in query.split_whitespace() {
        let score = if let Some(score) = catalog::match_score(word, name) {
            3 * TIER + score
        } else if let Some(score) = catalog::match_score(word, display_path) {
            2 * TIER + score
        } else if let Some(score) = subsequence_score(word, name) {
            TIER + score
        } else {
            subsequence_score(word, display_path)?
        };
        total += score;
    }
    Some(total)
}

/// Score `word` as a subsequence of `haystack`, case-insensitively: its
/// characters in order, not necessarily adjacent. Runs of adjacent matches
/// and matches at the start of a word score higher, and a match spread over
/// a long stretch scores lower. Always within `1..1000`, so it stays inside
/// its tier in [`repo_match_score`].
fn subsequence_score(word: &str, haystack: &str) -> Option<i32> {
    let needle: Vec<char> = word.to_lowercase().chars().collect();
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let first = *needle.first()?;
    // The earliest occurrence of the first character is not always the
    // best place to start (`tcr` in `~/tools/tuicr`), so try each.
    (0..hay.len())
        .filter(|&start| hay[start] == first)
        .filter_map(|start| subsequence_score_from(&needle, &hay, start))
        .max()
}

/// Greedy left-to-right match of `needle` in `hay` with its first character
/// at `start`.
fn subsequence_score_from(needle: &[char], hay: &[char], start: usize) -> Option<i32> {
    let mut score = 0;
    let mut pos = start;
    let mut previous: Option<usize> = None;
    for &wanted in needle {
        let at = (pos..hay.len()).find(|&i| hay[i] == wanted)?;
        score += 10;
        if previous.is_some_and(|prev| prev + 1 == at) {
            score += 15;
        }
        if at == 0 || !hay[at - 1].is_alphanumeric() {
            score += 10;
        }
        previous = Some(at);
        pos = at + 1;
    }
    let span = pos - start;
    let skipped = span.saturating_sub(needle.len()).min(50) as i32;
    Some((score - skipped).clamp(1, 999))
}

/// The user's home directory, for `~` in paths typed and shown.
pub(crate) fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// What the switcher calls a repository: the last component of its root.
pub(crate) fn repo_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `~` and `~/rest` name the home directory; anything else is taken as
/// written.
pub(crate) fn expand_tilde(arg: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(arg);
    };
    match arg.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with(std::path::is_separator) => home.join(&rest[1..]),
        _ => PathBuf::from(arg),
    }
}

/// `path` for display, with the home directory written as `~`.
pub(crate) fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Directory completions for a path being typed after `:repo `. `typed` is
/// split at its last `/`: the part before names the directory to list
/// (`~` expanded, relative paths under `base`), the part after is the
/// prefix a directory's name must start with. Hidden directories are
/// offered only to a prefix that starts with a dot. The answers are sorted
/// and spelled the way the user spelled the first part, each ending in `/`
/// so the next Tab descends.
pub(crate) fn complete_dir(typed: &str, home: Option<&Path>, base: &Path) -> Vec<String> {
    if typed == "~" {
        return home.map(|_| vec!["~/".to_string()]).unwrap_or_default();
    }
    let (typed_dir, prefix) = match typed.rfind(std::path::is_separator) {
        Some(separator) => typed.split_at(separator + 1),
        None => ("", typed),
    };
    let dir = if typed_dir.is_empty() {
        base.to_path_buf()
    } else {
        let expanded = expand_tilde(typed_dir, home);
        if expanded.is_absolute() {
            expanded
        } else {
            base.join(expanded)
        }
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        // `is_dir` follows symlinks, so a link to a directory completes too.
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| prefix.starts_with('.') || !name.starts_with('.'))
        .collect();
    let starting_with = |matches: &dyn Fn(&str) -> bool| {
        let mut found: Vec<String> = names
            .iter()
            .filter(|name| matches(name))
            .map(|name| format!("{typed_dir}{name}/"))
            .collect();
        found.sort();
        found
    };
    let exact = starting_with(&|name: &str| name.starts_with(prefix));
    if !exact.is_empty() {
        return exact;
    }
    // Nothing matches as typed: forgive the case, as the filesystem often
    // does.
    let lowered = prefix.to_lowercase();
    starting_with(&|name: &str| name.to_lowercase().starts_with(&lowered))
}

/// [`complete_dir`] with the user's real home directory, for the `:repo`
/// command line.
pub(crate) fn complete_repo_path(typed: &str, base: &Path) -> Vec<String> {
    complete_dir(typed, home_dir().as_deref(), base)
}

impl App {
    /// The root of the repository this app was opened on. `None` for apps
    /// not opened on a repository of their own: `--file`, `--all-files` and
    /// `tuicr pr` launches.
    pub fn current_repo_root(&self) -> Option<&Path> {
        self.vcs_open_options.root.as_deref()
    }

    /// Note in the recent-repositories list that this app's repository was
    /// just opened. Does nothing without a list file or without a
    /// repository; a write that fails is not worth interrupting the user
    /// for.
    pub fn record_current_repo(&self) {
        let (Some(file), Some(root)) =
            (self.recent_repos_path.as_deref(), self.current_repo_root())
        else {
            return;
        };
        let _ = recent_repos::record(file, root, Utc::now().timestamp());
    }

    /// Drop `path` from the list file and from the open picker's rows.
    pub(in crate::app) fn forget_recent_repo(&mut self, path: &Path) {
        if let Some(file) = self.recent_repos_path.as_deref() {
            let _ = recent_repos::remove(file, path);
        }
        if let Some(picker) = self.repo_picker.as_mut() {
            picker.rows.retain(|row| row.path != path);
            picker.clamp_cursor();
        }
    }

    /// Open the repo switcher over the current screen. From the command
    /// line the picker returns to whatever mode `:` was typed in.
    pub fn open_repo_picker(&mut self) {
        let return_mode = match self.input_mode {
            InputMode::Command => self.command_return_mode,
            InputMode::CommitSelect => InputMode::CommitSelect,
            InputMode::RepoPicker => return,
            _ => InputMode::Normal,
        };
        self.forget_stale_fetch_all();
        let home = home_dir();
        let current = self.current_repo_root().map(recent_repos::canonical);
        let rows = self
            .recent_repos_path
            .as_deref()
            .map(recent_repos::load)
            .unwrap_or_default()
            .into_iter()
            .map(|repo| RepoRow::new(repo, current.as_deref(), home.as_deref()))
            .collect();
        let mut picker = RepoPicker {
            rows,
            filter: String::new(),
            cursor: 0,
            return_mode,
        };
        picker.cursor = picker.default_cursor();
        self.repo_picker = Some(picker);
        self.command_buffer.clear();
        self.input_mode = InputMode::RepoPicker;
    }

    pub fn close_repo_picker(&mut self) {
        if let Some(picker) = self.repo_picker.take() {
            self.input_mode = picker.return_mode;
        } else {
            self.input_mode = InputMode::Normal;
        }
        self.forget_finished_fetch_all();
    }

    /// Move the cursor. A single step wraps around the ends; a page stops
    /// at them.
    pub fn repo_picker_move(&mut self, delta: isize) {
        if let Some(picker) = self.repo_picker.as_mut() {
            let n = picker.visible().len() as isize;
            if n == 0 {
                return;
            }
            let target = picker.cursor as isize + delta;
            let landed = if delta.abs() == 1 {
                target.rem_euclid(n)
            } else {
                target.clamp(0, n - 1)
            };
            picker.cursor = landed as usize;
        }
    }

    pub fn repo_picker_push(&mut self, c: char) {
        if let Some(picker) = self.repo_picker.as_mut() {
            picker.filter.push(c);
            picker.reset_cursor();
        }
    }

    pub fn repo_picker_pop(&mut self) {
        if let Some(picker) = self.repo_picker.as_mut() {
            picker.filter.pop();
            picker.reset_cursor();
        }
    }

    pub fn repo_picker_clear(&mut self) {
        if let Some(picker) = self.repo_picker.as_mut() {
            picker.filter.clear();
            picker.reset_cursor();
        }
    }

    /// Enter in the picker: switch to the highlighted repository. A row
    /// whose directory is gone is dropped from the list instead, and the
    /// picker stays open.
    pub fn repo_picker_confirm(&mut self) {
        let Some(row) = self
            .repo_picker
            .as_ref()
            .and_then(|picker| picker.selected().cloned())
        else {
            return;
        };
        if row.is_current {
            self.close_repo_picker();
            self.set_message(format!("Already in {}", row.name));
            return;
        }
        if !row.path.is_dir() {
            self.forget_recent_repo(&row.path);
            self.set_warning(format!(
                "{} no longer exists; removed from the list ({})",
                row.name, row.display_path
            ));
            return;
        }
        if self.output_to_stdout {
            self.set_warning("Cannot switch repositories under --stdout");
            return;
        }
        self.close_repo_picker();
        self.pending_repo_switch = Some(row.path);
    }

    /// Ctrl-x in the picker: take the highlighted repository off the list.
    /// The repository itself is not touched.
    pub fn repo_picker_remove(&mut self) {
        let Some(row) = self
            .repo_picker
            .as_ref()
            .and_then(|picker| picker.selected().cloned())
        else {
            return;
        };
        self.forget_recent_repo(&row.path);
        self.set_message(format!("Removed {} from the list", row.name));
    }

    /// `:repo <path>`: switch to the repository containing `arg`, which is
    /// how one that was never opened gets onto the list. `~` is expanded and
    /// a relative path is taken from the working directory. With no path
    /// this opens the picker.
    pub fn open_repo_by_path(&mut self, arg: &str) {
        let arg = arg.trim();
        if arg.is_empty() {
            self.open_repo_picker();
            return;
        }
        let mut path = expand_tilde(arg, home_dir().as_deref());
        if path.is_relative()
            && let Ok(cwd) = std::env::current_dir()
        {
            path = cwd.join(&path);
        }
        if !path.is_dir() {
            self.set_warning(format!("No such directory: {arg}"));
            return;
        }
        self.pending_repo_switch = Some(path);
    }

    /// Whether a repository switch is waiting for the main loop.
    pub fn repo_switch_pending(&self) -> bool {
        self.pending_repo_switch.is_some()
    }

    /// The repository to switch to, if one was asked for. The main loop
    /// takes it and calls [`App::switch_repo`].
    pub fn take_pending_repo_switch(&mut self) -> Option<PathBuf> {
        self.pending_repo_switch.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, last_opened: i64) -> RepoRow {
        RepoRow::new(
            RecentRepo {
                path: PathBuf::from(path),
                last_opened,
            },
            None,
            Some(Path::new("/home/me")),
        )
    }

    /// A picker over `paths`, most recent first, as if every directory
    /// existed and the first were the repository on screen.
    fn picker(paths: &[&str]) -> RepoPicker {
        let rows = paths
            .iter()
            .enumerate()
            .map(|(idx, path)| RepoRow {
                exists: true,
                is_current: idx == 0,
                ..row(path, 1_000 - idx as i64)
            })
            .collect();
        let mut picker = RepoPicker {
            rows,
            filter: String::new(),
            cursor: 0,
            return_mode: InputMode::Normal,
        };
        picker.cursor = picker.default_cursor();
        picker
    }

    fn visible_names(picker: &RepoPicker) -> Vec<&str> {
        picker
            .visible()
            .into_iter()
            .map(|idx| picker.rows[idx].name.as_str())
            .collect()
    }

    // ── Rows ─────────────────────────────────────────────────────────────

    #[test]
    fn should_name_a_row_after_its_last_path_component_and_shorten_home() {
        // given / when
        let row = row("/home/me/code/tuicr", 7);

        // then
        assert_eq!(row.name, "tuicr");
        assert_eq!(row.display_path, "~/code/tuicr");
        assert_eq!(row.last_opened, 7);
        assert!(!row.exists, "the path is made up");
        assert!(!row.is_current);
    }

    #[test]
    fn should_mark_the_row_whose_path_is_the_current_repository() {
        // given
        let repo = RecentRepo {
            path: PathBuf::from("/work/api"),
            last_opened: 1,
        };

        // when
        let row = RepoRow::new(repo, Some(Path::new("/work/api")), None);

        // then
        assert!(row.is_current);
        assert_eq!(row.display_path, "/work/api");
    }

    // ── Default cursor ───────────────────────────────────────────────────

    #[test]
    fn should_start_on_the_most_recent_other_repository() {
        // given: the current repository is the most recent, as it always is
        let picker = picker(&["/w/current", "/w/previous", "/w/older"]);

        // then
        assert_eq!(picker.cursor, 1);
        assert_eq!(picker.selected().unwrap().name, "previous");
    }

    #[test]
    fn should_start_on_the_first_row_when_the_current_repository_is_not_listed_first() {
        // given: launched somewhere that is not the newest entry
        let mut picker = picker(&["/w/a", "/w/b", "/w/c"]);
        picker.rows[0].is_current = false;
        picker.rows[2].is_current = true;

        // when
        let cursor = picker.default_cursor();

        // then
        assert_eq!(cursor, 0);
    }

    #[test]
    fn should_pass_over_a_missing_repository_when_choosing_where_to_start() {
        // given
        let mut picker = picker(&["/w/current", "/w/gone", "/w/older"]);
        picker.rows[1].exists = false;

        // when
        let cursor = picker.default_cursor();

        // then
        assert_eq!(picker.rows[picker.visible()[cursor]].name, "older");
    }

    #[test]
    fn should_start_on_a_missing_repository_when_nothing_else_is_listed() {
        // given
        let mut picker = picker(&["/w/current", "/w/gone"]);
        picker.rows[1].exists = false;

        // when
        let cursor = picker.default_cursor();

        // then
        assert_eq!(cursor, 1);
    }

    #[test]
    fn should_start_on_the_only_row_when_it_is_the_current_repository() {
        // given
        let picker = picker(&["/w/current"]);

        // then
        assert_eq!(picker.cursor, 0);
    }

    #[test]
    fn should_select_nothing_in_an_empty_list() {
        // given
        let picker = picker(&[]);

        // then
        assert_eq!(picker.cursor, 0);
        assert!(picker.selected().is_none());
    }

    // ── Filtering and ranking ────────────────────────────────────────────

    #[test]
    fn should_list_every_row_in_recency_order_without_a_filter() {
        // given
        let picker = picker(&["/w/zeta", "/w/alpha", "/w/mid"]);

        // then
        assert_eq!(visible_names(&picker), ["zeta", "alpha", "mid"]);
    }

    #[test]
    fn should_match_a_subsequence_of_the_name() {
        assert!(repo_match_score("tcr", "tuicr", "~/code/tuicr").is_some());
        assert!(repo_match_score("TCR", "tuicr", "~/code/tuicr").is_some());
        assert!(repo_match_score("xyz", "tuicr", "~/code/tuicr").is_none());
        assert!(
            repo_match_score("rct", "tuicr", "~/code/tuicr").is_none(),
            "a subsequence keeps the order of its characters"
        );
    }

    #[test]
    fn should_rank_a_name_match_above_a_path_match() {
        // given: `api` is the older repository's name, and only a
        // directory on the way to the newer one
        let mut picker = picker(&["/w/current", "/w/api/server", "/w/tools/api"]);

        // when
        picker.filter = "api".to_string();

        // then
        assert_eq!(visible_names(&picker), ["api", "server"]);
    }

    #[test]
    fn should_rank_a_substring_above_a_subsequence() {
        // given
        let name_substring = repo_match_score("tui", "tuicr", "/w/tuicr").unwrap();
        let path_substring = repo_match_score("tui", "notes", "/w/tui/notes").unwrap();
        let name_subsequence = repo_match_score("tui", "tau-ui", "/w/tau-ui").unwrap();
        let path_subsequence = repo_match_score("tui", "notes", "/w/tau-ui/notes").unwrap();

        // then
        assert!(name_substring > path_substring);
        assert!(path_substring > name_subsequence);
        assert!(name_subsequence > path_subsequence);
    }

    #[test]
    fn should_keep_recency_order_among_equal_matches() {
        // given: the same name in three places, newest first
        let mut picker = picker(&["/a/web", "/b/web", "/c/web"]);

        // when
        picker.filter = "web".to_string();

        // then
        let paths: Vec<&str> = picker
            .visible()
            .into_iter()
            .map(|idx| picker.rows[idx].display_path.as_str())
            .collect();
        assert_eq!(paths, ["/a/web", "/b/web", "/c/web"]);
    }

    #[test]
    fn should_require_every_word_of_the_filter() {
        // given
        let mut picker = picker(&["/work/acme/api", "/home/me/api", "/work/acme/web"]);

        // when
        picker.filter = "api acme".to_string();

        // then
        let paths: Vec<&str> = picker
            .visible()
            .into_iter()
            .map(|idx| picker.rows[idx].display_path.as_str())
            .collect();
        assert_eq!(paths, ["/work/acme/api"]);
    }

    #[test]
    fn should_prefer_a_tighter_subsequence() {
        // given
        let tight = subsequence_score("tcr", "tuicr").unwrap();
        let spread = subsequence_score("tcr", "tzzzzzczzzzzr").unwrap();

        // then
        assert!(tight > spread, "{tight} vs {spread}");
    }

    #[test]
    fn should_reward_matches_at_the_start_of_words() {
        // given: the same three characters, as initials and buried in a word
        let initials = subsequence_score("tcr", "the-crate-registry").unwrap();
        let buried = subsequence_score("tcr", "attic-fire").unwrap();

        // then
        assert!(initials > buried, "{initials} vs {buried}");
    }

    #[test]
    fn should_keep_subsequence_scores_inside_their_tier() {
        // given: long enough that an unbounded score would cross a tier
        let word = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnop";

        // when
        let score = subsequence_score(word, word).unwrap();

        // then
        let far_apart = subsequence_score("ab", &format!("a{}b", "-".repeat(80))).unwrap();
        assert!((1..1000).contains(&score), "{score}");
        assert!((1..1000).contains(&far_apart), "{far_apart}");
    }

    #[test]
    fn should_try_every_start_for_a_subsequence() {
        // The first `t` of the path is far from the rest; the match that
        // starts at `tuicr` is the one that should count.
        let from_best_start = subsequence_score("tcr", "/t/x/x/x/x/x/x/x/tuicr").unwrap();
        let alone = subsequence_score("tcr", "/tuicr").unwrap();
        assert_eq!(from_best_start, alone);
    }

    #[test]
    fn should_match_everything_equally_for_an_empty_filter() {
        assert_eq!(repo_match_score("", "anything", "/at/all"), Some(0));
        assert_eq!(repo_match_score("   ", "anything", "/at/all"), Some(0));
    }

    // ── Cursor follows the filter ────────────────────────────────────────

    #[test]
    fn should_put_the_cursor_on_the_best_match_while_filtering() {
        // given
        let mut picker = picker(&["/w/current", "/w/previous", "/w/older"]);
        assert_eq!(picker.cursor, 1);

        // when
        picker.filter.push('o');
        picker.reset_cursor();

        // then
        assert_eq!(picker.cursor, 0);
    }

    #[test]
    fn should_return_to_the_starting_row_when_the_filter_is_emptied() {
        // given
        let mut picker = picker(&["/w/current", "/w/previous", "/w/older"]);
        picker.filter.push('o');
        picker.reset_cursor();

        // when
        picker.filter.clear();
        picker.reset_cursor();

        // then
        assert_eq!(picker.selected().unwrap().name, "previous");
    }

    // ── Paths ────────────────────────────────────────────────────────────

    #[test]
    fn should_expand_a_leading_tilde_only() {
        let home = Some(Path::new("/home/me"));
        assert_eq!(expand_tilde("~", home), PathBuf::from("/home/me"));
        assert_eq!(
            expand_tilde("~/code/x", home),
            PathBuf::from("/home/me/code/x")
        );
        assert_eq!(expand_tilde("/abs/~/x", home), PathBuf::from("/abs/~/x"));
        assert_eq!(expand_tilde("~other/x", home), PathBuf::from("~other/x"));
        assert_eq!(expand_tilde("rel/x", home), PathBuf::from("rel/x"));
        assert_eq!(expand_tilde("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn should_write_the_home_directory_as_a_tilde() {
        let home = Some(Path::new("/home/me"));
        assert_eq!(abbreviate_home(Path::new("/home/me"), home), "~");
        assert_eq!(abbreviate_home(Path::new("/home/me/a/b"), home), "~/a/b");
        assert_eq!(
            abbreviate_home(Path::new("/home/melanie/a"), home),
            "/home/melanie/a",
            "a sibling whose name merely starts the same is not under home"
        );
        assert_eq!(abbreviate_home(Path::new("/srv/x"), home), "/srv/x");
        assert_eq!(abbreviate_home(Path::new("/home/me/a"), None), "/home/me/a");
    }

    #[test]
    fn should_name_a_repository_after_its_directory() {
        assert_eq!(repo_name(Path::new("/w/tuicr")), "tuicr");
        assert_eq!(repo_name(Path::new("/w/tuicr/")), "tuicr");
        assert_eq!(repo_name(Path::new("/")), "/");
    }

    /// `home/` with `Software/`, `Sources/`, `.config/`, `notes.txt`, and
    /// `work/` with `app/`.
    fn completion_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for sub in [
            "home/Software/tuicr",
            "home/Sources",
            "home/.config",
            "work/app",
        ] {
            std::fs::create_dir_all(dir.path().join(sub)).expect("create dir");
        }
        std::fs::write(dir.path().join("home/notes.txt"), "x").expect("write file");
        dir
    }

    #[test]
    fn should_complete_directories_under_a_tilde_path() {
        // given
        let dir = completion_fixture();
        let home = dir.path().join("home");
        let base = dir.path().join("work");

        // when
        let matches = complete_dir("~/So", Some(&home), &base);

        // then: sorted, spelled as typed, ready for the next Tab
        assert_eq!(matches, ["~/Software/", "~/Sources/"]);
    }

    #[test]
    fn should_complete_only_directories_and_hide_dot_directories() {
        // given
        let dir = completion_fixture();
        let home = dir.path().join("home");
        let base = dir.path().join("work");

        // when
        let everything = complete_dir("~/", Some(&home), &base);
        let hidden = complete_dir("~/.", Some(&home), &base);

        // then: no `notes.txt`, and `.config` only when asked for
        assert_eq!(everything, ["~/Software/", "~/Sources/"]);
        assert_eq!(hidden, ["~/.config/"]);
    }

    #[test]
    fn should_complete_relative_and_absolute_paths() {
        // given
        let dir = completion_fixture();
        let home = dir.path().join("home");
        let base = dir.path().join("work");
        let absolute = format!("{}/Sof", home.display());

        // when
        let relative = complete_dir("ap", Some(&home), &base);
        let nested = complete_dir("../home/Software/tu", Some(&home), &base);
        let from_root = complete_dir(&absolute, Some(&home), &base);

        // then
        assert_eq!(relative, ["app/"]);
        assert_eq!(nested, ["../home/Software/tuicr/"]);
        assert_eq!(from_root, [format!("{}/Software/", home.display())]);
    }

    #[test]
    fn should_forgive_case_only_when_nothing_matches_as_typed() {
        // given
        let dir = completion_fixture();
        let home = dir.path().join("home");
        let base = dir.path().join("work");

        // when
        let lowered = complete_dir("~/soft", Some(&home), &base);
        let exact = complete_dir("~/So", Some(&home), &base);

        // then
        assert_eq!(lowered, ["~/Software/"]);
        assert_eq!(exact.len(), 2);
    }

    #[test]
    fn should_offer_nothing_for_a_directory_that_cannot_be_read() {
        // given
        let dir = completion_fixture();
        let home = dir.path().join("home");
        let base = dir.path().join("work");

        // then
        assert!(complete_dir("~/nowhere/x", Some(&home), &base).is_empty());
        assert!(complete_dir("~/So", None, &base).is_empty());
        assert_eq!(complete_dir("~", Some(&home), &base), ["~/"]);
    }
}
