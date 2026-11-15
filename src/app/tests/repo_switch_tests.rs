//! Switching repositories in process, against real repositories in temp
//! directories: `App::switch_repo`, the picker's choices on a live app, and
//! the recent-repositories list they keep.
//!
//! Nothing here depends on the process's working directory (every app is
//! opened at an explicit root, and `switch_repo` never changes directory),
//! and the list file and the reviews directory are per-test temp paths.

use std::path::PathBuf;
use std::process::Command;

use crate::app::*;
use crate::editor::EditorTarget;
use crate::helix::fake::{FakePane, PaneCall};
use crate::persistence::recent_repos;
use crate::persistence::storage;
use crate::update::UpdateInfo;

/// A git repository named `name`, in a temp directory of its own, with one
/// commit whose subject is `subject`. Dropping it deletes it.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    name: String,
    subject: String,
}

impl Repo {
    fn new(name: &str, subject: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(name);
        std::fs::create_dir(&root).expect("create the repository directory");
        let repo = Self {
            // The root as tuicr knows it: symlinks resolved.
            root: recent_repos::canonical(&root),
            _dir: dir,
            name: name.to_string(),
            subject: subject.to_string(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "tester"]);
        repo.git(&["config", "user.email", "tester@example.com"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.root.join("file.txt"), format!("{name}\n")).expect("write");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", subject]);
        repo
    }

    fn root(&self) -> PathBuf {
        self.root.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Per-test state: a reviews directory (thread-local, so parallel tests do
/// not share it) and a recent-repositories file.
struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        storage::set_test_reviews_dir(Some(dir.path().join("reviews")));
        Self { dir }
    }

    fn store(&self) -> PathBuf {
        self.dir.path().join("recent_repos.json")
    }

    fn reviews(&self) -> PathBuf {
        self.dir.path().join("reviews")
    }

    fn settings(&self) -> LaunchSettings {
        LaunchSettings {
            recent_repos_path: Some(self.store()),
            ..LaunchSettings::default()
        }
    }

    /// The paths in the list, most recent first.
    fn listed(&self) -> Vec<PathBuf> {
        recent_repos::load(&self.store())
            .into_iter()
            .map(|repo| repo.path)
            .collect()
    }

    /// The session files of live tuicr instances, i.e. of this test's
    /// app, with symlinks resolved.
    fn active_sessions(&self) -> Vec<PathBuf> {
        storage::active_session_paths_in_dir(&self.reviews())
            .expect("active sessions")
            .into_iter()
            .map(|path| recent_repos::canonical(&path))
            .collect()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        storage::set_test_reviews_dir(None);
    }
}

/// What `main.rs` does to start in `repo` with no arguments.
fn launch_with(repo: &Repo, settings: &LaunchSettings) -> App {
    let root = repo.root();
    let mut app = App::new(
        Theme::dark(),
        settings.comment_types(),
        settings.output_to_stdout,
        settings.startup_options(Some(&root)),
    )
    .expect("open the fixture repository");
    let warnings = app.finish_launch(settings, None);
    assert!(warnings.is_empty(), "{warnings:?}");
    app.record_current_repo();
    app
}

fn launch(repo: &Repo, sandbox: &Sandbox) -> App {
    launch_with(repo, &sandbox.settings())
}

fn message(app: &App) -> (String, MessageType) {
    let message = app.message.as_ref().expect("a message");
    (message.content.clone(), message.message_type.clone())
}

fn subjects(app: &App) -> Vec<&str> {
    app.commit_list
        .iter()
        .map(|commit| commit.summary.as_str())
        .collect()
}

fn root_of(app: &App) -> PathBuf {
    recent_repos::canonical(app.current_repo_root().expect("a repository root"))
}

// ── Switching ────────────────────────────────────────────────────────────

#[test]
fn should_land_in_the_selector_of_the_target_repository() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    assert_eq!(subjects(&app), [alpha.subject.as_str()]);

    // when
    let switched = app.switch_repo(&beta.root(), &settings, None);

    // then
    assert!(switched);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert_eq!(root_of(&app), beta.root());
    assert_eq!(
        recent_repos::canonical(&app.vcs_info.root_path),
        beta.root()
    );
    assert_eq!(subjects(&app), [beta.subject.as_str()]);
    assert_eq!(app.session.repo_path, app.vcs_info.root_path);
    let (text, kind) = message(&app);
    assert_eq!(text, format!("Switched to {}", beta.name()));
    assert_eq!(kind, MessageType::Info);
    assert!(
        app.take_full_redraw(),
        "every cell has to be repainted after a switch"
    );
}

#[test]
fn should_keep_the_list_most_recent_first_across_switches() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    assert_eq!(sandbox.listed(), [alpha.root()]);

    // when
    assert!(app.switch_repo(&beta.root(), &settings, None));
    let after_first = sandbox.listed();
    assert!(app.switch_repo(&alpha.root(), &settings, None));
    let after_second = sandbox.listed();

