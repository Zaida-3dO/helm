//! Chat management ops.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `chat_archive` | maintenance | [`ChatIdArgs`] → [`ChatRecord`] (refused while busy; idempotent; retires the idle worker) |
//! | `chat_restore` | maintenance | [`ChatIdArgs`] → [`ChatRecord`] (refused in an archived project) |
//! | `chat_delete` | maintenance | [`ChatIdArgs`] → `null` (archived and idle only; emits `chat_removed`) |
//! | `chat_mute` | maintenance | [`ChatMuteArgs`] → [`ChatRecord`] (user-only) |
//! | `attention_address` | maintenance | [`ChatIdArgs`] → `null` (out of the attention groups until new activity) |
//! | `attention_snooze` | maintenance | [`AttentionSnoozeArgs`] → `null` (out until the time, or new activity) |
//! | `unread_mark` | maintenance | [`ChatIdArgs`] → [`ChatRecord`] (a fresh token; re-marking replaces it) |
//! | `unread_clear` | maintenance | [`UnreadClearArgs`] → [`ChatRecord`] (only with the matching token) |
//! | `completion_ack` | maintenance | [`CompletionAckArgs`] → [`ChatRecord`] (only the matching completion) |
//!
//! Every op that changes a chat emits its `chat` event and republishes the attention summary.
//! Undo and redo of archive live in the UI, on top of these ops.

use crate::archive::{self, Refusal};
use crate::clock::Clock;
use crate::delete::{self, DeleteRule};
use crate::ops::{not_found, parse_args, sql};
use crate::runtime::ChatRuntime;
use crate::seams::ProjectGates;
use crate::store;
use crate::types::*;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;

pub fn register(reg: &mut OpRegistry) {
    reg.blocking("chat_archive", OpClass::Maintenance, chat_archive)
        .blocking("chat_restore", OpClass::Maintenance, chat_restore)
        .blocking("chat_delete", OpClass::Maintenance, chat_delete)
        .blocking("unread_mark", OpClass::Maintenance, unread_mark)
        .blocking("unread_clear", OpClass::Maintenance, unread_clear)
        .blocking("completion_ack", OpClass::Maintenance, completion_ack)
        .blocking("chat_mute", OpClass::Maintenance, chat_mute)
        .blocking("attention_address", OpClass::Maintenance, attention_address)
        .blocking("attention_snooze", OpClass::Maintenance, attention_snooze);
}

/// Whether the chat's project is archived, through the projects crate's gate (open when none).
pub(crate) fn project_archived(
    ctx: &OpCtx,
    tx: &Transaction<'_>,
    project_id: &str,
) -> rusqlite::Result<bool> {
    match ctx.services.get::<ProjectGates>() {
        Some(g) => g.0.project_archived(tx, project_id),
        None => Ok(false),
    }
}

pub(crate) const ARCHIVED_PROJECT: &str =
    "This chat's project is archived; restore the project first.";

/// Emit the chat's `chat` event and republish attention; returns the record.
fn changed(ctx: &OpCtx, chat_id: &str) -> Result<ChatRecord, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let rev = ctx.services.need::<Clock>()?.next();
    let chat = db
        .snapshot(|tx| store::get_chat(tx, chat_id, rev))
        .map_err(sql)?
        .ok_or_else(not_found)?;
    ctx.events
        .emit(ChatEvent::Chat { chat: chat.clone() }.to_app_event());
    if let Some(rt) = ctx.services.get::<ChatRuntime>() {
        rt.publish_attention();
    }
    Ok(chat)
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn chat_archive(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    let r = db
        .write(|tx| archive::archive_in_tx(tx, &a.chat_id, now))
        .map_err(sql)?;
    r.map_err(ApiError::from)?;
    if let Some(rt) = ctx.services.get::<ChatRuntime>() {
        rt.retire(&a.chat_id);
    }
    to_value(&changed(ctx, &a.chat_id)?)
}

