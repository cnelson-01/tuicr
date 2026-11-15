//! Fetching every remote of one repository: `:fetch` for the repository on
//! screen, and fetch-all for every recent repository at once.
//!
//! Everything here is given the repository root and runs git with that as
//! the child's working directory. It opens no backend and never reads the
//! process's working directory, so worker threads can fetch different
//! repositories side by side whatever directory tuicr itself is in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{non_interactive_git_env, run_git_command, run_git_env};
use crate::error::Result;

/// Who is fetching, which decides how patient git may be and how the
/// command reads in the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchMode {
    /// `:fetch` / `F` in the repository on screen.
    Current,
    /// One repository of many, fetched in the background by fetch-all. The
    /// command names the repository (`git -C <root> fetch ...`), because
    /// the console then holds the fetches of many repositories mixed
    /// together, and a remote that does not answer is given up on rather
    /// than waited for.
    Batch,
}

/// What fetching a repository's remotes did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FetchReport {
    /// How many remotes the repository has.
    pub remotes: usize,
    /// Remote-tracking refs that changed: `origin/main +3`, `origin/dev
    /// new`, `origin/x moved`. Sorted by name.
    pub moved: Vec<String>,
    /// Remote-tracking refs that went away.
    pub pruned: usize,
    /// `(remote, first line of the reason)` for each remote that could not
    /// be fetched.
    pub failures: Vec<(String, String)>,
}

impl FetchReport {
    /// Whether any remote-tracking ref changed.
    pub fn updated(&self) -> bool {
        !self.moved.is_empty() || self.pruned > 0
    }

    /// Whether not one remote could be fetched.
    pub fn all_failed(&self) -> bool {
        self.remotes > 0 && self.failures.len() == self.remotes
    }

    /// `origin failed: could not read from remote · backup failed: ...`
    pub fn failure_text(&self) -> String {
        self.failures
            .iter()
            .map(|(remote, reason)| format!("{remote} failed: {reason}"))
            .collect::<Vec<_>>()
            .join(" \u{00b7} ")
    }

    /// `Fetched: origin/main +3, origin/dev new · 1 pruned`, or
    /// `Fetched: everything up to date`, followed by the remotes that
    /// failed.
    pub fn summary(&self) -> String {
        let mut summary = if !self.updated() {
            "Fetched: everything up to date".to_string()
        } else if self.moved.is_empty() {
            "Fetched".to_string()
        } else {
            let shown = self.moved.len().min(5);
            let mut text = format!("Fetched: {}", self.moved[..shown].join(", "));
            if self.moved.len() > shown {
                text.push_str(&format!(" and {} more", self.moved.len() - shown));
            }
            text
        };
        if self.pruned > 0 {
            summary.push_str(&format!(" \u{00b7} {} pruned", self.pruned));
        }
        if !self.failures.is_empty() {
            summary.push_str(" \u{00b7} ");
            summary.push_str(&self.failure_text());
        }
        summary
    }
}

/// What [`fetch_remotes`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FetchOutcome {
    /// The repository has no remote: nothing to fetch, and not a failure.
    NoRemotes,
    Done(FetchReport),
}

/// Fetch every remote of the repository at `root`, one remote at a time,
/// never interactive and never into submodules: a remote that wants
/// credentials fails with its name in the report instead of prompting
/// under the TUI or stalling the other remotes.
///
/// `Err` means the repository itself could not be read; a remote that
/// could not be reached is a line in [`FetchReport::failures`].
pub(crate) fn fetch_remotes(root: &Path, mode: FetchMode) -> Result<FetchOutcome> {
    let outcome = fetch_remotes_unlogged(root, mode);
    if mode == FetchMode::Batch
        && let Err(e) = &outcome
    {
        // No `git fetch` ran, so the console would hold nothing a person
        // could find this repository's failure by. Say what was not
        // started, and why.
        crate::git_console::record(
            &fetch_command(root, mode),
            None,
            false,
            Duration::ZERO,
            &e.to_string(),
        );
    }
    outcome
}

