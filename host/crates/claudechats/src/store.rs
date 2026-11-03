//! The index: one row per record file, plus full text over its user and assistant messages.
//!
//! The tables are refreshed when the list or a search runs, not on a timer. A file whose size and
//! modification time match what the row already holds is not re-parsed, so a refresh over a few
//! hundred unchanged records is just a folder walk. Ranking follows the chat search: text hits
//! score bm25 softened by age and are one per chat; chats whose title contains the whole query
//! come first, newest first (there is no pin, run or open signal on an outside record to boost
//! by).

use crate::parse::{self, Decoded, Role, Transcript};
use crate::query::{fts_expression, words};
use crate::types::*;
use helm_core::db::Database;
use rusqlite::{params, OptionalExtension, Transaction};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Only the newest this many records are indexed.
pub const MAX_FILES: usize = 1_000;
/// Record files bigger than this are skipped.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Text hits read before the reduction to one per chat.
const MAX_TEXT_HITS: i64 = 1_000;
/// Text results kept, and title matches listed.
const KEEP_TEXT: usize = 60;
const KEEP_TITLES: usize = 30;
/// The view returns at most this much line text, newest lines first.
const VIEW_BUDGET_BYTES: usize = 256 * 1024;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS claude_chat_records (
             session_id    TEXT PRIMARY KEY,
             path          TEXT NOT NULL UNIQUE,
             folder        TEXT NOT NULL DEFAULT '',
             title         TEXT NOT NULL DEFAULT '',
             updated_at    INTEGER NOT NULL,
             message_count INTEGER NOT NULL,
             size          INTEGER NOT NULL,
             mtime         INTEGER NOT NULL
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS claude_chat_search USING fts5(
             text, session_id UNINDEXED, tokenize = 'unicode61'
         );",
    )
}

struct Candidate {
    path: PathBuf,
    size: u64,
    mtime: i64,
}

/// Walk `<root>/*/*.jsonl`, newest first, capped at [`MAX_FILES`], skipping oversized files.
/// Ok(false) when the root itself is missing.
fn walk(root: &Path) -> rusqlite::Result<(Vec<Candidate>, bool)> {
    // Any failure to open the root (missing or unreadable) counts as "not found": the caller
    // keeps the rows it has and reports the source missing, and nothing is dropped.
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(_) => return Ok((Vec::new(), false)),
    };
    let mut out: Vec<Candidate> = Vec::new();
    for entry in rd.flatten() {
        let sub = match entry.file_type() {
            Ok(ft) if ft.is_dir() => std::fs::read_dir(entry.path()),
            _ => continue,
        };
        for f in sub.ok().into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = f.metadata() else { continue };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            out.push(Candidate {
                path: p,
                size: meta.len(),
                mtime,
            });
        }
    }
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
    out.truncate(MAX_FILES);
    Ok((out, true))
}

