//! Fetch-all (`:fetchall`, `Ctrl-f` in the repo switcher): fetch every
//! remote of every repository on the recent list, a few at a time on worker
//! threads, while the status bar counts them off.
//!
//! The main thread only plans (filesystem probes, no git) and installs what
//! the workers send back ([`App::poll_fetch_all`], once per tick). A worker
//! is handed a repository root and runs git there
//! ([`fetch_remotes`](crate::vcs::git::fetch::fetch_remotes)); it never
//! opens a backend and never reads the process's working directory, which
//! the repo switcher may move at any time.
//!
//! Checkouts that share one repository (a repository and its linked
//! worktrees) share its refs, and two fetches into the same refs fight over
//! the ref locks. They are fetched once, as a group, and every row of the
//! group shows the one result.
//!
//! The job belongs to the process, not to the repository on screen: a repo
//! switch carries it over (`App::carry_process_state_from`). So nothing
//! here remembers which repository was current when the run started; that
//! is asked when a result arrives.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::repos::repo_name;
use super::{App, DiffSource, InputMode};
use crate::error::Result;
use crate::persistence::recent_repos;
use crate::vcs::git::fetch::{FetchMode, FetchOutcome, fetch_remotes, git_common_dir};

/// Repositories fetched at once.
pub(crate) const FETCH_ALL_WORKERS: usize = 4;
/// Failed repositories named in the summary line before "and N more".
const MAX_FAILED_NAMES: usize = 5;
/// How long a finished run's results stay worth showing on the repo
/// switcher's rows. Past this, "up to date" is no longer a fact.
const RESULTS_SHOWN_FOR: Duration = Duration::from_secs(10 * 60);

/// Where one repository stands in a fetch-all run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoFetchState {
    /// Waiting for a worker.
    Queued,
    Fetching,
    /// Fetched, and a remote-tracking ref changed.
    Updated,
    /// Fetched, and nothing was new.
    UpToDate,
    /// Not fetched, and not a failure: the reason says why.
    Skipped(&'static str),
    /// A remote, or the repository itself, could not be fetched.
    Failed(String),
}

impl RepoFetchState {
    /// Whether the run is done with this repository.
    pub fn is_settled(&self) -> bool {
        !matches!(self, Self::Queued | Self::Fetching)
    }
}

/// One repository of a fetch-all run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchAllRow {
    /// The root as the recent list stores it.
    pub path: PathBuf,
    pub name: String,
    pub state: RepoFetchState,
}

/// Checkouts of one repository, fetched together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FetchGroup {
    /// The repository's own `.git` directory, which the checkouts share.
    pub(crate) common_dir: PathBuf,
    /// Indices into [`FetchAllJob::rows`]; the fetch runs in the first.
    pub(crate) rows: Vec<usize>,
}

/// What a worker reports about a group, by its index.
pub(crate) enum FetchAllEvent {
    Started(usize),
    Finished(usize, std::result::Result<FetchOutcome, String>),
}

/// A fetch-all run, in flight or finished. A finished one is kept so the
/// repo switcher can show each repository's result: until that picker
/// closes, or until the results have gone stale unseen.
pub struct FetchAllJob {
    /// Every repository on the recent list when the run started, most
    /// recent first.
    pub rows: Vec<FetchAllRow>,
    groups: Vec<FetchGroup>,
    /// Rows that were queued when the run started.
    total: usize,
    /// `Some` while workers may still report.
    rx: Option<Receiver<FetchAllEvent>>,
    /// When the run ended; `None` while it is going.
    pub(crate) finished_at: Option<Instant>,
}

impl FetchAllJob {
    /// A run over `rows`, and the sender its workers report through. With
    /// nothing queued the run is already over.
    pub(crate) fn new(
        rows: Vec<FetchAllRow>,
        groups: Vec<FetchGroup>,
    ) -> (Self, Sender<FetchAllEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let total = rows
            .iter()
            .filter(|row| row.state == RepoFetchState::Queued)
            .count();
        let job = Self {
            rows,
            groups,
            total,
            rx: (total > 0).then_some(rx),
            finished_at: (total == 0).then(Instant::now),
        };
        (job, tx)
    }

    pub fn running(&self) -> bool {
        self.rx.is_some()
    }

    /// `(repositories done, repositories that were queued)`.
    pub fn progress(&self) -> (usize, usize) {
        let waiting = self
            .rows
            .iter()
            .filter(|row| !row.state.is_settled())
            .count();
        (self.total.saturating_sub(waiting), self.total)
    }

