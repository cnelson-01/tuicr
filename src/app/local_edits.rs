//! Lines changed in `$EDITOR` during this session, and `:local`.
//!
//! Just before the editor first opens a working-tree file, its content is kept
//! as that file's baseline. Each time the diff is applied the file on disk is
//! compared with the baseline, and the diff view tints the lines the user
//! changed — an IDE's local-changes gutter, measured from the moment tuicr
//! handed the file to the editor rather than from HEAD.
//!
//! `:local` is the matching "compare with local": a commit-range review
//! becomes the same commits plus the working tree, so edits made while
//! reviewing a commit show up (and get the tint) without leaving the review.

use std::collections::BTreeSet;
use std::io;

use super::editor_target::short_sha;
use super::*;

/// How a working-tree line relates to its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalEditMark {
    /// Added or changed since the baseline.
    Edited,
    /// Unchanged, but baseline lines were deleted just above it.
    RemovedAbove,
}

/// One file's baseline and how the working tree differs from it now.
#[derive(Debug, Clone)]
pub struct LocalEditedFile {
    /// Absolute path read on refresh. Kept apart from the repo-relative key
    /// because PR review swaps `vcs_info.root_path` for a synthetic identity.
    path: PathBuf,
    baseline: Vec<u8>,
    /// 1-based working-tree lines added or modified relative to the baseline.
    pub changed: BTreeSet<u32>,
    /// 1-based working-tree line now following each place where baseline
    /// lines were deleted; `line_count + 1` for a deletion at the end.
    pub removed: BTreeSet<u32>,
    /// Whether the file has differed from the baseline yet. A windowed
    /// editor's baseline can be refreshed before the user saves, so only a
    /// file that was edited and then put back is dropped.
    seen_edit: bool,
}

impl LocalEditedFile {
    /// How working-tree line `new_lineno` differs from the baseline.
    pub fn mark(&self, new_lineno: Option<u32>) -> Option<LocalEditMark> {
        let line = new_lineno?;
        if self.changed.contains(&line) {
            Some(LocalEditMark::Edited)
        } else if self.removed.contains(&line) {
            Some(LocalEditMark::RemovedAbove)
        } else {
            None
        }
    }

    /// Status-bar wording, e.g. `4 lines edited, 1 deletion`; `None` when the
    /// file matches its baseline.
    fn summary(&self) -> Option<String> {
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let edited = self.changed.len();
        let removed = self.removed.len();
        match (edited, removed) {
            (0, 0) => None,
            (0, r) => Some(format!("{r} deletion{}", plural(r))),
            (e, 0) => Some(format!("{e} line{} edited", plural(e))),
            (e, r) => Some(format!(
                "{e} line{} edited, {r} deletion{}",
                plural(e),
                plural(r)
            )),
        }
    }
}

/// Baselines by repo-relative path.
#[derive(Debug, Default)]
pub struct LocalEdits {
    files: HashMap<PathBuf, LocalEditedFile>,
}

