//! The shared shape of a source reader: a doc made of chunks, plus the helpers the readers
//! share (the chunk size caps, paragraph packing, hashing, and the small amount of calendar
//! math needed to put a human date into a header line).
//!
//! Every reader is a pure function over text that is already on disk: it never opens a file,
//! never talks to the hub, and never looks at the clock. The caller (the indexer) decides
//! which files are sources at all, keeps the cursors, and pushes the result.

pub mod markdown;

use sha2::{Digest, Sha256};

/// Hard cap on one chunk's text, in characters (about 400–500 tokens).
pub const MAX_CHUNK_CHARS: usize = 2000;
/// Aim for chunks at least around this long; short neighbours merge when the cap allows.
pub const TARGET_CHUNK_CHARS: usize = 1400;
/// How much of a turn's ask is repeated at the head of a long reply's split pieces.
pub const ASK_PREFIX_CHARS: usize = 300;

/// One searchable piece of a doc. `heading` is the heading path the piece sits under (empty
/// when it does not), `at` is the instant the content belongs to in Unix milliseconds, and
/// `anchor` names the spot inside the source a reader can jump back to (a message id, a
/// record id), when the source has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkOut {
    pub ord: usize,
    pub heading: String,
    pub text: String,
    pub at: i64,
    pub anchor: Option<String>,
}

/// One searchable unit of a source: a file, a chat, a session, a week of logs. `key` is the
/// source's own stable name for it (a relative path, a chat id, a session id); `hash` covers
/// everything searchable about it, so an unchanged doc is recognised without re-sending it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocOut {
    pub key: String,
    pub hash: String,
    pub title: String,
    pub locator: String,
    pub occurred_from: i64,
    pub occurred_to: i64,
    pub chunks: Vec<ChunkOut>,
}

/// Finish a doc: number the chunks and hash the content. The hash covers the title, every
/// chunk's heading, text, instant and anchor, so any change a searcher could notice — words,
/// a moved time, a different heading path — moves it, and nothing else does.
pub(crate) fn finish_doc(
    key: String,
    title: String,
    locator: String,
    occurred_from: i64,
    occurred_to: i64,
    mut chunks: Vec<ChunkOut>,
) -> DocOut {
    for (i, c) in chunks.iter_mut().enumerate() {
        c.ord = i;
    }
    let mut h = Sha256::new();
    h.update(title.as_bytes());
    for c in &chunks {
        h.update([1]);
        h.update(c.heading.as_bytes());
        h.update([1]);
        h.update(c.text.as_bytes());
        h.update([1]);
        h.update(c.at.to_string().as_bytes());
        h.update([1]);
        h.update(c.anchor.as_deref().unwrap_or("").as_bytes());
    }
    let hash = format!("{:x}", h.finalize());
    DocOut {
        key,
        hash,
        title,
        locator,
        occurred_from,
        occurred_to,
        chunks,
    }
}

/// Split on blank lines, order kept. (A line of only spaces counts as blank, the same way
/// markdown readers treat it.)
pub(crate) fn paragraphs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            if let Some(s) = start.take() {
                out.push(text[s..pos].trim_end_matches(['\n', ' ', '\t', '\r']));
            }
        } else if start.is_none() {
            start = Some(pos);
        }
        pos += line.len();
    }
    if let Some(s) = start {
        out.push(text[s..].trim_end_matches(['\n', ' ', '\t', '\r']));
    }
    out
}

