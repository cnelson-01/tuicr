//! The working-tree change set behind the Changes view (an IDE's Commit /
//! Local Changes tool window): one status pass grouped into staged, unstaged,
//! unversioned and conflicted entries, with submodules as first-class
//! entries. Backends build it from status, never from diffs: the Git CLI and
//! libgit2 disagree about untracked-only submodule content when read through
//! `diff`, and status carries the `160000` gitlink modes a diff drops.

use std::path::{Path, PathBuf};

use crate::hash::Fnv1aHasher;
use crate::model::FileStatus;
use crate::vcs::traits::HunkScope;

/// A section of the Changes tree. Display order is [`ChangeGroup::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChangeGroup {
    /// HEAD → index.
    Staged,
    /// index → working tree, tracked paths (and submodules).
    Unstaged,
    /// Untracked, not ignored.
    Untracked,
    /// Unmerged index entries.
    Conflicted,
}

impl ChangeGroup {
    /// Tree order: conflicts first so they are never scrolled out of sight.
    pub const ALL: [ChangeGroup; 4] = [
        ChangeGroup::Conflicted,
        ChangeGroup::Staged,
        ChangeGroup::Unstaged,
        ChangeGroup::Untracked,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ChangeGroup::Staged => "Staged",
            ChangeGroup::Unstaged => "Unstaged",
            ChangeGroup::Untracked => "Unversioned files",
            ChangeGroup::Conflicted => "Merge conflicts",
        }
    }

    /// Short form for the header chunk and messages.
    pub fn short_label(self) -> &'static str {
        match self {
            ChangeGroup::Staged => "staged",
            ChangeGroup::Unstaged => "unstaged",
            ChangeGroup::Untracked => "unversioned",
            ChangeGroup::Conflicted => "conflict",
        }
    }

    /// Which git diff an entry of this group was displayed from, for hunk
    /// and line operations on the previewed diff.
    pub fn hunk_scope(self) -> HunkScope {
        match self {
            ChangeGroup::Staged => HunkScope::Staged,
            ChangeGroup::Unstaged | ChangeGroup::Untracked | ChangeGroup::Conflicted => {
                HunkScope::Unstaged
            }
        }
    }

    /// What the group's own checkbox (Space on the group row) does.
    pub fn bulk_op(self) -> Option<ChangeBulkOp> {
        match self {
            ChangeGroup::Staged => Some(ChangeBulkOp::UnstageAll),
            ChangeGroup::Unstaged => Some(ChangeBulkOp::StageTracked),
            ChangeGroup::Untracked => Some(ChangeBulkOp::StageUntracked),
            ChangeGroup::Conflicted => None,
        }
    }
}

/// What happened to an entry, as the tree glyph shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    /// File ↔ symlink ↔ gitlink (`T` in porcelain).
    TypeChanged,
    Untracked,
    Conflicted,
}

impl ChangeStatus {
    pub fn glyph(self) -> char {
        match self {
            ChangeStatus::Added => 'A',
            ChangeStatus::Modified => 'M',
            ChangeStatus::Deleted => 'D',
            ChangeStatus::Renamed => 'R',
            ChangeStatus::Copied => 'C',
            ChangeStatus::TypeChanged => 'T',
            ChangeStatus::Untracked => '?',
            ChangeStatus::Conflicted => '!',
        }
    }

    /// The `DiffFile` status a synthesized or fetched diff for this entry
    /// carries (the diff model has no untracked / conflicted / typechange).
    pub fn as_file_status(self) -> FileStatus {
        match self {
            ChangeStatus::Added | ChangeStatus::Untracked => FileStatus::Added,
            ChangeStatus::Modified | ChangeStatus::TypeChanged | ChangeStatus::Conflicted => {
                FileStatus::Modified
            }
            ChangeStatus::Deleted => FileStatus::Deleted,
            ChangeStatus::Renamed => FileStatus::Renamed,
            ChangeStatus::Copied => FileStatus::Copied,
        }
    }

    /// One letter of a porcelain v2 `XY` field. Lenient: a letter this does
    /// not know still yields an entry (as `Modified`) rather than dropping it.
    pub fn from_porcelain(letter: u8) -> Self {
        match letter {
            b'A' => ChangeStatus::Added,
            b'D' => ChangeStatus::Deleted,
            b'R' => ChangeStatus::Renamed,
            b'C' => ChangeStatus::Copied,
            b'T' => ChangeStatus::TypeChanged,
            _ => ChangeStatus::Modified,
        }
    }
}