    // then
    assert_eq!(after_first, [beta.root(), alpha.root()]);
    assert_eq!(after_second, [alpha.root(), beta.root()]);
    assert_eq!(
        app.recent_repos_path.as_deref(),
        Some(sandbox.store().as_path()),
        "the new app keeps writing to the same list"
    );
}

#[test]
fn should_flip_between_the_last_two_repositories_with_the_picker_and_enter() {
    // given: alpha, then beta, then gamma was opened
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let gamma = Repo::new("gamma", "commit in gamma");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    assert!(app.switch_repo(&beta.root(), &settings, None));
    assert!(app.switch_repo(&gamma.root(), &settings, None));

    // when: the picker is opened and Enter pressed, twice
    let mut visited = Vec::new();
    for _ in 0..2 {
        app.open_repo_picker();
        app.repo_picker_confirm();
        let target = app.take_pending_repo_switch().expect("a switch was asked");
        assert!(app.switch_repo(&target, &settings, None));
        visited.push(root_of(&app));
    }

    // then: back to beta, then to gamma again; alpha is never in the way
    assert_eq!(visited, [beta.root(), gamma.root()]);
}

#[test]
fn should_stay_put_and_keep_working_when_the_target_is_not_a_repository() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let plain = tempfile::tempdir().expect("tempdir");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    let session_path = app.session_path.clone().expect("a session file");
    let listed = sandbox.listed();

    // when
    let switched = app.switch_repo(plain.path(), &settings, None);

    // then: refused, with the reason
    assert!(!switched);
    let (text, kind) = message(&app);
    assert_eq!(kind, MessageType::Warning);
    let plain_name = plain.path().file_name().unwrap().to_string_lossy();
    assert!(
        text.starts_with(&format!("Cannot open {plain_name}: Not a repository (")),
        "{text}"
    );
    // and nothing about the running app moved
    assert_eq!(root_of(&app), alpha.root());
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert_eq!(subjects(&app), [alpha.subject.as_str()]);
    assert_eq!(app.session_path.as_ref(), Some(&session_path));
    assert!(session_path.exists(), "the review session file is kept");
    assert_eq!(
        sandbox.active_sessions(),
        [recent_repos::canonical(&session_path)]
    );
    assert_eq!(sandbox.listed(), listed);
    assert!(!app.repo_switch_pending());
    // and it still reads its own repository
    app.after_head_moved();
    assert_eq!(
        recent_repos::canonical(&app.vcs_info.root_path),
        alpha.root()
    );
    assert_eq!(subjects(&app), [alpha.subject.as_str()]);
}

#[test]
fn should_stay_put_when_the_target_repository_has_nothing_to_review() {
    // given: a repository with no commit and no change
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let empty = tempfile::tempdir().expect("tempdir");
    let init = Command::new("git")
        .current_dir(empty.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .expect("git init");
    assert!(init.status.success());
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);

    // when
    let switched = app.switch_repo(empty.path(), &settings, None);

    // then
    assert!(!switched);
    assert_eq!(root_of(&app), alpha.root());
    let (text, kind) = message(&app);
    assert_eq!(kind, MessageType::Warning);
    assert!(text.contains(": No changes to review ("), "{text}");
    assert_eq!(sandbox.listed(), [alpha.root()], "it was never opened");
}

#[test]
fn should_drop_a_vanished_target_from_the_list() {
    // given: beta was opened once and has since been deleted
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let beta_root = beta.root();
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    assert!(app.switch_repo(&beta_root, &settings, None));
    assert!(app.switch_repo(&alpha.root(), &settings, None));
    drop(beta);
    assert!(!beta_root.exists());
    assert_eq!(sandbox.listed(), [alpha.root(), beta_root.clone()]);

    // when
    let switched = app.switch_repo(&beta_root, &settings, None);

    // then
    assert!(!switched);
    assert_eq!(root_of(&app), alpha.root());
    assert_eq!(sandbox.listed(), [alpha.root()]);
    let (text, kind) = message(&app);
    assert_eq!(kind, MessageType::Warning);
    assert!(
        text.starts_with("beta no longer exists; removed from the list ("),
        "{text}"
    );
}

#[test]
fn should_save_an_unsaved_review_before_leaving() {
    // given: a comment that has not been written yet
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    let alpha_session = app.session_path.clone().expect("a session file");
    app.session.review_comments.push(crate::model::Comment::new(
        "do not lose me".to_string(),
        crate::model::CommentType::default(),
        None,
    ));
    app.dirty = true;

    // when
    assert!(app.switch_repo(&beta.root(), &settings, None));

    // then: alpha's review is on disk with the comment
    let saved = storage::load_session(&alpha_session).expect("alpha's session was saved");
    let contents: Vec<&str> = saved
        .review_comments
        .iter()
        .map(|comment| comment.content.as_str())
        .collect();
    assert_eq!(contents, ["do not lose me"]);
    // and the new app starts clean, on a session of its own
    assert!(!app.dirty);
    assert!(app.session.review_comments.is_empty());
    assert_ne!(app.session_path.as_ref(), Some(&alpha_session));
}

