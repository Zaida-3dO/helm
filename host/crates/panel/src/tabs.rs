//! Panel tabs that survive a restart: `panel_tabs_get` / `panel_tabs_save`.
//!
//! The value is `{version: 1, sets: {"<chat id>" | "" | "app": [{kind, key, v, data}]}}`. A save is
//! validated whole and refused whole: at most 48 entries in at most 64 KiB, keys and data at most
//! 4 KiB, kinds at most 32 characters, no control characters, exactly the four entry keys. Sets for
//! chats that do not exist are dropped. Each chat's set is a row in a side table with
//! `ON DELETE CASCADE`, so deleting a chat removes its saved tabs in that same transaction.

use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{Map, Value};

pub const MAX_ENTRIES: usize = 48;
pub const MAX_TOTAL_BYTES: usize = 64 * 1024;
pub const MAX_KEY_BYTES: usize = 4096;
pub const MAX_DATA_BYTES: usize = 4096;
pub const MAX_KIND_CHARS: usize = 32;
pub const APP_SET: &str = "app";

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS panel_tab_sets (
             set_key  TEXT PRIMARY KEY,
             chat_id  TEXT REFERENCES chats(id) ON DELETE CASCADE,
             entries  TEXT NOT NULL
         )",
    )
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

/// Validates a value from the renderer. Returns the sets in input order.
pub fn validate(value: &Value) -> Result<Vec<(String, Vec<Value>)>, String> {
    let text = serde_json::to_string(value).map_err(|e| e.to_string())?;
    if text.len() > MAX_TOTAL_BYTES {
        return Err("The saved tabs are larger than 64 KiB.".into());
    }
    let obj = value
        .as_object()
        .ok_or("The saved tabs must be an object.")?;
    if obj.len() != 2 || obj.get("version") != Some(&Value::from(1)) {
        return Err("The saved tabs must be {version: 1, sets}.".into());
    }
    let sets = obj
        .get("sets")
        .and_then(Value::as_object)
        .ok_or("`sets` must be an object.")?;
    let mut total = 0;
    let mut out = Vec::new();
    for (set, entries) in sets {
        if has_control(set) || set.len() > 128 {
            return Err("A set name is not valid.".into());
        }
        let entries = entries.as_array().ok_or("Each set must be a list.")?;
        for e in entries {
            total += 1;
            if total > MAX_ENTRIES {
                return Err("More than 48 saved tabs.".into());
            }
            let e = e.as_object().ok_or("Each saved tab must be an object.")?;
            if e.len() != 4
                || !["kind", "key", "v", "data"]
                    .iter()
                    .all(|k| e.contains_key(*k))
            {
                return Err("Each saved tab must have exactly kind, key, v and data.".into());
            }
            let kind = e["kind"].as_str().ok_or("`kind` must be text.")?;
            let key = e["key"].as_str().ok_or("`key` must be text.")?;
            if kind.is_empty() || kind.chars().count() > MAX_KIND_CHARS || has_control(kind) {
                return Err("A tab kind is not valid.".into());
            }
            if key.len() > MAX_KEY_BYTES || has_control(key) {
                return Err("A tab key is not valid.".into());
            }
            if !e["v"].is_u64() && !e["v"].is_i64() {
                return Err("`v` must be a whole number.".into());
            }
            let data = serde_json::to_string(&e["data"]).map_err(|e| e.to_string())?;
            if data.len() > MAX_DATA_BYTES {
                return Err("A tab's data is larger than 4 KiB.".into());
            }
        }
        out.push((set.clone(), entries.clone()));
    }
    Ok(out)
}