    /// Where the repository listed at `path` stands, if it is part of
    /// this run. `path` is compared as the recent list spells it.
    pub fn state_for(&self, path: &Path) -> Option<&RepoFetchState> {
        self.rows
            .iter()
            .find(|row| row.path == path)
            .map(|row| &row.state)
    }

    /// The first checkout of each group, which is where its fetch runs.
    fn work(&self) -> Vec<(usize, PathBuf)> {
        self.groups
            .iter()
            .enumerate()
            .filter_map(|(idx, group)| {
                let row = *group.rows.first()?;
                Some((idx, self.rows[row].path.clone()))
            })
            .collect()
    }

    /// Whether the run is still waiting on, or fetching into, the
    /// repository whose `.git` directory is `common_dir`.
    fn busy_in(&self, common_dir: &Path) -> bool {
        self.running()
            && self.groups.iter().any(|group| {
                group.common_dir == common_dir
                    && group
                        .rows
                        .iter()
                        .any(|&row| !self.rows[row].state.is_settled())
            })
    }

    fn set_group(&mut self, group: usize, state: &RepoFetchState) {
        let Some(group) = self.groups.get(group) else {
            return;
        };
        for &row in &group.rows {
            self.rows[row].state = state.clone();
        }
    }

    /// Install one worker report. Returns the group's repository when the
    /// fetch changed its remote-tracking refs.
    fn apply(&mut self, event: FetchAllEvent) -> Option<PathBuf> {
        match event {
            FetchAllEvent::Started(group) => {
                self.set_group(group, &RepoFetchState::Fetching);
                None
            }
            FetchAllEvent::Finished(group, result) => {
                let (state, updated) = match result {
                    Ok(FetchOutcome::NoRemotes) => (RepoFetchState::Skipped("no remotes"), false),
                    Ok(FetchOutcome::Done(report)) => {
                        let state = if !report.failures.is_empty() {
                            let reasons: Vec<String> = report
                                .failures
                                .iter()
                                .map(|(remote, reason)| {
                                    format!("{remote}: {}", plain_reason(reason))
                                })
                                .collect();
                            RepoFetchState::Failed(reasons.join(" \u{00b7} "))
                        } else if report.updated() {
                            RepoFetchState::Updated
                        } else {
                            RepoFetchState::UpToDate
                        };
                        // A repository with one dead remote may still have
                        // taken new commits from another.
                        (state, report.updated())
                    }
                    Err(reason) => (RepoFetchState::Failed(plain_reason(&reason)), false),
                };
                self.set_group(group, &state);
                updated
                    .then(|| self.groups.get(group).map(|g| g.common_dir.clone()))
                    .flatten()
            }
        }
    }

    /// Close the run. A repository no worker reported on (its worker died)
    /// counts as failed rather than being left "fetching" for good.
    fn finish(&mut self) {
        self.rx = None;
        self.finished_at = Some(Instant::now());
        for row in &mut self.rows {
            if !row.state.is_settled() {
                row.state = RepoFetchState::Failed("worker stopped".to_string());
            }
        }
    }
}

/// The first line of an error, without the wrapper every git failure
/// carries.
fn plain_reason(reason: &str) -> String {
    let line = reason
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("failed");
    line.strip_prefix("VCS command failed: ")
        .unwrap_or(line)
        .to_string()
}

/// Sort the recent repositories into what will be fetched and what will
/// not, using nothing but the filesystem. Checkouts of one repository form
/// one group, and the group of the repository on screen (`current`, its
/// root) comes first so its refresh is not kept waiting.
///
/// A repository is taken for what tuicr would open it as: Jujutsu before
/// Git before Mercurial. Only Git has a fetch here.
pub(crate) fn plan_fetch_all(
    paths: &[PathBuf],
    current: Option<&Path>,
) -> (Vec<FetchAllRow>, Vec<FetchGroup>) {
    let mut rows: Vec<FetchAllRow> = Vec::with_capacity(paths.len());
    let mut groups: Vec<FetchGroup> = Vec::new();
    for (idx, path) in paths.iter().enumerate() {
        let state = if !path.is_absolute() {
            // Only a hand-edited list holds one. A worker must not resolve
            // a path against the process's working directory.
            RepoFetchState::Skipped("not an absolute path")
        } else if !path.is_dir() {
            RepoFetchState::Skipped("missing")
        } else if path.join(".jj").exists() {
            RepoFetchState::Skipped("jj repository")
        } else if let Some(common_dir) = git_common_dir(path) {
            match groups
                .iter_mut()
                .find(|group| group.common_dir == common_dir)
            {
                Some(group) => group.rows.push(idx),
                None => groups.push(FetchGroup {
                    common_dir,
                    rows: vec![idx],
                }),
            }
            RepoFetchState::Queued
        } else if path.join(".hg").exists() {
            RepoFetchState::Skipped("hg repository")
        } else {
            RepoFetchState::Skipped("not a Git repository")
        };
        rows.push(FetchAllRow {
            path: path.clone(),
            name: repo_name(path),
            state,
        });
    }
    if let Some(current) = current.and_then(git_common_dir)
        && let Some(at) = groups.iter().position(|group| group.common_dir == current)
    {
        let group = groups.remove(at);
        groups.insert(0, group);
    }
    (rows, groups)
}

