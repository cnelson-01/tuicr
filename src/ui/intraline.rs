//! Intra-line ("word diff") emphasis for a modified line.
//!
//! A deletion paired with the addition that replaced it usually differs in a
//! few tokens. IDE diff viewers paint just those tokens a stronger shade so the
//! eye lands on `10` → `1000` instead of re-reading both lines.
//!
//! Which lines pair is decided here too. The unified view pairs by position
//! (`pair_partners`: the k-th deletion of a run with the k-th addition that
//! follows it). The side-by-side view first looks for the likeliest
//! counterpart of each line (`align_block`) and lays its rows out from that
//! (`BlockRows`).

use std::ops::Range;

use ratatui::style::Color;
use ratatui::text::Span;

use crate::model::{DiffLine, LineOrigin};

/// Token-pair budget for the LCS table. Lines past it fall back to the
/// common prefix/suffix trim, which still isolates a single edited region.
const MAX_LCS_CELLS: usize = 250_000;

/// Below this share of unchanged characters the lines are a rewrite, not an
/// edit; whole-line colouring already says that, so emphasis would be noise.
const MIN_SHARED_FRACTION: f32 = 0.3;

/// For every line of a hunk, the index of the line it pairs with: each
/// deletion in a run with the addition at the same offset in the run of
/// additions right after it, and vice versa. Context lines and unmatched
/// lines map to `None`.
pub fn pair_partners(lines: &[DiffLine]) -> Vec<Option<usize>> {
    let mut partners = vec![None; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if lines[i].origin != LineOrigin::Deletion {
            i += 1;
            continue;
        }
        let del_start = i;
        while i < lines.len() && lines[i].origin == LineOrigin::Deletion {
            i += 1;
        }
        let add_start = i;
        while i < lines.len() && lines[i].origin == LineOrigin::Addition {
            i += 1;
        }
        let pairs = (add_start - del_start).min(i - add_start);
        for k in 0..pairs {
            partners[del_start + k] = Some(add_start + k);
            partners[add_start + k] = Some(del_start + k);
        }
    }
    partners
}

/// Line pairs a change block may weigh before it falls back to pairing by
/// position. Keeps a pathological block (hundreds of lines replaced by
/// hundreds) from stalling a reload.
const MAX_ALIGN_PAIRS: usize = 900;

/// Words compared per line when scoring a pair; the tail of a longer line
/// is ignored.
const MAX_ALIGN_WORDS: usize = 48;

/// Below this share of common words two lines are not the same line edited,
/// and score nothing.
const MIN_PAIR_SIMILARITY: f32 = 0.4;

/// How much likelier than pairing by position an alignment has to be, in
/// total, before it is used. Lines that share only boilerplate score alike
/// with several candidates; moving them around for a sliver of likeness
/// would scatter blank rows through the block for nothing.
const MIN_ALIGN_GAIN: f32 = 0.2;