/// Re-index as needed. Ok(source_missing).
pub fn refresh(db: &Database, root: &Path) -> rusqlite::Result<bool> {
    let (candidates, found) = walk(root)?;
    if !found {
        return Ok(true);
    }
    // Parse outside the transaction: a refresh is mostly disk.
    let stored: HashMap<String, (i64, i64)> = db.snapshot(|tx| {
        let mut st = tx.prepare("SELECT path, size, mtime FROM claude_chat_records")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let mut m = HashMap::new();
        for r in rows {
            let (p, size, mtime) = r?;
            m.insert(p, (size, mtime));
        }
        Ok::<_, rusqlite::Error>(m)
    })?;
    let mut fresh: Vec<(Candidate, Transcript)> = Vec::new();
    let mut alive: HashSet<String> = HashSet::new();
    for c in candidates {
        let key = c.path.to_string_lossy().into_owned();
        alive.insert(key.clone());
        if stored.get(&key) == Some(&(c.size as i64, c.mtime)) {
            continue;
        }
        if let Ok(t) = parse::parse_file(&c.path) {
            fresh.push((c, t));
        }
    }
    db.write(|tx| -> rusqlite::Result<()> {
        for (c, t) in &fresh {
            let old_session: Option<String> = tx
                .query_row(
                    "SELECT session_id FROM claude_chat_records WHERE path = ?1",
                    [c.path.to_string_lossy().as_ref()],
                    |r| r.get(0),
                )
                .optional()?;
            for sid in [old_session.as_deref(), Some(t.session_id.as_str())]
                .into_iter()
                .flatten()
                .collect::<HashSet<_>>()
            {
                tx.execute("DELETE FROM claude_chat_search WHERE session_id = ?1", [sid])?;
            }
            let updated = t.last_ts.max(c.mtime);
            tx.execute(
                "INSERT INTO claude_chat_records(session_id, path, folder, title, updated_at, message_count, size, mtime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(session_id) DO UPDATE SET
                     path = excluded.path, folder = excluded.folder, title = excluded.title,
                     updated_at = excluded.updated_at, message_count = excluded.message_count,
                     size = excluded.size, mtime = excluded.mtime",
                params![
                    t.session_id,
                    c.path.to_string_lossy().as_ref(),
                    t.cwd,
                    t.title,
                    updated,
                    t.message_count,
                    c.size as i64,
                    c.mtime
                ],
            )?;
            // The newest messages of a long chat are the ones search cares about.
            let msgs: Vec<&Decoded> = t
                .lines
                .iter()
                .filter(|l| matches!(l, Decoded::Message { .. }))
                .rev()
                .take(512)
                .collect();
            for m in msgs {
                let Decoded::Message { text, .. } = m else {
                    unreachable!("filtered to messages");
                };
                tx.execute(
                    "INSERT INTO claude_chat_search(text, session_id) VALUES (?1, ?2)",
                    params![text, t.session_id],
                )?;
            }
        }
        // Files that are gone lose their rows.
        let stale: Vec<String> = {
            let mut st = tx.prepare("SELECT session_id, path FROM claude_chat_records")?;
            let rows = st.query_map([], |r| -> rusqlite::Result<(String, String)> {
                Ok((r.get(0)?, r.get(1)?))
            })?;
            rows.filter_map(|r| r.ok())
                .filter(|(_, p)| !alive.contains(p))
                .map(|(s, _)| s)
                .collect()
        };
        for s in &stale {
            tx.execute("DELETE FROM claude_chat_records WHERE session_id = ?1", [s])?;
            tx.execute("DELETE FROM claude_chat_search WHERE session_id = ?1", [s])?;
        }
        Ok(())
    })?;
    Ok(false)
}

const ROW_COLUMNS: &str = "session_id, title, folder, updated_at, message_count";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatRow> {
    Ok(ChatRow {
        session_id: r.get(0)?,
        title: r.get(1)?,
        folder: r.get(2)?,
        updated_at: r.get(3)?,
        message_count: r.get(4)?,
    })
}

/// The indexed records, newest first.
pub fn list(db: &Database, limit: i64) -> rusqlite::Result<Vec<ChatRow>> {
    db.snapshot(|tx| {
        let mut st = tx.prepare(&format!(
            "SELECT {ROW_COLUMNS} FROM claude_chat_records ORDER BY updated_at DESC, session_id LIMIT ?1"
        ))?;
        let rows = st.query_map([limit], row)?;
        rows.collect()
    })
}

/// A record's file path, folder, title and activity time, by session id.
pub fn record(
    db: &Database,
    session_id: &str,
) -> rusqlite::Result<Option<(String, String, String, i64)>> {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT path, folder, title, updated_at FROM claude_chat_records WHERE session_id = ?1",
            [session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
    })
}

struct TextHit {
    row: ChatRow,
    text: String,
    score: f64,
}