/// Fetch `work` (group index, checkout root) on at most `workers` detached
/// threads, reporting through `tx`. Nothing here keeps a sender once the
/// threads are started, so the channel disconnects when the last worker
/// is done. A worker whose reports are no longer wanted (the job was
/// dropped) starts nothing further.
pub(crate) fn run_fetch_pool<F>(
    work: Vec<(usize, PathBuf)>,
    workers: usize,
    tx: Sender<FetchAllEvent>,
    fetch: F,
) where
    F: Fn(&Path) -> Result<FetchOutcome> + Send + Sync + 'static,
{
    let threads = workers.min(work.len());
    let queue = Arc::new(Mutex::new(VecDeque::from(work)));
    let fetch = Arc::new(fetch);
    for _ in 0..threads {
        let queue = Arc::clone(&queue);
        let fetch = Arc::clone(&fetch);
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                let next = queue
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pop_front();
                let Some((group, root)) = next else {
                    break;
                };
                if tx.send(FetchAllEvent::Started(group)).is_err() {
                    break;
                }
                let result = fetch(&root).map_err(|e| e.to_string());
                if tx.send(FetchAllEvent::Finished(group, result)).is_err() {
                    break;
                }
            }
        });
    }
}

/// The one line a finished run leaves in the status bar, and whether any
/// repository failed (which makes it an error that stays on screen).
pub(crate) fn summarize(rows: &[FetchAllRow]) -> (String, bool) {
    let count =
        |wanted: fn(&RepoFetchState) -> bool| rows.iter().filter(|row| wanted(&row.state)).count();
    let updated = count(|state| *state == RepoFetchState::Updated);
    let up_to_date = count(|state| *state == RepoFetchState::UpToDate);
    let skipped = count(|state| matches!(state, RepoFetchState::Skipped(_)));
    let failed: Vec<&str> = rows
        .iter()
        .filter(|row| matches!(row.state, RepoFetchState::Failed(_)))
        .map(|row| row.name.as_str())
        .collect();
    let fetched = updated + up_to_date + failed.len();
    let repositories = |n: usize| {
        if n == 1 {
            "1 repository".to_string()
        } else {
            format!("{n} repositories")
        }
    };

    if fetched == 0 {
        return (
            format!("Nothing to fetch: {} skipped", repositories(skipped)),
            false,
        );
    }
    let mut text = format!("Fetched {}: {updated} updated", repositories(fetched));
    if failed.is_empty() {
        if skipped > 0 {
            text.push_str(&format!(" \u{00b7} {skipped} skipped"));
        }
        return (text, false);
    }
    let shown = failed.len().min(MAX_FAILED_NAMES);
    let mut names = failed[..shown].join(", ");
    if failed.len() > shown {
        names.push_str(&format!(" and {} more", failed.len() - shown));
    }
    text.push_str(&format!(
        ", {} failed ({names}). Reasons: :console",
        failed.len()
    ));
    (text, true)
}

impl App {
    /// `(done, total)` while a fetch-all is running, for the status bar.
    pub fn fetch_all_progress(&self) -> Option<(usize, usize)> {
        self.fetch_all
            .as_ref()
            .filter(|job| job.running())
            .map(FetchAllJob::progress)
    }