fn chat_restore(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    let r = db
        .write(|tx| {
            let project: Option<String> = tx
                .query_row(
                    "SELECT project_id FROM chats WHERE id = ?1",
                    [&a.chat_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(p) = project {
                if project_archived(ctx, tx, &p)? {
                    return Ok(Err(Refusal::new("refused", ARCHIVED_PROJECT)));
                }
            }
            archive::restore_in_tx(tx, &a.chat_id, now)
        })
        .map_err(sql)?;
    r.map_err(ApiError::from)?;
    to_value(&changed(ctx, &a.chat_id)?)
}

/// Delete through `rule`, then retire any actor, emit `chatDeleted` and clean up files.
pub(crate) fn delete_chat(ctx: &OpCtx, chat_id: &str, rule: DeleteRule) -> Result<(), ApiError> {
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    let r = db
        .write(|tx| delete::delete_in_tx(tx, chat_id, rule, now))
        .map_err(sql)?;
    r.map_err(ApiError::from)?;
    if let Some(rt) = ctx.services.get::<ChatRuntime>() {
        rt.retire(chat_id);
        rt.publish_attention();
    }
    ctx.events.emit(
        ChatEvent::ChatRemoved {
            chat_id: chat_id.to_owned(),
        }
        .to_app_event(),
    );
    if let Some(profile) = ctx.services.get::<Profile>() {
        delete::run_cleanup(&db, &profile.path(""));
    }
    Ok(())
}

fn chat_delete(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    delete_chat(ctx, &a.chat_id, DeleteRule::Archived)?;
    Ok(Value::Null)
}

fn exists(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1)",
        [chat_id],
        |r| r.get(0),
    )
}

fn unread_mark(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let token = uuid::Uuid::new_v4().to_string();
    let found = db
        .write(|tx| {
            if !exists(tx, &a.chat_id)? {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO unread_marks(chat_id, token, marked_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(chat_id) DO UPDATE SET token = excluded.token, marked_at = excluded.marked_at",
                params![a.chat_id, token, helm_core::now_ms()],
            )?;
            Ok::<_, rusqlite::Error>(true)
        })
        .map_err(sql)?;
    if !found {
        return Err(not_found());
    }
    to_value(&changed(ctx, &a.chat_id)?)
}

fn unread_clear(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: UnreadClearArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let found = db
        .write(|tx| {
            if !exists(tx, &a.chat_id)? {
                return Ok(false);
            }
            tx.execute(
                "DELETE FROM unread_marks WHERE chat_id = ?1 AND token = ?2",
                params![a.chat_id, a.token],
            )?;
            Ok::<_, rusqlite::Error>(true)
        })
        .map_err(sql)?;
    if !found {
        return Err(not_found());
    }
    to_value(&changed(ctx, &a.chat_id)?)
}

fn completion_ack(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CompletionAckArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let found = db
        .write(|tx| {
            if !exists(tx, &a.chat_id)? {
                return Ok(false);
            }
            tx.execute(
                "UPDATE chat_completions SET read = 1 WHERE chat_id = ?1 AND id = ?2",
                params![a.chat_id, a.completion_id],
            )?;
            Ok::<_, rusqlite::Error>(true)
        })
        .map_err(sql)?;
    if !found {
        return Err(not_found());
    }
    to_value(&changed(ctx, &a.chat_id)?)
}

fn chat_mute(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatMuteArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let found = db
        .write(|tx| {
            if !exists(tx, &a.chat_id)? {
                return Ok(false);
            }
            if a.muted {
                tx.execute(
                    "INSERT OR IGNORE INTO chat_mutes(chat_id, muted_at) VALUES (?1, ?2)",
                    params![a.chat_id, helm_core::now_ms()],
                )?;
            } else {
                tx.execute("DELETE FROM chat_mutes WHERE chat_id = ?1", [&a.chat_id])?;
            }
            Ok::<_, rusqlite::Error>(true)
        })
        .map_err(sql)?;
    if !found {
        return Err(not_found());
    }
    to_value(&changed(ctx, &a.chat_id)?)
}

/// The longest snooze.
pub const MAX_SNOOZE_MS: i64 = 367 * 24 * 60 * 60 * 1000;

fn attention_write(
    ctx: &OpCtx,
    chat_id: &str,
    sql_text: &str,
    value: Option<i64>,
) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    let found = db
        .write(|tx| {
            if !exists(tx, chat_id)? {
                return Ok(false);
            }
            tx.execute(sql_text, params![chat_id, value, now])?;
            Ok::<_, rusqlite::Error>(true)
        })
        .map_err(sql)?;
    if !found {
        return Err(not_found());
    }
    changed(ctx, chat_id)?;
    Ok(Value::Null)
}