/// Title matches first (case-insensitive substring of the whole query, newest first), then the
/// best-scoring record of each text hit with a snippet around the first matching word.
pub fn search(db: &Database, query: &str, now_ms: i64) -> rusqlite::Result<SearchResults> {
    let ws = words(query);
    let fts = fts_expression(&ws);
    let needle = query.to_lowercase();
    let (titles, texts) = db.snapshot(|tx| {
        let mut st = tx.prepare(&format!(
            "SELECT {ROW_COLUMNS} FROM claude_chat_records ORDER BY updated_at DESC, session_id"
        ))?;
        let mut titles: Vec<ChatRow> = Vec::new();
        for r in st.query_map([], row)? {
            let r = r?;
            if r.title.to_lowercase().contains(&needle) {
                titles.push(r);
                if titles.len() >= 200 {
                    break;
                }
            }
        }
        let mut best: HashMap<String, TextHit> = HashMap::new();
        if let Some(f) = &fts {
            let mut st = tx.prepare(
                "SELECT s.session_id, s.text, bm25(claude_chat_search),
                        r.title, r.folder, r.updated_at, r.message_count
                 FROM claude_chat_search s JOIN claude_chat_records r ON r.session_id = s.session_id
                 WHERE claude_chat_search MATCH ?1 ORDER BY bm25(claude_chat_search) LIMIT ?2",
            )?;
            let rows = st.query_map(params![f, MAX_TEXT_HITS], |r| {
                let sid: String = r.get(0)?;
                Ok(TextHit {
                    row: ChatRow {
                        session_id: sid.clone(),
                        title: r.get(3)?,
                        folder: r.get(4)?,
                        updated_at: r.get(5)?,
                        message_count: r.get(6)?,
                    },
                    text: r.get(1)?,
                    score: age_score(-r.get::<_, f64>(2)?, r.get(5)?, now_ms),
                })
            })?;
            for h in rows {
                let h = h?;
                match best.get(&h.row.session_id) {
                    Some(cur) if cur.score >= h.score => {}
                    _ => {
                        best.insert(h.row.session_id.clone(), h);
                    }
                }
            }
        }
        Ok::<_, rusqlite::Error>((titles, best))
    })?;
    let mut seen: HashSet<&str> = HashSet::new();
    let mut hits: Vec<SearchHit> = Vec::new();
    for t in titles.iter().take(KEEP_TITLES) {
        seen.insert(t.session_id.as_str());
        let text = texts.get(&t.session_id);
        hits.push(SearchHit {
            session_id: t.session_id.clone(),
            title: t.title.clone(),
            folder: t.folder.clone(),
            updated_at: t.updated_at,
            message_count: t.message_count,
            snippet: text.map(|h| snippet(&h.text, &ws)).unwrap_or_default(),
            title_only: text.is_none(),
        });
    }
    let mut rest: Vec<&TextHit> = texts
        .values()
        .filter(|h| !seen.contains(h.row.session_id.as_str()))
        .collect();
    rest.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.row.session_id.cmp(&b.row.session_id))
    });
    for h in rest.into_iter().take(KEEP_TEXT) {
        hits.push(SearchHit {
            session_id: h.row.session_id.clone(),
            title: h.row.title.clone(),
            folder: h.row.folder.clone(),
            updated_at: h.row.updated_at,
            message_count: h.row.message_count,
            snippet: snippet(&h.text, &ws),
            title_only: false,
        });
    }
    Ok(SearchResults { hits })
}

/// bm25 softened by the record's age, as the chat search softens it.
fn age_score(relevance: f64, updated_at_ms: i64, now_ms: i64) -> f64 {
    const WEEK_MS: f64 = 7.0 * 24.0 * 60.0 * 60.0 * 1000.0;
    let age = (now_ms - updated_at_ms).max(0) as f64;
    relevance / (1.0 + 0.15 * (1.0 + age / WEEK_MS).ln())
}

const BEFORE: usize = 60;
const AFTER: usize = 100;
const ELLIPSIS: char = '\u{2026}';

/// About 160 characters around the first occurrence of any of `words` (a prefix match,
/// case-insensitive), whitespace collapsed, with an ellipsis where it was cut.
fn snippet(text: &str, words: &[String]) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    let lower: Vec<char> = flat.to_lowercase().chars().collect();
    let at = if lower.len() == chars.len() {
        words
            .iter()
            .filter_map(|w| find(&lower, &w.to_lowercase().chars().collect::<Vec<_>>()))
            .min()
    } else {
        None
    }
    .unwrap_or(0);
    let start = at.saturating_sub(BEFORE);
    let end = (at + AFTER).min(chars.len());
    let mut out: String = chars[start..end].iter().collect();
    if start > 0 {
        out.insert(0, ELLIPSIS);
    }
    if end < chars.len() {
        out.push(ELLIPSIS);
    }
    out
}