    /// Whether a running fetch-all has yet to finish with the repository
    /// on screen. A fetch, pull or push started here meanwhile would
    /// collide with it over the ref locks.
    pub(in crate::app) fn fetch_all_busy_here(&self) -> bool {
        let Some(job) = self.fetch_all.as_ref().filter(|job| job.running()) else {
            return false;
        };
        self.current_repo_root()
            .and_then(git_common_dir)
            .is_some_and(|common_dir| job.busy_in(&common_dir))
    }

    /// `:fetchall` / `Ctrl-f` in the repo switcher: fetch every remote of
    /// every recent repository in the background. One run at a time, and
    /// not while a fetch, pull or push of this repository is in flight.
    pub fn start_fetch_all(&mut self) {
        if let Some((done, total)) = self.fetch_all_progress() {
            self.set_warning(format!("Fetch-all already running ({done}/{total})"));
            return;
        }
        if let Some(running) = self.remote_op_running() {
            self.set_warning(format!(
                "{} already \u{2014} wait for it to finish",
                running.verb()
            ));
            return;
        }
        let paths: Vec<PathBuf> = self
            .recent_repos_path
            .as_deref()
            .map(recent_repos::load)
            .unwrap_or_default()
            .into_iter()
            .map(|repo| repo.path)
            .collect();
        if paths.is_empty() {
            self.set_warning("No recent repositories to fetch");
            return;
        }
        let (rows, groups) = plan_fetch_all(&paths, self.current_repo_root());
        let (job, tx) = FetchAllJob::new(rows, groups);
        let work = job.work();
        if work.is_empty() {
            // Everything on the list is gone, or not Git.
            let (summary, _) = summarize(&job.rows);
            self.fetch_all = Some(job);
            self.set_message(summary);
            return;
        }
        self.fetch_all = Some(job);
        // The count takes the message's place in the status bar.
        self.message = None;
        run_fetch_pool(work, FETCH_ALL_WORKERS, tx, |root| {
            fetch_remotes(root, FetchMode::Batch)
        });
    }

    /// Main-loop tick: install what the fetch-all workers reported since
    /// the last one. Returns whether the screen changed.
    pub fn poll_fetch_all(&mut self) -> bool {
        let Some(job) = self.fetch_all.as_mut() else {
            return false;
        };
        let Some(rx) = job.rx.as_ref() else {
            return false;
        };
        // Everything waiting is taken first: installing a result needs the
        // job mutably, which the receiver is part of.
        let mut events = Vec::new();
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if events.is_empty() && !disconnected {
            return false;
        }
        let updated: Vec<PathBuf> = events
            .into_iter()
            .filter_map(|event| job.apply(event))
            .collect();
        let over = disconnected || job.rows.iter().all(|row| row.state.is_settled());
        let summary = over.then(|| {
            job.finish();
            summarize(&job.rows)
        });

        self.refresh_after_fetch_all(&updated);
        match summary {
            Some((text, true)) => self.set_error(text),
            Some((text, false)) => self.set_message(text),
            None => {}
        }
        true
    }

    /// The repositories whose `.git` directories are in `updated` took new
    /// remote-tracking refs. If the one on screen is among them, refresh
    /// it the way a finished `:fetch` does. Which repository is on screen
    /// is asked now, not when the run started: there may have been a
    /// switch since.
    fn refresh_after_fetch_all(&mut self, updated: &[PathBuf]) {
        if updated.is_empty() || matches!(self.diff_source, DiffSource::PullRequest(_)) {
            // A pull request review has no local repository behind it to
            // reopen.
            return;
        }
        let here = self.current_repo_root().and_then(git_common_dir);
        if !here.is_some_and(|common_dir| updated.contains(&common_dir)) {
            return;
        }
        // The refresh speaks for itself on screen. After `:fetch` whatever
        // it reports is replaced by the fetch summary; here it must not
        // take the place of the running count either.
        let message = self.message.take();
        self.after_head_moved();
        match self.input_mode {
            InputMode::BranchPicker => self.reopen_branch_picker_in_place(),
            // `after_head_moved` refreshes the selector's rows only when
            // the selector is the mode on top.
            InputMode::RepoPicker
                if self
                    .repo_picker
                    .as_ref()
                    .is_some_and(|picker| picker.return_mode == InputMode::CommitSelect) =>
            {
                self.refresh_commit_selector_rows();
            }
            _ => {}
        }
        self.message = message;
    }

    /// The repo switcher closed: a finished run's results were on its
    /// rows, and shown again later they would be stale.
    pub(in crate::app) fn forget_finished_fetch_all(&mut self) {
        if self.fetch_all.as_ref().is_some_and(|job| !job.running()) {
            self.fetch_all = None;
        }
    }

