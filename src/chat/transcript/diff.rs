//! Line diffs for file edits shown in the transcript.
//!
//! `lines` is a plain Myers edit script over line slices; `unified` turns it
//! into hunks with context, merging neighbours, and marks the words that
//! changed inside a replaced line so the screen can emphasise them. Pure
//! functions, no I/O.

use unicode_segmentation::UnicodeSegmentation;

/// Above this many lines on either side the quadratic worst case of Myers is
/// too slow for a frame, so the diff degrades to "everything replaced".
pub(crate) const MAX_LINES: usize = 10_000;

/// Lines longer than this on either side of a pair are not word-diffed: the
/// tokeniser and the inner Myers would dominate render time for no legible gain.
const MAX_EMPHASIS_BYTES: usize = 1024;

/// How one element of an edit script relates `before` to `after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    Equal,
    Removed,
    Added,
}

/// Line-level Myers diff (O(ND), no linear-space refinement). Above `MAX_LINES` (10_000) on either
/// side, returns everything removed then everything added.
pub(crate) fn lines<'a>(before: &[&'a str], after: &[&'a str]) -> Vec<(Change, &'a str)> {
    if before.len() > MAX_LINES || after.len() > MAX_LINES {
        return before
            .iter()
            .map(|line| (Change::Removed, *line))
            .chain(after.iter().map(|line| (Change::Added, *line)))
            .collect();
    }
    let mut old = 0;
    let mut new = 0;
    edit_script(before, after)
        .into_iter()
        .map(|change| match change {
            Change::Equal => {
                old += 1;
                new += 1;
                (Change::Equal, before[old - 1])
            }
            Change::Removed => {
                old += 1;
                (Change::Removed, before[old - 1])
            }
            Change::Added => {
                new += 1;
                (Change::Added, after[new - 1])
            }
        })
        .collect()
}

/// One unified-diff hunk. Positions are 1-based like `@@ -a,b +c,d @@`; a side
/// with zero lines reports the line before the change, as diff tools do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub(crate) old_start: usize,
    pub(crate) old_count: usize,
    pub(crate) new_start: usize,
    pub(crate) new_count: usize,
    pub(crate) lines: Vec<HunkLine>,
}

/// One row of a hunk with its line numbers on the sides it exists on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HunkLine {
    Context {
        old: usize,
        new: usize,
        text: String,
    },
    /// `emphasis` is the byte ranges that differ from the paired line, when there is a pair
    /// (word-level, both sides under 1 KiB), else empty.
    Removed {
        old: usize,
        text: String,
        emphasis: Vec<std::ops::Range<usize>>,
    },
    Added {
        new: usize,
        text: String,
        emphasis: Vec<std::ops::Range<usize>>,
    },
}