/// Submodule state relative to the side the entry sits on: for a Staged
/// entry `old` is `head_id` and `new` is `index_id`; for an Unstaged entry
/// `old` is `index_id` and `new` is `workdir_id`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SubmoduleChange {
    /// Gitlink recorded in HEAD (`git rev-parse HEAD:<sub>`), full hex.
    pub head_id: Option<String>,
    /// Gitlink recorded in the index (`git rev-parse :<sub>`).
    pub index_id: Option<String>,
    /// The commit the submodule is checked out at (`git -C <sub> rev-parse
    /// HEAD`); `None` when the directory is missing or not a checkout.
    pub workdir_id: Option<String>,
    /// Commits in `new` that are not in `old`.
    pub ahead: usize,
    /// Commits in `old` that are not in `new` (a rewind).
    pub behind: usize,
    /// Tracked files modified inside the submodule (its own index or worktree).
    pub modified_content: bool,
    /// Untracked files inside the submodule.
    pub untracked_content: bool,
    /// Listed in `.gitmodules`. `false` for a gitlink git only knows from the
    /// index / HEAD (an embedded repo that was `git add`ed).
    pub registered: bool,
    /// Why the detailed probe (status flags, ids, counts) failed, if it did.
    /// The entry is still listed from the plain status; the pane shows this
    /// as a dim note under it.
    pub probe_error: Option<String>,
}

impl SubmoduleChange {
    /// The two sides the entry compares.
    pub fn sides(&self, group: ChangeGroup) -> (Option<&str>, Option<&str>) {
        match group {
            ChangeGroup::Staged => (self.head_id.as_deref(), self.index_id.as_deref()),
            _ => (self.index_id.as_deref(), self.workdir_id.as_deref()),
        }
    }

    /// Both sides present and different: the only state `git add <sub>` /
    /// `checkout --detach` act on. Added and deleted gitlinks have their own
    /// commands (see `ChangeEntry::stageable` and the discard matrix).
    pub fn pointer_moved(&self, group: ChangeGroup) -> bool {
        match self.sides(group) {
            (Some(old), Some(new)) => old != new,
            _ => false,
        }
    }

    pub fn dirty(&self) -> bool {
        self.modified_content || self.untracked_content
    }

    /// Tree detail: `+3 commits`, `rewound 2 commits`, `modified content`,
    /// `untracked content`, joined with `, `; `(not in .gitmodules)` for an
    /// unregistered gitlink.
    pub fn detail(&self) -> String {
        let mut parts = Vec::new();
        if self.ahead > 0 {
            parts.push(format!(
                "+{} commit{}",
                self.ahead,
                if self.ahead == 1 { "" } else { "s" }
            ));
        }
        if self.behind > 0 {
            parts.push(format!(
                "rewound {} commit{}",
                self.behind,
                if self.behind == 1 { "" } else { "s" }
            ));
        }
        if self.modified_content {
            parts.push("modified content".to_string());
        }
        if self.untracked_content {
            parts.push("untracked content".to_string());
        }
        if !self.registered {
            parts.push("(not in .gitmodules)".to_string());
        }
        parts.join(", ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChangeEntryKind {
    File,
    Submodule(SubmoduleChange),
    /// An untracked directory that is itself a git repository and is not in
    /// `.gitmodules` (`?? nested/`). Not stageable from the view.
    EmbeddedRepo,
}

/// One row-worthy change. `path` is relative to the repository the entry
/// belongs to (the superproject, or the submodule named by
/// [`ChangeDiffKey::submodule`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChangeEntry {
    pub path: PathBuf,
    /// Source of a rename / copy.
    pub old_path: Option<PathBuf>,
    pub status: ChangeStatus,
    pub kind: ChangeEntryKind,
}

impl ChangeEntry {
    pub fn file(path: impl Into<PathBuf>, status: ChangeStatus) -> Self {
        Self {
            path: path.into(),
            old_path: None,
            status,
            kind: ChangeEntryKind::File,
        }
    }

    pub fn submodule(&self) -> Option<&SubmoduleChange> {
        match &self.kind {
            ChangeEntryKind::Submodule(change) => Some(change),
            _ => None,
        }
    }

    pub fn is_submodule(&self) -> bool {
        self.submodule().is_some()
    }