    /// The repo switcher is opening: a run that finished a while ago
    /// without the picker ever showing it has nothing true left to say.
    pub(in crate::app) fn forget_stale_fetch_all(&mut self) {
        self.forget_fetch_all_finished_before(Instant::now());
    }

    /// Drop a run whose results were already too old to show at `now`.
    pub(in crate::app) fn forget_fetch_all_finished_before(&mut self, now: Instant) {
        let stale = self
            .fetch_all
            .as_ref()
            .and_then(|job| job.finished_at)
            .is_some_and(|at| now.saturating_duration_since(at) > RESULTS_SHOWN_FOR);
        if stale {
            self.fetch_all = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::vcs::git::fetch::FetchReport;

    fn row(name: &str, state: RepoFetchState) -> FetchAllRow {
        FetchAllRow {
            path: PathBuf::from(format!("/w/{name}")),
            name: name.to_string(),
            state,
        }
    }

    fn failed(reason: &str) -> RepoFetchState {
        RepoFetchState::Failed(reason.to_string())
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A Git repository at `base/name` with one commit.
    fn repo(base: &Path, name: &str) -> PathBuf {
        let root = base.join(name);
        std::fs::create_dir_all(&root).expect("mkdir");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "tester"]);
        git(&root, &["config", "user.email", "tester@example.com"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("f.txt"), "one\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "one"]);
        root
    }

    // ── Planning ─────────────────────────────────────────────────────────

    #[test]
    fn should_queue_git_repositories_and_say_why_the_rest_are_skipped() {
        // given: one of everything the list can hold
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path();
        let alpha = repo(base, "alpha");
        let jj = repo(base, "colocated");
        std::fs::create_dir(jj.join(".jj")).expect("mkdir");
        let hg = base.join("mercurial");
        std::fs::create_dir_all(hg.join(".hg")).expect("mkdir");
        let plain = base.join("plain");
        std::fs::create_dir(&plain).expect("mkdir");
        let gone = base.join("gone");
        let relative = PathBuf::from("src");
        assert!(relative.is_dir(), "tests run from the crate root");
        let paths = [alpha, jj, hg, plain, gone, relative];

        // when
        let (rows, groups) = plan_fetch_all(&paths, None);

        // then
        let states: Vec<(&str, &RepoFetchState)> = rows
            .iter()
            .map(|row| (row.name.as_str(), &row.state))
            .collect();
        assert_eq!(
            states,
            [
                ("alpha", &RepoFetchState::Queued),
                ("colocated", &RepoFetchState::Skipped("jj repository")),
                ("mercurial", &RepoFetchState::Skipped("hg repository")),
                ("plain", &RepoFetchState::Skipped("not a Git repository")),
                ("gone", &RepoFetchState::Skipped("missing")),
                // never resolved against wherever the process happens to be
                ("src", &RepoFetchState::Skipped("not an absolute path")),
            ]
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].rows, [0]);
        assert_eq!(
            rows[0].path, paths[0],
            "paths are kept as the list has them"
        );
    }

    #[test]
    fn should_fetch_a_repository_and_its_worktrees_as_one_group() {
        // given: beta, alpha, and a linked worktree of alpha
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path();
        let alpha = repo(base, "alpha");
        let beta = repo(base, "beta");
        let worktree = base.join("alpha-wt");
        git(
            &alpha,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                &worktree.to_string_lossy(),
            ],
        );
        let paths = [beta, alpha, worktree];

        // when
        let (rows, groups) = plan_fetch_all(&paths, None);

        // then: every row is queued, in two groups, in list order
        assert!(rows.iter().all(|row| row.state == RepoFetchState::Queued));
        let grouped: Vec<&[usize]> = groups.iter().map(|group| group.rows.as_slice()).collect();
        assert_eq!(grouped, [&[0][..], &[1, 2][..]]);
    }

    #[test]
    fn should_put_the_current_repository_first_in_line() {
        // given: the repository on screen is the worktree, listed last
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path();
        let alpha = repo(base, "alpha");
        let beta = repo(base, "beta");
        let gamma = repo(base, "gamma");
        let worktree = base.join("gamma-wt");
        git(
            &gamma,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                &worktree.to_string_lossy(),
            ],
        );
        let paths = [alpha, beta, gamma, worktree.clone()];

        // when
        let (_, groups) = plan_fetch_all(&paths, Some(&worktree));
        let (_, unordered) = plan_fetch_all(&paths, None);
        let (_, elsewhere) = plan_fetch_all(&paths, Some(base));

        // then: gamma's group moved to the front, the rest kept their order
        let order = |groups: &[FetchGroup]| -> Vec<Vec<usize>> {
            groups.iter().map(|group| group.rows.clone()).collect()
        };
        assert_eq!(order(&groups), [vec![2, 3], vec![0], vec![1]]);
        assert_eq!(order(&unordered), [vec![0], vec![1], vec![2, 3]]);
        assert_eq!(order(&elsewhere), order(&unordered));
    }