fn fetch_remotes_unlogged(root: &Path, mode: FetchMode) -> Result<FetchOutcome> {
    let before = remote_ref_tips(root)?;
    let remotes: Vec<String> = run_git_command(root, &["remote"])?
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .map(str::to_string)
        .collect();
    if remotes.is_empty() {
        return Ok(FetchOutcome::NoRemotes);
    }
    let envs = fetch_env(root, mode);
    let mut report = FetchReport {
        remotes: remotes.len(),
        ..FetchReport::default()
    };
    for remote in &remotes {
        if let Err(e) = run_git_env(root, &fetch_args(root, remote, mode), &envs) {
            let reason = e.to_string();
            let reason = reason
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("failed")
                .trim()
                .to_string();
            report.failures.push((remote.clone(), reason));
        }
    }
    if report.all_failed() {
        return Ok(FetchOutcome::Done(report));
    }
    let after = remote_ref_tips(root)?;
    (report.moved, report.pruned) = ref_changes(root, &before, &after, mode);
    Ok(FetchOutcome::Done(report))
}

/// `git fetch` as run for `mode`, without a remote.
fn fetch_command(root: &Path, mode: FetchMode) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    // `args` are strings, so a root that is not UTF-8 goes unnamed; git
    // still runs in it, through the child's working directory.
    if mode == FetchMode::Batch
        && let Some(root) = root.to_str()
    {
        args.extend(["-C".to_string(), root.to_string()]);
    }
    args.extend(["fetch", "--prune", "--no-recurse-submodules", "--quiet"].map(String::from));
    args
}

/// The arguments of the `git fetch` for one remote.
pub(crate) fn fetch_args(root: &Path, remote: &str, mode: FetchMode) -> Vec<String> {
    let mut args = fetch_command(root, mode);
    args.push(remote.to_string());
    args
}

fn fetch_env(root: &Path, mode: FetchMode) -> Vec<(&'static str, String)> {
    let envs = non_interactive_git_env(root);
    match mode {
        FetchMode::Current => envs,
        FetchMode::Batch => batch_env(envs),
    }
}

/// What a background fetch adds to the non-interactive environment: give up
/// on a host that does not answer and on a transfer that has stalled, since
/// nobody is watching this one fetch and three others share its queue. No
/// askpass or credential overrides: fetch-all must not fail where `:fetch`
/// succeeds.
fn batch_env(mut envs: Vec<(&'static str, String)>) -> Vec<(&'static str, String)> {
    for (key, value) in envs.iter_mut() {
        // Present only when the ssh command is tuicr's own; a user's
        // `GIT_SSH_COMMAND` / `core.sshCommand` is left exactly as it is.
        if *key == "GIT_SSH_COMMAND" {
            // Also notice a connection that dies after it was made (a VPN
            // drop mid-transfer), which otherwise waits on TCP keepalive.
            value.push_str(
                " -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=2",
            );
        }
    }
    envs.push(("GIT_HTTP_LOW_SPEED_LIMIT", "1000".to_string()));
    envs.push(("GIT_HTTP_LOW_SPEED_TIME", "30".to_string()));
    envs
}

/// Remote-tracking refs and their tips, `origin/HEAD` aliases excluded.
/// Names are cut from the full refname: newer git shortens `origin/HEAD`
/// to plain `origin`, which a `/HEAD` check on the short name would miss.
fn remote_ref_tips(root: &Path) -> Result<HashMap<String, String>> {
    let output = run_git_command(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            "refs/remotes",
        ],
    )?;
    Ok(output
        .lines()
        .filter_map(|line| line.split_once('\0'))
        .filter(|(full, _)| !full.ends_with("/HEAD"))
        .filter_map(|(full, oid)| {
            let name = full.strip_prefix("refs/remotes/")?;
            Some((name.to_string(), oid.to_string()))
        })
        .collect())
}

