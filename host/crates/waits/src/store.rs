//! The `agent_waits` table: every wait, open until it fires or is cancelled. Rows survive a
//! restart, so an open wait carries on, and a wait that fired but was not delivered yet is
//! delivered at the next sweep. Deleting the waiting chat deletes its waits.

use crate::model::{Outcome, Target, Wait, MAX_IN_APP, MAX_PER_CHAT};
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;

/// Settled rows are kept this long (for `list`), then removed.
const KEEP_SETTLED_MS: i64 = 24 * 3_600_000;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_waits (
             id          TEXT PRIMARY KEY,
             chat_id     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             kind        TEXT NOT NULL,
             target      TEXT NOT NULL,
             note        TEXT,
             state       TEXT NOT NULL DEFAULT 'open'
                         CHECK (state IN ('open', 'fired', 'cancelled')),
             memory      TEXT NOT NULL DEFAULT '{}',
             outcome     TEXT,
             detail      TEXT,
             created_at  INTEGER NOT NULL,
             expires_at  INTEGER NOT NULL,
             settled_at  INTEGER,
             delivered   INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS agent_waits_chat ON agent_waits(chat_id, state);",
    )
}

/// Why a wait could not be added.
#[derive(Debug, PartialEq, Eq)]
pub enum AddError {
    ChatFull,
    AppFull,
    NoChat,
}

/// Add an open wait, enforcing the per-chat and app-wide caps in the same transaction.
pub fn add(tx: &Transaction<'_>, w: &Wait) -> rusqlite::Result<Result<(), AddError>> {
    let chat_exists: bool = tx
        .query_row("SELECT 1 FROM chats WHERE id = ?1", [&w.chat_id], |_| {
            Ok(true)
        })
        .optional()?
        .unwrap_or(false);
    if !chat_exists {
        return Ok(Err(AddError::NoChat));
    }
    let count = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> rusqlite::Result<usize> {
        tx.query_row(sql, p, |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
    };
    if count(
        "SELECT COUNT(*) FROM agent_waits WHERE chat_id = ?1 AND state = 'open'",
        &[&w.chat_id],
    )? >= MAX_PER_CHAT
    {
        return Ok(Err(AddError::ChatFull));
    }
    if count("SELECT COUNT(*) FROM agent_waits WHERE state = 'open'", &[])? >= MAX_IN_APP {
        return Ok(Err(AddError::AppFull));
    }
    tx.execute(
        "INSERT INTO agent_waits(id, chat_id, kind, target, note, memory, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            w.id,
            w.chat_id,
            w.target.kind(),
            serde_json::to_string(&w.target).unwrap_or_default(),
            w.note,
            w.state.to_string(),
            w.created_at,
            w.expires_at
        ],
    )?;
    Ok(Ok(()))
}

fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Wait>> {
    let target: String = r.get(2)?;
    let memory: String = r.get(4)?;
    let Ok(target) = serde_json::from_str::<Target>(&target) else {
        return Ok(None);
    };
    Ok(Some(Wait {
        id: r.get(0)?,
        chat_id: r.get(1)?,
        target,
        note: r.get(3)?,
        state: serde_json::from_str(&memory).unwrap_or(Value::Null),
        created_at: r.get(5)?,
        expires_at: r.get(6)?,
    }))
}

const COLS: &str = "id, chat_id, target, note, memory, created_at, expires_at";

/// Every open wait, or one chat's.
pub fn open(tx: &Transaction<'_>, chat: Option<&str>) -> rusqlite::Result<Vec<Wait>> {
    let mut st = tx.prepare(&format!(
        "SELECT {COLS} FROM agent_waits WHERE state = 'open' AND (?1 IS NULL OR chat_id = ?1)
         ORDER BY created_at, rowid"
    ))?;
    let rows = st.query_map([chat], read)?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

/// Save the watcher's memory for an open wait.
pub fn save_memory(tx: &Transaction<'_>, id: &str, memory: &Value) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE agent_waits SET memory = ?2 WHERE id = ?1 AND state = 'open'",
        params![id, memory.to_string()],
    )?;
    Ok(())
}

/// Settle an open wait as fired. True only for the call that did it, so a wait never fires twice.
pub fn fire(tx: &Transaction<'_>, id: &str, o: &Outcome, now: i64) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "UPDATE agent_waits SET state = 'fired', outcome = ?2, detail = ?3, settled_at = ?4
         WHERE id = ?1 AND state = 'open'",
        params![id, o.code, o.detail, now],
    )? == 1)
}

/// Cancel an open wait (optionally only if it belongs to `chat`). True if one was cancelled.
pub fn cancel(
    tx: &Transaction<'_>,
    id: &str,
    chat: Option<&str>,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "UPDATE agent_waits SET state = 'cancelled', settled_at = ?3
         WHERE id = ?1 AND state = 'open' AND (?2 IS NULL OR chat_id = ?2)",
        params![id, chat, now],
    )? == 1)
}

/// A fired wait still owed to its chat: the wait, and the outcome to deliver.
pub fn undelivered(tx: &Transaction<'_>) -> rusqlite::Result<Vec<(Wait, Outcome)>> {
    let mut st = tx.prepare(&format!(
        "SELECT {COLS}, outcome, detail FROM agent_waits
         WHERE state = 'fired' AND delivered = 0 ORDER BY settled_at, rowid"
    ))?;
    let rows = st.query_map([], |r| {
        let w = read(r)?;
        let code: String = r.get(7)?;
        let detail: String = r.get::<_, Option<String>>(8)?.unwrap_or_default();
        Ok((w, code, detail))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (w, code, detail) = row?;
        if let Some(w) = w {
            out.push((
                w,
                Outcome {
                    code: code_name(&code),
                    detail,
                },
            ));
        }
    }
    Ok(out)
}

/// The static name of a stored outcome code (an unknown one reads as `changed`).
fn code_name(code: &str) -> &'static str {
    [
        "timeout",
        "finished",
        "stopped",
        "errored",
        "gone",
        "exited",
        "changed",
        "checks_passed",
        "checks_failed",
        "pr_closed",
        "review_changed",
    ]
    .into_iter()
    .find(|c| *c == code)
    .unwrap_or("changed")
}

pub fn mark_delivered(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<()> {
    tx.execute("UPDATE agent_waits SET delivered = 1 WHERE id = ?1", [id])?;
    Ok(())
}

/// Remove settled, delivered rows past the keep window.
pub fn prune(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM agent_waits WHERE state != 'open' AND settled_at < ?1
           AND (state = 'cancelled' OR delivered = 1)",
        [now - KEEP_SETTLED_MS],
    )?;
    Ok(())
}