fn find(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// The read-only view: view lines up to the budget, keeping the newest when it cuts. The caller
/// marks that a cut happened.
pub fn view(tr: &Transcript) -> (Vec<Line>, bool) {
    let mut budget: i64 = VIEW_BUDGET_BYTES as i64;
    let mut kept: Vec<Line> = Vec::new();
    let mut truncated = false;
    for d in tr.lines.iter().rev() {
        let line = match d {
            Decoded::Message {
                role,
                text,
                timestamp,
            } => Line {
                kind: LineKind::Message,
                role: Some(match role {
                    Role::User => LineRole::User,
                    Role::Assistant => LineRole::Assistant,
                }),
                text: text.clone(),
                tool_name: None,
                timestamp: *timestamp,
            },
            Decoded::Tool {
                name,
                args,
                result,
                timestamp,
                ..
            } => Line {
                kind: LineKind::Tool,
                role: None,
                text: if result.is_empty() {
                    args.clone()
                } else {
                    result.clone()
                },
                tool_name: Some(name.clone()),
                timestamp: *timestamp,
            },
        };
        let cost = line.text.len() as i64;
        if budget - cost < 0 {
            truncated = true;
            break;
        }
        budget -= cost;
        kept.push(line);
    }
    kept.reverse();
    (kept, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = helm_core::db::MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        crate::register_migrations(&mut reg);
        let db = Database::open(&dir.path().join("t.db"), &reg).unwrap();
        (dir, db)
    }

    /// `<root>/<folder>/<name>.jsonl`
    pub(crate) fn fixture(root: &Path, folder: &str, name: &str, lines: &[&str]) -> PathBuf {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{name}.jsonl"));
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        p
    }

    /// The instant NOW names; every fixture record carries it, so `updated_at` is NOW wherever a
    /// record has text (and file mtimes, always older than 2033, never decide ordering).
    pub(crate) const NOW: i64 = 2_000_000_000_000;

    fn user(sid: &str, text: &str) -> String {
        format!(
            "{{\"sessionId\":\"{sid}\",\"cwd\":\"C:/proj\",\"timestamp\":\"2033-05-18T03:33:20.000Z\",\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}"
        )
    }
    fn assistant(sid: &str, text: &str) -> String {
        format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}"
        )
    }

    #[test]
    fn a_refresh_indexes_new_files_skips_unchanged_and_drops_deleted_ones() {
        let root = tempfile::tempdir().unwrap();
        let (_d, database) = db();
        let a = fixture(
            root.path(),
            "proj-a",
            "11111111-0000-0000-0000-00000000000a",
            &[
                &user("s-a", "first prompt about walrus"),
                &assistant("s-a", "answer one"),
            ],
        );
        assert!(!refresh(&database, root.path()).unwrap());
        let chats = list(&database, 200).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].session_id, "s-a");
        assert_eq!(chats[0].title, "first prompt about walrus");
        assert_eq!(chats[0].folder, "C:/proj");
        assert_eq!(chats[0].message_count, 2);
        assert_eq!(
            chats[0].updated_at, NOW,
            "the records' own time is the activity"
        );

        // Re-refreshing an unchanged folder changes nothing (no re-parse, no lost row)...
        refresh(&database, root.path()).unwrap();
        assert_eq!(list(&database, 200).unwrap().len(), 1);
        // ...and a deleted file loses its row and its text.
        std::fs::remove_file(&a).unwrap();
        refresh(&database, root.path()).unwrap();
        assert!(list(&database, 200).unwrap().is_empty());
        assert!(record(&database, "s-a").unwrap().is_none());
        assert!(search(&database, "walrus", NOW).unwrap().hits.is_empty());
    }

    #[test]
    fn a_missing_root_reports_source_missing_and_keeps_the_rows() {
        let root = tempfile::tempdir().unwrap();
        let (_d, database) = db();
        fixture(
            root.path(),
            "p",
            "22222222-0000-0000-0000-00000000000b",
            &[&user("s-b", "one")],
        );
        refresh(&database, root.path()).unwrap();
        assert_eq!(list(&database, 200).unwrap().len(), 1);
        let gone = root.path().join("nope");
        assert!(refresh(&database, &gone).unwrap(), "source missing");
        assert_eq!(list(&database, 200).unwrap().len(), 1);
    }

    #[test]
    fn search_is_titles_first_prefix_anded_one_per_chat_with_a_snippet() {
        let root = tempfile::tempdir().unwrap();
        let (_d, database) = db();
        fixture(
            root.path(),
            "one",
            "33333333-0000-0000-0000-00000000000c",
            &[
                &user("s-zebra", "nothing here"),
                &assistant(
                    "s-zebra",
                    &format!("{} zebra crossing ahead", "pad ".repeat(50)),
                ),
                &assistant("s-zebra", "second zebra mention"),
            ],
        );
        fixture(
            root.path(),
            "two",
            "33333333-0000-0000-0000-00000000000d",
            &[
                &user("s-plans", "unrelated"),
                "{\"type\":\"summary\",\"summary\":\"Zebra plans\"}",
            ],
        );
        refresh(&database, root.path()).unwrap();

        let r = search(&database, "zebra", NOW + 1).unwrap();
        assert_eq!(
            r.hits.iter().map(|h| h.title.as_str()).collect::<Vec<_>>(),
            ["Zebra plans", "nothing here"],
            "title match first, then the text hit"
        );
        assert!(r.hits[0].title_only && r.hits[0].snippet.is_empty());
        assert!(!r.hits[1].title_only);
        assert!(
            r.hits[1].snippet.contains("second zebra"),
            "the best-scoring message of the chat shows (bm25 favours the shorter one): {}",
            r.hits[1].snippet
        );
        assert!(r.hits[1].snippet.chars().count() <= 162);
        // Both zebra messages of one chat collapse to one hit.
        assert_eq!(r.hits.len(), 2, "one hit per record");
        // Prefix terms, all required: "zebr cro" hits only via text.
        let r = search(&database, "zebr cro", NOW + 1).unwrap();
        assert_eq!(
            r.hits.iter().map(|h| h.title.as_str()).collect::<Vec<_>>(),
            ["nothing here"],
            "prefixes match; a title needs the whole query as a substring"
        );
        assert!(search(&database, "zebra unlikelyword", NOW + 1)
            .unwrap()
            .hits
            .is_empty());
    }

    #[test]
    fn the_view_keeps_the_newest_lines_says_when_it_cut_and_shows_tool_results() {
        let texts: Vec<String> = (0..400)
            .map(|i| format!("message {i} {}", "x".repeat(1000)))
            .collect();
        let lines: Vec<String> = texts.iter().map(|t| user("s-v", t)).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let dir = tempfile::tempdir().unwrap();
        let p = fixture(
            dir.path(),
            "v",
            "44444444-0000-0000-0000-00000000000e",
            &lines,
        );
        let t = parse::parse_file(&p).unwrap();
        let (got, truncated) = view(&t);
        assert!(truncated);
        assert!(!got.is_empty() && got.len() < 400);
        assert_eq!(
            got.last().unwrap().text,
            texts[399],
            "the newest line is kept"
        );
        assert_eq!(
            got.first().unwrap().text,
            texts[400 - got.len()],
            "older lines dropped, contiguous newest kept"
        );

        // A tool line shows its result under the tool's name.
        let dir2 = tempfile::tempdir().unwrap();
        let p2 = fixture(
            dir2.path(),
            "t",
            "44444444-0000-0000-0000-00000000000f",
            &[
                &user("s-t", "go"),
                "{\"sessionId\":\"s-t\",\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tu1\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}]}}",
                "{\"sessionId\":\"s-t\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tu1\",\"content\":\"a.txt\"}]}}",
            ],
        );
        let t2 = parse::parse_file(&p2).unwrap();
        let (v2, tr2) = view(&t2);
        assert!(!tr2);
        assert_eq!(v2.len(), 2, "the result rides on the tool's own line");
        assert_eq!(
            (v2[1].kind, v2[1].tool_name.as_deref(), v2[1].text.as_str()),
            (LineKind::Tool, Some("Bash"), "a.txt")
        );
        assert_eq!(
            (v2[0].kind, v2[0].role, v2[0].timestamp),
            (LineKind::Message, Some(LineRole::User), NOW)
        );
    }
}
