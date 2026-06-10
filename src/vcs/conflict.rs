//! Conflict-marker parsing and reconstruction for the merge conflict view.
//!
//! Parses a working-tree file containing `<<<<<<<` / `=======` / `>>>>>>>`
//! markers (optionally diff3-style `|||||||` base sections) into alternating
//! common/conflict segments, and rebuilds file content after the user picks
//! a resolution per conflict.

const OURS_MARKER: &str = "<<<<<<<";
const BASE_MARKER: &str = "|||||||";
const SEPARATOR_MARKER: &str = "=======";
const THEIRS_MARKER: &str = ">>>>>>>";

/// One alternating piece of a conflicted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictSegment {
    /// Lines outside any conflict region.
    Common(Vec<String>),
    /// One `<<<<<<<` … `>>>>>>>` region.
    Conflict(ConflictBlock),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConflictBlock {
    pub ours: Vec<String>,
    /// Present only for diff3-style markers.
    pub base: Option<Vec<String>>,
    pub theirs: Vec<String>,
    /// Text after `<<<<<<< ` on the opening marker (e.g. `HEAD`).
    pub ours_label: Option<String>,
    /// Text after `>>>>>>> ` on the closing marker (e.g. a branch name).
    pub theirs_label: Option<String>,
}

impl ConflictBlock {
    /// Lines this block contributes to the resolved file under
    /// `resolution`; `None` while unresolved (markers would be re-emitted).
    pub fn resolved_lines(&self, resolution: ConflictResolution) -> Option<Vec<&str>> {
        match resolution {
            ConflictResolution::Unresolved => None,
            ConflictResolution::Ours => Some(self.ours.iter().map(String::as_str).collect()),
            ConflictResolution::Theirs => Some(self.theirs.iter().map(String::as_str).collect()),
            ConflictResolution::Both => Some(
                self.ours
                    .iter()
                    .chain(self.theirs.iter())
                    .map(String::as_str)
                    .collect(),
            ),
        }
    }

    /// Display height in the merge view: one header row plus enough body
    /// rows for both sides and the chosen result.
    pub fn display_height(&self, resolution: ConflictResolution) -> usize {
        let result_len = self
            .resolved_lines(resolution)
            .map(|lines| lines.len())
            .unwrap_or(0);
        1 + self.ours.len().max(self.theirs.len()).max(result_len)
    }
}

/// User's pick for one conflict block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictResolution {
    #[default]
    Unresolved,
    Ours,
    Theirs,
    /// Keep ours followed by theirs.
    Both,
}

/// A parsed conflicted file.
#[derive(Debug, Clone, Default)]
pub struct ConflictFile {
    pub segments: Vec<ConflictSegment>,
    /// Whether the source content ended with a newline, so reconstruction
    /// is byte-faithful for untouched regions.
    pub trailing_newline: bool,
}

impl ConflictFile {
    pub fn conflict_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| matches!(s, ConflictSegment::Conflict(_)))
            .count()
    }

    pub fn conflict_blocks(&self) -> impl Iterator<Item = &ConflictBlock> {
        self.segments.iter().filter_map(|s| match s {
            ConflictSegment::Conflict(block) => Some(block),
            ConflictSegment::Common(_) => None,
        })
    }

    /// Display geometry shared by the merge view renderer and its scroll
    /// math: common segments render one row per line; each conflict renders
    /// one header row plus enough body rows for both sides and the chosen
    /// result (see [`ConflictBlock::display_height`]). Taking `Both` grows
    /// a block, so heights depend on the current resolutions.
    pub fn display_row_count(&self, resolutions: &[ConflictResolution]) -> usize {
        let mut conflict_idx = 0usize;
        self.segments
            .iter()
            .map(|segment| match segment {
                ConflictSegment::Common(lines) => lines.len(),
                ConflictSegment::Conflict(block) => {
                    let resolution = resolutions.get(conflict_idx).copied().unwrap_or_default();
                    conflict_idx += 1;
                    block.display_height(resolution)
                }
            })
            .sum()
    }

    /// Display row index of each conflict's header row, in order.
    pub fn conflict_header_rows(&self, resolutions: &[ConflictResolution]) -> Vec<usize> {
        let mut rows = Vec::new();
        let mut row = 0usize;
        let mut conflict_idx = 0usize;
        for segment in &self.segments {
            match segment {
                ConflictSegment::Common(lines) => row += lines.len(),
                ConflictSegment::Conflict(block) => {
                    rows.push(row);
                    let resolution = resolutions.get(conflict_idx).copied().unwrap_or_default();
                    conflict_idx += 1;
                    row += block.display_height(resolution);
                }
            }
        }
        rows
    }
}

