//! Moving HEAD without fighting local changes. Switching branches,
//! `reset --keep` and `:undo` share one rule: uncommitted changes come
//! along, and only when one of them sits in a file the move rewrites does a
//! prompt ask whether to stash it and put it back, or to set it aside.
//! Nothing here throws work away: what a move cannot carry ends up in the
//! stash list.
//!
//! Also here: the reset chooser (`x` / `:reset`), the record of the last
//! branch move that `:undo` puts back, and the check for a rebase autostash
//! that git could not put back.

use super::{App, ConfirmAction, InputMode, MessageType};
use crate::vcs::ResetMode;

/// Above this many files a stash takes the whole tree rather than naming
/// each path on the command line.
const MAX_STASH_PATHS: usize = 400;
/// Files listed in the "in the way" prompt before "and N more".
const MAX_LISTED_FILES: usize = 8;
/// Commit subjects in prompts and messages are cut to this many characters.
const MAX_SUMMARY_CHARS: usize = 60;

/// Where HEAD is: the checked-out branch (`None` when detached) and commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadPosition {
    pub branch: Option<String>,
    pub id: String,
}

/// A branch move `:undo` can put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMove {
    /// Where the move left HEAD. `:undo` only acts while HEAD is still here.
    pub at: HeadPosition,
    /// The commit the branch was on before.
    pub from: String,
    /// What moved it: `reset`, `rebase`, `drop`, ...
    pub what: String,
    /// The kind of reset that takes it back.
    pub back: ResetMode,
    /// Stash commits holding what a hard reset discarded, popped on undo.
    pub saved: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadMoveKind {
    /// `git checkout <target>`.
    Switch,
    /// `git reset --keep <target>`.
    Reset,
}

/// A move of HEAD that uncommitted changes ride along with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadMove {
    pub kind: HeadMoveKind,
    /// A branch name or commit for a switch; a commit id for a reset.
    pub target: String,
    /// How messages name the target.
    pub label: String,
    /// The branch move this one puts back, when it is `:undo`.
    pub undoes: Option<BranchMove>,
}

impl HeadMove {
    fn switch(target: &str) -> Self {
        Self {
            kind: HeadMoveKind::Switch,
            target: target.to_string(),
            label: target.to_string(),
            undoes: None,
        }
    }

    fn reset(id: &str, summary: &str) -> Self {
        Self {
            kind: HeadMoveKind::Reset,
            target: id.to_string(),
            label: commit_label(id, summary),
            undoes: None,
        }
    }

    /// "switching to feature" / "resetting to 3f2a1c9".
    fn doing(&self) -> String {
        match self.kind {
            HeadMoveKind::Switch => format!("switching to {}", self.target),
            HeadMoveKind::Reset => format!("resetting to {}", short(&self.target)),
        }
    }

    fn verb(&self) -> &'static str {
        match (self.kind, self.undoes.is_some()) {
            (HeadMoveKind::Switch, _) => "Checkout",
            (HeadMoveKind::Reset, false) => "Reset",
            (HeadMoveKind::Reset, true) => "Undo",
        }
    }
}

/// One answer on a [`ChoicePrompt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    pub key: char,
    pub label: &'static str,
    pub description: String,
}

/// What the answer to a [`ChoicePrompt`] decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceThen {
    /// How to reset the branch to the commit.
    Reset { id: String, summary: String },
    /// What to do with the changed files in the way of a move.
    InTheWay {
        head_move: HeadMove,
        files: Vec<String>,
    },
}

/// A prompt answered with one key out of several (`ConfirmAction::Choice`).
/// Enter takes the default, Esc cancels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoicePrompt {
    /// The dialog's border title.
    pub heading: &'static str,
    pub title: String,
    /// Facts under the title: what will happen, or the files in the way.
    pub detail: Vec<String>,
    pub options: Vec<ChoiceOption>,
    pub default: char,
    pub then: ChoiceThen,
    pub return_mode: InputMode,
}

