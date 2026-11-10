use crate::vcs::traits::{MainTrack, RefKind, RefLabel};
use chrono::{DateTime, TimeZone, Utc};
use git2::{BranchType, Oid, Repository};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::error::{Result, TuicrError};
use crate::vcs::{ResolvedRevisionRange, RevisionDiffTarget};

use super::RevisionExpression;

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub id: String,
    pub short_id: String,
    pub branch_name: Option<String>,
    pub parent_ids: Vec<String>,
    pub refs: Vec<RefLabel>,
    pub is_head: bool,
    pub summary: String,
    pub body: Option<String>,
    pub author: String,
    pub time: DateTime<Utc>,
}

/// Parse a full commit message into (summary, optional body).
/// The summary is the first line; the body is everything after the first blank line, trimmed.
fn parse_commit_message(message: &str) -> (String, Option<String>) {
    let mut lines = message.lines();
    let summary = lines.next().unwrap_or("(no message)").to_string();
    // Skip blank separator line(s) between summary and body
    let body_text: String = lines
        .skip_while(|l| l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let body = if body_text.trim().is_empty() {
        None
    } else {
        Some(body_text)
    };
    (summary, body)
}

fn get_branch_tip_names(repo: &Repository) -> HashMap<Oid, Vec<String>> {
    let mut names_by_tip: HashMap<Oid, Vec<String>> = HashMap::new();

    if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
        for (branch, _) in branches.flatten() {
            let Some(target) = branch.get().target() else {
                continue;
            };

            let Ok(Some(name)) = branch.name() else {
                continue;
            };

            names_by_tip
                .entry(target)
                .or_default()
                .push(name.to_string());
        }
    }

    for names in names_by_tip.values_mut() {
        names.sort_unstable();
    }

    names_by_tip
}

/// Tips of the remote-tracking branches, sorted by name. Symbolic refs
/// (`origin/HEAD`) have no direct target and are skipped; the branch they
/// name is already in the list.
fn remote_branch_tips(repo: &Repository) -> Vec<Oid> {
    let mut tips: Vec<(String, Oid)> = Vec::new();
    if let Ok(branches) = repo.branches(Some(BranchType::Remote)) {
        for (branch, _) in branches.flatten() {
            let (Some(target), Ok(Some(name))) = (branch.get().target(), branch.name()) else {
                continue;
            };
            tips.push((name.to_string(), target));
        }
    }
    tips.sort_unstable();
    tips.into_iter().map(|(_, oid)| oid).collect()
}

/// Every ref pointing at each commit plus HEAD, the libgit2 twin of the
/// CLI backend's ref index. Annotated tags are peeled to their commit.
struct RefIndex {
    labels: HashMap<Oid, Vec<RefLabel>>,
    head: Option<Oid>,
}

/// The branch HEAD is attached to, `None` when detached.
fn attached_branch(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    head.is_branch()
        .then(|| head.shorthand().map(str::to_string))
        .flatten()
}

impl RefIndex {
    fn build(repo: &Repository) -> Self {
        let mut labels: HashMap<Oid, Vec<RefLabel>> = HashMap::new();
        let current = attached_branch(repo);
        if let Ok(references) = repo.references() {
            for reference in references.flatten() {
                let kind = if reference.is_branch() {
                    if reference.shorthand() == current.as_deref() {
                        RefKind::Head
                    } else {
                        RefKind::Local
                    }
                } else if reference.is_remote() {
                    RefKind::Remote
                } else if reference.is_tag() {
                    RefKind::Tag
                } else {
                    continue;
                };
                let Some(short) = reference.shorthand() else {
                    continue;
                };
                if kind == RefKind::Remote && short.ends_with("/HEAD") {
                    continue;
                }
                let Ok(commit) = reference.peel_to_commit() else {
                    continue;
                };
                labels.entry(commit.id()).or_default().push(RefLabel {
                    name: short.to_string(),
                    kind,
                });
            }
        }
        for refs in labels.values_mut() {
            refs.sort_by(|a, b| {
                ref_kind_rank(a.kind)
                    .cmp(&ref_kind_rank(b.kind))
                    .then_with(|| a.name.cmp(&b.name))
            });
        }
        let head = repo.head().ok().and_then(|head| head.target());
        Self { labels, head }
    }

