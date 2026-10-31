//! Branch, tag, and push commands against a scripted git: each `run_git`
//! call is answered from a table keyed by its arguments.

use std::sync::{Arc, Mutex};

use super::log_pane_tests::build_review_app;
use crate::app::*;
use crate::model::FileStatus;
use crate::vcs::traits::VcsType;

type Script = Vec<(
    &'static str,
    std::result::Result<&'static str, &'static str>,
)>;

struct ScriptedGit {
    info: VcsInfo,
    script: Script,
    calls: Arc<Mutex<Vec<String>>>,
}

impl VcsBackend for ScriptedGit {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, _highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        Err(TuicrError::NoChanges)
    }

    fn fetch_context_lines(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
        _start_line: u32,
        _end_line: u32,
    ) -> Result<Vec<DiffLine>> {
        Ok(Vec::new())
    }

    fn file_line_count(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
    ) -> Result<u32> {
        Ok(0)
    }

    fn run_git(&self, args: &[&str], _network: bool) -> Result<String> {
        let line = args.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        match self
            .script
            .iter()
            .find(|(prefix, _)| line.starts_with(prefix))
        {
            Some((_, Ok(out))) => Ok(out.to_string()),
            Some((_, Err(err))) => Err(TuicrError::VcsCommand(err.to_string())),
            None => Err(TuicrError::VcsCommand(format!("unscripted: {line}"))),
        }
    }
}

fn app_with_git(script: Script) -> (App, Arc<Mutex<Vec<String>>>) {
    let mut app = build_review_app(&["a"]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let info = VcsInfo {
        root_path: std::path::PathBuf::from("/tmp/no-such-repo"),
        head_commit: "head".to_string(),
        branch_name: Some("main".to_string()),
        vcs_type: VcsType::Git,
    };
    app.vcs_info = info.clone();
    app.vcs = Box::new(ScriptedGit {
        info,
        script,
        calls: calls.clone(),
    });
    (app, calls)
}

#[test]
fn push_preview_lists_outgoing_commits_and_target() {
    let (mut app, _) = app_with_git(vec![
        ("rev-parse --git-dir", Ok(".git\n")),
        ("rev-parse --abbrev-ref", Ok("origin/main\n")),
        ("rev-list --left-right", Ok("2\t0\n")),
        (
            "log --format=%h %s -n 50 origin/main..HEAD",
            Ok("abc1234 two\ndef5678 one\n"),
        ),
    ]);
    app.push_preview(false);
    assert_eq!(app.input_mode, InputMode::Confirm);
    assert_eq!(app.pending_confirm, Some(ConfirmAction::GitCommand));
    let pending = app.pending_git.as_ref().expect("push pending");
    assert_eq!(pending.prompt, "Push 2 commits from main to origin/main?");
    assert_eq!(pending.detail, vec!["  abc1234 two", "  def5678 one"]);
    assert_eq!(pending.then, GitThen::Remote(RemoteOp::Push));
    assert_eq!(app.repo_status.as_ref().unwrap().ahead, 2);

    app.confirm_git_no();
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(app.pending_git.is_none());
}

#[test]
fn push_refuses_when_behind_unless_forced() {
    let script = || -> Script {
        vec![
            ("rev-parse --git-dir", Ok(".git\n")),
            ("rev-parse --abbrev-ref", Ok("origin/main\n")),
            ("rev-list --left-right", Ok("1\t3\n")),
            ("log --format=%h %s", Ok("abc1234 rewritten\n")),
        ]
    };
    let (mut app, _) = app_with_git(script());
    app.push_preview(false);
    assert_eq!(
        app.input_mode,
        InputMode::Normal,
        "a behind push must not prompt"
    );
    assert!(app.pending_git.is_none());

    let (mut app, _) = app_with_git(script());
    app.push_preview(true);
    let pending = app.pending_git.as_ref().expect("force push pending");
    assert!(pending.prompt.starts_with("Force-push (with lease)"));
    assert_eq!(pending.then, GitThen::Remote(RemoteOp::ForcePush));
    assert!(
        pending
            .detail
            .last()
            .unwrap()
            .contains("Overwrites 3 commits")
    );
}

#[test]
fn deleting_an_unmerged_branch_asks_before_forcing() {
    let (mut app, calls) = app_with_git(vec![(
        "branch -d topic",
        Err("error: the branch 'topic' is not fully merged"),
    )]);
    app.delete_branch("topic");
    let pending = app.pending_git.as_ref().expect("force-delete prompt");
    assert!(pending.prompt.contains("not fully merged"));
    assert_eq!(
        pending.then,
        GitThen::Local(vec!["branch".into(), "-D".into(), "topic".into()])
    );
    assert_eq!(calls.lock().unwrap().as_slice(), ["branch -d topic"]);
}

#[test]
fn the_current_branch_is_never_deleted() {
    let (mut app, calls) = app_with_git(vec![]);
    app.delete_branch("main");
    assert!(calls.lock().unwrap().is_empty());
    assert!(app.pending_git.is_none());
}

#[test]
fn repo_status_reads_upstream_divergence() {
    let (mut app, _) = app_with_git(vec![
        ("rev-parse --git-dir", Ok(".git\n")),
        ("rev-parse --abbrev-ref", Ok("origin/main\n")),
        ("rev-list --left-right", Ok("4\t1\n")),
    ]);
    app.refresh_repo_status();
    let status = app.repo_status.clone().expect("status");
    assert_eq!(status.upstream.as_deref(), Some("origin/main"));
    assert_eq!((status.ahead, status.behind), (4, 1));
    assert_eq!(status.operation, None);
}

struct RebaseGit {
    info: VcsInfo,
    steps: Arc<Mutex<Option<Vec<crate::vcs::traits::RebaseStep>>>>,
}

impl VcsBackend for RebaseGit {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, _highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        Err(TuicrError::NoChanges)
    }

    fn fetch_context_lines(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
        _start_line: u32,
        _end_line: u32,
    ) -> Result<Vec<DiffLine>> {
        Ok(Vec::new())
    }

    fn file_line_count(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
    ) -> Result<u32> {
        Ok(0)
    }

    fn run_git(&self, args: &[&str], _network: bool) -> Result<String> {
        let line = args.join(" ");
        Ok(match line.as_str() {
            l if l.starts_with("rev-parse --verify --quiet") => "base000\n".to_string(),
            l if l.starts_with("rev-parse --verify") => "aaa1111\n".to_string(),
            l if l.starts_with("merge-base") => String::new(),
            l if l.starts_with("rev-list --merges") => String::new(),
            l if l.starts_with("log --reverse") => {
                "aaa1111\0aaa1111\0one\nbbb2222\0bbb2222\0two\nccc3333\0ccc3333\0three\n"
                    .to_string()
            }
            other => return Err(TuicrError::VcsCommand(format!("unscripted: {other}"))),
        })
    }

    fn commit_message(&self, _id: &str) -> Result<String> {
        Ok("two\n\nthe body\n".to_string())
    }

    fn rebase_interactive(
        &self,
        base: Option<&str>,
        steps: &[crate::vcs::traits::RebaseStep],
    ) -> Result<()> {
        assert_eq!(base, Some("base000"));
        *self.steps.lock().unwrap() = Some(steps.to_vec());
        Err(TuicrError::VcsCommand("stop here".into()))
    }
}