    // ── The job ──────────────────────────────────────────────────────────

    fn job_of(states: &[(&str, RepoFetchState)], groups: &[&[usize]]) -> FetchAllJob {
        let rows = states
            .iter()
            .map(|(name, state)| row(name, state.clone()))
            .collect();
        let groups = groups
            .iter()
            .enumerate()
            .map(|(idx, rows)| FetchGroup {
                common_dir: PathBuf::from(format!("/git/{idx}")),
                rows: rows.to_vec(),
            })
            .collect();
        FetchAllJob::new(rows, groups).0
    }

    fn done(moved: &[&str], failures: &[(&str, &str)]) -> FetchOutcome {
        FetchOutcome::Done(FetchReport {
            remotes: 1.max(failures.len()),
            moved: moved.iter().map(|s| s.to_string()).collect(),
            pruned: 0,
            failures: failures
                .iter()
                .map(|(remote, reason)| (remote.to_string(), reason.to_string()))
                .collect(),
        })
    }

    #[test]
    fn should_count_only_queued_repositories_as_progress() {
        // given: two to fetch (one of them with a worktree) and one skipped
        let mut job = job_of(
            &[
                ("a", RepoFetchState::Queued),
                ("a-wt", RepoFetchState::Queued),
                ("gone", RepoFetchState::Skipped("missing")),
                ("b", RepoFetchState::Queued),
            ],
            &[&[0, 1], &[3]],
        );
        assert!(job.running());
        assert_eq!(job.progress(), (0, 3));

        // when a group starts, nothing is done yet
        job.apply(FetchAllEvent::Started(0));
        assert_eq!(job.rows[0].state, RepoFetchState::Fetching);
        assert_eq!(job.rows[1].state, RepoFetchState::Fetching);
        assert_eq!(job.progress(), (0, 3));

        // and when it finishes, every checkout of the group is
        let updated = job.apply(FetchAllEvent::Finished(
            0,
            Ok(done(&["origin/main +1"], &[])),
        ));
        assert_eq!(updated, Some(PathBuf::from("/git/0")));
        assert_eq!(job.rows[0].state, RepoFetchState::Updated);
        assert_eq!(job.rows[1].state, RepoFetchState::Updated);
        assert_eq!(job.progress(), (2, 3));
        assert_eq!(
            job.state_for(Path::new("/w/a-wt")),
            Some(&RepoFetchState::Updated)
        );
        assert_eq!(job.state_for(Path::new("/w/unlisted")), None);
    }

    #[test]
    fn should_turn_each_outcome_into_a_row_state() {
        // given
        let mut job = job_of(
            &[
                ("same", RepoFetchState::Queued),
                ("bare", RepoFetchState::Queued),
                ("dead", RepoFetchState::Queued),
                ("half", RepoFetchState::Queued),
                ("broken", RepoFetchState::Queued),
            ],
            &[&[0], &[1], &[2], &[3], &[4]],
        );

        // when
        let same = job.apply(FetchAllEvent::Finished(0, Ok(done(&[], &[]))));
        let bare = job.apply(FetchAllEvent::Finished(1, Ok(FetchOutcome::NoRemotes)));
        let dead = job.apply(FetchAllEvent::Finished(
            2,
            Ok(done(
                &[],
                &[("origin", "VCS command failed: fatal: no such host")],
            )),
        ));
        let half = job.apply(FetchAllEvent::Finished(
            3,
            Ok(done(
                &["origin/main +2"],
                &[("backup", "fatal: refused"), ("mirror", "timed out")],
            )),
        ));
        let broken = job.apply(FetchAllEvent::Finished(
            4,
            Err("VCS command failed: fatal: not a git repository\nmore".to_string()),
        ));

        // then
        let states: Vec<&RepoFetchState> = job.rows.iter().map(|row| &row.state).collect();
        assert_eq!(
            states,
            [
                &RepoFetchState::UpToDate,
                &RepoFetchState::Skipped("no remotes"),
                &failed("origin: fatal: no such host"),
                &failed("backup: fatal: refused \u{00b7} mirror: timed out"),
                &failed("fatal: not a git repository"),
            ]
        );
        // only a repository whose refs moved asks for a refresh, and one
        // with a dead remote still does when another remote delivered
        assert_eq!(
            [same, bare, dead, broken],
            [None, None, None, None],
            "nothing moved in these"
        );
        assert_eq!(half, Some(PathBuf::from("/git/3")));
    }