    fn apply(&self, oid: Oid, commit: &mut CommitInfo) {
        if let Some(labels) = self.labels.get(&oid) {
            commit.refs = labels.clone();
        }
        commit.is_head = self.head == Some(oid);
    }
}

fn ref_kind_rank(kind: RefKind) -> u8 {
    match kind {
        RefKind::Head => 0,
        RefKind::Local => 1,
        RefKind::Remote => 2,
        RefKind::Tag => 3,
    }
}

/// Branch names the log treats as the repository's main line, in order of
/// preference.
pub(super) const MAIN_BRANCH_NAMES: [&str; 2] = ["main", "master"];

/// The log's leftmost track (`VcsBackend::main_track`), read in process.
/// `cli::main_track` is the same rule through the CLI, and a test holds the
/// two to the same answers.
///
/// The local branch is `main`, else `master`; its remote-tracking twin is
/// the same name on `origin`, else on the first other remote. With only
/// one of the two, the track is a line from it. With both:
///
/// - a twin ahead of the local branch along first parents (fetched, not
///   pulled) is the line, and the local tip sits on it further down;
/// - a twin the local branch is ahead of along first parents is on the
///   local branch's line, which is the track;
/// - otherwise they have diverged: the line is the twin's, and the local
///   branch's own first-parent commits are stacked on top of it.
pub fn main_track(repo: &Repository) -> Option<MainTrack> {
    // (remote, branch, oid), `origin` first and the rest by name.
    let mut remotes: Vec<(String, String, Oid)> = Vec::new();
    if let Ok(branches) = repo.branches(Some(BranchType::Remote)) {
        for (branch, _) in branches.flatten() {
            let (Some(oid), Ok(Some(name))) = (branch.get().target(), branch.name()) else {
                continue;
            };
            if let Some((remote, name)) = name.split_once('/')
                && MAIN_BRANCH_NAMES.contains(&name)
            {
                remotes.push((remote.to_string(), name.to_string(), oid));
            }
        }
    }
    remotes.sort_by(|a, b| (a.0 != "origin", &a.0).cmp(&(b.0 != "origin", &b.0)));

    for name in MAIN_BRANCH_NAMES {
        let Ok(local) = repo.refname_to_id(&format!("refs/heads/{name}")) else {
            continue;
        };
        let twins: Vec<Oid> = remotes
            .iter()
            .filter(|(_, branch, _)| branch == name)
            .map(|(_, _, oid)| *oid)
            .collect();
        if let Some(ahead) = twins
            .iter()
            .find(|&&twin| twin != local && first_parent_chain_reaches(repo, twin, local))
        {
            return Some(MainTrack::line(ahead.to_string()));
        }
        let Some(&twin) = twins.first() else {
            return Some(MainTrack::line(local.to_string()));
        };
        if twin == local || first_parent_chain_reaches(repo, local, twin) {
            return Some(MainTrack::line(local.to_string()));
        }
        return Some(MainTrack {
            tip: twin.to_string(),
            stacked: first_parents_beyond(repo, local, twin)
                .iter()
                .map(Oid::to_string)
                .collect(),
        });
    }
    MAIN_BRANCH_NAMES.iter().find_map(|name| {
        remotes
            .iter()
            .find(|(_, branch, _)| branch == name)
            .map(|(_, _, oid)| MainTrack::line(oid.to_string()))
    })
}