fn rebase_app() -> (App, Arc<Mutex<Option<Vec<crate::vcs::traits::RebaseStep>>>>) {
    let mut app = build_review_app(&["a"]);
    let steps = Arc::new(Mutex::new(None));
    let info = app.vcs_info.clone();
    app.vcs = Box::new(RebaseGit {
        info,
        steps: steps.clone(),
    });
    (app, steps)
}

#[test]
fn rebase_editor_lists_commits_oldest_first_with_cursor_on_newest() {
    let (mut app, _) = rebase_app();
    app.open_rebase_editor("aaa1111");
    assert_eq!(app.input_mode, InputMode::RebaseEditor);
    let editor = app.rebase_editor.as_ref().unwrap();
    let subjects: Vec<&str> = editor.rows.iter().map(|r| r.subject.as_str()).collect();
    assert_eq!(subjects, vec!["one", "two", "three"]);
    assert_eq!(editor.cursor, 2);
    assert_eq!(editor.summary(&app.rebase_original), None);
}

#[test]
fn rebase_editor_builds_the_todo_it_shows() {
    use crate::vcs::traits::RebaseAction;
    let (mut app, steps) = rebase_app();
    app.open_rebase_editor("aaa1111");
    // move "three" above "two", fold it into "one"... then reword "two"
    app.rebase_editor_shift(-1);
    app.rebase_editor_set(RebaseAction::Fixup);
    app.rebase_editor_move(1);
    app.rebase_editor_set(RebaseAction::Reword);
    app.rebase_editor_clear_subject();
    for c in "two, better".chars() {
        app.rebase_editor_type(c);
    }
    app.rebase_editor_finish_reword();
    let summary = app
        .rebase_editor
        .as_ref()
        .unwrap()
        .summary(&app.rebase_original);
    assert_eq!(
        summary.as_deref(),
        Some("reordered, 1 reworded, 1 fixed up")
    );

    app.rebase_editor_run();
    let steps = steps.lock().unwrap().clone().expect("rebase ran");
    let plan: Vec<(RebaseAction, &str)> = steps.iter().map(|s| (s.action, s.id.as_str())).collect();
    assert_eq!(
        plan,
        vec![
            (RebaseAction::Pick, "aaa1111"),
            (RebaseAction::Fixup, "ccc3333"),
            (RebaseAction::Reword, "bbb2222"),
        ]
    );
    assert_eq!(
        steps[2].message.as_deref(),
        Some("two, better\n\nthe body\n"),
        "a reword keeps the original body"
    );
}

#[test]
fn rebase_editor_refuses_a_leading_fold_and_resets() {
    use crate::vcs::traits::RebaseAction;
    let (mut app, steps) = rebase_app();
    app.open_rebase_editor("aaa1111");
    app.rebase_editor_move(-2);
    app.rebase_editor_set(RebaseAction::Squash);
    assert!(app.rebase_editor.as_ref().unwrap().problem().is_some());
    app.rebase_editor_run();
    assert!(steps.lock().unwrap().is_none(), "must not run");
    assert_eq!(app.input_mode, InputMode::RebaseEditor);

    app.rebase_editor_shift(1);
    app.rebase_editor_reset();
    let editor = app.rebase_editor.as_ref().unwrap();
    assert!(editor.rows.iter().all(|r| r.action == RebaseAction::Pick));
    assert_eq!(editor.summary(&app.rebase_original), None);
}