impl LocalEdits {
    pub fn get(&self, rel: &Path) -> Option<&LocalEditedFile> {
        self.files.get(rel)
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Keep `baseline` for `rel` unless it already has one: highlights stay
    /// relative to the content before the first edit, however many follow.
    fn snapshot(&mut self, rel: PathBuf, path: PathBuf, baseline: Vec<u8>) {
        self.files.entry(rel).or_insert_with(|| LocalEditedFile {
            path,
            baseline,
            changed: BTreeSet::new(),
            removed: BTreeSet::new(),
            seen_edit: false,
        });
    }

    /// Re-read every tracked file and recompute its line sets. A file that
    /// cannot be read keeps its previous sets; a deleted one counts as empty.
    fn refresh(&mut self) {
        self.files.retain(|_, file| {
            let current = match std::fs::read(&file.path) {
                Ok(content) => content,
                Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(_) => return true,
            };
            if current == file.baseline {
                file.changed.clear();
                file.removed.clear();
                return !file.seen_edit;
            }
            (file.changed, file.removed) = line_changes(&file.baseline, &current);
            file.seen_edit = true;
            true
        });
    }
}

/// The working-tree lines that differ from `baseline`: `(changed, removed)`
/// as described on [`LocalEditedFile`].
fn line_changes(baseline: &[u8], current: &[u8]) -> (BTreeSet<u32>, BTreeSet<u32>) {
    let mut changed = BTreeSet::new();
    let mut removed = BTreeSet::new();
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(0);
    let Ok(patch) = git2::Patch::from_buffers(baseline, None, current, None, Some(&mut opts))
    else {
        return (changed, removed);
    };
    for idx in 0..patch.num_hunks() {
        let Ok((hunk, _)) = patch.hunk(idx) else {
            continue;
        };
        let (start, count) = (hunk.new_start(), hunk.new_lines());
        if count == 0 {
            // A pure deletion's `new_start` is the line before the gap (0
            // when the deleted lines opened the file).
            removed.insert(start + 1);
            continue;
        }
        changed.extend(start..start + count);
        // A replacement that also dropped lines: the line after it is where
        // the rest went missing.
        if hunk.old_lines() > count {
            removed.insert(start + count);
        }
    }
    (changed, removed)
}

impl App {
    /// Called right before `$EDITOR` opens `target`: keep the file's content
    /// as its baseline the first time it is edited this session.
    ///
    /// Only working-tree files of this repository count. PR snapshots live in
    /// the temp dir, a submodule review's lines belong to another repository,
    /// and the merge view re-parses the file itself.
    pub fn snapshot_local_edit_baseline(&mut self, target: &EditorTarget) {
        if self.input_mode == InputMode::MergeConflict
            || matches!(self.diff_source, DiffSource::Submodule(_))
        {
            return;
        }
        let Ok(rel) = target.path.strip_prefix(&self.vcs_info.root_path) else {
            return;
        };
        let baseline = match std::fs::read(&target.path) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return,
        };
        self.local_edits
            .snapshot(rel.to_path_buf(), target.path.clone(), baseline);
    }

    /// Recompute the edited lines from disk. Runs wherever a diff is applied
    /// (`apply_diff_files`) and after an editor returns in a review that does
    /// not reload.
    pub fn refresh_local_edits(&mut self) {
        self.local_edits.refresh();
    }

    /// `:clearedits`: forget every baseline, so the highlights go away.
    pub fn clear_local_edits(&mut self) {
        let count = self.local_edits.files.len();
        self.local_edits.files.clear();
        match count {
            0 => self.set_message("No edits to clear"),
            1 => self.set_message("Cleared edit highlights for 1 file"),
            n => self.set_message(format!("Cleared edit highlights for {n} files")),
        }
    }

    /// Edit highlight for line `new_lineno` of diff file `file_idx`, when the
    /// review's new side is the working tree.
    pub fn local_edit_mark(
        &self,
        file_idx: usize,
        new_lineno: Option<u32>,
    ) -> Option<LocalEditMark> {
        if self.local_edits.is_empty() || !self.new_side_is_working_tree() {
            return None;
        }
        let file = self.diff_files.get(file_idx)?;
        if file.is_commit_message {
            return None;
        }
        self.local_edits
            .get(file.new_path.as_deref()?)?
            .mark(new_lineno)
    }

    /// Status-bar wording for what changed in `target` since its baseline.
    pub fn local_edit_summary(&self, target: &EditorTarget) -> Option<String> {
        let rel = target.path.strip_prefix(&self.vcs_info.root_path).ok()?;
        self.local_edits.get(rel)?.summary()
    }