#[test]
fn should_leave_no_empty_session_file_behind_and_mark_the_new_one_active() {
    // given: nothing was reviewed in alpha
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    let alpha_session = app.session_path.clone().expect("a session file");
    assert!(alpha_session.exists());
    assert_eq!(
        sandbox.active_sessions(),
        [recent_repos::canonical(&alpha_session)]
    );

    // when
    assert!(app.switch_repo(&beta.root(), &settings, None));

    // then: what quitting would have cleaned up is gone
    assert!(!alpha_session.exists());
    let beta_session = app.session_path.clone().expect("a session file");
    assert!(beta_session.exists());
    assert_eq!(
        sandbox.active_sessions(),
        [recent_repos::canonical(&beta_session)],
        "this process has one active review: the new one"
    );
}

#[test]
fn should_carry_the_process_state_and_nothing_of_the_old_repository() {
    // given: an app with all sorts of state from alpha
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    // process-level: survives
    app.update_info = Some(UpdateInfo {
        current_version: "1.0.0".to_string(),
        latest_version: "9.9.9".to_string(),
        update_available: true,
        is_ahead: false,
    });
    app.theme = Theme::light();
    // repository-level: must not survive
    let (remote_tx, remote_rx) = std::sync::mpsc::channel();
    app.remote_op = Some(RemoteOpInFlight {
        op: RemoteOp::Fetch,
        rx: remote_rx,
    });
    let (status_tx, status_rx) = std::sync::mpsc::channel();
    app.status_probe_rx = Some(status_rx);
    app.commit_filter = Some(crate::vcs::CommitQuery {
        text: vec!["alpha-only".to_string()],
        ..Default::default()
    });
    app.branch_picker = Some(BranchPicker {
        purpose: BranchPickerPurpose::Checkout,
        branches: Vec::new(),
        filter: "stale".to_string(),
        cursor: 0,
        return_mode: InputMode::CommitSelect,
        picked: Vec::new(),
    });
    app.current_pr_head = Some("0123456789abcdef".to_string());
    app.pending_count = Some(7);
    app.injected_keys.push_back(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    ));
    app.last_change_status = Some(crate::vcs::traits::VcsChangeStatus {
        staged: true,
        unstaged: true,
    });

    // when
    assert!(app.switch_repo(&beta.root(), &settings, None));

    // then: what belongs to the process came along
    assert_eq!(
        app.update_info
            .as_ref()
            .map(|info| info.latest_version.as_str()),
        Some("9.9.9")
    );
    assert_eq!(app.theme.panel_bg, Theme::light().panel_bg);
    // and what belonged to alpha is gone
    assert!(app.remote_op.is_none());
    assert!(app.status_probe_rx.is_none());
    assert!(app.commit_filter.is_none());
    assert!(app.branch_picker.is_none());
    assert!(app.repo_picker.is_none());
    assert!(app.current_pr_head.is_none());
    assert!(app.pr_commits.is_empty());
    assert!(!matches!(app.diff_source, DiffSource::PullRequest(_)));
    assert!(app.pending_count.is_none());
    assert!(app.injected_keys.is_empty());
    assert_eq!(
        app.last_change_status,
        Some(crate::vcs::traits::VcsChangeStatus {
            staged: false,
            unstaged: false,
        }),
        "the status is beta's own, read when it was opened"
    );
    // the old app's receivers were dropped with it, so a worker still
    // running for alpha has nowhere to deliver its answer
    assert!(
        remote_tx
            .send(RemoteOpAnswer {
                op: RemoteOp::Fetch,
                result: Ok("late".to_string()),
            })
            .is_err()
    );
    assert!(
        status_tx
            .send(Ok(crate::vcs::traits::VcsChangeStatus {
                staged: true,
                unstaged: true,
            }))
            .is_err()
    );
    // and the message says what was left running
    let (text, _) = message(&app);
    assert_eq!(
        text,
        format!(
            "Switched to {} \u{00b7} the fetch in {} is still running",
            beta.name(),
            alpha.name()
        )
    );
}

#[test]
fn should_refuse_to_switch_under_stdout() {
    // given: `--stdout`, where the caller waits for this repository's review
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let settings = LaunchSettings {
        output_to_stdout: true,
        ..sandbox.settings()
    };
    let mut app = launch_with(&alpha, &settings);
    assert!(app.output_to_stdout);

    // when
    let switched = app.switch_repo(&beta.root(), &settings, None);

    // then
    assert!(!switched);
    assert_eq!(root_of(&app), alpha.root());
    assert_eq!(
        message(&app),
        (
            "Cannot switch repositories under --stdout".to_string(),
            MessageType::Warning
        )
    );
    assert_eq!(sandbox.listed(), [alpha.root()]);
}

