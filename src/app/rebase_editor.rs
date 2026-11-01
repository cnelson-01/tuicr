//! Interactive rebase editor (an IDE's "Interactively Rebase from Here"):
//! the commits from the log cursor up to HEAD, oldest first, each with an
//! action. Reorder with `J` / `K`, set `p`ick / `r`eword / `s`quash /
//! `f`ixup / `d`rop, and Enter runs the rebase.

use super::{App, InputMode};
use crate::vcs::traits::{RebaseAction, RebaseStep};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseRow {
    pub id: String,
    pub short_id: String,
    pub subject: String,
    pub action: RebaseAction,
    /// Edited message for a reword (subject line, then the original body).
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RebaseEditor {
    /// Exclusive base the rows are replayed onto; `None` for the root.
    pub base: Option<String>,
    pub base_label: String,
    /// Oldest first, the order git applies them.
    pub rows: Vec<RebaseRow>,
    pub cursor: usize,
    /// Subject being typed for a reword, while `r` is open.
    pub editing: Option<String>,
    pub return_mode: InputMode,
}

impl RebaseEditor {
    /// What the run would change, for the footer: `None` when every row is
    /// still a pick in its original order.
    pub fn summary(&self, original: &[String]) -> Option<String> {
        let reordered = self.rows.iter().map(|r| &r.id).ne(original.iter());
        let count = |a: RebaseAction| self.rows.iter().filter(|r| r.action == a).count();
        let mut parts = Vec::new();
        if reordered {
            parts.push("reordered".to_string());
        }
        for (action, label) in [
            (RebaseAction::Reword, "reworded"),
            (RebaseAction::Squash, "squashed"),
            (RebaseAction::Fixup, "fixed up"),
            (RebaseAction::Drop, "dropped"),
        ] {
            let n = count(action);
            if n > 0 {
                parts.push(format!("{n} {label}"));
            }
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// Why the plan can't run, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        let first_kept = self.rows.iter().find(|r| r.action != RebaseAction::Drop)?;
        if first_kept.action.melds() {
            return Some("the first kept commit has nothing above it to squash into");
        }
        None
    }
}

impl App {
    /// `i` in the log pane / `:rebase -i [rev]`: open the editor on the
    /// commits from `from` (inclusive) to HEAD.
    pub fn open_rebase_editor(&mut self, from: &str) {
        let git = |args: &[&str]| self.vcs.run_git(args, false);
        let full = match git(&["rev-parse", "--verify", &format!("{from}^{{commit}}")]) {
            Ok(id) => id.trim().to_string(),
            Err(e) => {
                self.set_error(format!("Unknown commit {from}: {}", e.to_string().trim()));
                return;
            }
        };
        if git(&["merge-base", "--is-ancestor", &full, "HEAD"]).is_err() {
            self.set_warning("That commit is not in the current branch's history");
            return;
        }
        let base = git(&["rev-parse", "--verify", "--quiet", &format!("{full}^")])
            .ok()
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty());
        let range = match &base {
            Some(b) => format!("{b}..HEAD"),
            None => "HEAD".to_string(),
        };
        if git(&["rev-list", "--merges", &range]).is_ok_and(|m| !m.trim().is_empty()) {
            self.set_warning("The commits above that one include merges; rebase them by hand");
            return;
        }
        let rows: Vec<RebaseRow> = git(&["log", "--reverse", "--format=%H%x00%h%x00%s", &range])
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\0');
                Some(RebaseRow {
                    id: fields.next()?.to_string(),
                    short_id: fields.next()?.to_string(),
                    subject: fields.next().unwrap_or_default().to_string(),
                    action: RebaseAction::Pick,
                    message: None,
                })
            })
            .collect();
        if rows.is_empty() {
            self.set_message("Nothing to rebase above that commit");
            return;
        }
        let base_label = base
            .as_deref()
            .map(|b| b[..b.len().min(7)].to_string())
            .unwrap_or_else(|| "the root".to_string());
        self.rebase_original = rows.iter().map(|r| r.id.clone()).collect();
        self.rebase_editor = Some(RebaseEditor {
            base,
            base_label,
            cursor: rows.len() - 1,
            rows,
            editing: None,
            return_mode: self.input_mode,
        });
        self.input_mode = InputMode::RebaseEditor;
    }

    /// `i` in the log pane: rebase from the cursor commit.
    pub fn open_rebase_editor_at_cursor(&mut self) {
        let Some(id) = self.with_log_state(|app| app.cursor_commit_id()) else {
            self.set_warning("Move the log cursor onto the oldest commit to rebase");
            return;
        };
        self.open_rebase_editor(&id);
    }

    pub fn close_rebase_editor(&mut self) {
        if let Some(editor) = self.rebase_editor.take() {
            self.input_mode = editor.return_mode;
        }
    }

    pub fn rebase_editor_move(&mut self, delta: isize) {
        if let Some(editor) = self.rebase_editor.as_mut() {
            let n = editor.rows.len() as isize;
            editor.cursor = (editor.cursor as isize + delta).clamp(0, n - 1) as usize;
        }
    }

    /// `J` / `K`: move the cursor row later / earlier in the todo.
    pub fn rebase_editor_shift(&mut self, delta: isize) {
        if let Some(editor) = self.rebase_editor.as_mut() {
            let from = editor.cursor;
            let to = from as isize + delta;
            if to < 0 || to >= editor.rows.len() as isize {
                return;
            }
            editor.rows.swap(from, to as usize);
            editor.cursor = to as usize;
        }
    }

    pub fn rebase_editor_set(&mut self, action: RebaseAction) {
        if action == RebaseAction::Reword {
            self.rebase_editor_begin_reword();
            return;
        }
        if let Some(editor) = self.rebase_editor.as_mut()
            && let Some(row) = editor.rows.get_mut(editor.cursor)
        {
            row.action = action;
            row.message = None;
        }
    }

    fn rebase_editor_begin_reword(&mut self) {
        if let Some(editor) = self.rebase_editor.as_mut()
            && let Some(row) = editor.rows.get(editor.cursor)
        {
            let current = row
                .message
                .as_deref()
                .and_then(|m| m.lines().next())
                .unwrap_or(&row.subject)
                .to_string();
            editor.editing = Some(current);
        }
    }

    pub fn rebase_editor_type(&mut self, c: char) {
        if let Some(buf) = self.rebase_editor.as_mut().and_then(|e| e.editing.as_mut()) {
            buf.push(c);
        }
    }

    pub fn rebase_editor_backspace(&mut self) {
        if let Some(buf) = self.rebase_editor.as_mut().and_then(|e| e.editing.as_mut()) {
            buf.pop();
        }
    }

    pub fn rebase_editor_clear_subject(&mut self) {
        if let Some(buf) = self.rebase_editor.as_mut().and_then(|e| e.editing.as_mut()) {
            buf.clear();
        }
    }

    /// Enter while rewording: keep the new subject, and the original body.
    pub fn rebase_editor_finish_reword(&mut self) {
        let Some(editor) = self.rebase_editor.as_ref() else {
            return;
        };
        let Some(subject) = editor.editing.clone() else {
            return;
        };
        let Some(id) = editor.rows.get(editor.cursor).map(|r| r.id.clone()) else {
            return;
        };
        let subject = subject.trim().to_string();
        if subject.is_empty() {
            self.set_warning("A commit message can't be empty");
            return;
        }
        let body = self
            .vcs
            .commit_message(&id)
            .ok()
            .and_then(|m| {
                m.split_once("\n\n")
                    .map(|(_, body)| body.trim_end().to_string())
            })
            .filter(|b| !b.is_empty());
        let message = match body {
            Some(body) => format!("{subject}\n\n{body}\n"),
            None => format!("{subject}\n"),
        };
        if let Some(editor) = self.rebase_editor.as_mut() {
            editor.editing = None;
            if let Some(row) = editor.rows.get_mut(editor.cursor) {
                row.action = RebaseAction::Reword;
                row.message = Some(message);
            }
        }
    }

    pub fn rebase_editor_cancel_reword(&mut self) {
        if let Some(editor) = self.rebase_editor.as_mut() {
            editor.editing = None;
        }
    }

    /// `u`: put the plan back to all picks in the original order.
    pub fn rebase_editor_reset(&mut self) {
        let original = self.rebase_original.clone();
        if let Some(editor) = self.rebase_editor.as_mut() {
            editor
                .rows
                .sort_by_key(|r| original.iter().position(|id| *id == r.id));
            for row in &mut editor.rows {
                row.action = RebaseAction::Pick;
                row.message = None;
            }
        }
    }

    /// Enter: run the plan.
    pub fn rebase_editor_run(&mut self) {
        let Some(editor) = self.rebase_editor.as_ref() else {
            return;
        };
        if let Some(problem) = editor.problem() {
            self.set_warning(format!("Can't rebase: {problem}"));
            return;
        }
        let Some(summary) = editor.summary(&self.rebase_original) else {
            self.set_message("Nothing changed: every commit is still a pick in order");
            return;
        };
        let steps: Vec<RebaseStep> = editor
            .rows
            .iter()
            .map(|r| RebaseStep {
                action: r.action,
                id: r.id.clone(),
                message: r.message.clone(),
            })
            .collect();
        let base = editor.base.clone();
        let label = editor.base_label.clone();
        self.close_rebase_editor();
        match self.vcs.rebase_interactive(base.as_deref(), &steps) {
            Ok(()) => {
                self.after_head_moved();
                self.set_message(format!("Rebased onto {label}: {summary}"));
            }
            Err(err) => self.handle_history_op_failure("rebase", err),
        }
    }
}