    /// Whether the diff on screen ends at this repository's working tree.
    /// Narrowing the inline selector of a commits-plus-working-tree review to
    /// commits only puts a commit on the new side again.
    fn new_side_is_working_tree(&self) -> bool {
        match &self.diff_source {
            DiffSource::Submodule(_) => false,
            DiffSource::StagedUnstagedAndCommits(_)
                if Self::is_strict_commit_selection(
                    self.commit_selection_range,
                    self.review_commits.len(),
                ) =>
            {
                let (start, end) = self
                    .commit_selection_range
                    .expect("is_strict_commit_selection guarantees Some(range)");
                Self::source_for_commit_subrange(&self.review_commits, start, end)
                    .includes_worktree_changes()
            }
            source => source.includes_worktree_changes(),
        }
    }

    /// Load `ids` as a commit range or, `to_local`, as those commits plus the
    /// working tree, with the Staged / Unstaged rows the commit selector
    /// would show beside them.
    fn load_local_compare(&mut self, ids: Vec<String>, to_local: bool) -> Result<()> {
        let mut commits = self.vcs.get_commits_info(&ids)?;
        if !to_local {
            return self.load_commit_range_selection(ids, commits);
        }
        let status = Self::get_change_status_with_ignore(
            self.vcs.as_ref(),
            &self.vcs_info.root_path,
            self.theme.syntax_highlighter(),
            self.path_filter.as_deref(),
        )?;
        // Oldest first: the loader reverses into the selector's newest-first
        // order, which puts Unstaged on top as the commit selector does.
        if status.staged {
            commits.push(Self::staged_commit_entry());
        }
        if status.unstaged {
            commits.push(Self::unstaged_commit_entry());
        }
        self.load_staged_unstaged_and_commits_selection(ids, commits)
    }