/// `tip`'s first-parent commits that `base` does not have, newest first:
/// the walk stops at the first one `base` can reach.
fn first_parents_beyond(repo: &Repository, tip: Oid, base: Oid) -> Vec<Oid> {
    // One walk for everything `tip` has that `base` does not, then the
    // first-parent line through it.
    let Ok(mut walk) = repo.revwalk() else {
        return Vec::new();
    };
    if walk.push(tip).is_err() || walk.hide(base).is_err() {
        return Vec::new();
    }
    let beyond: std::collections::HashSet<Oid> = walk.flatten().collect();
    let mut out = Vec::new();
    let mut at = tip;
    while out.len() < MainTrack::MAX_STACKED && beyond.contains(&at) {
        out.push(at);
        match repo.find_commit(at).and_then(|commit| commit.parent_id(0)) {
            Ok(parent) => at = parent,
            Err(_) => break,
        }
    }
    out
}

/// Whether following first parents from `tip` arrives at `ancestor`.
fn first_parent_chain_reaches(repo: &Repository, tip: Oid, ancestor: Oid) -> bool {
    // Every step that has not yet entered `ancestor`'s history is one of
    // the commits `tip` is ahead by, which bounds the walk.
    let Ok((ahead, _)) = repo.graph_ahead_behind(tip, ancestor) else {
        return false;
    };
    let mut at = tip;
    for _ in 0..ahead {
        let Ok(parent) = repo.find_commit(at).and_then(|commit| commit.parent_id(0)) else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        at = parent;
    }
    false
}

/// The log's row order for one repository and one set of tips.
///
/// libgit2 sorts topologically only after reading every reachable commit,
/// so each call to the walk costs the whole history however few rows are
/// asked for: 50 ms warm and 400 ms cold on a 57k-commit repository, paid
/// again for every page, every refresh and every time the selector opens.
/// The order depends on nothing but the tips, so it is kept until one of
/// them moves.
struct HistoryOrder {
    git_dir: PathBuf,
    /// The walk's inputs, in push order: HEAD, local tips, remote tips.
    tips: Vec<Oid>,
    order: Vec<Oid>,
    /// Whether `order` is the whole walk or was cut at `MAX_CACHED_HISTORY`.
    complete: bool,
    /// Its own handle on the repository. libgit2 caches parsed commits per
    /// handle, and the backends reopen theirs after every operation, so
    /// walking with this one is what keeps a re-walk warm.
    repo: Repository,
}

/// Commits kept per cached walk (20 bytes each). Rows past this are read
/// by walking again.
const MAX_CACHED_HISTORY: usize = 250_000;

static HISTORY_ORDER: Mutex<Option<HistoryOrder>> = Mutex::new(None);

/// Tips the log walks, in the order they are pushed: HEAD, then every
/// local branch, then every remote-tracking branch. Tips go in sorted by
/// name: HashMap iteration is randomized per process and would reshuffle
/// equal-date rows on every launch.
fn history_tips(repo: &Repository, branch_tip_names: &HashMap<Oid, Vec<String>>) -> Vec<Oid> {
    let mut tips: Vec<Oid> = Vec::new();
    if let Ok(head) = repo.head().and_then(|head| head.peel_to_commit()) {
        tips.push(head.id());
    }
    let mut local: Vec<_> = branch_tip_names.keys().copied().collect();
    local.sort_unstable_by_key(|oid| {
        branch_tip_names
            .get(oid)
            .and_then(|names| names.first())
            .cloned()
            .unwrap_or_default()
    });
    tips.extend(local);
    tips.extend(remote_branch_tips(repo));
    tips
}

/// Walk every local and remote-tracking branch, not just HEAD, so the
/// selector can show branch tracks and commits that exist only on a
/// remote. TOPOLOGICAL | TIME matches `git log --date-order`: rows sort by
/// commit date while children stay above parents, which the commit-graph
/// lane layout relies on.
fn history_walk<'r>(repo: &'r Repository, tips: &[Oid]) -> Result<git2::Revwalk<'r>> {
    let mut revwalk = repo.revwalk()?;
    for tip in tips {
        let _ = revwalk.push(*tip);
    }
    revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
    Ok(revwalk)
}

