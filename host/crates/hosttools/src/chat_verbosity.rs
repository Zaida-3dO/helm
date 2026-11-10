//! How long one chat's replies should be.
//!
//! The new-chat default is the user-only `responseVerbosity` preference. A chat can have its own
//! level, set from the model and effort picker. It is kept apart from the chat record, and a chat
//! with no row follows the default. A row goes with its chat (`ON DELETE CASCADE`).
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `chat_verbosity_get` | blocking | `{chatId}` → `{level, own, default}`: the level in force, whether the chat has its own, and the new-chat default |
//! | `chat_verbosity_set` | maintenance | `{chatId, level}` (`low`, `medium`, `high`, or `null` to follow the default) → the same answer; a chat that does not exist is refused |
//!
//! A level reaches the agent in the prompt addendum when the chat's worker starts, so a change
//! applies from the chat's next start. The picker says so where it offers the choice.

use helm_core::db::Database;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};

pub const LEVELS: [&str; 3] = ["low", "medium", "high"];

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_verbosity (
             chat_id  TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             level    TEXT NOT NULL,
             saved_at INTEGER NOT NULL
         );",
    )
}

/// The chat's own level, if it has one.
fn own(conn: &rusqlite::Connection, chat_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT level FROM chat_verbosity WHERE chat_id = ?1",
        [chat_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .filter(|l| LEVELS.contains(&l.as_str()))
}

fn own_of(db: &Database, chat_id: &str) -> Option<String> {
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(own(tx, chat_id)))
        .ok()
        .flatten()
}

/// The level in force for a chat: its own, else the new-chat default (`low` when unset).
pub fn level_for(db: &Database, chat_id: &str) -> String {
    own_of(db, chat_id).unwrap_or_else(|| crate::sections::verbosity(db))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetArgs {
    chat_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetArgs {
    chat_id: String,
    level: Option<String>,
}

fn parse<T: for<'de> serde::Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn answer(db: &Database, chat_id: &str) -> Value {
    let mine = own_of(db, chat_id);
    let default = crate::sections::verbosity(db);
    json!({
        "level": mine.clone().unwrap_or_else(|| default.clone()),
        "own": mine.is_some(),
        "default": default,
    })
}

fn get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: GetArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    Ok(answer(&db, &a.chat_id))
}

fn set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SetArgs = parse(args)?;
    if a.chat_id.is_empty() || a.chat_id.len() > 200 {
        return Err(ApiError::bad_args("chatId must be 1 to 200 bytes."));
    }
    if let Some(l) = a.level.as_deref() {
        if !LEVELS.contains(&l) {
            return Err(ApiError::bad_args("level must be low, medium or high."));
        }
    }
    let db = ctx.services.need::<Database>()?;
    let known = db
        .write(|tx| {
            let known = tx
                .query_row("SELECT 1 FROM chats WHERE id = ?1", [&a.chat_id], |_| Ok(()))
                .optional()?
                .is_some();
            if !known {
                return Ok(false);
            }
            let _changed: usize = match a.level.as_deref() {
                Some(l) => tx.execute(
                    "INSERT INTO chat_verbosity(chat_id, level, saved_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(chat_id) DO UPDATE SET level = excluded.level, saved_at = excluded.saved_at",
                    params![a.chat_id, l, helm_core::now_ms()],
                ),
                None => tx.execute("DELETE FROM chat_verbosity WHERE chat_id = ?1", [&a.chat_id]),
            }?;
            Ok::<_, rusqlite::Error>(true)
        })
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if !known {
        return Err(ApiError::new("not_found", "That chat does not exist."));
    }
    Ok(answer(&db, &a.chat_id))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("chat_verbosity_get", OpClass::Blocking, get)
        .blocking("chat_verbosity_set", OpClass::Maintenance, set);
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;
    use helm_ipc::{NullSink, Services};
    use std::sync::Arc;

    fn add_chat(db: &Database, id: &str) {
        db.write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO projects (id, name, path, created_at) VALUES ('p', 'p', 'C:/p', 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO chats (id, project_id, created_at, updated_at) VALUES (?1, 'p', 0, 0)",
                [id],
            )
        })
        .unwrap();
    }

    #[test]
    fn a_chat_follows_the_default_until_it_has_its_own_level_and_loses_it_with_the_chat() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        reg.initializer("test-chat-verbosity", init);
        let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
        add_chat(&db, "c1");
        add_chat(&db, "c2");
        let mut services = Services::new();
        services.insert(db.clone());
        let ctx = OpCtx {
            services: Arc::new(services),
            events: Arc::new(NullSink),
        };
        let first = get(&ctx, json!({"chatId": "c1"})).unwrap();
        assert_eq!(
            first,
            json!({"level": "low", "own": false, "default": "low"})
        );
        let a = set(&ctx, json!({"chatId": "c1", "level": "high"})).unwrap();
        assert_eq!(a, json!({"level": "high", "own": true, "default": "low"}));
        assert_eq!(level_for(&db, "c1"), "high");
        assert_eq!(
            level_for(&db, "c2"),
            "low",
            "another chat follows the default"
        );
        let a = set(&ctx, json!({"chatId": "c1", "level": null})).unwrap();
        assert_eq!(a["own"], false);
        for bad in [
            json!({"chatId": "c1", "level": "loud"}),
            json!({"chatId": ""}),
            json!({"level": "low"}),
            json!({"chatId": "nope", "level": "low"}),
            json!({"chatId": "nope", "level": null}),
        ] {
            assert!(set(&ctx, bad.clone()).is_err(), "{bad}");
        }
        set(&ctx, json!({"chatId": "c2", "level": "medium"})).unwrap();
        db.write(|tx| tx.execute("DELETE FROM chats WHERE id = 'c2'", []))
            .unwrap();
        let rows: i64 = db
            .snapshot(|tx| tx.query_row("SELECT COUNT(*) FROM chat_verbosity", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(rows, 0, "deleting a chat deletes its row");
    }
}