/// Which line of the longer side each line of the shorter side sits beside
/// in a side-by-side change block of `dels` removed and `adds` added lines:
/// offsets into the longer side, strictly increasing, one per line of the
/// shorter side. `None` means by position (the k-th with the k-th), which
/// is also the answer whenever no line has a likelier counterpart.
///
/// An IDE diff matches the lines inside a changed block a second time, so a
/// line that was edited sits beside its edit and not beside whatever was
/// inserted above it. This does the same within the block's own row count:
/// every line of the shorter side keeps a partner, order is kept, and the
/// total likeness of the pairs is the largest possible. Ties go to the
/// earliest lines, and an alignment barely likelier than pairing by position
/// is not worth the blank rows it would scatter.
pub fn align_block(dels: &[DiffLine], adds: &[DiffLine]) -> Option<Vec<usize>> {
    let (short, long) = if dels.len() <= adds.len() {
        (dels, adds)
    } else {
        (adds, dels)
    };
    let (n, m) = (short.len(), long.len());
    if n == 0 || n == m || n * m > MAX_ALIGN_PAIRS {
        return None;
    }
    let words = |lines: &[DiffLine]| -> Vec<(String, Vec<Range<usize>>)> {
        lines
            .iter()
            .map(|l| {
                let text = l.content.trim().to_string();
                let mut tokens = tokenize(&text);
                tokens.retain(|t| text[t.clone()].starts_with(|c| class(c) == Class::Word));
                tokens.truncate(MAX_ALIGN_WORDS);
                (text, tokens)
            })
            .collect()
    };
    let (short_words, long_words) = (words(short), words(long));
    let score = |i: usize, j: usize| {
        let s = similarity(&short_words[i], &long_words[j]);
        if s >= MIN_PAIR_SIMILARITY { s } else { 0.0 }
    };

    // best[i][j]: the most likeness the first `i` short lines can collect
    // from the first `j` long ones (`i <= j`, every short line placed).
    let at = |i: usize, j: usize| i * (m + 1) + j;
    let mut best = vec![0.0_f32; (n + 1) * (m + 1)];
    let mut scores = vec![0.0_f32; n * m];
    for i in 1..=n {
        for j in i..=m - (n - i) {
            let s = score(i - 1, j - 1);
            scores[(i - 1) * m + (j - 1)] = s;
            let paired = best[at(i - 1, j - 1)] + s;
            best[at(i, j)] = if j > i {
                paired.max(best[at(i, j - 1)])
            } else {
                paired
            };
        }
    }
    let by_position: f32 = (0..n).map(|i| scores[i * m + i]).sum();
    if best[at(n, m)] < by_position + MIN_ALIGN_GAIN {
        return None;
    }
    // Walk back, leaving a long line unpaired whenever that costs nothing,
    // so equal scores settle on the earliest lines.
    let mut assign = vec![0; n];
    let (mut i, mut j) = (n, m);
    while i > 0 {
        if j > i && best[at(i, j - 1)] >= best[at(i - 1, j - 1)] + scores[(i - 1) * m + (j - 1)] {
            j -= 1;
        } else {
            assign[i - 1] = j - 1;
            i -= 1;
            j -= 1;
        }
    }
    let by_position = assign.iter().enumerate().all(|(i, j)| i == *j);
    (!by_position).then_some(assign)
}

/// The rows of a side-by-side change block, top to bottom: the longer side
/// has a line on every row, the shorter side on the rows `align_block`
/// assigned (or the first ones, by position).
#[derive(Clone, Copy)]
pub struct BlockRows<'a> {
    dels: usize,
    adds: usize,
    assign: Option<&'a [usize]>,
}

impl<'a> BlockRows<'a> {
    /// `assign` is `align_block`'s answer for the block. One that does not
    /// fit it is dropped in favour of pairing by position, so a stale
    /// answer can never index out of range.
    pub fn new(dels: usize, adds: usize, assign: Option<&'a [usize]>) -> Self {
        let (short, long) = (dels.min(adds), dels.max(adds));
        let fits = |a: &&[usize]| {
            a.len() == short
                && a.windows(2).all(|w| w[0] < w[1])
                && a.last().is_none_or(|j| *j < long)
        };
        Self {
            dels,
            adds,
            assign: assign.filter(fits),
        }
    }

    /// Number of rows: every line of the longer side gets one.
    pub fn count(&self) -> usize {
        self.dels.max(self.adds)
    }

    /// Row `row` as offsets into the block's removed and added lines.
    pub fn row(&self, row: usize) -> (Option<usize>, Option<usize>) {
        let short = self.dels.min(self.adds);
        let partner = match self.assign {
            Some(assign) => assign.binary_search(&row).ok(),
            None => (row < short).then_some(row),
        };
        if self.dels <= self.adds {
            (partner, Some(row))
        } else {
            (Some(row), partner)
        }
    }

