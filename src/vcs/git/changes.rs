//! The Changes view's change set and per-entry previews for Git. One status
//! pass per backend variant (libgit2 `statuses` + `submodule_status`, or
//! `git status --porcelain=v2`) decides what every entry is; diffs are only
//! ever computed for the one entry being previewed. Shared here: the
//! porcelain v2 parser, the synthesized `Subproject commit` and context-only
//! diffs, embedded-repo detection, and the `git -C <sub>` helpers that read
//! a submodule's checkout, log and commit counts. Status, never a diff, is
//! the source of truth: the two backends disagree about untracked-only
//! submodule content when read through `diff`, and a diff drops the
//! `160000` gitlink mode.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use git2::{Delta, FileMode, Oid, Repository, Status, SubmoduleIgnore, SubmoduleStatus};

use crate::error::{Result, TuicrError};
use crate::model::{DiffFile, DiffHunk, DiffLine, FileStatus, LineOrigin};
use crate::vcs::changes::{
    ChangeEntry, ChangeEntryKind, ChangeGroup, ChangeSet, ChangeStatus, SubmoduleChange,
};
use crate::vcs::tabify;

use super::run_git_command;

/// The gitlink mode in an index entry or a tree entry.
pub(super) const GITLINK_MODE: u32 = 0o160000;

/// Most commits listed in a submodule's synthesized diff per direction.
const MAX_SUBMODULE_LOG: usize = 100;

/// `root/path` is a directory with its own `.git` (file or directory): a
/// nested repository that git lists as `?? path/` and never descends into.
pub(super) fn is_embedded_repo(root: &Path, path: &Path) -> bool {
    let dir = root.join(path);
    dir.is_dir() && dir.join(".git").exists()
}

fn strip_trailing_slash(path: &str) -> &str {
    path.strip_suffix('/').unwrap_or(path)
}

/// A hex object id, or `None` for the all-zero id porcelain prints for a
/// missing side.
fn non_zero_id(hex: &str) -> Option<String> {
    let hex = hex.trim();
    (!hex.is_empty() && !hex.bytes().all(|b| b == b'0')).then(|| hex.to_string())
}

fn oid_string(oid: Oid) -> Option<String> {
    (!oid.is_zero()).then(|| oid.to_string())
}

// ---------------------------------------------------------------------------
// Porcelain v2
// ---------------------------------------------------------------------------

/// Parse `git status --porcelain=v2 -z --untracked-files=all` output.
///
/// Records are NUL-terminated and paths are never quoted, so each record
/// is split with a bounded `splitn` and a path with spaces survives.
/// `is_embedded_repo(path)` decides whether an untracked directory is a
/// nested repository. Submodule entries carry the ids and dirt flags the
/// `S<c><m><u>` field holds; `workdir_id`, the commit counts and the
/// `.gitmodules` registration are filled in by [`cli_change_set`].
pub(super) fn parse_porcelain_v2(
    bytes: &[u8],
    mut is_embedded_repo: impl FnMut(&Path) -> bool,
) -> ChangeSet {
    let mut set = ChangeSet::default();
    let mut fields = bytes.split(|b| *b == 0);
    while let Some(record) = fields.next() {
        let Some(&tag) = record.first() else {
            continue;
        };
        let text = String::from_utf8_lossy(record);
        match tag {
            b'1' => {
                let parts: Vec<&str> = text.splitn(9, ' ').collect();
                if parts.len() < 9 {
                    continue;
                }
                let path = PathBuf::from(parts[8]);
                push_porcelain_sides(&mut set, parts[1], parts[2], &parts[3..8], path, None);
            }
            b'2' => {
                let orig = fields
                    .next()
                    .map(|p| PathBuf::from(String::from_utf8_lossy(p).into_owned()));
                let parts: Vec<&str> = text.splitn(10, ' ').collect();
                if parts.len() < 10 {
                    continue;
                }
                let path = PathBuf::from(parts[9]);
                push_porcelain_sides(&mut set, parts[1], parts[2], &parts[3..8], path, orig);
            }
            b'u' => {
                let parts: Vec<&str> = text.splitn(11, ' ').collect();
                if parts.len() < 11 {
                    continue;
                }
                set.push(
                    ChangeGroup::Conflicted,
                    ChangeEntry::file(parts[10], ChangeStatus::Conflicted),
                );
            }
            b'?' => {
                let Some(path) = text.strip_prefix("? ") else {
                    continue;
                };
                let dir = strip_trailing_slash(path);
                if path.ends_with('/') && is_embedded_repo(Path::new(dir)) {
                    set.push(
                        ChangeGroup::Untracked,
                        ChangeEntry {
                            path: PathBuf::from(dir),
                            old_path: None,
                            status: ChangeStatus::Untracked,
                            kind: ChangeEntryKind::EmbeddedRepo,
                        },
                    );
                } else {
                    set.push(
                        ChangeGroup::Untracked,
                        ChangeEntry::file(path, ChangeStatus::Untracked),
                    );
                }
            }
            _ => {}
        }
    }
    set
}