fn attention_address(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    attention_write(
        ctx,
        &a.chat_id,
        "INSERT INTO chat_attention(chat_id, addressed_at) VALUES (?1, ?3)
         ON CONFLICT(chat_id) DO UPDATE SET addressed_at = ?3",
        None,
    )
}

fn attention_snooze(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: AttentionSnoozeArgs = parse_args(args)?;
    let now = helm_core::now_ms();
    if let Some(u) = a.until {
        if u <= now || u > now + MAX_SNOOZE_MS {
            return Err(ApiError::bad_args(
                "A snooze must end in the future and within 367 days.",
            ));
        }
    }
    attention_write(
        ctx,
        &a.chat_id,
        "INSERT INTO chat_attention(chat_id, snoozed_until, snoozed_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(chat_id) DO UPDATE SET snoozed_until = ?2, snoozed_at = ?3",
        a.until,
    )
}

// ---------------------------------------------------------------------------------------------
// Completions, written by the actor
// ---------------------------------------------------------------------------------------------

pub const PREVIEW_CHARS: usize = 240;

/// The reply's opening as one line of at most [`PREVIEW_CHARS`] characters.
pub fn preview(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PREVIEW_CHARS {
        return flat;
    }
    let mut out: String = flat.chars().take(PREVIEW_CHARS - 1).collect();
    out.push('…');
    out
}

/// A clean finish: create or refresh the chat's completion (new id, read and dismissed cleared).
/// Returns it for the `completion` event, or `None` for a side chat (never announced).
pub fn record_completion(
    tx: &Transaction<'_>,
    chat_id: &str,
    now: i64,
) -> rusqlite::Result<Option<Completion>> {
    let row: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT c.title, COALESCE(p.name, ''), c.side_source_id FROM chats c
             JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((title, project, side)) = row else {
        return Ok(None);
    };
    let last: String = tx
        .query_row(
            "SELECT text FROM messages WHERE chat_id = ?1 AND role = 'assistant' AND active = 1
             AND text != '' ORDER BY timestamp DESC, id DESC LIMIT 1",
            [chat_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default();
    let id = uuid::Uuid::new_v4().to_string();
    let preview = preview(&last);
    tx.execute(
        "INSERT INTO chat_completions(chat_id, id, preview, completed_at, read, dismissed)
         VALUES (?1, ?2, ?3, ?4, 0, 0)
         ON CONFLICT(chat_id) DO UPDATE SET id = excluded.id, preview = excluded.preview,
             completed_at = excluded.completed_at, read = 0, dismissed = 0",
        params![chat_id, id, preview, now],
    )?;
    let muted: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_mutes WHERE chat_id = ?1)",
        [chat_id],
        |r| r.get(0),
    )?;
    if side.is_some() || muted {
        // Kept as the chat's read state, but never announced (no toast for a muted chat).
        return Ok(None);
    }
    Ok(Some(Completion {
        id,
        chat_id: chat_id.to_owned(),
        title: if title.trim().is_empty() {
            "Untitled".into()
        } else {
            title
        },
        project,
        preview,
        completed_at: now,
        read: false,
        dismissed: false,
    }))
}

