//! tuicr keeps working on the repository it was started in after the
//! process's working directory has moved somewhere else.
//!
//! The embedded editor makes that an ordinary event: `:cd` typed in Helix
//! changes the working directory of the whole process. tuicr used to look
//! its repository up again from the working directory after every checkout
//! and for every fetch, pull and push, so a `:cd ~/src/other-project` in the
//! editor made `:push` push the other project, and after the next branch
//! switch the review showed the other project and committed there.
//!
//! This file holds exactly one test: it changes the working directory,
//! which is the whole process's.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tuicr::app::{App, AppStartupOptions, CommitSelectionStart, RemoteOp};
use tuicr::theme::Theme;
use tuicr::vcs::{DiffWhitespaceMode, git::GitBackendPreference};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A repository with one commit pushed to a bare `origin` beside it, one
/// commit that is not pushed yet, and a change in the working tree.
fn project(parent: &Path, name: &str) -> (PathBuf, PathBuf) {
    let origin = parent.join(format!("{name}-origin.git"));
    std::fs::create_dir_all(&origin).expect("origin dir");
    git(&origin, &["init", "-q", "--bare", "-b", "main"]);

    let work = parent.join(name);
    std::fs::create_dir_all(&work).expect("work dir");
    git(&work, &["init", "-q", "-b", "main"]);
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().expect("utf-8")],
    );
    std::fs::write(work.join("file.txt"), format!("{name}: first\n")).expect("write");
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", &format!("{name}: first")]);
    git(&work, &["push", "-q", "-u", "origin", "main"]);
    std::fs::write(work.join("file.txt"), format!("{name}: second\n")).expect("write");
    git(&work, &["commit", "-q", "-am", &format!("{name}: second")]);
    std::fs::write(
        work.join("file.txt"),
        format!("{name}: in the working tree\n"),
    )
    .expect("write");
    // As the system spells it, which is how git reports it.
    (work.canonicalize().expect("canonical"), origin)
}

fn branches(dir: &Path) -> Vec<String> {
    git(dir, &["branch", "--format=%(refname:short)"])
        .lines()
        .map(str::to_string)
        .collect()
}

/// Run a remote operation to its end, as the event loop's polls do.
fn run_remote_op(app: &mut App, op: RemoteOp) -> String {
    app.start_remote_op(op);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !app.poll_remote_op() {
        assert!(Instant::now() < deadline, "the remote operation hangs");
        std::thread::sleep(Duration::from_millis(10));
    }
    app.message
        .as_ref()
        .map(|message| message.content.clone())
        .unwrap_or_default()
}