#[test]
fn should_say_already_in_for_the_current_repository_and_its_subdirectories() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let nested = alpha.root().join("src").join("deep");
    std::fs::create_dir_all(&nested).expect("nested dir");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    let session_path = app.session_path.clone();
    let expected = (format!("Already in {}", alpha.name()), MessageType::Info);

    // when / then: the root itself
    assert!(!app.switch_repo(&alpha.root(), &settings, None));
    assert_eq!(message(&app), expected);

    // when / then: a directory inside it
    app.message = None;
    assert!(!app.switch_repo(&nested, &settings, None));
    assert_eq!(message(&app), expected);
    assert_eq!(root_of(&app), alpha.root());
    assert_eq!(app.session_path, session_path);
    assert_eq!(sandbox.listed(), [alpha.root()]);
}

#[test]
fn should_reopen_its_own_repository_whatever_the_working_directory_is() {
    // given: the test process's working directory is this checkout, not
    // the fixture, so a reopen from the working directory would land here
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let mut app = launch(&alpha, &sandbox);
    let cwd = std::env::current_dir().expect("cwd");
    assert_ne!(recent_repos::canonical(&cwd), alpha.root());

    // when
    app.after_head_moved();

    // then
    assert_eq!(
        recent_repos::canonical(&app.vcs_info.root_path),
        alpha.root()
    );
    assert_eq!(subjects(&app), [alpha.subject.as_str()]);
    let said = app.message.as_ref().map(|m| m.content.as_str());
    assert!(
        !said.is_some_and(|text| text.contains("Reopening the repository failed")),
        "{said:?}"
    );
}

#[test]
fn should_open_a_linked_worktree_as_a_repository_of_its_own() {
    // given: a second worktree of alpha
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let parent = tempfile::tempdir().expect("tempdir");
    let worktree = parent.path().join("alpha-wt");
    alpha.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "side",
        worktree.to_str().unwrap(),
    ]);
    let worktree = recent_repos::canonical(&worktree);
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);

    // when
    let switched = app.switch_repo(&worktree, &settings, None);

    // then: two entries, one per checkout
    assert!(switched);
    assert_eq!(root_of(&app), worktree);
    assert_eq!(sandbox.listed(), [worktree, alpha.root()]);
}

#[test]
fn should_record_nothing_without_a_list_file() {
    // given: the default settings, as every other test in this crate gets
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let settings = LaunchSettings::default();
    let mut app = launch_with(&alpha, &settings);

    // when
    assert!(app.switch_repo(&beta.root(), &settings, None));
    app.open_repo_picker();

    // then
    assert!(app.recent_repos_path.is_none());
    assert!(!sandbox.store().exists());
    assert!(app.repo_picker.as_ref().unwrap().rows.is_empty());
}

// ── The picker on a live app ─────────────────────────────────────────────

/// An app in alpha with beta and gamma opened before it, so the list reads
/// alpha (current), gamma, beta.
fn app_with_history(sandbox: &Sandbox, alpha: &Repo, beta: &Repo, gamma: &Repo) -> App {
    let store = sandbox.store();
    recent_repos::record(&store, &beta.root(), 100).unwrap();
    recent_repos::record(&store, &gamma.root(), 200).unwrap();
    launch(alpha, sandbox)
}

fn picker_names(app: &App) -> Vec<String> {
    let picker = app.repo_picker.as_ref().expect("picker open");
    picker
        .visible()
        .into_iter()
        .map(|idx| picker.rows[idx].name.clone())
        .collect()
}

#[test]
fn should_open_the_picker_on_the_most_recent_other_repository() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta, gamma) = (
        Repo::new("alpha", "a"),
        Repo::new("beta", "b"),
        Repo::new("gamma", "c"),
    );
    let mut app = app_with_history(&sandbox, &alpha, &beta, &gamma);

    // when
    app.open_repo_picker();

    // then
    assert_eq!(app.input_mode, InputMode::RepoPicker);
    assert_eq!(
        picker_names(&app),
        [alpha.name(), gamma.name(), beta.name()]
    );
    let picker = app.repo_picker.as_ref().unwrap();
    assert_eq!(picker.return_mode, InputMode::CommitSelect);
    assert!(picker.rows[0].is_current);
    assert!(picker.rows.iter().all(|row| row.exists));
    assert_eq!(picker.selected().unwrap().path, gamma.root());
}

#[test]
fn should_queue_the_switch_and_close_when_a_repository_is_chosen() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta, gamma) = (
        Repo::new("alpha", "a"),
        Repo::new("beta", "b"),
        Repo::new("gamma", "c"),
    );
    let mut app = app_with_history(&sandbox, &alpha, &beta, &gamma);
    app.open_repo_picker();
    app.repo_picker_move(1);

    // when
    app.repo_picker_confirm();

    // then: asked for, not done: the main loop performs it
    assert!(app.repo_picker.is_none());
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert!(app.repo_switch_pending());
    assert_eq!(root_of(&app), alpha.root());
    assert_eq!(app.take_pending_repo_switch(), Some(beta.root()));
    assert!(!app.repo_switch_pending());
}

