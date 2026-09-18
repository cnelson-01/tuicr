//! Blame gutter (`:blame`) and per-file history (`:history`).
//!
//! With blame on, the line-number gutter grows by [`BLAME_WIDTH`] columns
//! and every new-side line is prefixed with `sha author age` for the commit
//! that last touched it. Blame is computed lazily, one file per main-loop
//! tick starting with the file under the cursor, and cached by the diff
//! file's content hash so a reload re-blames only what changed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::{App, DiffSource, FileTreeItem, FocusedPanel};
use crate::model::DiffFile;
use crate::vcs::{BlameLine, CommitQuery};

/// `abcdef1 author.... age ` — short sha, ten author columns, four for
/// the age, and a separating space.
pub const BLAME_WIDTH: usize = 7 + 1 + 10 + 1 + 4 + 1;

/// Per-file blame keyed by the diff file's display path and content hash,
/// so a stale entry from before a reload is simply never looked up again.
pub type BlameKey = (PathBuf, u64);

#[derive(Debug, Default)]
pub struct BlameState {
    pub enabled: bool,
    /// One entry per new-side line, index 0 = line 1; `None` where git
    /// reported nothing (uncommitted lines).
    pub cache: HashMap<BlameKey, Vec<Option<BlameLine>>>,
    /// Files whose blame failed (deleted, binary, outside the repo); not
    /// retried until the diff reloads.
    pub failed: HashSet<BlameKey>,
}

fn blame_key(file: &DiffFile) -> BlameKey {
    (file.display_path().clone(), file.content_hash)
}

/// The gutter label for one line: `sha author age`, padded to
/// `BLAME_WIDTH - 1` columns.
pub(crate) fn blame_label(line: &BlameLine) -> String {
    let sha: String = line.sha.chars().take(7).collect();
    let author: String = line.author.chars().take(10).collect();
    let age = compact_age(&line.time);
    format!("{sha:<7} {author:<10} {age:>4}")
}

/// Age in at most four columns: `now`, `12m`, `3h`, `9d`, `4mo`, `2y`.
fn compact_age(time: &chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - *time).num_seconds().max(0);
    let minutes = secs / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if minutes < 1 {
        "now".to_string()
    } else if hours < 1 {
        format!("{minutes}m")
    } else if days < 1 {
        format!("{hours}h")
    } else if days < 30 {
        format!("{days}d")
    } else if days < 365 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

/// Write `label` over the leading columns of a right-aligned number
/// `field`, which the widened gutter guarantees are blank. Lines without a
/// label keep their blanks so the numbers stay aligned.
pub fn overlay_label(field: String, label: Option<&str>, width: usize) -> String {
    let Some(label) = label else {
        return field;
    };
    let mut chars: Vec<char> = field.chars().collect();
    if chars.len() < width {
        return field;
    }
    for (i, ch) in label.chars().take(width).enumerate() {
        chars[i] = ch;
    }
    chars.into_iter().collect()
}

impl App {
    /// Extra gutter columns while blame is on.
    pub fn blame_width(&self) -> usize {
        if self.blame.enabled { BLAME_WIDTH } else { 0 }
    }

    pub fn set_blame_enabled(&mut self, enabled: bool) {
        if enabled && self.blame_revision_unsupported() {
            self.set_error("Blame needs a local review (working tree, staged, or commits)");
            return;
        }
        self.blame.enabled = enabled;
        // The gutter changed width, so every wrapped row height did too.
        self.rebuild_annotations();
        let status = if enabled { "on" } else { "off" };
        self.set_message(format!("Blame: {status}"));
    }

    pub fn toggle_blame(&mut self) {
        self.set_blame_enabled(!self.blame.enabled);
    }

    fn blame_revision_unsupported(&self) -> bool {
        matches!(self.diff_source, DiffSource::PullRequest(_))
    }

    /// The revision whose lines the new side shows: the newest commit of a
    /// commit range, otherwise the working tree.
    fn blame_revision(&self) -> Option<String> {
        match &self.diff_source {
            DiffSource::CommitRange(ids) | DiffSource::StagedUnstagedAndCommits(ids) => {
                ids.last().cloned()
            }
            _ => None,
        }
    }

    /// Compute blame for one file that is still missing, the file under the
    /// cursor first. Returns whether anything changed.
    pub fn poll_blame(&mut self) -> bool {
        if !self.blame.enabled {
            return false;
        }
        let current = self.diff_state.current_file_idx;
        let order = (current..self.diff_files.len()).chain(0..current.min(self.diff_files.len()));
        let next = order
            .map(|idx| &self.diff_files[idx])
            .find(|file| {
                !file.is_commit_message
                    && !file.is_binary
                    && file.display_path().as_os_str() != ""
                    && !self.blame.cache.contains_key(&blame_key(file))
                    && !self.blame.failed.contains(&blame_key(file))
            })
            .map(|file| (blame_key(file), file.display_path().clone()));
        let Some((key, path)) = next else {
            return false;
        };
        let revision = self.blame_revision();
        match self.vcs.blame_file(&path, revision.as_deref()) {
            Ok(lines) => {
                self.blame.cache.insert(key, lines);
            }
            Err(_) => {
                self.blame.failed.insert(key);
            }
        }
        true
    }