/// Reorder `order` so the local main branch's own commits come before the
/// remote line they are drawn on top of (`MainTrack::stacked` above
/// `MainTrack::tip`).
///
/// Date order puts whichever side committed last on top, so a fetch that
/// brings newer commits would list the remote tip first and leave the graph
/// no way to stack the local branch on it. The two sides are unrelated, so
/// either order keeps children above parents. The remote tip and those of
/// its ancestors that sit above the stack's oldest commit sink to just
/// below it, keeping their own order: everything they depend on is already
/// further down, and everything that depends on them stays where it is,
/// above.
fn stack_local_main_first(repo: &Repository, order: &mut Vec<Oid>) {
    let Some(track) = main_track(repo) else {
        return;
    };
    let (Some(Ok(oldest)), Ok(tip)) = (
        track.stacked.last().map(|id| Oid::from_str(id)),
        Oid::from_str(&track.tip),
    ) else {
        return;
    };
    let positions: HashMap<Oid, usize> = order.iter().enumerate().map(|(i, o)| (*o, i)).collect();
    let (Some(&floor), Some(&tip_at)) = (positions.get(&oldest), positions.get(&tip)) else {
        return;
    };
    if tip_at > floor {
        return;
    }
    let mut sinking: std::collections::HashSet<Oid> = std::collections::HashSet::new();
    let mut pending = vec![tip];
    while let Some(oid) = pending.pop() {
        if positions.get(&oid).is_none_or(|&at| at > floor) || !sinking.insert(oid) {
            continue;
        }
        if let Ok(commit) = repo.find_commit(oid) {
            pending.extend(commit.parent_ids());
        }
    }
    let (upper, lower) = order.split_at(floor + 1);
    let mut fixed = Vec::with_capacity(order.len());
    fixed.extend(upper.iter().filter(|oid| !sinking.contains(oid)));
    fixed.extend(upper.iter().filter(|oid| sinking.contains(oid)));
    fixed.extend(lower);
    *order = fixed;
}

/// Rows `offset..offset + limit` of the log's order, from the kept walk
/// when the tips have not moved since it was made.
fn history_page(repo: &Repository, tips: &[Oid], offset: usize, limit: usize) -> Result<Vec<Oid>> {
    let mut slot = HISTORY_ORDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fresh = slot
        .as_ref()
        .is_some_and(|kept| kept.git_dir == repo.path() && kept.tips == tips);
    if !fresh {
        // Reuse the kept handle for the same repository: its commit cache
        // is what makes this walk cheap.
        let walker = match slot.take() {
            Some(kept) if kept.git_dir == repo.path() => Some(kept.repo),
            _ => Repository::open(repo.path()).ok(),
        };
        if let Some(walker) = walker {
            let mut order = Vec::new();
            let mut complete = true;
            for oid in history_walk(&walker, tips)? {
                if order.len() == MAX_CACHED_HISTORY {
                    complete = false;
                    break;
                }
                order.push(oid?);
            }
            stack_local_main_first(&walker, &mut order);
            *slot = Some(HistoryOrder {
                git_dir: repo.path().to_path_buf(),
                tips: tips.to_vec(),
                order,
                complete,
                repo: walker,
            });
        }
    }
    match slot.as_ref() {
        Some(kept) if kept.complete || offset.saturating_add(limit) <= kept.order.len() => Ok(kept
            .order
            .iter()
            .skip(offset)
            .take(limit)
            .copied()
            .collect()),
        // Past what is kept, or no second handle could be opened.
        _ => history_walk(repo, tips)?
            .skip(offset)
            .take(limit)
            .map(|oid| oid.map_err(Into::into))
            .collect(),
    }
}

