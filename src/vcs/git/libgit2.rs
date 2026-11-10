use git2::Repository;
use std::path::{Path, PathBuf};
use std::sync::Once;

use crate::error::{Result, TuicrError};
use crate::model::{DiffFile, DiffLine, FileStatus};
use crate::syntax::SyntaxHighlighter;

use super::{context, diff, repository, staging};
use crate::vcs::traits::{
    ChangeKind, CommitInfo, ConflictCommit, DiffWhitespaceMode, MainTrack, MergeConflictInfo,
    ResolvedRevisionRange, VcsBackend, VcsInfo, VcsType,
};

/// Git backend implementation using the git2/libgit2 library.
pub struct Libgit2Backend {
    repo: Repository,
    info: VcsInfo,
    whitespace_mode: DiffWhitespaceMode,
}

/// Declare libgit2 extensions tuicr understands so discovery doesn't refuse
/// repos that opt into newer git on-disk features.
///
/// Currently: `relativeworktrees` (git 2.48+ `worktree.useRelativePaths`).
/// Without this declaration libgit2 refuses to open a worktree created from a
/// bare clone with that setting — tuicr would surface as "Not a repository"
/// while plain `git status` works fine. Path resolution for relative
/// `gitdir:` pointers already works in libgit2; only the safety gate refuses.
/// Short id + summary for a commit, used to attribute merge-view panes.
fn commit_brief(commit: &git2::Commit<'_>) -> ConflictCommit {
    let id = commit.id().to_string();
    ConflictCommit {
        short_id: id[..7.min(id.len())].to_string(),
        summary: commit.summary().unwrap_or("(no message)").to_string(),
    }
}

/// Friendly label and commit identity for the incoming side of an
/// in-progress merge, cherry-pick, rebase, or revert: a local branch
/// pointing at the incoming commit when one exists, else the short commit
/// id, else "incoming".
fn incoming_head_info(repo: &Repository) -> (String, Option<ConflictCommit>) {
    for head in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REBASE_HEAD",
        "REVERT_HEAD",
    ] {
        let Ok(reference) = repo.find_reference(head) else {
            continue;
        };
        let Some(oid) = reference.target() else {
            continue;
        };
        let commit = repo.find_commit(oid).ok().map(|c| commit_brief(&c));
        if let Ok(branches) = repo.branches(Some(git2::BranchType::Local)) {
            for (branch, _) in branches.flatten() {
                if branch.get().target() == Some(oid)
                    && let Ok(Some(name)) = branch.name()
                {
                    return (name.to_string(), commit);
                }
            }
        }
        let id = oid.to_string();
        return (id[..7.min(id.len())].to_string(), commit);
    }
    ("incoming".to_string(), None)
}

fn register_supported_extensions() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: libgit2 stores extensions in a process-wide static. We call
        // this exactly once, before any `Repository::discover`, via `Once`.
        unsafe {
            let _ = git2::opts::set_extensions(&["relativeworktrees"]);
        }
    });
}

