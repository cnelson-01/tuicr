mod cli;
pub mod context;
pub mod diff;
mod libgit2;
pub(crate) mod raw;
pub mod repository;
pub mod staging;

use crate::vcs::traits::{
    BlameLine, BranchInfo, CommitQuery, HunkScope, LinePick, RebaseAction, RebaseStep, ResetMode,
    StashEntry,
};
use chrono::{DateTime, TimeZone, Utc};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::{Result, TuicrError};
use crate::model::{DiffFile, DiffLine, FileStatus};
use crate::process::{CommandOutputError, CommandOutputErrorKind, run_command_output};
use crate::syntax::SyntaxHighlighter;

use super::traits::{
    ChangeKind, CommitInfo, DiffWhitespaceMode, MergeConflictInfo, ResolvedRevisionRange,
    VcsBackend, VcsChangeStatus, VcsInfo,
};
use cli::GitCliBackend;
pub use libgit2::Libgit2Backend;

// Re-exported for UI/app gap calculations.
pub use context::calculate_gap;

/// RevisionExpression is Git's parsed view of a user-supplied revision string.
///
/// Accepted forms are:
/// `REV` for a single commit, such as `HEAD`;
/// `A..B`, `A..`, or `..B` for a two-dot range; and
/// `A...B` for a merge-base range.
pub(super) enum RevisionExpression<'a> {
    /// A single commit expression, for example `HEAD`.
    Single(&'a str),

    /// A two-dot range.
    ///
    /// `A..B` is represented as `base = "A"` and `head = "B"`.
    /// `A..` is represented as `base = "A"` and `head = "HEAD"`.
    /// `..B` is represented as `base = "HEAD"` and `head = "B"`.
    Range { base: &'a str, head: &'a str },

    /// A three-dot range, for example `A...B`.
    MergeBaseRange { left: &'a str, right: &'a str },
}

impl<'a> RevisionExpression<'a> {
    /// Parse Git revision syntax accepted by tuicr's `-r` option.
    ///
    /// This accepts `REV`, `A..B`, `A..`, `..B`, and `A...B`.
    /// Open-ended two-dot ranges are normalized to use `HEAD` for the
    /// missing endpoint.
    pub(super) fn parse(revisions: &'a str) -> Result<Self> {
        if let Some((left, right)) = revisions.split_once("...") {
            if left.is_empty() || right.is_empty() {
                return Err(TuicrError::VcsCommand(
                    "Invalid revision range: missing endpoint".into(),
                ));
            }
            return Ok(Self::MergeBaseRange { left, right });
        }

        if let Some((base, head)) = revisions.split_once("..") {
            let base = if base.is_empty() { "HEAD" } else { base };
            let head = if head.is_empty() { "HEAD" } else { head };
            return Ok(Self::Range { base, head });
        }

        Ok(Self::Single(revisions))
    }
}

/// Top-level Git backend.
///
/// This wrapper keeps Git backend selection in one place. Today it delegates to
/// the git2/libgit2 implementation; sparse-checkout support can add another
/// variant without pushing backend-specific branches into every operation.
pub enum GitBackend {
    Libgit2(Libgit2Backend),
    Cli(GitCliBackend),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitBackendPreference {
    Libgit2,
    Cli,
}

impl GitBackendPreference {
    pub fn from_config(value: Option<&str>) -> Self {
        match value {
            Some("cli") => Self::Cli,
            _ => Self::Libgit2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRepoMode {
    Standard,
    SparseCheckout,
    SparseIndex,
}

impl GitRepoMode {
    fn detect(root_path: &Path) -> Result<Self> {
        let output = run_git_command(
            root_path,
            &[
                "config",
                "--get-regexp",
                r"^(core\.sparsecheckout|index\.sparse)$",
            ],
        )
        .unwrap_or_default();

        Ok(Self::from_config(&output))
    }

    fn from_config(output: &str) -> Self {
        let mut sparse_checkout = false;
        let mut sparse_index = false;

        for line in output.lines() {
            let mut parts = line.splitn(2, char::is_whitespace);
            let Some(key) = parts.next() else {
                continue;
            };
            let raw_value = parts.next().unwrap_or_default();

            match key {
                "core.sparsecheckout" => sparse_checkout = git_bool_config_enabled(raw_value),
                "index.sparse" => sparse_index = git_bool_config_enabled(raw_value),
                _ => {}
            }
        }

        if sparse_index {
            Self::SparseIndex
        } else if sparse_checkout {
            Self::SparseCheckout
        } else {
            Self::Standard
        }
    }

    fn is_sparse_checkout(self) -> bool {
        matches!(self, Self::SparseCheckout | Self::SparseIndex)
    }
}

impl GitBackend {
    /// CLI history operations rewrite `.git/index` (and move HEAD) behind
    /// libgit2's back. Refresh the cached index afterwards — success or
    /// failure, since a conflict stop also rewrites the index — so the
    /// status probes and staged/unstaged diffs that follow see reality.
    fn refresh_after_cli_op<T>(&self, result: Result<T>) -> Result<T> {
        if let Self::Libgit2(backend) = self {
            backend.reload_index();
        }
        result
    }

    /// Discover a git repository from the current directory.
    pub fn discover(
        preference: GitBackendPreference,
        whitespace_mode: DiffWhitespaceMode,
    ) -> Result<Self> {
        let cwd = std::env::current_dir().map_err(|_| TuicrError::NotARepository)?;
        Self::discover_from(&cwd, preference, whitespace_mode)
    }

    pub(crate) fn discover_from(
        cwd: &Path,
        preference: GitBackendPreference,
        whitespace_mode: DiffWhitespaceMode,
    ) -> Result<Self> {
        // libgit2 doesn't support reftable, split index, or SHA-256 repositories.
        // TODO: remove reftable fallback logic when libgit2 supports it as part of https://github.com/libgit2/libgit2/issues/5352
        // TODO: remove split index fallback logic when libgit2 supports it as part of https://github.com/libgit2/libgit2/issues/6132
        let use_cli = preference == GitBackendPreference::Cli
            || uses_reftable(cwd)
            || uses_split_index(cwd)
            || uses_sha256(cwd);

        if use_cli {
            return Ok(Self::Cli(GitCliBackend::discover_from(
                cwd,
                whitespace_mode,
            )?));
        }

        let backend = Self::Libgit2(Libgit2Backend::discover_from(cwd, whitespace_mode.clone())?);
        let repo_mode = GitRepoMode::detect(&backend.info().root_path)?;
        if repo_mode.is_sparse_checkout() && !backend.supports_sparse_checkout() {
            return Ok(Self::Cli(GitCliBackend::discover_from(
                cwd,
                whitespace_mode,
            )?));
        }

        Ok(backend)
    }
}

/// Read a named remote's fetch URL, including Git's `insteadOf` rewrites.
pub(super) fn remote_url(repo_root: &Path, name: &str) -> Result<String> {
    run_git_command(repo_root, &["remote", "get-url", "--", name])
}

fn run_git_command(workdir: &Path, args: &[&str]) -> Result<String> {
    // `-c commit.gpgsign=false` is a no-op for the read-only `config`/`init`
    // calls this makes in production, but it keeps the test-only `commit`
    // calls below from prompting contributors with commit signing enabled
    // globally to sign throwaway commits in temp repos.
    let full_args: Vec<&str> = [
        "-c",
        "commit.gpgsign=false",
        "-c",
        "init.defaultRefFormat=files",
    ]
    .into_iter()
    .chain(args.iter().copied())
    .collect();
    run_command_output(
        "git",
        Some(workdir),
        full_args.iter().map(|arg| OsStr::new(*arg)),
    )
    .map_err(git_command_error)
}

pub(super) fn git_command_error(error: CommandOutputError) -> TuicrError {
    match error.kind {
        CommandOutputErrorKind::Unsuccessful => TuicrError::VcsCommand(error.stderr),
        CommandOutputErrorKind::NotFound | CommandOutputErrorKind::SpawnFailed => {
            TuicrError::VcsCommand(format!("Failed to run git: {}", error.stderr))
        }
    }
}

fn git_bool_config_enabled(value: &str) -> bool {
    matches!(value.trim(), "true" | "1" | "yes" | "on")
}

fn git_fsmonitor_config_enabled(value: &str) -> bool {
    let value = value.trim();
    git_bool_config_enabled(value)
        || (!value.is_empty() && !matches!(value, "false" | "0" | "no" | "off"))
}

fn uses_sha256(cwd: &Path) -> bool {
    run_git_command(cwd, &["rev-parse", "--show-object-format"])
        .is_ok_and(|format| format.trim() == "sha256")
}

fn uses_reftable(cwd: &Path) -> bool {
    run_git_command(cwd, &["config", "--get", "extensions.refStorage"])
        .ok()
        .map(|v| v.trim().eq_ignore_ascii_case("reftable"))
        .unwrap_or(false)
}

fn uses_split_index(cwd: &Path) -> bool {
    run_git_command(cwd, &["config", "--get", "core.splitIndex"])
        .ok()
        .map(|v| git_bool_config_enabled(&v))
        .unwrap_or(false)
}

impl VcsBackend for GitBackend {
    fn info(&self) -> &VcsInfo {
        match self {
            Self::Libgit2(backend) => backend.info(),
            Self::Cli(backend) => backend.info(),
        }
    }

    fn remote_url(&self, name: &str) -> Result<String> {
        remote_url(&self.info().root_path, name)
    }

    fn startup_warnings(&self) -> Vec<String> {
        match self {
            Self::Libgit2(backend) => backend.startup_warnings(),
            Self::Cli(backend) => backend.startup_warnings(),
        }
    }

    fn supports_sparse_checkout(&self) -> bool {
        match self {
            Self::Libgit2(backend) => backend.supports_sparse_checkout(),
            Self::Cli(backend) => backend.supports_sparse_checkout(),
        }
    }

    fn get_working_tree_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        match self {
            Self::Libgit2(backend) => backend.get_working_tree_diff(highlighter),
            Self::Cli(backend) => backend.get_working_tree_diff(highlighter),
        }
    }

    fn get_staged_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        match self {
            Self::Libgit2(backend) => backend.get_staged_diff(highlighter),
            Self::Cli(backend) => backend.get_staged_diff(highlighter),
        }
    }

    fn get_unstaged_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        match self {
            Self::Libgit2(backend) => backend.get_unstaged_diff(highlighter),
            Self::Cli(backend) => backend.get_unstaged_diff(highlighter),
        }
    }

    fn get_change_status(&self) -> Result<VcsChangeStatus> {
        match self {
            Self::Libgit2(backend) => backend.get_change_status(),
            Self::Cli(backend) => backend.get_change_status(),
        }
    }

    fn list_changed_paths(&self, kind: ChangeKind) -> Result<Vec<PathBuf>> {
        match self {
            Self::Libgit2(backend) => backend.list_changed_paths(kind),
            Self::Cli(backend) => backend.list_changed_paths(kind),
        }
    }

    fn fetch_context_lines(
        &self,
        file_path: &Path,
        file_status: FileStatus,
        ref_commit: Option<&str>,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<DiffLine>> {
        match self {
            Self::Libgit2(backend) => backend.fetch_context_lines(
                file_path,
                file_status,
                ref_commit,
                start_line,
                end_line,
            ),
            Self::Cli(backend) => backend.fetch_context_lines(
                file_path,
                file_status,
                ref_commit,
                start_line,
                end_line,
            ),
        }
    }

    fn file_line_count(
        &self,
        file_path: &Path,
        file_status: FileStatus,
        ref_commit: Option<&str>,
    ) -> Result<u32> {
        match self {
            Self::Libgit2(backend) => backend.file_line_count(file_path, file_status, ref_commit),
            Self::Cli(backend) => backend.file_line_count(file_path, file_status, ref_commit),
        }
    }

    fn get_recent_commits(&self, offset: usize, limit: usize) -> Result<Vec<CommitInfo>> {
        match self {
            Self::Libgit2(backend) => backend.get_recent_commits(offset, limit),
            Self::Cli(backend) => backend.get_recent_commits(offset, limit),
        }
    }

    fn resolve_revision_range(&self, revisions: &str) -> Result<ResolvedRevisionRange<'static>> {
        match self {
            Self::Libgit2(backend) => backend.resolve_revision_range(revisions),
            Self::Cli(backend) => backend.resolve_revision_range(revisions),
        }
    }

