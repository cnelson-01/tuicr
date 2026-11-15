//! Fetch-all on a live app: `App::start_fetch_all` against real
//! repositories with local bare remotes, and `App::poll_fetch_all` driven
//! report by report through the channel its workers use.
//!
//! Every repository, the recent-repositories file and the reviews directory
//! live in one temp directory per test. Nothing depends on the process's
//! working directory: every app is opened at an explicit root and the
//! workers are handed roots.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::log_pane_tests::build_review_app;
use crate::app::fetch_all::{FetchAllEvent, plan_fetch_all};
use crate::app::*;
use crate::handler::{handle_command_action, handle_repo_picker_action};
use crate::input::Action;
use crate::persistence::recent_repos;
use crate::persistence::storage;
use crate::vcs::git::fetch::{FetchOutcome, FetchReport, git_common_dir};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {} in {} failed: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn identify(repo: &Path) {
    git(repo, &["config", "user.name", "tester"]);
    git(repo, &["config", "user.email", "tester@example.com"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

/// One test's world: repositories and their bare remotes side by side in a
/// temp directory, with the recent-repositories file and the reviews
/// directory (thread-local, so parallel tests do not share it).
struct World {
    dir: tempfile::TempDir,
    base: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // Roots as tuicr knows them: symlinks resolved.
        let base = recent_repos::canonical(dir.path());
        storage::set_test_reviews_dir(Some(base.join("reviews")));
        Self { dir, base }
    }

    fn store(&self) -> PathBuf {
        self.base.join("recent_repos.json")
    }

    fn settings(&self) -> LaunchSettings {
        LaunchSettings {
            recent_repos_path: Some(self.store()),
            ..LaunchSettings::default()
        }
    }

    /// A repository `name` with one commit and no remote.
    fn local(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        std::fs::create_dir(&root).expect("mkdir");
        git(&root, &["init", "-q", "-b", "main"]);
        identify(&root);
        std::fs::write(root.join("file.txt"), format!("{name}\n")).expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", &format!("first in {name}")]);
        root
    }

    /// A clone `name` of a bare remote `name.git` that holds one commit on
    /// `main`. `name-upstream` is the clone later commits are pushed from.
    fn cloned(&self, name: &str) -> PathBuf {
        let bare = format!("{name}.git");
        git(&self.base, &["init", "-q", "--bare", "-b", "main", &bare]);
        let upstream = self.local(&format!("{name}-upstream"));
        let url = self.base.join(&bare).to_string_lossy().into_owned();
        git(&upstream, &["remote", "add", "origin", &url]);
        git(&upstream, &["push", "-q", "-u", "origin", "main"]);
        git(&self.base, &["clone", "-q", &url, name]);
        let root = self.base.join(name);
        identify(&root);
        root
    }

    /// A new commit on `name`'s remote that the clone has not fetched.
    fn advance_remote(&self, name: &str, subject: &str) {
        let upstream = self.base.join(format!("{name}-upstream"));
        std::fs::write(upstream.join("file.txt"), format!("{subject}\n")).expect("write");
        git(&upstream, &["commit", "-q", "-am", subject]);
        git(&upstream, &["push", "-q", "origin", "main"]);
    }

    /// Put `roots` on the recent list, the first one the most recent.
    fn list(&self, roots: &[&Path]) {
        for (age, root) in roots.iter().rev().enumerate() {
            recent_repos::record(&self.store(), root, 1_000 + age as i64).expect("record");
        }
    }

    /// What `main.rs` does to start in `root` with no arguments.
    fn launch(&self, root: &Path) -> App {
        let settings = self.settings();
        let mut app = App::new(
            Theme::dark(),
            settings.comment_types(),
            settings.output_to_stdout,
            settings.startup_options(Some(root)),
        )
        .expect("open the fixture repository");
        let warnings = app.finish_launch(&settings, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        app.record_current_repo();
        app
    }
}

impl Drop for World {
    fn drop(&mut self) {
        storage::set_test_reviews_dir(None);
        // Named so the temp directory outlives everything above.
        let _ = &self.dir;
    }
}

/// Install a fetch-all run over `roots` as `start_fetch_all` would plan it,
/// and return the sender a test plays the workers with.
fn install_run(app: &mut App, roots: &[&Path]) -> Sender<FetchAllEvent> {
    let paths: Vec<PathBuf> = roots.iter().map(|root| root.to_path_buf()).collect();
    let (rows, groups) = plan_fetch_all(&paths, app.current_repo_root());
    let (job, tx) = FetchAllJob::new(rows, groups);
    app.fetch_all = Some(job);
    tx
}

/// The group index a worker would report for the repository at `root`.
fn group_of(app: &App, roots: &[&Path], root: &Path) -> usize {
    let paths: Vec<PathBuf> = roots.iter().map(|root| root.to_path_buf()).collect();
    let (_, groups) = plan_fetch_all(&paths, app.current_repo_root());
    let common_dir = git_common_dir(root).expect("a git checkout");
    groups
        .iter()
        .position(|group| group.common_dir == common_dir)
        .expect("the repository is part of the run")
}

fn fetched(moved: &[&str]) -> std::result::Result<FetchOutcome, String> {
    Ok(FetchOutcome::Done(FetchReport {
        remotes: 1,
        moved: moved.iter().map(|s| s.to_string()).collect(),
        pruned: 0,
        failures: Vec::new(),
    }))
}

fn unreachable_remote(reason: &str) -> std::result::Result<FetchOutcome, String> {
    Ok(FetchOutcome::Done(FetchReport {
        remotes: 1,
        moved: Vec::new(),
        pruned: 0,
        failures: vec![("origin".to_string(), reason.to_string())],
    }))
}

fn state_of(app: &App, root: &Path) -> RepoFetchState {
    app.fetch_all
        .as_ref()
        .expect("a fetch-all run")
        .state_for(root)
        .unwrap_or_else(|| panic!("{} is not part of the run", root.display()))
        .clone()
}

fn message(app: &App) -> Option<(String, MessageType)> {
    app.message
        .as_ref()
        .map(|message| (message.content.clone(), message.message_type.clone()))
}

fn subjects(app: &App) -> Vec<&str> {
    app.commit_list
        .iter()
        .map(|commit| commit.summary.as_str())
        .collect()
}

/// Poll until the run is over, as the main loop would tick by tick.
fn run_to_the_end(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while app.fetch_all_progress().is_some() {
        assert!(
            Instant::now() < deadline,
            "fetch-all did not finish: {:?}",
            app.fetch_all.as_ref().map(|job| job.rows.clone())
        );
        app.poll_fetch_all();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn screen(app: &mut App) -> Vec<String> {
    screen_of_width(app, 140)
}

fn screen_of_width(app: &mut App, width: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .expect("draw frame");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

// ── The real thing ───────────────────────────────────────────────────────

#[test]
fn should_fetch_every_recent_repository_and_report_each_one() {
    // given: one repository of every kind on the list
    let world = World::new();
    let fresh = world.cloned("fresh");
    world.advance_remote("fresh", "second in fresh");
    let same = world.cloned("same");
    let local = world.local("local");
    let gone = world.local("gone");
    let dead = world.local("dead");
    git(
        &dead,
        &["remote", "add", "origin", "/nonexistent/remote.git"],
    );
    let home = world.local("home");
    world.list(&[&fresh, &same, &local, &gone, &dead]);
    std::fs::remove_dir_all(&gone).expect("delete the repository");
    let mut app = world.launch(&home);
    let cwd = std::env::current_dir().expect("cwd");
    let behind = git(&fresh, &["rev-parse", "origin/main"]);

    // when
    app.start_fetch_all();

    // then: counted off in the status bar while it runs
    assert_eq!(
        app.fetch_all_progress().map(|(_, total)| total),
        Some(5),
        "everything but the missing repository is queued"
    );
    assert_eq!(state_of(&app, &gone), RepoFetchState::Skipped("missing"));
    assert_eq!(message(&app), None, "the count takes the message's place");
    run_to_the_end(&mut app);

    // then: each repository says how it went
    assert_eq!(state_of(&app, &fresh), RepoFetchState::Updated);
    assert_eq!(state_of(&app, &same), RepoFetchState::UpToDate);
    assert_eq!(
        state_of(&app, &local),
        RepoFetchState::Skipped("no remotes")
    );
    assert_eq!(state_of(&app, &home), RepoFetchState::Skipped("no remotes"));
    assert_eq!(state_of(&app, &gone), RepoFetchState::Skipped("missing"));
    let RepoFetchState::Failed(reason) = state_of(&app, &dead) else {
        panic!("the unreachable remote is a failure");
    };
    assert!(reason.starts_with("origin: "), "{reason}");
    assert!(!reason.contains("VCS command failed"), "{reason}");
    // one summary line, which stays on screen because something failed
    assert_eq!(
        message(&app),
        Some((
            "Fetched 3 repositories: 1 updated, 1 failed (dead). Reasons: :console".to_string(),
            MessageType::Error
        ))
    );
    // the fetch really happened, in the repository it was meant for
    let now = git(&fresh, &["rev-parse", "origin/main"]);
    assert_ne!(now, behind);
    assert_eq!(
        now,
        git(&world.base.join("fresh.git"), &["rev-parse", "main"])
    );
    // without the process ever leaving its directory
    assert_eq!(std::env::current_dir().expect("cwd"), cwd);
    // and the finished run is kept for the picker to show
    assert!(app.fetch_all.as_ref().is_some_and(|job| !job.running()));
    assert!(!app.poll_fetch_all(), "nothing more arrives");
}

#[test]
fn should_fetch_a_repository_and_its_worktree_once() {
    // given: a clone and a linked worktree of it, both on the list
    let world = World::new();
    let repo = world.cloned("shared");
    let worktree = world.base.join("shared-wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &worktree.to_string_lossy(),
        ],
    );
    world.advance_remote("shared", "second in shared");
    let home = world.local("home");
    world.list(&[&repo, &worktree]);
    let mut app = world.launch(&home);

    // when
    app.start_fetch_all();
    run_to_the_end(&mut app);

    // then: both rows carry the one result, and nothing collided
    assert_eq!(state_of(&app, &repo), RepoFetchState::Updated);
    assert_eq!(state_of(&app, &worktree), RepoFetchState::Updated);
    assert_eq!(
        message(&app),
        Some((
            "Fetched 2 repositories: 2 updated \u{00b7} 1 skipped".to_string(),
            MessageType::Info
        ))
    );
}

#[test]
fn should_warn_when_there_is_no_list_to_fetch() {
    // given: an app that keeps no recent list, as tests get by default
    let world = World::new();
    let home = world.local("home");
    let mut app = world.launch(&home);
    app.recent_repos_path = None;

    // when
    app.start_fetch_all();

    // then
    assert_eq!(
        message(&app),
        Some((
            "No recent repositories to fetch".to_string(),
            MessageType::Warning
        ))
    );
    assert!(app.fetch_all.is_none());
}

#[test]
fn should_finish_at_once_when_nothing_on_the_list_can_be_fetched() {
    // given: the only listed repository is gone
    let world = World::new();
    let gone = world.base.join("gone");
    let mut app = build_review_app(&["a"]);
    app.recent_repos_path = Some(world.store());
    world.list(&[&gone]);

    // when
    app.start_fetch_all();

    // then: no worker was needed
    assert_eq!(app.fetch_all_progress(), None);
    assert_eq!(
        message(&app),
        Some((
            "Nothing to fetch: 1 repository skipped".to_string(),
            MessageType::Info
        ))
    );
    assert_eq!(state_of(&app, &gone), RepoFetchState::Skipped("missing"));
    assert!(!app.poll_fetch_all());
}

// ── Reports, one at a time ───────────────────────────────────────────────

#[test]
fn should_follow_the_workers_reports_in_progress_and_row_states() {
    // given: a run over three repositories, none of them the current one
    let world = World::new();
    let (a, b, c) = (world.local("a"), world.local("b"), world.local("c"));
    let roots = [a.as_path(), b.as_path(), c.as_path()];
    let mut app = build_review_app(&["x"]);
    let tx = install_run(&mut app, &roots);

    // then: nothing has arrived yet
    assert_eq!(app.fetch_all_progress(), Some((0, 3)));
    assert!(
        !app.poll_fetch_all(),
        "nothing to install, nothing to redraw"
    );

    // when the first two start and one of them finishes
    tx.send(FetchAllEvent::Started(0)).unwrap();
    tx.send(FetchAllEvent::Started(1)).unwrap();
    tx.send(FetchAllEvent::Finished(1, fetched(&[]))).unwrap();
    assert!(app.poll_fetch_all());

    // then
    assert_eq!(state_of(&app, &a), RepoFetchState::Fetching);
    assert_eq!(state_of(&app, &b), RepoFetchState::UpToDate);
    assert_eq!(state_of(&app, &c), RepoFetchState::Queued);
    assert_eq!(app.fetch_all_progress(), Some((1, 3)));
    assert_eq!(message(&app), None, "no summary before the end");
    assert!(!app.poll_fetch_all());
}

#[test]
fn should_end_a_clean_run_with_one_info_message() {
    // given
    let world = World::new();
    let (a, b) = (world.local("a"), world.local("b"));
    let roots = [a.as_path(), b.as_path()];
    let mut app = build_review_app(&["x"]);
    let tx = install_run(&mut app, &roots);

    // when
    tx.send(FetchAllEvent::Finished(0, fetched(&["origin/main +2"])))
        .unwrap();
    tx.send(FetchAllEvent::Finished(1, fetched(&[]))).unwrap();
    assert!(app.poll_fetch_all());

    // then
    assert_eq!(
        message(&app),
        Some((
            "Fetched 2 repositories: 1 updated".to_string(),
            MessageType::Info
        ))
    );
    assert_eq!(app.fetch_all_progress(), None);
    assert!(app.fetch_all.is_some(), "kept for the picker");
}

#[test]
fn should_end_a_run_with_failures_as_one_error_naming_them() {
    // given
    let world = World::new();
    let (a, b, c) = (world.local("a"), world.local("b"), world.local("c"));
    let roots = [a.as_path(), b.as_path(), c.as_path()];
    let mut app = build_review_app(&["x"]);
    let tx = install_run(&mut app, &roots);

    // when: one remote is unreachable and one repository cannot be read
    tx.send(FetchAllEvent::Finished(
        0,
        unreachable_remote("VCS command failed: fatal: unable to access"),
    ))
    .unwrap();
    tx.send(FetchAllEvent::Finished(1, fetched(&[]))).unwrap();
    tx.send(FetchAllEvent::Finished(
        2,
        Err("VCS command failed: fatal: bad object".to_string()),
    ))
    .unwrap();
    assert!(app.poll_fetch_all());

    // then
    let (text, kind) = message(&app).expect("a summary");
    assert_eq!(
        text,
        "Fetched 3 repositories: 0 updated, 2 failed (a, c). Reasons: :console"
    );
    assert_eq!(kind, MessageType::Error);
    assert!(
        app.message.as_ref().unwrap().expires_at.is_none(),
        "an error stays until something replaces it"
    );
    assert_eq!(
        state_of(&app, &a),
        RepoFetchState::Failed("origin: fatal: unable to access".to_string())
    );
    assert_eq!(
        state_of(&app, &c),
        RepoFetchState::Failed("fatal: bad object".to_string())
    );
}

#[test]
fn should_close_the_run_when_the_workers_are_gone_without_reporting() {
    // given: a run whose workers died with one repository in flight
    let world = World::new();
    let (a, b) = (world.local("a"), world.local("b"));
    let roots = [a.as_path(), b.as_path()];
    let mut app = build_review_app(&["x"]);
    let tx = install_run(&mut app, &roots);
    tx.send(FetchAllEvent::Finished(0, fetched(&[]))).unwrap();
    tx.send(FetchAllEvent::Started(1)).unwrap();

    // when
    drop(tx);
    assert!(app.poll_fetch_all());

    // then: it does not count for ever
    assert_eq!(app.fetch_all_progress(), None);
    assert_eq!(
        state_of(&app, &b),
        RepoFetchState::Failed("worker stopped".to_string())
    );
    let (text, kind) = message(&app).expect("a summary");
    assert!(text.contains("1 failed (b)"), "{text}");
    assert_eq!(kind, MessageType::Error);
}

// ── One at a time ────────────────────────────────────────────────────────

#[test]
fn should_refuse_a_second_fetch_all_while_one_is_running() {
    // given
    let world = World::new();
    let (a, b) = (world.local("a"), world.local("b"));
    let roots = [a.as_path(), b.as_path()];
    let mut app = build_review_app(&["x"]);
    app.recent_repos_path = Some(world.store());
    world.list(&roots);
    let tx = install_run(&mut app, &roots);
    tx.send(FetchAllEvent::Finished(0, fetched(&[]))).unwrap();
    app.poll_fetch_all();

    // when
    app.start_fetch_all();

    // then: the run in flight is untouched
    assert_eq!(
        message(&app),
        Some((
            "Fetch-all already running (1/2)".to_string(),
            MessageType::Warning
        ))
    );
    assert_eq!(app.fetch_all_progress(), Some((1, 2)));
    assert_eq!(state_of(&app, &a), RepoFetchState::UpToDate);
    assert_eq!(state_of(&app, &b), RepoFetchState::Queued);
}

#[test]
fn should_refuse_to_start_while_a_fetch_pull_or_push_is_in_flight() {
    // given
    let world = World::new();
    let a = world.local("a");
    let mut app = build_review_app(&["x"]);
    app.recent_repos_path = Some(world.store());
    world.list(&[&a]);
    let (_remote_tx, remote_rx) = std::sync::mpsc::channel();
    app.remote_op = Some(RemoteOpInFlight {
        op: RemoteOp::Pull,
        rx: remote_rx,
    });

    // when
    app.start_fetch_all();

    // then
    assert!(app.fetch_all.is_none());
    assert_eq!(
        message(&app),
        Some((
            "Pulling already \u{2014} wait for it to finish".to_string(),
            MessageType::Warning
        ))
    );
}

#[test]
fn should_refuse_a_fetch_here_until_fetch_all_is_done_with_this_repository() {
    // given: fetch-all has this repository queued
    let world = World::new();
    let here = world.cloned("here");
    let other = world.local("other");
    let roots = [other.as_path(), here.as_path()];
    let mut app = world.launch(&here);
    let tx = install_run(&mut app, &roots);
    let here_group = group_of(&app, &roots, &here);
    assert_eq!(here_group, 0, "the repository on screen is first in line");

    // when / then: queued, then fetching
    app.start_remote_op(RemoteOp::Fetch);
    assert!(app.remote_op.is_none());
    assert_eq!(
        message(&app),
        Some((
            "Fetch-all is still working on this repository".to_string(),
            MessageType::Warning
        ))
    );
    tx.send(FetchAllEvent::Started(here_group)).unwrap();
    app.poll_fetch_all();
    app.message = None;
    app.start_remote_op(RemoteOp::Push);
    assert!(app.remote_op.is_none());
    assert!(message(&app).is_some());

    // when fetch-all has moved on to the other repository
    tx.send(FetchAllEvent::Finished(here_group, fetched(&[])))
        .unwrap();
    app.poll_fetch_all();
    assert!(app.fetch_all_progress().is_some(), "the run goes on");
    app.start_remote_op(RemoteOp::Fetch);

    // then: this repository is free again
    assert_eq!(app.remote_op_running(), Some(RemoteOp::Fetch));
    let deadline = Instant::now() + Duration::from_secs(60);
    while !app.poll_remote_op() {
        assert!(Instant::now() < deadline, "the fetch did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn should_guard_a_worktree_whose_repository_is_being_fetched() {
    // given: the app is in a linked worktree that is not on the list, and
    // fetch-all has the repository it belongs to queued
    let world = World::new();
    let repo = world.cloned("shared");
    let worktree = world.base.join("shared-wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &worktree.to_string_lossy(),
        ],
    );
    let mut app = world.launch(&worktree);
    let _tx = install_run(&mut app, &[&repo]);

    // when
    app.start_remote_op(RemoteOp::Fetch);

    // then: they share refs, so the fetch here has to wait as well
    assert!(app.remote_op.is_none());
}

// ── Across a switch ──────────────────────────────────────────────────────

#[test]
fn should_keep_running_across_a_repo_switch_and_refresh_the_new_repository() {
    // given: a run started in alpha, with beta one commit behind its remote
    let world = World::new();
    let alpha = world.local("alpha");
    let beta = world.cloned("beta");
    let roots = [alpha.as_path(), beta.as_path()];
    world.list(&[&beta]);
    let mut app = world.launch(&alpha);
    let tx = install_run(&mut app, &roots);
    let beta_group = group_of(&app, &roots, &beta);
    tx.send(FetchAllEvent::Started(beta_group)).unwrap();
    app.poll_fetch_all();

    // when: the user switches to beta mid-run
    assert!(app.switch_repo(&beta, &world.settings(), None));

    // then: the run came along, still counting
    assert_eq!(app.fetch_all_progress(), Some((0, 2)));
    assert_eq!(state_of(&app, &beta), RepoFetchState::Fetching);
    assert_eq!(subjects(&app), ["first in beta-upstream"]);

    // when the worker finishes beta, which is now the repository on screen
    world.advance_remote("beta", "second in beta");
    git(&beta, &["fetch", "-q", "origin"]);
    tx.send(FetchAllEvent::Finished(
        beta_group,
        fetched(&["origin/main +1"]),
    ))
    .unwrap();
    assert!(app.poll_fetch_all());

    // then: beta's selector shows what the fetch brought in
    assert_eq!(state_of(&app, &beta), RepoFetchState::Updated);
    assert_eq!(subjects(&app), ["second in beta", "first in beta-upstream"]);
    assert_eq!(app.fetch_all_progress(), Some((1, 2)));
}

// ── Refreshing the repository on screen ──────────────────────────────────

/// An app in the selector of a clone whose remote has since moved on, with
/// the new commit already fetched: what a worker leaves behind just before
/// it reports.
fn app_behind_its_remote(world: &World) -> (App, PathBuf, PathBuf) {
    let here = world.cloned("here");
    let other = world.local("other");
    let app = world.launch(&here);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert_eq!(subjects(&app), ["first in here-upstream"]);
    world.advance_remote("here", "second in here");
    git(&here, &["fetch", "-q", "origin"]);
    (app, here, other)
}

#[test]
fn should_show_fetched_commits_in_the_selector_of_the_current_repository() {
    // given
    let world = World::new();
    let (mut app, here, other) = app_behind_its_remote(&world);
    let roots = [other.as_path(), here.as_path()];
    let tx = install_run(&mut app, &roots);
    let here_group = group_of(&app, &roots, &here);

    // when: the worker reports new commits here, with the run still going
    tx.send(FetchAllEvent::Finished(
        here_group,
        fetched(&["origin/main +1"]),
    ))
    .unwrap();
    assert!(app.poll_fetch_all());

    // then: the rows were read again, as after `:fetch`
    assert_eq!(subjects(&app), ["second in here", "first in here-upstream"]);
    assert_eq!(
        message(&app),
        None,
        "the refresh leaves the running count on screen"
    );
    assert_eq!(app.fetch_all_progress(), Some((1, 2)));
}

#[test]
fn should_leave_the_selector_alone_when_nothing_new_came_in_here() {
    // given
    let world = World::new();
    let (mut app, here, other) = app_behind_its_remote(&world);
    let roots = [other.as_path(), here.as_path()];
    let tx = install_run(&mut app, &roots);
    let here_group = group_of(&app, &roots, &here);
    let other_group = group_of(&app, &roots, &other);

    // when: this repository was up to date, and another one was not
    tx.send(FetchAllEvent::Finished(here_group, fetched(&[])))
        .unwrap();
    tx.send(FetchAllEvent::Finished(
        other_group,
        fetched(&["origin/main +3"]),
    ))
    .unwrap();
    assert!(app.poll_fetch_all());

    // then: the rows are the ones read at launch
    assert_eq!(subjects(&app), ["first in here-upstream"]);
}

#[test]
fn should_refresh_the_selector_behind_the_repo_picker() {
    // given: the picker is open over the selector
    let world = World::new();
    let (mut app, here, other) = app_behind_its_remote(&world);
    let roots = [other.as_path(), here.as_path()];
    app.open_repo_picker();
    assert_eq!(app.input_mode, InputMode::RepoPicker);
    let tx = install_run(&mut app, &roots);
    let here_group = group_of(&app, &roots, &here);

    // when
    tx.send(FetchAllEvent::Finished(
        here_group,
        fetched(&["origin/main +1"]),
    ))
    .unwrap();
    assert!(app.poll_fetch_all());

    // then: the picker stays up, over rows that are current
    assert_eq!(app.input_mode, InputMode::RepoPicker);
    assert_eq!(subjects(&app), ["second in here", "first in here-upstream"]);
}

#[test]
fn should_not_reopen_the_repository_during_a_pull_request_review() {
    // given: a pull request is on screen, which has no local repository
    // behind it
    let world = World::new();
    let (mut app, here, other) = app_behind_its_remote(&world);
    let details = crate::forge::traits::PullRequestDetails {
        repository: crate::forge::traits::ForgeRepository::github("github.com", "acme", "here"),
        number: 7,
        title: "a pull request".to_string(),
        url: "https://github.com/acme/here/pull/7".to_string(),
        state: "OPEN".to_string(),
        is_draft: false,
        author: None,
        head_ref_name: "feat".to_string(),
        base_ref_name: "main".to_string(),
        head_sha: "abcdef0123456789".to_string(),
        base_sha: "1234567890abcdef".to_string(),
        body: String::new(),
        updated_at: None,
        closed: false,
        merged_at: None,
        diff_start_sha: None,
    };
    app.diff_source =
        DiffSource::PullRequest(Box::new(PullRequestDiffSource::from_details(&details)));
    let roots = [other.as_path(), here.as_path()];
    let tx = install_run(&mut app, &roots);
    let here_group = group_of(&app, &roots, &here);

    // when
    tx.send(FetchAllEvent::Finished(
        here_group,
        fetched(&["origin/main +1"]),
    ))
    .unwrap();
    assert!(app.poll_fetch_all());

    // then: the result is on the row, and nothing was reloaded
    assert_eq!(state_of(&app, &here), RepoFetchState::Updated);
    assert_eq!(subjects(&app), ["first in here-upstream"]);
}

// ── The picker ───────────────────────────────────────────────────────────

#[test]
fn should_start_from_the_picker_and_keep_it_open() {
    // given: the picker over two listed repositories
    let world = World::new();
    let same = world.cloned("same");
    let home = world.local("home");
    world.list(&[&same]);
    let mut app = world.launch(&home);
    app.open_repo_picker();

    // when: Ctrl-f
    handle_repo_picker_action(&mut app, Action::RepoPickerFetchAll);

    // then
    assert_eq!(app.input_mode, InputMode::RepoPicker);
    assert_eq!(app.fetch_all_progress().map(|(_, total)| total), Some(2));
    run_to_the_end(&mut app);
    assert_eq!(state_of(&app, &same), RepoFetchState::UpToDate);
    assert_eq!(app.input_mode, InputMode::RepoPicker);
}

#[test]
fn should_start_from_the_command_line() {
    // given
    let world = World::new();
    let same = world.cloned("same");
    let home = world.local("home");
    world.list(&[&same]);
    let mut app = world.launch(&home);

    // when: `:fetchall`
    app.enter_command_mode();
    for c in "fetchall".chars() {
        handle_command_action(&mut app, Action::InsertChar(c));
    }
    handle_command_action(&mut app, Action::SubmitInput);

    // then: back where the command was typed, with the run going
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert!(app.fetch_all.is_some());
    run_to_the_end(&mut app);
    assert_eq!(state_of(&app, &same), RepoFetchState::UpToDate);
}

#[test]
fn should_forget_a_finished_run_when_the_picker_closes() {
    // given: a finished run, shown by the picker
    let world = World::new();
    let a = world.local("a");
    let mut app = build_review_app(&["x"]);
    let tx = install_run(&mut app, &[&a]);
    tx.send(FetchAllEvent::Finished(0, fetched(&[]))).unwrap();
    app.poll_fetch_all();
    app.open_repo_picker();
    assert!(
        app.fetch_all.is_some(),
        "opening the picker shows the results"
    );

    // when
    app.close_repo_picker();

    // then
    assert!(app.fetch_all.is_none());
}

#[test]
fn should_not_show_results_that_went_stale_before_the_picker_opened() {
    // given: a run that ended without the picker open
    let world = World::new();
    let a = world.local("a");
    let mut app = build_review_app(&["x"]);
    let tx = install_run(&mut app, &[&a]);
    tx.send(FetchAllEvent::Finished(0, fetched(&[]))).unwrap();
    app.poll_fetch_all();
    let ended = app
        .fetch_all
        .as_ref()
        .and_then(|job| job.finished_at)
        .expect("the run is over");

    // when the picker opens a minute later
    app.forget_fetch_all_finished_before(ended + Duration::from_secs(60));
    // then: the results are still news
    assert!(app.fetch_all.is_some());

    // when it opens an hour later
    app.forget_fetch_all_finished_before(ended + Duration::from_secs(3600));
    // then: "up to date" is no longer shown as if it were
    assert!(app.fetch_all.is_none());
}

#[test]
fn should_keep_a_running_fetch_all_however_long_it_takes() {
    // given: a run still going
    let world = World::new();
    let a = world.local("a");
    let mut app = build_review_app(&["x"]);
    let _tx = install_run(&mut app, &[&a]);

    // when
    app.forget_fetch_all_finished_before(Instant::now() + Duration::from_secs(24 * 3600));

    // then
    assert_eq!(app.fetch_all_progress(), Some((0, 1)));
}

#[test]
fn should_keep_a_running_fetch_all_when_the_picker_closes() {
    // given
    let world = World::new();
    let a = world.local("a");
    let mut app = build_review_app(&["x"]);
    let _tx = install_run(&mut app, &[&a]);
    app.open_repo_picker();

    // when
    app.close_repo_picker();

    // then
    assert_eq!(app.fetch_all_progress(), Some((0, 1)));
}

// ── On screen ────────────────────────────────────────────────────────────

#[test]
fn should_count_the_run_off_in_the_status_bar() {
    // given: a review on screen, and a run one third done
    let world = World::new();
    let (a, b, c) = (world.local("a"), world.local("b"), world.local("c"));
    let mut app = build_review_app(&["x"]);
    let tx = install_run(&mut app, &[&a, &b, &c]);
    tx.send(FetchAllEvent::Finished(0, fetched(&[]))).unwrap();
    app.poll_fetch_all();

    // when
    let running = screen(&mut app);
    app.set_message("something else");
    let with_message = screen(&mut app);

    // then: on the bottom row, unless a message has the slot
    let last = running.last().unwrap();
    assert!(last.contains(" Fetching 1/3 "), "{last:?}");
    let last = with_message.last().unwrap();
    assert!(last.contains("something else"), "{last:?}");
    assert!(!last.contains("Fetching"), "{last:?}");
}

#[test]
fn should_count_the_run_off_in_the_selector_footer() {
    // given: the fullscreen selector, and a run that has not got anywhere
    let world = World::new();
    let home = world.local("home");
    let other = world.local("other");
    let mut app = world.launch(&home);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    let _tx = install_run(&mut app, &[&home, &other]);
    app.message = None;

    // when
    let screen = screen(&mut app);

    // then
    let last = screen.last().unwrap();
    assert!(last.contains(" Fetching 0/2 "), "{last:?}");
}

#[test]
fn should_say_nothing_about_fetching_once_the_run_is_over() {
    // given
    let world = World::new();
    let a = world.local("a");
    let mut app = build_review_app(&["x"]);
    let tx = install_run(&mut app, &[&a]);
    tx.send(FetchAllEvent::Finished(0, fetched(&[]))).unwrap();
    app.poll_fetch_all();
    app.message = None;

    // when
    let screen = screen(&mut app);

    // then
    assert!(
        !screen.iter().any(|line| line.contains("Fetching")),
        "{}",
        screen.join("\n")
    );
}

// ── Where a user finds it ────────────────────────────────────────────────

#[test]
fn should_run_fetch_all_from_the_command_palette() {
    // given
    let mut app = build_review_app(&["x"]);
    app.open_palette();

    // when: searched for by what it does
    for c in "fetch all".chars() {
        app.palette_push(c);
    }

    // then: the entry that runs it comes before the picker's own Ctrl-f row
    let palette = app.palette.as_ref().expect("palette open");
    let top = crate::input::catalog::ENTRIES[palette.matches[palette.cursor]];
    assert_eq!(top.run, crate::input::catalog::Run::Command("fetchall"));
    assert_eq!(top.keys, ":fetchall");

    // when it is run
    app.palette_run();

    // then: `:fetchall` and Enter are replayed as keys
    let typed: String = app
        .injected_keys
        .iter()
        .filter_map(|key| match key.code {
            crossterm::event::KeyCode::Char(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(typed, ":fetchall");
    assert_eq!(
        app.injected_keys.back().map(|key| key.code),
        Some(crossterm::event::KeyCode::Enter)
    );
}

#[test]
fn should_list_the_repo_switcher_and_fetch_all_in_the_help() {
    // given: a configured leader, to see it is the one shown
    let mut app = build_review_app(&["x"]);
    app.leader_key = ',';
    app.toggle_help();
    assert_eq!(app.input_mode, InputMode::Help);

    // when
    screen(&mut app);

    // then
    let help = &app.help_state.searchable_lines;
    let line = |needle: &str| {
        help.iter()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no help line with {needle:?}"))
    };
    assert!(line("  ,r        ").contains("Repo switcher"));
    assert!(line("Repo Switcher (,r, `:repos`)").starts_with("Repo Switcher"));
    assert!(line("  Ctrl-x    Remove the repository").contains("list"));
    assert!(line("  Ctrl-f    Fetch all").contains("`:fetchall`"));
    assert!(line("  ,r ,b ,p  ").contains("over the selector"));
    assert!(line("  :repos    ").contains("alias `:repo`, ,r"));
    assert!(line("  :repo <path>").contains("Tab completes"));
    assert!(line("  :fetchall ").contains("`:console`"));
}

#[test]
fn should_advertise_the_repo_switcher_in_the_hint_lines() {
    // given: a review, and the fullscreen selector of a repository
    let world = World::new();
    let home = world.local("home");
    let mut review = build_review_app(&["x"]);
    let mut selector = world.launch(&home);
    selector.message = None;

    // when: both drawn wide enough to list every key
    let review = screen_of_width(&mut review, 200);
    let selector = screen_of_width(&mut selector, 260);

    // then
    let last = review.last().unwrap();
    assert!(last.contains(";b branches \u{00b7} ;r repos"), "{last:?}");
    let last = selector.last().unwrap();
    assert!(last.contains("B branches \u{00b7} ;r repos"), "{last:?}");
}