fn chat_exists(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row("SELECT 1 FROM chats WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Replaces every saved set. Sets for chats that do not exist are dropped, not stored.
pub fn save(tx: &Transaction<'_>, sets: &[(String, Vec<Value>)]) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM panel_tab_sets", [])?;
    for (set, entries) in sets {
        if entries.is_empty() {
            continue;
        }
        let chat_id = if set.is_empty() || set == APP_SET {
            None
        } else if chat_exists(tx, set)? {
            Some(set.as_str())
        } else {
            continue;
        };
        tx.execute(
            "INSERT INTO panel_tab_sets (set_key, chat_id, entries) VALUES (?1, ?2, ?3)",
            params![set, chat_id, Value::Array(entries.clone()).to_string()],
        )?;
    }
    Ok(())
}

pub fn load(tx: &Transaction<'_>) -> rusqlite::Result<Value> {
    let mut st = tx.prepare("SELECT set_key, entries FROM panel_tab_sets ORDER BY rowid")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut sets = Map::new();
    for row in rows {
        let (set, entries) = row?;
        if let Ok(v @ Value::Array(_)) = serde_json::from_str::<Value>(&entries) {
            sets.insert(set, v);
        }
    }
    Ok(serde_json::json!({ "version": 1, "sets": sets }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;
    use serde_json::json;

    fn db() -> (tempfile::TempDir, helm_core::db::Database) {
        let mut reg = MigrationRegistry::new();
        reg.initializer("panel_tab_sets", init);
        let dir = tempfile::tempdir().unwrap();
        let db = helm_core::db::Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        (dir, db)
    }

    fn add_chat(db: &helm_core::db::Database, id: &str) {
        db.write(|tx| {
            tx.execute("INSERT OR IGNORE INTO projects (id, name, path, created_at) VALUES ('p', 'p', 'C:/p', 0)", [])?;
            tx.execute("INSERT INTO chats (id, project_id, created_at, updated_at) VALUES (?1, 'p', 0, 0)", [id])
        })
        .unwrap();
    }

    fn entry(key: &str) -> Value {
        json!({"kind": "file", "key": key, "v": 1, "data": {"path": key}})
    }

    #[test]
    fn a_save_round_trips_and_drops_sets_of_missing_chats() {
        let (_d, db) = db();
        add_chat(&db, "c1");
        let value = json!({"version": 1, "sets": {"c1": [entry("a"), entry("b")], "gone": [entry("x")], "app": [entry("p")], "": [entry("n")]}});
        let sets = validate(&value).unwrap();
        db.write(|tx| save(tx, &sets)).unwrap();
        let back = db.snapshot(load).unwrap();
        assert_eq!(back["sets"]["c1"], json!([entry("a"), entry("b")]));
        assert_eq!(back["sets"]["app"], json!([entry("p")]));
        assert_eq!(back["sets"][""], json!([entry("n")]));
        assert!(back["sets"].get("gone").is_none());
    }

    #[test]
    fn deleting_a_chat_removes_its_saved_tabs_in_the_same_transaction() {
        let (_d, db) = db();
        add_chat(&db, "c1");
        let sets =
            validate(&json!({"version": 1, "sets": {"c1": [entry("a")], "app": [entry("p")]}}))
                .unwrap();
        db.write(|tx| save(tx, &sets)).unwrap();
        db.write(|tx| tx.execute("DELETE FROM chats WHERE id = 'c1'", []))
            .unwrap();
        let back = db.snapshot(load).unwrap();
        assert!(back["sets"].get("c1").is_none());
        assert_eq!(back["sets"]["app"], json!([entry("p")]));
    }

    #[test]
    fn invalid_values_are_refused_whole() {
        let many: Vec<Value> = (0..=MAX_ENTRIES).map(|i| entry(&format!("k{i}"))).collect();
        let bad = [
            json!(null),
            json!({"version": 2, "sets": {}}),
            json!({"version": 1, "sets": {}, "extra": 1}),
            json!({"version": 1, "sets": {"c": [{"kind": "file", "key": "a", "v": 1}]}}),
            json!({"version": 1, "sets": {"c": [{"kind": "file", "key": "a", "v": 1, "data": {}, "title": "x"}]}}),
            json!({"version": 1, "sets": {"c": [{"kind": "f".repeat(33), "key": "a", "v": 1, "data": {}}]}}),
            json!({"version": 1, "sets": {"c": [{"kind": "file", "key": "a\u{1}", "v": 1, "data": {}}]}}),
            json!({"version": 1, "sets": {"c": [{"kind": "file", "key": "k".repeat(MAX_KEY_BYTES + 1), "v": 1, "data": {}}]}}),
            json!({"version": 1, "sets": {"c": [{"kind": "file", "key": "a", "v": 1, "data": "d".repeat(MAX_DATA_BYTES)}]}}),
            json!({"version": 1, "sets": {"c": [{"kind": "file", "key": "a", "v": "1", "data": {}}]}}),
            json!({"version": 1, "sets": {"c": many}}),
        ];
        for v in bad {
            assert!(validate(&v).is_err(), "{v}");
        }
        let at_limit: Vec<Value> = (0..MAX_ENTRIES).map(|i| entry(&format!("k{i}"))).collect();
        assert!(validate(&json!({"version": 1, "sets": {"c": at_limit}})).is_ok());
        let big = "x".repeat(4000);
        let wide: Vec<Value> = (0..20)
            .map(|i| json!({"kind": "file", "key": format!("k{i}"), "v": 1, "data": big}))
            .collect();
        assert!(
            validate(&json!({"version": 1, "sets": {"c": wide}})).is_err(),
            "over 64 KiB in total"
        );
    }
}