#[test]
fn should_only_say_already_in_when_the_current_repository_is_chosen() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta, gamma) = (
        Repo::new("alpha", "a"),
        Repo::new("beta", "b"),
        Repo::new("gamma", "c"),
    );
    let mut app = app_with_history(&sandbox, &alpha, &beta, &gamma);
    app.open_repo_picker();
    app.repo_picker_move(-1);
    assert!(
        app.repo_picker
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .is_current
    );

    // when
    app.repo_picker_confirm();

    // then
    assert!(app.repo_picker.is_none());
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert!(!app.repo_switch_pending());
    assert_eq!(
        message(&app),
        (format!("Already in {}", alpha.name()), MessageType::Info)
    );
}

#[test]
fn should_drop_a_missing_repository_when_it_is_chosen_and_stay_open() {
    // given: gamma, the row the cursor starts near, has been deleted
    let sandbox = Sandbox::new();
    let (alpha, beta, gamma) = (
        Repo::new("alpha", "a"),
        Repo::new("beta", "b"),
        Repo::new("gamma", "c"),
    );
    let mut app = app_with_history(&sandbox, &alpha, &beta, &gamma);
    let (gamma_root, gamma_name) = (gamma.root(), gamma.name());
    drop(gamma);
    app.open_repo_picker();
    {
        let picker = app.repo_picker.as_ref().unwrap();
        assert!(!picker.rows[1].exists, "shown as missing");
        assert_eq!(
            picker.selected().unwrap().path,
            beta.root(),
            "the cursor starts on a repository that can be opened"
        );
    }
    app.repo_picker_move(-1);
    assert_eq!(
        app.repo_picker.as_ref().unwrap().selected().unwrap().name,
        gamma_name
    );

    // when
    app.repo_picker_confirm();

    // then
    assert_eq!(app.input_mode, InputMode::RepoPicker, "the picker stays");
    assert!(!app.repo_switch_pending());
    assert_eq!(picker_names(&app), [alpha.name(), beta.name()]);
    assert_eq!(sandbox.listed(), [alpha.root(), beta.root()]);
    assert!(!sandbox.listed().contains(&gamma_root));
    let (text, kind) = message(&app);
    assert_eq!(kind, MessageType::Warning);
    assert!(
        text.starts_with("gamma no longer exists; removed from the list ("),
        "{text}"
    );
}

#[test]
fn should_remove_the_highlighted_repository_from_the_list_only() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta, gamma) = (
        Repo::new("alpha", "a"),
        Repo::new("beta", "b"),
        Repo::new("gamma", "c"),
    );
    let mut app = app_with_history(&sandbox, &alpha, &beta, &gamma);
    app.open_repo_picker();
    app.repo_picker_move(1);
    assert_eq!(
        app.repo_picker.as_ref().unwrap().selected().unwrap().path,
        beta.root()
    );

    // when
    app.repo_picker_remove();

    // then: off the list and off the screen, the cursor still on a row
    assert_eq!(app.input_mode, InputMode::RepoPicker);
    assert_eq!(picker_names(&app), [alpha.name(), gamma.name()]);
    assert_eq!(sandbox.listed(), [alpha.root(), gamma.root()]);
    assert_eq!(
        app.repo_picker.as_ref().unwrap().selected().unwrap().path,
        gamma.root()
    );
    assert_eq!(
        message(&app),
        (
            format!("Removed {} from the list", beta.name()),
            MessageType::Info
        )
    );
    // the repository itself is untouched
    assert!(beta.root().join(".git").exists());
}

#[test]
fn should_filter_as_the_user_types_and_restore_the_start_when_cleared() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta, gamma) = (
        Repo::new("alpha", "a"),
        Repo::new("beta", "b"),
        Repo::new("gamma", "c"),
    );
    let mut app = app_with_history(&sandbox, &alpha, &beta, &gamma);
    app.open_repo_picker();

    // when
    for c in "bet".chars() {
        app.repo_picker_push(c);
    }
    let filtered = picker_names(&app);
    let chosen = app
        .repo_picker
        .as_ref()
        .unwrap()
        .selected()
        .map(|row| row.path.clone());
    app.repo_picker_clear();

    // then: the best match first, and the cursor on it
    assert_eq!(filtered[0], beta.name());
    assert_eq!(chosen, Some(beta.root()));
    let picker = app.repo_picker.as_ref().unwrap();
    assert!(picker.filter.is_empty());
    assert_eq!(picker.selected().unwrap().path, gamma.root());
}

#[test]
fn should_wrap_single_steps_and_stop_pages_at_the_ends() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta, gamma) = (
        Repo::new("alpha", "a"),
        Repo::new("beta", "b"),
        Repo::new("gamma", "c"),
    );
    let mut app = app_with_history(&sandbox, &alpha, &beta, &gamma);
    app.open_repo_picker();
    let cursor = |app: &App| app.repo_picker.as_ref().unwrap().cursor;
    assert_eq!(cursor(&app), 1);

    // when / then
    app.repo_picker_move(1);
    assert_eq!(cursor(&app), 2);
    app.repo_picker_move(1);
    assert_eq!(cursor(&app), 0, "down from the last row wraps");
    app.repo_picker_move(-1);
    assert_eq!(cursor(&app), 2, "up from the first row wraps");
    app.repo_picker_move(-10);
    assert_eq!(cursor(&app), 0, "a page up stops at the top");
    app.repo_picker_move(10);
    assert_eq!(cursor(&app), 2, "a page down stops at the bottom");
}

