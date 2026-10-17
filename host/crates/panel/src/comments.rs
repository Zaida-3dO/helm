//! Review comments on working-tree diffs (plan PN6).
//!
//! The user selects a run of consecutive lines on one side of a diff in the Changes view and writes
//! a comment. The host re-reads that diff, refuses when it no longer matches the fingerprint the
//! renderer saw ("this diff changed, refresh"), and stores the comment with the quote taken from its
//! own copy of the lines. Bodies are at most 4,000 bytes, ranges at most 120 lines, quotes at most
//! 16,000 bytes, and a chat holds at most 32 pending comments. Adding is idempotent by id. Comments
//! live in a per-chat table and go with the chat. "Add to message" is the renderer's: it puts the
//! numbered list into the draft and never sends.

use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const MAX_BODY: usize = 4000;
pub const MAX_LINES: u32 = 120;
pub const MAX_QUOTE: usize = 16_000;
pub const MAX_PENDING: i64 = 32;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS review_comments (
             id TEXT PRIMARY KEY,
             chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             path TEXT NOT NULL,
             diff_side TEXT NOT NULL,
             side TEXT NOT NULL,
             start_line INTEGER NOT NULL,
             end_line INTEGER NOT NULL,
             quote TEXT NOT NULL,
             body TEXT NOT NULL,
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS review_comments_chat ON review_comments(chat_id, created_at);",
    )
}

/// The fingerprint of a diff the renderer is commenting on.
pub fn fingerprint(patch: &str) -> String {
    hex::encode(&Sha256::digest(patch.as_bytes())[..16])
}

/// The text of lines `start..=end` on one side (`left` = old, `right` = new) of a unified diff.
/// None when the range is not a run of consecutive lines on that side.
pub fn quote(patch: &str, side: &str, start: u32, end: u32) -> Option<String> {
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    let mut out: Vec<(u32, &str)> = Vec::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("@@ -") {
            let mut parts = rest.split(' ');
            let o = parts.next()?.split(',').next()?.parse().ok()?;
            let n = parts
                .next()?
                .trim_start_matches('+')
                .split(',')
                .next()?
                .parse()
                .ok()?;
            (old, new, in_hunk) = (o, n, true);
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (tag, text) = line.split_at(line.len().min(1));
        match tag {
            "+" => {
                if side == "right" {
                    out.push((new, text));
                }
                new += 1;
            }
            "-" => {
                if side == "left" {
                    out.push((old, text));
                }
                old += 1;
            }
            " " => {
                out.push((if side == "left" { old } else { new }, text));
                old += 1;
                new += 1;
            }
            _ => {}
        }
    }
    let picked: Vec<&(u32, &str)> = out
        .iter()
        .filter(|(n, _)| *n >= start && *n <= end)
        .collect();
    if picked.len() as u32 != end - start + 1 || picked.windows(2).any(|w| w[1].0 != w[0].0 + 1) {
        return None;
    }
    Some(
        picked
            .iter()
            .map(|(_, t)| *t)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub path: String,
    pub diff_side: String,
    pub side: String,
    pub start_line: u32,
    pub end_line: u32,
    pub quote: String,
    pub body: String,
    pub created_at: i64,
}

pub struct NewComment<'a> {
    pub id: &'a str,
    pub path: &'a str,
    pub diff_side: &'a str,
    pub side: &'a str,
    pub start: u32,
    pub end: u32,
    pub body: &'a str,
}

/// Checks the shape of a new comment; the diff check happens separately against a fresh read.
pub fn validate(c: &NewComment<'_>) -> Result<(), String> {
    if c.id.is_empty()
        || c.id.len() > 64
        || !c
            .id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return Err("The comment id is not valid.".into());
    }
    if c.body.trim().is_empty() || c.body.len() > MAX_BODY {
        return Err("A comment needs text, at most 4,000 bytes.".into());
    }
    if !matches!(c.side, "left" | "right") || !matches!(c.diff_side, "head" | "staged" | "unstaged")
    {
        return Err("Unknown diff side.".into());
    }
    if c.start == 0 || c.end < c.start || c.end - c.start + 1 > MAX_LINES {
        return Err("A comment covers 1 to 120 consecutive lines.".into());
    }
    crate::changes::check_rel(c.path)
}