    pub fn is_embedded_repo(&self) -> bool {
        matches!(self.kind, ChangeEntryKind::EmbeddedRepo)
    }

    /// A submodule with content changes can be opened to its own change set.
    pub fn expandable(&self) -> bool {
        self.submodule().is_some_and(SubmoduleChange::dirty)
    }

    /// Whether `git add` / `git reset` in the owning repository does
    /// something useful: files always; submodules when their pointer moved
    /// or the gitlink itself was added or deleted; never embedded repos or
    /// dirty-only submodules.
    pub fn stageable(&self, group: ChangeGroup) -> bool {
        match &self.kind {
            ChangeEntryKind::File => true,
            ChangeEntryKind::Submodule(change) => {
                change.pointer_moved(group)
                    || matches!(self.status, ChangeStatus::Added | ChangeStatus::Deleted)
            }
            ChangeEntryKind::EmbeddedRepo => false,
        }
    }

    /// Entry name for the tree row: the file name, or the full path for a
    /// submodule (its directory *is* the entry).
    pub fn display_name(&self) -> String {
        match self.kind {
            ChangeEntryKind::File => self
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.to_string_lossy().into_owned()),
            ChangeEntryKind::Submodule(_) | ChangeEntryKind::EmbeddedRepo => {
                self.path.to_string_lossy().into_owned()
            }
        }
    }

    /// Whether `X` on the group row leaves this entry alone (counted as
    /// skipped in the result): embedded repositories and conflicted entries
    /// have no discard; a dirty-only submodule's files would be thrown away
    /// under a prompt that only mentions the index and working tree; a
    /// deleted gitlink that is not in `.gitmodules` is refused per entry.
    pub fn skipped_by_group_discard(&self, group: ChangeGroup) -> bool {
        if self.is_embedded_repo() || self.status == ChangeStatus::Conflicted {
            return true;
        }
        let Some(sm) = self.submodule() else {
            return false;
        };
        match self.status {
            ChangeStatus::Deleted => !sm.registered,
            ChangeStatus::Added => false,
            _ => sm.dirty() && !sm.pointer_moved(group),
        }
    }

    /// Every path git must be told about for this entry: the path, plus the
    /// rename / copy source when there is one.
    pub fn pathspecs(&self) -> Vec<&Path> {
        let mut paths = vec![self.path.as_path()];
        if let Some(old) = &self.old_path {
            paths.push(old.as_path());
        }
        paths
    }
}

/// Every local change of one repository, grouped. A path modified in both
/// the index and the working tree is in `staged` *and* `unstaged`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub staged: Vec<ChangeEntry>,
    pub unstaged: Vec<ChangeEntry>,
    pub untracked: Vec<ChangeEntry>,
    pub conflicted: Vec<ChangeEntry>,
    /// Submodules in `.gitmodules` whose directory is empty (not checked
    /// out). Listed dimly; never a change.
    pub uninitialized_submodules: Vec<PathBuf>,
}

impl ChangeSet {
    pub fn group(&self, group: ChangeGroup) -> &[ChangeEntry] {
        match group {
            ChangeGroup::Staged => &self.staged,
            ChangeGroup::Unstaged => &self.unstaged,
            ChangeGroup::Untracked => &self.untracked,
            ChangeGroup::Conflicted => &self.conflicted,
        }
    }

    pub fn group_mut(&mut self, group: ChangeGroup) -> &mut Vec<ChangeEntry> {
        match group {
            ChangeGroup::Staged => &mut self.staged,
            ChangeGroup::Unstaged => &mut self.unstaged,
            ChangeGroup::Untracked => &mut self.untracked,
            ChangeGroup::Conflicted => &mut self.conflicted,
        }
    }

    pub fn push(&mut self, group: ChangeGroup, entry: ChangeEntry) {
        self.group_mut(group).push(entry);
    }

    /// No entries in any group (uninitialized submodules do not count).
    pub fn is_empty(&self) -> bool {
        ChangeGroup::ALL.iter().all(|g| self.group(*g).is_empty())
    }

    pub fn len(&self) -> usize {
        ChangeGroup::ALL.iter().map(|g| self.group(*g).len()).sum()
    }

    pub fn find(&self, group: ChangeGroup, path: &Path) -> Option<&ChangeEntry> {
        self.group(group).iter().find(|e| e.path == path)
    }