#[test]
fn should_keep_the_picker_open_and_warn_under_stdout() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta) = (Repo::new("alpha", "a"), Repo::new("beta", "b"));
    recent_repos::record(&sandbox.store(), &beta.root(), 100).unwrap();
    let settings = LaunchSettings {
        output_to_stdout: true,
        ..sandbox.settings()
    };
    let mut app = launch_with(&alpha, &settings);
    app.open_repo_picker();
    assert_eq!(
        app.repo_picker.as_ref().unwrap().selected().unwrap().path,
        beta.root()
    );

    // when
    app.repo_picker_confirm();

    // then
    assert_eq!(app.input_mode, InputMode::RepoPicker);
    assert!(!app.repo_switch_pending());
    assert_eq!(message(&app).0, "Cannot switch repositories under --stdout");
}

#[test]
fn should_return_to_the_mode_the_picker_was_opened_from() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "a");
    let mut app = launch(&alpha, &sandbox);

    // when: opened from the selector, and from the command line over it
    app.open_repo_picker();
    let from_selector = app.repo_picker.as_ref().unwrap().return_mode;
    app.open_repo_picker();
    assert_eq!(
        app.input_mode,
        InputMode::RepoPicker,
        "a second open is a no-op"
    );
    app.close_repo_picker();
    let after_close = app.input_mode;
    app.enter_command_mode();
    app.open_repo_picker();
    let from_command = app.repo_picker.as_ref().unwrap().return_mode;
    app.close_repo_picker();

    // then
    assert_eq!(from_selector, InputMode::CommitSelect);
    assert_eq!(after_close, InputMode::CommitSelect);
    assert_eq!(from_command, InputMode::CommitSelect);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
}

// ── `:repo <path>` ───────────────────────────────────────────────────────

#[test]
fn should_queue_a_switch_to_a_directory_given_by_path() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta) = (Repo::new("alpha", "a"), Repo::new("beta", "b"));
    let mut app = launch(&alpha, &sandbox);
    let typed = format!("  {}  ", beta.root().display());

    // when
    app.open_repo_by_path(&typed);

    // then
    assert_eq!(app.take_pending_repo_switch(), Some(beta.root()));
    assert!(app.message.is_none());
}

#[test]
fn should_warn_about_a_path_that_is_not_a_directory() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "a");
    let mut app = launch(&alpha, &sandbox);
    let file = alpha.root().join("file.txt");

    // when
    app.open_repo_by_path(file.to_str().unwrap());

    // then
    assert!(!app.repo_switch_pending());
    assert_eq!(
        message(&app),
        (
            format!("No such directory: {}", file.display()),
            MessageType::Warning
        )
    );
}

#[test]
fn should_open_the_picker_when_no_path_is_given() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "a");
    let mut app = launch(&alpha, &sandbox);

    // when
    app.open_repo_by_path("   ");

    // then
    assert_eq!(app.input_mode, InputMode::RepoPicker);
    assert!(!app.repo_switch_pending());
}

#[test]
fn should_put_a_repository_opened_by_path_on_the_list() {
    // given: beta has never been opened
    let sandbox = Sandbox::new();
    let (alpha, beta) = (Repo::new("alpha", "a"), Repo::new("beta", "b"));
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    assert_eq!(sandbox.listed(), [alpha.root()]);

    // when: what the main loop does with `:repo <beta>`
    app.open_repo_by_path(beta.root().to_str().unwrap());
    let target = app.take_pending_repo_switch().expect("a switch was asked");
    assert!(app.switch_repo(&target, &settings, None));

    // then
    assert_eq!(sandbox.listed(), [beta.root(), alpha.root()]);
}

// ── Commands, keys and the palette ───────────────────────────────────────

use crate::handler::{
    handle_command_action, handle_mouse_event, handle_repo_picker_action, paste_target_open,
};
use crate::input::Action;
use crate::input::catalog::{ENTRIES, Run, Scope};

/// Type `text` on the `:` line and press Enter.
fn run_command(app: &mut App, text: &str) {
    app.enter_command_mode();
    for c in text.chars() {
        handle_command_action(app, Action::InsertChar(c));
    }
    handle_command_action(app, Action::SubmitInput);
}

#[test]
fn should_open_the_picker_from_the_repos_command_and_its_alias() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "a");
    let mut app = launch(&alpha, &sandbox);

    for command in ["repos", "repo", "repo  "] {
        // when
        run_command(&mut app, command);

        // then: over the selector it was typed in
        assert_eq!(app.input_mode, InputMode::RepoPicker, ":{command}");
        assert_eq!(
            app.repo_picker.as_ref().unwrap().return_mode,
            InputMode::CommitSelect
        );
        assert!(app.command_buffer.is_empty());
        handle_repo_picker_action(&mut app, Action::ExitMode);
        assert_eq!(app.input_mode, InputMode::CommitSelect);
    }
}

