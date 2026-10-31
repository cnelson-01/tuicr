//! Branch, tag, and in-progress-operation commands from the JetBrains
//! branch popup and VCS menu: merge into current, rebase current onto,
//! continue / skip, delete / rename / track a branch, delete a tag, and a
//! push preview that lists the outgoing commits (with force-with-lease).
//! Local commands run synchronously through [`VcsBackend::run_git`]; pushes
//! go through the remote worker so the UI stays live.
//!
//! [`VcsBackend::run_git`]: crate::vcs::traits::VcsBackend::run_git

use super::{App, ConfirmAction, InputMode, RemoteOp};

/// Header facts about the checked-out branch: its upstream, how far it has
/// diverged, and any merge / rebase / cherry-pick / revert in progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoStatus {
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    /// `merge`, `rebase`, `cherry-pick`, or `revert`.
    pub operation: Option<&'static str>,
}

/// What a yes on the git confirmation prompt does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitThen {
    /// Run these git args locally.
    Local(Vec<String>),
    /// Start this network operation on the worker.
    Remote(RemoteOp),
}

/// A git command waiting on a yes/no, with what to show while it waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingGit {
    pub prompt: String,
    /// Extra lines under the prompt (the commits a push would send).
    pub detail: Vec<String>,
    pub then: GitThen,
    /// Message after a successful local run.
    pub done: String,
}

impl App {
    /// Re-read [`RepoStatus`] for the header. Cheap: two `rev-parse`s, one
    /// `rev-list`, and a few file checks.
    pub fn refresh_repo_status(&mut self) {
        self.repo_status = self.read_repo_status();
    }

    fn read_repo_status(&self) -> Option<RepoStatus> {
        let git = |args: &[&str]| self.vcs.run_git(args, false).ok();
        let git_dir = git(&["rev-parse", "--git-dir"])?;
        let git_dir = self.vcs_info.root_path.join(git_dir.trim());
        let exists = |name: &str| git_dir.join(name).exists();
        let operation = if exists("MERGE_HEAD") {
            Some("merge")
        } else if exists("rebase-merge") || exists("rebase-apply") {
            Some("rebase")
        } else if exists("CHERRY_PICK_HEAD") {
            Some("cherry-pick")
        } else if exists("REVERT_HEAD") {
            Some("revert")
        } else {
            None
        };
        let upstream = git(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());
        let (ahead, behind) = upstream
            .as_ref()
            .and_then(|_| git(&["rev-list", "--left-right", "--count", "HEAD...@{u}"]))
            .and_then(|out| {
                let mut nums = out.split_whitespace().map(|n| n.parse::<usize>().ok());
                Some((nums.next()??, nums.next()??))
            })
            .unwrap_or((0, 0));
        Some(RepoStatus {
            upstream,
            ahead,
            behind,
            operation,
        })
    }

    /// Run a local git command; on success refresh everything HEAD-dependent
    /// and say `done`. A failure that leaves an operation in progress
    /// (conflicts) points at the commands that finish or abort it.
    pub fn run_git_op(&mut self, args: &[&str], done: &str) -> bool {
        match self.vcs.run_git(args, false) {
            Ok(out) => {
                self.after_head_moved();
                // git says when there was nothing to do; pass that on
                // rather than claiming the operation happened.
                let noop = out
                    .lines()
                    .find(|l| l.contains("Already up to date") || l.contains("is up to date"));
                match noop {
                    Some(line) => self.set_message(line.trim().to_string()),
                    None => self.set_message(done.to_string()),
                }
                true
            }
            Err(e) => {
                self.after_head_moved();
                let err = e.to_string();
                match self.repo_status.as_ref().and_then(|s| s.operation) {
                    Some(op) => self.set_warning(format!(
                        "{op} stopped on conflicts \u{00b7} :merge resolves, :continue finishes, :abort undoes"
                    )),
                    None => self.set_error(format!("git {} failed: {}", args[0], err.trim())),
                }
                false
            }
        }
    }

    fn current_branch(&self) -> Option<String> {
        self.vcs_info.branch_name.clone()
    }

    /// `:merge <branch>` / `Ctrl-g` in the branch picker.
    pub fn merge_into_current(&mut self, branch: &str) {
        let Some(current) = self.current_branch() else {
            self.set_warning("HEAD is detached; check out a branch to merge into");
            return;
        };
        if branch == current {
            self.set_warning(format!("{branch} is the current branch"));
            return;
        }
        self.run_git_op(
            &["merge", "--no-edit", branch],
            &format!("Merged {branch} into {current}"),
        );
    }

    /// `:rebase <branch>` / `Ctrl-b` in the branch picker.
    pub fn rebase_current_onto(&mut self, onto: &str) {
        let Some(current) = self.current_branch() else {
            self.set_warning("HEAD is detached; check out a branch to rebase");
            return;
        };
        self.run_git_op(
            &["-c", "core.editor=true", "rebase", onto],
            &format!("Rebased {current} onto {onto}"),
        );
    }