/// Match a marker line: the seven marker characters followed by end of line
/// or whitespace (labels). Tolerates a trailing `\r` from CRLF files.
fn marker_rest<'a>(line: &'a str, marker: &str) -> Option<Option<&'a str>> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let rest = line.strip_prefix(marker)?;
    if rest.is_empty() {
        return Some(None);
    }
    rest.strip_prefix(' ')
        .map(|label| if label.is_empty() { None } else { Some(label) })
}

enum ParseState {
    Common,
    Ours,
    Base,
    Theirs,
}

pub fn parse_conflict_file(content: &str) -> ConflictFile {
    let trailing_newline = content.ends_with('\n');
    let mut lines: Vec<&str> = content.split('\n').collect();
    if trailing_newline {
        lines.pop();
    }

    let mut segments: Vec<ConflictSegment> = Vec::new();
    let mut common: Vec<String> = Vec::new();
    let mut block = ConflictBlock::default();
    // Raw lines of the in-progress conflict (markers included) so an
    // unterminated region degrades to plain common lines instead of
    // silently dropping content.
    let mut raw_block: Vec<String> = Vec::new();
    let mut state = ParseState::Common;

    for line in lines {
        match state {
            ParseState::Common => {
                if let Some(label) = marker_rest(line, OURS_MARKER) {
                    if !common.is_empty() {
                        segments.push(ConflictSegment::Common(std::mem::take(&mut common)));
                    }
                    block = ConflictBlock {
                        ours_label: label.map(str::to_string),
                        ..ConflictBlock::default()
                    };
                    raw_block = vec![line.to_string()];
                    state = ParseState::Ours;
                } else {
                    common.push(line.to_string());
                }
            }
            ParseState::Ours => {
                raw_block.push(line.to_string());
                if marker_rest(line, BASE_MARKER).is_some() {
                    block.base = Some(Vec::new());
                    state = ParseState::Base;
                } else if marker_rest(line, SEPARATOR_MARKER) == Some(None) {
                    state = ParseState::Theirs;
                } else {
                    block.ours.push(line.to_string());
                }
            }
            ParseState::Base => {
                raw_block.push(line.to_string());
                if marker_rest(line, SEPARATOR_MARKER) == Some(None) {
                    state = ParseState::Theirs;
                } else if let Some(base) = &mut block.base {
                    base.push(line.to_string());
                }
            }
            ParseState::Theirs => {
                raw_block.push(line.to_string());
                if let Some(label) = marker_rest(line, THEIRS_MARKER) {
                    block.theirs_label = label.map(str::to_string);
                    segments.push(ConflictSegment::Conflict(std::mem::take(&mut block)));
                    raw_block.clear();
                    state = ParseState::Common;
                } else {
                    block.theirs.push(line.to_string());
                }
            }
        }
    }

    // Unterminated conflict at EOF: keep the raw lines as common content.
    if !raw_block.is_empty() {
        common.extend(raw_block);
    }
    if !common.is_empty() {
        segments.push(ConflictSegment::Common(common));
    }

    ConflictFile {
        segments,
        trailing_newline,
    }
}