#[test]
fn should_queue_a_switch_from_the_repo_command_with_a_path() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta) = (Repo::new("alpha", "a"), Repo::new("beta", "b"));
    let mut app = launch(&alpha, &sandbox);

    // when
    run_command(&mut app, &format!("repo {}", beta.root().display()));

    // then: back in the selector, with the switch waiting for the main loop
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert!(app.command_buffer.is_empty());
    assert_eq!(root_of(&app), alpha.root());
    assert_eq!(app.take_pending_repo_switch(), Some(beta.root()));
}

#[test]
fn should_leave_the_command_line_and_warn_when_the_repo_path_is_wrong() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "a");
    let mut app = launch(&alpha, &sandbox);
    let missing = alpha.root().join("no-such-dir");

    // when
    run_command(&mut app, &format!("repo {}", missing.display()));

    // then
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert!(!app.repo_switch_pending());
    assert_eq!(
        message(&app).0,
        format!("No such directory: {}", missing.display())
    );
}

#[test]
fn should_complete_and_cycle_directories_after_the_repo_command() {
    // given: two directories that share a prefix, and a file that does too
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "a");
    let mut app = launch(&alpha, &sandbox);
    let parent = tempfile::tempdir().expect("tempdir");
    let parent_path = recent_repos::canonical(parent.path());
    std::fs::create_dir(parent_path.join("beta")).unwrap();
    std::fs::create_dir(parent_path.join("better")).unwrap();
    std::fs::write(parent_path.join("bet.txt"), "x").unwrap();
    let typed = format!("repo {}/be", parent_path.display());
    app.enter_command_mode();
    for c in typed.chars() {
        handle_command_action(&mut app, Action::InsertChar(c));
    }

    // when: Tab three times
    let mut seen = Vec::new();
    for _ in 0..3 {
        handle_command_action(&mut app, Action::CompleteCommand);
        seen.push(app.command_buffer.clone());
    }

    // then: each directory in turn, then around again; never the file
    let beta = format!("repo {}/beta/", parent_path.display());
    let better = format!("repo {}/better/", parent_path.display());
    assert_eq!(seen, [beta.clone(), better, beta]);
    assert_eq!(app.input_mode, InputMode::Command);
}

#[test]
fn should_say_no_directory_matches_when_completion_finds_none() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "a");
    let mut app = launch(&alpha, &sandbox);
    let typed = format!("repo {}/zzz", alpha.root().display());
    app.enter_command_mode();
    for c in typed.chars() {
        handle_command_action(&mut app, Action::InsertChar(c));
    }

    // when
    handle_command_action(&mut app, Action::CompleteCommand);

    // then
    assert_eq!(app.command_buffer, typed);
    assert_eq!(message(&app).0, "No directory matches");
}

#[test]
fn should_drive_the_picker_through_its_handler() {
    // given
    let sandbox = Sandbox::new();
    let (alpha, beta, gamma) = (
        Repo::new("alpha", "a"),
        Repo::new("beta", "b"),
        Repo::new("gamma", "c"),
    );
    let mut app = app_with_history(&sandbox, &alpha, &beta, &gamma);
    app.open_repo_picker();
    let cursor = |app: &App| app.repo_picker.as_ref().unwrap().cursor;
    let filter = |app: &App| app.repo_picker.as_ref().unwrap().filter.clone();

    // when / then: moving
    handle_repo_picker_action(&mut app, Action::RepoPickerDown);
    assert_eq!(cursor(&app), 2);
    handle_repo_picker_action(&mut app, Action::RepoPickerUp);
    assert_eq!(cursor(&app), 1);
    handle_repo_picker_action(&mut app, Action::PageUp);
    assert_eq!(cursor(&app), 0);
    handle_repo_picker_action(&mut app, Action::PageDown);
    assert_eq!(cursor(&app), 2);

    // typing, pasting and editing the filter
    assert!(paste_target_open(&app));
    handle_repo_picker_action(&mut app, Action::InsertChar('g'));
    handle_repo_picker_action(&mut app, Action::Paste("am\nma".to_string()));
    assert_eq!(filter(&app), "gamma", "a pasted newline is dropped");
    handle_repo_picker_action(&mut app, Action::DeleteChar);
    assert_eq!(filter(&app), "gamm");
    // Best match first. (Not "only match": the other rows can match too,
    // loosely, through whatever temp directory this machine put them in.)
    assert_eq!(picker_names(&app)[0], gamma.name());

    // Esc clears the filter first, and closes only when it is empty
    handle_repo_picker_action(&mut app, Action::ExitMode);
    assert_eq!(app.input_mode, InputMode::RepoPicker);
    assert_eq!(filter(&app), "");
    handle_repo_picker_action(&mut app, Action::InsertChar('x'));
    handle_repo_picker_action(&mut app, Action::ClearLine);
    assert_eq!(filter(&app), "");

    // Ctrl-x takes the row off the list, Enter asks for the switch
    handle_repo_picker_action(&mut app, Action::RepoPickerRemove);
    assert_eq!(sandbox.listed(), [alpha.root(), beta.root()]);
    handle_repo_picker_action(&mut app, Action::RepoPickerConfirm);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert_eq!(app.take_pending_repo_switch(), Some(beta.root()));
}