impl Libgit2Backend {
    pub(super) fn discover_from(cwd: &Path, whitespace_mode: DiffWhitespaceMode) -> Result<Self> {
        register_supported_extensions();
        let repo = Repository::discover(cwd).map_err(|_| TuicrError::NotARepository)?;

        let root_path = repo
            .workdir()
            .ok_or(TuicrError::NotARepository)?
            .to_path_buf();

        let head_commit = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .map(|c| c.id().to_string())
            .unwrap_or_else(|| "HEAD".to_string());

        // For unborn HEAD (fresh `git init` / `git clone` of an empty remote),
        // `repo.head()` errors, so fall back to reading HEAD's symbolic target
        // directly. That way the status bar still shows e.g. `git:main` instead
        // of `git:detached` before the first commit lands.
        let branch_name = repo
            .head()
            .ok()
            .and_then(|h| {
                if h.is_branch() {
                    h.shorthand().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .or_else(|| {
                repo.find_reference("HEAD")
                    .ok()
                    .and_then(|r| r.symbolic_target().map(str::to_string))
                    .and_then(|t| t.strip_prefix("refs/heads/").map(str::to_string))
            });

        let info = VcsInfo {
            root_path,
            head_commit,
            branch_name,
            vcs_type: VcsType::Git,
        };

        Ok(Self {
            repo,
            info,
            whitespace_mode,
        })
    }
}

impl Libgit2Backend {
    /// Refresh the in-memory index from disk. `Repository::index()` hands
    /// back the same refcounted git_index every time, so one `read`
    /// refreshes every future user of the cache. Needed whenever the git
    /// CLI rewrites `.git/index` behind libgit2's back (history ops).
    pub(super) fn reload_index(&self) {
        if let Ok(mut index) = self.repo.index() {
            let _ = index.read(false);
        }
    }
}

impl VcsBackend for Libgit2Backend {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn supports_sparse_checkout(&self) -> bool {
        false
    }

    fn get_working_tree_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        diff::get_working_tree_diff(&self.repo, &self.whitespace_mode, highlighter)
    }

    fn get_staged_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        diff::get_staged_diff(&self.repo, &self.whitespace_mode, highlighter)
    }

    fn get_unstaged_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        diff::get_unstaged_diff(&self.repo, &self.whitespace_mode, highlighter)
    }

    fn list_changed_paths(&self, kind: ChangeKind) -> Result<Vec<PathBuf>> {
        diff::list_changed_paths(&self.repo, kind)
    }

    fn fetch_context_lines(
        &self,
        file_path: &Path,
        file_status: FileStatus,
        ref_commit: Option<&str>,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<DiffLine>> {
        context::fetch_context_lines(
            &self.repo,
            file_path,
            file_status,
            ref_commit,
            start_line,
            end_line,
        )
    }

    fn file_line_count(
        &self,
        file_path: &Path,
        file_status: FileStatus,
        ref_commit: Option<&str>,
    ) -> Result<u32> {
        context::file_line_count(&self.repo, file_path, file_status, ref_commit)
    }

    fn main_track(&self) -> Option<MainTrack> {
        repository::main_track(&self.repo)
    }

    fn get_recent_commits(&self, offset: usize, limit: usize) -> Result<Vec<CommitInfo>> {
        let git_commits = repository::get_recent_commits(&self.repo, offset, limit)?;
        Ok(git_commits
            .into_iter()
            .map(|c| CommitInfo {
                id: c.id,
                short_id: c.short_id,
                branch_name: c.branch_name,
                parent_ids: c.parent_ids,
                refs: c.refs,
                is_head: c.is_head,
                summary: c.summary,
                body: c.body,
                author: c.author,
                time: c.time,
            })
            .collect())
    }

    fn resolve_revision_range(&self, revisions: &str) -> Result<ResolvedRevisionRange<'static>> {
        repository::resolve_revision_range(&self.repo, revisions)
    }

    fn get_commit_range_diff(
        &self,
        revision_range: &ResolvedRevisionRange<'_>,
        highlighter: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        diff::get_commit_range_diff(
            &self.repo,
            revision_range,
            &self.whitespace_mode,
            highlighter,
        )
    }

    fn get_commits_info(&self, ids: &[String]) -> Result<Vec<CommitInfo>> {
        let git_commits = repository::get_commits_info(&self.repo, ids)?;
        Ok(git_commits
            .into_iter()
            .map(|c| CommitInfo {
                id: c.id,
                short_id: c.short_id,
                branch_name: c.branch_name,
                parent_ids: c.parent_ids,
                refs: c.refs,
                is_head: c.is_head,
                summary: c.summary,
                body: c.body,
                author: c.author,
                time: c.time,
            })
            .collect())
    }