    /// Whether `row` has a line on one side only and starts a run of such
    /// rows: where the other side folds.
    pub fn starts_one_sided_run(&self, row: usize) -> bool {
        let sides = |r: usize| {
            let (del, add) = self.row(r);
            (del.is_some(), add.is_some())
        };
        let here = sides(row);
        here != (true, true) && (row == 0 || sides(row - 1) != here)
    }
}

/// How alike two lines are, 0 to 1: the share of the longer line's word
/// characters that a longest common subsequence of words keeps. Identical
/// non-blank lines score 1 even without a word (`}` beside `}`).
fn similarity(a: &(String, Vec<Range<usize>>), b: &(String, Vec<Range<usize>>)) -> f32 {
    let ((a_text, a_words), (b_text, b_words)) = (a, b);
    if a_text.is_empty() || b_text.is_empty() {
        return 0.0;
    }
    if a_text == b_text {
        return 1.0;
    }
    let (n, m) = (a_words.len(), b_words.len());
    if n == 0 || m == 0 {
        return 0.0;
    }
    let wa = |i: usize| &a_text[a_words[i].clone()];
    let wb = |j: usize| &b_text[b_words[j].clone()];
    // Rolling rows of the weighted LCS table.
    let mut prev = vec![0usize; m + 1];
    let mut row = vec![0usize; m + 1];
    for i in 1..=n {
        for j in 1..=m {
            row[j] = if wa(i - 1) == wb(j - 1) {
                prev[j - 1] + wa(i - 1).len()
            } else {
                prev[j].max(row[j - 1])
            };
        }
        std::mem::swap(&mut prev, &mut row);
    }
    let total = |words: &[Range<usize>]| words.iter().map(|w| w.len()).sum::<usize>();
    let longest = total(a_words).max(total(b_words)).max(1);
    prev[m] as f32 / longest as f32
}