#[test]
fn should_move_the_picker_cursor_with_the_mouse_wheel() {
    // given
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
    let sandbox = Sandbox::new();
    let (alpha, beta, gamma) = (
        Repo::new("alpha", "a"),
        Repo::new("beta", "b"),
        Repo::new("gamma", "c"),
    );
    let mut app = app_with_history(&sandbox, &alpha, &beta, &gamma);
    app.open_repo_picker();
    let wheel = |kind| MouseEvent {
        kind,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };

    // when / then
    handle_mouse_event(&mut app, wheel(MouseEventKind::ScrollDown));
    assert_eq!(app.repo_picker.as_ref().unwrap().cursor, 2);
    handle_mouse_event(&mut app, wheel(MouseEventKind::ScrollUp));
    assert_eq!(app.repo_picker.as_ref().unwrap().cursor, 1);
}

#[test]
fn should_reach_the_switcher_from_the_command_palette() {
    // given
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "a");
    let mut app = launch(&alpha, &sandbox);
    let top = |app: &App| {
        let palette = app.palette.as_ref().expect("palette open");
        ENTRIES[palette.matches[palette.cursor]]
    };

    // when: searched by what it does
    app.open_palette();
    for c in "switch repository".chars() {
        app.palette_push(c);
    }
    let by_key = top(&app);
    app.palette_clear();
    for c in "repo switcher".chars() {
        app.palette_push(c);
    }
    let by_command = top(&app);
    app.palette_clear();
    for c in "open a repository by path".chars() {
        app.palette_push(c);
    }
    let by_path = top(&app);
    app.close_palette();

    // then
    assert_eq!(by_key.run, Run::Keys("<leader>r"));
    assert_eq!(by_command.run, Run::Command("repos"));
    assert_eq!(by_path.run, Run::Prompt("repo "));
    // and inside the picker the palette ranks the picker's own rows first
    app.open_repo_picker();
    assert_eq!(app.current_scope(), Scope::Repos);
}

// ── The embedded editor ──────────────────────────────────────────────────

fn editor_target(path: &std::path::Path) -> EditorTarget {
    EditorTarget {
        path: path.to_path_buf(),
        line: None,
        label: path.display().to_string(),
    }
}

#[test]
fn should_hand_the_embedded_editor_on_to_the_repository_switched_to() {
    // given: an editor that was used in alpha and holds an unsaved buffer
    let screen = ratatui::layout::Size::new(80, 24);
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let beta = Repo::new("beta", "commit in beta");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    let (factory, pane) = FakePane::factory();
    assert!(app.install_editor_pane_factory(factory));
    let in_alpha = alpha.root().join("file.txt");
    app.open_editor_pane(&editor_target(&in_alpha), screen)
        .expect("the fake pane opens anything");
    app.leave_editor_mode();
    pane.borrow_mut().state.modified = vec![in_alpha.clone()];

    // when
    assert!(app.switch_repo(&beta.root(), &settings, None));

    // then: the editor was not ended with the app that was replaced
    assert!(!pane.borrow().calls.contains(&PaneCall::Shutdown));
    assert!(app.editor_pane_installed());
    assert!(app.editor_pane_available());
    assert_eq!(app.input_mode, InputMode::CommitSelect);

    // and it is the same editor that opens the new repository's files
    let in_beta = beta.root().join("file.txt");
    app.open_editor_pane(&editor_target(&in_beta), screen)
        .expect("the pane that came along opens the file");
    assert_eq!(app.input_mode, InputMode::Editor);
    assert_eq!(pane.borrow().created_at.len(), 1, "built once, not twice");
    assert!(pane.borrow().calls.contains(&PaneCall::Open {
        path: in_beta,
        line: None,
    }));

    // and what it has not saved, from alpha, still holds a quit back
    app.leave_editor_mode();
    assert!(app.quit_blocked_by_editor());
}

#[test]
fn should_leave_the_editor_alone_when_the_switch_does_not_happen() {
    // given
    let screen = ratatui::layout::Size::new(80, 24);
    let sandbox = Sandbox::new();
    let alpha = Repo::new("alpha", "commit in alpha");
    let settings = sandbox.settings();
    let mut app = launch(&alpha, &sandbox);
    let (factory, pane) = FakePane::factory();
    assert!(app.install_editor_pane_factory(factory));
    app.open_editor_pane(&editor_target(&alpha.root().join("file.txt")), screen)
        .expect("the fake pane opens anything");
    app.leave_editor_mode();
    let not_a_repository = tempfile::tempdir().expect("tempdir");

    // when: the app built for the target is dropped unused
    assert!(!app.switch_repo(not_a_repository.path(), &settings, None));

    // then
    assert!(!pane.borrow().calls.contains(&PaneCall::Shutdown));
    assert!(app.editor_pane_available());
}