pub fn add(
    tx: &Transaction<'_>,
    chat_id: &str,
    c: &NewComment<'_>,
    quote: &str,
    now: i64,
) -> Result<Comment, String> {
    let sql = |e: rusqlite::Error| e.to_string();
    if quote.len() > MAX_QUOTE {
        return Err("The selected lines are too long to quote.".into());
    }
    let existing: Option<String> = tx
        .query_row(
            "SELECT chat_id FROM review_comments WHERE id = ?1",
            [c.id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    match existing {
        Some(owner) if owner == chat_id => {
            return list(tx, chat_id)
                .map_err(sql)?
                .into_iter()
                .find(|x| x.id == c.id)
                .ok_or_else(|| "missing".into())
        }
        Some(_) => return Err("The comment id is already used.".into()),
        None => {}
    }
    let pending: i64 = tx
        .query_row(
            "SELECT count(*) FROM review_comments WHERE chat_id = ?1",
            [chat_id],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if pending >= MAX_PENDING {
        return Err(
            "A chat holds at most 32 pending comments. Add them to a message first.".into(),
        );
    }
    tx.execute(
        "INSERT INTO review_comments (id, chat_id, path, diff_side, side, start_line, end_line, quote, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![c.id, chat_id, c.path, c.diff_side, c.side, c.start, c.end, quote, c.body, now],
    )
    .map_err(sql)?;
    Ok(Comment {
        id: c.id.into(),
        path: c.path.into(),
        diff_side: c.diff_side.into(),
        side: c.side.into(),
        start_line: c.start,
        end_line: c.end,
        quote: quote.into(),
        body: c.body.into(),
        created_at: now,
    })
}

pub fn list(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Vec<Comment>> {
    let mut st = tx.prepare(
        "SELECT id, path, diff_side, side, start_line, end_line, quote, body, created_at
         FROM review_comments WHERE chat_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = st.query_map([chat_id], |r| {
        Ok(Comment {
            id: r.get(0)?,
            path: r.get(1)?,
            diff_side: r.get(2)?,
            side: r.get(3)?,
            start_line: r.get(4)?,
            end_line: r.get(5)?,
            quote: r.get(6)?,
            body: r.get(7)?,
            created_at: r.get(8)?,
        })
    })?;
    rows.collect()
}

pub fn remove(tx: &Transaction<'_>, chat_id: &str, id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM review_comments WHERE chat_id = ?1 AND id = ?2",
        params![chat_id, id],
    )?;
    Ok(())
}

pub fn clear(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM review_comments WHERE chat_id = ?1", [chat_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    const PATCH: &str = "--- a/a\n+++ b/a\n@@ -1,4 +1,4 @@\n one\n-two\n+TWO\n+extra\n three\n";

    #[test]
    fn quotes_come_from_the_named_side_and_must_be_consecutive() {
        assert_eq!(quote(PATCH, "right", 2, 3).as_deref(), Some("TWO\nextra"));
        assert_eq!(quote(PATCH, "left", 2, 2).as_deref(), Some("two"));
        assert_eq!(
            quote(PATCH, "left", 1, 3).as_deref(),
            Some("one\ntwo\nthree")
        );
        assert_eq!(quote(PATCH, "right", 4, 4).as_deref(), Some("three"));
        assert!(quote(PATCH, "right", 4, 9).is_none(), "past the end");
        assert_ne!(
            fingerprint(PATCH),
            fingerprint(&PATCH.replace("TWO", "Two"))
        );
    }

    fn db() -> (tempfile::TempDir, Database) {
        let mut reg = MigrationRegistry::new();
        reg.initializer("review_comments", init);
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        db.write(|tx| {
            tx.execute("INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'p', 'C:/p', 0)", [])?;
            tx.execute("INSERT INTO chats (id, project_id, created_at, updated_at) VALUES ('c', 'p', 0, 0)", [])
        })
        .unwrap();
        (dir, db)
    }

    fn nc<'a>(id: &'a str, body: &'a str) -> NewComment<'a> {
        NewComment {
            id,
            path: "a",
            diff_side: "unstaged",
            side: "right",
            start: 2,
            end: 3,
            body,
        }
    }

    #[test]
    fn limits_idempotence_cap_and_cascade() {
        let (_d, db) = db();
        assert!(validate(&nc("x", "")).is_err());
        assert!(validate(&nc("x", &"b".repeat(MAX_BODY + 1))).is_err());
        assert!(validate(&NewComment {
            start: 1,
            end: 121,
            ..nc("x", "b")
        })
        .is_err());
        assert!(validate(&NewComment {
            path: "../x",
            ..nc("x", "b")
        })
        .is_err());
        assert!(validate(&nc("ok-1", "b")).is_ok());
        db.write(|tx| {
            add(tx, "c", &nc("a1", "first"), "q", 1).unwrap();
            add(tx, "c", &nc("a1", "first"), "q", 2).unwrap();
            assert_eq!(list(tx, "c")?.len(), 1, "adding is idempotent by id");
            for i in 1..MAX_PENDING {
                add(tx, "c", &nc(&format!("n{i}"), "b"), "q", 3 + i).unwrap();
            }
            assert!(
                add(tx, "c", &nc("over", "b"), "q", 99).is_err(),
                "at most 32 pending"
            );
            remove(tx, "c", "a1")?;
            assert_eq!(list(tx, "c")?.len() as i64, MAX_PENDING - 1);
            tx.execute("DELETE FROM chats WHERE id = 'c'", [])?;
            assert!(list(tx, "c")?.is_empty(), "comments go with the chat");
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
    }
}
