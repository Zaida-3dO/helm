//! The governor's one table: its settings record (own table, like helm-afk's).

use crate::bands::{read_lenient, GovernorSettings};
use rusqlite::{OptionalExtension, Transaction};

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS governor_settings (
             id     INTEGER PRIMARY KEY CHECK (id = 1),
             value  TEXT NOT NULL
         );",
    )
}

pub fn load(tx: &Transaction<'_>) -> rusqlite::Result<GovernorSettings> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM governor_settings WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .map(|v| read_lenient(&v))
        .unwrap_or_default())
}

pub fn save(tx: &Transaction<'_>, s: &GovernorSettings) -> rusqlite::Result<()> {
    let v = serde_json::to_string(s).expect("settings serialise");
    tx.execute(
        "INSERT INTO governor_settings (id, value) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET value = excluded.value",
        [v],
    )?;
    Ok(())
}
