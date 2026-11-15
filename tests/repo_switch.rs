//! Real-binary regression for switching repositories: the main-loop wiring
//! (`:repo <path>`, the leader in the selector, the swap between events,
//! the repaint) that App tests cannot reach. Runs tuicr in an isolated tmux
//! pane with its own HOME, so the user's list of repositories and reviews
//! are never touched. Skipped when tmux is not installed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn tmux(socket: &Path, args: &[&str]) -> String {
    let out = Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .args(args)
        .output()
        .expect("run tmux");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository at `root` with one commit whose subject is `subject`.
fn make_repo(root: &Path, subject: &str) {
    std::fs::create_dir_all(root).expect("create repo dir");
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("file.txt"), "content\n").expect("write file");
    git(&["add", "file.txt"]);
    git(&[
        "-c",
        "user.name=tester",
        "-c",
        "user.email=tester@example.com",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        subject,
    ]);
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
    }
    None
}

#[test]
fn should_switch_repositories_by_path_and_flip_back_with_the_picker() {
    if !tmux_available() {
        eprintln!("skipping: tmux not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    // Resolved, so the paths typed below are the ones tuicr records.
    let base = dir.path().canonicalize().expect("canonical tempdir");
    let home = base.join("home");
    let alpha = base.join("alpha");
    let beta = base.join("beta");
    make_repo(&alpha, "alpha-only-subject");
    make_repo(&beta, "beta-only-subject");
    // A leader other than the default: the app made by a switch has to
    // honor the config too, and `,` needs no escaping on a tmux command line.
    let config_dir = home.join(".config").join("tuicr");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    std::fs::write(config_dir.join("config.toml"), "leader = \",\"\n").expect("write config");

    let socket = base.join("tmux.sock");
    // Always stop this test's isolated server, including on assertion failure.
    struct Server<'a>(&'a Path);
    impl Drop for Server<'_> {
        fn drop(&mut self) {
            tmux(self.0, &["kill-server"]);
        }
    }
    let _server = Server(&socket);

    let command = format!(
        "env HOME={home} XDG_CONFIG_HOME={home}/.config XDG_DATA_HOME={home}/.local/share \
         {bin} --no-update-check",
        home = home.display(),
        bin = env!("CARGO_BIN_EXE_tuicr"),
    );
    tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-x",
            "120",
            "-y",
            "30",
            "-c",
            alpha.to_str().unwrap(),
            &command,
        ],
    );
    let wait_screen = |text: &str| -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let screen = tmux(&socket, &["capture-pane", "-p"]);
            if screen.contains(text) {
                return screen;
            }
            assert!(Instant::now() < deadline, "missing {text}:\n{screen}");
            sleep(Duration::from_millis(50));
        }
    };

    // Launched in alpha: the selector lists its commit.
    let screen = wait_screen("alpha-only-subject");
    assert!(!screen.contains("beta-only-subject"), "{screen}");

    // `:repo <path>` opens a repository that was never opened before.
    let typed = format!(":repo {}", beta.display());
    tmux(&socket, &["send-keys", "-l", &typed]);
    tmux(&socket, &["send-keys", "Enter"]);
    let screen = wait_screen("beta-only-subject");
    assert!(
        !screen.contains("alpha-only-subject"),
        "nothing of alpha may be left on screen:\n{screen}"
    );
    wait_screen("Switched to beta");

    // The leader works in the selector, and the picker lists both, with the
    // cursor on the one that is not on screen.
    tmux(&socket, &["send-keys", "-l", ",r"]);
    let screen = wait_screen(" Repositories ");
    assert!(screen.contains("> alpha"), "{screen}");
    assert!(screen.contains("* beta"), "{screen}");

    // Enter flips back.
    tmux(&socket, &["send-keys", "Enter"]);
    let screen = wait_screen("alpha-only-subject");
    assert!(!screen.contains("beta-only-subject"), "{screen}");
    assert!(!screen.contains(" Repositories "), "{screen}");
    wait_screen("Switched to alpha");

    // The list lives under the isolated home, most recent first.
    let store = find_file(&home, "recent_repos.json").expect("a list under the isolated home");
    let list: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&store).expect("read the list"))
            .expect("the list is JSON");
    let paths: Vec<&str> = list["repos"]
        .as_array()
        .expect("repos")
        .iter()
        .map(|repo| repo["path"].as_str().expect("path"))
        .collect();
    assert_eq!(paths, [alpha.to_str().unwrap(), beta.to_str().unwrap()]);
}