    fn get_commit_range_diff(
        &self,
        revision_range: &ResolvedRevisionRange<'_>,
        highlighter: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        match self {
            Self::Libgit2(backend) => backend.get_commit_range_diff(revision_range, highlighter),
            Self::Cli(backend) => backend.get_commit_range_diff(revision_range, highlighter),
        }
    }

    fn get_commits_info(&self, ids: &[String]) -> Result<Vec<CommitInfo>> {
        match self {
            Self::Libgit2(backend) => backend.get_commits_info(ids),
            Self::Cli(backend) => backend.get_commits_info(ids),
        }
    }

    fn get_working_tree_with_commits_diff(
        &self,
        commit_ids: &[String],
        highlighter: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        match self {
            Self::Libgit2(backend) => {
                backend.get_working_tree_with_commits_diff(commit_ids, highlighter)
            }
            Self::Cli(backend) => {
                backend.get_working_tree_with_commits_diff(commit_ids, highlighter)
            }
        }
    }

    fn stage_file(&self, path: &Path) -> Result<()> {
        match self {
            Self::Libgit2(backend) => backend.stage_file(path),
            Self::Cli(backend) => backend.stage_file(path),
        }
    }

    fn get_merge_conflicts(&self) -> Result<Option<MergeConflictInfo>> {
        match self {
            Self::Libgit2(backend) => backend.get_merge_conflicts(),
            Self::Cli(backend) => backend.get_merge_conflicts(),
        }
    }