/// Unified hunks with `context` lines, hunks closer than 2*context merged. Lines are split on
/// '\n' with a trailing '\r' kept in text (CRLF files round-trip). A missing final newline is
/// not marked. `before == None` means a created file: one hunk, every line Added. An empty
/// created file has nothing to show and yields no hunks.
pub(crate) fn unified(before: Option<&str>, after: &str, context: usize) -> Vec<Hunk> {
    let Some(before) = before else {
        let lines = split_lines(after);
        if lines.is_empty() {
            return Vec::new();
        }
        return vec![Hunk {
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: lines.len(),
            lines: lines
                .iter()
                .enumerate()
                .map(|(index, line)| HunkLine::Added {
                    new: index + 1,
                    text: (*line).to_owned(),
                    emphasis: Vec::new(),
                })
                .collect(),
        }];
    };
    let script = lines(&split_lines(before), &split_lines(after));

    // Group the script into hunk windows: each run of changes widened by
    // `context` on both sides, with windows that touch or overlap merged.
    let mut windows: Vec<(usize, usize)> = Vec::new();
    let mut index = 0;
    while index < script.len() {
        if script[index].0 == Change::Equal {
            index += 1;
            continue;
        }
        let mut end = index;
        while end + 1 < script.len() && script[end + 1].0 != Change::Equal {
            end += 1;
        }
        match windows.last_mut() {
            Some((_, last_end)) if index - *last_end - 1 <= 2 * context => *last_end = end,
            _ => windows.push((index, end)),
        }
        index = end + 1;
    }

    let mut hunks = Vec::with_capacity(windows.len());
    let mut cursor = 0;
    let mut old_line = 0;
    let mut new_line = 0;
    for (first, last) in windows {
        let start = first.saturating_sub(context);
        let end = (last + context + 1).min(script.len());
        // Lines skipped between hunks still advance the line counters.
        for (change, _) in &script[cursor..start] {
            advance(*change, &mut old_line, &mut new_line);
        }
        let old_before = old_line;
        let new_before = new_line;
        let mut rows = Vec::with_capacity(end - start);
        let mut position = start;
        while position < end {
            let (change, text) = script[position];
            if change == Change::Equal {
                old_line += 1;
                new_line += 1;
                rows.push(HunkLine::Context {
                    old: old_line,
                    new: new_line,
                    text: text.to_owned(),
                });
                position += 1;
                continue;
            }
            // One replaced block: all removals, then all additions, paired by
            // position for word emphasis.
            let mut removed = Vec::new();
            let mut added = Vec::new();
            while position < end && script[position].0 != Change::Equal {
                match script[position] {
                    (Change::Removed, text) => removed.push(text),
                    (_, text) => added.push(text),
                }
                position += 1;
            }
            for (offset, text) in removed.iter().enumerate() {
                old_line += 1;
                let emphasis = added
                    .get(offset)
                    .map_or_else(Vec::new, |pair| word_emphasis(text, pair).0);
                rows.push(HunkLine::Removed {
                    old: old_line,
                    text: (*text).to_owned(),
                    emphasis,
                });
            }
            for (offset, text) in added.iter().enumerate() {
                new_line += 1;
                let emphasis = removed
                    .get(offset)
                    .map_or_else(Vec::new, |pair| word_emphasis(pair, text).1);
                rows.push(HunkLine::Added {
                    new: new_line,
                    text: (*text).to_owned(),
                    emphasis,
                });
            }
        }
        let old_count = old_line - old_before;
        let new_count = new_line - new_before;
        hunks.push(Hunk {
            old_start: if old_count == 0 {
                old_before
            } else {
                old_before + 1
            },
            old_count,
            new_start: if new_count == 0 {
                new_before
            } else {
                new_before + 1
            },
            new_count,
            lines: rows,
        });
        cursor = end;
    }
    hunks
}

fn advance(change: Change, old_line: &mut usize, new_line: &mut usize) {
    match change {
        Change::Equal => {
            *old_line += 1;
            *new_line += 1;
        }
        Change::Removed => *old_line += 1,
        Change::Added => *new_line += 1,
    }
}

/// Split on '\n', keeping any '\r' in the line. A trailing newline does not
/// produce an empty final line; an empty text has no lines.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n').collect()
}

/// Byte ranges that differ between a removed line and its added pair, as
/// (ranges in `removed`, ranges in `added`). Tokens are Unicode word and
/// non-word runs; adjacent changed tokens coalesce into one range.
fn word_emphasis(
    removed: &str,
    added: &str,
) -> (Vec<std::ops::Range<usize>>, Vec<std::ops::Range<usize>>) {
    if removed.len() >= MAX_EMPHASIS_BYTES || added.len() >= MAX_EMPHASIS_BYTES {
        return (Vec::new(), Vec::new());
    }
    let old_tokens: Vec<&str> = removed.split_word_bounds().collect();
    let new_tokens: Vec<&str> = added.split_word_bounds().collect();
    let mut old_ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut new_ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut old_offset = 0;
    let mut new_offset = 0;
    let mut old_index = 0;
    let mut new_index = 0;
    for change in edit_script(&old_tokens, &new_tokens) {
        match change {
            Change::Equal => {
                old_offset += old_tokens[old_index].len();
                new_offset += new_tokens[new_index].len();
                old_index += 1;
                new_index += 1;
            }
            Change::Removed => {
                let len = old_tokens[old_index].len();
                push_range(&mut old_ranges, old_offset..old_offset + len);
                old_offset += len;
                old_index += 1;
            }
            Change::Added => {
                let len = new_tokens[new_index].len();
                push_range(&mut new_ranges, new_offset..new_offset + len);
                new_offset += len;
                new_index += 1;
            }
        }
    }
    (old_ranges, new_ranges)
}