/// The text a diff line renders: its highlighted spans joined, else its
/// raw content. Ranges are computed against this so they line up with the
/// spans they will split.
pub fn rendered_text(line: &DiffLine) -> String {
    match &line.highlighted_spans {
        Some(spans) => spans.iter().map(|(_, t)| t.as_str()).collect(),
        None => line.content.clone(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class(c: char) -> Class {
    if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

/// Split into identifier runs, whitespace runs, and single punctuation
/// characters, as byte ranges.
fn tokenize(s: &str) -> Vec<Range<usize>> {
    let mut tokens = Vec::new();
    let mut iter = s.char_indices().peekable();
    while let Some((start, c)) = iter.next() {
        let cls = class(c);
        let mut end = start + c.len_utf8();
        if cls != Class::Other {
            while let Some(&(i, next)) = iter.peek() {
                if class(next) != cls {
                    break;
                }
                end = i + next.len_utf8();
                iter.next();
            }
        }
        tokens.push(start..end);
    }
    tokens
}

/// Changed byte ranges of the old line and of the new line.
pub type ChangedRanges = (Vec<Range<usize>>, Vec<Range<usize>>);

/// Byte ranges of `old` and `new` that differ, at token granularity, or
/// `None` when the lines are identical or too different to be worth it.
pub fn changed_ranges(old: &str, new: &str) -> Option<ChangedRanges> {
    if old == new {
        return None;
    }
    let a = tokenize(old);
    let b = tokenize(new);
    let ta = |i: usize| &old[a[i].clone()];
    let tb = |j: usize| &new[b[j].clone()];

    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && ta(prefix) == tb(prefix) {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && ta(a.len() - 1 - suffix) == tb(b.len() - 1 - suffix)
    {
        suffix += 1;
    }
    let (a_mid, b_mid) = (prefix..a.len() - suffix, prefix..b.len() - suffix);

    // Which middle tokens are kept by the longest common subsequence.
    let mut keep_a = vec![false; a_mid.len()];
    let mut keep_b = vec![false; b_mid.len()];
    let (n, m) = (a_mid.len(), b_mid.len());
    if n > 0 && m > 0 && (n + 1) * (m + 1) <= MAX_LCS_CELLS {
        let mut table = vec![0u32; (n + 1) * (m + 1)];
        let at = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[at(i, j)] = if ta(a_mid.start + i) == tb(b_mid.start + j) {
                    table[at(i + 1, j + 1)] + 1
                } else {
                    table[at(i + 1, j)].max(table[at(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if ta(a_mid.start + i) == tb(b_mid.start + j) {
                keep_a[i] = true;
                keep_b[j] = true;
                i += 1;
                j += 1;
            } else if table[at(i + 1, j)] >= table[at(i, j + 1)] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }

    let shared: usize = (0..prefix).map(|i| ta(i).len()).sum::<usize>()
        + (0..suffix)
            .map(|k| ta(a.len() - 1 - k).len())
            .sum::<usize>()
        + keep_a
            .iter()
            .enumerate()
            .filter(|(_, k)| **k)
            .map(|(i, _)| ta(a_mid.start + i).len())
            .sum::<usize>();
    let longest = old.len().max(new.len()).max(1);
    if (shared as f32) / (longest as f32) < MIN_SHARED_FRACTION {
        return None;
    }

    let old_ranges = collect_changed(old, &a, a_mid, &keep_a);
    let new_ranges = collect_changed(new, &b, b_mid, &keep_b);
    if old_ranges.is_empty() && new_ranges.is_empty() {
        return None;
    }
    Some((old_ranges, new_ranges))
}

/// Merge the dropped middle tokens into ranges, bridging gaps that are only
/// whitespace so `foo bar` → `baz qux` reads as one edit, then trim each
/// range's outer whitespace (unless the change is whitespace alone, like a
/// re-indent).
fn collect_changed(
    s: &str,
    tokens: &[Range<usize>],
    mid: Range<usize>,
    keep: &[bool],
) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (k, kept) in keep.iter().enumerate() {
        if *kept {
            continue;
        }
        let tok = tokens[mid.start + k].clone();
        match out.last_mut() {
            Some(last) if s[last.end..tok.start].trim().is_empty() => last.end = tok.end,
            _ => out.push(tok),
        }
    }
    for r in &mut out {
        let text = &s[r.clone()];
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lead = text.len() - text.trim_start().len();
        r.start += lead;
        r.end = r.start + trimmed.len();
    }
    out
}

/// Repaint the background of the bytes in `ranges` (offsets into the
/// concatenated span text) with `bg`, splitting spans at range edges.
pub fn emphasize(
    spans: Vec<Span<'static>>,
    ranges: &[Range<usize>],
    bg: Color,
) -> Vec<Span<'static>> {
    if ranges.is_empty() {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len() + ranges.len() * 2);
    let mut offset = 0;
    for span in spans {
        let text = span.content.as_ref();
        let (start, end) = (offset, offset + text.len());
        offset = end;
        let mut cuts = vec![start, end];
        for r in ranges {
            for edge in [r.start, r.end] {
                if edge > start && edge < end && text.is_char_boundary(edge - start) {
                    cuts.push(edge);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let piece = text[a - start..b - start].to_string();
            let inside = ranges.iter().any(|r| r.start <= a && b <= r.end);
            let style = if inside {
                span.style.bg(bg)
            } else {
                span.style
            };
            out.push(Span::styled(piece, style));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(origin: LineOrigin, content: &str) -> DiffLine {
        DiffLine {
            origin,
            content: content.to_string(),
            old_lineno: None,
            new_lineno: None,
            raw_content: None,
            highlighted_spans: None,
        }
    }

    fn slices<'a>(s: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|r| &s[r.clone()]).collect()
    }

    #[test]
    fn isolates_the_changed_number() {
        let old = "    return alpha * 10 + beta - gamma";
        let new = "    return alpha * 1000 + beta - gamma";
        let (o, n) = changed_ranges(old, new).expect("ranges");
        assert_eq!(slices(old, &o), vec!["10"]);
        assert_eq!(slices(new, &n), vec!["1000"]);
    }

    #[test]
    fn finds_changes_on_both_ends_of_a_line() {
        let old = "let foo = bar(1);";
        let new = "let baz = bar(2);";
        let (o, n) = changed_ranges(old, new).expect("ranges");
        assert_eq!(slices(old, &o), vec!["foo", "1"]);
        assert_eq!(slices(new, &n), vec!["baz", "2"]);
    }

    #[test]
    fn pure_insertion_marks_only_the_new_side() {
        let old = "call(a, b)";
        let new = "call(a, extra, b)";
        let (o, n) = changed_ranges(old, new).expect("ranges");
        assert!(o.is_empty());
        assert_eq!(slices(new, &n), vec!["extra,"]);
    }

    #[test]
    fn rewrites_get_no_emphasis() {
        assert!(changed_ranges("fn alpha() {}", "struct Totally;").is_none());
        assert!(changed_ranges("same", "same").is_none());
    }

    #[test]
    fn pairs_follow_side_by_side_rows() {
        let lines = vec![
            line(LineOrigin::Context, "c"),
            line(LineOrigin::Deletion, "d1"),
            line(LineOrigin::Deletion, "d2"),
            line(LineOrigin::Addition, "a1"),
            line(LineOrigin::Context, "c"),
            line(LineOrigin::Addition, "lone"),
        ];
        assert_eq!(
            pair_partners(&lines),
            vec![None, Some(3), None, Some(1), None, None]
        );
    }

    fn lines(origin: LineOrigin, contents: &[&str]) -> Vec<DiffLine> {
        contents.iter().map(|c| line(origin, c)).collect()
    }

    #[test]
    fn a_removed_line_sits_beside_the_added_line_most_like_it() {
        let dels = lines(
            LineOrigin::Deletion,
            &["    let total = compute(alpha, beta);"],
        );
        let adds = lines(
            LineOrigin::Addition,
            &[
                "    // widen the sum",
                "    let extra = 1;",
                "    let total = compute(alpha, beta, extra);",
            ],
        );
        assert_eq!(align_block(&dels, &adds), Some(vec![2]));
        // The same holds with the sides swapped: the lone added line picks
        // its counterpart among the removed ones.
        assert_eq!(align_block(&adds, &dels), Some(vec![2]));
    }

    #[test]
    fn unlike_lines_pair_by_position() {
        let dels = lines(LineOrigin::Deletion, &["GONE1"]);
        let adds = lines(LineOrigin::Addition, &["KEPT1", "KEPT2"]);
        assert_eq!(align_block(&dels, &adds), None);
        // Equal counts leave nothing to choose.
        let adds = lines(LineOrigin::Addition, &["GONE1 again"]);
        assert_eq!(align_block(&dels, &adds), None);
        // A match that is already first is by position too.
        let adds = lines(LineOrigin::Addition, &["GONE1", "KEPT2"]);
        assert_eq!(align_block(&dels, &adds), None);
        // Blank lines never count as a match.
        let dels = lines(LineOrigin::Deletion, &[""]);
        let adds = lines(LineOrigin::Addition, &["first", ""]);
        assert_eq!(align_block(&dels, &adds), None);
    }

    #[test]
    fn a_slightly_likelier_pairing_does_not_beat_position() {
        // Every line shares the same boilerplate; the second added line is
        // a touch closer to the third removed one than to the second.
        let dels = lines(
            LineOrigin::Deletion,
            &[
                "fn step_28(x: u32) -> u32 { x + 28 }",
                "fn step_29(x: u32) -> u32 { x + 29 }",
                "fn step_30(x: u32) -> u32 { x + 30 }",
            ],
        );
        let adds = lines(
            LineOrigin::Addition,
            &[
                "fn merged_a(x: u32) -> u32 { x + 28 + 29 }",
                "fn merged_b(x: u32) -> u32 { x + 30 + 31 }",
            ],
        );
        assert_eq!(align_block(&dels, &adds), None);
    }

    #[test]
    fn alignment_keeps_line_order() {
        // The second removed line is most like the first added line, but
        // pairing them would cross the first pair.
        let dels = lines(
            LineOrigin::Deletion,
            &["alpha one two three", "gamma four five six"],
        );
        let adds = lines(
            LineOrigin::Addition,
            &[
                "gamma four five six seven",
                "unrelated",
                "alpha one two three four",
                "trailing",
            ],
        );
        let assign = align_block(&dels, &adds).expect("alignment");
        assert!(assign[0] < assign[1], "{assign:?}");
        // One of the two likely pairs is kept; which one, the scores decide.
        assert!(assign == [0, 1] || assign == [2, 3], "{assign:?}");
    }

    #[test]
    fn oversized_blocks_pair_by_position() {
        let dels = lines(LineOrigin::Deletion, &["let needle = 1;"; 10]);
        let mut contents = vec!["filler"; 100];
        contents[99] = "let needle = 2;";
        let adds = lines(LineOrigin::Addition, &contents);
        assert!(dels.len() * adds.len() > MAX_ALIGN_PAIRS);
        assert_eq!(align_block(&dels, &adds), None);
    }

    #[test]
    fn block_rows_follow_the_alignment() {
        let rows = BlockRows::new(1, 3, Some(&[2]));
        assert_eq!(rows.count(), 3);
        let all: Vec<_> = (0..3).map(|r| rows.row(r)).collect();
        assert_eq!(
            all,
            vec![(None, Some(0)), (None, Some(1)), (Some(0), Some(2))]
        );
        assert!(rows.starts_one_sided_run(0));
        assert!(!rows.starts_one_sided_run(1));
        assert!(!rows.starts_one_sided_run(2));

        // More removed than added: the old side has a line on every row.
        let rows = BlockRows::new(3, 1, Some(&[1]));
        let all: Vec<_> = (0..3).map(|r| rows.row(r)).collect();
        assert_eq!(
            all,
            vec![(Some(0), None), (Some(1), Some(0)), (Some(2), None)]
        );
        assert!(rows.starts_one_sided_run(0));
        assert!(rows.starts_one_sided_run(2));
    }

    #[test]
    fn block_rows_ignore_an_alignment_that_does_not_fit() {
        let by_position = vec![(Some(0), Some(0)), (None, Some(1)), (None, Some(2))];
        for stale in [&[3][..], &[0, 1][..], &[][..]] {
            let rows = BlockRows::new(1, 3, Some(stale));
            let all: Vec<_> = (0..3).map(|r| rows.row(r)).collect();
            assert_eq!(all, by_position, "{stale:?}");
        }
        let rows = BlockRows::new(1, 3, None);
        assert!(!rows.starts_one_sided_run(0));
        assert!(rows.starts_one_sided_run(1));
        assert!(!rows.starts_one_sided_run(2));
    }

    #[test]
    fn emphasize_splits_spans_at_range_edges() {
        use ratatui::style::Style;
        let spans = vec![
            Span::styled("abc".to_string(), Style::default()),
            Span::styled("def".to_string(), Style::default().fg(Color::Red)),
        ];
        let range = 2..4;
        let out = emphasize(spans, std::slice::from_ref(&range), Color::Blue);
        let texts: Vec<&str> = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["ab", "c", "d", "ef"]);
        assert_eq!(out[1].style.bg, Some(Color::Blue));
        assert_eq!(out[2].style.bg, Some(Color::Blue));
        assert_eq!(out[2].style.fg, Some(Color::Red));
        assert_eq!(out[0].style.bg, None);
    }
}