    /// `:continue` — finish the merge / rebase / cherry-pick / revert in
    /// progress once its conflicts are resolved and staged.
    pub fn continue_operation(&mut self) {
        match self.vcs.continue_merge_operation() {
            Ok(op) => {
                self.after_head_moved();
                self.set_message(format!("{op} continued"));
            }
            Err(e) => {
                self.after_head_moved();
                let err = e.to_string();
                let hint = if self
                    .repo_status
                    .as_ref()
                    .and_then(|s| s.operation)
                    .is_some()
                {
                    " \u{00b7} resolve and stage every conflict first (:merge)"
                } else {
                    ""
                };
                self.set_error(format!("Continue failed: {}{hint}", err.trim()));
            }
        }
    }

    /// `:skip` — drop the commit a rebase / cherry-pick / revert stopped on.
    pub fn skip_operation(&mut self) {
        self.refresh_repo_status();
        let op = self.repo_status.as_ref().and_then(|s| s.operation);
        match op {
            Some(op @ ("rebase" | "cherry-pick" | "revert")) => {
                self.run_git_op(
                    &["-c", "core.editor=true", op, "--skip"],
                    &format!("Skipped the commit; {op} continues"),
                );
            }
            Some("merge") => self.set_warning("A merge can't skip; :abort it instead"),
            _ => self.set_warning("No rebase, cherry-pick, or revert in progress"),
        }
    }

    /// `:branch -d <name>` / `Ctrl-x` in the branch picker. An unmerged
    /// branch asks before it is force-deleted.
    pub fn delete_branch(&mut self, name: &str) {
        if self.current_branch().as_deref() == Some(name) {
            self.set_warning(format!("{name} is checked out; switch away to delete it"));
            return;
        }
        match self.vcs.run_git(&["branch", "-d", name], false) {
            Ok(_) => {
                self.after_head_moved();
                self.reopen_branch_picker_if_open();
                self.set_message(format!("Deleted branch {name}"));
            }
            Err(e) if e.to_string().contains("not fully merged") => {
                self.confirm_git(PendingGit {
                    prompt: format!("{name} is not fully merged. Delete it anyway?"),
                    detail: vec!["Its commits stay reachable only from the reflog.".to_string()],
                    then: GitThen::Local(vec!["branch".into(), "-D".into(), name.into()]),
                    done: format!("Deleted branch {name}"),
                });
            }
            Err(e) => self.set_error(format!("Delete failed: {}", e.to_string().trim())),
        }
    }

    /// `:branch -m [old] <new>` / `Ctrl-r` in the branch picker.
    pub fn rename_branch(&mut self, old: Option<&str>, new: &str) {
        let Some(old) = old.map(str::to_string).or_else(|| self.current_branch()) else {
            self.set_warning("HEAD is detached; name the branch to rename");
            return;
        };
        if self.run_git_op(
            &["branch", "-m", &old, new],
            &format!("Renamed {old} to {new}"),
        ) {
            self.reopen_branch_picker_if_open();
        }
    }

    /// `:upstream <remote/branch>` — track a remote branch.
    pub fn set_upstream(&mut self, upstream: &str) {
        let Some(current) = self.current_branch() else {
            self.set_warning("HEAD is detached; check out a branch first");
            return;
        };
        let flag = format!("--set-upstream-to={upstream}");
        self.run_git_op(
            &["branch", &flag],
            &format!("{current} now tracks {upstream}"),
        );
    }

    /// `:tag -d <name>` — delete a local tag, after asking.
    pub fn delete_tag(&mut self, name: &str) {
        self.confirm_git(PendingGit {
            prompt: format!("Delete tag {name}?"),
            detail: vec!["Only the local tag; a pushed tag stays on the remote.".to_string()],
            then: GitThen::Local(vec!["tag".into(), "-d".into(), name.into()]),
            done: format!("Deleted tag {name}"),
        });
    }