    #[test]
    fn should_fail_what_no_worker_reported_on_when_the_run_is_closed() {
        // given: one repository still being fetched when the workers died
        let mut job = job_of(
            &[
                ("a", RepoFetchState::Queued),
                ("b", RepoFetchState::Queued),
                ("gone", RepoFetchState::Skipped("missing")),
            ],
            &[&[0], &[1]],
        );
        job.apply(FetchAllEvent::Finished(0, Ok(done(&[], &[]))));
        job.apply(FetchAllEvent::Started(1));

        // when
        job.finish();

        // then
        assert!(!job.running());
        assert!(job.finished_at.is_some());
        assert_eq!(job.rows[0].state, RepoFetchState::UpToDate);
        assert_eq!(job.rows[1].state, failed("worker stopped"));
        assert_eq!(job.rows[2].state, RepoFetchState::Skipped("missing"));
        assert_eq!(job.progress(), (2, 2));
    }

    #[test]
    fn should_be_over_at_once_when_nothing_is_queued() {
        // given / when
        let job = job_of(&[("gone", RepoFetchState::Skipped("missing"))], &[]);

        // then
        assert!(!job.running());
        assert!(job.finished_at.is_some());
        assert_eq!(job.progress(), (0, 0));
        assert!(job.work().is_empty());
    }

    #[test]
    fn should_be_busy_in_a_repository_until_its_group_settles() {
        // given
        let mut job = job_of(
            &[("a", RepoFetchState::Queued), ("b", RepoFetchState::Queued)],
            &[&[0], &[1]],
        );
        let a = Path::new("/git/0");

        // then: while queued and while fetching, not once it is done
        assert!(job.busy_in(a));
        job.apply(FetchAllEvent::Started(0));
        assert!(job.busy_in(a));
        job.apply(FetchAllEvent::Finished(0, Ok(done(&[], &[]))));
        assert!(!job.busy_in(a));
        assert!(job.busy_in(Path::new("/git/1")));
        assert!(!job.busy_in(Path::new("/git/elsewhere")));
    }

    // ── The pool ─────────────────────────────────────────────────────────

    #[test]
    fn should_fetch_every_group_once_and_never_more_than_four_at_a_time() {
        // given: twenty groups, and a fetch that notes how many run at once
        let work: Vec<(usize, PathBuf)> = (0..20)
            .map(|idx| (idx, PathBuf::from(format!("/w/repo-{idx}"))))
            .collect();
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let (running_in, peak_in) = (Arc::clone(&running), Arc::clone(&peak));

        // when
        run_fetch_pool(work, FETCH_ALL_WORKERS, tx, move |root| {
            let now = running_in.fetch_add(1, Ordering::SeqCst) + 1;
            peak_in.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(15));
            running_in.fetch_sub(1, Ordering::SeqCst);
            if root.ends_with("repo-7") {
                Err(crate::error::TuicrError::VcsCommand("boom".to_string()))
            } else {
                Ok(FetchOutcome::NoRemotes)
            }
        });

