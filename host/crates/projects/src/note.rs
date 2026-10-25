//! The project context note: one short shared note per folder project, written and read by the
//! chats working in that folder (through two agent tools) so what one chat learned reaches the
//! next. Stored in `project_notes`, keyed by the folder project (the one a chat runs in).
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `project_note_get` | blocking | `{chatId}` → `{projectId, text, revision, updatedAt}` |
//! | `project_note_set` | maintenance | `{chatId, text, expectedRevision}` → the same shape |
//!
//! A save carries the revision it was based on and is refused when the note has moved on, so two
//! chats never overwrite each other unseen. Text that looks like a credential is refused.

use crate::ops::{parse_args, to_value};
use helm_core::db::Database;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};

/// The longest note, in characters.
pub const MAX_NOTE_CHARS: usize = 4_000;

pub(crate) fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("project_note_get", OpClass::Blocking, note_get)
        .blocking("project_note_set", OpClass::Maintenance, note_set);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetArgs {
    chat_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetArgs {
    chat_id: String,
    text: String,
    expected_revision: i64,
}

fn folder_of(tx: &Transaction<'_>, chat: &str) -> Result<String, ApiError> {
    tx.query_row("SELECT project_id FROM chats WHERE id = ?1", [chat], |r| {
        r.get(0)
    })
    .optional()
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::new("not_found", "That chat does not exist."))
}

fn read(tx: &Transaction<'_>, project: &str) -> Result<Value, ApiError> {
    let row: Option<(String, i64, i64)> = tx
        .query_row(
            "SELECT text, revision, updated_at FROM project_notes WHERE project_id = ?1",
            [project],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let (text, revision, updated_at) = row.unwrap_or_default();
    Ok(json!({"projectId": project, "text": text, "revision": revision, "updatedAt": updated_at}))
}

fn note_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: GetArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(folder_of(tx, &a.chat_id).and_then(|p| read(tx, &p))))
        .map_err(|e| ApiError::internal(e.to_string()))?
}

/// Whether the text looks like a credential: held back rather than stored, since the note is
/// read by every chat in the folder.
pub fn looks_like_secret(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if lower.contains("-----begin") && lower.contains("private key") {
        return true;
    }
    let prefixed = [
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "glpat-",
        "sk-ant-",
        "akia",
    ]
    .iter()
    .any(|p| {
        lower.match_indices(p).any(|(i, _)| {
            lower[i + p.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .count()
                >= 12
        })
    });
    prefixed
        || lower.lines().any(|l| {
            let l = l.trim();
            ["password", "passwd", "secret", "api_key", "apikey", "token"]
                .iter()
                .any(|k| {
                    l.starts_with(k)
                        && l[k.len()..].trim_start().starts_with(['=', ':'])
                        && l.len() > k.len() + 4
                })
        })
}

fn note_set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SetArgs = parse_args(args)?;
    if a.text.chars().count() > MAX_NOTE_CHARS {
        return Err(ApiError::bad_args(format!(
            "The note is at most {MAX_NOTE_CHARS} characters."
        )));
    }
    if looks_like_secret(&a.text) {
        return Err(ApiError::new(
            "refused",
            "That text looks like a password or key, so it was not saved. Leave secrets out of the note.",
        ));
    }
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    let v = db.write(|tx| {
        let project = match folder_of(tx, &a.chat_id) {
            Ok(p) => p,
            Err(e) => return Ok::<_, rusqlite::Error>(Err(e)),
        };
        let current = match read(tx, &project) {
            Ok(v) => v["revision"].as_i64().unwrap_or(0),
            Err(e) => return Ok(Err(e)),
        };
        if current != a.expected_revision {
            return Ok(Err(ApiError::conflict(format!(
                "The note changed since you read it (now revision {current}); read it again and merge your change."
            ))));
        }
        tx.execute(
            "INSERT INTO project_notes(project_id, text, revision, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id) DO UPDATE SET text = excluded.text,
                 revision = excluded.revision, updated_at = excluded.updated_at",
            params![project, a.text, current + 1, now],
        )?;
        Ok(read(tx, &project))
    })
    .map_err(|e| ApiError::internal(e.to_string()))??;
    to_value(&v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;
    use helm_ipc::{RecordingSink, Services};
    use std::sync::Arc;

    #[test]
    fn credential_looking_text_is_held_back_and_plain_notes_pass() {
        for (text, secret) in [
            ("Run `npm test` before pushing.", false),
            ("The token budget is tracked in usage.md", false),
            ("ghp_abcdefghijklmnop1234", true),
            ("password: hunter2hunter2", true),
            ("-----BEGIN RSA PRIVATE KEY-----", true),
            ("api_key=abc123456", true),
        ] {
            assert_eq!(looks_like_secret(text), secret, "{text}");
        }
    }

    #[test]
    fn a_note_is_shared_by_the_folders_chats_and_saves_check_the_revision() {
        let (_d, db) = temp_db();
        let (a, b, other) = db
            .write(|tx| {
                Ok::<_, rusqlite::Error>((
                    helm_chats::store::create_chat(tx, "C:/work/one", None, None, 1).unwrap(),
                    helm_chats::store::create_chat(tx, "C:/work/one", None, None, 2).unwrap(),
                    helm_chats::store::create_chat(tx, "C:/work/two", None, None, 3).unwrap(),
                ))
            })
            .unwrap();
        let mut services = Services::new();
        services.insert(Arc::new(db));
        let ctx = OpCtx {
            services: Arc::new(services),
            events: Arc::new(RecordingSink::default()),
        };
        let get = |chat: &str| note_get(&ctx, json!({"chatId": chat}));
        let set = |chat: &str, text: &str, rev: i64| {
            note_set(
                &ctx,
                json!({"chatId": chat, "text": text, "expectedRevision": rev}),
            )
        };
        // (chat, text, expected revision, error code or "" for success, revision after)
        let long = "x".repeat(MAX_NOTE_CHARS + 1);
        for (chat, text, rev, err, after) in [
            (&a, "Use npm ci.", 0, "", 1),
            (&a, "stale", 0, "conflict", 1),
            (&b, "Use npm ci. Tests are in checks/.", 1, "", 2),
            (&b, long.as_str(), 2, "bad_args", 2),
            (&b, "token: abcdefghijkl", 2, "refused", 2),
            (&other, "Another folder.", 0, "", 1),
            (&a, "", 2, "", 3),
        ] {
            match set(chat, text, rev) {
                Ok(v) => assert!(err.is_empty() && v["revision"] == after, "{text}: {v}"),
                Err(e) => assert_eq!(e.code, err, "{text}"),
            }
            assert_eq!(get(chat).unwrap()["revision"], after, "{text}");
        }
        // The same folder shares one note; another folder has its own; a clear leaves it empty.
        assert_eq!(get(&b).unwrap()["text"], "");
        assert_eq!(get(&other).unwrap()["text"], "Another folder.");
        assert_eq!(get("no-such-chat").unwrap_err().code, "not_found");
    }
}