/// The staged (`X`) and unstaged (`Y`) entries of one ordinary or rename
/// record. `modes` is `[mH, mI, mW, hH, hI]`.
fn push_porcelain_sides(
    set: &mut ChangeSet,
    xy: &str,
    sub: &str,
    modes: &[&str],
    path: PathBuf,
    old_path: Option<PathBuf>,
) {
    let xy = xy.as_bytes();
    let (x, y) = (
        xy.first().copied().unwrap_or(b'.'),
        xy.get(1).copied().unwrap_or(b'.'),
    );
    let is_gitlink = modes[..3].contains(&"160000");
    let kind = if is_gitlink {
        let flags = sub.as_bytes();
        ChangeEntryKind::Submodule(SubmoduleChange {
            head_id: non_zero_id(modes[3]),
            index_id: non_zero_id(modes[4]),
            workdir_id: None,
            modified_content: flags.get(2) == Some(&b'M'),
            untracked_content: flags.get(3) == Some(&b'U'),
            registered: true,
            ..Default::default()
        })
    } else {
        ChangeEntryKind::File
    };
    if x != b'.' {
        set.push(
            ChangeGroup::Staged,
            ChangeEntry {
                path: path.clone(),
                old_path: old_path.clone(),
                status: ChangeStatus::from_porcelain(x),
                kind: kind.clone(),
            },
        );
    }
    if y != b'.' {
        set.push(
            ChangeGroup::Unstaged,
            ChangeEntry {
                path,
                old_path: None,
                status: ChangeStatus::from_porcelain(y),
                kind,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// CLI change set
// ---------------------------------------------------------------------------

/// `git status --porcelain=v2` plus one `rev-parse` and one `rev-list
/// --count` per changed submodule, each best-effort (`probe_error`).
pub(super) fn cli_change_set(root: &Path) -> Result<ChangeSet> {
    let output = run_git_command(
        root,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "--renames",
        ],
    )?;
    let mut set = parse_porcelain_v2(output.as_bytes(), |path| is_embedded_repo(root, path));
    let registered = gitmodules_paths(root);
    let mut heads: HashMap<PathBuf, std::result::Result<Option<String>, String>> = HashMap::new();
    for group in [ChangeGroup::Staged, ChangeGroup::Unstaged] {
        for entry in set.group_mut(group) {
            let ChangeEntryKind::Submodule(sm) = &mut entry.kind else {
                continue;
            };
            sm.registered = registered.contains(&entry.path);
            let head = heads
                .entry(entry.path.clone())
                .or_insert_with(|| {
                    if !root.join(&entry.path).join(".git").exists() {
                        return Ok(None);
                    }
                    submodule_head(root, &entry.path)
                        .map(Some)
                        .map_err(|e| e.to_string())
                })
                .clone();
            match head {
                Ok(id) => sm.workdir_id = id,
                Err(err) => sm.probe_error = Some(err),
            }
            let (old, new) = sm.sides(group);
            if let (Some(old), Some(new)) = (old, new)
                && old != new
            {
                match submodule_counts(root, &entry.path, old, new) {
                    Ok((behind, ahead)) => {
                        sm.ahead = ahead;
                        sm.behind = behind;
                    }
                    Err(err) => sm.probe_error = Some(err.to_string()),
                }
            }
        }
    }
    let listed: HashSet<PathBuf> = set.paths().into_iter().collect();
    for path in registered {
        if listed.contains(&path) {
            continue;
        }
        let dir = root.join(&path);
        if dir.join(".git").exists() {
            continue;
        }
        let empty = !dir.exists()
            || std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_none());
        if empty {
            set.uninitialized_submodules.push(path);
        }
    }
    set.sort();
    Ok(set)
}

/// Every `submodule.<name>.path` in `.gitmodules`; empty when there is none.
pub(super) fn gitmodules_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(output) = run_git_command(
        root,
        &[
            "config",
            "--file",
            ".gitmodules",
            "-z",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ],
    ) else {
        return Vec::new();
    };
    output
        .split('\0')
        .filter_map(|record| record.split_once('\n'))
        .map(|(_, value)| PathBuf::from(value))
        .collect()
}

/// `git -C <sub> rev-parse HEAD`: the commit the submodule is checked out at.
pub(super) fn submodule_head(root: &Path, sub: &Path) -> Result<String> {
    let sub = sub.to_string_lossy();
    run_git_command(root, &["-C", &sub, "rev-parse", "HEAD"]).map(|id| id.trim().to_string())
}

/// `(behind, ahead)` of `new` relative to `old` inside the submodule, from
/// `git -C <sub> rev-list --left-right --count <old>...<new>`.
pub(super) fn submodule_counts(
    root: &Path,
    sub: &Path,
    old: &str,
    new: &str,
) -> Result<(usize, usize)> {
    let sub = sub.to_string_lossy();
    let range = format!("{old}...{new}");
    let output = run_git_command(
        root,
        &["-C", &sub, "rev-list", "--left-right", "--count", &range],
    )?;
    let mut counts = output.split_whitespace().map(|n| n.parse::<usize>().ok());
    match (counts.next().flatten(), counts.next().flatten()) {
        (Some(behind), Some(ahead)) => Ok((behind, ahead)),
        _ => Err(TuicrError::VcsCommand(format!(
            "unexpected rev-list output: {}",
            output.trim()
        ))),
    }
}

/// `git -C <sub> log --oneline <old>..<new>`, newest first; empty when the
/// range cannot be walked (an object the checkout does not have yet).
pub(super) fn submodule_log(root: &Path, sub: &Path, old: &str, new: &str) -> Vec<String> {
    let sub = sub.to_string_lossy();
    let range = format!("{old}..{new}");
    let max = format!("--max-count={MAX_SUBMODULE_LOG}");
    run_git_command(
        root,
        &[
            "-C",
            &sub,
            "log",
            "--oneline",
            "--no-decorate",
            &max,
            &range,
        ],
    )
    .map(|out| {
        out.lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// libgit2 change set
// ---------------------------------------------------------------------------

/// One `statuses` pass with rename detection on both sides, submodules
/// probed first through `submodule_status(.., SubmoduleIgnore::None)` so
/// untracked-only content inside a submodule counts, every probe
/// best-effort.
pub(super) fn libgit2_change_set(repo: &Repository, root: &Path) -> Result<ChangeSet> {
    let mut set = ChangeSet::default();
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        .exclude_submodules(false)
        .include_unmodified(false);
    let statuses = repo.statuses(Some(&mut opts))?;

    let subs = repo.submodules().unwrap_or_default();
    let sub_paths: HashSet<PathBuf> = subs.iter().map(|sm| sm.path().to_path_buf()).collect();
    let mut covered: HashSet<PathBuf> = HashSet::new();
    let mut probe_failed: HashMap<PathBuf, String> = HashMap::new();
    for sm in &subs {
        let path = sm.path().to_path_buf();
        let Some(name) = sm.name() else {
            continue;
        };
        let st = match repo.submodule_status(name, SubmoduleIgnore::None) {
            Ok(st) => st,
            Err(e) => {
                probe_failed.insert(path, e.message().to_string());
                continue;
            }
        };
        covered.insert(path.clone());
        // An empty directory, or a `.gitmodules` entry nothing else knows
        // about, is "not checked out" rather than a change.
        if st.is_wd_uninitialized() || (!st.is_in_wd() && !st.is_in_index() && !st.is_in_head()) {
            set.uninitialized_submodules.push(path);
            continue;
        }
        let inner = sm.open().ok();
        let base = SubmoduleChange {
            head_id: sm.head_id().and_then(oid_string),
            index_id: sm.index_id().and_then(oid_string),
            workdir_id: sm.workdir_id().and_then(oid_string),
            modified_content: st.contains(SubmoduleStatus::WD_INDEX_MODIFIED)
                || st.is_wd_wd_modified(),
            untracked_content: st.is_wd_untracked(),
            registered: st.is_in_config(),
            ..Default::default()
        };
        if st.is_index_added() || st.is_index_modified() || st.is_index_deleted() {
            let status = if st.is_index_added() {
                ChangeStatus::Added
            } else if st.is_index_deleted() {
                ChangeStatus::Deleted
            } else {
                ChangeStatus::Modified
            };
            let (ahead, behind) = commits_between(
                inner.as_ref(),
                base.head_id.as_deref(),
                base.index_id.as_deref(),
            );
            set.push(
                ChangeGroup::Staged,
                submodule_entry(
                    &path,
                    status,
                    SubmoduleChange {
                        ahead,
                        behind,
                        ..base.clone()
                    },
                ),
            );
        }
        let wd_changed = st.is_wd_modified()
            || st.is_wd_added()
            || st.is_wd_deleted()
            || st.contains(SubmoduleStatus::WD_INDEX_MODIFIED)
            || st.is_wd_wd_modified()
            || st.is_wd_untracked();
        if wd_changed {
            let status = if st.is_wd_deleted() {
                ChangeStatus::Deleted
            } else if st.is_wd_added() {
                ChangeStatus::Added
            } else {
                ChangeStatus::Modified
            };
            let (ahead, behind) = commits_between(
                inner.as_ref(),
                base.index_id.as_deref(),
                base.workdir_id.as_deref(),
            );
            set.push(
                ChangeGroup::Unstaged,
                submodule_entry(
                    &path,
                    status,
                    SubmoduleChange {
                        ahead,
                        behind,
                        ..base
                    },
                ),
            );
        }
    }

    for entry in statuses.iter() {
        let raw_path = match entry.path() {
            Some(p) => p.to_string(),
            None => String::from_utf8_lossy(entry.path_bytes()).into_owned(),
        };
        let path = PathBuf::from(strip_trailing_slash(&raw_path));
        if covered.contains(&path) {
            continue;
        }
        let s = entry.status();
        if s.is_conflicted() {
            set.push(
                ChangeGroup::Conflicted,
                ChangeEntry::file(path, ChangeStatus::Conflicted),
            );
            continue;
        }
        if s.intersects(
            Status::INDEX_NEW
                | Status::INDEX_MODIFIED
                | Status::INDEX_DELETED
                | Status::INDEX_RENAMED
                | Status::INDEX_TYPECHANGE,
        ) && let Some(delta) = entry.head_to_index()
        {
            set.push(
                ChangeGroup::Staged,
                delta_entry(ChangeGroup::Staged, &delta, &sub_paths, &probe_failed),
            );
        }
        if s.is_wt_new() {
            if is_embedded_repo(root, &path) {
                set.push(
                    ChangeGroup::Untracked,
                    ChangeEntry {
                        path,
                        old_path: None,
                        status: ChangeStatus::Untracked,
                        kind: ChangeEntryKind::EmbeddedRepo,
                    },
                );
            } else {
                set.push(
                    ChangeGroup::Untracked,
                    ChangeEntry::file(path, ChangeStatus::Untracked),
                );
            }
        } else if s.intersects(
            Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_RENAMED | Status::WT_TYPECHANGE,
        ) && let Some(delta) = entry.index_to_workdir()
        {
            set.push(
                ChangeGroup::Unstaged,
                delta_entry(ChangeGroup::Unstaged, &delta, &sub_paths, &probe_failed),
            );
        }
    }
    set.sort();
    Ok(set)
}

fn submodule_entry(path: &Path, status: ChangeStatus, change: SubmoduleChange) -> ChangeEntry {
    ChangeEntry {
        path: path.to_path_buf(),
        old_path: None,
        status,
        kind: ChangeEntryKind::Submodule(change),
    }
}

/// A plain status delta as an entry of `group`. A delta with a gitlink mode
/// on either side is a submodule the probe did not cover: either its probe
/// failed (`probe_error`) or git only knows it from the index / HEAD
/// (`registered: false`). Its two oids are the sides of that group: HEAD →
/// index when staged, index → working tree when unstaged.
fn delta_entry(
    group: ChangeGroup,
    delta: &git2::DiffDelta<'_>,
    sub_paths: &HashSet<PathBuf>,
    probe_failed: &HashMap<PathBuf, String>,
) -> ChangeEntry {
    let old_path = delta.old_file().path().map(PathBuf::from);
    let new_path = delta.new_file().path().map(PathBuf::from);
    let (status, path, old) = match delta.status() {
        Delta::Added | Delta::Untracked => (ChangeStatus::Added, new_path.or(old_path), None),
        Delta::Deleted => (ChangeStatus::Deleted, old_path.or(new_path), None),
        Delta::Renamed => (ChangeStatus::Renamed, new_path, old_path),
        Delta::Copied => (ChangeStatus::Copied, new_path, old_path),
        Delta::Typechange => (ChangeStatus::TypeChanged, new_path.or(old_path), None),
        _ => (ChangeStatus::Modified, new_path.or(old_path), None),
    };
    let path = path.unwrap_or_default();
    let is_gitlink =
        delta.old_file().mode() == FileMode::Commit || delta.new_file().mode() == FileMode::Commit;
    let kind = if is_gitlink {
        let old_id = oid_string(delta.old_file().id());
        let new_id = oid_string(delta.new_file().id());
        let (head_id, index_id, workdir_id) = match group {
            ChangeGroup::Staged => (old_id, new_id, None),
            _ => (None, old_id, new_id),
        };
        ChangeEntryKind::Submodule(SubmoduleChange {
            head_id,
            index_id,
            workdir_id,
            registered: sub_paths.contains(&path),
            probe_error: probe_failed.get(&path).cloned(),
            ..Default::default()
        })
    } else {
        ChangeEntryKind::File
    };
    ChangeEntry {
        path,
        old_path: old,
        status,
        kind,
    }
}

/// `(ahead, behind)`: commits reachable from `new` but not `old`, and the
/// reverse, walked inside the submodule. `(0, 0)` when either side is
/// missing or the walk fails (an object the checkout does not have).
pub(super) fn commits_between(
    inner: Option<&Repository>,
    old: Option<&str>,
    new: Option<&str>,
) -> (usize, usize) {
    let (Some(inner), Some(old), Some(new)) = (inner, old, new) else {
        return (0, 0);
    };
    let (Ok(old), Ok(new)) = (Oid::from_str(old), Oid::from_str(new)) else {
        return (0, 0);
    };
    if old == new {
        return (0, 0);
    }
    let count = |from: Oid, hide: Oid| -> usize {
        let mut walk = match inner.revwalk() {
            Ok(walk) => walk,
            Err(_) => return 0,
        };
        if walk.push(from).is_err() || walk.hide(hide).is_err() {
            return 0;
        }
        walk.filter_map(std::result::Result::ok).count()
    };
    (count(new, old), count(old, new))
}

// ---------------------------------------------------------------------------
// Synthesized diffs
// ---------------------------------------------------------------------------

fn synthesized_line(
    origin: LineOrigin,
    content: String,
    old: Option<u32>,
    new: Option<u32>,
) -> DiffLine {
    DiffLine {
        origin,
        raw_content: content.contains('\t').then(|| content.clone()),
        content: tabify(&content),
        old_lineno: old,
        new_lineno: new,
        highlighted_spans: None,
    }
}

fn synthesized_file(path: &Path, status: FileStatus, hunks: Vec<DiffHunk>) -> DiffFile {
    let content_hash = DiffFile::compute_content_hash(&hunks);
    DiffFile {
        old_path: Some(path.to_path_buf()),
        new_path: Some(path.to_path_buf()),
        status,
        hunks,
        is_binary: false,
        is_too_large: false,
        is_commit_message: false,
        content_hash,
    }
}

/// The preview of a submodule entry: git's `Subproject commit` pair, with
/// `-dirty` when the submodule has modified or untracked content, followed
/// by the commits the pointer moved forward over (`> …`, newest first) and
/// the ones it rewound past (`< …`), and a line per kind of dirt.
pub(super) fn gitlink_diff_file(
    path: &Path,
    old: Option<&str>,
    new: Option<&str>,
    change: &SubmoduleChange,
    forward_log: &[String],
    rewind_log: &[String],
) -> DiffFile {
    let mut lines = Vec::new();
    let (mut old_count, mut new_count) = (0u32, 0u32);
    if let Some(old) = old {
        old_count += 1;
        lines.push(synthesized_line(
            LineOrigin::Deletion,
            format!("Subproject commit {old}"),
            Some(1),
            None,
        ));
    }
    if let Some(new) = new {
        new_count += 1;
        let dirty = if change.dirty() { "-dirty" } else { "" };
        lines.push(synthesized_line(
            LineOrigin::Addition,
            format!("Subproject commit {new}{dirty}"),
            None,
            Some(1),
        ));
    }
    for entry in forward_log {
        new_count += 1;
        lines.push(synthesized_line(
            LineOrigin::Addition,
            format!("> {entry}"),
            None,
            Some(new_count),
        ));
    }
    for entry in rewind_log {
        old_count += 1;
        lines.push(synthesized_line(
            LineOrigin::Deletion,
            format!("< {entry}"),
            Some(old_count),
            None,
        ));
    }
    for (flag, text) in [
        (change.modified_content, "  contains modified content"),
        (change.untracked_content, "  contains untracked content"),
    ] {
        if flag {
            new_count += 1;
            lines.push(synthesized_line(
                LineOrigin::Addition,
                text.to_string(),
                None,
                Some(new_count),
            ));
        }
    }
    let status = match (old, new) {
        (None, _) => FileStatus::Added,
        (_, None) => FileStatus::Deleted,
        _ => FileStatus::Modified,
    };
    let hunks = vec![DiffHunk {
        header: format!("@@ -1,{old_count} +1,{new_count} @@"),
        lines,
        old_start: 1,
        old_count,
        new_start: 1,
        new_count,
    }];
    synthesized_file(path, status, hunks)
}

/// The preview of a conflicted entry: the working-tree file as context
/// lines numbered on both sides, so the markers are visible and the line
/// numbers match what the merge view opens on. A missing file (`DU` /
/// `UD`) yields one context line saying so.
pub(super) fn context_only_diff_file(path: &Path, content: Option<String>) -> DiffFile {
    let lines: Vec<String> = match content {
        Some(content) => content
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect(),
        None => vec!["(file missing from the working tree)".to_string()],
    };
    let hunks = if lines.is_empty() {
        Vec::new()
    } else {
        let count = lines.len() as u32;
        vec![DiffHunk {
            header: format!("@@ -1,{count} +1,{count} @@"),
            lines: lines
                .into_iter()
                .enumerate()
                .map(|(idx, content)| {
                    let n = idx as u32 + 1;
                    synthesized_line(LineOrigin::Context, content, Some(n), Some(n))
                })
                .collect(),
            old_start: 1,
            old_count: count,
            new_start: 1,
            new_count: count,
        }]
    };
    synthesized_file(path, FileStatus::Modified, hunks)
}

/// The preview of an embedded repository: an added path with nothing to
/// show, so the diff pane prints `(no changes)`.
pub(super) fn embedded_repo_diff_file(path: &Path) -> DiffFile {
    synthesized_file(path, FileStatus::Added, Vec::new())
}

/// Entries of an expanded submodule come back with their paths prefixed by
/// the submodule's superproject-relative path.
pub(super) fn prefix_paths(files: &mut [DiffFile], sub: &Path) {
    for file in files {
        if let Some(old) = &file.old_path {
            file.old_path = Some(sub.join(old));
        }
        if let Some(new) = &file.new_path {
            file.new_path = Some(sub.join(new));
        }
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Real temporary repositories with a submodule, in every state the Changes
/// view distinguishes. Shared by the backend tests in this module and the
/// variant-specific ones in `cli.rs` / `libgit2.rs`.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::TempDir;

    use crate::vcs::git::{GitBackend, GitBackendPreference};
    use crate::vcs::traits::DiffWhitespaceMode;

    /// Every git call in these fixtures: identity, no signing, files ref
    /// format, and `protocol.file.allow=always`, which git >= 2.38.1 needs
    /// for `submodule add ../lib`, `clone`, and `submodule update` over
    /// local paths.
    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let (ok, out, err) = git_try(dir, args);
        assert!(ok, "git {} failed: {err}", args.join(" "));
        out
    }

    /// Like [`git`] but reports failure instead of panicking (a merge that
    /// is meant to conflict).
    pub(crate) fn git_try(dir: &Path, args: &[&str]) -> (bool, String, String) {
        let out = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=Tuicr Test",
                "-c",
                "user.email=tuicr@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultRefFormat=files",
                "-c",
                "init.defaultBranch=main",
                "-c",
                "protocol.file.allow=always",
            ])
            .args(args)
            .output()
            .expect("run git");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    pub(crate) fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    pub(crate) fn rev(dir: &Path, spec: &str) -> String {
        git(dir, &["rev-parse", spec]).trim().to_string()
    }

    /// `base/` = superproject with `top.txt` and submodule `lib` (one commit
    /// `lib c1` with `a.txt`), plus the `lib/` source repo next to it.
    pub(crate) fn super_with_submodule() -> (TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("temp dir");
        let lib = temp.path().join("lib");
        fs::create_dir(&lib).unwrap();
        git(&lib, &["init", "-q"]);
        write(&lib, "a.txt", "one\n");
        git(&lib, &["add", "a.txt"]);
        git(&lib, &["commit", "-qm", "lib c1"]);

        let base = temp.path().join("base");
        fs::create_dir(&base).unwrap();
        git(&base, &["init", "-q"]);
        write(&base, "top.txt", "top\n");
        git(&base, &["add", "top.txt"]);
        git(&base, &["commit", "-qm", "super init"]);
        git(&base, &["submodule", "add", "-q", "../lib", "lib"]);
        git(&base, &["commit", "-qm", "add lib submodule"]);
        (temp, base)
    }

    /// A second commit inside the submodule (`b.txt`, `lib c2`), not staged
    /// in the superproject: unstaged pointer, ahead 1. Returns the id of c2.
    pub(crate) fn state_a(root: &Path) -> String {
        let lib = root.join("lib");
        write(&lib, "b.txt", "two\n");
        git(&lib, &["add", "b.txt"]);
        git(&lib, &["commit", "-qm", "lib c2"]);
        rev(&lib, "HEAD")
    }

    /// `state_a` + `git add lib`: staged pointer.
    pub(crate) fn state_b(root: &Path) -> String {
        let c2 = state_a(root);
        git(root, &["add", "lib"]);
        c2
    }

    /// A tracked file modified inside the submodule.
    pub(crate) fn state_c(root: &Path) {
        write(&root.join("lib"), "a.txt", "changed\n");
    }

    /// An untracked file inside the submodule.
    pub(crate) fn state_d(root: &Path) {
        write(&root.join("lib"), "untracked.txt", "junk\n");
    }

    /// `git clone base super_e` without `--recurse-submodules`: `lib/` is an
    /// empty directory.
    pub(crate) fn state_e(temp: &Path, base: &Path) -> PathBuf {
        let clone = temp.join("super_e");
        git(
            temp,
            &[
                "clone",
                "-q",
                base.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        clone
    }

    /// A nested repository `nested/` that is not in `.gitmodules`.
    pub(crate) fn state_f(root: &Path) {
        let nested = root.join("nested");
        fs::create_dir(&nested).unwrap();
        git(&nested, &["init", "-q"]);
        write(&nested, "n.txt", "n\n");
        git(&nested, &["add", "n.txt"]);
        git(&nested, &["commit", "-qm", "nested c1"]);
    }

    /// `state_a` + `state_c` + `state_d`: `SCMU`.
    pub(crate) fn state_g(root: &Path) -> String {
        let c2 = state_a(root);
        state_c(root);
        state_d(root);
        c2
    }

    /// Superproject committed at c2, submodule checked out at c1: rewind 1.
    /// Returns `(c1, c2)`.
    pub(crate) fn state_h(root: &Path) -> (String, String) {
        let c1 = rev(root, "HEAD:lib");
        let c2 = state_b(root);
        git(root, &["commit", "-qm", "bump lib"]);
        git(root, &["-C", "lib", "checkout", "-q", "--detach", &c1]);
        (c1, c2)
    }

    /// Superproject committed at c2, submodule on a branch with its own c2'
    /// off c1: ahead 1, behind 1. Returns `(c2, c2')`.
    pub(crate) fn state_div(root: &Path) -> (String, String) {
        let c1 = rev(root, "HEAD:lib");
        let c2 = state_b(root);
        git(root, &["commit", "-qm", "bump lib"]);
        let lib = root.join("lib");
        git(&lib, &["checkout", "-q", "-b", "alt", &c1]);
        write(&lib, "c.txt", "three\n");
        git(&lib, &["add", "c.txt"]);
        git(&lib, &["commit", "-qm", "lib c2 alt"]);
        (c2, rev(&lib, "HEAD"))
    }

    /// `git rm --cached lib && rm -rf lib`: staged deleted gitlink.
    pub(crate) fn state_del(root: &Path) {
        git(root, &["rm", "-q", "--cached", "lib"]);
        fs::remove_dir_all(root.join("lib")).unwrap();
    }

    /// `rm -rf lib` with the index untouched: unstaged deleted gitlink.
    pub(crate) fn state_gone(root: &Path) {
        fs::remove_dir_all(root.join("lib")).unwrap();
    }

    /// `git init other && git add other`: a staged, unregistered gitlink.
    pub(crate) fn state_new(root: &Path) {
        let other = root.join("other");
        fs::create_dir(&other).unwrap();
        git(&other, &["init", "-q"]);
        write(&other, "o.txt", "o\n");
        git(&other, &["add", "o.txt"]);
        git(&other, &["commit", "-qm", "other c1"]);
        git(root, &["add", "other"]);
    }

    /// A `.gitmodules` entry naming a path that does not exist.
    pub(crate) fn state_broken(root: &Path) {
        git(
            root,
            &[
                "config",
                "--file",
                ".gitmodules",
                "submodule.ghost.path",
                "ghost",
            ],
        );
        git(
            root,
            &[
                "config",
                "--file",
                ".gitmodules",
                "submodule.ghost.url",
                "../ghost",
            ],
        );
    }

    /// Plain files in every state: staged add, unstaged modify, both,
    /// untracked file in a new directory, staged rename, unstaged rename
    /// (delete + untracked pair), staged delete, a path with a space, and a
    /// `.gitignore`d file.
    pub(crate) fn plain_matrix(root: &Path) {
        write(root, "both.txt", "both\n");
        write(root, "del.txt", "del\n");
        write(root, "old name.txt", "old name\n");
        write(root, "moved.txt", "moved\n");
        write(root, ".gitignore", "ignored.txt\n");
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "plain matrix base"]);

        write(root, "added.txt", "added\n");
        git(root, &["add", "added.txt"]);
        write(root, "top.txt", "top\nmore\n");
        write(root, "both.txt", "both\nstaged\n");
        git(root, &["add", "both.txt"]);
        write(root, "both.txt", "both\nstaged\nunstaged\n");
        write(root, "newdir/file.txt", "new\n");
        git(root, &["mv", "old name.txt", "new name.txt"]);
        fs::rename(root.join("moved.txt"), root.join("moved2.txt")).unwrap();
        git(root, &["rm", "-q", "del.txt"]);
        write(root, "file with space.txt", "space\n");
        write(root, "ignored.txt", "ignored\n");
    }

    /// Two branches editing `top.txt`; the merge fails and leaves the file
    /// conflicted.
    pub(crate) fn conflicted(root: &Path) {
        git(root, &["checkout", "-q", "-b", "side"]);
        write(root, "top.txt", "side\n");
        git(root, &["commit", "-qam", "side"]);
        git(root, &["checkout", "-q", "main"]);
        write(root, "top.txt", "main\n");
        git(root, &["commit", "-qam", "main"]);
        let (ok, _, _) = git_try(root, &["merge", "side"]);
        assert!(!ok, "the merge should conflict");
    }

    pub(crate) fn backend(root: &Path, preference: GitBackendPreference) -> GitBackend {
        GitBackend::discover_from(root, preference, DiffWhitespaceMode::default())
            .expect("discover backend")
    }

    pub(crate) const PREFERENCES: [(GitBackendPreference, &str); 2] = [
        (GitBackendPreference::Libgit2, "libgit2"),
        (GitBackendPreference::Cli, "cli"),
    ];

    /// Run a read-only scenario against both backend variants on one tree.
    pub(crate) fn for_each_backend(root: &Path, f: impl Fn(&GitBackend, &'static str)) {
        for (preference, name) in PREFERENCES {
            f(&backend(root, preference), name);
        }
    }

    /// Run a mutating scenario against both variants, each on a fresh
    /// superproject prepared by `setup`.
    pub(crate) fn for_each_backend_fresh(
        setup: impl Fn(&Path),
        f: impl Fn(&GitBackend, &Path, &'static str),
    ) {
        for (preference, name) in PREFERENCES {
            let (_temp, root) = super_with_submodule();
            setup(&root);
            f(&backend(&root, preference), &root, name);
        }
    }

    /// `git status --porcelain=v2` records, one per line, for assertions.
    pub(crate) fn status_v2(root: &Path) -> Vec<String> {
        git(root, &["status", "--porcelain=v2", "--renames"])
            .lines()
            .map(str::to_string)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::fixtures::*;
    use super::*;
    use crate::syntax::SyntaxHighlighter;
    use crate::vcs::changes::{ChangeBulkOp, ChangeDiffKey};
    use crate::vcs::traits::VcsBackend;

    fn paths(entries: &[ChangeEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| e.path.to_string_lossy().into_owned())
            .collect()
    }

    fn find<'a>(set: &'a ChangeSet, group: ChangeGroup, path: &str) -> &'a ChangeEntry {
        set.find(group, Path::new(path))
            .unwrap_or_else(|| panic!("{path} missing from {group:?}: {set:#?}"))
    }

    fn sub<'a>(set: &'a ChangeSet, group: ChangeGroup, path: &str) -> &'a SubmoduleChange {
        find(set, group, path)
            .submodule()
            .unwrap_or_else(|| panic!("{path} is not a submodule entry"))
    }

    fn key(group: ChangeGroup, path: &str) -> ChangeDiffKey {
        ChangeDiffKey::new(group, path)
    }

    fn lines(files: &[DiffFile]) -> Vec<String> {
        files
            .iter()
            .flat_map(|f| f.hunks.iter())
            .flat_map(|h| h.lines.iter())
            .map(|l| {
                let tag = match l.origin {
                    LineOrigin::Addition => '+',
                    LineOrigin::Deletion => '-',
                    LineOrigin::Context => ' ',
                };
                format!("{tag}{}", l.content)
            })
            .collect()
    }

    // --- pure parser -------------------------------------------------------

    #[test]
    fn parse_porcelain_v2_fixture_cases() {
        let c1 = "806ed11a0bd533f10a3198678427393e5d09d9e9";
        let c2 = "40c7ed93c8c1b7cbb61bbdfcd63d32bfae257e61";
        let h = "bf1a1fdefa3c7f4b0180a75a951e9574662a8bc8";
        let input = format!(
            "1 .M SC.. 160000 160000 160000 {c1} {c1} lib\0\
             2 R. N... 100644 100644 100644 {h} {h} R100 new name.txt\0old name.txt\0\
             u UU N... 100644 100644 100644 100644 {h} {h} {h} top.txt\0\
             ? nested/\0\
             ? dir/file with space.txt\0\
             ! build/\0"
        );
        let set = parse_porcelain_v2(input.as_bytes(), |p| p == Path::new("nested"));
        let lib = sub(&set, ChangeGroup::Unstaged, "lib");
        assert_eq!(lib.head_id.as_deref(), Some(c1));
        assert_eq!(lib.index_id.as_deref(), Some(c1));
        assert!(!lib.modified_content && !lib.untracked_content);
        assert!(set.staged.iter().all(|e| e.path != Path::new("lib")));
        let renamed = find(&set, ChangeGroup::Staged, "new name.txt");
        assert_eq!(renamed.status, ChangeStatus::Renamed);
        assert_eq!(renamed.old_path.as_deref(), Some(Path::new("old name.txt")));
        assert_eq!(set.conflicted.len(), 1);
        assert_eq!(set.conflicted[0].path, PathBuf::from("top.txt"));
        let nested = find(&set, ChangeGroup::Untracked, "nested");
        assert!(nested.is_embedded_repo());
        let spaced = find(&set, ChangeGroup::Untracked, "dir/file with space.txt");
        assert_eq!(spaced.status, ChangeStatus::Untracked);
        assert_eq!(set.untracked.len(), 2, "ignored paths are skipped");

        for (record, group, status, modified, untracked) in [
            (
                format!("1 M. S... 160000 160000 160000 {c1} {c2} lib\0"),
                ChangeGroup::Staged,
                ChangeStatus::Modified,
                false,
                false,
            ),
            (
                format!("1 .M S.M. 160000 160000 160000 {c1} {c1} lib\0"),
                ChangeGroup::Unstaged,
                ChangeStatus::Modified,
                true,
                false,
            ),
            (
                format!("1 .M S..U 160000 160000 160000 {c1} {c1} lib\0"),
                ChangeGroup::Unstaged,
                ChangeStatus::Modified,
                false,
                true,
            ),
            (
                format!("1 .M SCMU 160000 160000 160000 {c1} {c1} lib\0"),
                ChangeGroup::Unstaged,
                ChangeStatus::Modified,
                true,
                true,
            ),
            (
                format!("1 .D S... 160000 160000 000000 {c1} {c1} lib\0"),
                ChangeGroup::Unstaged,
                ChangeStatus::Deleted,
                false,
                false,
            ),
        ] {
            let set = parse_porcelain_v2(record.as_bytes(), |_| false);
            assert_eq!(set.len(), 1, "{record}");
            let entry = find(&set, group, "lib");
            assert_eq!(entry.status, status, "{record}");
            let sm = entry.submodule().expect("gitlink");
            assert_eq!(sm.modified_content, modified, "{record}");
            assert_eq!(sm.untracked_content, untracked, "{record}");
        }
        let staged_add = format!(
            "1 A. S... 000000 160000 160000 0000000000000000000000000000000000000000 {c1} nested\0"
        );
        let set = parse_porcelain_v2(staged_add.as_bytes(), |_| false);
        let sm = sub(&set, ChangeGroup::Staged, "nested");
        assert_eq!(sm.head_id, None);
        assert_eq!(sm.index_id.as_deref(), Some(c1));
    }

    #[test]
    fn gitlink_diff_file_layout_is_gits_subproject_hunk() {
        let change = SubmoduleChange {
            ahead: 1,
            modified_content: true,
            untracked_content: true,
            ..Default::default()
        };
        let file = gitlink_diff_file(
            Path::new("lib"),
            Some("aaaa"),
            Some("bbbb"),
            &change,
            &["bbbb lib c2".to_string()],
            &[],
        );
        assert_eq!(file.status, FileStatus::Modified);
        assert_eq!(file.hunks[0].header, "@@ -1,1 +1,4 @@");
        assert_eq!(
            lines(&[file]),
            vec![
                "-Subproject commit aaaa",
                "+Subproject commit bbbb-dirty",
                "+> bbbb lib c2",
                "+  contains modified content",
                "+  contains untracked content",
            ]
        );
        let added = gitlink_diff_file(
            Path::new("lib"),
            None,
            Some("bbbb"),
            &SubmoduleChange::default(),
            &[],
            &[],
        );
        assert_eq!(added.status, FileStatus::Added);
        assert_eq!(added.hunks[0].header, "@@ -1,0 +1,1 @@");
    }

    #[test]
    fn context_only_diff_numbers_both_sides_and_notes_a_missing_file() {
        let file = context_only_diff_file(Path::new("x"), Some("a\nb\n".into()));
        assert_eq!(file.hunks[0].header, "@@ -1,2 +1,2 @@");
        assert_eq!(file.hunks[0].lines[1].old_lineno, Some(2));
        assert_eq!(file.hunks[0].lines[1].new_lineno, Some(2));
        let missing = context_only_diff_file(Path::new("x"), None);
        assert_eq!(
            lines(&[missing]),
            vec![" (file missing from the working tree)"]
        );
    }

    // --- plain files --------------------------------------------------------

    #[test]
    fn change_set_groups_the_plain_matrix() {
        let (_temp, root) = super_with_submodule();
        plain_matrix(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            assert_eq!(
                find(&set, ChangeGroup::Staged, "added.txt").status,
                ChangeStatus::Added,
                "{name}"
            );
            assert_eq!(
                find(&set, ChangeGroup::Staged, "del.txt").status,
                ChangeStatus::Deleted,
                "{name}"
            );
            assert_eq!(
                find(&set, ChangeGroup::Unstaged, "top.txt").status,
                ChangeStatus::Modified,
                "{name}"
            );
            assert!(
                set.find(ChangeGroup::Staged, Path::new("both.txt"))
                    .is_some()
            );
            assert!(
                set.find(ChangeGroup::Unstaged, Path::new("both.txt"))
                    .is_some()
            );
            let untracked = paths(&set.untracked);
            assert!(
                untracked.contains(&"newdir/file.txt".to_string()),
                "{name}: {untracked:?}"
            );
            assert!(
                untracked.contains(&"file with space.txt".to_string()),
                "{name}"
            );
            assert!(!untracked.contains(&"ignored.txt".to_string()), "{name}");
            assert!(set.conflicted.is_empty() && set.uninitialized_submodules.is_empty());
            assert!(
                set.staged.iter().all(|e| e.kind == ChangeEntryKind::File),
                "{name}: no submodule entries in a clean submodule"
            );
        });
    }

    #[test]
    fn renamed_staged_entry_carries_old_path() {
        let (_temp, root) = super_with_submodule();
        plain_matrix(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let renamed = find(&set, ChangeGroup::Staged, "new name.txt");
            assert_eq!(renamed.status, ChangeStatus::Renamed, "{name}");
            assert_eq!(
                renamed.old_path.as_deref(),
                Some(Path::new("old name.txt")),
                "{name}"
            );
            assert_eq!(
                renamed.pathspecs(),
                vec![Path::new("new name.txt"), Path::new("old name.txt")]
            );
        });
    }

    #[test]
    fn staging_an_unstaged_rename_stages_both_paths() {
        // libgit2 pairs a deleted index entry with an untracked file and
        // reports one `R`; git itself never does for untracked targets, so
        // the CLI lists the delete and the untracked file separately. Both
        // routes end at a staged `R` with no leftover ` D`.
        for_each_backend_fresh(plain_matrix, |backend, root, name| {
            let set = backend.get_change_set().unwrap();
            if name == "libgit2" {
                let renamed = find(&set, ChangeGroup::Unstaged, "moved2.txt");
                assert_eq!(renamed.status, ChangeStatus::Renamed);
                assert_eq!(renamed.old_path.as_deref(), Some(Path::new("moved.txt")));
                backend
                    .stage_change_entry(&key(ChangeGroup::Unstaged, "moved2.txt"), renamed)
                    .unwrap();
            } else {
                let deleted = find(&set, ChangeGroup::Unstaged, "moved.txt");
                assert_eq!(deleted.status, ChangeStatus::Deleted);
                let added = find(&set, ChangeGroup::Untracked, "moved2.txt");
                backend
                    .stage_change_entry(&key(ChangeGroup::Unstaged, "moved.txt"), deleted)
                    .unwrap();
                backend
                    .stage_change_entry(&key(ChangeGroup::Untracked, "moved2.txt"), added)
                    .unwrap();
            }
            let status = status_v2(root);
            assert!(
                status
                    .iter()
                    .any(|l| l.starts_with("2 R.") && l.ends_with("moved2.txt\tmoved.txt")),
                "{name}: {status:?}"
            );
            assert!(
                !status.iter().any(|l| l.ends_with(" moved.txt")),
                "{name}: {status:?}"
            );
        });
    }

    #[test]
    fn conflicted_paths_land_in_the_conflicted_group_and_their_diff_shows_markers() {
        let (_temp, root) = super_with_submodule();
        conflicted(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Conflicted, "top.txt");
            assert_eq!(entry.status, ChangeStatus::Conflicted, "{name}");
            assert!(
                set.find(ChangeGroup::Unstaged, Path::new("top.txt"))
                    .is_none()
            );
            let files = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Conflicted, "top.txt"),
                    entry,
                    &SyntaxHighlighter::default(),
                )
                .unwrap();
            let shown = lines(&files);
            assert!(
                shown.iter().any(|l| l.starts_with(" <<<<<<<")),
                "{name}: {shown:?}"
            );
            assert!(
                shown.iter().all(|l| l.starts_with(' ')),
                "{name}: context only"
            );
        });
    }

    #[test]
    fn stage_change_entry_on_a_resolved_conflict_marks_it_resolved() {
        for_each_backend_fresh(conflicted, |backend, root, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Conflicted, "top.txt").clone();
            write(root, "top.txt", "resolved\n");
            backend
                .stage_change_entry(&key(ChangeGroup::Conflicted, "top.txt"), &entry)
                .unwrap();
            let after = backend.get_change_set().unwrap();
            assert!(after.conflicted.is_empty(), "{name}: {after:#?}");
            assert_eq!(
                find(&after, ChangeGroup::Staged, "top.txt").status,
                ChangeStatus::Modified
            );
        });
    }

    #[test]
    fn stage_change_entry_refuses_a_file_with_markers() {
        for_each_backend_fresh(conflicted, |backend, root, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Conflicted, "top.txt").clone();
            let err = backend
                .stage_change_entry(&key(ChangeGroup::Conflicted, "top.txt"), &entry)
                .unwrap_err();
            assert!(
                matches!(&err, TuicrError::InvalidInput(m) if m == "top.txt still has conflict markers"),
                "{name}: {err}"
            );
            assert!(status_v2(root).iter().any(|l| l.starts_with("u UU")));
        });
    }

    #[test]
    fn typechange_is_reported_as_t() {
        #[cfg(unix)]
        {
            let (_temp, root) = super_with_submodule();
            std::fs::remove_file(root.join("top.txt")).unwrap();
            std::os::unix::fs::symlink("lib", root.join("top.txt")).unwrap();
            for_each_backend(&root, |backend, name| {
                let set = backend.get_change_set().unwrap();
                let entry = find(&set, ChangeGroup::Unstaged, "top.txt");
                assert_eq!(entry.status, ChangeStatus::TypeChanged, "{name}");
                assert_eq!(entry.status.glyph(), 'T');
            });
        }
    }

    // --- entry diffs --------------------------------------------------------

    #[test]
    fn entry_diffs_distinguish_staged_unstaged_and_untracked() {
        let (_temp, root) = super_with_submodule();
        plain_matrix(&root);
        let hl = SyntaxHighlighter::default();
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let staged = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Staged, "both.txt"),
                    find(&set, ChangeGroup::Staged, "both.txt"),
                    &hl,
                )
                .unwrap();
            assert_eq!(staged.len(), 1);
            assert_eq!(lines(&staged), vec![" both", "+staged"], "{name}");
            let unstaged = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Unstaged, "both.txt"),
                    find(&set, ChangeGroup::Unstaged, "both.txt"),
                    &hl,
                )
                .unwrap();
            assert_eq!(
                lines(&unstaged),
                vec![" both", " staged", "+unstaged"],
                "{name}"
            );
            let untracked = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Untracked, "newdir/file.txt"),
                    find(&set, ChangeGroup::Untracked, "newdir/file.txt"),
                    &hl,
                )
                .unwrap();
            assert_eq!(untracked[0].status, FileStatus::Added);
            assert_eq!(lines(&untracked), vec!["+new"], "{name}");
            assert_eq!(
                untracked[0].display_path(),
                &PathBuf::from("newdir/file.txt")
            );
            let renamed = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Staged, "new name.txt"),
                    find(&set, ChangeGroup::Staged, "new name.txt"),
                    &hl,
                )
                .unwrap();
            assert_eq!(
                renamed.len(),
                1,
                "{name}: one rename, not a delete plus an add"
            );
            assert_eq!(renamed[0].status, FileStatus::Renamed, "{name}");
            assert_eq!(
                renamed[0].old_path.as_deref(),
                Some(Path::new("old name.txt"))
            );
            let deleted = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Staged, "del.txt"),
                    find(&set, ChangeGroup::Staged, "del.txt"),
                    &hl,
                )
                .unwrap();
            assert_eq!(deleted[0].status, FileStatus::Deleted);
            assert_eq!(lines(&deleted), vec!["-del"], "{name}");
        });
    }

    #[test]
    fn entry_diff_with_a_stale_row_kind_returns_no_changes() {
        let (_temp, root) = super_with_submodule();
        plain_matrix(&root);
        for_each_backend(&root, |backend, name| {
            // A row that claims `top.txt` is a submodule: the gitlink probe
            // says otherwise, so nothing is synthesized.
            let stale = ChangeEntry {
                path: PathBuf::from("top.txt"),
                old_path: None,
                status: ChangeStatus::Modified,
                kind: ChangeEntryKind::Submodule(SubmoduleChange {
                    index_id: Some("a".repeat(40)),
                    workdir_id: Some("b".repeat(40)),
                    ..Default::default()
                }),
            };
            let err = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Unstaged, "top.txt"),
                    &stale,
                    &SyntaxHighlighter::default(),
                )
                .unwrap_err();
            assert!(matches!(err, TuicrError::NoChanges), "{name}: {err}");
        });
    }

    // --- submodules ---------------------------------------------------------

    #[test]
    fn submodule_with_new_commits_is_an_unstaged_submodule_entry_with_ahead_one() {
        let (_temp, root) = super_with_submodule();
        let c2 = state_a(&root);
        let c1 = rev(&root, "HEAD:lib");
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            assert_eq!(set.len(), 1, "{name}: {set:#?}");
            let entry = find(&set, ChangeGroup::Unstaged, "lib");
            assert_eq!(entry.status, ChangeStatus::Modified, "{name}");
            let sm = entry.submodule().unwrap();
            assert_eq!(sm.head_id.as_deref(), Some(c1.as_str()), "{name}");
            assert_eq!(sm.index_id.as_deref(), Some(c1.as_str()), "{name}");
            assert_eq!(sm.workdir_id.as_deref(), Some(c2.as_str()), "{name}");
            assert_eq!((sm.ahead, sm.behind), (1, 0), "{name}");
            assert!(sm.registered && sm.probe_error.is_none(), "{name}");
            assert!(sm.pointer_moved(ChangeGroup::Unstaged));
            assert!(!entry.expandable(), "{name}");
            assert_eq!(sm.detail(), "+1 commit");
        });
    }

    #[test]
    fn staged_submodule_pointer_is_a_staged_submodule_entry() {
        let (_temp, root) = super_with_submodule();
        let c2 = state_b(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let sm = sub(&set, ChangeGroup::Staged, "lib");
            assert_eq!(sm.index_id.as_deref(), Some(c2.as_str()), "{name}");
            assert_eq!((sm.ahead, sm.behind), (1, 0), "{name}");
            assert!(sm.pointer_moved(ChangeGroup::Staged));
            assert!(set.unstaged.is_empty(), "{name}: {:?}", set.unstaged);
        });
    }

    #[test]
    fn submodule_with_modified_tracked_content_is_dirty_and_expandable() {
        let (_temp, root) = super_with_submodule();
        state_c(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Unstaged, "lib");
            let sm = entry.submodule().unwrap();
            assert!(sm.modified_content && !sm.untracked_content, "{name}");
            assert!(!sm.pointer_moved(ChangeGroup::Unstaged), "{name}");
            assert!(entry.expandable(), "{name}");
            assert!(!entry.stageable(ChangeGroup::Unstaged), "{name}");
            assert_eq!(sm.detail(), "modified content");
        });
    }

    #[test]
    fn submodule_with_untracked_only_content_is_dirty_on_both_backends() {
        let (_temp, root) = super_with_submodule();
        state_d(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let sm = sub(&set, ChangeGroup::Unstaged, "lib");
            assert!(sm.untracked_content && !sm.modified_content, "{name}");
            assert_eq!(sm.detail(), "untracked content");
        });
    }

    #[test]
    fn uninitialized_submodule_is_listed_as_uninitialized_not_as_a_change() {
        let (temp, base) = super_with_submodule();
        let clone = state_e(temp.path(), &base);
        for_each_backend(&clone, |backend, name| {
            let set = backend.get_change_set().unwrap();
            assert!(set.is_empty(), "{name}: {set:#?}");
            assert_eq!(
                set.uninitialized_submodules,
                vec![PathBuf::from("lib")],
                "{name}"
            );
        });
    }

    #[test]
    fn rewound_submodule_reports_behind_one_and_its_diff_rewinds() {
        let (_temp, root) = super_with_submodule();
        let (c1, c2) = state_h(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Unstaged, "lib");
            let sm = entry.submodule().unwrap();
            assert_eq!(sm.index_id.as_deref(), Some(c2.as_str()), "{name}");
            assert_eq!(sm.workdir_id.as_deref(), Some(c1.as_str()), "{name}");
            assert_eq!((sm.ahead, sm.behind), (0, 1), "{name}");
            assert_eq!(sm.detail(), "rewound 1 commit");
            let files = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Unstaged, "lib"),
                    entry,
                    &SyntaxHighlighter::default(),
                )
                .unwrap();
            let shown = lines(&files);
            assert_eq!(shown[0], format!("-Subproject commit {c2}"), "{name}");
            assert_eq!(shown[1], format!("+Subproject commit {c1}"), "{name}");
            assert!(
                shown[2].starts_with("-< ") && shown[2].ends_with("lib c2"),
                "{name}: {shown:?}"
            );
            assert_eq!(files[0].hunks[0].header, "@@ -1,2 +1,1 @@");
        });
    }

    #[test]
    fn diverged_submodule_diff_lists_both_directions() {
        let (_temp, root) = super_with_submodule();
        let (c2, alt) = state_div(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Unstaged, "lib");
            let sm = entry.submodule().unwrap();
            assert_eq!((sm.ahead, sm.behind), (1, 1), "{name}");
            assert_eq!(sm.workdir_id.as_deref(), Some(alt.as_str()));
            let files = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Unstaged, "lib"),
                    entry,
                    &SyntaxHighlighter::default(),
                )
                .unwrap();
            let shown = lines(&files);
            assert_eq!(shown[0], format!("-Subproject commit {c2}"));
            assert!(
                shown
                    .iter()
                    .any(|l| l.starts_with("+> ") && l.ends_with("lib c2 alt")),
                "{name}: {shown:?}"
            );
            assert!(
                shown
                    .iter()
                    .any(|l| l.starts_with("-< ") && l.ends_with("lib c2")),
                "{name}: {shown:?}"
            );
            assert_eq!(files[0].hunks[0].header, "@@ -1,2 +1,2 @@");
        });
    }

    #[test]
    fn deleted_gitlink_is_a_staged_deleted_submodule_entry() {
        let (_temp, root) = super_with_submodule();
        let c1 = rev(&root, "HEAD:lib");
        state_del(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Staged, "lib");
            assert_eq!(entry.status, ChangeStatus::Deleted, "{name}");
            let sm = entry.submodule().unwrap();
            assert_eq!(sm.head_id.as_deref(), Some(c1.as_str()), "{name}");
            assert_eq!(sm.index_id, None, "{name}");
            assert_eq!(sm.workdir_id, None, "{name}");
            assert!(sm.registered, "{name}: rm --cached keeps .gitmodules");
            assert!(entry.stageable(ChangeGroup::Staged));
            assert!(
                set.unstaged.is_empty() && set.uninitialized_submodules.is_empty(),
                "{name}"
            );
        });
    }

    #[test]
    fn missing_submodule_directory_is_an_unstaged_deleted_entry_and_discard_restores_it() {
        for_each_backend_fresh(state_gone, |backend, root, name| {
            let c1 = rev(root, "HEAD:lib");
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Unstaged, "lib").clone();
            assert_eq!(entry.status, ChangeStatus::Deleted, "{name}");
            let sm = entry.submodule().unwrap();
            assert_eq!(sm.workdir_id, None, "{name}");
            assert_eq!(sm.index_id.as_deref(), Some(c1.as_str()), "{name}");
            assert!(sm.registered && sm.probe_error.is_none(), "{name}");
            assert!(set.uninitialized_submodules.is_empty(), "{name}");
            backend
                .discard_change_entry(&key(ChangeGroup::Unstaged, "lib"), &entry)
                .unwrap();
            assert_eq!(rev(&root.join("lib"), "HEAD"), c1, "{name}");
            assert!(backend.get_change_set().unwrap().is_empty(), "{name}");
        });
    }

    #[test]
    fn discard_of_a_staged_deleted_gitlink_restores_the_checkout() {
        for_each_backend_fresh(state_del, |backend, root, name| {
            let c1 = rev(root, "HEAD:lib");
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Staged, "lib").clone();
            backend
                .discard_change_entry(&key(ChangeGroup::Staged, "lib"), &entry)
                .unwrap();
            assert_eq!(rev(&root.join("lib"), "HEAD"), c1, "{name}");
            assert!(backend.get_change_set().unwrap().is_empty(), "{name}");
        });
    }

    #[test]
    fn discard_of_an_unregistered_deleted_gitlink_is_refused() {
        for_each_backend_fresh(
            |root| {
                state_new(root);
                git(root, &["commit", "-qm", "embed other"]);
                std::fs::remove_dir_all(root.join("other")).unwrap();
            },
            |backend, _root, name| {
                let set = backend.get_change_set().unwrap();
                let entry = find(&set, ChangeGroup::Unstaged, "other").clone();
                assert_eq!(entry.status, ChangeStatus::Deleted, "{name}");
                assert!(!entry.submodule().unwrap().registered, "{name}");
                let err = backend
                    .discard_change_entry(&key(ChangeGroup::Unstaged, "other"), &entry)
                    .unwrap_err();
                assert_eq!(
                    err.to_string(),
                    "Unsupported operation: other was removed and is not in .gitmodules; restore it by hand",
                    "{name}"
                );
            },
        );
    }

    #[test]
    fn broken_gitmodules_entry_does_not_fail_the_change_set() {
        let (_temp, root) = super_with_submodule();
        state_broken(&root);
        state_a(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            assert!(
                set.find(ChangeGroup::Unstaged, Path::new("lib")).is_some(),
                "{name}"
            );
            assert!(
                set.find(ChangeGroup::Unstaged, Path::new(".gitmodules"))
                    .is_some(),
                "{name}"
            );
            assert!(
                set.uninitialized_submodules
                    .contains(&PathBuf::from("ghost")),
                "{name}: {:?}",
                set.uninitialized_submodules
            );
        });
    }

    #[test]
    fn sc_mu_submodule_combines_pointer_and_dirt() {
        let (_temp, root) = super_with_submodule();
        let c2 = state_g(&root);
        let c1 = rev(&root, "HEAD:lib");
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Unstaged, "lib");
            let sm = entry.submodule().unwrap();
            assert_eq!((sm.ahead, sm.behind), (1, 0), "{name}");
            assert!(sm.modified_content && sm.untracked_content, "{name}");
            assert_eq!(
                sm.detail(),
                "+1 commit, modified content, untracked content"
            );
            assert!(entry.expandable() && entry.stageable(ChangeGroup::Unstaged));
            let files = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Unstaged, "lib"),
                    entry,
                    &SyntaxHighlighter::default(),
                )
                .unwrap();
            let shown = lines(&files);
            assert_eq!(shown[0], format!("-Subproject commit {c1}"));
            assert_eq!(shown[1], format!("+Subproject commit {c2}-dirty"), "{name}");
            assert_eq!(shown[3], "+  contains modified content");
            assert_eq!(shown[4], "+  contains untracked content");
        });
    }

    #[test]
    fn gitlink_entry_diff_lists_the_commits_between() {
        let (_temp, root) = super_with_submodule();
        let c2 = state_b(&root);
        let c1 = rev(&root, "HEAD:lib");
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Staged, "lib");
            let files = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Staged, "lib"),
                    entry,
                    &SyntaxHighlighter::default(),
                )
                .unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].display_path(), &PathBuf::from("lib"));
            assert_eq!(files[0].hunks[0].header, "@@ -1,1 +1,2 @@", "{name}");
            let shown = lines(&files);
            assert_eq!(shown[0], format!("-Subproject commit {c1}"));
            assert_eq!(shown[1], format!("+Subproject commit {c2}"));
            assert_eq!(shown[2], format!("+> {} lib c2", &c2[..7]), "{name}");
        });
    }

    #[test]
    fn embedded_repo_is_an_untracked_entry_of_kind_embedded_repo() {
        let (_temp, root) = super_with_submodule();
        state_f(&root);
        for_each_backend(&root, |backend, name| {
            let set = backend.get_change_set().unwrap();
            assert_eq!(set.len(), 1, "{name}: {set:#?}");
            let entry = find(&set, ChangeGroup::Untracked, "nested");
            assert!(entry.is_embedded_repo(), "{name}");
            assert_eq!(entry.status, ChangeStatus::Untracked);
            assert!(!entry.stageable(ChangeGroup::Untracked));
            let files = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Untracked, "nested"),
                    entry,
                    &SyntaxHighlighter::default(),
                )
                .unwrap();
            assert_eq!(files[0].status, FileStatus::Added);
            assert!(files[0].hunks.is_empty());
            let err = backend
                .stage_change_entry(&key(ChangeGroup::Untracked, "nested"), entry)
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                "Unsupported operation: nested is a git repository; add it with git submodule add, or remove its .git"
            );
            let err = backend
                .discard_change_entry(&key(ChangeGroup::Untracked, "nested"), entry)
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                "Unsupported operation: nested is a git repository; delete it by hand"
            );
        });
    }

    #[test]
    fn added_gitlink_from_an_embedded_repo_is_unregistered() {
        for_each_backend_fresh(state_new, |backend, root, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Staged, "other").clone();
            assert_eq!(entry.status, ChangeStatus::Added, "{name}");
            let sm = entry.submodule().unwrap();
            assert!(!sm.registered, "{name}");
            assert_eq!(sm.head_id, None);
            assert!(sm.index_id.is_some(), "{name}");
            assert_eq!(sm.detail(), "(not in .gitmodules)");
            let files = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Staged, "other"),
                    &entry,
                    &SyntaxHighlighter::default(),
                )
                .unwrap();
            assert_eq!(files[0].status, FileStatus::Added, "{name}");
            backend
                .discard_change_entry(&key(ChangeGroup::Staged, "other"), &entry)
                .unwrap();
            assert!(root.join("other/.git").exists(), "{name}: directory kept");
            let after = backend.get_change_set().unwrap();
            assert!(after.staged.is_empty(), "{name}: {after:#?}");
            assert!(find(&after, ChangeGroup::Untracked, "other").is_embedded_repo());
        });
    }

    #[test]
    fn submodule_change_set_lists_inner_files_and_entry_diffs_prefix_the_path() {
        let (_temp, root) = super_with_submodule();
        state_c(&root);
        for_each_backend(&root, |backend, name| {
            let inner = backend.get_submodule_change_set(Path::new("lib")).unwrap();
            assert_eq!(inner.len(), 1, "{name}: {inner:#?}");
            let entry = find(&inner, ChangeGroup::Unstaged, "a.txt");
            assert_eq!(entry.status, ChangeStatus::Modified);
            let files = backend
                .get_change_entry_diff(
                    &ChangeDiffKey::in_submodule(ChangeGroup::Unstaged, "lib", "a.txt"),
                    entry,
                    &SyntaxHighlighter::default(),
                )
                .unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(
                files[0].display_path(),
                &PathBuf::from("lib/a.txt"),
                "{name}"
            );
            assert_eq!(files[0].hunks.len(), 1);
            assert_eq!(lines(&files), vec!["-one", "+changed"], "{name}");
            let err = backend
                .get_submodule_change_set(Path::new("nope"))
                .unwrap_err();
            assert!(err.to_string().contains("not checked out"), "{name}: {err}");
        });
    }

    #[test]
    fn uninitialized_submodule_has_no_inner_change_set() {
        let (temp, base) = super_with_submodule();
        let clone = state_e(temp.path(), &base);
        for_each_backend(&clone, |backend, name| {
            // The empty directory must not resolve to the superproject.
            let err = backend
                .get_submodule_change_set(Path::new("lib"))
                .unwrap_err();
            assert!(err.to_string().contains("not checked out"), "{name}: {err}");
        });
    }

    // --- staging and unstaging ---------------------------------------------

    #[test]
    fn stage_change_entry_on_a_gitlink_stages_the_pointer() {
        for_each_backend_fresh(
            |root| {
                state_a(root);
            },
            |backend, root, name| {
                let set = backend.get_change_set().unwrap();
                let entry = find(&set, ChangeGroup::Unstaged, "lib").clone();
                backend
                    .stage_change_entry(&key(ChangeGroup::Unstaged, "lib"), &entry)
                    .unwrap();
                let after = backend.get_change_set().unwrap();
                assert!(
                    find(&after, ChangeGroup::Staged, "lib").is_submodule(),
                    "{name}"
                );
                assert!(after.unstaged.is_empty(), "{name}");
                assert!(status_v2(root)[0].starts_with("1 M. S..."), "{name}");
            },
        );
    }

    #[test]
    fn unstage_change_entry_on_a_gitlink_unstages_it() {
        for_each_backend_fresh(
            |root| {
                state_b(root);
            },
            |backend, root, name| {
                let set = backend.get_change_set().unwrap();
                let entry = find(&set, ChangeGroup::Staged, "lib").clone();
                backend
                    .unstage_change_entry(&key(ChangeGroup::Staged, "lib"), &entry)
                    .unwrap();
                let after = backend.get_change_set().unwrap();
                assert!(after.staged.is_empty(), "{name}");
                assert_eq!(sub(&after, ChangeGroup::Unstaged, "lib").ahead, 1, "{name}");
                assert!(status_v2(root)[0].starts_with("1 .M SC.."), "{name}");
            },
        );
    }

    #[test]
    fn stage_change_entry_refuses_a_dirty_only_submodule() {
        for_each_backend_fresh(state_c, |backend, _root, name| {
            let set = backend.get_change_set().unwrap();
            let entry = find(&set, ChangeGroup::Unstaged, "lib").clone();
            let err = backend
                .stage_change_entry(&key(ChangeGroup::Unstaged, "lib"), &entry)
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                "Unsupported operation: lib has uncommitted changes inside it; commit there first",
                "{name}"
            );
        });
    }

    #[test]
    fn stage_inside_a_submodule_stages_in_the_inner_repo() {
        for_each_backend_fresh(state_c, |backend, root, name| {
            let inner = backend.get_submodule_change_set(Path::new("lib")).unwrap();
            let entry = find(&inner, ChangeGroup::Unstaged, "a.txt").clone();
            let key = ChangeDiffKey::in_submodule(ChangeGroup::Unstaged, "lib", "a.txt");
            backend.stage_change_entry(&key, &entry).unwrap();
            let staged = git(&root.join("lib"), &["diff", "--cached", "--name-only"]);
            assert_eq!(staged.trim(), "a.txt", "{name}");
            assert!(
                git(root, &["diff", "--cached", "--name-only"])
                    .trim()
                    .is_empty(),
                "{name}: the superproject index is untouched"
            );
            let inner = backend.get_submodule_change_set(Path::new("lib")).unwrap();
            let entry = find(&inner, ChangeGroup::Staged, "a.txt").clone();
            backend
                .unstage_change_entry(
                    &ChangeDiffKey::in_submodule(ChangeGroup::Staged, "lib", "a.txt"),
                    &entry,
                )
                .unwrap();
            assert!(
                git(&root.join("lib"), &["diff", "--cached", "--name-only"])
                    .trim()
                    .is_empty(),
                "{name}"
            );
        });
    }

    // --- discard ------------------------------------------------------------

    #[test]
    fn discard_change_entry_on_an_unstaged_gitlink_checks_the_submodule_out_at_the_index_commit() {
        for_each_backend_fresh(
            |root| {
                state_a(root);
            },
            |backend, root, name| {
                let c1 = rev(root, "HEAD:lib");
                let set = backend.get_change_set().unwrap();
                let entry = find(&set, ChangeGroup::Unstaged, "lib").clone();
                crate::git_console::clear();
                backend
                    .discard_change_entry(&key(ChangeGroup::Unstaged, "lib"), &entry)
                    .unwrap();
                assert_eq!(rev(&root.join("lib"), "HEAD"), c1, "{name}");
                assert!(backend.get_change_set().unwrap().is_empty(), "{name}");
                let logged: Vec<String> = crate::git_console::snapshot()
                    .into_iter()
                    .map(|e| e.command)
                    .collect();
                assert!(
                    logged
                        .iter()
                        .any(|c| c.starts_with("git -C lib checkout -q --detach")),
                    "{name}: {logged:?}"
                );
            },
        );
    }

    #[test]
    fn discard_change_entry_on_a_staged_gitlink_resets_pointer_and_checkout_to_head() {
        for_each_backend_fresh(
            |root| {
                state_b(root);
            },
            |backend, root, name| {
                let c1 = rev(root, "HEAD:lib");
                let set = backend.get_change_set().unwrap();
                let entry = find(&set, ChangeGroup::Staged, "lib").clone();
                backend
                    .discard_change_entry(&key(ChangeGroup::Staged, "lib"), &entry)
                    .unwrap();
                assert_eq!(rev(root, ":lib"), c1, "{name}: index pointer");
                assert_eq!(rev(&root.join("lib"), "HEAD"), c1, "{name}: checkout");
                assert!(backend.get_change_set().unwrap().is_empty(), "{name}");
            },
        );
    }

    #[test]
    fn discard_change_entry_on_a_dirty_submodule_cleans_its_worktree() {
        for_each_backend_fresh(
            |root| {
                state_c(root);
                state_d(root);
            },
            |backend, root, name| {
                let set = backend.get_change_set().unwrap();
                let entry = find(&set, ChangeGroup::Unstaged, "lib").clone();
                assert!(entry.submodule().unwrap().dirty());
                backend
                    .discard_change_entry(&key(ChangeGroup::Unstaged, "lib"), &entry)
                    .unwrap();
                assert_eq!(
                    git(&root.join("lib"), &["status", "--porcelain"]).trim(),
                    "",
                    "{name}"
                );
                assert!(backend.get_change_set().unwrap().is_empty(), "{name}");
            },
        );
    }

    #[test]
    fn discard_keeps_unrelated_modified_content_when_only_the_pointer_moved() {
        for_each_backend_fresh(
            |root| {
                state_a(root);
            },
            |backend, root, name| {
                let c1 = rev(root, "HEAD:lib");
                // The row was read before the edit, so it says "pointer
                // moved, not dirty": the checkout runs without `--force` and
                // an edit the move does not touch survives.
                let set = backend.get_change_set().unwrap();
                let entry = find(&set, ChangeGroup::Unstaged, "lib").clone();
                write(&root.join("lib"), "a.txt", "edited later\n");
                backend
                    .discard_change_entry(&key(ChangeGroup::Unstaged, "lib"), &entry)
                    .unwrap();
                assert_eq!(rev(&root.join("lib"), "HEAD"), c1, "{name}");
                assert_eq!(
                    std::fs::read_to_string(root.join("lib/a.txt")).unwrap(),
                    "edited later\n",
                    "{name}"
                );
            },
        );
    }

    #[test]
    fn discard_change_entry_on_plain_files_follows_the_matrix() {
        for_each_backend_fresh(plain_matrix, |backend, root, name| {
            let hl = SyntaxHighlighter::default();
            let set = backend.get_change_set().unwrap();
            let discard = |group, path: &str| {
                let entry = find(&set, group, path).clone();
                backend
                    .discard_change_entry(&key(group, path), &entry)
                    .unwrap();
            };
            discard(ChangeGroup::Staged, "added.txt");
            assert!(
                !root.join("added.txt").exists(),
                "{name}: staged add deleted"
            );
            discard(ChangeGroup::Staged, "new name.txt");
            assert!(
                root.join("old name.txt").exists(),
                "{name}: rename source restored"
            );
            assert!(
                !root.join("new name.txt").exists(),
                "{name}: rename target deleted"
            );
            discard(ChangeGroup::Staged, "del.txt");
            assert!(
                root.join("del.txt").exists(),
                "{name}: staged delete restored"
            );
            discard(ChangeGroup::Untracked, "file with space.txt");
            assert!(!root.join("file with space.txt").exists(), "{name}");
            discard(ChangeGroup::Unstaged, "top.txt");
            assert_eq!(
                std::fs::read_to_string(root.join("top.txt")).unwrap(),
                "top\n"
            );
            // The both-sides file: discarding the unstaged half keeps the
            // staged half.
            discard(ChangeGroup::Unstaged, "both.txt");
            assert_eq!(
                std::fs::read_to_string(root.join("both.txt")).unwrap(),
                "both\nstaged\n",
                "{name}"
            );
            let after = backend.get_change_set().unwrap();
            assert!(
                after
                    .unstaged
                    .iter()
                    .all(|e| e.path != Path::new("both.txt"))
            );
            let staged = backend
                .get_change_entry_diff(
                    &key(ChangeGroup::Staged, "both.txt"),
                    find(&after, ChangeGroup::Staged, "both.txt"),
                    &hl,
                )
                .unwrap();
            assert_eq!(lines(&staged), vec![" both", "+staged"], "{name}");
        });
    }

    #[test]
    fn discard_change_group_unstaged_restores_every_file_and_counts_skips() {
        for_each_backend_fresh(
            |root| {
                plain_matrix(root);
                state_c(root);
            },
            |backend, root, name| {
                let set = backend.get_change_set().unwrap();
                let skipped = backend
                    .discard_change_group(ChangeGroup::Unstaged, &set.unstaged, None)
                    .unwrap();
                assert_eq!(skipped, 1, "{name}: the dirty-only submodule");
                let after = backend.get_change_set().unwrap();
                assert_eq!(paths(&after.unstaged), vec!["lib"], "{name}: {after:#?}");
                assert_eq!(
                    std::fs::read_to_string(root.join("top.txt")).unwrap(),
                    "top\n"
                );
                assert_eq!(
                    std::fs::read_to_string(root.join("lib/a.txt")).unwrap(),
                    "changed\n",
                    "{name}: skipped means untouched"
                );
                let skipped = backend
                    .discard_change_group(ChangeGroup::Untracked, &after.untracked, None)
                    .unwrap();
                assert_eq!(skipped, 0, "{name}");
                let after = backend.get_change_set().unwrap();
                assert!(after.untracked.is_empty(), "{name}: {:?}", after.untracked);
                assert!(
                    !root.join("newdir").exists()
                        || root.join("newdir").read_dir().unwrap().next().is_none()
                );
            },
        );
    }

    #[test]
    fn discard_change_group_untracked_keeps_embedded_repos() {
        for_each_backend_fresh(
            |root| {
                state_f(root);
                write(root, "junk.txt", "junk\n");
            },
            |backend, root, name| {
                let set = backend.get_change_set().unwrap();
                let skipped = backend
                    .discard_change_group(ChangeGroup::Untracked, &set.untracked, None)
                    .unwrap();
                assert_eq!(skipped, 1, "{name}");
                assert!(root.join("nested/.git").exists(), "{name}");
                assert!(!root.join("junk.txt").exists(), "{name}");
            },
        );
    }

    #[test]
    fn discard_change_group_inside_a_submodule_runs_in_the_inner_repo() {
        for_each_backend_fresh(
            |root| {
                state_c(root);
                write(root, "top.txt", "outer edit\n");
            },
            |backend, root, name| {
                let inner = backend.get_submodule_change_set(Path::new("lib")).unwrap();
                let skipped = backend
                    .discard_change_group(
                        ChangeGroup::Unstaged,
                        &inner.unstaged,
                        Some(Path::new("lib")),
                    )
                    .unwrap();
                assert_eq!(skipped, 0);
                assert_eq!(
                    std::fs::read_to_string(root.join("lib/a.txt")).unwrap(),
                    "one\n",
                    "{name}"
                );
                assert_eq!(
                    std::fs::read_to_string(root.join("top.txt")).unwrap(),
                    "outer edit\n",
                    "{name}: the superproject is untouched"
                );
            },
        );
    }

    // --- bulk ops -----------------------------------------------------------

    fn staged_names(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = git(root, &["diff", "--cached", "--name-only"])
            .lines()
            .map(str::to_string)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn bulk_stage_tracked_leaves_untracked_alone() {
        for_each_backend_fresh(plain_matrix, |backend, root, name| {
            backend
                .change_bulk_op(ChangeBulkOp::StageTracked, None)
                .unwrap();
            let after = backend.get_change_set().unwrap();
            assert!(after.unstaged.is_empty(), "{name}: {:?}", after.unstaged);
            assert!(
                paths(&after.untracked).contains(&"newdir/file.txt".to_string()),
                "{name}"
            );
            assert!(
                staged_names(root).contains(&"top.txt".to_string()),
                "{name}"
            );
        });
    }

    #[test]
    fn bulk_stage_untracked_stages_only_untracked_files_excluding_embedded_repos() {
        for_each_backend_fresh(
            |root| {
                plain_matrix(root);
                state_f(root);
            },
            |backend, root, name| {
                backend
                    .change_bulk_op(ChangeBulkOp::StageUntracked, None)
                    .unwrap();
                let after = backend.get_change_set().unwrap();
                assert_eq!(
                    paths(&after.untracked),
                    vec!["nested"],
                    "{name}: {after:#?}"
                );
                assert_eq!(
                    find(&after, ChangeGroup::Unstaged, "top.txt").status,
                    ChangeStatus::Modified,
                    "{name}: tracked edits stay unstaged"
                );
                let staged = staged_names(root);
                assert!(
                    staged.contains(&"newdir/file.txt".to_string()),
                    "{name}: {staged:?}"
                );
                assert!(
                    staged.contains(&"file with space.txt".to_string()),
                    "{name}"
                );
                assert!(!staged.iter().any(|p| p.starts_with("nested")), "{name}");
            },
        );
    }

    #[test]
    fn bulk_stage_all_includes_untracked_but_not_embedded_repos() {
        for_each_backend_fresh(
            |root| {
                plain_matrix(root);
                state_f(root);
            },
            |backend, root, name| {
                backend
                    .change_bulk_op(ChangeBulkOp::StageAll, None)
                    .unwrap();
                let after = backend.get_change_set().unwrap();
                assert!(after.unstaged.is_empty(), "{name}: {:?}", after.unstaged);
                assert_eq!(paths(&after.untracked), vec!["nested"], "{name}");
                let staged = staged_names(root);
                assert!(staged.contains(&"top.txt".to_string()));
                assert!(staged.contains(&"newdir/file.txt".to_string()));
                assert!(!staged.iter().any(|p| p.starts_with("nested")), "{name}");
            },
        );
    }

    #[test]
    fn bulk_unstage_all_empties_the_index() {
        for_each_backend_fresh(plain_matrix, |backend, root, name| {
            backend
                .change_bulk_op(ChangeBulkOp::UnstageAll, None)
                .unwrap();
            assert!(staged_names(root).is_empty(), "{name}");
            let after = backend.get_change_set().unwrap();
            assert!(after.staged.is_empty(), "{name}");
            assert!(
                paths(&after.untracked).contains(&"added.txt".to_string()),
                "{name}"
            );
        });
    }

    #[test]
    fn bulk_op_inside_a_submodule_runs_in_the_inner_repo() {
        for_each_backend_fresh(
            |root| {
                state_c(root);
                write(root, "top.txt", "outer edit\n");
            },
            |backend, root, name| {
                backend
                    .change_bulk_op(ChangeBulkOp::StageTracked, Some(Path::new("lib")))
                    .unwrap();
                assert_eq!(staged_names(&root.join("lib")), vec!["a.txt"], "{name}");
                assert!(
                    staged_names(root).is_empty(),
                    "{name}: outer index untouched"
                );
            },
        );
    }

    // --- fingerprint and console -------------------------------------------

    #[test]
    fn change_set_fingerprint_changes_when_a_file_is_staged() {
        let (_temp, root) = super_with_submodule();
        write(&root, "top.txt", "top\nedit\n");
        write(&root, "new.txt", "new\n");
        state_a(&root);
        let before: Vec<u64> = PREFERENCES
            .iter()
            .map(|(p, _)| backend(&root, *p).get_change_set().unwrap().fingerprint())
            .collect();
        assert_eq!(
            before[0], before[1],
            "both backends agree on the same state"
        );
        git(&root, &["add", "top.txt"]);
        let after: Vec<u64> = PREFERENCES
            .iter()
            .map(|(p, _)| backend(&root, *p).get_change_set().unwrap().fingerprint())
            .collect();
        assert_eq!(after[0], after[1]);
        assert_ne!(before[0], after[0]);
    }

    #[test]
    fn submodule_commands_are_logged_with_their_minus_c_dir() {
        for_each_backend_fresh(
            |root| {
                state_c(root);
            },
            |backend, _root, name| {
                let set = backend.get_change_set().unwrap();
                let entry = find(&set, ChangeGroup::Unstaged, "lib").clone();
                crate::git_console::clear();
                backend
                    .discard_change_entry(&key(ChangeGroup::Unstaged, "lib"), &entry)
                    .unwrap();
                let entries = crate::git_console::snapshot();
                let checkout = entries
                    .iter()
                    .find(|e| e.command == "git -C lib checkout -q -- .")
                    .unwrap_or_else(|| {
                        panic!(
                            "{name}: {:?}",
                            entries.iter().map(|e| &e.command).collect::<Vec<_>>()
                        )
                    });
                assert!(checkout.ok && checkout.mutating);
                assert!(
                    entries.iter().any(|e| e.command == "git -C lib clean -fdq"),
                    "{name}"
                );
            },
        );
    }
}
