//! This PC's own record of what it has read and what the hub has taken.
//!
//! - `memindex_sources`: the registered sources.
//! - `memindex_docs`: one row per doc sent (or about to be): `stamp` is the cheap fingerprint of
//!   the thing it was read from (so an unchanged file is not read again), `hash` is what the hub
//!   is told, and `pushed_hash` is the hash the hub last acknowledged. A row whose two hashes
//!   differ still has to be sent; a restart carries on from there. `parent` names the doc a long
//!   one was split from.
//! - `memindex_settings`: one JSON row.

use crate::model::{Kind, Scope, Settings, Source};
use rusqlite::{params, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS memindex_sources (
             id          TEXT PRIMARY KEY,
             kind        TEXT NOT NULL,
             path        TEXT NOT NULL DEFAULT '',
             person_id   TEXT NOT NULL DEFAULT '',
             scope       TEXT NOT NULL DEFAULT 'me',
             include     TEXT NOT NULL DEFAULT '[]',
             exclude     TEXT NOT NULL DEFAULT '[]',
             enabled     INTEGER NOT NULL DEFAULT 1,
             created_at  INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS memindex_docs (
             source_id    TEXT NOT NULL,
             key          TEXT NOT NULL,
             parent       TEXT NOT NULL,
             stamp        TEXT NOT NULL,
             hash         TEXT NOT NULL,
             pushed_hash  TEXT,
             updated_at   INTEGER NOT NULL,
             PRIMARY KEY (source_id, key)
         );
         CREATE INDEX IF NOT EXISTS memindex_docs_parent ON memindex_docs(source_id, parent);
         CREATE TABLE IF NOT EXISTS memindex_settings (
             id    INTEGER PRIMARY KEY CHECK (id = 1),
             json  TEXT NOT NULL
         );",
    )
}

pub fn load_settings(tx: &Transaction<'_>) -> rusqlite::Result<Settings> {
    let raw: Option<String> = tx
        .query_row("SELECT json FROM memindex_settings WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(raw
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default())
}

/// Whether any settings were ever saved on this install.
pub fn settings_saved(tx: &Transaction<'_>) -> rusqlite::Result<bool> {
    tx.query_row("SELECT 1 FROM memindex_settings WHERE id = 1", [], |_| {
        Ok(())
    })
    .optional()
    .map(|r| r.is_some())
}

pub fn save_settings(tx: &Transaction<'_>, s: &Settings) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO memindex_settings (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [serde_json::to_string(s).expect("settings serialise")],
    )
    .map(drop)
}