    /// Overlay the blame label for `new_lineno` of `file` onto a
    /// right-aligned number field. A no-op while blame is off.
    pub fn blame_overlay(&self, file: &DiffFile, new_lineno: Option<u32>, field: String) -> String {
        if !self.blame.enabled {
            return field;
        }
        let label = new_lineno
            .and_then(|n| {
                self.blame
                    .cache
                    .get(&blame_key(file))
                    .and_then(|lines| lines.get(n.saturating_sub(1) as usize))
                    .and_then(Option::as_ref)
            })
            .map(blame_label);
        overlay_label(field, label.as_deref(), BLAME_WIDTH - 1)
    }

    /// The label text alone (for renderers that overlay it themselves).
    pub fn blame_label_for(&self, file_idx: usize, new_lineno: Option<u32>) -> Option<String> {
        if !self.blame.enabled {
            return None;
        }
        let file = self.diff_files.get(file_idx)?;
        let n = new_lineno?;
        self.blame
            .cache
            .get(&blame_key(file))
            .and_then(|lines| lines.get(n.saturating_sub(1) as usize))
            .and_then(Option::as_ref)
            .map(blame_label)
    }

    /// Same as [`blame_overlay`](Self::blame_overlay) by file index, for
    /// renderers that only carry the index.
    pub fn blame_overlay_idx(
        &self,
        file_idx: usize,
        new_lineno: Option<u32>,
        field: String,
    ) -> String {
        match self.diff_files.get(file_idx) {
            Some(file) => self.blame_overlay(file, new_lineno, field),
            None => field,
        }
    }

    /// `:history`: the log pane filtered to commits touching the focused
    /// file, with the pane focused.
    pub fn show_file_history(&mut self) {
        let path = if self.focused_panel == FocusedPanel::FileList {
            match self.get_selected_tree_item() {
                Some(FileTreeItem::File { file_idx, .. }) => self
                    .diff_files
                    .get(file_idx)
                    .map(|f| f.display_path().clone()),
                _ => None,
            }
        } else {
            self.current_file_path().cloned()
        };
        let Some(path) = path else {
            self.set_warning("Move the cursor onto a file to show its history");
            return;
        };
        self.commit_filter = Some(CommitQuery {
            text: Vec::new(),
            author: None,
            path: Some(path.to_string_lossy().into_owned()),
            branch: None,
        });
        if self.log_pane.is_none() {
            self.enable_log_pane();
        } else {
            self.refresh_log_rows_keep_cursor();
            self.focused_panel = FocusedPanel::CommitSelector;
        }
        let label = self.commit_filter_match_label();
        self.set_message(format!(
            "History of {} \u{00b7} {label} \u{00b7} esc clears",
            path.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn overlay_writes_the_label_into_the_blank_lead_and_keeps_the_number() {
        let field = format!("{:>30} ", 42);
        let out = overlay_label(
            field.clone(),
            Some("abcdef1 kim         3mo"),
            BLAME_WIDTH - 1,
        );
        assert!(out.starts_with("abcdef1 kim         3mo"));
        assert!(out.ends_with("42 "));
        assert_eq!(out.chars().count(), field.chars().count());
        // no label: untouched
        assert_eq!(overlay_label(field.clone(), None, BLAME_WIDTH - 1), field);
        // too narrow to hold a label: untouched
        assert_eq!(overlay_label("  1 ".to_string(), Some("x"), 10), "  1 ");
    }

    #[test]
    fn compact_age_fits_four_columns() {
        use chrono::Duration;
        let now = Utc::now();
        for (delta, expected) in [
            (Duration::seconds(5), "now"),
            (Duration::minutes(12), "12m"),
            (Duration::hours(3), "3h"),
            (Duration::days(9), "9d"),
            (Duration::days(120), "4mo"),
            (Duration::days(800), "2y"),
        ] {
            let age = compact_age(&(now - delta));
            assert_eq!(age, expected);
            assert!(age.chars().count() <= 4);
        }
    }

    #[test]
    fn label_truncates_sha_and_author_to_their_columns() {
        let line = BlameLine {
            sha: "0123456789abcdef".to_string(),
            author: "Christopher Nelson".to_string(),
            time: Utc::now(),
        };
        let label = blame_label(&line);
        assert!(label.starts_with("0123456 Christophe "), "{label:?}");
        assert_eq!(label.chars().count(), BLAME_WIDTH - 1);
    }
}