    /// The group an entry with `path` lives in, preferring `prefer`, for
    /// re-selecting after a stage / unstage moved it.
    pub fn group_of(&self, path: &Path, prefer: ChangeGroup) -> Option<ChangeGroup> {
        if self.find(prefer, path).is_some() {
            return Some(prefer);
        }
        ChangeGroup::ALL
            .into_iter()
            .find(|g| self.find(*g, path).is_some())
    }

    /// Sort every group by path so the tree and the fingerprint are stable.
    pub fn sort(&mut self) {
        for group in ChangeGroup::ALL {
            self.group_mut(group).sort_by(|a, b| a.path.cmp(&b.path));
        }
        self.uninitialized_submodules.sort();
    }

    /// Drop entries whose path fails `keep` (`.tuicrignore`, `--file` path filter).
    pub fn retain_paths(&mut self, mut keep: impl FnMut(&Path) -> bool) {
        for group in ChangeGroup::ALL {
            self.group_mut(group).retain(|e| keep(&e.path));
        }
    }

    /// Every entry path, for batch filters such as `tuicrignore::filter_paths`.
    pub fn paths(&self) -> Vec<PathBuf> {
        ChangeGroup::ALL
            .iter()
            .flat_map(|g| self.group(*g).iter().map(|e| e.path.clone()))
            .collect()
    }

    /// Order-sensitive content fingerprint (call [`ChangeSet::sort`] first),
    /// used by the diff watch to notice a stage / unstage without a byte
    /// change in any diff.
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = Fnv1aHasher::new();
        for group in ChangeGroup::ALL {
            hasher.write(&[group as u8]);
            for entry in self.group(group) {
                hasher.write(entry.path.to_string_lossy().as_bytes());
                hasher.write(&[0, entry.status.glyph() as u8]);
                match &entry.kind {
                    ChangeEntryKind::File => hasher.write(&[1]),
                    ChangeEntryKind::EmbeddedRepo => hasher.write(&[2]),
                    ChangeEntryKind::Submodule(sm) => {
                        hasher.write(&[3]);
                        for id in [&sm.head_id, &sm.index_id, &sm.workdir_id] {
                            hasher.write(id.as_deref().unwrap_or("").as_bytes());
                            hasher.write(&[0]);
                        }
                        hasher.write(&[
                            sm.modified_content as u8,
                            sm.untracked_content as u8,
                            sm.registered as u8,
                            sm.probe_error.is_some() as u8,
                        ]);
                    }
                }
                hasher.write(&[0]);
            }
        }
        hasher.write(&[0xff]);
        for path in &self.uninitialized_submodules {
            hasher.write(path.to_string_lossy().as_bytes());
            hasher.write(&[0]);
        }
        hasher.finish()
    }
}

/// Identity of one entry's diff: which side, which path, and which
/// repository (`None` = the superproject; `Some(sub)` = the submodule at
/// that superproject-relative path, `path` being relative to it). The
/// Changes view caches diffs and addresses operations by this key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChangeDiffKey {
    pub group: ChangeGroup,
    pub path: PathBuf,
    pub submodule: Option<PathBuf>,
}

impl ChangeDiffKey {
    pub fn new(group: ChangeGroup, path: impl Into<PathBuf>) -> Self {
        Self {
            group,
            path: path.into(),
            submodule: None,
        }
    }

    pub fn in_submodule(
        group: ChangeGroup,
        submodule: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            group,
            path: path.into(),
            submodule: Some(submodule.into()),
        }
    }

    /// The same key as seen from inside its own repository.
    pub fn local(&self) -> Self {
        Self {
            group: self.group,
            path: self.path.clone(),
            submodule: None,
        }
    }

    /// Superproject-relative path, for the diff pane title and `$EDITOR`.
    pub fn display_path(&self) -> PathBuf {
        match &self.submodule {
            Some(sub) => sub.join(&self.path),
            None => self.path.clone(),
        }
    }
}

/// Whole-group operations the pane offers with one key (or the group's
/// checkbox). They run in the superproject, or in the submodule a nested
/// group belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeBulkOp {
    /// `git add -u`: every tracked change, submodule pointers included.
    StageTracked,
    /// Stage every untracked file (embedded repos excluded).
    StageUntracked,
    /// `git add -A` (embedded repos excluded).
    StageAll,
    /// `git reset -q`: empty the index back to HEAD.
    UnstageAll,
}