/// Entering an active or stopped state drops the completion.
pub fn drop_completion(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM chat_completions WHERE chat_id = ?1", [chat_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{harness, main};
    use serde_json::json;

    #[test]
    fn previews_are_one_line_and_cut_at_240() {
        assert_eq!(preview("  a\n\n b  "), "a b");
        let long = "x".repeat(300);
        let p = preview(&long);
        assert_eq!(p.chars().count(), PREVIEW_CHARS);
        assert!(p.ends_with('…'));
    }

    async fn create(h: &crate::ops::tests::Harness) -> String {
        let c = h
            .api
            .call(
                &main(),
                "chat_create",
                json!({"cwd": h.work.to_string_lossy()}),
            )
            .await
            .unwrap();
        c["id"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn marks_clear_only_with_their_token() {
        let h = harness();
        let id = create(&h).await;
        let m1 = h
            .api
            .call(&main(), "unread_mark", json!({"chatId": id}))
            .await
            .unwrap();
        let t1 = m1["unread"]["markToken"].as_str().unwrap().to_owned();
        let m2 = h
            .api
            .call(&main(), "unread_mark", json!({"chatId": id}))
            .await
            .unwrap();
        let t2 = m2["unread"]["markToken"].as_str().unwrap().to_owned();
        assert_ne!(t1, t2, "re-marking replaces the token");
        let stale = h
            .api
            .call(&main(), "unread_clear", json!({"chatId": id, "token": t1}))
            .await
            .unwrap();
        assert_eq!(
            stale["unread"]["markToken"],
            t2.as_str(),
            "a stale read leaves the newer mark"
        );
        let cleared = h
            .api
            .call(&main(), "unread_clear", json!({"chatId": id, "token": t2}))
            .await
            .unwrap();
        assert!(cleared["unread"]["markToken"].is_null());
        let gone = h
            .api
            .call(&main(), "unread_mark", json!({"chatId": "nope"}))
            .await
            .unwrap_err();
        assert_eq!(gone.code, "not_found");
    }

    #[tokio::test]
    async fn completion_ack_needs_the_matching_id() {
        let h = harness();
        let id = create(&h).await;
        let k =
            h.db.write(|tx| record_completion(tx, &id, 5))
                .unwrap()
                .unwrap();
        let got = h
            .api
            .call(&main(), "chat_get", json!({"chatId": id}))
            .await
            .unwrap();
        assert_eq!(got["chat"]["unread"]["completionId"], k.id.as_str());
        h.api
            .call(
                &main(),
                "completion_ack",
                json!({"chatId": id, "completionId": "other"}),
            )
            .await
            .unwrap();
        let got = h
            .api
            .call(&main(), "chat_get", json!({"chatId": id}))
            .await
            .unwrap();
        assert_eq!(
            got["chat"]["unread"]["completionId"],
            k.id.as_str(),
            "a stale ack changes nothing"
        );
        let acked = h
            .api
            .call(
                &main(),
                "completion_ack",
                json!({"chatId": id, "completionId": k.id}),
            )
            .await
            .unwrap();
        assert!(acked["unread"]["completionId"].is_null());
    }

    struct AllArchived;
    impl crate::seams::ProjectGate for AllArchived {
        fn project_archived(&self, _tx: &Transaction<'_>, _p: &str) -> rusqlite::Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn an_archived_project_refuses_restore_new_chats_and_sends() {
        let open = harness();
        let id = create(&open).await;
        open.api
            .call(&main(), "chat_archive", json!({"chatId": id}))
            .await
            .unwrap();
        // The same database, now behind a gate that calls every project archived.
        let gated = crate::ops::tests::harness_with(|s| {
            s.insert(std::sync::Arc::new(ProjectGates(std::sync::Arc::new(
                AllArchived,
            ))))
        });
        let id2 = gated
            .db
            .write(|tx| store::create_chat(tx, &gated.work.to_string_lossy(), None, None, 1))
            .unwrap();
        gated
            .db
            .write(|tx| tx.execute("UPDATE chats SET archived = 1 WHERE id = ?1", [&id2]))
            .unwrap();
        let r = gated
            .api
            .call(&main(), "chat_restore", json!({"chatId": id2}))
            .await
            .unwrap_err();
        assert_eq!(r.message, ARCHIVED_PROJECT);
        let c = gated
            .api
            .call(
                &main(),
                "chat_create",
                json!({"cwd": gated.work.to_string_lossy()}),
            )
            .await
            .unwrap_err();
        assert_eq!(c.code, "refused");
    }

    #[tokio::test]
    async fn archive_restore_and_delete_through_the_ops() {
        let h = harness();
        let id = create(&h).await;
        let refused = h
            .api
            .call(&main(), "chat_delete", json!({"chatId": id}))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "refused");
        let a = h
            .api
            .call(&main(), "chat_archive", json!({"chatId": id}))
            .await
            .unwrap();
        assert_eq!(a["archived"], true);
        let again = h
            .api
            .call(&main(), "chat_archive", json!({"chatId": id}))
            .await
            .unwrap();
        assert_eq!(again["archived"], true, "idempotent");
        let r = h
            .api
            .call(&main(), "chat_restore", json!({"chatId": id}))
            .await
            .unwrap();
        assert_eq!(r["archived"], false);
        h.api
            .call(&main(), "chat_archive", json!({"chatId": id}))
            .await
            .unwrap();
        h.api
            .call(&main(), "chat_delete", json!({"chatId": id}))
            .await
            .unwrap();
        assert!(h.sink.kinds().contains(&"chat_removed".to_owned()));
        let missing = h
            .api
            .call(&main(), "chat_get", json!({"chatId": id}))
            .await
            .unwrap_err();
        assert_eq!(missing.code, "not_found");
    }
}