    fn get_working_tree_with_commits_diff(
        &self,
        commit_ids: &[String],
        highlighter: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        diff::get_working_tree_with_commits_diff(
            &self.repo,
            commit_ids,
            &self.whitespace_mode,
            highlighter,
        )
    }

    fn stage_file(&self, path: &Path) -> Result<()> {
        staging::stage_file(&self.repo, path)
    }

    fn get_merge_conflicts(&self) -> Result<Option<MergeConflictInfo>> {
        let mut index = self.repo.index()?;
        // The Repository caches its index in memory; a cherry-pick/rebase
        // run after startup (selector `p`/`b`) rewrites it on disk, so
        // re-read or the new conflict entries are invisible.
        index.read(false)?;
        if !index.has_conflicts() {
            return Ok(None);
        }

        let mut paths: Vec<PathBuf> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for conflict in index.conflicts()? {
            let conflict = conflict?;
            let entry = conflict
                .our
                .as_ref()
                .or(conflict.their.as_ref())
                .or(conflict.ancestor.as_ref());
            let Some(entry) = entry else { continue };
            let path = String::from_utf8_lossy(&entry.path).to_string();
            if seen.insert(path.clone()) {
                paths.push(PathBuf::from(path));
            }
        }
        if paths.is_empty() {
            return Ok(None);
        }

        let ours_label = self
            .info
            .branch_name
            .clone()
            .unwrap_or_else(|| "HEAD".to_string());
        let ours_commit = self
            .repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .map(|c| commit_brief(&c));
        let (theirs_label, theirs_commit) = incoming_head_info(&self.repo);
        Ok(Some(MergeConflictInfo {
            ours_label,
            theirs_label,
            ours_commit,
            theirs_commit,
            paths,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(workdir: &Path, args: &[&str]) {
        // `-c commit.gpgsign=false` overrides any global signing config so
        // contributors with commit signing enabled aren't prompted to sign
        // throwaway commits in these temp repos.
        // `-c safe.bareRepository=all` allows commands run inside bare test
        // repositories to work on systems with `safe.bareRepository=explicit`.
        let output = Command::new("git")
            .current_dir(workdir)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultRefFormat=files",
                "-c",
                "safe.bareRepository=all",
            ])
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn should_discover_worktree_with_relativeworktrees_extension() {
        // given: a bare clone with `extensions.relativeworktrees = true` set
        // and a worktree linked to it. This is the on-disk state git 2.48+
        // produces with `worktree.useRelativePaths`. Without
        // `set_extensions(["relativeworktrees"])` libgit2 refuses to open the
        // worktree at all, surfacing as "Not a repository" in tuicr.
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        let bare = temp.path().join("bare.git");
        let worktree = temp.path().join("wt");

        fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "-q", "-b", "main"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test User"]);
        fs::write(source.join("README"), "hello\n").unwrap();
        git(&source, &["add", "README"]);
        git(&source, &["commit", "-q", "-m", "init"]);

        git(
            temp.path(),
            &[
                "clone",
                "--bare",
                "-q",
                source.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        // The extension declaration is the gate libgit2 enforces. Setting
        // this alone reproduces the failure regardless of the local git
        // version — `worktree.useRelativePaths` (git 2.48+) is what writes
        // this in real-world setups. `core.repositoryFormatVersion = 1` is
        // required to opt into the `extensions.*` namespace at all.
        git(&bare, &["config", "core.repositoryFormatVersion", "1"]);
        git(&bare, &["config", "extensions.relativeworktrees", "true"]);
        git(
            &bare,
            &["worktree", "add", "-q", worktree.to_str().unwrap()],
        );

        // when
        let backend = Libgit2Backend::discover_from(&worktree, DiffWhitespaceMode::Normal)
            .expect("worktree with relativeworktrees extension should open");

        // then
        assert_eq!(backend.info().vcs_type, VcsType::Git);
        assert!(
            backend.repo.workdir().is_some(),
            "worktree must report a workdir"
        );
    }
}
