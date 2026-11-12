//! Commit context actions on the log pane and the fullscreen selector:
//! revert, reset and undo-last-commit, reword, drop, fixup into the parent,
//! tag, and copy hash. History rewrites run through git's own interactive
//! rebase with a prepared todo list, so a conflict stop lands in the merge
//! view exactly like a cherry-pick's would and `a` / `:abort` backs out.
//! They start with `--autostash`, so uncommitted changes do not block them,
//! and `:undo` puts the branch back (see `branch_moves.rs`, which also has
//! the reset chooser).

use super::branch_moves::short;
use super::*;
use crate::output::copy_text_to_clipboard;
use crate::vcs::ResetMode;

/// A history rewrite waiting on the y/n prompt, with the mode to return to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryAction {
    Drop { id: String, summary: String },
}

impl HistoryAction {
    /// The confirmation question.
    pub fn prompt(&self) -> String {
        match self {
            Self::Drop { id, summary } => format!(
                "Drop commit {} \"{summary}\"? Later commits are replayed without it.",
                short(id)
            ),
        }
    }
}

impl App {
    /// The commit an action applies to: the row under the cursor in the
    /// fullscreen selector, or in the log pane. Pseudo rows and the overflow
    /// row do not count.
    pub(in crate::app) fn action_commit(&mut self) -> Option<(String, String)> {
        let pick = |app: &App| {
            app.commit_list
                .get(app.commit_list_cursor)
                .filter(|c| !Self::is_special_commit(c))
                .map(|c| (c.id.clone(), c.summary.clone()))
        };
        if self.input_mode == InputMode::CommitSelect
            || (self.input_mode == InputMode::Command
                && self.command_return_mode == InputMode::CommitSelect)
        {
            pick(self)
        } else if self.log_pane.is_some() {
            self.with_log_state(|app| pick(app))
        } else {
            None
        }
    }

    fn require_commit(&mut self, what: &str) -> Option<(String, String)> {
        let found = self.action_commit();
        if found.is_none() {
            self.set_warning(format!(
                "Move the log cursor onto a commit to {what} (<leader>g opens the log)"
            ));
        }
        found
    }

    /// The mode a prompt or composer should hand back to.
    pub(in crate::app) fn history_return_mode(&self) -> InputMode {
        match self.input_mode {
            InputMode::CommitSelect => InputMode::CommitSelect,
            InputMode::Command => self.command_return_mode,
            _ => InputMode::Normal,
        }
    }

    /// Whether the cursor commit is HEAD on the current branch.
    fn is_head_commit(&self, id: &str) -> bool {
        self.vcs_info.head_commit == id
    }

    /// `v` / `:revert`: a new commit undoing the cursor commit.
    pub fn revert_cursor_commit(&mut self) {
        let Some((id, summary)) = self.require_commit("revert") else {
            return;
        };
        match self.vcs.revert_commit(&id) {
            Ok(()) => {
                self.after_head_moved();
                self.set_message(format!("Reverted {} \"{summary}\"", short(&id)));
            }
            Err(err) => self.handle_history_op_failure("revert", err),
        }
    }

    /// `R` / `:reword`: the comment composer opens with the commit's message;
    /// saving rewrites it (amend for HEAD, a todo-driven rebase otherwise).
    pub fn start_reword_composer(&mut self) {
        let Some((id, _)) = self.require_commit("reword") else {
            return;
        };
        let message = match self.vcs.commit_message(&id) {
            Ok(message) => message,
            Err(e) => {
                self.set_error(format!("Failed to read the commit message: {e}"));
                return;
            }
        };
        let return_mode = self.history_return_mode();
        self.enter_review_comment_mode();
        self.comment_buffer = message.trim_end().to_string();
        self.comment_cursor = self.comment_buffer.len();
        self.commit_composer = Some(CommitComposer {
            amend: false,
            reword: Some(id.clone()),
            return_mode,
            repo: std::path::PathBuf::new(),
        });
        let how = if self.is_head_commit(&id) {
            "amends HEAD"
        } else {
            "rebases the later commits"
        };
        self.set_message(format!(
            "Reword {}: edit the message, Enter / Ctrl-s rewrites ({how}), Esc cancels",
            short(&id)
        ));
    }

    pub(in crate::app) fn reword_commit(&mut self, id: &str, message: &str) {
        if message.trim().is_empty() {
            self.set_error("Commit message cannot be empty");
            return;
        }
        self.tracking_branch_move("reword", |app| match app.vcs.reword_commit(id, message) {
            Ok(()) => {
                app.after_head_moved();
                let summary = message.lines().next().unwrap_or_default();
                app.set_message(format!("Reworded {}: {summary}", short(id)));
            }
            Err(err) => app.handle_history_op_failure("reword", err),
        });
    }

    /// `D` / `:drop`: ask, then replay history without the cursor commit.
    pub fn request_drop_commit(&mut self) {
        let Some((id, summary)) = self.require_commit("drop") else {
            return;
        };
        let return_mode = self.history_return_mode();
        self.pending_history = Some((HistoryAction::Drop { id, summary }, return_mode));
        self.enter_confirm_mode(ConfirmAction::HistoryRewrite);
    }