#[test]
fn should_stay_with_its_repository_when_the_working_directory_moves() {
    let temp = tempfile::tempdir().expect("temp dir");
    // Nothing of the user's is read or written: no git config, and tuicr's
    // sessions and Helix's files would land in the temporary directory.
    // SAFETY: this binary runs one test, and no other thread exists yet.
    unsafe {
        for (var, value) in [
            ("HOME", temp.path().join("home")),
            ("XDG_CONFIG_HOME", temp.path().join("home/config")),
            ("XDG_DATA_HOME", temp.path().join("home/data")),
            ("XDG_CACHE_HOME", temp.path().join("home/cache")),
            ("XDG_STATE_HOME", temp.path().join("home/state")),
            ("GIT_CONFIG_GLOBAL", PathBuf::from("/dev/null")),
            ("GIT_CONFIG_SYSTEM", PathBuf::from("/dev/null")),
        ] {
            std::env::set_var(var, value);
        }
        for (var, value) in [
            ("GIT_AUTHOR_NAME", "tuicr test"),
            ("GIT_AUTHOR_EMAIL", "tuicr-test@example.invalid"),
            ("GIT_COMMITTER_NAME", "tuicr test"),
            ("GIT_COMMITTER_EMAIL", "tuicr-test@example.invalid"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ] {
            std::env::set_var(var, value);
        }
    }
    std::fs::create_dir_all(temp.path().join("home")).expect("home");

    let (here, here_origin) = project(temp.path(), "here");
    let (elsewhere, elsewhere_origin) = project(temp.path(), "elsewhere");

    // tuicr is started in one project...
    std::env::set_current_dir(&here).expect("chdir");
    let mut app = App::new(
        Theme::dark(),
        None,
        false,
        AppStartupOptions {
            revisions: None,
            working_tree: true,
            path_filter: None,
            file_path: None,
            all_files: false,
            show_pr_checks: false,
            show_pr_comments: false,
            pr_comments_visibility: None,
            git_backend_preference: GitBackendPreference::Libgit2,
            diff_whitespace_mode: DiffWhitespaceMode::default(),
            commit_selection: CommitSelectionStart::All,
            pr_target: None,
            repo_url_override: None,
            repo_root: None,
        },
    )
    .unwrap_or_else(|err| panic!("the review of `here` opens: {err}"));
    assert_eq!(app.vcs_info.root_path, here);

    // ...and the user types `:cd` to another one in the embedded editor,
    // which moves the whole process.
    std::env::set_current_dir(&elsewhere).expect("chdir");

    // A new branch: created where the review is, and the reopen that
    // follows every move of HEAD stays there too.
    app.create_branch("made-in-the-review");
    assert_eq!(app.vcs_info.root_path, here, "{:?}", app.message);
    assert_eq!(
        app.vcs_info.branch_name.as_deref(),
        Some("made-in-the-review")
    );
    // The second one is the one that used to land in the other project,
    // because by then tuicr had reopened itself there.
    app.create_branch("and-another");
    assert_eq!(app.vcs_info.root_path, here, "{:?}", app.message);
    assert_eq!(
        branches(&here),
        ["and-another", "made-in-the-review", "main"]
    );
    assert_eq!(branches(&elsewhere), ["main"]);
    assert_eq!(git(&elsewhere, &["branch", "--show-current"]), "main");

    // Reopening for another whitespace mode.
    app.set_ignore_whitespace(true);
    assert_eq!(app.vcs_info.root_path, here, "{:?}", app.message);
    assert!(
        app.diff_files
            .iter()
            .any(|file| file.display_path().ends_with("file.txt")),
        "the review still shows this project's change"
    );

    // A push, which runs on a worker thread with a handle of its own: it
    // is this project's branch that goes to this project's remote.
    git(&here, &["switch", "-q", "main"]);
    let said = run_remote_op(&mut app, RemoteOp::Push);
    assert_eq!(
        git(&here_origin, &["log", "-1", "--format=%s", "main"]),
        "here: second",
        "{said}"
    );
    assert_eq!(
        git(&elsewhere_origin, &["log", "-1", "--format=%s", "main"]),
        "elsewhere: first",
        "the other project was not pushed: {said}"
    );
    assert_eq!(app.vcs_info.root_path, here);

    // A fetch likewise: the other project's remote-tracking branch stays
    // behind its remote, which got a commit meanwhile.
    let other_clone = temp.path().join("someone-else");
    git(
        temp.path(),
        &[
            "clone",
            "-q",
            elsewhere_origin.to_str().expect("utf-8"),
            other_clone.to_str().expect("utf-8"),
        ],
    );
    std::fs::write(other_clone.join("new.txt"), "new\n").expect("write");
    git(&other_clone, &["add", "-A"]);
    git(&other_clone, &["commit", "-q", "-m", "elsewhere: third"]);
    git(&other_clone, &["push", "-q", "origin", "main"]);
    let said = run_remote_op(&mut app, RemoteOp::Fetch);
    assert_eq!(
        git(&elsewhere, &["log", "-1", "--format=%s", "origin/main"]),
        "elsewhere: first",
        "the other project was not fetched: {said}"
    );
    assert_eq!(app.vcs_info.root_path, here);

    // Back, so that the temporary directory can go.
    std::env::set_current_dir(temp.path()).expect("chdir");
}