pub fn get_recent_commits(
    repo: &Repository,
    offset: usize,
    limit: usize,
) -> Result<Vec<CommitInfo>> {
    // Unborn HEAD (fresh `git init` / `git clone` of an empty remote): the
    // branch ref does not point at a commit yet, so there is nothing to walk.
    // Probe with `repo.head()` — pushing HEAD onto a walk rewraps the
    // underlying "reference not found" error with a generic code, which
    // would slip past an `ErrorCode::UnbornBranch` check. Treat unborn HEAD
    // as zero commits so App startup falls through to the staged/unstaged
    // paths.
    if matches!(
        repo.head().map_err(|e| e.code()),
        Err(git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound)
    ) {
        return Ok(Vec::new());
    }
    let branch_tip_names = get_branch_tip_names(repo);
    let tips = history_tips(repo, &branch_tip_names);

    let ref_index = RefIndex::build(repo);
    let mut commits = Vec::new();
    for oid in history_page(repo, &tips, offset, limit)? {
        let commit = repo.find_commit(oid)?;

        let id = oid.to_string();
        let short_id = id[..7.min(id.len())].to_string();
        let full_message = commit.message().unwrap_or("(no message)");
        let (summary, body) = parse_commit_message(full_message);
        let author = commit.author().name().unwrap_or("Unknown").to_string();
        let branch_name = branch_tip_names
            .get(&oid)
            .and_then(|names| names.first().cloned());
        let parent_ids = commit.parent_ids().map(|p| p.to_string()).collect();
        let time = Utc
            .timestamp_opt(commit.time().seconds(), 0)
            .single()
            .unwrap_or_else(Utc::now);

        let mut info = CommitInfo {
            id,
            short_id,
            branch_name,
            parent_ids,
            refs: Vec::new(),
            is_head: false,
            summary,
            body,
            author,
            time,
        };
        ref_index.apply(oid, &mut info);
        commits.push(info);
    }

    Ok(commits)
}

/// Get commit info for specific commit IDs.
/// Returns CommitInfo in the same order as the input IDs.
pub fn get_commits_info(repo: &Repository, ids: &[String]) -> Result<Vec<CommitInfo>> {
    let branch_tip_names = get_branch_tip_names(repo);
    let mut commits = Vec::new();

    for id_str in ids {
        let oid = Oid::from_str(id_str)
            .map_err(|e| TuicrError::VcsCommand(format!("Invalid commit ID {}: {}", id_str, e)))?;
        let commit = repo
            .find_commit(oid)
            .map_err(|e| TuicrError::VcsCommand(format!("Commit not found {}: {}", id_str, e)))?;

        let id = oid.to_string();
        let short_id = id[..7.min(id.len())].to_string();
        let full_message = commit.message().unwrap_or("(no message)");
        let (summary, body) = parse_commit_message(full_message);
        let author = commit.author().name().unwrap_or("Unknown").to_string();
        let branch_name = branch_tip_names
            .get(&oid)
            .and_then(|names| names.first().cloned());
        let parent_ids = commit.parent_ids().map(|p| p.to_string()).collect();
        let time = Utc
            .timestamp_opt(commit.time().seconds(), 0)
            .single()
            .unwrap_or_else(Utc::now);

        commits.push(CommitInfo {
            id,
            short_id,
            branch_name,
            parent_ids,
            refs: Vec::new(),
            is_head: false,
            summary,
            body,
            author,
            time,
        });
    }

    Ok(commits)
}

