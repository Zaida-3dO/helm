//! The record of messages other devices sent here, so one that arrives twice runs once.
//!
//! A remote send carries a fresh id of its own; the first time it is seen it is recorded and
//! delivered, and any repeat is answered as a duplicate. Records are kept for seven days.

use rusqlite::{params, Connection, Transaction};

/// How long a record is kept, in milliseconds.
pub const KEPT_MS: i64 = 7 * 24 * 60 * 60 * 1000;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS remote_sends (
             client_id  TEXT PRIMARY KEY,
             chat_id    TEXT NOT NULL,
             at         INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS remote_sends_at ON remote_sends(at);",
    )
}

/// Record the send; `false` when its id was recorded before.
pub fn claim(
    conn: &Connection,
    client_id: &str,
    chat_id: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    conn.execute("DELETE FROM remote_sends WHERE at < ?1", [now - KEPT_MS])?;
    let n = conn.execute(
        "INSERT OR IGNORE INTO remote_sends(client_id, chat_id, at) VALUES (?1, ?2, ?3)",
        params![client_id, chat_id, now],
    )?;
    Ok(n == 1)
}

/// Forget a record whose send was refused, so the same id may be tried again.
pub fn release(conn: &Connection, client_id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM remote_sends WHERE client_id = ?1", [client_id])?;
    Ok(())
}