    // History-rewrite operations shell out to the git CLI regardless of the
    // selected diff backend: libgit2's cherry-pick/rebase plumbing would
    // have to reimplement sequencer state (.git/sequencer, --continue)
    // that users expect to finish from their shell.
    fn cherry_pick(&self, commit_ids: &[String]) -> Result<()> {
        if commit_ids.is_empty() {
            return Ok(());
        }
        let mut args: Vec<&str> = vec!["cherry-pick"];
        args.extend(commit_ids.iter().map(String::as_str));
        let result = run_git_command(&self.info().root_path, &args).map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn rebase_onto(&self, commit_id: &str) -> Result<()> {
        let result = run_git_command(&self.info().root_path, &["rebase", commit_id]).map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn squash_commits(&self, commit_ids: &[String]) -> Result<()> {
        let result = self.squash_commits_inner(commit_ids);
        self.refresh_after_cli_op(result)
    }

    fn continue_merge_operation(&self) -> Result<String> {
        let result = self.continue_merge_operation_inner();
        self.refresh_after_cli_op(result)
    }

    fn search_commits(
        &self,
        query: &CommitQuery,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<CommitInfo>> {
        // Both backends search through the CLI: libgit2 has no message /
        // author / path limiting in its revwalk.
        cli::search_commits(&self.info().root_path, query, offset, limit)
    }

    fn list_branches(&self) -> Result<Vec<BranchInfo>> {
        let output = run_git_command(
            &self.info().root_path,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(refname:short)%00%(objectname)%00%(upstream:short)%00%(upstream:track)%00%(HEAD)",
                "refs/heads",
                "refs/remotes",
            ],
        )?;
        Ok(parse_branch_list(&output))
    }

    fn checkout(&self, target: &str) -> Result<()> {
        let result = run_git_command(&self.info().root_path, &["checkout", target]).map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn create_branch(&self, name: &str, start_point: Option<&str>) -> Result<()> {
        let mut args = vec!["checkout", "-b", name];
        if let Some(start) = start_point {
            args.push(start);
        }
        let result = run_git_command(&self.info().root_path, &args).map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn stash_push(&self, message: &str) -> Result<()> {
        let result = run_git_command(
            &self.info().root_path,
            &["stash", "push", "--include-untracked", "-m", message],
        )
        .map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn stash_pop(&self) -> Result<()> {
        let result = run_git_command(&self.info().root_path, &["stash", "pop"]).map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn abort_merge_operation(&self) -> Result<String> {
        let result = self.abort_merge_operation_inner();
        self.refresh_after_cli_op(result)
    }

    fn unstage_file(&self, path: &Path) -> Result<()> {
        let root = &self.info().root_path;
        let path = path.to_string_lossy();
        // `reset` needs a HEAD; on an unborn branch fall back to dropping
        // the entry from the index.
        let result = run_git_command(root, &["reset", "-q", "--", &path])
            .or_else(|_| run_git_command(root, &["rm", "-q", "--cached", "--", &path]))
            .map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn discard_file(&self, path: &Path) -> Result<()> {
        let root = &self.info().root_path;
        let path = path.to_string_lossy();
        let tracked = run_git_command(root, &["ls-files", "--error-unmatch", "--", &path]).is_ok();
        let in_head = run_git_command(root, &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok();
        let result = if !tracked {
            run_git_command(root, &["clean", "-f", "-q", "--", &path])
        } else if in_head {
            run_git_command(root, &["checkout", "HEAD", "--", &path])
        } else {
            run_git_command(root, &["rm", "-q", "-f", "--", &path])
        }
        .map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn stage_hunk(
        &self,
        path: &Path,
        scope: HunkScope,
        old_start: u32,
        new_start: u32,
    ) -> Result<()> {
        let root = &self.info().root_path;
        let result =
            extract_hunk_patch(root, path, scope, old_start, new_start).and_then(|patch| {
                run_git_stdin(root, &["apply", "--cached", "--whitespace=nowarn"], &patch)
                    .map(|_| ())
            });
        self.refresh_after_cli_op(result)
    }

    fn stage_lines(
        &self,
        path: &Path,
        scope: HunkScope,
        old_start: u32,
        new_start: u32,
        pick: &LinePick,
    ) -> Result<()> {
        let root = &self.info().root_path;
        let result = extract_hunk_patch(root, path, scope, old_start, new_start)
            .and_then(|patch| filter_patch_lines(&patch, pick, false))
            .and_then(|patch| {
                run_git_stdin(
                    root,
                    &["apply", "--cached", "--recount", "--whitespace=nowarn"],
                    &patch,
                )
                .map(|_| ())
            });
        self.refresh_after_cli_op(result)
    }

    fn unstage_lines(
        &self,
        path: &Path,
        old_start: u32,
        new_start: u32,
        pick: &LinePick,
    ) -> Result<()> {
        let root = &self.info().root_path;
        let result = extract_hunk_patch(root, path, HunkScope::Staged, old_start, new_start)
            .and_then(|patch| filter_patch_lines(&patch, pick, true))
            .and_then(|patch| {
                run_git_stdin(
                    root,
                    &[
                        "apply",
                        "--cached",
                        "-R",
                        "--recount",
                        "--whitespace=nowarn",
                    ],
                    &patch,
                )
                .map(|_| ())
            });
        self.refresh_after_cli_op(result)
    }

    fn unstage_hunk(&self, path: &Path, old_start: u32, new_start: u32) -> Result<()> {
        let root = &self.info().root_path;
        let result = extract_hunk_patch(root, path, HunkScope::Staged, old_start, new_start)
            .and_then(|patch| {
                run_git_stdin(
                    root,
                    &["apply", "--cached", "-R", "--whitespace=nowarn"],
                    &patch,
                )
                .map(|_| ())
            });
        self.refresh_after_cli_op(result)
    }

    fn discard_hunk(
        &self,
        path: &Path,
        scope: HunkScope,
        old_start: u32,
        new_start: u32,
    ) -> Result<()> {
        let root = &self.info().root_path;
        let result =
            extract_hunk_patch(root, path, scope, old_start, new_start).and_then(|patch| {
                let worktree_only = ["apply", "-R", "--whitespace=nowarn"];
                let with_index = ["apply", "-R", "--index", "--whitespace=nowarn"];
                match scope {
                    HunkScope::Unstaged => run_git_stdin(root, &worktree_only, &patch),
                    HunkScope::Staged => run_git_stdin(root, &with_index, &patch),
                    // A HEAD-based hunk may be partly staged: revert both when
                    // the index agrees with the tree, else just the tree.
                    HunkScope::WorkingTree => run_git_stdin(root, &with_index, &patch)
                        .or_else(|_| run_git_stdin(root, &worktree_only, &patch)),
                }
                .map(|_| ())
            });
        self.refresh_after_cli_op(result)
    }

    fn commit(&self, message: &str, amend: bool) -> Result<String> {
        let root = &self.info().root_path;
        let mut args = vec!["commit", "-q"];
        if amend {
            args.push("--amend");
        }
        args.extend(["-m", message]);
        let result = run_git_command(root, &args)
            .and_then(|_| run_git_command(root, &["rev-parse", "--short", "HEAD"]))
            .map(|id| id.trim().to_string());
        self.refresh_after_cli_op(result)
    }

    fn head_commit_message(&self) -> Result<String> {
        run_git_command(&self.info().root_path, &["log", "-1", "--format=%B"])
    }

    fn stash_list(&self) -> Result<Vec<StashEntry>> {
        let output = run_git_command(
            &self.info().root_path,
            &["stash", "list", "--format=%gd%x00%s"],
        )?;
        Ok(output
            .lines()
            .filter_map(|line| {
                let (selector, message) = line.split_once('\0')?;
                let index = selector
                    .trim_start_matches("stash@{")
                    .trim_end_matches('}')
                    .parse()
                    .ok()?;
                Some(StashEntry {
                    index,
                    message: message.to_string(),
                })
            })
            .collect())
    }

    fn stash_apply(&self, index: usize, pop: bool) -> Result<()> {
        let selector = format!("stash@{{{index}}}");
        let verb = if pop { "pop" } else { "apply" };
        let result =
            run_git_command(&self.info().root_path, &["stash", verb, "-q", &selector]).map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn stash_drop(&self, index: usize) -> Result<()> {
        let selector = format!("stash@{{{index}}}");
        run_git_command(&self.info().root_path, &["stash", "drop", "-q", &selector]).map(|_| ())
    }

    fn revert_commit(&self, id: &str) -> Result<()> {
        let result = run_git_command(
            &self.info().root_path,
            &["-c", "core.editor=true", "revert", "--no-edit", id],
        )
        .map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn reset_to(&self, target: &str, mode: ResetMode) -> Result<()> {
        let flag = match mode {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        };
        let result =
            run_git_command(&self.info().root_path, &["reset", "-q", flag, target]).map(|_| ());
        self.refresh_after_cli_op(result)
    }

    fn reword_commit(&self, id: &str, message: &str) -> Result<()> {
        let root = &self.info().root_path;
        let head = run_git_command(root, &["rev-parse", "HEAD"])?
            .trim()
            .to_string();
        let full = run_git_command(
            root,
            &["rev-parse", "--verify", &format!("{id}^{{commit}}")],
        )?
        .trim()
        .to_string();
        let result = if full == head {
            // `--only` with no paths amends the message without sweeping in
            // whatever happens to be staged.
            run_git_command(root, &["commit", "--amend", "--only", "-q", "-m", message]).map(|_| ())
        } else {
            rebase_with_todo(root, &full, TodoAction::Reword(message))
        };
        self.refresh_after_cli_op(result)
    }

    fn drop_commit(&self, id: &str) -> Result<()> {
        let root = &self.info().root_path;
        let full = run_git_command(
            root,
            &["rev-parse", "--verify", &format!("{id}^{{commit}}")],
        )?
        .trim()
        .to_string();
        let result = rebase_with_todo(root, &full, TodoAction::Drop);
        self.refresh_after_cli_op(result)
    }

    fn fixup_commit(&self, id: &str) -> Result<()> {
        let root = &self.info().root_path;
        let full = run_git_command(
            root,
            &["rev-parse", "--verify", &format!("{id}^{{commit}}")],
        )?
        .trim()
        .to_string();
        if run_git_command(
            root,
            &["rev-parse", "--verify", "--quiet", &format!("{full}^")],
        )
        .is_err()
        {
            return Err(TuicrError::VcsCommand(
                "The root commit has no parent to squash into".into(),
            ));
        }
        let result = rebase_with_todo(root, &full, TodoAction::Fixup);
        self.refresh_after_cli_op(result)
    }

    fn create_tag(&self, name: &str, target: &str) -> Result<()> {
        run_git_command(&self.info().root_path, &["tag", name, target]).map(|_| ())
    }

    fn commit_message(&self, id: &str) -> Result<String> {
        run_git_command(&self.info().root_path, &["log", "-1", "--format=%B", id])
    }

    fn fetch(&self) -> Result<String> {
        let root = &self.info().root_path;
        let before = remote_ref_tips(root)?;
        let remotes: Vec<String> = run_git_command(root, &["remote"])?
            .lines()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string)
            .collect();
        if remotes.is_empty() {
            return Err(TuicrError::VcsCommand("No remotes configured".into()));
        }
        // One remote at a time, never interactive and never into
        // submodules, so a remote that wants credentials fails with its name
        // in the message instead of prompting under the TUI or stalling the
        // other remotes.
        let envs = non_interactive_git_env(root);
        let mut failures: Vec<String> = Vec::new();
        for remote in &remotes {
            let args: Vec<String> = ["fetch", "--prune", "--no-recurse-submodules", "--quiet"]
                .iter()
                .map(|s| s.to_string())
                .chain(std::iter::once(remote.clone()))
                .collect();
            if let Err(e) = run_git_env(root, &args, &envs) {
                let reason = e.to_string();
                let reason = reason
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("failed")
                    .trim()
                    .to_string();
                failures.push(format!("{remote} failed: {reason}"));
            }
        }
        if failures.len() == remotes.len() {
            return Err(TuicrError::VcsCommand(failures.join(" \u{00b7} ")));
        }
        let after = remote_ref_tips(root)?;
        let mut summary = describe_fetch(root, &before, &after);
        if !failures.is_empty() {
            summary.push_str(" \u{00b7} ");
            summary.push_str(&failures.join(" \u{00b7} "));
        }
        Ok(summary)
    }

    fn pull(&self, rebase: bool) -> Result<String> {
        let root = &self.info().root_path;
        let before = run_git_command(root, &["rev-parse", "HEAD"])?
            .trim()
            .to_string();
        let mode = if rebase { "--rebase" } else { "--ff-only" };
        let args: Vec<String> = ["pull", mode, "--no-recurse-submodules", "--quiet"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let envs = non_interactive_git_env(root);
        let result = run_git_env(root, &args, &envs).and_then(|_| {
            let after = run_git_command(root, &["rev-parse", "HEAD"])?
                .trim()
                .to_string();
            if after == before {
                return Ok("Already up to date".to_string());
            }
            let count = run_git_command(
                root,
                &["rev-list", "--count", &format!("{before}..{after}")],
            )
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|_| "some".to_string());
            let how = if rebase {
                "rebased onto"
            } else {
                "fast-forwarded to"
            };
            Ok(format!(
                "Pulled {count} commit{}, {how} {}",
                if count == "1" { "" } else { "s" },
                &after[..7.min(after.len())]
            ))
        });
        self.refresh_after_cli_op(result)
    }

    fn push(&self) -> Result<String> {
        let root = &self.info().root_path;
        let branch = run_git_command(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map_err(|_| {
                TuicrError::VcsCommand("HEAD is detached; check out a branch to push".into())
            })?
            .trim()
            .to_string();
        let upstream = run_git_command(
            root,
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        )
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty());
        match upstream {
            Some(upstream) => {
                let ahead = run_git_command(root, &["rev-list", "--count", "@{u}..HEAD"])
                    .map(|n| n.trim().to_string())
                    .unwrap_or_else(|_| "some".to_string());
                if ahead == "0" {
                    return Ok(format!(
                        "Nothing to push: {branch} is up to date with {upstream}"
                    ));
                }
                let envs = non_interactive_git_env(root);
                let args: Vec<String> = ["push", "--quiet", "--recurse-submodules=no"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                run_git_env(root, &args, &envs)?;
                Ok(format!(
                    "Pushed {ahead} commit{} from {branch} to {upstream}",
                    if ahead == "1" { "" } else { "s" }
                ))
            }
            None => {
                let remotes = run_git_command(root, &["remote"])?;
                let remote = remotes
                    .lines()
                    .find(|r| *r == "origin")
                    .or_else(|| remotes.lines().next())
                    .ok_or_else(|| TuicrError::VcsCommand("No remote configured".into()))?
                    .to_string();
                let envs = non_interactive_git_env(root);
                let args: Vec<String> = [
                    "push",
                    "--quiet",
                    "--recurse-submodules=no",
                    "--set-upstream",
                    remote.as_str(),
                    branch.as_str(),
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
                run_git_env(root, &args, &envs)?;
                Ok(format!(
                    "Pushed {branch} to {remote}/{branch} and set it as upstream"
                ))
            }
        }
    }

    fn rebase_interactive(&self, base: Option<&str>, steps: &[RebaseStep]) -> Result<()> {
        let root = &self.info().root_path;
        let result = rebase_with_steps(root, base, steps);
        self.refresh_after_cli_op(result)
    }

    fn run_git(&self, args: &[&str], network: bool) -> Result<String> {
        let root = &self.info().root_path;
        let result = if network {
            let envs = non_interactive_git_env(root);
            let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            run_git_env(root, &owned, &envs)
        } else {
            run_git_command(root, args)
        };
        self.refresh_after_cli_op(result)
    }

    fn blame_file(&self, path: &Path, revision: Option<&str>) -> Result<Vec<Option<BlameLine>>> {
        let path = path.to_string_lossy();
        let mut args = vec!["blame", "--porcelain"];
        if let Some(rev) = revision {
            args.push(rev);
        }
        args.extend(["--", &path]);
        let output = run_git_command(&self.info().root_path, &args)?;
        Ok(parse_blame_porcelain(&output))
    }
}

/// `git blame --porcelain`: each group starts with `<sha> <orig> <final>
/// [<n>]`, carries `author` / `author-time` the first time a sha appears,
/// and ends with a tab-prefixed content line. Uncommitted lines blame to the
/// all-zero sha and are reported as `None`.
fn parse_blame_porcelain(output: &str) -> Vec<Option<BlameLine>> {
    let mut authors: HashMap<String, (String, DateTime<Utc>)> = HashMap::new();
    let mut lines: Vec<Option<BlameLine>> = Vec::new();
    let mut current: Option<(String, usize)> = None;
    let mut pending_author: Option<String> = None;
    let mut pending_time: Option<DateTime<Utc>> = None;
    for line in output.lines() {
        if let Some(content) = line.strip_prefix('\t') {
            let _ = content;
            if let Some((sha, final_line)) = current.take() {
                if let (Some(author), Some(time)) = (pending_author.take(), pending_time.take()) {
                    authors.insert(sha.clone(), (author, time));
                }
                if lines.len() < final_line {
                    lines.resize(final_line, None);
                }
                let entry = if sha.chars().all(|c| c == '0') {
                    None
                } else {
                    authors.get(&sha).map(|(author, time)| BlameLine {
                        sha: sha.clone(),
                        author: author.clone(),
                        time: *time,
                    })
                };
                if final_line >= 1 {
                    lines[final_line - 1] = entry;
                }
            }
            continue;
        }
        if current.is_none() {
            let mut fields = line.split(' ');
            if let (Some(sha), Some(_orig), Some(final_line)) =
                (fields.next(), fields.next(), fields.next())
                && sha.len() >= 40
                && let Ok(final_line) = final_line.parse::<usize>()
            {
                current = Some((sha.to_string(), final_line));
            }
            continue;
        }
        if let Some(author) = line.strip_prefix("author ") {
            pending_author = Some(author.to_string());
        } else if let Some(ts) = line.strip_prefix("author-time ")
            && let Ok(ts) = ts.trim().parse::<i64>()
        {
            pending_time = Utc.timestamp_opt(ts, 0).single();
        }
    }
    lines
}

/// What a todo-driven rebase does to its target commit.
enum TodoAction<'a> {
    Reword(&'a str),
    Drop,
    Fixup,
}

/// Rewrite the first-parent history from `target` to HEAD with git's own
/// interactive rebase, feeding it a prepared todo list instead of an
/// editor: every commit is `pick` except `target`, which is reworded,
/// dropped, or fixed up into the commit before it. Refuses merge commits in
/// the range, since a linear todo cannot express them. A conflict stop is
/// left in place for the merge view; `git rebase --abort` backs out.
fn rebase_with_todo(root: &Path, target: &str, action: TodoAction<'_>) -> Result<()> {
    // The todo starts one commit earlier for a fixup, so its parent is
    // listed to be squashed into.
    let first = match action {
        TodoAction::Fixup => run_git_command(root, &["rev-parse", &format!("{target}^")])?
            .trim()
            .to_string(),
        _ => target.to_string(),
    };
    let (base_args, range): (Vec<String>, String) = match run_git_command(
        root,
        &["rev-parse", "--verify", "--quiet", &format!("{first}^")],
    ) {
        Ok(base) => {
            let base = base.trim().to_string();
            (vec![base.clone()], format!("{base}..HEAD"))
        }
        Err(_) => (vec!["--root".to_string()], "HEAD".to_string()),
    };
    let merges = run_git_command(root, &["rev-list", "--merges", &range])?;
    if !merges.trim().is_empty() {
        return Err(TuicrError::VcsCommand(
            "The history above that commit contains merges; rewrite it by hand".into(),
        ));
    }
    let commits = run_git_command(root, &["rev-list", "--reverse", &range])?;
    let mut todo = String::new();
    let mut seen = false;
    for sha in commits.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let verb = if sha == target {
            seen = true;
            match action {
                TodoAction::Reword(_) => "reword",
                TodoAction::Drop => "drop",
                TodoAction::Fixup => "fixup",
            }
        } else {
            "pick"
        };
        todo.push_str(&format!("{verb} {sha}\n"));
    }
    if !seen {
        return Err(TuicrError::VcsCommand(
            "That commit is not in the current branch's history".into(),
        ));
    }
    let scratch = tempfile::tempdir()
        .map_err(|e| TuicrError::VcsCommand(format!("Failed to create a temp dir: {e}")))?;
    let todo_path = scratch.path().join("todo");
    std::fs::write(&todo_path, todo)
        .map_err(|e| TuicrError::VcsCommand(format!("Failed to write the rebase todo: {e}")))?;
    let mut envs: Vec<(&str, String)> = vec![
        (
            "GIT_SEQUENCE_EDITOR",
            format!("cp '{}'", todo_path.display()),
        ),
        ("GIT_EDITOR", "true".to_string()),
    ];
    if let TodoAction::Reword(message) = action {
        let message_path = scratch.path().join("message");
        std::fs::write(&message_path, message)
            .map_err(|e| TuicrError::VcsCommand(format!("Failed to write the message: {e}")))?;
        envs[1] = ("GIT_EDITOR", format!("cp '{}'", message_path.display()));
    }
    let mut args: Vec<String> = vec!["rebase".to_string(), "-i".to_string()];
    args.extend(base_args);
    run_git_env(root, &args, &envs).map(|_| ())
}

/// `git rebase -i` driven by a prepared todo. Rewords get their messages
/// from a queue: git opens the editor once per `reword` and `squash`, in
/// todo order, and a small script hands each reword its message (squash
/// stops keep git's combined message).
fn rebase_with_steps(root: &Path, base: Option<&str>, steps: &[RebaseStep]) -> Result<()> {
    if steps.is_empty() {
        return Err(TuicrError::VcsCommand("Nothing to rebase".into()));
    }
    if steps[0].action.melds() {
        return Err(TuicrError::VcsCommand(
            "The first commit has nothing above it to squash or fix up into".into(),
        ));
    }
    let scratch = tempfile::tempdir()
        .map_err(|e| TuicrError::VcsCommand(format!("Failed to create a temp dir: {e}")))?;
    let io_err = |e: std::io::Error| TuicrError::VcsCommand(format!("Failed to write: {e}"));
    let mut todo = String::new();
    let mut editor_calls = 0usize;
    for step in steps {
        todo.push_str(&format!("{} {}\n", step.action.verb(), step.id));
        match step.action {
            RebaseAction::Reword => {
                editor_calls += 1;
                let message = step.message.as_deref().unwrap_or_default();
                std::fs::write(scratch.path().join(format!("msg.{editor_calls}")), message)
                    .map_err(io_err)?;
            }
            RebaseAction::Squash => editor_calls += 1,
            _ => {}
        }
    }
    let todo_path = scratch.path().join("todo");
    std::fs::write(&todo_path, todo).map_err(io_err)?;
    let editor_path = scratch.path().join("editor.sh");
    let dir = scratch.path().display().to_string();
    std::fs::write(
        &editor_path,
        format!(
            "#!/bin/sh\nn=$(cat '{dir}/count' 2>/dev/null || echo 0)\nn=$((n + 1))\n\
             echo \"$n\" > '{dir}/count'\n\
             if [ -f '{dir}/msg.'\"$n\" ]; then cp '{dir}/msg.'\"$n\" \"$1\"; fi\n"
        ),
    )
    .map_err(io_err)?;
    let envs: Vec<(&str, String)> = vec![
        (
            "GIT_SEQUENCE_EDITOR",
            format!("cp '{}'", todo_path.display()),
        ),
        ("GIT_EDITOR", format!("sh '{}'", editor_path.display())),
    ];
    let mut args: Vec<String> = vec!["rebase".to_string(), "-i".to_string()];
    match base {
        Some(base) => args.push(base.to_string()),
        None => args.push("--root".to_string()),
    }
    run_git_env(root, &args, &envs).map(|_| ())
}

/// Run git with extra environment variables (editor overrides).
fn run_git_env(workdir: &Path, args: &[String], envs: &[(&str, String)]) -> Result<String> {
    let mut command = std::process::Command::new("git");
    command.current_dir(workdir).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let started = std::time::Instant::now();
    let output = command.output().map_err(|e| {
        crate::git_console::record(args, None, false, started.elapsed(), &e.to_string());
        TuicrError::VcsCommand(format!("Failed to run git: {e}"))
    })?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // Network commands report progress and results on stderr.
    let shown = if stdout.trim().is_empty() {
        &stderr
    } else {
        &stdout
    };
    crate::git_console::record(
        args,
        output.status.code(),
        output.status.success(),
        started.elapsed(),
        shown,
    );
    if !output.status.success() {
        return Err(TuicrError::VcsCommand(if stderr.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            stderr
        }));
    }
    Ok(stdout)
}

/// Environment for network git calls made from under the TUI: never
/// prompt for HTTP credentials, and run ssh in batch mode so a missing key
/// or passphrase fails instead of asking. The ssh override is skipped when
/// the user has their own `GIT_SSH_COMMAND` / `GIT_SSH` / `core.sshCommand`,
/// since that may carry the identity they need.
fn non_interactive_git_env(root: &Path) -> Vec<(&'static str, String)> {
    let mut envs = vec![("GIT_TERMINAL_PROMPT", "0".to_string())];
    let custom_ssh = std::env::var_os("GIT_SSH_COMMAND").is_some()
        || std::env::var_os("GIT_SSH").is_some()
        || run_git_command(root, &["config", "--get", "core.sshCommand"])
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
    if !custom_ssh {
        envs.push(("GIT_SSH_COMMAND", "ssh -o BatchMode=yes".to_string()));
    }
    envs
}

/// Remote-tracking refs and their tips, `origin/HEAD` aliases excluded.
fn remote_ref_tips(root: &Path) -> Result<HashMap<String, String>> {
    let output = run_git_command(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)",
            "refs/remotes",
        ],
    )?;
    Ok(output
        .lines()
        .filter_map(|line| line.split_once('\0'))
        .filter(|(name, _)| !name.ends_with("/HEAD"))
        .map(|(name, oid)| (name.to_string(), oid.to_string()))
        .collect())
}

/// `Fetched: origin/main +3, origin/dev new · 1 pruned`, or `up to date`.
fn describe_fetch(
    root: &Path,
    before: &HashMap<String, String>,
    after: &HashMap<String, String>,
) -> String {
    let mut moved: Vec<String> = Vec::new();
    let mut names: Vec<&String> = after.keys().collect();
    names.sort();
    for name in names {
        let now = &after[name];
        match before.get(name) {
            None => moved.push(format!("{name} new")),
            Some(old) if old != now => {
                let count =
                    run_git_command(root, &["rev-list", "--count", &format!("{old}..{now}")])
                        .map(|n| n.trim().to_string())
                        .ok()
                        .filter(|n| n != "0");
                match count {
                    Some(n) => moved.push(format!("{name} +{n}")),
                    None => moved.push(format!("{name} moved")),
                }
            }
            Some(_) => {}
        }
    }
    let pruned = before
        .keys()
        .filter(|name| !after.contains_key(*name))
        .count();
    if moved.is_empty() && pruned == 0 {
        return "Fetched: everything up to date".to_string();
    }
    let shown = moved.len().min(5);
    let mut summary = format!("Fetched: {}", moved[..shown].join(", "));
    if moved.len() > shown {
        summary.push_str(&format!(" and {} more", moved.len() - shown));
    }
    if moved.is_empty() {
        summary = "Fetched".to_string();
    }
    if pruned > 0 {
        summary.push_str(&format!(" \u{00b7} {pruned} pruned"));
    }
    summary
}

/// Run git with `input` on stdin (patches for `git apply`).
fn run_git_stdin(workdir: &Path, args: &[&str], input: &str) -> Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .current_dir(workdir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| TuicrError::VcsCommand(format!("Failed to run git: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| TuicrError::VcsCommand(format!("Failed to feed git: {e}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| TuicrError::VcsCommand(format!("Failed to wait for git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(TuicrError::VcsCommand(if stderr.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            stderr
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The unified diff of `path` from the scope's base, cut down to the file
/// header plus the one `@@` block whose starts are (`old_start`,
/// `new_start`): a patch `git apply` accepts for exactly that hunk.
fn extract_hunk_patch(
    root: &Path,
    path: &Path,
    scope: HunkScope,
    old_start: u32,
    new_start: u32,
) -> Result<String> {
    let path = path.to_string_lossy();
    let mut args = vec!["diff", "--no-color", "--no-ext-diff"];
    match scope {
        HunkScope::WorkingTree => args.push("HEAD"),
        HunkScope::Staged => args.push("--cached"),
        HunkScope::Unstaged => {}
    }
    args.extend(["--", &path]);
    let diff = run_git_command(root, &args)?;

    #[derive(PartialEq)]
    enum State {
        Header,
        OtherHunk,
        Target,
    }
    let mut header = String::new();
    let mut target = String::new();
    let mut state = State::Header;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@") {
            if state == State::Target {
                break;
            }
            state = if parse_hunk_header_starts(line) == Some((old_start, new_start)) {
                State::Target
            } else {
                State::OtherHunk
            };
        }
        match state {
            State::Header => header.push_str(line),
            State::Target => target.push_str(line),
            State::OtherHunk => {}
        }
    }
    if target.is_empty() {
        return Err(TuicrError::VcsCommand(format!(
            "hunk @@ -{old_start} +{new_start} @@ not found in {path} (untracked files can only be staged whole; if the file changed, `:e` and retry)"
        )));
    }
    Ok(header + &target)
}

/// Cut a one-hunk patch down to the picked lines. Applied forward (stage),
/// an unpicked deletion stays as context and an unpicked addition is left
/// out; applied in reverse (unstage, `-R`), the roles swap, since the
/// index already holds the additions. Header counts are recomputed.
fn filter_patch_lines(patch: &str, pick: &LinePick, reverse: bool) -> Result<String> {
    let Some(hunk_at) = patch.find("\n@@").map(|i| i + 1) else {
        return Err(TuicrError::VcsCommand("No hunk in the patch".into()));
    };
    let (header, hunk) = patch.split_at(hunk_at);
    let mut lines = hunk.split_inclusive('\n');
    let hunk_header = lines.next().unwrap_or_default();
    let (old_start, new_start) = parse_hunk_header_starts(hunk_header)
        .ok_or_else(|| TuicrError::VcsCommand("Malformed hunk header".into()))?;
    let trailer = hunk_header
        .splitn(4, ' ')
        .nth(3)
        .and_then(|rest| rest.split_once("@@"))
        .map(|(_, after)| after.to_string())
        .unwrap_or_else(|| "\n".to_string());

    let (mut old_ln, mut new_ln) = (old_start, new_start);
    let mut body = String::new();
    let (mut old_count, mut new_count, mut changes) = (0u32, 0u32, 0usize);
    let mut last_kept = true;
    for line in lines {
        let (tag, rest) = line.split_at(line.len().min(1));
        match tag {
            "-" => {
                let picked = pick.old.contains(&old_ln);
                old_ln += 1;
                if picked {
                    body.push_str(line);
                    old_count += 1;
                    changes += 1;
                    last_kept = true;
                } else if reverse {
                    last_kept = false;
                } else {
                    body.push(' ');
                    body.push_str(rest);
                    old_count += 1;
                    new_count += 1;
                    last_kept = true;
                }
            }
            "+" => {
                let picked = pick.new.contains(&new_ln);
                new_ln += 1;
                if picked {
                    body.push_str(line);
                    new_count += 1;
                    changes += 1;
                    last_kept = true;
                } else if reverse {
                    body.push(' ');
                    body.push_str(rest);
                    old_count += 1;
                    new_count += 1;
                    last_kept = true;
                } else {
                    last_kept = false;
                }
            }
            "\\" => {
                if last_kept {
                    body.push_str(line);
                }
            }
            _ => {
                old_ln += 1;
                new_ln += 1;
                body.push_str(line);
                old_count += 1;
                new_count += 1;
                last_kept = true;
            }
        }
    }
    if changes == 0 {
        return Err(TuicrError::VcsCommand(
            "No changed lines in the selection".into(),
        ));
    }
    Ok(format!(
        "{header}@@ -{old_start},{old_count} +{new_start},{new_count} @@{trailer}{body}"
    ))
}

/// `@@ -12,3 +14,5 @@ ...` → `(12, 14)`.
fn parse_hunk_header_starts(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace();
    parts.next()?; // "@@"
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |spec: &str| spec.split(',').next()?.parse::<u32>().ok();
    Some((start(old)?, start(new)?))
}

/// Parse `for-each-ref` output over `refs/heads` and `refs/remotes` (see
/// [`GitBackend::list_branches`] for the format). `origin/HEAD` aliases are
/// dropped; local branches come first, each group sorted by name.
fn parse_branch_list(output: &str) -> Vec<BranchInfo> {
    let mut branches: Vec<BranchInfo> = output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let full = fields.next()?;
            let name = fields.next()?.to_string();
            let tip_id = fields.next()?.to_string();
            let upstream = fields.next().unwrap_or("").to_string();
            let track = fields.next().unwrap_or("");
            let head = fields.next().unwrap_or("");
            let is_remote = full.starts_with("refs/remotes/");
            if is_remote && name.ends_with("/HEAD") {
                return None;
            }
            let (ahead, behind) = parse_track_counts(track);
            Some(BranchInfo {
                name,
                tip_id,
                is_current: head == "*",
                is_remote,
                upstream: (!upstream.is_empty()).then_some(upstream),
                ahead,
                behind,
                upstream_gone: track.contains("gone"),
            })
        })
        .collect();
    branches.sort_by(|a, b| {
        a.is_remote
            .cmp(&b.is_remote)
            .then_with(|| a.name.cmp(&b.name))
    });
    branches
}

/// `[ahead 1, behind 2]`, `[ahead 3]`, `[behind 1]`, `[gone]` or empty.
fn parse_track_counts(track: &str) -> (usize, usize) {
    let mut ahead = 0;
    let mut behind = 0;
    for part in track.trim_matches(|c| c == '[' || c == ']').split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            ahead = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            behind = n.trim().parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

impl GitBackend {
    fn squash_commits_inner(&self, commit_ids: &[String]) -> Result<()> {
        if commit_ids.len() < 2 {
            return Err(TuicrError::VcsCommand(
                "Select at least two commits to squash".into(),
            ));
        }
        let root = &self.info().root_path;

        // Refuse when the index already has staged changes: `reset --soft`
        // would silently fold them into the squash commit.
        if run_git_command(root, &["diff", "--cached", "--quiet"]).is_err() {
            return Err(TuicrError::VcsCommand(
                "The index has staged changes; commit or unstage them first".into(),
            ));
        }

        // The selection must be exactly HEAD's first-parent chain so the
        // rewrite is a plain soft-reset + commit (no history surgery on
        // commits below unrelated work).
        let chain = run_git_command(
            root,
            &[
                "rev-list",
                "--first-parent",
                &format!("--max-count={}", commit_ids.len()),
                "HEAD",
            ],
        )?;
        let chain: Vec<&str> = chain.split_whitespace().collect();
        let newest_first: Vec<&str> = commit_ids.iter().rev().map(String::as_str).collect();
        if chain != newest_first {
            return Err(TuicrError::VcsCommand(
                "Squash needs a contiguous range of commits ending at HEAD".into(),
            ));
        }

        let oldest = &commit_ids[0];
        if run_git_command(
            root,
            &["rev-parse", "--verify", "--quiet", &format!("{oldest}^")],
        )
        .is_err()
        {
            return Err(TuicrError::VcsCommand(
                "Cannot squash through the root commit".into(),
            ));
        }

        // Combined message: every selected message, oldest first, the way
        // an interactive-rebase squash would assemble it.
        let mut message = String::new();
        for id in commit_ids {
            let body = run_git_command(root, &["log", "-1", "--format=%B", id])?;
            if !message.is_empty() {
                message.push('\n');
            }
            message.push_str(body.trim_end());
            message.push('\n');
        }

        let original_head = run_git_command(root, &["rev-parse", "HEAD"])?
            .trim()
            .to_string();
        run_git_command(root, &["reset", "--soft", &format!("{oldest}^")])?;
        if let Err(err) =
            run_git_command(root, &["commit", "--allow-empty", "-m", message.trim_end()])
        {
            // Put the branch back where it was; the tree is untouched.
            let _ = run_git_command(root, &["reset", "--soft", &original_head]);
            return Err(err);
        }
        Ok(())
    }

    fn abort_merge_operation_inner(&self) -> Result<String> {
        let root = &self.info().root_path;
        let in_progress = |head: &str| {
            run_git_command(root, &["rev-parse", "--verify", "--quiet", head])
                .map(|out| !out.trim().is_empty())
                .unwrap_or(false)
        };
        let git_dir = run_git_command(root, &["rev-parse", "--git-dir"])
            .map(|out| root.join(out.trim()))
            .unwrap_or_else(|_| root.join(".git"));
        let rebasing =
            git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir();
        let op = if in_progress("MERGE_HEAD") {
            "merge"
        } else if rebasing || in_progress("REBASE_HEAD") {
            "rebase"
        } else if in_progress("CHERRY_PICK_HEAD") {
            "cherry-pick"
        } else if in_progress("REVERT_HEAD") {
            "revert"
        } else {
            return Err(TuicrError::VcsCommand(
                "No merge operation in progress".into(),
            ));
        };
        run_git_command(root, &[op, "--abort"])?;
        Ok(op.to_string())
    }

    fn continue_merge_operation_inner(&self) -> Result<String> {
        let root = &self.info().root_path;
        let in_progress = |head: &str| {
            run_git_command(root, &["rev-parse", "--verify", "--quiet", head])
                .map(|out| !out.trim().is_empty())
                .unwrap_or(false)
        };
        if in_progress("MERGE_HEAD") {
            run_git_command(root, &["commit", "--no-edit"])?;
            return Ok("merge".to_string());
        }
        // `core.editor=true` accepts the prepared commit message without
        // opening $EDITOR inside the TUI.
        for (head, op) in [
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("REBASE_HEAD", "rebase"),
            ("REVERT_HEAD", "revert"),
        ] {
            if in_progress(head) {
                run_git_command(root, &["-c", "core.editor=true", op, "--continue"])?;
                return Ok(op.to_string());
            }
        }
        Err(TuicrError::VcsCommand(
            "No merge operation in progress".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{resolve_remote_repository, traits::ForgeRepository};
    use git2::Repository;
    use std::fs;
    use tempfile::tempdir;

    fn init_repo_with_origin(url: &str) -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.remote("origin", url).unwrap();
        dir
    }

    #[test]
    fn resolves_named_remote_fetch_url_from_subdirectory() {
        let dir = init_repo_with_origin("https://github.com/owner/repo");
        let repo = Repository::open(dir.path()).unwrap();
        repo.remote("staging-upstream", "git@github.com:org/repo-staging.git")
            .unwrap();
        repo.remote_set_pushurl(
            "staging-upstream",
            Some("https://github.com/contributor/repo-staging.git"),
        )
        .unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();

        for preference in [GitBackendPreference::Libgit2, GitBackendPreference::Cli] {
            let backend =
                GitBackend::discover_from(&nested, preference, DiffWhitespaceMode::Normal).unwrap();
            assert_eq!(
                resolve_remote_repository(&backend, "staging-upstream").unwrap(),
                ForgeRepository::github("github.com", "org", "repo-staging")
            );
        }
    }

    #[test]
    fn resolves_remote_url_rewrites_without_using_push_rewrites() {
        let dir = init_repo_with_origin("https://github.com/owner/repo");
        let repo = Repository::open(dir.path()).unwrap();
        repo.remote("staging", "tuicr-test:group/repo.git").unwrap();
        let mut config = repo.config().unwrap();
        config
            .set_str("url.https://gitlab.com/.insteadOf", "tuicr-test:")
            .unwrap();
        config
            .set_str("url.https://github.com/.pushInsteadOf", "tuicr-test:")
            .unwrap();
        let backend = GitBackend::discover_from(
            dir.path(),
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .unwrap();

        assert_eq!(
            resolve_remote_repository(&backend, "staging").unwrap(),
            ForgeRepository::gitlab("gitlab.com", "group", "repo")
        );
    }

    #[test]
    fn rejects_unusable_named_remotes_instead_of_falling_back_to_origin() {
        let dir = init_repo_with_origin("https://github.com/owner/repo");
        let repo = Repository::open(dir.path()).unwrap();
        repo.remote("invalid", "not-a-url").unwrap();
        let backend = GitBackend::discover_from(
            dir.path(),
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .unwrap();
        for name in ["missing", "invalid", "--all"] {
            assert!(matches!(
                resolve_remote_repository(&backend, name),
                Err(TuicrError::Forge(_))
            ));
        }
    }

    #[test]
    fn derives_git_repo_mode_from_config() {
        assert_eq!(GitRepoMode::from_config(""), GitRepoMode::Standard);
        assert_eq!(
            GitRepoMode::from_config("core.sparsecheckout true\n"),
            GitRepoMode::SparseCheckout
        );
        assert_eq!(
            GitRepoMode::from_config("core.sparsecheckout true\nindex.sparse true\n"),
            GitRepoMode::SparseIndex
        );
    }

    #[test]
    fn derives_backend_preference_from_config() {
        assert_eq!(
            GitBackendPreference::from_config(None),
            GitBackendPreference::Libgit2
        );
        assert_eq!(
            GitBackendPreference::from_config(Some("libgit2")),
            GitBackendPreference::Libgit2
        );
        assert_eq!(
            GitBackendPreference::from_config(Some("cli")),
            GitBackendPreference::Cli
        );
    }

    #[test]
    fn default_preference_routes_sparse_index_repo_to_cli_with_warning() {
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        run_git_command(
            root,
            &["sparse-checkout", "init", "--cone", "--sparse-index"],
        )
        .expect("failed to enable sparse checkout");
        run_git_command(root, &["sparse-checkout", "set", "src"])
            .expect("failed to set sparse checkout paths");

        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");

        match backend {
            GitBackend::Cli(backend) => {
                assert!(backend.supports_sparse_checkout());
                assert_eq!(
                    backend.startup_warnings().first().map(String::as_str),
                    Some("Sparse checkout detected; using Git CLI backend.")
                );
            }
            GitBackend::Libgit2(_) => panic!("sparse-index repo should use Git CLI backend"),
        }
    }

    #[test]
    fn default_preference_keeps_standard_repo_on_libgit2() {
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);

        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");

        match backend {
            GitBackend::Libgit2(backend) => assert!(!backend.supports_sparse_checkout()),
            GitBackend::Cli(_) => panic!("standard repo should use libgit2 by default"),
        }
    }

    #[test]
    fn default_preference_routes_reftable_repo_to_cli() {
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        run_git_command(root, &["config", "core.repositoryFormatVersion", "1"])
            .expect("failed to set repositoryFormatVersion");
        run_git_command(root, &["config", "extensions.refStorage", "reftable"])
            .expect("failed to set reftable extension");

        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("reftable repo should open via CLI fallback");

        assert!(
            matches!(backend, GitBackend::Cli(_)),
            "reftable repo should use Git CLI backend, not libgit2"
        );
    }

    #[test]
    fn default_preference_routes_split_index_repo_to_cli() {
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        run_git_command(root, &["config", "core.splitIndex", "true"])
            .expect("failed to set splitIndex");

        let backend: GitBackend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("split-index repo should open via CLI fallback");

        assert!(
            matches!(backend, GitBackend::Cli(_)),
            "split-index repo should use Git CLI backend, not libgit2"
        );
    }

    fn git_id(root: &Path, rev: &str) -> String {
        run_git_command(root, &["rev-parse", rev])
            .expect("failed to resolve revision")
            .trim()
            .to_string()
    }

    #[test]
    fn cherry_pick_applies_clean_commit_and_stops_on_conflict() {
        // given main with an extra commit, plus two branches off the base:
        // one adding a new file (clean pick) and one editing the same line
        // main later changed (conflicting pick)
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        run_git_command(root, &["checkout", "-b", "trunk"]).expect("failed to branch trunk");
        run_git_command(root, &["branch", "clean-pick"]).expect("failed to branch");
        run_git_command(root, &["branch", "conflicting-pick"]).expect("failed to branch");

        fs::write(root.join("src/file.txt"), "main version\n").expect("failed to write");
        run_git_command(root, &["commit", "-am", "main change"]).expect("failed to commit");

        run_git_command(root, &["checkout", "clean-pick"]).expect("failed to checkout");
        fs::write(root.join("src/new.txt"), "new\n").expect("failed to write");
        run_git_command(root, &["add", "src/new.txt"]).expect("failed to add");
        run_git_command(root, &["commit", "-m", "add new file"]).expect("failed to commit");
        let clean_id = git_id(root, "HEAD");

        run_git_command(root, &["checkout", "conflicting-pick"]).expect("failed to checkout");
        fs::write(root.join("src/file.txt"), "pick version\n").expect("failed to write");
        run_git_command(root, &["commit", "-am", "pick change"]).expect("failed to commit");
        let conflict_id = git_id(root, "HEAD");

        run_git_command(root, &["checkout", "trunk"]).expect("failed to checkout trunk");
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");

        // when / then: a clean pick lands on the current branch AS A
        // COMMIT (not just staged changes), and the libgit2 index cache
        // is refreshed so no phantom staged/unstaged state remains
        let head_before = git_id(root, "HEAD");
        backend
            .cherry_pick(&[clean_id])
            .expect("clean cherry-pick should succeed");
        assert!(root.join("src/new.txt").exists());
        assert_ne!(git_id(root, "HEAD"), head_before, "pick must commit");
        let head_summary =
            run_git_command(root, &["log", "-1", "--format=%s"]).expect("failed to read summary");
        assert_eq!(head_summary.trim(), "add new file");
        let highlighter = crate::syntax::SyntaxHighlighter::default();
        assert!(
            matches!(
                backend.get_staged_diff(&highlighter),
                Err(TuicrError::NoChanges)
            ),
            "no staged changes should remain after a clean pick"
        );
        assert!(
            matches!(
                backend.get_unstaged_diff(&highlighter),
                Err(TuicrError::NoChanges)
            ),
            "no unstaged changes should remain after a clean pick"
        );

        // and a conflicting pick fails, leaving conflict state behind
        let err = backend.cherry_pick(std::slice::from_ref(&conflict_id));
        assert!(err.is_err(), "conflicting cherry-pick should fail");
        let info = backend
            .get_merge_conflicts()
            .expect("conflict probe failed")
            .expect("conflict state should be reported");
        assert_eq!(info.paths, vec![PathBuf::from("src/file.txt")]);
        assert_eq!(info.theirs_label, "conflicting-pick");
        run_git_command(root, &["cherry-pick", "--abort"]).expect("failed to abort");
    }

    #[test]
    fn rebase_onto_replays_current_branch() {
        // given feature branched before main's latest commit
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        run_git_command(root, &["checkout", "-b", "feature"]).expect("failed to branch");
        fs::write(root.join("src/feature.txt"), "feature\n").expect("failed to write");
        run_git_command(root, &["add", "src/feature.txt"]).expect("failed to add");
        run_git_command(root, &["commit", "-m", "feature work"]).expect("failed to commit");

        run_git_command(root, &["checkout", "-"]).expect("failed to checkout main");
        fs::write(root.join("src/main.txt"), "main\n").expect("failed to write");
        run_git_command(root, &["add", "src/main.txt"]).expect("failed to add");
        run_git_command(root, &["commit", "-m", "main advance"]).expect("failed to commit");
        let main_tip = git_id(root, "HEAD");

        run_git_command(root, &["checkout", "feature"]).expect("failed to checkout feature");
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");

        // when
        backend
            .rebase_onto(&main_tip)
            .expect("rebase should succeed");

        // then feature's commit now sits on top of main's tip
        assert_eq!(git_id(root, "HEAD~1"), main_tip);
    }

    #[test]
    fn continue_merge_operation_commits_resolved_merge() {
        // given a merge stopped on a conflict, then resolved and staged
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        run_git_command(root, &["checkout", "-b", "trunk"]).expect("failed to branch trunk");
        run_git_command(root, &["branch", "other"]).expect("failed to branch");

        fs::write(root.join("src/file.txt"), "trunk version\n").expect("failed to write");
        run_git_command(root, &["commit", "-am", "trunk change"]).expect("failed to commit");

        run_git_command(root, &["checkout", "other"]).expect("failed to checkout");
        fs::write(root.join("src/file.txt"), "other version\n").expect("failed to write");
        run_git_command(root, &["commit", "-am", "other change"]).expect("failed to commit");

        run_git_command(root, &["checkout", "trunk"]).expect("failed to checkout trunk");
        let merge = std::process::Command::new("git")
            .current_dir(root)
            .args(["merge", "other"])
            .output()
            .expect("failed to run git merge");
        assert!(!merge.status.success(), "merge should stop on conflicts");

        fs::write(root.join("src/file.txt"), "resolved version\n").expect("failed to write");
        run_git_command(root, &["add", "src/file.txt"]).expect("failed to stage");

        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");

        // when
        let op = backend
            .continue_merge_operation()
            .expect("continuing the merge should succeed");

        // then the merge commit landed and the merge state is gone
        assert_eq!(op, "merge");
        assert!(
            run_git_command(root, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"]).is_err(),
            "MERGE_HEAD should be gone after committing"
        );
        run_git_command(root, &["rev-parse", "HEAD^2"])
            .expect("merge commit should have a second parent");
    }

    #[test]
    fn continue_merge_operation_without_state_errors() {
        // given a clean repository
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");
        // when / then
        assert!(backend.continue_merge_operation().is_err());
    }

    #[test]
    fn squash_combines_head_range_into_one_commit() {
        // given three commits on the current branch
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        fs::write(root.join("src/file.txt"), "two\n").expect("failed to write");
        run_git_command(root, &["commit", "-am", "second"]).expect("failed to commit");
        fs::write(root.join("src/file.txt"), "three\n").expect("failed to write");
        run_git_command(root, &["commit", "-am", "third"]).expect("failed to commit");

        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");
        let second = git_id(root, "HEAD~1");
        let third = git_id(root, "HEAD");

        // when squashing the top two (oldest first)
        backend
            .squash_commits(&[second, third])
            .expect("squash should succeed");

        // then one commit replaces them, content intact, messages combined
        let count = run_git_command(root, &["rev-list", "--count", "HEAD"])
            .expect("failed to count")
            .trim()
            .to_string();
        assert_eq!(count, "2");
        let message =
            run_git_command(root, &["log", "-1", "--format=%B"]).expect("failed to read message");
        assert!(message.contains("second"), "got: {message:?}");
        assert!(message.contains("third"), "got: {message:?}");
        assert_eq!(
            fs::read_to_string(root.join("src/file.txt")).expect("read"),
            "three\n"
        );
    }

    #[test]
    fn squash_rejects_ranges_not_ending_at_head() {
        // given three commits
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        fs::write(root.join("src/file.txt"), "two\n").expect("failed to write");
        run_git_command(root, &["commit", "-am", "second"]).expect("failed to commit");
        fs::write(root.join("src/file.txt"), "three\n").expect("failed to write");
        run_git_command(root, &["commit", "-am", "third"]).expect("failed to commit");

        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");
        let first = git_id(root, "HEAD~2");
        let second = git_id(root, "HEAD~1");

        // when / then: a mid-history range is refused, history untouched
        assert!(backend.squash_commits(&[first, second]).is_err());
        let count = run_git_command(root, &["rev-list", "--count", "HEAD"])
            .expect("failed to count")
            .trim()
            .to_string();
        assert_eq!(count, "3");
    }

    #[test]
    fn squash_rejects_dirty_index() {
        // given two commits and a staged change
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        fs::write(root.join("src/file.txt"), "two\n").expect("failed to write");
        run_git_command(root, &["commit", "-am", "second"]).expect("failed to commit");
        fs::write(root.join("src/file.txt"), "staged\n").expect("failed to write");
        run_git_command(root, &["add", "src/file.txt"]).expect("failed to stage");

        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");
        let first = git_id(root, "HEAD~1");
        let second = git_id(root, "HEAD");

        // when / then
        let err = backend.squash_commits(&[first, second]);
        assert!(err.is_err(), "staged changes must block squash");
    }

    #[test]
    fn parse_branch_list_classifies_rows() {
        let output = "refs/heads/main\0main\0aaa\0origin/main\0[ahead 1, behind 2]\0*\n\
                      refs/heads/dev\0dev\0bbb\0\0\0 \n\
                      refs/remotes/origin/HEAD\0origin/HEAD\0aaa\0\0\0 \n\
                      refs/remotes/origin/main\0origin/main\0aaa\0\0\0 \n\
                      refs/heads/old\0old\0ccc\0origin/old\0[gone]\0 \n";
        let branches = parse_branch_list(output);
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        // locals first (by name), then remotes; origin/HEAD dropped
        assert_eq!(names, ["dev", "main", "old", "origin/main"]);
        let main = &branches[1];
        assert!(main.is_current);
        assert!(!main.is_remote);
        assert_eq!(main.upstream.as_deref(), Some("origin/main"));
        assert_eq!((main.ahead, main.behind), (1, 2));
        assert!(branches[2].upstream_gone);
        assert!(branches[3].is_remote);
        assert!(!branches[0].is_current);
    }

    #[test]
    fn parse_track_counts_reads_ahead_and_behind() {
        assert_eq!(parse_track_counts(""), (0, 0));
        assert_eq!(parse_track_counts("[ahead 3]"), (3, 0));
        assert_eq!(parse_track_counts("[behind 4]"), (0, 4));
        assert_eq!(parse_track_counts("[ahead 1, behind 2]"), (1, 2));
        assert_eq!(parse_track_counts("[gone]"), (0, 0));
    }

    #[test]
    fn branch_ops_create_checkout_and_stash_round_trip() {
        // given a fresh repo
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");
        let current = |b: &GitBackend| {
            b.list_branches()
                .expect("list branches")
                .into_iter()
                .find(|b| b.is_current)
                .map(|b| b.name)
        };
        let initial = current(&backend).expect("initial branch");
        let start = git_id(root, "HEAD");

        // when a branch is created at HEAD, it is checked out
        backend
            .create_branch("feature", Some(&start))
            .expect("create branch");
        assert_eq!(current(&backend).as_deref(), Some("feature"));

        // and a dirty tree can be stashed around a switch back
        fs::write(root.join("src/file.txt"), "dirty\n").expect("write");
        assert!(is_dirty(root));
        backend.stash_push("tuicr test").expect("stash push");
        assert!(!is_dirty(root));
        backend.checkout(&initial).expect("checkout back");
        assert_eq!(current(&backend).as_deref(), Some(initial.as_str()));
        backend.stash_pop().expect("stash pop");
        assert!(is_dirty(root));

        // and a commit checks out detached
        backend.checkout(&start).expect("detached checkout");
        assert!(
            current(&backend).is_none(),
            "detached HEAD has no current branch"
        );
    }

    #[test]
    fn abort_cherry_pick_restores_head() {
        // given a conflicting cherry-pick stopped on trunk
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        run_git_command(root, &["checkout", "-b", "trunk"]).expect("branch trunk");
        run_git_command(root, &["branch", "pick"]).expect("branch pick");
        fs::write(root.join("src/file.txt"), "trunk version\n").expect("write");
        run_git_command(root, &["commit", "-am", "trunk change"]).expect("commit");
        run_git_command(root, &["checkout", "pick"]).expect("checkout pick");
        fs::write(root.join("src/file.txt"), "pick version\n").expect("write");
        run_git_command(root, &["commit", "-am", "pick change"]).expect("commit");
        let pick_id = git_id(root, "HEAD");
        run_git_command(root, &["checkout", "trunk"]).expect("checkout trunk");
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");
        let head_before = git_id(root, "HEAD");
        assert!(backend.cherry_pick(&[pick_id]).is_err());
        assert!(backend.get_merge_conflicts().expect("probe").is_some());

        // when
        let op = backend.abort_merge_operation().expect("abort");

        // then the operation is gone and HEAD is back where it was
        assert_eq!(op, "cherry-pick");
        assert!(backend.get_merge_conflicts().expect("probe").is_none());
        assert_eq!(git_id(root, "HEAD"), head_before);
        assert!(
            backend.abort_merge_operation().is_err(),
            "nothing left to abort"
        );
    }

    #[test]
    fn hunk_header_starts_parse() {
        assert_eq!(
            parse_hunk_header_starts("@@ -12,3 +14,5 @@ fn x"),
            Some((12, 14))
        );
        assert_eq!(parse_hunk_header_starts("@@ -1 +1 @@"), Some((1, 1)));
        assert_eq!(parse_hunk_header_starts("diff --git a b"), None);
    }

    fn hunk_starts(root: &Path, args: &[&str]) -> Vec<(u32, u32)> {
        run_git_command(root, args)
            .expect("git diff")
            .lines()
            .filter(|l| l.starts_with("@@"))
            .filter_map(parse_hunk_header_starts)
            .collect()
    }

    #[test]
    fn hunk_and_file_operations_stage_unstage_and_discard() {
        // given a twenty-line file edited near the top and near the bottom,
        // so the two edits land in separate hunks
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        let base: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        fs::write(root.join("src/file.txt"), &base).expect("write");
        run_git_command(root, &["commit", "-qam", "twenty lines"]).expect("commit");
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");
        let edited = base
            .replace("line 2\n", "line two\n")
            .replace("line 18\n", "line eighteen\n");
        fs::write(root.join("src/file.txt"), &edited).expect("write");
        let path = Path::new("src/file.txt");
        let staged_diff = || run_git_command(root, &["diff", "--cached"]).expect("diff");

        let worktree_hunks = hunk_starts(root, &["diff", "HEAD", "--", "src/file.txt"]);
        assert_eq!(worktree_hunks.len(), 2, "{worktree_hunks:?}");

        // when the first hunk is staged, only it reaches the index
        let (old_start, new_start) = worktree_hunks[0];
        backend
            .stage_hunk(path, HunkScope::WorkingTree, old_start, new_start)
            .expect("stage hunk");
        assert!(staged_diff().contains("+line two"));
        assert!(!staged_diff().contains("+line eighteen"));

        // and it can be unstaged from the staged diff's coordinates
        let staged_hunks = hunk_starts(root, &["diff", "--cached", "--", "src/file.txt"]);
        assert_eq!(staged_hunks.len(), 1);
        backend
            .unstage_hunk(path, staged_hunks[0].0, staged_hunks[0].1)
            .expect("unstage hunk");
        assert!(staged_diff().trim().is_empty());

        // discarding the second hunk reverts just that edit in the tree
        let unstaged_hunks = hunk_starts(root, &["diff", "--", "src/file.txt"]);
        let (old_start, new_start) = unstaged_hunks[1];
        backend
            .discard_hunk(path, HunkScope::Unstaged, old_start, new_start)
            .expect("discard hunk");
        let now = fs::read_to_string(root.join("src/file.txt")).expect("read");
        assert!(now.contains("line two\n"));
        assert!(now.contains("line 18\n"));

        // a hunk that is not there is an error, not a silent no-op
        assert!(
            backend
                .stage_hunk(path, HunkScope::WorkingTree, 999, 999)
                .is_err()
        );

        // whole-file stage / unstage / discard
        backend.stage_file(path).expect("stage file");
        assert!(!staged_diff().trim().is_empty());
        backend.unstage_file(path).expect("unstage file");
        assert!(staged_diff().trim().is_empty());
        backend.discard_file(path).expect("discard file");
        assert_eq!(
            fs::read_to_string(root.join("src/file.txt")).expect("read"),
            base
        );

        // an untracked file is removed by discard
        fs::write(root.join("src/new.txt"), "new\n").expect("write");
        backend
            .discard_file(Path::new("src/new.txt"))
            .expect("discard untracked");
        assert!(!root.join("src/new.txt").exists());
    }

    #[test]
    fn commit_amend_and_stash_list_round_trip() {
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");
        let file = root.join("src/file.txt");
        let path = Path::new("src/file.txt");

        // commit, then amend the message
        fs::write(&file, "changed\n").expect("write");
        backend.stage_file(path).expect("stage");
        let id = backend
            .commit("first message\n\nbody", false)
            .expect("commit");
        assert!(id.len() >= 7);
        assert!(
            backend
                .head_commit_message()
                .expect("message")
                .starts_with("first message")
        );
        let amended = backend.commit("amended message", true).expect("amend");
        assert_ne!(id, amended);
        assert!(
            backend
                .head_commit_message()
                .expect("message")
                .starts_with("amended message")
        );
        assert_eq!(
            run_git_command(root, &["rev-list", "--count", "HEAD"])
                .expect("count")
                .trim(),
            "2"
        );

        // two stashes: list, apply the older one (kept), drop it, pop the rest
        fs::write(&file, "stashed\n").expect("write");
        backend.stash_push("wip one").expect("stash one");
        fs::write(&file, "stashed two\n").expect("write");
        backend.stash_push("wip two").expect("stash two");
        let list = backend.stash_list().expect("list");
        assert_eq!(list.iter().map(|e| e.index).collect::<Vec<_>>(), [0, 1]);
        assert!(list[0].message.contains("wip two"));
        assert!(list[1].message.contains("wip one"));

        backend.stash_apply(1, false).expect("apply");
        assert_eq!(fs::read_to_string(&file).expect("read"), "stashed\n");
        assert_eq!(backend.stash_list().expect("list").len(), 2);
        backend.discard_file(path).expect("discard");
        backend.stash_drop(1).expect("drop");
        assert_eq!(backend.stash_list().expect("list").len(), 1);
        backend.stash_apply(0, true).expect("pop");
        assert_eq!(fs::read_to_string(&file).expect("read"), "stashed two\n");
        assert!(backend.stash_list().expect("list").is_empty());
    }

    #[test]
    fn blame_porcelain_parses_authors_times_and_uncommitted_lines() {
        let sha_a = "a".repeat(40);
        let sha_b = "b".repeat(40);
        let zero = "0".repeat(40);
        let output = format!(
            "{sha_a} 1 1 2\nauthor Kim\nauthor-mail <k@x>\nauthor-time 1700000000\nfilename f\n\tline one\n\
             {sha_a} 2 2\n\tline two\n\
             {sha_b} 1 3 1\nauthor Lee\nauthor-time 1700100000\nfilename f\n\tline three\n\
             {zero} 4 4 1\nauthor Not Committed Yet\nauthor-time 1700200000\nfilename f\n\tline four\n"
        );
        let lines = parse_blame_porcelain(&output);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].as_ref().map(|l| l.author.as_str()), Some("Kim"));
        assert_eq!(
            lines[1].as_ref().map(|l| l.sha.as_str()),
            Some(sha_a.as_str())
        );
        assert_eq!(lines[2].as_ref().map(|l| l.author.as_str()), Some("Lee"));
        assert_eq!(
            lines[2].as_ref().map(|l| l.time.timestamp()),
            Some(1_700_100_000)
        );
        assert!(lines[3].is_none(), "uncommitted lines carry no blame");
    }

    #[test]
    fn blame_file_reads_the_working_tree_and_a_revision() {
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("failed to discover backend");
        let first = git_id(root, "HEAD");
        fs::write(root.join("src/file.txt"), "one\ntwo\n").expect("write");
        run_git_command(root, &["commit", "-qam", "add two"]).expect("commit");
        fs::write(root.join("src/file.txt"), "one\ntwo\nthree\n").expect("write");

        let tree = backend
            .blame_file(Path::new("src/file.txt"), None)
            .expect("blame");
        assert_eq!(tree.len(), 3);
        assert_eq!(
            tree[0].as_ref().map(|l| l.sha.as_str()),
            Some(first.as_str())
        );
        assert!(tree[1].is_some());
        assert!(tree[2].is_none(), "the unsaved third line has no commit");

        let at_first = backend
            .blame_file(Path::new("src/file.txt"), Some(&first))
            .expect("blame at revision");
        assert_eq!(at_first.len(), 1);
    }

    #[test]
    fn fetch_pull_and_push_through_a_local_bare_remote() {
        // given a bare remote with two clones, A and B
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base = temp_dir.path();
        let remote = base.join("remote.git");
        run_git_command(base, &["init", "--bare", "-b", "main", "remote.git"]).expect("bare init");
        let remote_str = remote.to_string_lossy().into_owned();
        run_git_command(base, &["clone", "-q", &remote_str, "a"]).expect("clone a");
        let a = base.join("a");
        for args in [
            ["config", "user.name", "Tuicr Test"],
            ["config", "user.email", "tuicr@example.com"],
            ["config", "commit.gpgsign", "false"],
        ] {
            run_git_command(&a, &args).expect("config");
        }
        run_git_command(&a, &["checkout", "-q", "-b", "main"]).expect("branch main");
        fs::write(a.join("f.txt"), "one\n").expect("write");
        run_git_command(&a, &["add", "."]).expect("add");
        run_git_command(&a, &["commit", "-qm", "one"]).expect("commit");
        run_git_command(&a, &["push", "-q", "-u", "origin", "main"]).expect("push");
        run_git_command(base, &["clone", "-q", &remote_str, "b"]).expect("clone b");
        let b = base.join("b");
        for args in [
            ["config", "user.name", "Tuicr Test"],
            ["config", "user.email", "tuicr@example.com"],
            ["config", "commit.gpgsign", "false"],
        ] {
            run_git_command(&b, &args).expect("config");
        }
        let backend = |root: &Path| {
            GitBackend::discover_from(
                root,
                GitBackendPreference::Libgit2,
                DiffWhitespaceMode::Normal,
            )
            .expect("backend")
        };

        // when A pushes a second commit, B's fetch reports it and pull takes it
        fs::write(a.join("f.txt"), "one\ntwo\n").expect("write");
        run_git_command(&a, &["commit", "-qam", "two"]).expect("commit");
        assert!(
            backend(&a)
                .push()
                .expect("push")
                .starts_with("Pushed 1 commit")
        );
        let fetched = backend(&b).fetch().expect("fetch");
        assert!(fetched.contains("origin/main +1"), "{fetched}");
        assert_eq!(
            backend(&b).fetch().expect("fetch"),
            "Fetched: everything up to date"
        );
        // a remote that cannot be reached is named, and the others still fetch
        run_git_command(&b, &["remote", "add", "dead", "/nonexistent/repo.git"]).expect("remote");
        let partial = backend(&b).fetch().expect("partial fetch");
        assert!(partial.contains("dead failed"), "{partial}");
        run_git_command(&b, &["remote", "remove", "dead"]).expect("remove");
        let pulled = backend(&b).pull(false).expect("pull");
        assert!(pulled.starts_with("Pulled 1 commit"), "{pulled}");
        assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "one\ntwo\n");
        assert_eq!(backend(&b).pull(false).expect("pull"), "Already up to date");

        // and a push with no upstream sets one
        run_git_command(&b, &["checkout", "-q", "-b", "topic"]).expect("branch");
        fs::write(b.join("t.txt"), "t\n").expect("write");
        run_git_command(&b, &["add", "."]).expect("add");
        run_git_command(&b, &["commit", "-qm", "topic"]).expect("commit");
        let pushed = backend(&b).push().expect("push");
        assert!(pushed.contains("set it as upstream"), "{pushed}");
        assert!(
            backend(&b)
                .push()
                .expect("push")
                .starts_with("Nothing to push")
        );

        // and when the branches diverge, ff-only refuses but rebase pulls
        run_git_command(&b, &["checkout", "-q", "main"]).expect("checkout");
        fs::write(b.join("f.txt"), "one\ntwo\nthree\n").expect("write");
        run_git_command(&b, &["commit", "-qam", "three"]).expect("commit");
        backend(&b).push().expect("push three");
        fs::write(a.join("g.txt"), "g\n").expect("write");
        run_git_command(&a, &["add", "."]).expect("add");
        run_git_command(&a, &["commit", "-qm", "g"]).expect("commit");
        backend(&a).fetch().expect("fetch");
        assert!(
            backend(&a).pull(false).is_err(),
            "diverged: ff-only must refuse"
        );
        let rebased = backend(&a).pull(true).expect("pull --rebase");
        assert!(rebased.contains("rebased onto"), "{rebased}");
        assert!(a.join("g.txt").exists());
        assert_eq!(
            fs::read_to_string(a.join("f.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
    }

    #[test]
    fn stages_and_unstages_only_the_picked_lines() {
        // given a file with three edited lines in one hunk
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        fs::write(root.join("f.txt"), "a\nb\nc\nd\ne\n").expect("write");
        run_git_command(root, &["add", "f.txt"]).expect("add");
        run_git_command(root, &["commit", "-qm", "base"]).expect("commit");
        fs::write(root.join("f.txt"), "a\nB\nc\nD\ne\nf\n").expect("write");
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("backend");
        let staged = || run_git_command(root, &["show", ":f.txt"]).expect("index");

        // when only the b → B change (old line 2, new line 2) is staged
        let mut pick = LinePick::default();
        pick.old.insert(2);
        pick.new.insert(2);
        backend
            .stage_lines(Path::new("f.txt"), HunkScope::Unstaged, 1, 1, &pick)
            .expect("stage lines");

        // then the index has B but not D or the new f
        assert_eq!(staged(), "a\nB\nc\nd\ne\n");

        // when the appended f (new line 6 of index → worktree) is staged too
        let mut pick = LinePick::default();
        pick.new.insert(6);
        backend
            .stage_lines(Path::new("f.txt"), HunkScope::Unstaged, 1, 1, &pick)
            .expect("stage the addition");
        assert_eq!(staged(), "a\nB\nc\nd\ne\nf\n");

        // and unstaging just the B line in the staged diff restores b
        let mut pick = LinePick::default();
        pick.old.insert(2);
        pick.new.insert(2);
        backend
            .unstage_lines(Path::new("f.txt"), 1, 1, &pick)
            .expect("unstage lines");
        assert_eq!(staged(), "a\nb\nc\nd\ne\nf\n");
        // the working tree is untouched throughout
        assert_eq!(
            fs::read_to_string(root.join("f.txt")).unwrap(),
            "a\nB\nc\nD\ne\nf\n"
        );

        // a pick of only context lines is refused
        let mut pick = LinePick::default();
        pick.new.insert(1);
        assert!(
            backend
                .stage_lines(Path::new("f.txt"), HunkScope::Unstaged, 1, 1, &pick)
                .is_err()
        );
    }

    #[test]
    fn interactive_rebase_reorders_rewords_folds_and_drops() {
        // given five commits, each adding its own file
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        let base = git_id(root, "HEAD");
        let add_commit = |name: &str| {
            fs::write(root.join(format!("src/{name}.txt")), format!("{name}\n")).expect("write");
            run_git_command(root, &["add", "."]).expect("add");
            run_git_command(root, &["commit", "-qm", name]).expect("commit");
            git_id(root, "HEAD")
        };
        let a = add_commit("a");
        let b = add_commit("b");
        let c = add_commit("c");
        let d = add_commit("d");
        let e = add_commit("e");
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("backend");
        let step = |action, id: &str, message: Option<&str>| RebaseStep {
            action,
            id: id.to_string(),
            message: message.map(str::to_string),
        };

        // when: move c first, reword it, fixup b into a, drop d, squash e
        backend
            .rebase_interactive(
                Some(&base),
                &[
                    step(RebaseAction::Reword, &c, Some("c first\n\nmoved up")),
                    step(RebaseAction::Pick, &a, None),
                    step(RebaseAction::Fixup, &b, None),
                    step(RebaseAction::Drop, &d, None),
                    step(RebaseAction::Squash, &e, None),
                ],
            )
            .expect("interactive rebase");

        // then: two commits remain above the base, in the new order
        let subjects =
            run_git_command(root, &["log", "--format=%s", &format!("{base}..HEAD")]).expect("log");
        let subjects: Vec<&str> = subjects.lines().collect();
        assert_eq!(subjects, vec!["a", "c first"]);
        assert!(
            backend
                .commit_message("HEAD~1")
                .unwrap()
                .contains("moved up")
        );
        // b was folded into a, e squashed into it too, d is gone
        let files =
            run_git_command(root, &["show", "--name-only", "--format=", "HEAD"]).expect("show");
        assert!(files.contains("src/a.txt") && files.contains("src/b.txt"));
        assert!(files.contains("src/e.txt"), "{files}");
        assert!(!root.join("src/d.txt").exists());
        let squashed = backend.commit_message("HEAD").unwrap();
        assert!(
            squashed.contains('a') && squashed.contains('e'),
            "{squashed}"
        );

        // and a todo starting with a fold is refused before git runs
        let head = git_id(root, "HEAD");
        assert!(
            backend
                .rebase_interactive(None, &[step(RebaseAction::Squash, &head, None)])
                .is_err()
        );
    }

    #[test]
    fn commit_context_actions_rewrite_history() {
        // given four commits, each adding its own file
        let temp_dir = tempdir().expect("failed to create temp dir");
        let root = temp_dir.path();
        setup_standard_repo(root);
        let add_commit = |name: &str| {
            fs::write(root.join(format!("src/{name}.txt")), format!("{name}\n")).expect("write");
            run_git_command(root, &["add", "."]).expect("add");
            run_git_command(root, &["commit", "-qm", name]).expect("commit");
            git_id(root, "HEAD")
        };
        let two = add_commit("two");
        let three = add_commit("three");
        let four = add_commit("four");
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("backend");
        let count = || {
            run_git_command(root, &["rev-list", "--count", "HEAD"])
                .expect("count")
                .trim()
                .to_string()
        };
        let subject = |rev: &str| {
            run_git_command(root, &["log", "-1", "--format=%s", rev])
                .expect("log")
                .trim()
                .to_string()
        };
        assert_eq!(count(), "4");

        // reword an older commit: the later ones are replayed on top
        backend
            .reword_commit(&three, "three reworded\n\nwith a body")
            .expect("reword older");
        assert_eq!(subject("HEAD~1"), "three reworded");
        assert!(
            backend
                .commit_message("HEAD~1")
                .unwrap()
                .contains("with a body")
        );
        assert_eq!(subject("HEAD"), "four");
        assert_eq!(count(), "4");

        // reword HEAD amends without sweeping in staged changes
        fs::write(root.join("src/extra.txt"), "extra\n").expect("write");
        run_git_command(root, &["add", "src/extra.txt"]).expect("add");
        backend
            .reword_commit("HEAD", "four reworded")
            .expect("reword head");
        assert_eq!(subject("HEAD"), "four reworded");
        let staged = run_git_command(root, &["diff", "--cached", "--name-only"]).expect("diff");
        assert!(
            staged.contains("src/extra.txt"),
            "staged file must stay staged"
        );
        run_git_command(root, &["rm", "-qf", "--cached", "src/extra.txt"]).expect("unstage");
        fs::remove_file(root.join("src/extra.txt")).expect("rm");

        // fixup HEAD into its parent keeps the parent's message
        backend.fixup_commit("HEAD").expect("fixup");
        assert_eq!(count(), "3");
        assert_eq!(subject("HEAD"), "three reworded");
        assert!(root.join("src/four.txt").exists());

        // drop the "two" commit: its file goes, later work stays
        backend.drop_commit(&two).expect("drop");
        assert_eq!(count(), "2");
        assert!(!root.join("src/two.txt").exists());
        assert!(root.join("src/three.txt").exists());

        // tag, then revert HEAD
        backend.create_tag("v1", "HEAD").expect("tag");
        let tags = run_git_command(root, &["tag", "--points-at", "HEAD"]).expect("tags");
        assert!(tags.contains("v1"));
        let head_before = git_id(root, "HEAD");
        backend.revert_commit("HEAD").expect("revert");
        assert!(subject("HEAD").starts_with("Revert"));
        assert!(!root.join("src/three.txt").exists());
        assert_ne!(git_id(root, "HEAD"), head_before);

        // reset soft keeps the reverted change staged; hard discards
        backend
            .reset_to("HEAD~1", ResetMode::Soft)
            .expect("reset soft");
        assert_eq!(git_id(root, "HEAD"), head_before);
        assert!(
            !run_git_command(root, &["diff", "--cached", "--name-only"])
                .unwrap()
                .trim()
                .is_empty()
        );
        backend
            .reset_to("HEAD", ResetMode::Hard)
            .expect("reset hard");
        assert!(root.join("src/three.txt").exists());
        assert!(
            run_git_command(root, &["status", "--porcelain"])
                .unwrap()
                .trim()
                .is_empty()
        );

        // a commit off the branch is refused for rewrites
        assert!(
            backend.drop_commit(&four).is_err(),
            "{four} was rewritten away"
        );
    }

    fn is_dirty(root: &Path) -> bool {
        !run_git_command(root, &["status", "--porcelain"])
            .expect("git status")
            .trim()
            .is_empty()
    }

    fn setup_object_format_repo(format: &str) -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        run_git_command(dir.path(), &["init", &format!("--object-format={format}")]).unwrap();
        setup_standard_repo(dir.path());
        dir
    }

    #[test]
    fn sha256_default_discovery_from_root_and_subdirectory_uses_cli() {
        let dir = setup_object_format_repo("sha256");
        for path in [dir.path().to_path_buf(), dir.path().join("src")] {
            let backend = GitBackend::discover_from(
                &path,
                GitBackendPreference::Libgit2,
                DiffWhitespaceMode::Normal,
            )
            .expect("SHA-256 repositories should open via the Git CLI");
            assert!(matches!(backend, GitBackend::Cli(_)));
            assert_eq!(backend.get_recent_commits(0, 10).unwrap()[0].id.len(), 64);
        }
    }

    fn assert_root_diff_for_object_formats(target: &str) {
        for format in ["sha1", "sha256"] {
            let dir = setup_object_format_repo(format);
            let backend = GitBackend::discover_from(
                dir.path(),
                GitBackendPreference::Cli,
                DiffWhitespaceMode::Normal,
            )
            .unwrap();
            let highlighter = SyntaxHighlighter::default();
            let explicit = backend.resolve_revision_range("HEAD").unwrap();
            let files = match target {
                "explicit" => backend.get_commit_range_diff(&explicit, &highlighter),
                "list" => backend.get_commit_range_diff(
                    &ResolvedRevisionRange::from_owned_commit_ids(
                        explicit.commit_ids.to_vec(),
                        super::super::traits::RevisionDiffTarget::CommitList,
                    ),
                    &highlighter,
                ),
                "worktree" => {
                    backend.get_working_tree_with_commits_diff(&explicit.commit_ids, &highlighter)
                }
                _ => unreachable!(),
            }
            .unwrap_or_else(|error| panic!("{format} {target} root diff failed: {error}"));
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].new_path, Some(PathBuf::from("src/file.txt")));
            assert_eq!(files[0].status, FileStatus::Added);
            assert!(
                files[0]
                    .hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .any(|l| l.content.contains("one"))
            );
        }
    }

    #[test]
    fn sha256_ranges_worktree_changes_and_context_keep_full_commit_ids() {
        let dir = setup_object_format_repo("sha256");
        let root = dir.path();
        let first = run_git_command(root, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        fs::write(root.join("src/file.txt"), "one\ntwo\n").unwrap();
        run_git_command(root, &["add", "."]).unwrap();
        run_git_command(root, &["commit", "-m", "second"]).unwrap();
        let second = run_git_command(root, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        let backend = GitBackend::discover_from(
            root,
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .unwrap();
        let highlighter = SyntaxHighlighter::default();
        let range = backend
            .resolve_revision_range(&format!("{first}..{second}"))
            .unwrap();
        assert_eq!(range.commit_ids.as_ref(), std::slice::from_ref(&second));
        assert_eq!(
            backend
                .get_commits_info(&[first.clone(), second.clone()])
                .unwrap()
                .len(),
            2
        );
        let files = backend.get_commit_range_diff(&range, &highlighter).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Modified);
        let context = backend
            .fetch_context_lines(
                Path::new("src/file.txt"),
                FileStatus::Modified,
                Some(&second),
                1,
                2,
            )
            .unwrap();
        assert_eq!(context.len(), 2);
        assert!(context[1].content.contains("two"));
        fs::write(root.join("src/file.txt"), "one\ntwo\nstaged\n").unwrap();
        run_git_command(root, &["add", "."]).unwrap();
        fs::write(root.join("src/file.txt"), "one\ntwo\nstaged\nunstaged\n").unwrap();
        assert_eq!(backend.get_staged_diff(&highlighter).unwrap().len(), 1);
        assert_eq!(backend.get_unstaged_diff(&highlighter).unwrap().len(), 1);
        assert_eq!(
            backend.get_working_tree_diff(&highlighter).unwrap().len(),
            1
        );
        assert_eq!(
            run_git_command(root, &["rev-parse", "HEAD"])
                .unwrap()
                .trim(),
            second
        );
    }

    #[test]
    fn sha256_root_commit_explicit_diff() {
        assert_root_diff_for_object_formats("explicit");
    }

    #[test]
    fn sha256_root_commit_list_diff() {
        assert_root_diff_for_object_formats("list");
    }

    #[test]
    fn sha256_root_commit_with_worktree_diff() {
        assert_root_diff_for_object_formats("worktree");
    }

    fn setup_standard_repo(root: &Path) {
        fs::create_dir(root.join("src")).expect("failed to create src dir");
        fs::write(root.join("src/file.txt"), "one\n").expect("failed to write file");

        run_git_command(root, &["init"]).expect("failed to init repo");
        run_git_command(root, &["config", "user.name", "Tuicr Test"])
            .expect("failed to set user name");
        run_git_command(root, &["config", "user.email", "tuicr@example.com"])
            .expect("failed to set user email");
        run_git_command(root, &["add", "src/file.txt"]).expect("failed to add file");
        run_git_command(root, &["commit", "-m", "initial"]).expect("failed to commit");
    }
}