fn list_of(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn source_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Source> {
    let kind: String = r.get(1)?;
    let scope: String = r.get(4)?;
    Ok(Source {
        id: r.get(0)?,
        kind: Kind::parse(&kind).unwrap_or(Kind::Folder),
        path: r.get(2)?,
        person_id: r.get(3)?,
        scope: Scope::parse(&scope).unwrap_or(Scope::Me),
        include: list_of(&r.get::<_, String>(5)?),
        exclude: list_of(&r.get::<_, String>(6)?),
        enabled: r.get(7)?,
        created_at: r.get(8)?,
    })
}

const SOURCE_COLS: &str = "id, kind, path, person_id, scope, include, exclude, enabled, created_at";

pub fn sources(tx: &Transaction<'_>) -> rusqlite::Result<Vec<Source>> {
    let mut st = tx.prepare(&format!(
        "SELECT {SOURCE_COLS} FROM memindex_sources ORDER BY created_at, id"
    ))?;
    let rows = st.query_map([], source_row)?;
    rows.collect()
}

pub fn source(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<Source>> {
    tx.query_row(
        &format!("SELECT {SOURCE_COLS} FROM memindex_sources WHERE id = ?1"),
        [id],
        source_row,
    )
    .optional()
}

pub fn save_source(tx: &Transaction<'_>, s: &Source) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO memindex_sources (id, kind, path, person_id, scope, include, exclude, enabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET path = excluded.path, person_id = excluded.person_id,
           scope = excluded.scope, include = excluded.include, exclude = excluded.exclude,
           enabled = excluded.enabled",
        params![
            s.id,
            s.kind.as_str(),
            s.path,
            s.person_id,
            s.scope.as_str(),
            serde_json::to_string(&s.include).expect("a list serialises"),
            serde_json::to_string(&s.exclude).expect("a list serialises"),
            s.enabled,
            s.created_at
        ],
    )
    .map(drop)
}

/// Remove a source and its rows. The hub's copy is deleted by the caller first.
pub fn remove_source(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM memindex_docs WHERE source_id = ?1", [id])?;
    tx.execute("DELETE FROM memindex_sources WHERE id = ?1", [id])
        .map(drop)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocRow {
    pub parent: String,
    pub stamp: String,
    pub hash: String,
    pub pushed_hash: Option<String>,
}

impl DocRow {
    pub fn is_pushed(&self) -> bool {
        self.pushed_hash.as_deref() == Some(self.hash.as_str())
    }
}

pub fn docs(tx: &Transaction<'_>, source: &str) -> rusqlite::Result<HashMap<String, DocRow>> {
    let mut st = tx.prepare(
        "SELECT key, parent, stamp, hash, pushed_hash FROM memindex_docs WHERE source_id = ?1",
    )?;
    let rows = st.query_map([source], |r| {
        Ok((
            r.get::<_, String>(0)?,
            DocRow {
                parent: r.get(1)?,
                stamp: r.get(2)?,
                hash: r.get(3)?,
                pushed_hash: r.get(4)?,
            },
        ))
    })?;
    rows.collect()
}

/// Record a doc as read. Its push state is kept when the hash is the same.
pub fn put_doc(
    tx: &Transaction<'_>,
    source: &str,
    key: &str,
    parent: &str,
    stamp: &str,
    hash: &str,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO memindex_docs (source_id, key, parent, stamp, hash, pushed_hash, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)
         ON CONFLICT(source_id, key) DO UPDATE SET
           parent = excluded.parent, stamp = excluded.stamp, updated_at = excluded.updated_at,
           pushed_hash = CASE WHEN hash = excluded.hash THEN pushed_hash ELSE NULL END,
           hash = excluded.hash",
        params![source, key, parent, stamp, hash, now],
    )
    .map(drop)
}

/// Update only the fingerprint (the file changed on disk, its words did not).
pub fn touch_stamp(
    tx: &Transaction<'_>,
    source: &str,
    parent: &str,
    stamp: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE memindex_docs SET stamp = ?3 WHERE source_id = ?1 AND parent = ?2",
        params![source, parent, stamp],
    )
    .map(drop)
}

pub fn mark_pushed(
    tx: &Transaction<'_>,
    source: &str,
    key: &str,
    hash: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE memindex_docs SET pushed_hash = ?3 WHERE source_id = ?1 AND key = ?2 AND hash = ?3",
        params![source, key, hash],
    )
    .map(drop)
}

/// Make a doc be sent again.
pub fn forget_pushed(tx: &Transaction<'_>, source: &str, key: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE memindex_docs SET pushed_hash = NULL WHERE source_id = ?1 AND key = ?2",
        params![source, key],
    )
    .map(drop)
}

pub fn delete_docs(tx: &Transaction<'_>, source: &str, keys: &[String]) -> rusqlite::Result<()> {
    for k in keys {
        tx.execute(
            "DELETE FROM memindex_docs WHERE source_id = ?1 AND key = ?2",
            params![source, k],
        )?;
    }
    Ok(())
}

/// The digest the hub computes of what it holds: sha256 of the sorted `key:hash` lines, joined by
/// newlines. Here it covers the docs the hub has acknowledged.
pub fn digest(docs: &HashMap<String, DocRow>) -> (usize, String) {
    let mut lines: Vec<(&String, &String)> = docs
        .iter()
        .filter(|(_, d)| d.is_pushed())
        .map(|(k, d)| (k, &d.hash))
        .collect();
    lines.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let n = lines.len();
    let text: Vec<String> = lines.iter().map(|(k, h)| format!("{k}:{h}")).collect();
    (
        n,
        format!("{:x}", Sha256::digest(text.join("\n").as_bytes())),
    )
}
