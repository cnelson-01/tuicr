//! Intra-line ("word diff") emphasis for a modified line.
//!
//! A deletion paired with the addition that replaced it usually differs in a
//! few tokens. JetBrains paints just those tokens a stronger shade so the
//! eye lands on `10` → `1000` instead of re-reading both lines. Pairing
//! follows the side-by-side layout: the k-th deletion of a run pairs with
//! the k-th addition that follows it.

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