    /// `:push` / `:push!` — list what would be pushed and where, then push
    /// on yes. `force` uses `--force-with-lease`, which refuses when the
    /// remote moved since the last fetch.
    pub fn push_preview(&mut self, force: bool) {
        let Some(branch) = self.current_branch() else {
            self.set_warning("HEAD is detached; check out a branch to push");
            return;
        };
        self.refresh_repo_status();
        let status = self.repo_status.clone().unwrap_or_default();
        let (range, target) = match status.upstream.as_deref() {
            Some(up) => (format!("{up}..HEAD"), up.to_string()),
            None => (
                String::new(),
                format!("origin/{branch} (new, sets upstream)"),
            ),
        };
        let log_args: Vec<&str> = if range.is_empty() {
            vec![
                "log",
                "--format=%h %s",
                "-n",
                "50",
                "HEAD",
                "--not",
                "--remotes",
            ]
        } else {
            vec!["log", "--format=%h %s", "-n", "50", &range]
        };
        let commits: Vec<String> = self
            .vcs
            .run_git(&log_args, false)
            .map(|out| out.lines().map(str::to_string).collect())
            .unwrap_or_default();
        if commits.is_empty() && !force {
            self.set_message(format!(
                "Nothing to push: {branch} is up to date with {target}"
            ));
            return;
        }
        if status.behind > 0 && !force {
            self.set_warning(format!(
                "{branch} is {} behind {target}: :pull first, or :push! to force (with lease)",
                status.behind
            ));
            return;
        }
        let mut detail: Vec<String> = commits.iter().map(|c| format!("  {c}")).collect();
        if force && status.behind > 0 {
            detail.push(format!(
                "Overwrites {} commit{} on {target}",
                status.behind,
                if status.behind == 1 { "" } else { "s" }
            ));
        }
        let n = commits.len();
        let verb = if force {
            "Force-push (with lease)"
        } else {
            "Push"
        };
        self.confirm_git(PendingGit {
            prompt: format!(
                "{verb} {n} commit{} from {branch} to {target}?",
                if n == 1 { "" } else { "s" }
            ),
            detail,
            then: GitThen::Remote(if force {
                RemoteOp::ForcePush
            } else {
                RemoteOp::Push
            }),
            done: String::new(),
        });
    }

    fn confirm_git(&mut self, pending: PendingGit) {
        self.pending_git = Some(pending);
        self.git_confirm_return = self.input_mode;
        self.pending_confirm = Some(ConfirmAction::GitCommand);
        self.input_mode = InputMode::Confirm;
    }

    /// Yes on the git prompt.
    pub fn confirm_git_yes(&mut self) {
        self.pending_confirm = None;
        self.input_mode = self.git_confirm_return;
        let Some(pending) = self.pending_git.take() else {
            return;
        };
        match pending.then {
            GitThen::Local(args) => {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                if self.run_git_op(&args, &pending.done) {
                    self.reopen_branch_picker_if_open();
                }
            }
            GitThen::Remote(op) => self.start_remote_op(op),
        }
    }

    /// No on the git prompt.
    pub fn confirm_git_no(&mut self) {
        self.pending_confirm = None;
        self.pending_git = None;
        self.input_mode = self.git_confirm_return;
        self.set_message("Cancelled");
    }

    fn picker_branch(&self) -> Option<crate::vcs::traits::BranchInfo> {
        self.branch_picker
            .as_ref()
            .and_then(|p| p.selected().cloned())
    }

    /// `Ctrl-x` in the branch picker.
    pub fn branch_picker_delete(&mut self) {
        let Some(branch) = self.picker_branch() else {
            return;
        };
        if branch.is_remote {
            self.set_warning(format!(
                "{} is a remote-tracking branch; delete it on the remote",
                branch.name
            ));
            return;
        }
        self.delete_branch(&branch.name);
    }

    /// `Ctrl-r` in the branch picker: close it and pre-fill the rename.
    pub fn branch_picker_rename(&mut self) {
        let Some(branch) = self.picker_branch() else {
            return;
        };
        if branch.is_remote {
            self.set_warning("Rename a local branch");
            return;
        }
        self.close_branch_picker();
        let text = format!(":branch -m {} ", branch.name);
        self.injected_keys.extend(text.chars().map(|c| {
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            )
        }));
    }

    /// `Ctrl-g` in the branch picker.
    pub fn branch_picker_merge(&mut self) {
        if let Some(branch) = self.picker_branch() {
            self.merge_into_current(&branch.name);
            self.reopen_branch_picker_if_open();
        }
    }

    /// `Ctrl-b` in the branch picker.
    pub fn branch_picker_rebase(&mut self) {
        if let Some(branch) = self.picker_branch() {
            self.rebase_current_onto(&branch.name);
            self.reopen_branch_picker_if_open();
        }
    }

    /// `Ctrl-t` in the branch picker: track the highlighted remote branch.
    pub fn branch_picker_track(&mut self) {
        let Some(branch) = self.picker_branch() else {
            return;
        };
        if !branch.is_remote {
            self.set_warning("Pick a remote-tracking branch (origin/...) to track");
            return;
        }
        self.set_upstream(&branch.name);
        self.reopen_branch_picker_if_open();
    }

    fn reopen_branch_picker_if_open(&mut self) {
        if self.input_mode != InputMode::BranchPicker {
            return;
        }
        if let Ok(branches) = self.vcs.list_branches()
            && let Some(picker) = self.branch_picker.as_mut()
        {
            picker.branches = branches;
            picker.picked.clear();
            let n = picker.visible().len();
            picker.cursor = picker.cursor.min(n.saturating_sub(1));
        }
    }
}
