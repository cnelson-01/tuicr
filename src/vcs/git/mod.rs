mod cli;
pub mod context;
pub mod diff;
mod libgit2;
pub(crate) mod raw;
pub mod repository;
pub mod staging;

use crate::vcs::traits::BranchInfo;
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

    fn discover_from(
        cwd: &Path,
        preference: GitBackendPreference,
        whitespace_mode: DiffWhitespaceMode,
    ) -> Result<Self> {
        // libgit2 doesn't support reftable or split index repositories, so we fallback to cli
        // TODO: remove reftable fallback logic when libgit2 supports it as part of https://github.com/libgit2/libgit2/issues/5352
        // TODO: remove split index fallback logic when libgit2 supports it as part of https://github.com/libgit2/libgit2/issues/6132
        let use_cli =
            preference == GitBackendPreference::Cli || uses_reftable(cwd) || uses_split_index(cwd);

        if use_cli {
            return Ok(Self::Cli(GitCliBackend::discover_from(
                cwd,
                whitespace_mode,
            )?));
        }

        let backend = Self::Libgit2(Libgit2Backend::discover_from(cwd, whitespace_mode)?);
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

fn run_git_command(workdir: &Path, args: &[&str]) -> Result<String> {
    // `-c commit.gpgsign=false` is a no-op for the read-only `config`/`init`
    // calls this makes in production, but it keeps the test-only `commit`
    // calls below from prompting contributors with commit signing enabled
    // globally to sign throwaway commits in temp repos.
    let full_args: Vec<&str> = ["-c", "commit.gpgsign=false"]
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
    use std::fs;
    use tempfile::tempdir;

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

    fn is_dirty(root: &Path) -> bool {
        !run_git_command(root, &["status", "--porcelain"])
            .expect("git status")
            .trim()
            .is_empty()
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