    /// `f` / `:fixup`: squash the cursor commit into its parent, keeping
    /// the parent's message.
    pub fn fixup_cursor_commit(&mut self) {
        let Some((id, summary)) = self.require_commit("fix up") else {
            return;
        };
        self.tracking_branch_move("fixup", |app| match app.vcs.fixup_commit(&id) {
            Ok(()) => {
                app.after_head_moved();
                app.set_message(format!(
                    "Squashed {} \"{summary}\" into its parent",
                    short(&id)
                ));
            }
            Err(err) => app.handle_history_op_failure("fixup", err),
        });
    }

    /// `x`: ask how to reset the branch to the cursor commit.
    pub fn start_reset_prompt(&mut self) {
        let Some((id, _)) = self.require_commit("reset to") else {
            return;
        };
        self.open_reset_chooser(id);
    }

    /// `:reset [keep|hard|mixed|soft] [commit]`. Without a commit it targets
    /// the log cursor; without a mode it asks, saying what each one does.
    pub fn reset_command(&mut self, args: &str) {
        let mut mode: Option<ResetMode> = None;
        let mut target: Option<String> = None;
        for word in args.split_whitespace() {
            match word {
                "keep" | "--keep" => mode = Some(ResetMode::Keep),
                "soft" | "--soft" => mode = Some(ResetMode::Soft),
                "mixed" | "--mixed" => mode = Some(ResetMode::Mixed),
                "hard" | "--hard" => mode = Some(ResetMode::Hard),
                other => target = Some(other.to_string()),
            }
        }
        let id = match target {
            // A branch or tag name is as good as a hash here.
            Some(rev) => match self
                .vcs
                .run_git(
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("{rev}^{{commit}}"),
                    ],
                    false,
                )
                .map(|out| out.trim().to_string())
            {
                Ok(id) if !id.is_empty() => id,
                _ => {
                    self.set_error(format!("{rev} does not name a commit"));
                    return;
                }
            },
            None => match self.action_commit() {
                Some((id, _)) => id,
                None => {
                    self.set_error(
                        "Usage: :reset [keep|hard|mixed|soft] <commit>  (or press x on a log row)",
                    );
                    return;
                }
            },
        };
        match mode {
            Some(mode) => {
                let summary = self
                    .vcs
                    .commit_message(&id)
                    .ok()
                    .and_then(|m| m.lines().next().map(str::to_string))
                    .unwrap_or_default();
                self.reset_to_commit(&id, &summary, mode);
            }
            None => self.open_reset_chooser(id),
        }
    }

    /// `:undo` with no branch move to put back: take the last commit back,
    /// keeping its changes staged.
    pub(in crate::app) fn undo_last_commit(&mut self) {
        match self.vcs.reset_to("HEAD~1", ResetMode::Soft) {
            Ok(()) => {
                self.after_head_moved();
                self.set_message("Undid the last commit; its changes are staged");
            }
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("Undo failed: {}", err.trim()));
            }
        }
    }

    /// `y` on the confirmation.
    pub fn confirm_history_action(&mut self) {
        let Some((action, return_mode)) = self.pending_history.take() else {
            self.exit_confirm_mode();
            return;
        };
        self.pending_confirm = None;
        self.input_mode = return_mode;
        match action {
            HistoryAction::Drop { id, summary } => {
                self.tracking_branch_move("drop", |app| match app.vcs.drop_commit(&id) {
                    Ok(()) => {
                        app.after_head_moved();
                        app.set_message(format!("Dropped {} \"{summary}\"", short(&id)));
                    }
                    Err(err) => app.handle_history_op_failure("drop", err),
                });
            }
        }
    }

    pub fn cancel_history_action(&mut self) {
        if let Some((_, return_mode)) = self.pending_history.take() {
            self.input_mode = return_mode;
        } else {
            self.input_mode = InputMode::Normal;
        }
        self.pending_confirm = None;
    }

    /// `t`: the command line opens with `tag ` for the name; the cursor
    /// commit is remembered as the target.
    pub fn start_tag_prompt(&mut self) {
        let Some((id, _)) = self.require_commit("tag") else {
            return;
        };
        self.pending_branch_start = Some(id);
        self.enter_command_mode();
        self.command_buffer = "tag ".to_string();
    }

    /// `:tag <name> [commit]`: a lightweight tag at the commit `t` was
    /// pressed on, the given commit, or HEAD.
    pub fn create_tag(&mut self, args: &str) {
        let mut words = args.split_whitespace();
        let Some(name) = words.next() else {
            self.set_error("Usage: :tag <name> [commit]");
            return;
        };
        let target = words
            .next()
            .map(str::to_string)
            .or_else(|| self.pending_branch_start.take())
            .unwrap_or_else(|| "HEAD".to_string());
        match self.vcs.create_tag(name, &target) {
            Ok(()) => {
                self.after_head_moved();
                self.set_message(format!("Tagged {} as {name}", short(&target)));
            }
            Err(e) => {
                let err = e.to_string();
                self.set_error(format!("Tag failed: {}", err.trim()));
            }
        }
    }

    /// `y`: the full hash of the cursor commit to the clipboard.
    pub fn copy_cursor_commit_hash(&mut self) {
        let Some((id, _)) = self.require_commit("copy") else {
            return;
        };
        match copy_text_to_clipboard(&id) {
            Ok(_) => self.set_message(format!("Copied {id}")),
            Err(e) => self.set_error(format!("Copy failed: {e}")),
        }
    }
}
