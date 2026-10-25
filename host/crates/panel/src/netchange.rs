//! The net change of one file over a turn: a bounded, line-based reconstruction from
//! the file-change tool calls the agent made, never guessed.
//!
//! - A `Write` is a full snapshot. Later edits apply to it, and their old text must be found in it
//!   (otherwise the history is inconsistent and the counts are marked incomplete).
//! - Without a snapshot, edits are tracked as regions: an edit whose old text lies inside an earlier
//!   edit's new text, or whose old text contains it, merges with it; anything else is a new region.
//!   A partial overlap cannot be verified and marks the result incomplete.
//! - Counts come from a line-level longest-common-subsequence diff under a fixed budget (inputs at
//!   most 256 KiB and 20,000 lines, at most 4 million cells). Repeated edits count once, a revert
//!   cancels, creating then editing counts the final contents. Over budget: incomplete.

pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_LINES: usize = 20_000;
pub const MAX_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A full snapshot (the Write tool's content). `created` is true only when the tool reported
    /// creating the file; overwriting an existing file leaves the earlier contents unknown.
    Write { content: String, created: bool },
    /// A replacement of `old` with `new` (Edit, or one MultiEdit step). `all` replaces every match.
    Edit { old: String, new: String, all: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Net {
    pub added: u64,
    pub removed: u64,
    pub complete: bool,
}

fn lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return vec![];
    }
    s.trim_end_matches('\n').split('\n').collect()
}