fn push_range(ranges: &mut Vec<std::ops::Range<usize>>, range: std::ops::Range<usize>) {
    match ranges.last_mut() {
        Some(last) if last.end == range.start => last.end = range.end,
        _ => ranges.push(range),
    }
}

/// Myers' greedy O(ND) edit script. The common prefix and suffix are peeled
/// first, which keeps the trace small for the usual "edit in the middle" case.
/// The trace keeps only the diagonals each round can consult, so memory is
/// O(D^2) rather than O(D * (N + M)).
fn edit_script<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Change> {
    let prefix = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let a = &a[prefix..a.len() - suffix];
    let b = &b[prefix..b.len() - suffix];
    let n = a.len();
    let m = b.len();

    let mut script = vec![Change::Equal; prefix];
    script.extend(middle_script(a, b, n, m));
    script.extend(std::iter::repeat_n(Change::Equal, suffix));
    script
}

/// The edit script for a slice pair with no common prefix or suffix.
fn middle_script<T: PartialEq>(a: &[T], b: &[T], n: usize, m: usize) -> Vec<Change> {
    if n == 0 {
        return vec![Change::Added; m];
    }
    if m == 0 {
        return vec![Change::Removed; n];
    }
    let max = n + m;
    let offset = max as isize;
    // v[k + offset] is the furthest x reached on diagonal k = x - y.
    let mut v = vec![0usize; 2 * max + 2];
    // trace[d] holds v for diagonals -(d-1)..=(d-1) as they were before round d.
    let mut trace: Vec<Vec<usize>> = Vec::new();
    let at = |k: isize| (k + offset) as usize;
    let mut found = None;
    'rounds: for d in 0..=max as isize {
        let window = if d == 0 {
            Vec::new()
        } else {
            v[at(-(d - 1))..=at(d - 1)].to_vec()
        };
        trace.push(window);
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && a[x] == b[y] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'rounds;
            }
            k += 2;
        }
    }
    let Some(found) = found else {
        // Unreachable: round n + m always reaches the far corner.
        return Vec::new();
    };

    let mut edits = Vec::with_capacity(max);
    let mut x = n;
    let mut y = m;
    for d in (0..=found).rev() {
        let window = &trace[d as usize];
        let k = x as isize - y as isize;
        if d == 0 {
            while x > 0 && y > 0 {
                edits.push(Change::Equal);
                x -= 1;
                y -= 1;
            }
            break;
        }
        let read = |diag: isize| window[(diag + d - 1) as usize];
        let prev_k = if k == -d || (k != d && read(k - 1) < read(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = read(prev_k);
        let prev_y = (prev_x as isize - prev_k) as usize;
        while x > prev_x && y > prev_y {
            edits.push(Change::Equal);
            x -= 1;
            y -= 1;
        }
        if x == prev_x {
            edits.push(Change::Added);
        } else {
            edits.push(Change::Removed);
        }
        x = prev_x;
        y = prev_y;
    }
    edits.reverse();
    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// One emphasis range as a `Vec`, spelled out so the literal is not a
    /// `vec![a..b]` that clippy reads as a mistaken range collect.
    fn span(range: std::ops::Range<usize>) -> Vec<std::ops::Range<usize>> {
        vec![range]
    }

    fn apply<'a>(script: &[(Change, &'a str)]) -> Vec<&'a str> {
        script
            .iter()
            .filter(|(change, _)| *change != Change::Removed)
            .map(|(_, line)| *line)
            .collect()
    }

    fn removed<'a>(script: &[(Change, &'a str)]) -> Vec<&'a str> {
        script
            .iter()
            .filter(|(change, _)| *change != Change::Added)
            .map(|(_, line)| *line)
            .collect()
    }

    #[test]
    fn empty_inputs_produce_an_empty_script_and_no_hunks() {
        assert!(lines(&[], &[]).is_empty());
        assert!(unified(Some(""), "", 3).is_empty());
    }

    #[test]
    fn identical_inputs_are_all_equal() {
        let before = ["a", "b", "c"];
        let script = lines(&before, &before);
        assert_eq!(
            script,
            vec![
                (Change::Equal, "a"),
                (Change::Equal, "b"),
                (Change::Equal, "c")
            ]
        );
        assert!(unified(Some("a\nb\nc\n"), "a\nb\nc\n", 3).is_empty());
    }

    #[test]
    fn one_line_change_is_a_pair_with_emphasis() {
        let hunks = unified(Some("let x = 1;\n"), "let x = 2;\n", 3);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];
        assert_eq!((hunk.old_start, hunk.old_count), (1, 1));
        assert_eq!((hunk.new_start, hunk.new_count), (1, 1));
        assert_eq!(
            hunk.lines,
            vec![
                HunkLine::Removed {
                    old: 1,
                    text: "let x = 1;".into(),
                    emphasis: span(8..9),
                },
                HunkLine::Added {
                    new: 1,
                    text: "let x = 2;".into(),
                    emphasis: span(8..9),
                },
            ]
        );
    }

    #[test]
    fn insert_at_end_reports_the_line_before_on_the_old_side() {
        let hunks = unified(Some("a\nb\n"), "a\nb\nc\n", 1);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];
        assert_eq!((hunk.old_start, hunk.old_count), (2, 1));
        assert_eq!((hunk.new_start, hunk.new_count), (2, 2));
        assert_eq!(
            hunk.lines,
            vec![
                HunkLine::Context {
                    old: 2,
                    new: 2,
                    text: "b".into()
                },
                HunkLine::Added {
                    new: 3,
                    text: "c".into(),
                    emphasis: Vec::new()
                },
            ]
        );

        // Pure insertion into an empty file: old side is empty at line 0.
        let hunks = unified(Some(""), "x\n", 3);
        assert_eq!((hunks[0].old_start, hunks[0].old_count), (0, 0));
        assert_eq!((hunks[0].new_start, hunks[0].new_count), (1, 1));
    }

    #[test]
    fn crlf_is_kept_in_line_text() {
        let hunks = unified(Some("a\r\nb\r\n"), "a\r\nc\r\n", 0);
        assert_eq!(
            hunks[0].lines,
            vec![
                HunkLine::Removed {
                    old: 2,
                    text: "b\r".into(),
                    emphasis: span(0..1),
                },
                HunkLine::Added {
                    new: 2,
                    text: "c\r".into(),
                    emphasis: span(0..1),
                },
            ]
        );
        // A missing final newline is not a difference by itself.
        assert!(unified(Some("a\nb"), "a\nb\n", 3).is_empty());
    }

    #[test]
    fn created_file_is_one_hunk_of_additions() {
        let hunks = unified(None, "one\ntwo\n", 3);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];
        assert_eq!((hunk.old_start, hunk.old_count), (0, 0));
        assert_eq!((hunk.new_start, hunk.new_count), (1, 2));
        assert_eq!(
            hunk.lines,
            vec![
                HunkLine::Added {
                    new: 1,
                    text: "one".into(),
                    emphasis: Vec::new()
                },
                HunkLine::Added {
                    new: 2,
                    text: "two".into(),
                    emphasis: Vec::new()
                },
            ]
        );
        assert!(unified(None, "", 3).is_empty());
    }

    #[test]
    fn nearby_changes_merge_into_one_hunk() {
        // Two changes separated by exactly 2*context equal lines share a hunk.
        let before = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let after = "A\nb\nc\nd\ne\nf\ng\nH\n";
        let hunks = unified(Some(before), after, 3);
        assert_eq!(hunks.len(), 1);
        assert_eq!((hunks[0].old_start, hunks[0].old_count), (1, 8));
        assert_eq!((hunks[0].new_start, hunks[0].new_count), (1, 8));
        assert_eq!(hunks[0].lines.len(), 10);
    }

    #[test]
    fn distant_changes_stay_separate_hunks() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
        let after = "A\nb\nc\nd\ne\nf\ng\nh\nI\n";
        let hunks = unified(Some(before), after, 3);
        assert_eq!(hunks.len(), 2);
        assert_eq!((hunks[0].old_start, hunks[0].old_count), (1, 4));
        assert_eq!((hunks[1].old_start, hunks[1].old_count), (6, 4));
        assert_eq!((hunks[1].new_start, hunks[1].new_count), (6, 4));
        assert!(matches!(
            hunks[1].lines.last(),
            Some(HunkLine::Added { new: 9, .. })
        ));
    }

    #[test]
    fn over_the_cap_everything_is_replaced() {
        let before: Vec<String> = (0..=MAX_LINES).map(|i| i.to_string()).collect();
        let before: Vec<&str> = before.iter().map(String::as_str).collect();
        let after = ["0", "1"];
        let script = lines(&before, &after);
        assert_eq!(script.len(), before.len() + after.len());
        assert!(script[..before.len()]
            .iter()
            .all(|(change, _)| *change == Change::Removed));
        assert!(script[before.len()..]
            .iter()
            .all(|(change, _)| *change == Change::Added));
        assert_eq!(apply(&script), after);
    }

    #[test]
    fn word_emphasis_marks_only_changed_tokens() {
        let (old, new) = word_emphasis("the quick brown fox", "the slow brown fox jumps");
        assert_eq!(old, span(4..9));
        assert_eq!(new, vec![4..8, 18..24]);

        // Unicode words: emphasis stays on char boundaries.
        let (old, new) = word_emphasis("héllo wörld", "héllo monde");
        assert_eq!(&"héllo wörld"[old[0].clone()], "wörld");
        assert_eq!(&"héllo monde"[new[0].clone()], "monde");

        // Long lines are not word-diffed.
        let long = "x".repeat(MAX_EMPHASIS_BYTES);
        assert_eq!(word_emphasis(&long, "x"), (Vec::new(), Vec::new()));

        // Pairing within a replaced block is positional: the third removed
        // line has no partner and gets no emphasis.
        let hunks = unified(Some("a b\nc d\ne f\n"), "a B\nc D\n", 0);
        let emphasis: Vec<_> = hunks[0]
            .lines
            .iter()
            .map(|line| match line {
                HunkLine::Removed { emphasis, .. } | HunkLine::Added { emphasis, .. } => {
                    emphasis.clone()
                }
                HunkLine::Context { .. } => Vec::new(),
            })
            .collect();
        assert_eq!(
            emphasis,
            vec![span(2..3), span(2..3), Vec::new(), span(2..3), span(2..3)]
        );
    }

    #[test]
    fn hunk_counts_match_their_rows() {
        let before = "a\nb\nc\nd\n";
        let after = "a\nx\ny\nd\nz\n";
        for hunk in unified(Some(before), after, 1) {
            let old = hunk
                .lines
                .iter()
                .filter(|line| !matches!(line, HunkLine::Added { .. }))
                .count();
            let new = hunk
                .lines
                .iter()
                .filter(|line| !matches!(line, HunkLine::Removed { .. }))
                .count();
            assert_eq!(hunk.old_count, old);
            assert_eq!(hunk.new_count, new);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn applying_the_script_reproduces_both_sides(
            before in proptest::collection::vec("[ab]{0,2}", 0..24),
            after in proptest::collection::vec("[ab]{0,2}", 0..24),
        ) {
            let before: Vec<&str> = before.iter().map(String::as_str).collect();
            let after: Vec<&str> = after.iter().map(String::as_str).collect();
            let script = lines(&before, &after);
            prop_assert_eq!(apply(&script), after);
            prop_assert_eq!(removed(&script), before);
        }

        #[test]
        fn word_emphasis_stays_within_char_boundaries(
            old in "[a-c é,]{0,12}",
            new in "[a-c é,]{0,12}",
        ) {
            let (old_ranges, new_ranges) = word_emphasis(&old, &new);
            for range in old_ranges {
                prop_assert!(old.is_char_boundary(range.start) && old.is_char_boundary(range.end));
            }
            for range in new_ranges {
                prop_assert!(new.is_char_boundary(range.start) && new.is_char_boundary(range.end));
            }
        }
    }
}