        // then: the channel closes by itself once the workers are done
        let mut started = Vec::new();
        let mut finished = Vec::new();
        let mut errors = Vec::new();
        while let Ok(event) = rx.recv_timeout(Duration::from_secs(20)) {
            match event {
                FetchAllEvent::Started(group) => started.push(group),
                FetchAllEvent::Finished(group, result) => {
                    assert!(started.contains(&group), "finished before it started");
                    if let Err(e) = result {
                        errors.push((group, e));
                    }
                    finished.push(group);
                }
            }
        }
        started.sort_unstable();
        finished.sort_unstable();
        let every: Vec<usize> = (0..20).collect();
        assert_eq!(started, every);
        assert_eq!(finished, every);
        assert_eq!(errors, [(7, "VCS command failed: boom".to_string())]);
        let peak = peak.load(Ordering::SeqCst);
        assert!(
            (2..=FETCH_ALL_WORKERS).contains(&peak),
            "{peak} fetches ran at once"
        );
    }

    #[test]
    fn should_start_no_more_threads_than_there_is_work() {
        // given: one group
        let (tx, rx) = std::sync::mpsc::channel();

        // when
        run_fetch_pool(vec![(0, PathBuf::from("/w/only"))], 4, tx, |_| {
            Ok(FetchOutcome::NoRemotes)
        });

        // then: one start, one finish, and the channel closes
        let events: Vec<FetchAllEvent> = rx.iter().collect();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], FetchAllEvent::Started(0)));
        assert!(matches!(events[1], FetchAllEvent::Finished(0, Ok(_))));
    }

    #[test]
    fn should_stop_taking_work_once_nobody_is_listening() {
        // given: the job was dropped before the workers got going
        let fetched = Arc::new(AtomicUsize::new(0));
        let fetched_in = Arc::clone(&fetched);
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let work: Vec<(usize, PathBuf)> = (0..50)
            .map(|idx| (idx, PathBuf::from(format!("/w/{idx}"))))
            .collect();

        // when
        run_fetch_pool(work, 4, tx, move |_| {
            fetched_in.fetch_add(1, Ordering::SeqCst);
            Ok(FetchOutcome::NoRemotes)
        });
        std::thread::sleep(Duration::from_millis(200));

        // then: no worker fetched anything, since none could report a start
        assert_eq!(fetched.load(Ordering::SeqCst), 0);
    }

    // ── The summary ──────────────────────────────────────────────────────

    #[test]
    fn should_summarize_a_clean_run() {
        // given
        let rows = [
            row("a", RepoFetchState::Updated),
            row("b", RepoFetchState::UpToDate),
            row("c", RepoFetchState::UpToDate),
        ];

        // when
        let (text, any_failed) = summarize(&rows);

        // then
        assert_eq!(text, "Fetched 3 repositories: 1 updated");
        assert!(!any_failed);
    }

    #[test]
    fn should_count_what_was_skipped_without_calling_it_a_failure() {
        // given
        let rows = [
            row("a", RepoFetchState::Updated),
            row("gone", RepoFetchState::Skipped("missing")),
            row("local", RepoFetchState::Skipped("no remotes")),
        ];

        // when
        let (text, any_failed) = summarize(&rows);

        // then
        assert_eq!(text, "Fetched 1 repository: 1 updated \u{00b7} 2 skipped");
        assert!(!any_failed);
    }

    #[test]
    fn should_name_the_repositories_that_failed() {
        // given
        let rows = [
            row("a", RepoFetchState::Updated),
            row("b", failed("origin: no route")),
            row("c", RepoFetchState::UpToDate),
            row("d", failed("origin: denied")),
            row("gone", RepoFetchState::Skipped("missing")),
        ];

        // when
        let (text, any_failed) = summarize(&rows);

        // then
        assert_eq!(
            text,
            "Fetched 4 repositories: 1 updated, 2 failed (b, d). Reasons: :console"
        );
        assert!(any_failed);
    }

    #[test]
    fn should_cap_the_failed_names() {
        // given
        let rows: Vec<FetchAllRow> = (0..8)
            .map(|idx| row(&format!("r{idx}"), failed("x")))
            .collect();

        // when
        let (text, _) = summarize(&rows);

        // then
        assert_eq!(
            text,
            "Fetched 8 repositories: 0 updated, 8 failed (r0, r1, r2, r3, r4 and 3 more). \
             Reasons: :console"
        );
    }

    #[test]
    fn should_say_so_when_there_was_nothing_to_fetch() {
        // given
        let rows = [
            row("gone", RepoFetchState::Skipped("missing")),
            row("local", RepoFetchState::Skipped("no remotes")),
        ];

        // when
        let (text, any_failed) = summarize(&rows);

        // then
        assert_eq!(text, "Nothing to fetch: 2 repositories skipped");
        assert!(!any_failed);
    }

    #[test]
    fn should_keep_the_first_line_of_a_reason_without_the_wrapper() {
        assert_eq!(
            plain_reason("VCS command failed: fatal: no\nhint: try"),
            "fatal: no"
        );
        assert_eq!(plain_reason("\n  timed out  \n"), "timed out");
        assert_eq!(plain_reason(""), "failed");
    }
}