/// Cut a string into pieces of at most `cap` characters, breaking at word edges where a word
/// allows it; a single word longer than the cap is cut mid-word.
pub(crate) fn wrap_capped(s: &str, cap: usize) -> Vec<String> {
    let cap = cap.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in s.split_whitespace() {
        let wl = word.chars().count();
        if wl > cap {
            if cur_len > 0 {
                out.push(std::mem::take(&mut cur));
            }
            let mut it = word.chars().peekable();
            loop {
                let piece: String = it.by_ref().take(cap).collect();
                if it.peek().is_none() {
                    cur = piece;
                    cur_len = cur.chars().count();
                    break;
                }
                out.push(piece);
            }
            continue;
        }
        let join = if cur_len > 0 { 1 } else { 0 };
        if cur_len + join + wl > cap {
            if cur_len > 0 {
                out.push(std::mem::take(&mut cur));
            }
            cur = word.to_string();
            cur_len = wl;
        } else {
            if cur_len > 0 {
                cur.push(' ');
                cur_len += 1;
            }
            cur.push_str(word);
            cur_len += wl;
        }
    }
    if cur_len > 0 {
        out.push(cur);
    }
    out
}

/// Days-from-civil and civil-from-days (the public-domain calendar math), enough to print
/// and read `YYYY-MM-DD` at whole-day boundaries. Dates read this way are UTC.
const DAY_MS: i64 = 86_400_000;

pub(crate) fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = (y - if m <= 2 { 1 } else { 0 }) as i64;
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (m + 9) % 12;
    let doy = (153 * mp as u64 + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

pub(crate) fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m, d)
}

/// `YYYY-MM-DD` for an instant, as the UTC calendar sees it.
pub(crate) fn fmt_date(ms: i64) -> String {
    let day = if ms >= 0 { ms } else { ms - (DAY_MS - 1) } / DAY_MS;
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DD` at UTC midnight, or `None` if the ten characters are not a real date.
pub(crate) fn parse_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if !b.iter().take(10).enumerate().all(|(i, c)| {
        if i == 4 || i == 7 {
            *c == b'-'
        } else {
            c.is_ascii_digit()
        }
    }) {
        return None;
    }
    let y: i32 = s[0..4].parse().ok()?;
    let m: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * DAY_MS)
}

/// The first `YYYY-MM-DD` inside a string (a front-matter value, a file name), if any.
pub(crate) fn find_date(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    for i in 0..bytes.len().saturating_sub(9) {
        if bytes[i].is_ascii_digit() && bytes[i + 4] == b'-' && bytes[i + 7] == b'-' {
            if let Some(ms) = parse_date(&s[i..]) {
                return Some(ms);
            }
        }
    }
    None
}

/// File stem of a path, for a fallback title. Handles `/` and `\` separated paths.
pub(crate) fn file_stem(path: &str) -> String {
    let tail = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match tail.rfind('.') {
        Some(dot) if dot > 0 => tail[..dot].to_string(),
        _ => tail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_and_packing() {
        // wrap_capped: word edges, a long word, and the cap itself.
        assert_eq!(
            wrap_capped("one two three four", 9),
            ["one two", "three", "four"]
        );
        assert_eq!(
            wrap_capped("one two three four", 10),
            ["one two", "three four"]
        );
        assert_eq!(wrap_capped("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_capped("", 5), Vec::<String>::new());
        assert_eq!(wrap_capped("ab cd", 1), ["a", "b", "c", "d"]);
        // paragraphs: blank runs and space-only lines collapse.
        assert_eq!(paragraphs("a\n\nb\n \nc"), ["a", "b", "c"]);
        assert_eq!(paragraphs("  \n\n"), Vec::<&str>::new());
    }

    #[test]
    fn date_helpers() {
        assert_eq!(fmt_date(parse_date("2026-11-15").unwrap()), "2026-11-15");
        assert_eq!(fmt_date(0), "1970-01-01");
        assert_eq!(fmt_date(-1), "1969-12-31");
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("2026-00-10"), None);
        assert_eq!(
            parse_date("2026-02-30"),
            Some(days_from_civil(2026, 2, 30) * DAY_MS)
        ); // no month-length claim
        assert_eq!(find_date("notes 2025-04-07 todo"), parse_date("2025-04-07"));
        assert_eq!(find_date("no date here"), None);
        assert_eq!(file_stem("C:\\a\\b\\my-note.md"), "my-note");
        assert_eq!(file_stem("dir/plain"), "plain");
    }
}