/// The refs that changed between two readings of [`remote_ref_tips`]
/// (`origin/main +3`, `origin/dev new`, `origin/x moved`, by name), and how
/// many went away.
fn ref_changes(
    root: &Path,
    before: &HashMap<String, String>,
    after: &HashMap<String, String>,
    mode: FetchMode,
) -> (Vec<String>, usize) {
    let mut moved: Vec<String> = Vec::new();
    let mut names: Vec<&String> = after.keys().collect();
    names.sort();
    for name in names {
        let now = &after[name];
        match before.get(name) {
            None => moved.push(format!("{name} new")),
            // A batch run only reports that a repository changed, so it
            // skips the per-ref count: one logged command per moved ref
            // would crowd the failures out of the console.
            Some(old) if old != now && mode == FetchMode::Batch => {
                moved.push(format!("{name} moved"));
            }
            Some(old) if old != now => {
                let count =
                    run_git_command(root, &["rev-list", "--count", &format!("{old}..{now}")])
                        .map(|n| n.trim().to_string())
                        .ok()
                        .filter(|n| n != "0");
                match count {
                    Some(n) => moved.push(format!("{name} +{n}")),
                    None => moved.push(format!("{name} moved")),
                }
            }
            Some(_) => {}
        }
    }
    let pruned = before
        .keys()
        .filter(|name| !after.contains_key(*name))
        .count();
    (moved, pruned)
}