/// Rebuild file content with each conflict replaced by its resolution.
/// Unresolved conflicts re-emit their original markers (diff3 base
/// included) so a partial save stays a valid conflicted file.
pub fn apply_resolutions(file: &ConflictFile, resolutions: &[ConflictResolution]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut conflict_idx = 0usize;

    for segment in &file.segments {
        match segment {
            ConflictSegment::Common(lines) => out.extend(lines.iter().cloned()),
            ConflictSegment::Conflict(block) => {
                let resolution = resolutions
                    .get(conflict_idx)
                    .copied()
                    .unwrap_or(ConflictResolution::Unresolved);
                conflict_idx += 1;
                match block.resolved_lines(resolution) {
                    Some(lines) => out.extend(lines.iter().map(|l| l.to_string())),
                    None => {
                        out.push(marker_line(OURS_MARKER, block.ours_label.as_deref()));
                        out.extend(block.ours.iter().cloned());
                        if let Some(base) = &block.base {
                            out.push(marker_line(BASE_MARKER, None));
                            out.extend(base.iter().cloned());
                        }
                        out.push(SEPARATOR_MARKER.to_string());
                        out.extend(block.theirs.iter().cloned());
                        out.push(marker_line(THEIRS_MARKER, block.theirs_label.as_deref()));
                    }
                }
            }
        }
    }

    let mut content = out.join("\n");
    if file.trailing_newline && !content.is_empty() {
        content.push('\n');
    }
    content
}

/// One-based line, in the [`apply_resolutions`] output, where conflict
/// `idx`'s region begins: its `<<<<<<<` marker while unresolved, or the
/// first line of its resolved content. Used to open `$EDITOR` at the
/// focused conflict. `None` when `idx` is out of range.
pub fn conflict_output_line(
    file: &ConflictFile,
    resolutions: &[ConflictResolution],
    idx: usize,
) -> Option<usize> {
    let mut line = 0usize;
    let mut conflict_idx = 0usize;
    for segment in &file.segments {
        match segment {
            ConflictSegment::Common(lines) => line += lines.len(),
            ConflictSegment::Conflict(block) => {
                let resolution = resolutions.get(conflict_idx).copied().unwrap_or_default();
                if conflict_idx == idx {
                    return Some(line + 1);
                }
                line += match block.resolved_lines(resolution) {
                    Some(lines) => lines.len(),
                    None => {
                        // markers: <<<<<<< ours [||||||| base] ======= theirs >>>>>>>
                        let base = block.base.as_ref().map(|b| b.len() + 1).unwrap_or(0);
                        3 + block.ours.len() + base + block.theirs.len()
                    }
                };
                conflict_idx += 1;
            }
        }
    }
    None
}