/// Added and removed line counts between two texts, or None over budget.
pub fn line_diff(a: &str, b: &str) -> Option<(u64, u64)> {
    if a.len() > MAX_BYTES || b.len() > MAX_BYTES {
        return None;
    }
    let (x, y) = (lines(a), lines(b));
    if x.len() > MAX_LINES || y.len() > MAX_LINES {
        return None;
    }
    // Trim the common head and tail first: most edits touch a small middle.
    let mut start = 0;
    while start < x.len() && start < y.len() && x[start] == y[start] {
        start += 1;
    }
    let mut end = 0;
    while end < x.len() - start
        && end < y.len() - start
        && x[x.len() - 1 - end] == y[y.len() - 1 - end]
    {
        end += 1;
    }
    let xs = &x[start..x.len() - end];
    let ys = &y[start..y.len() - end];
    if xs.len().saturating_mul(ys.len()) > MAX_CELLS {
        return None;
    }
    // LCS length with two rows.
    let mut prev = vec![0u32; ys.len() + 1];
    let mut cur = vec![0u32; ys.len() + 1];
    for xi in xs {
        for (j, yj) in ys.iter().enumerate() {
            cur[j + 1] = if xi == yj {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let lcs = prev[ys.len()] as u64;
    Some((ys.len() as u64 - lcs, xs.len() as u64 - lcs))
}

struct Region {
    old: String,
    new: String,
}

/// What a file's events settle to: a snapshot (known or unknown base, final text) or edit regions.
struct Settled {
    complete: bool,
    snapshot: Option<(Option<String>, String)>,
    regions: Vec<Region>,
}

/// Plays a file's events in order.
fn settle(events: &[Event]) -> Settled {
    let mut complete = true;
    // With a snapshot: the known text before (None = unknown) and now.
    let mut snapshot: Option<(Option<String>, String)> = None;
    let mut regions: Vec<Region> = Vec::new();
    for (i, e) in events.iter().enumerate() {
        match e {
            Event::Write { content, created } => {
                if i == 0 {
                    // Creating the file: the baseline is empty. Overwriting one: it is unknown.
                    let base = created.then(String::new);
                    snapshot = Some((base, content.clone()));
                } else if snapshot.is_some() {
                    if let Some((_, now)) = snapshot.as_mut() {
                        *now = content.clone();
                    }
                } else {
                    // Edits on an unknown base, then a full write: the base is still unknown.
                    snapshot = Some((None, content.clone()));
                    regions.clear();
                }
            }
            Event::Edit { old, new, all } => {
                if let Some((_, now)) = snapshot.as_mut() {
                    if old.is_empty() || !now.contains(old.as_str()) {
                        complete = false;
                        continue;
                    }
                    *now = if *all {
                        now.replace(old.as_str(), new)
                    } else {
                        now.replacen(old.as_str(), new, 1)
                    };
                    continue;
                }
                // Without the file, how many places "replace all" changed is unknown.
                if old.is_empty() || *all {
                    complete = false;
                    continue;
                }
                if let Some(r) = regions.iter_mut().find(|r| r.new.contains(old.as_str())) {
                    r.new = if *all {
                        r.new.replace(old.as_str(), new)
                    } else {
                        r.new.replacen(old.as_str(), new, 1)
                    };
                } else if let Some(idx) = regions
                    .iter()
                    .position(|r| !r.new.is_empty() && contains_whole_lines(old, &r.new))
                {
                    // The new edit spans an earlier one: rebuild the region's original text.
                    let r = regions.remove(idx);
                    let original = old.replacen(r.new.as_str(), &r.old, 1);
                    regions.push(Region {
                        old: original,
                        new: new.clone(),
                    });
                } else if regions.iter().any(|r| {
                    overlaps(&r.new, old) || (!r.new.is_empty() && old.contains(r.new.as_str()))
                }) {
                    // A partial overlap, or an earlier edit's text inside this one mid-line (it
                    // may be the same place or another): it cannot be verified.
                    complete = false;
                    regions.push(Region {
                        old: old.clone(),
                        new: new.clone(),
                    });
                } else {
                    regions.push(Region {
                        old: old.clone(),
                        new: new.clone(),
                    });
                }
            }
        }
    }
    Settled {
        complete,
        snapshot,
        regions,
    }
}

/// The net change of a file's events in order.
pub fn net(events: &[Event]) -> Net {
    let Settled {
        complete,
        snapshot,
        regions,
    } = settle(events);
    let mut total = Net {
        added: 0,
        removed: 0,
        complete,
    };
    let mut add = |a: &str, b: &str| match line_diff(a, b) {
        Some((ad, rm)) => {
            total.added += ad;
            total.removed += rm;
        }
        None => total.complete = false,
    };
    let base_unknown = snapshot.as_ref().is_some_and(|(b, _)| b.is_none());
    match snapshot {
        Some((Some(base), now)) => add(&base, &now),
        Some((None, _)) => total.complete = false,
        None => {
            for r in &regions {
                add(&r.old, &r.new);
            }
        }
    }
    if !total.complete && base_unknown {
        total.added = 0;
        total.removed = 0;
    }
    total
}

#[derive(Clone, Copy, PartialEq)]
enum Op<'a> {
    Keep(&'a str),
    Del(&'a str),
    Add(&'a str),
}

/// A line-level edit script from `a` to `b`, or None over the same budget as [`line_diff`].
fn edit_script<'a>(a: &'a str, b: &'a str) -> Option<Vec<Op<'a>>> {
    if a.len() > MAX_BYTES || b.len() > MAX_BYTES {
        return None;
    }
    let (x, y) = (lines(a), lines(b));
    if x.len() > MAX_LINES || y.len() > MAX_LINES {
        return None;
    }
    let mut start = 0;
    while start < x.len() && start < y.len() && x[start] == y[start] {
        start += 1;
    }
    let mut end = 0;
    while end < x.len() - start
        && end < y.len() - start
        && x[x.len() - 1 - end] == y[y.len() - 1 - end]
    {
        end += 1;
    }
    let xs = &x[start..x.len() - end];
    let ys = &y[start..y.len() - end];
    if xs.len().saturating_mul(ys.len()) > MAX_CELLS {
        return None;
    }
    // lcs[i][j]: the common length of xs[i..] and ys[j..].
    let w = ys.len() + 1;
    let mut lcs = vec![0u32; (xs.len() + 1) * w];
    for i in (0..xs.len()).rev() {
        for j in (0..ys.len()).rev() {
            lcs[i * w + j] = if xs[i] == ys[j] {
                lcs[(i + 1) * w + j + 1] + 1
            } else {
                lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
            };
        }
    }
    let mut out: Vec<Op> = x[..start].iter().map(|l| Op::Keep(l)).collect();
    let (mut i, mut j) = (0, 0);
    while i < xs.len() || j < ys.len() {
        if i < xs.len() && j < ys.len() && xs[i] == ys[j] {
            out.push(Op::Keep(xs[i]));
            i += 1;
            j += 1;
        } else if i < xs.len() && (j == ys.len() || lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
            // Removals before additions, as a reader expects to see a replaced line.
            out.push(Op::Del(xs[i]));
            i += 1;
        } else {
            out.push(Op::Add(ys[j]));
            j += 1;
        }
    }
    out.extend(x[x.len() - end..].iter().map(|l| Op::Keep(l)));
    Some(out)
}

fn push_ops(out: &mut String, ops: &[Op<'_>]) {
    for op in ops {
        let (sign, text) = match op {
            Op::Keep(t) => (' ', t),
            Op::Del(t) => ('-', t),
            Op::Add(t) => ('+', t),
        };
        out.push(sign);
        out.push_str(text);
        out.push('\n');
    }
}

/// The file's net change as unified-diff text, and whether every edit could be verified.
///
/// A file the turn created is all additions with real line numbers. Otherwise each edit region is
/// one hunk with its own surrounding lines and the header `@@ @@ edit`: the position of an edit in
/// the file is not recorded, so no line numbers are claimed. None when the file's earlier text is
/// unknown (it was overwritten whole) or a region is over the size budget.
pub fn patch(events: &[Event]) -> Option<(String, bool)> {
    let Settled {
        complete,
        snapshot,
        regions,
    } = settle(events);
    let mut out = String::new();
    match snapshot {
        Some((Some(base), now)) => {
            let ops = edit_script(&base, &now)?;
            if base.is_empty() {
                out.push_str(&format!("@@ -0,0 +1,{} @@\n", lines(&now).len()));
            } else {
                out.push_str("@@ @@ whole file\n");
            }
            push_ops(&mut out, &ops);
        }
        Some((None, _)) => return None,
        None => {
            for r in &regions {
                let ops = edit_script(&r.old, &r.new)?;
                if ops.iter().all(|o| matches!(o, Op::Keep(_))) {
                    continue;
                }
                out.push_str("@@ @@ edit\n");
                push_ops(&mut out, &ops);
            }
        }
    }
    Some((out, complete))
}

/// True when `needle` occurs in `hay` starting and ending on line boundaries.
fn contains_whole_lines(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        let end = i + needle.len();
        (i == 0 || hay.as_bytes()[i - 1] == b'\n')
            && (end == hay.len() || hay.as_bytes()[end] == b'\n' || needle.ends_with('\n'))
    })
}

/// True when a suffix of `a` is a prefix of `b` or the other way round (an unverifiable overlap).
fn overlaps(a: &str, b: &str) -> bool {
    const MIN: usize = 8;
    let max = a.len().min(b.len());
    (MIN..max).any(|k| {
        (a.is_char_boundary(a.len() - k) && b.is_char_boundary(k) && a[a.len() - k..] == b[..k])
            || (b.is_char_boundary(b.len() - k)
                && a.is_char_boundary(k)
                && b[b.len() - k..] == a[..k])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Event {
        Event::Edit {
            old: old.into(),
            new: new.into(),
            all: false,
        }
    }

    #[test]
    fn line_diff_counts_added_and_removed_lines() {
        assert_eq!(line_diff("a\nb\nc\n", "a\nB\nc\nd\n"), Some((2, 1)));
        assert_eq!(line_diff("", "x\ny\n"), Some((2, 0)));
        assert_eq!(line_diff("same\n", "same\n"), Some((0, 0)));
        assert_eq!(
            line_diff(&"x\n".repeat(MAX_LINES + 1), ""),
            None,
            "over budget is not guessed"
        );
    }

    #[test]
    fn repeated_edits_of_the_same_lines_count_once() {
        let n = net(&[
            edit("let a = 1;", "let a = 2;"),
            edit("let a = 2;", "let a = 3;"),
        ]);
        assert_eq!(
            n,
            Net {
                added: 1,
                removed: 1,
                complete: true
            }
        );
    }

    #[test]
    fn a_revert_cancels() {
        let n = net(&[edit("one", "two"), edit("two", "one")]);
        assert_eq!(
            n,
            Net {
                added: 0,
                removed: 0,
                complete: true
            }
        );
    }

    #[test]
    fn creating_then_editing_counts_the_final_contents() {
        let n = net(&[
            Event::Write {
                content: "a\nb\n".into(),
                created: true,
            },
            edit("b", "b\nc"),
        ]);
        assert_eq!(
            n,
            Net {
                added: 3,
                removed: 0,
                complete: true
            }
        );
    }

    #[test]
    fn an_edit_spanning_an_earlier_one_rebuilds_the_original() {
        // First "x" -> "y", then a wider edit over "1\ny\n2" -> "1\nz\n2": net is x -> z.
        let n = net(&[edit("x", "y"), edit("1\ny\n2", "1\nz\n2")]);
        assert_eq!(
            n,
            Net {
                added: 1,
                removed: 1,
                complete: true
            }
        );
    }

    #[test]
    fn independent_edits_add_up() {
        let n = net(&[edit("alpha", "ALPHA"), edit("omega", "omega\nmore")]);
        assert_eq!(
            n,
            Net {
                added: 2,
                removed: 1,
                complete: true
            },
            "ALPHA replaces alpha; more is added under omega"
        );
    }

    #[test]
    fn inconsistent_or_clipped_history_is_marked_incomplete_not_guessed() {
        // An edit whose old text is not in the snapshot.
        let n = net(&[
            Event::Write {
                content: "abc".into(),
                created: true,
            },
            edit("zzz", "y"),
        ]);
        assert!(!n.complete);
        // Edits on an unknown base, then a full write over it: the base is unknown.
        let n = net(&[
            edit("a", "b"),
            Event::Write {
                content: "whole new file".into(),
                created: false,
            },
        ]);
        assert_eq!(
            n,
            Net {
                added: 0,
                removed: 0,
                complete: false
            }
        );
        // A partial overlap with an earlier edit's new text cannot be verified.
        let n = net(&[
            edit("start", "the quick brown fox"),
            edit("brown fox jumps", "x"),
        ]);
        assert!(!n.complete);
    }

    #[test]
    fn overwriting_an_existing_file_is_not_counted_as_creating_it() {
        let n = net(&[Event::Write {
            content: "a\nb\n".into(),
            created: false,
        }]);
        assert_eq!(
            n,
            Net {
                added: 0,
                removed: 0,
                complete: false
            }
        );
    }

    #[test]
    fn replace_all_without_the_file_is_incomplete() {
        let n = net(&[Event::Edit {
            old: "foo".into(),
            new: "bar".into(),
            all: true,
        }]);
        assert!(!n.complete);
    }

    #[test]
    fn an_earlier_edit_found_mid_line_in_a_later_one_is_not_merged() {
        // x -> y, then yes -> no: "y" inside "yes" may be another place entirely.
        let n = net(&[edit("x", "y"), edit("yes", "no")]);
        assert!(!n.complete, "{n:?}");
        // On whole lines the merge is sound: the region "X" became "Y", and the later edit of
        // "a" + newline + "Y" spans it, so the net change is X -> Z.
        let n = net(&[edit("X", "Y"), edit("a\nY", "a\nZ")]);
        assert_eq!(
            n,
            Net {
                added: 1,
                removed: 1,
                complete: true
            }
        );
    }
}