    /// `:local`: compare a commit range with the working tree, or go back.
    ///
    /// Both directions go through the loaders the commit selector uses, so
    /// the session, inline selector and file tree are set up exactly as if
    /// the user had picked the same rows there.
    pub fn toggle_compare_with_local(&mut self) {
        let (ids, to_local) = match &self.diff_source {
            DiffSource::CommitRange(ids) => (ids.clone(), true),
            DiffSource::StagedUnstagedAndCommits(ids) => (ids.clone(), false),
            DiffSource::WorkingTree | DiffSource::Unstaged | DiffSource::StagedAndUnstaged => {
                self.set_message("Already comparing with the working tree");
                return;
            }
            DiffSource::Staged => {
                self.set_warning(":local needs a commit-range review");
                return;
            }
            DiffSource::PullRequest(_) => {
                self.set_error(":local is not available in PR review");
                return;
            }
            DiffSource::Submodule(_) => {
                self.set_error(":local is not available in a submodule review");
                return;
            }
        };
        if ids.is_empty() {
            self.set_warning(":local needs a commit-range review");
            return;
        }

        let current_path = self.current_file_path().cloned();
        if let Err(err) = self.load_local_compare(ids.clone(), to_local) {
            self.set_error(format!("Compare with local failed: {err}"));
            return;
        }
        let switched = if to_local {
            matches!(self.diff_source, DiffSource::StagedUnstagedAndCommits(_))
        } else {
            matches!(self.diff_source, DiffSource::CommitRange(_))
        };
        if !switched {
            // The loader explained why (e.g. no changes) and left the review.
            return;
        }

        self.refresh_local_edits();
        if let Some(idx) = current_path.and_then(|path| {
            self.diff_files
                .iter()
                .position(|f| f.display_path() == &path)
        }) {
            self.jump_to_file(idx);
        }
        let range = match ids.as_slice() {
            [only] => short_sha(only).to_string(),
            [first, .., last] => format!("{}..{}", short_sha(first), short_sha(last)),
            [] => String::new(),
        };
        if to_local {
            self.set_message(format!("Comparing {range} with local (working tree)"));
        } else {
            self.set_message(format!("Back to reviewing {range}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(lines: &[u32]) -> BTreeSet<u32> {
        lines.iter().copied().collect()
    }

    #[test]
    fn inserted_lines_are_changed() {
        let (changed, removed) = line_changes(b"a\nb\nc\n", b"a\nx\ny\nb\nc\n");
        assert_eq!(changed, set(&[2, 3]));
        assert!(removed.is_empty());
    }

    #[test]
    fn modified_line_is_changed() {
        let (changed, removed) = line_changes(b"a\nb\nc\n", b"a\nB\nc\n");
        assert_eq!(changed, set(&[2]));
        assert!(removed.is_empty());
    }

    #[test]
    fn deletion_in_the_middle_marks_the_following_line() {
        let (changed, removed) = line_changes(b"a\nb\nc\nd\n", b"a\nd\n");
        assert!(changed.is_empty());
        assert_eq!(removed, set(&[2]));
    }

    #[test]
    fn replacement_that_drops_lines_marks_the_line_after_it() {
        // `b`, `c` became `B`: line 2 is edited, `d` (now line 3) follows the
        // lost line.
        let (changed, removed) = line_changes(b"a\nb\nc\nd\n", b"a\nB\nd\n");
        assert_eq!(changed, set(&[2]));
        assert_eq!(removed, set(&[3]));
    }

    #[test]
    fn deletion_at_the_start_marks_the_first_line() {
        let (changed, removed) = line_changes(b"a\nb\nc\n", b"c\n");
        assert!(changed.is_empty());
        assert_eq!(removed, set(&[1]));
    }

    #[test]
    fn deletion_at_eof_marks_one_past_the_last_line() {
        let (changed, removed) = line_changes(b"a\nb\nc\n", b"a\nb\n");
        assert!(changed.is_empty());
        assert_eq!(removed, set(&[3]));
    }

    #[test]
    fn new_file_is_all_changed() {
        let (changed, removed) = line_changes(b"", b"x\ny\n");
        assert_eq!(changed, set(&[1, 2]));
        assert!(removed.is_empty());
    }

    #[test]
    fn revert_to_baseline_drops_the_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "a\nb\n").unwrap();
        let mut edits = LocalEdits::default();
        edits.snapshot(PathBuf::from("f.txt"), path.clone(), b"a\nb\n".to_vec());

        // Unchanged so far: the baseline survives a refresh (a windowed
        // editor may not have saved yet).
        edits.refresh();
        assert!(edits.get(Path::new("f.txt")).is_some());

        std::fs::write(&path, "a\nB\n").unwrap();
        edits.refresh();
        let file = edits.get(Path::new("f.txt")).expect("tracked");
        assert_eq!(file.changed, set(&[2]));

        std::fs::write(&path, "a\nb\n").unwrap();
        edits.refresh();
        assert!(edits.get(Path::new("f.txt")).is_none());
    }

    #[test]
    fn first_baseline_wins() {
        let mut edits = LocalEdits::default();
        edits.snapshot(PathBuf::from("f"), PathBuf::from("/f"), b"one".to_vec());
        edits.snapshot(PathBuf::from("f"), PathBuf::from("/f"), b"two".to_vec());
        assert_eq!(edits.get(Path::new("f")).unwrap().baseline, b"one");
    }

    #[test]
    fn summary_counts_edits_and_deletions() {
        let file = |changed: &[u32], removed: &[u32]| LocalEditedFile {
            path: PathBuf::new(),
            baseline: Vec::new(),
            changed: set(changed),
            removed: set(removed),
            seen_edit: true,
        };
        assert_eq!(file(&[], &[]).summary(), None);
        assert_eq!(file(&[1], &[]).summary().as_deref(), Some("1 line edited"));
        assert_eq!(
            file(&[1, 2], &[5]).summary().as_deref(),
            Some("2 lines edited, 1 deletion")
        );
        assert_eq!(file(&[], &[3, 7]).summary().as_deref(), Some("2 deletions"));
    }
}