fn marker_line(marker: &str, label: Option<&str>) -> String {
    match label {
        Some(label) => format!("{marker} {label}"),
        None => marker.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = "\
line 1
<<<<<<< HEAD
ours line
=======
theirs line
>>>>>>> feature/x
line 2
";

    #[test]
    fn should_parse_single_conflict_with_labels() {
        // when
        let file = parse_conflict_file(SIMPLE);
        // then
        assert_eq!(file.conflict_count(), 1);
        assert_eq!(file.segments.len(), 3);
        let block = file.conflict_blocks().next().unwrap();
        assert_eq!(block.ours, vec!["ours line"]);
        assert_eq!(block.theirs, vec!["theirs line"]);
        assert_eq!(block.ours_label.as_deref(), Some("HEAD"));
        assert_eq!(block.theirs_label.as_deref(), Some("feature/x"));
        assert!(block.base.is_none());
    }

    #[test]
    fn should_parse_diff3_base_section() {
        // given
        let content = "\
<<<<<<< HEAD
ours
||||||| merged common ancestors
base
=======
theirs
>>>>>>> other
";
        // when
        let file = parse_conflict_file(content);
        // then
        let block = file.conflict_blocks().next().unwrap();
        assert_eq!(block.ours, vec!["ours"]);
        assert_eq!(block.base.as_deref(), Some(&["base".to_string()][..]));
        assert_eq!(block.theirs, vec!["theirs"]);
    }

    #[test]
    fn should_treat_file_without_markers_as_single_common_segment() {
        // when
        let file = parse_conflict_file("a\nb\n");
        // then
        assert_eq!(file.conflict_count(), 0);
        assert_eq!(
            file.segments,
            vec![ConflictSegment::Common(vec![
                "a".to_string(),
                "b".to_string()
            ])]
        );
    }

    #[test]
    fn should_keep_unterminated_conflict_as_common_lines() {
        // given a conflict missing its closing marker
        let content = "start\n<<<<<<< HEAD\nours\n=======\ntheirs\n";
        // when
        let file = parse_conflict_file(content);
        // then nothing is dropped
        assert_eq!(file.conflict_count(), 0);
        assert_eq!(apply_resolutions(&file, &[]), content);
    }

    #[test]
    fn should_roundtrip_unresolved_conflicts_byte_for_byte() {
        // given
        let file = parse_conflict_file(SIMPLE);
        // when nothing is resolved
        let rebuilt = apply_resolutions(&file, &[ConflictResolution::Unresolved]);
        // then
        assert_eq!(rebuilt, SIMPLE);
    }

    #[test]
    fn should_apply_each_resolution_kind() {
        // given
        let file = parse_conflict_file(SIMPLE);
        // when / then
        assert_eq!(
            apply_resolutions(&file, &[ConflictResolution::Ours]),
            "line 1\nours line\nline 2\n"
        );
        assert_eq!(
            apply_resolutions(&file, &[ConflictResolution::Theirs]),
            "line 1\ntheirs line\nline 2\n"
        );
        assert_eq!(
            apply_resolutions(&file, &[ConflictResolution::Both]),
            "line 1\nours line\ntheirs line\nline 2\n"
        );
    }

    #[test]
    fn should_locate_conflict_lines_in_resolved_output() {
        // given two conflicts separated by a common line
        let content = "\
top
<<<<<<< HEAD
a1
=======
b1
>>>>>>> x
mid
<<<<<<< HEAD
a2
=======
b2
>>>>>>> x
";
        let file = parse_conflict_file(content);
        let unresolved = vec![ConflictResolution::Unresolved; 2];

        // when both are unresolved, markers sit at their literal lines
        assert_eq!(conflict_output_line(&file, &unresolved, 0), Some(2));
        assert_eq!(conflict_output_line(&file, &unresolved, 1), Some(8));
        assert_eq!(conflict_output_line(&file, &unresolved, 2), None);

        // and resolving the first (5 marker lines -> 1 content line)
        // shifts the second conflict up accordingly
        let first_ours = vec![ConflictResolution::Ours, ConflictResolution::Unresolved];
        assert_eq!(conflict_output_line(&file, &first_ours, 0), Some(2));
        assert_eq!(conflict_output_line(&file, &first_ours, 1), Some(4));
    }

    #[test]
    fn should_parse_multiple_conflicts_in_order() {
        // given
        let content = "\
<<<<<<< HEAD
a1
=======
b1
>>>>>>> x
mid
<<<<<<< HEAD
a2
=======
b2
>>>>>>> x
";
        // when
        let file = parse_conflict_file(content);
        // then
        assert_eq!(file.conflict_count(), 2);
        let blocks: Vec<_> = file.conflict_blocks().collect();
        assert_eq!(blocks[0].ours, vec!["a1"]);
        assert_eq!(blocks[1].theirs, vec!["b2"]);
    }

    #[test]
    fn should_preserve_missing_trailing_newline() {
        // given
        let content = "a\n<<<<<<<\nx\n=======\ny\n>>>>>>>";
        // when
        let file = parse_conflict_file(content);
        let rebuilt = apply_resolutions(&file, &[ConflictResolution::Unresolved]);
        // then
        assert!(!file.trailing_newline);
        assert_eq!(rebuilt, content);
    }

    #[test]
    fn should_not_treat_indented_or_longer_runs_as_markers() {
        // given content where ====== runs appear in normal text
        let content = "  <<<<<<< not a marker\n========\ntext\n";
        // when
        let file = parse_conflict_file(content);
        // then
        assert_eq!(file.conflict_count(), 0);
    }
}