/// Resolve a Git revision expression into selected commits and diff endpoints.
///
/// This preserves the distinction between commits selected for review metadata
/// and the old/new trees that Git would compare for the original expression.
pub fn resolve_revision_range(
    repo: &Repository,
    revisions: &str,
) -> Result<ResolvedRevisionRange<'static>> {
    match RevisionExpression::parse(revisions)? {
        RevisionExpression::Single(revision) => {
            // `HEAD`
            let head = resolve_commit_id(repo, revision)?;
            let base = first_parent_id(repo, &head)?;
            Ok(ResolvedRevisionRange::from_owned_commit_ids(
                vec![head.clone()],
                RevisionDiffTarget::Explicit { base, head },
            ))
        }
        RevisionExpression::Range { base, head } => {
            // `A..B`, `A..`, or `..B`
            let base = resolve_commit_id(repo, base)?;
            let head = resolve_commit_id(repo, head)?;
            let commit_ids = revwalk_range(repo, &base, &head)?;
            Ok(ResolvedRevisionRange::from_owned_commit_ids(
                commit_ids,
                RevisionDiffTarget::Explicit {
                    base: Some(base),
                    head,
                },
            ))
        }
        RevisionExpression::MergeBaseRange { left, right } => {
            // `A...B`
            let left = resolve_commit_id(repo, left)?;
            let right = resolve_commit_id(repo, right)?;
            let base = repo
                .merge_base(Oid::from_str(&left)?, Oid::from_str(&right)?)?
                .to_string();
            let commit_ids = revwalk_range(repo, &base, &right)?;
            Ok(ResolvedRevisionRange::from_owned_commit_ids(
                commit_ids,
                RevisionDiffTarget::Explicit {
                    base: Some(base),
                    head: right,
                },
            ))
        }
    }
}

fn resolve_commit_id(repo: &Repository, revision: &str) -> Result<String> {
    let revision = format!("{revision}^{{commit}}");
    let obj = repo.revparse_single(&revision)?;
    let commit = obj
        .peel_to_commit()
        .map_err(|e| TuicrError::VcsCommand(format!("Not a commit: {e}")))?;
    Ok(commit.id().to_string())
}

// Single-revision reviews diff the commit against its first parent.
// Root commits have no parent,
// so callers represent the old side as the empty tree with None.
fn first_parent_id(repo: &Repository, commit_id: &str) -> Result<Option<String>> {
    let commit = repo.find_commit(Oid::from_str(commit_id)?)?;
    if commit.parent_count() == 0 {
        return Ok(None);
    }
    Ok(Some(commit.parent(0)?.id().to_string()))
}