/// The repository a checkout belongs to: its `.git` directory, or for a
/// linked worktree the `.git` directory of the repository it was added
/// from. Read from the files, without running git. `None` when `root` is
/// not the top of a Git checkout.
///
/// Two checkouts with the same answer share their refs, so fetching both at
/// once would have them fight over the same ref locks.
pub(crate) fn git_common_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return std::fs::canonicalize(&dot_git).ok();
    }
    // A linked worktree (or a submodule): `.git` is a file that points at
    // the real directory, relative to the checkout unless it is absolute.
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let git_dir = pointer
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .map(|path| root.join(path.trim()))?;
    // A worktree's own directory holds `commondir`, the way from there to
    // the repository's.
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(relative) => git_dir.join(relative.trim()),
        Err(_) => git_dir,
    };
    std::fs::canonicalize(common).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn git(dir: &Path, args: &[&str]) {
        run_git_command(dir, args)
            .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
    }

    fn identify(repo: &Path) {
        for args in [
            ["config", "user.name", "Tuicr Test"],
            ["config", "user.email", "tuicr@example.com"],
            ["config", "commit.gpgsign", "false"],
        ] {
            git(repo, &args);
        }
    }

    fn commit(repo: &Path, file: &str, text: &str) {
        fs::write(repo.join(file), text).expect("write");
        git(repo, &["add", "."]);
        git(repo, &["commit", "-qm", text.trim()]);
    }

    /// A bare remote and two clones of it, `a` and `b`, each on `main` with
    /// the first commit pushed from `a`.
    struct Remote {
        dir: tempfile::TempDir,
    }

    impl Remote {
        fn new() -> Self {
            let dir = tempdir().expect("tempdir");
            let base = dir.path();
            git(base, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
            let url = base.join("remote.git").to_string_lossy().into_owned();
            git(base, &["clone", "-q", &url, "a"]);
            let a = base.join("a");
            identify(&a);
            git(&a, &["checkout", "-q", "-b", "main"]);
            commit(&a, "f.txt", "one\n");
            git(&a, &["push", "-q", "-u", "origin", "main"]);
            git(base, &["clone", "-q", &url, "b"]);
            identify(&base.join("b"));
            Self { dir }
        }

        fn a(&self) -> PathBuf {
            self.dir.path().join("a")
        }

        fn b(&self) -> PathBuf {
            self.dir.path().join("b")
        }

        /// A new commit on the remote's `main`, pushed from `a`.
        fn push_from_a(&self, text: &str) {
            commit(&self.a(), "f.txt", text);
            git(&self.a(), &["push", "-q", "origin", "main"]);
        }
    }

    fn report(outcome: Result<FetchOutcome>) -> FetchReport {
        match outcome.expect("the repository can be read") {
            FetchOutcome::Done(report) => report,
            FetchOutcome::NoRemotes => panic!("the repository has a remote"),
        }
    }

    #[test]
    fn should_report_what_a_fetch_brought_in_and_then_nothing() {
        // given: the remote is one commit ahead of clone b
        let remote = Remote::new();
        remote.push_from_a("one\ntwo\n");

        // when
        let first = report(fetch_remotes(&remote.b(), FetchMode::Current));
        let second = report(fetch_remotes(&remote.b(), FetchMode::Current));

        // then
        assert_eq!(first.remotes, 1);
        assert_eq!(first.moved, ["origin/main +1"]);
        assert!(first.updated());
        assert!(first.failures.is_empty());
        assert_eq!(first.summary(), "Fetched: origin/main +1");
        assert!(!second.updated(), "{second:?}");
        assert_eq!(second.summary(), "Fetched: everything up to date");
    }

    #[test]
    fn should_report_the_same_in_either_mode() {
        // given
        let remote = Remote::new();
        remote.push_from_a("one\ntwo\n");

        // when
        let current = report(fetch_remotes(&remote.b(), FetchMode::Current));

        // then
        assert_eq!(current.moved, ["origin/main +1"]);
    }

    #[test]
    fn should_say_a_repository_has_no_remotes() {
        // given
        let dir = tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);

        // when
        let outcome = fetch_remotes(dir.path(), FetchMode::Batch);

        // then: not an error, and not a report
        assert_eq!(outcome.expect("readable"), FetchOutcome::NoRemotes);
    }

    #[test]
    fn should_name_a_remote_that_cannot_be_reached_and_fetch_the_others() {
        // given: a second remote that leads nowhere
        let remote = Remote::new();
        remote.push_from_a("one\ntwo\n");
        git(
            &remote.b(),
            &["remote", "add", "dead", "/nonexistent/repo.git"],
        );

        // when
        let partial = report(fetch_remotes(&remote.b(), FetchMode::Current));

        // then: origin was fetched all the same
        assert_eq!(partial.remotes, 2);
        assert_eq!(partial.moved, ["origin/main +1"]);
        assert_eq!(partial.failures.len(), 1, "{partial:?}");
        assert_eq!(partial.failures[0].0, "dead");
        assert!(!partial.failures[0].1.is_empty());
        assert!(!partial.failures[0].1.contains('\n'));
        assert!(!partial.all_failed());
        assert!(
            partial
                .summary()
                .starts_with("Fetched: origin/main +1 \u{00b7} dead failed: "),
            "{}",
            partial.summary()
        );
    }

    #[test]
    fn should_report_every_remote_failing() {
        // given: a repository whose only remote leads nowhere
        let dir = tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "/nonexistent/repo.git"],
        );

        // when
        let failed = report(fetch_remotes(dir.path(), FetchMode::Batch));

        // then
        assert!(failed.all_failed());
        assert!(!failed.updated());
        assert!(failed.failure_text().starts_with("origin failed: "));
    }

    #[test]
    fn should_fail_for_a_directory_that_is_not_a_repository() {
        // given
        let dir = tempdir().expect("tempdir");

        // when
        let outcome = fetch_remotes(dir.path(), FetchMode::Batch);

        // then
        assert!(outcome.is_err());
    }

    #[test]
    fn should_count_pruned_and_new_refs() {
        // given: the remote gains one branch and loses another after b
        // has seen it
        let remote = Remote::new();
        git(
            &remote.a(),
            &["push", "-q", "origin", "main:refs/heads/old"],
        );
        report(fetch_remotes(&remote.b(), FetchMode::Batch));
        git(&remote.a(), &["push", "-q", "origin", "--delete", "old"]);
        git(
            &remote.a(),
            &["push", "-q", "origin", "main:refs/heads/new"],
        );

        // when
        let fetched = report(fetch_remotes(&remote.b(), FetchMode::Batch));

        // then
        assert_eq!(fetched.moved, ["origin/new new"]);
        assert_eq!(fetched.pruned, 1);
        assert_eq!(
            fetched.summary(),
            "Fetched: origin/new new \u{00b7} 1 pruned"
        );
    }

    #[test]
    fn should_word_the_summary_for_each_shape_of_report() {
        let pruned_only = FetchReport {
            remotes: 1,
            pruned: 2,
            ..FetchReport::default()
        };
        assert_eq!(pruned_only.summary(), "Fetched \u{00b7} 2 pruned");
        let many = FetchReport {
            remotes: 1,
            moved: (0..7).map(|i| format!("origin/b{i} new")).collect(),
            ..FetchReport::default()
        };
        assert!(many.summary().ends_with("origin/b4 new and 2 more"));
        let none = FetchReport::default();
        assert!(!none.all_failed(), "no remotes is not every remote failing");
    }

    #[test]
    fn should_name_the_repository_only_in_a_batch_fetch() {
        // given
        let root = Path::new("/work/some repo");

        // when
        let current = fetch_args(root, "origin", FetchMode::Current);
        let batch = fetch_args(root, "origin", FetchMode::Batch);

        // then
        assert_eq!(
            current,
            [
                "fetch",
                "--prune",
                "--no-recurse-submodules",
                "--quiet",
                "origin"
            ]
        );
        assert_eq!(batch[..2], ["-C", "/work/some repo"]);
        assert_eq!(batch[2..], current[..]);
        // and the console shows it the way it would be typed
        assert_eq!(
            crate::git_console::display_args(&batch),
            "git -C '/work/some repo' fetch --prune --no-recurse-submodules --quiet origin"
        );
        assert!(crate::git_console::is_mutating(&batch));
    }

    #[test]
    fn should_give_up_sooner_in_a_batch_fetch() {
        // given: the environment tuicr builds when the ssh command is its
        // own, and when the user has theirs
        let own = vec![
            ("GIT_TERMINAL_PROMPT", "0".to_string()),
            ("GIT_SSH_COMMAND", "ssh -o BatchMode=yes".to_string()),
        ];
        let users = vec![("GIT_TERMINAL_PROMPT", "0".to_string())];

        // when
        let own = batch_env(own);
        let users = batch_env(users);

        // then
        let value = |envs: &[(&str, String)], key: &str| {
            envs.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            value(&own, "GIT_SSH_COMMAND").as_deref(),
            Some(
                "ssh -o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=2"
            )
        );
        assert_eq!(value(&own, "GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
        assert_eq!(value(&users, "GIT_SSH_COMMAND"), None);
        for envs in [&own, &users] {
            assert_eq!(
                value(envs, "GIT_HTTP_LOW_SPEED_LIMIT").as_deref(),
                Some("1000")
            );
            assert_eq!(
                value(envs, "GIT_HTTP_LOW_SPEED_TIME").as_deref(),
                Some("30")
            );
        }
    }

    #[test]
    fn should_find_one_common_dir_for_a_repository_and_its_worktrees() {
        // given: a repository, a linked worktree of it, and another clone
        let remote = Remote::new();
        let a = remote.a();
        let worktree = remote.dir.path().join("a-wt");
        git(
            &a,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                &worktree.to_string_lossy(),
            ],
        );

        // when
        let main = git_common_dir(&a);
        let linked = git_common_dir(&worktree);
        let other = git_common_dir(&remote.b());

        // then
        assert_eq!(
            main,
            Some(fs::canonicalize(a.join(".git")).expect("canonical"))
        );
        assert_eq!(linked, main, "a worktree shares its repository's refs");
        assert!(other.is_some());
        assert_ne!(other, main);
    }

    #[test]
    fn should_find_no_common_dir_outside_a_checkout() {
        // given
        let dir = tempdir().expect("tempdir");
        let inside = Remote::new();
        let nested = inside.a().join("src");
        fs::create_dir(&nested).expect("mkdir");
        let dangling = dir.path().join("dangling");
        fs::create_dir(&dangling).expect("mkdir");
        fs::write(dangling.join(".git"), "gitdir: /nonexistent/worktrees/x\n").expect("write");

        // then: a plain directory, a directory inside a checkout, a
        // missing path, and a worktree whose repository is gone
        assert_eq!(git_common_dir(dir.path()), None);
        assert_eq!(git_common_dir(&nested), None);
        assert_eq!(git_common_dir(&dir.path().join("missing")), None);
        assert_eq!(git_common_dir(&dangling), None);
    }
}
