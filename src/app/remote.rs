//! Remote operations: `:fetch`, `:pull` / `:pull!`, `:push`. They talk to
//! the network, so each runs on a worker thread with its own VCS handle
//! while the status bar shows what is in flight; the main loop installs
//! the result, reopens the VCS so branch state is current, and refreshes
//! whatever is on screen.

use std::sync::mpsc::Receiver;

use super::{App, InputMode};
use crate::error::Result;
use crate::vcs::detect_vcs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteOp {
    Fetch,
    /// `git pull --ff-only`.
    Pull,
    /// `git pull --rebase`.
    PullRebase,
    Push,
    /// `git push --force-with-lease`.
    ForcePush,
}

impl RemoteOp {
    fn verb(self) -> &'static str {
        match self {
            Self::Fetch => "Fetching",
            Self::Pull | Self::PullRebase => "Pulling",
            Self::Push => "Pushing",
            Self::ForcePush => "Force-pushing",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Pull => "pull",
            Self::PullRebase => "pull --rebase",
            Self::Push => "push",
            Self::ForcePush => "push --force-with-lease",
        }
    }
}

/// What the worker sends back: the operation and its summary or error.
pub struct RemoteOpAnswer {
    pub op: RemoteOp,
    pub result: Result<String>,
}

pub struct RemoteOpInFlight {
    pub op: RemoteOp,
    pub rx: Receiver<RemoteOpAnswer>,
}

impl App {
    /// Whether a remote operation is running (the status bar shows it).
    pub fn remote_op_running(&self) -> Option<RemoteOp> {
        self.remote_op.as_ref().map(|r| r.op)
    }

    /// Start `op` on a worker thread. One at a time: a second request while
    /// one is running is refused with a message.
    pub fn start_remote_op(&mut self, op: RemoteOp) {
        if let Some(running) = self.remote_op_running() {
            self.set_warning(format!(
                "{} already \u{2014} wait for it to finish",
                running.verb()
            ));
            return;
        }
        let options = self.vcs_open_options;
        let (tx, rx) = std::sync::mpsc::channel();
        self.remote_op = Some(RemoteOpInFlight { op, rx });
        self.set_sticky_warning(format!("{}\u{2026}", op.verb()));
        std::thread::spawn(move || {
            let result = detect_vcs(options.git_backend_preference, options.diff_whitespace_mode)
                .and_then(|vcs| match op {
                    RemoteOp::Fetch => vcs.fetch(),
                    RemoteOp::Pull => vcs.pull(false),
                    RemoteOp::PullRebase => vcs.pull(true),
                    RemoteOp::Push => vcs.push(),
                    RemoteOp::ForcePush => vcs
                        .run_git(
                            &["push", "--force-with-lease", "--recurse-submodules=no"],
                            true,
                        )
                        .map(|_| {
                            let branch = vcs.info().branch_name.clone().unwrap_or_default();
                            format!("Force-pushed {branch} (with lease)")
                        }),
                });
            let _ = tx.send(RemoteOpAnswer { op, result });
        });
    }

    /// Main-loop tick: install a finished remote operation. Returns whether
    /// the screen changed.
    pub fn poll_remote_op(&mut self) -> bool {
        let answer = match self.remote_op.as_ref() {
            Some(running) => running.rx.try_recv().ok(),
            None => None,
        };
        let Some(answer) = answer else {
            return false;
        };
        self.remote_op = None;
        self.message = None;
        match answer.result {
            Ok(summary) => {
                // Pull and push move HEAD or the upstream; a fetch changes the
                // remote-tracking refs the log chips and picker show. Reopen
                // the VCS either way so nothing on screen is stale.
                self.after_head_moved();
                if self.input_mode == InputMode::BranchPicker {
                    self.reopen_branch_picker_in_place();
                }
                self.set_message(summary);
            }
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("{} failed: {}", answer.op.name(), err.trim()));
            }
        }
        true
    }

    /// Refresh the branch picker's rows after a fetch without losing the
    /// filter or cursor.
    fn reopen_branch_picker_in_place(&mut self) {
        let Ok(branches) = self.vcs.list_branches() else {
            return;
        };
        if let Some(picker) = self.branch_picker.as_mut() {
            picker.branches = branches;
            picker.picked.clear();
            let n = picker.visible().len();
            picker.cursor = picker.cursor.min(n.saturating_sub(1));
        }
    }
}