fn revwalk_range(repo: &Repository, base: &str, head: &str) -> Result<Vec<String>> {
    let mut revwalk = repo.revwalk()?;
    revwalk.push(Oid::from_str(head)?)?;
    revwalk.hide(Oid::from_str(base)?)?;
    revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;

    let mut commit_ids = Vec::new();
    for oid in revwalk {
        commit_ids.push(oid?.to_string());
    }

    commit_ids.reverse();
    Ok(commit_ids)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn should_return_empty_recent_commits_for_unborn_head() {
        // given a fresh `git init` repo (HEAD points at refs/heads/main but
        // the ref does not exist yet — the "naked clone" state)
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let repo = Repository::init(temp_dir.path()).expect("failed to init repo");

        // when
        let result = get_recent_commits(&repo, 0, 50);

        // then unborn HEAD is treated as zero commits, not an error
        let commits = result.expect("unborn HEAD should yield an empty list");
        assert!(commits.is_empty(), "unborn HEAD has no commits to walk");
    }

    /// An empty-tree commit on HEAD at `time`.
    fn commit(repo: &Repository, message: &str, time: i64) -> Oid {
        let sig = git2::Signature::new("Tester", "t@example.com", &git2::Time::new(time, 0))
            .expect("signature");
        let tree_id = repo.index().and_then(|mut i| i.write_tree()).expect("tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .expect("commit")
    }

    fn summaries(repo: &Repository, offset: usize, limit: usize) -> Vec<String> {
        get_recent_commits(repo, offset, limit)
            .expect("recent commits")
            .into_iter()
            .map(|c| c.summary)
            .collect()
    }

    /// An empty-tree commit on `parent` at `time` that no ref points at.
    fn loose_commit(repo: &Repository, message: &str, time: i64, parent: Oid) -> Oid {
        let sig = git2::Signature::new("Tester", "t@example.com", &git2::Time::new(time, 0))
            .expect("signature");
        let parent = repo.find_commit(parent).expect("parent");
        let tree = parent.tree().expect("tree");
        repo.commit(None, &sig, &sig, message, &tree, &[&parent])
            .expect("commit")
    }

    #[test]
    fn should_list_a_diverged_local_main_above_the_remote_line() {
        // given local master two commits past `base`
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let repo = Repository::init(temp_dir.path()).expect("failed to init repo");
        repo.set_head("refs/heads/master").expect("head");
        let base = commit(&repo, "base", 1_700_000_000);
        commit(&repo, "local-1", 1_700_000_100);
        commit(&repo, "local-2", 1_700_000_200);
        // and a remote master with two newer commits of its own, plus a
        // branch cut from the remote tip that is newer still
        let remote_1 = loose_commit(&repo, "remote-1", 1_700_000_300, base);
        let remote_2 = loose_commit(&repo, "remote-2", 1_700_000_400, remote_1);
        repo.reference("refs/remotes/origin/master", remote_2, true, "test")
            .expect("remote ref");
        let topic = loose_commit(&repo, "topic", 1_700_000_500, remote_2);
        repo.reference("refs/remotes/origin/topic", topic, true, "test")
            .expect("topic ref");

        // when the log is listed
        let rows = summaries(&repo, 0, 10);
        // then the local commits come before the remote line, which date
        // order alone would put on top, and children still precede parents
        assert_eq!(
            rows,
            [
                "topic", "local-2", "local-1", "remote-2", "remote-1", "base"
            ]
        );
        // and the main track says the same
        let track = main_track(&repo).expect("track");
        assert_eq!(track.tip, remote_2.to_string());
        assert_eq!(track.stacked.len(), 2);

        // when the local branch catches up (fast-forwards to the remote)
        repo.reference("refs/heads/master", remote_2, true, "test")
            .expect("move master");
        // then plain date order is back
        assert_eq!(
            summaries(&repo, 0, 10),
            ["topic", "remote-2", "remote-1", "base"]
        );
    }

    #[test]
    fn should_reread_the_history_order_when_a_tip_moves() {
        // given two commits, listed once so their order is kept
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let repo = Repository::init(temp_dir.path()).expect("failed to init repo");
        let first = commit(&repo, "one", 1_700_000_000);
        commit(&repo, "two", 1_700_000_100);
        assert_eq!(summaries(&repo, 0, 10), ["two", "one"]);
        // then a page reads from the same order
        assert_eq!(summaries(&repo, 1, 10), ["one"]);
        assert!(summaries(&repo, 2, 10).is_empty());

        // when HEAD moves
        commit(&repo, "three", 1_700_000_200);
        // then the kept order is not served again
        assert_eq!(summaries(&repo, 0, 10), ["three", "two", "one"]);
        assert_eq!(summaries(&repo, 2, 1), ["one"]);

        // when a branch appears on a commit nothing else reaches
        let side = {
            let sig =
                git2::Signature::new("T", "t@example.com", &git2::Time::new(1_700_000_300, 0))
                    .expect("signature");
            let parent = repo.find_commit(first).expect("first");
            let tree = parent.tree().expect("tree");
            repo.commit(None, &sig, &sig, "side", &tree, &[&parent])
                .expect("side commit")
        };
        repo.branch("side", &repo.find_commit(side).expect("side"), false)
            .expect("branch");
        // then it is walked too, through a second handle as well
        assert_eq!(summaries(&repo, 0, 10), ["side", "three", "two", "one"]);
        let reopened = Repository::open(temp_dir.path()).expect("reopen");
        assert_eq!(summaries(&reopened, 0, 2), ["side", "three"]);

        // when the branch goes away again
        repo.find_branch("side", BranchType::Local)
            .and_then(|mut branch| branch.delete())
            .expect("delete branch");
        // then so do its commits
        assert_eq!(summaries(&repo, 0, 10), ["three", "two", "one"]);
    }
}