impl ChangeBulkOp {
    pub fn done_message(self) -> &'static str {
        match self {
            ChangeBulkOp::StageTracked => "Staged every tracked change",
            ChangeBulkOp::StageUntracked => "Staged every untracked file",
            ChangeBulkOp::StageAll => "Staged everything",
            ChangeBulkOp::UnstageAll => "Unstaged everything",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_changes_when_an_entry_moves_groups() {
        let mut a = ChangeSet::default();
        a.push(
            ChangeGroup::Unstaged,
            ChangeEntry::file("f", ChangeStatus::Modified),
        );
        let mut b = ChangeSet::default();
        b.push(
            ChangeGroup::Staged,
            ChangeEntry::file("f", ChangeStatus::Modified),
        );
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint(), a.clone().fingerprint());
    }

    #[test]
    fn submodule_detail_reads_like_the_ide() {
        let sm = SubmoduleChange {
            ahead: 3,
            modified_content: true,
            registered: true,
            ..Default::default()
        };
        assert_eq!(sm.detail(), "+3 commits, modified content");
        let rewind = SubmoduleChange {
            behind: 1,
            registered: true,
            ..Default::default()
        };
        assert_eq!(rewind.detail(), "rewound 1 commit");
    }

    #[test]
    fn pointer_moved_needs_both_sides() {
        let added = SubmoduleChange {
            index_id: Some("a".into()),
            ..Default::default()
        };
        assert!(!added.pointer_moved(ChangeGroup::Staged));
        let moved = SubmoduleChange {
            head_id: Some("a".into()),
            index_id: Some("b".into()),
            ..Default::default()
        };
        assert!(moved.pointer_moved(ChangeGroup::Staged));
        let mut entry = ChangeEntry::file("lib", ChangeStatus::Added);
        entry.kind = ChangeEntryKind::Submodule(added);
        assert!(entry.stageable(ChangeGroup::Staged));
    }

    #[test]
    fn group_of_prefers_the_asked_group_then_any() {
        let mut set = ChangeSet::default();
        set.push(
            ChangeGroup::Staged,
            ChangeEntry::file("f", ChangeStatus::Modified),
        );
        set.push(
            ChangeGroup::Unstaged,
            ChangeEntry::file("f", ChangeStatus::Modified),
        );
        assert_eq!(
            set.group_of(Path::new("f"), ChangeGroup::Unstaged),
            Some(ChangeGroup::Unstaged)
        );
        set.unstaged.clear();
        assert_eq!(
            set.group_of(Path::new("f"), ChangeGroup::Unstaged),
            Some(ChangeGroup::Staged)
        );
        assert_eq!(set.group_of(Path::new("g"), ChangeGroup::Unstaged), None);
    }

    #[test]
    fn group_discard_skips_embedded_conflicted_dirty_only_and_unregistered_deleted() {
        let dirty = SubmoduleChange {
            head_id: Some("a".into()),
            index_id: Some("a".into()),
            workdir_id: Some("a".into()),
            modified_content: true,
            registered: true,
            ..Default::default()
        };
        let mut entry = ChangeEntry::file("lib", ChangeStatus::Modified);
        entry.kind = ChangeEntryKind::Submodule(dirty.clone());
        assert!(entry.skipped_by_group_discard(ChangeGroup::Unstaged));
        let moved = SubmoduleChange {
            workdir_id: Some("b".into()),
            ..dirty
        };
        entry.kind = ChangeEntryKind::Submodule(moved);
        assert!(
            !entry.skipped_by_group_discard(ChangeGroup::Unstaged),
            "a moved pointer is discarded, dirt and all (the prompt says so)"
        );
        let mut gone = ChangeEntry::file("other", ChangeStatus::Deleted);
        gone.kind = ChangeEntryKind::Submodule(SubmoduleChange {
            head_id: Some("a".into()),
            ..Default::default()
        });
        assert!(gone.skipped_by_group_discard(ChangeGroup::Staged));
        let mut embedded = ChangeEntry::file("nested", ChangeStatus::Untracked);
        embedded.kind = ChangeEntryKind::EmbeddedRepo;
        assert!(embedded.skipped_by_group_discard(ChangeGroup::Untracked));
        assert!(
            ChangeEntry::file("f", ChangeStatus::Conflicted)
                .skipped_by_group_discard(ChangeGroup::Conflicted)
        );
        assert!(
            !ChangeEntry::file("f", ChangeStatus::Modified)
                .skipped_by_group_discard(ChangeGroup::Unstaged)
        );
    }
}