/// HEAD and the stash list as they were before an operation that may move
/// the branch, to tell afterwards what it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveWatch {
    before: Option<HeadPosition>,
    stash: Option<String>,
    /// Whether a rebase (or merge, cherry-pick, revert) was already stopped
    /// part-way: the operation being watched is then that one carrying on.
    resuming: bool,
}

/// What became of the files that were in the way of a move.
enum Carried {
    Nothing,
    PutBack(usize),
    SetAside(usize),
    /// Put back with conflict markers; the stash entry is kept.
    Conflicted,
    /// Not put back at all, nothing touched; the stash entry is kept.
    NotPutBack(usize),
}

pub(in crate::app) fn short(id: &str) -> &str {
    &id[..7.min(id.len())]
}

fn commit_label(id: &str, summary: &str) -> String {
    if summary.is_empty() {
        return short(id).to_string();
    }
    let mut cut: String = summary.chars().take(MAX_SUMMARY_CHARS).collect();
    if cut.len() < summary.len() {
        cut.push('\u{2026}');
    }
    format!("{} \"{cut}\"", short(id))
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl App {
    /// Trimmed stdout of a git command, `None` when it fails or prints
    /// nothing.
    fn git_line(&self, args: &[&str]) -> Option<String> {
        self.vcs
            .run_git(args, false)
            .ok()
            .map(|out| out.trim().to_string())
            .filter(|out| !out.is_empty())
    }

    /// The paths a `-z` listing names.
    fn git_paths(&self, args: &[&str]) -> Vec<String> {
        self.vcs
            .run_git(args, false)
            .map(|out| {
                out.split('\0')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where HEAD is right now, read from git rather than the cached
    /// [`VcsInfo`](crate::vcs::VcsInfo): a commit made from a shell moves it
    /// too.
    pub(in crate::app) fn head_position(&self) -> Option<HeadPosition> {
        let id = self.git_line(&["rev-parse", "--verify", "--quiet", "HEAD"])?;
        let branch = self.git_line(&["symbolic-ref", "--quiet", "--short", "HEAD"]);
        Some(HeadPosition { branch, id })
    }

    /// The commit of the newest stash entry.
    fn stash_tip(&self) -> Option<String> {
        self.git_line(&["rev-parse", "--verify", "--quiet", "refs/stash"])
    }

    fn commit_summary(&self, id: &str) -> String {
        self.vcs
            .commit_message(id)
            .ok()
            .and_then(|m| m.lines().next().map(str::to_string))
            .unwrap_or_default()
    }

    fn has_unmerged_entries(&self) -> bool {
        self.vcs
            .run_git(&["ls-files", "--unmerged"], false)
            .is_ok_and(|out| !out.trim().is_empty())
    }

    /// `git stash pop <selector>`, keeping staged changes staged when they
    /// apply. `false` leaves the entry in the stash list: git keeps it when
    /// the changes conflict with what is checked out now.
    fn pop_stash(&self, selector: &str) -> bool {
        if self
            .vcs
            .run_git(&["stash", "pop", "--index", "--quiet", selector], false)
            .is_ok()
        {
            return true;
        }
        // `--index` gives up before touching the tree when only the staged
        // half does not apply. Conflict markers mean it got further and the
        // plain pop would only fail on them.
        if self.has_unmerged_entries() {
            return false;
        }
        self.vcs
            .run_git(&["stash", "pop", "--quiet", selector], false)
            .is_ok()
    }

    /// Pop the stash entry whose commit is `id`, wherever it sits in the
    /// list by now. `None` when it is no longer there.
    fn pop_stash_commit(&self, id: &str) -> Option<bool> {
        let list = self
            .vcs
            .run_git(&["stash", "list", "--format=%H"], false)
            .ok()?;
        let index = list.lines().position(|line| line.trim() == id)?;
        Some(self.pop_stash(&format!("stash@{{{index}}}")))
    }

    /// Untracked files that `rev` has under version control, which a move
    /// there would write over.
    fn untracked_in_the_way(&self, rev: &str) -> Vec<String> {
        let added = self.git_paths(&[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--diff-filter=A",
            "HEAD",
            rev,
            "--",
        ]);
        if added.is_empty() || added.len() > MAX_STASH_PATHS {
            return Vec::new();
        }
        let mut args = vec![
            "--literal-pathspecs",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
        ];
        args.extend(added.iter().map(String::as_str));
        self.git_paths(&args)
    }

    /// The changed files that stop `head_move`. A refused checkout names
    /// them itself, one per tab-indented line (whatever the locale). A
    /// refused `reset --keep` names only the first, so there they are worked
    /// out: files changed here that also differ between HEAD and the
    /// target, plus untracked files the target would write over.
    fn changes_in_the_way(&self, head_move: &HeadMove, error: &str) -> Vec<String> {
        let named: Vec<String> = error
            .lines()
            .filter_map(|line| line.strip_prefix('\t'))
            .map(|path| path.trim_end().to_string())
            .filter(|path| !path.is_empty())
            .collect();
        // A checkout refused for any other reason (an unmerged index, a
        // name that is not a branch) has no files to offer to stash.
        if !named.is_empty() || head_move.kind == HeadMoveKind::Switch {
            return named;
        }
        let rev = head_move.target.as_str();
        let differs: std::collections::HashSet<String> = self
            .git_paths(&[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                "HEAD",
                rev,
                "--",
            ])
            .into_iter()
            .collect();
        if differs.is_empty() {
            return Vec::new();
        }
        let mut files: Vec<String> = self
            .git_paths(&["diff", "--name-only", "-z", "--no-renames", "HEAD", "--"])
            .into_iter()
            .filter(|path| differs.contains(path))
            .collect();
        files.extend(self.untracked_in_the_way(rev));
        files
    }

    /// An undo that was not carried out can still be asked for.
    fn keep_undo(&mut self, head_move: HeadMove) {
        if let Some(undone) = head_move.undoes {
            self.last_branch_move = Some(undone);
        }
    }

    fn run_head_move(&self, head_move: &HeadMove) -> crate::error::Result<()> {
        match head_move.kind {
            HeadMoveKind::Switch => self.vcs.checkout(&head_move.target),
            HeadMoveKind::Reset => self.vcs.reset_to(&head_move.target, ResetMode::Keep),
        }
    }

    /// `c` / `:checkout` / Enter in the branch picker. Uncommitted changes
    /// come along as git carries them; when some are in the way a prompt
    /// asks what to do with those files, unless `stash_without_asking`
    /// (`:checkout!`, Ctrl-s), which stashes them and puts them back.
    pub fn switch_to(&mut self, target: &str, stash_without_asking: bool) {
        self.start_head_move(HeadMove::switch(target), stash_without_asking);
    }

    fn start_head_move(&mut self, head_move: HeadMove, stash_without_asking: bool) {
        let before = self.head_position();
        let error = match self.run_head_move(&head_move) {
            Ok(()) => {
                self.finish_head_move(head_move, before, Carried::Nothing);
                return;
            }
            Err(e) => e.to_string(),
        };
        let files = self.changes_in_the_way(&head_move, &error);
        if files.is_empty() {
            self.set_error(format!("{} failed: {}", head_move.verb(), error.trim()));
            self.keep_undo(head_move);
        } else if stash_without_asking {
            self.carry_head_move(head_move, files, true);
        } else {
            self.ask_about_changes_in_the_way(head_move, files);
        }
    }

    fn ask_about_changes_in_the_way(&mut self, head_move: HeadMove, files: Vec<String>) {
        let n = files.len();
        let mut detail: Vec<String> = files
            .iter()
            .take(MAX_LISTED_FILES)
            .map(|f| format!("  {f}"))
            .collect();
        if n > MAX_LISTED_FILES {
            detail.push(format!("  \u{2026} and {} more", n - MAX_LISTED_FILES));
        }
        let action = match head_move.kind {
            HeadMoveKind::Switch => "switch",
            HeadMoveKind::Reset => "reset",
        };
        let prompt = ChoicePrompt {
            heading: " Changes in the way ",
            title: format!(
                "{} in the way of {}",
                if n == 1 {
                    "1 changed file is".to_string()
                } else {
                    format!("{n} changed files are")
                },
                head_move.doing()
            ),
            detail,
            options: vec![
                ChoiceOption {
                    key: 's',
                    label: "stash",
                    description: format!("set them aside, {action}, put them back"),
                },
                ChoiceOption {
                    key: 'd',
                    label: "discard",
                    description: format!("{action} without them (they stay in the stash list)"),
                },
            ],
            default: 's',
            then: ChoiceThen::InTheWay { head_move, files },
            return_mode: self.history_return_mode(),
        };
        self.open_choice(prompt);
    }

    /// Stash `files`, make the move, and put them back (`put_back`) or
    /// leave them in the stash list.
    fn carry_head_move(&mut self, head_move: HeadMove, files: Vec<String>, put_back: bool) {
        let before = self.head_position();
        let stash_before = self.stash_tip();
        let message = format!(
            "tuicr: {} while {}",
            if put_back { "carried" } else { "set aside" },
            head_move.doing()
        );
        let mut args = vec![
            "--literal-pathspecs",
            "stash",
            "push",
            "--include-untracked",
            "-m",
            message.as_str(),
        ];
        if files.len() <= MAX_STASH_PATHS {
            args.push("--");
            args.extend(files.iter().map(String::as_str));
        }
        if let Err(e) = self.vcs.run_git(&args, false) {
            self.set_error(format!(
                "Nothing moved: stashing the files in the way failed: {}",
                e.to_string().trim()
            ));
            self.keep_undo(head_move);
            return;
        }
        let stashed = self
            .stash_tip()
            .is_some_and(|tip| Some(&tip) != stash_before.as_ref());
        if let Err(e) = self.run_head_move(&head_move) {
            let restored = !stashed || self.pop_stash("stash@{0}");
            self.after_head_moved();
            let tail = if restored {
                ""
            } else {
                " \u{00b7} your changes are in stash@{0}"
            };
            self.set_error(format!(
                "{} failed: {}{tail}",
                head_move.verb(),
                e.to_string().trim()
            ));
            self.keep_undo(head_move);
            return;
        }
        let carried = match (stashed, put_back) {
            (false, _) => Carried::Nothing,
            (true, false) => Carried::SetAside(files.len()),
            (true, true) if self.pop_stash("stash@{0}") => Carried::PutBack(files.len()),
            // Overlapping changes leave conflict markers to resolve. A pop
            // that could not start (an untracked file of ours where the
            // target has a file of its own) leaves nothing to resolve.
            (true, true) if self.has_unmerged_entries() => Carried::Conflicted,
            (true, true) => Carried::NotPutBack(files.len()),
        };
        self.finish_head_move(head_move, before, carried);
    }

    /// HEAD is at the target: put back what an undo saved, refresh, record
    /// a reset for `:undo`, and say what happened.
    fn finish_head_move(
        &mut self,
        head_move: HeadMove,
        before: Option<HeadPosition>,
        carried: Carried,
    ) {
        let mut notes: Vec<String> = Vec::new();
        let mut trouble = false;
        match carried {
            Carried::Nothing => {}
            Carried::PutBack(n) => notes.push(format!(
                "{} came along",
                count(n, "changed file", "changed files")
            )),
            Carried::SetAside(n) => notes.push(format!(
                "{} set aside in stash@{{0}} (:stashes)",
                count(n, "changed file", "changed files")
            )),
            Carried::Conflicted => {
                trouble = true;
                notes.push(
                    "your changes conflicted when put back: resolve them (:merge), the originals stay in stash@{0}"
                        .to_string(),
                );
            }
            Carried::NotPutBack(n) => {
                trouble = true;
                notes.push(format!(
                    "{} could not be put back here: {} in stash@{{0}} (:stashes)",
                    count(n, "changed file", "changed files"),
                    if n == 1 { "it stays" } else { "they stay" }
                ));
            }
        }
        let undoing = head_move.undoes.is_some();
        if let Some(undone) = head_move.undoes.as_ref() {
            for id in &undone.saved {
                match self.pop_stash_commit(id) {
                    Some(true) => notes.push("the discarded changes are back".to_string()),
                    Some(false) => {
                        trouble = true;
                        notes.push(
                            "the discarded changes conflicted when put back: they stay in the stash list"
                                .to_string(),
                        );
                    }
                    None => {}
                }
            }
        }
        self.after_head_moved();
        let now = self.head_position();
        let branch = now
            .as_ref()
            .and_then(|p| p.branch.clone())
            .unwrap_or_else(|| "HEAD".to_string());
        let moved = matches!((&before, &now), (Some(b), Some(n)) if b.id != n.id);
        let headline = match head_move.kind {
            HeadMoveKind::Switch => format!("Switched to {}", head_move.label),
            HeadMoveKind::Reset if undoing => {
                format!("{branch} moved back to {}", head_move.label)
            }
            HeadMoveKind::Reset if moved => format!("{branch} \u{2192} {}", head_move.label),
            HeadMoveKind::Reset => format!("{branch} is already at {}", head_move.label),
        };
        if head_move.kind == HeadMoveKind::Reset
            && moved
            && let (Some(before), Some(now)) = (before, now)
        {
            notes.push(if undoing {
                ":undo again reverses this".to_string()
            } else {
                format!("was {}, :undo puts it back", short(&before.id))
            });
            self.last_branch_move = Some(BranchMove {
                at: now,
                from: before.id,
                what: head_move
                    .undoes
                    .map_or_else(|| "reset".to_string(), |undone| undone.what),
                back: ResetMode::Keep,
                saved: Vec::new(),
            });
        }
        let text = std::iter::once(headline)
            .chain(notes)
            .collect::<Vec<_>>()
            .join(" \u{00b7} ");
        if trouble {
            self.set_warning(text);
        } else {
            self.set_message(text);
        }
    }

    // ── Reset ────────────────────────────────────────────────────────────

    /// `x` on a commit, or `:reset` without a mode: ask how, saying what
    /// each answer does to the commits and to uncommitted changes.
    pub fn open_reset_chooser(&mut self, id: String) {
        let summary = self.commit_summary(&id);
        let branch = self
            .head_position()
            .and_then(|p| p.branch)
            .or_else(|| self.vcs_info.branch_name.clone())
            .unwrap_or_else(|| "HEAD (detached)".to_string());
        let (leave, arrive) = self
            .git_line(&[
                "rev-list",
                "--left-right",
                "--count",
                &format!("HEAD...{id}"),
            ])
            .and_then(|out| {
                let mut nums = out.split_whitespace().map(|n| n.parse::<usize>().ok());
                Some((nums.next()??, nums.next()??))
            })
            .unwrap_or((0, 0));
        let dirty = self
            .vcs
            .run_git(&["status", "--porcelain", "--untracked-files=no"], false)
            .map(|out| out.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0);

        let mut facts: Vec<String> = Vec::new();
        if leave > 0 {
            facts.push(format!(
                "{} the branch",
                count(leave, "commit leaves", "commits leave")
            ));
        }
        if arrive > 0 {
            facts.push(count(arrive, "commit arrives", "commits arrive"));
        }
        if leave == 0 && arrive == 0 {
            facts.push("the branch is already here".to_string());
        }
        facts.push(match dirty {
            0 => "no uncommitted changes".to_string(),
            n => format!("{} uncommitted changes", count(n, "file has", "files have")),
        });

        let theirs = match leave {
            0 => "the files stay as they are,".to_string(),
            n => format!("the {} changes become", count(n, "commit's", "commits'")),
        };
        let options = vec![
            ChoiceOption {
                key: 'k',
                label: "keep",
                description: if dirty > 0 {
                    "move the branch, keep my uncommitted changes".to_string()
                } else {
                    "move the branch and the files with it".to_string()
                },
            },
            ChoiceOption {
                key: 'h',
                label: "hard",
                description: if dirty > 0 {
                    "move the branch, discard uncommitted changes (saved as a stash)".to_string()
                } else {
                    "the same as keep here: nothing uncommitted to discard".to_string()
                },
            },
            ChoiceOption {
                key: 'm',
                label: "mixed",
                description: format!("move the branch, {theirs} unstaged"),
            },
            ChoiceOption {
                key: 's',
                label: "soft",
                description: format!("move the branch, {theirs} staged"),
            },
        ];
        let prompt = ChoicePrompt {
            heading: " Reset ",
            title: format!("Reset {branch} to {}", commit_label(&id, &summary)),
            detail: vec![facts.join(" \u{00b7} ")],
            options,
            default: 'k',
            then: ChoiceThen::Reset { id, summary },
            return_mode: self.history_return_mode(),
        };
        self.open_choice(prompt);
    }

    /// Reset the current branch to `id`. Keep carries uncommitted changes
    /// (asking when some are in the way); hard stashes them first, so even
    /// that can be taken back.
    pub(in crate::app) fn reset_to_commit(&mut self, id: &str, summary: &str, mode: ResetMode) {
        match mode {
            ResetMode::Keep => self.start_head_move(HeadMove::reset(id, summary), false),
            ResetMode::Hard => self.reset_hard(id, summary),
            ResetMode::Soft | ResetMode::Mixed => {
                let before = self.head_position();
                match self.vcs.reset_to(id, mode) {
                    Ok(()) => {
                        self.after_head_moved();
                        self.report_reset(before, id, summary, mode, Vec::new());
                    }
                    Err(e) => self.set_error(format!("Reset failed: {}", e.to_string().trim())),
                }
            }
        }
    }

    fn reset_hard(&mut self, id: &str, summary: &str) {
        let before = self.head_position();
        let mut saved: Vec<String> = Vec::new();
        // `reset --hard` writes over an untracked file the target has under
        // version control without a word, so those go to the stash first.
        let untracked = self.untracked_in_the_way(id);
        if !untracked.is_empty() {
            let tip = self.stash_tip();
            let message = format!(
                "tuicr: untracked files in the way of reset --hard to {}",
                short(id)
            );
            let mut args = vec![
                "--literal-pathspecs",
                "stash",
                "push",
                "--include-untracked",
                "-m",
                message.as_str(),
                "--",
            ];
            args.extend(untracked.iter().map(String::as_str));
            if let Err(e) = self.vcs.run_git(&args, false) {
                self.set_error(format!(
                    "Nothing was reset: setting aside the untracked files in the way failed: {}",
                    e.to_string().trim()
                ));
                return;
            }
            if let Some(new) = self.stash_tip().filter(|t| Some(t) != tip.as_ref()) {
                saved.push(new);
            }
        }
        let message = format!("tuicr: before reset --hard to {}", short(id));
        match self.snapshot_uncommitted(&message) {
            Ok(Some(stash)) => saved.push(stash),
            Ok(None) => {}
            Err(e) => {
                for stash in &saved {
                    self.pop_stash_commit(stash);
                }
                self.set_error(format!(
                    "Nothing was reset: saving the uncommitted changes first failed: {}",
                    e.to_string().trim()
                ));
                return;
            }
        }
        match self.vcs.reset_to(id, ResetMode::Hard) {
            Ok(()) => {
                self.after_head_moved();
                self.report_reset(before, id, summary, ResetMode::Hard, saved);
            }
            Err(e) => {
                let tail = if saved.is_empty() {
                    ""
                } else {
                    " \u{00b7} a copy of your changes is in the stash list"
                };
                self.set_error(format!("Reset failed: {}{tail}", e.to_string().trim()));
            }
        }
    }

    /// Store the uncommitted changes to tracked files as a stash entry
    /// without touching the tree. `None` when there are none.
    fn snapshot_uncommitted(&self, message: &str) -> crate::error::Result<Option<String>> {
        let id = self.vcs.run_git(&["stash", "create", message], false)?;
        let id = id.trim();
        if id.is_empty() {
            return Ok(None);
        }
        self.vcs
            .run_git(&["stash", "store", "-m", message, id], false)?;
        Ok(Some(id.to_string()))
    }

    fn report_reset(
        &mut self,
        before: Option<HeadPosition>,
        id: &str,
        summary: &str,
        mode: ResetMode,
        saved: Vec<String>,
    ) {
        let now = self.head_position();
        let branch = now
            .as_ref()
            .and_then(|p| p.branch.clone())
            .unwrap_or_else(|| "HEAD".to_string());
        let label = commit_label(id, summary);
        let how = match mode {
            ResetMode::Soft => " (soft: the difference is staged)",
            ResetMode::Mixed => " (mixed: the difference is unstaged)",
            ResetMode::Hard | ResetMode::Keep => "",
        };
        let moved = matches!((&before, &now), (Some(b), Some(n)) if b.id != n.id);
        let mut notes: Vec<String> = Vec::new();
        if !saved.is_empty() {
            notes.push("discarded changes saved as stash@{0}".to_string());
        }
        if let (Some(before), Some(now)) = (before, now)
            && (moved || !saved.is_empty())
        {
            notes.push(if moved {
                format!("was {}, :undo puts it back", short(&before.id))
            } else {
                ":undo puts them back".to_string()
            });
            self.last_branch_move = Some(BranchMove {
                at: now,
                from: before.id,
                what: "reset".to_string(),
                back: match mode {
                    ResetMode::Soft => ResetMode::Soft,
                    ResetMode::Mixed => ResetMode::Mixed,
                    ResetMode::Hard | ResetMode::Keep => ResetMode::Keep,
                },
                saved,
            });
        }
        let headline = if moved {
            format!("{branch} \u{2192} {label}{how}")
        } else {
            format!("{branch} reset to {label}{how}")
        };
        self.set_message(
            std::iter::once(headline)
                .chain(notes)
                .collect::<Vec<_>>()
                .join(" \u{00b7} "),
        );
    }

    // ── Undo ─────────────────────────────────────────────────────────────

    /// `:undo`. Right after a reset or a rebase it puts the branch back
    /// where it was (and again reverses that); otherwise it takes the last
    /// commit back, keeping its changes staged.
    pub fn undo(&mut self) {
        if let Some(last) = self.last_branch_move.take()
            && self.head_position().as_ref() == Some(&last.at)
        {
            self.undo_branch_move(last);
            return;
        }
        self.undo_last_commit();
    }

    fn undo_branch_move(&mut self, last: BranchMove) {
        let summary = self.commit_summary(&last.from);
        match last.back {
            ResetMode::Soft | ResetMode::Mixed => match self.vcs.reset_to(&last.from, last.back) {
                Ok(()) => {
                    self.after_head_moved();
                    let branch = last.at.branch.clone().unwrap_or_else(|| "HEAD".to_string());
                    self.set_message(format!(
                        "{branch} moved back to {} \u{00b7} :undo again reverses this",
                        commit_label(&last.from, &summary)
                    ));
                    if let Some(now) = self.head_position() {
                        self.last_branch_move = Some(BranchMove {
                            at: now,
                            from: last.at.id,
                            what: last.what,
                            back: last.back,
                            saved: Vec::new(),
                        });
                    }
                }
                Err(e) => {
                    self.set_error(format!("Undo failed: {}", e.to_string().trim()));
                    self.last_branch_move = Some(last);
                }
            },
            ResetMode::Hard | ResetMode::Keep => {
                let mut head_move = HeadMove::reset(&last.from, &summary);
                head_move.undoes = Some(last);
                self.start_head_move(head_move, false);
            }
        }
    }

    // ── Operations that move the branch on their own ─────────────────────

    /// Note HEAD and the stash list before a rebase or other history
    /// rewrite; hand the answer to [`settle_branch_move`](Self::settle_branch_move).
    pub(in crate::app) fn watch_branch_move(&self) -> MoveWatch {
        MoveWatch {
            before: self.head_position(),
            stash: self.stash_tip(),
            resuming: self.operation_in_progress().is_some(),
        }
    }

    /// Run a history rewrite, then record where it moved the branch from
    /// (for `:undo`) and check that its autostash went back.
    pub(in crate::app) fn tracking_branch_move<T>(
        &mut self,
        what: &str,
        op: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let watch = self.watch_branch_move();
        let out = op(self);
        self.settle_branch_move(what, watch);
        out
    }

    pub(in crate::app) fn settle_branch_move(&mut self, what: &str, watch: MoveWatch) {
        self.refresh_repo_status();
        let stopped = self
            .repo_status
            .as_ref()
            .and_then(|s| s.operation)
            .is_some();
        // The record of a stopped move is only good for the operation that
        // made it. One that ended some other way (aborted from a shell, or
        // with `:git`) is still here when the next operation starts fresh,
        // and `:undo` must not take the branch back to where that one began.
        if !watch.resuming {
            self.move_in_flight = None;
        }
        if stopped {
            // Stopped on conflicts: `:continue` finishes the move later,
            // from where the branch is now.
            if self.move_in_flight.is_none()
                && let Some(before) = watch.before
            {
                self.move_in_flight = Some((before, what.to_string()));
            }
            return;
        }
        let (start, what) = match self.move_in_flight.take() {
            Some((start, what)) => (Some(start), what),
            None => (watch.before, what.to_string()),
        };
        let now = self.head_position();
        if let (Some(start), Some(now)) = (start, now)
            && start.branch.is_some()
            && start.branch == now.branch
            && start.id != now.id
        {
            self.last_branch_move = Some(BranchMove {
                at: now,
                from: start.id,
                what,
                back: ResetMode::Keep,
                saved: Vec::new(),
            });
            if let Some(message) = self.message.as_mut()
                && message.message_type == MessageType::Info
            {
                message.content.push_str(" \u{00b7} :undo puts it back");
            }
        }
        // git finishes a rebase even when the autostash no longer applies:
        // it leaves conflict markers and moves the changes to the stash
        // list. Say so, or they look lost.
        let tip = self.stash_tip();
        if tip.is_some()
            && tip != watch.stash
            && self
                .git_line(&["stash", "list", "-1", "--format=%gs"])
                .as_deref()
                == Some("autostash")
        {
            let said = self
                .message
                .as_ref()
                .map(|m| format!("{} \u{00b7} ", m.content))
                .unwrap_or_default();
            self.set_warning(if self.has_unmerged_entries() {
                format!(
                    "{said}your uncommitted changes conflicted when put back: resolve them (:merge), the originals are in stash@{{0}}"
                )
            } else {
                format!(
                    "{said}your uncommitted changes could not be put back: they are in stash@{{0}} (:stashes)"
                )
            });
        }
    }

    // ── The choice prompt ────────────────────────────────────────────────

    fn open_choice(&mut self, prompt: ChoicePrompt) {
        self.pending_choice = Some(prompt);
        self.pending_confirm = Some(ConfirmAction::Choice);
        self.input_mode = InputMode::Confirm;
    }

    /// A key on the choice prompt. Keys that are not an option do nothing.
    pub fn choose(&mut self, key: char) {
        let key = key.to_ascii_lowercase();
        if !self
            .pending_choice
            .as_ref()
            .is_some_and(|p| p.options.iter().any(|o| o.key == key))
        {
            return;
        }
        let Some(prompt) = self.pending_choice.take() else {
            return;
        };
        self.pending_confirm = None;
        self.input_mode = prompt.return_mode;
        match prompt.then {
            ChoiceThen::Reset { id, summary } => {
                let mode = match key {
                    'h' => ResetMode::Hard,
                    'm' => ResetMode::Mixed,
                    's' => ResetMode::Soft,
                    _ => ResetMode::Keep,
                };
                self.reset_to_commit(&id, &summary, mode);
            }
            ChoiceThen::InTheWay { head_move, files } => {
                self.carry_head_move(head_move, files, key == 's');
            }
        }
    }

    /// Enter on the choice prompt.
    pub fn choose_default(&mut self) {
        if let Some(key) = self.pending_choice.as_ref().map(|p| p.default) {
            self.choose(key);
        }
    }

    /// Esc on the choice prompt: nothing happens.
    pub fn cancel_choice(&mut self) {
        self.pending_confirm = None;
        match self.pending_choice.take() {
            Some(prompt) => {
                self.input_mode = prompt.return_mode;
                if let ChoiceThen::InTheWay { head_move, .. } = prompt.then {
                    self.keep_undo(head_move);
                }
            }
            None => self.input_mode = InputMode::Normal,
        }
        self.set_message("Cancelled");
    }
}
