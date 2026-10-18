//! The workflow `api` ops.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `workflow_settings_get` | blocking | `{}` to [`WorkflowSettings`] |
//! | `workflow_settings_save` | maintenance | `{settings}` to [`WorkflowSettings`]; emits `workflow_settings {}` |
//! | `workflow_chat_mode_get` | blocking | [`ChatModeArgs`] to [`ChatModeView`] |
//! | `workflow_chat_mode_set` | maintenance | [`ChatModeSetArgs`] to [`ChatModeSetResult`]; emits `workflow_chat_mode {chatId}` |

use crate::types::*;
use crate::{mode, settings};
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use rusqlite::{OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};

pub const TOGGLE_WHILE_RUNNING: &str =
    "A Claude Code chat is running a turn; turn Orchestrator on or off once it has finished.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    settings: Value,
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("workflow_settings_get", OpClass::Blocking, settings_get)
        .blocking(
            "workflow_settings_save",
            OpClass::Maintenance,
            settings_save,
        )
        .blocking("workflow_chat_mode_get", OpClass::Blocking, chat_mode_get)
        .blocking(
            "workflow_chat_mode_set",
            OpClass::Maintenance,
            chat_mode_set,
        );
}

/// A Claude Code chat whose turn is starting, running or stopping.
pub fn any_claude_turn(tx: &Transaction<'_>) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM chats WHERE harness = 'claude'
               AND status IN ('starting', 'running', 'stopping') LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn settings_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    to_value(&db.snapshot(settings::load).map_err(sql)?)
}

fn settings_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SaveArgs = parse(args)?;
    let next = settings::validate(a.settings).map_err(ApiError::bad_args)?;
    let db = ctx.services.need::<Database>()?;
    let saved = db
        .write(|tx| {
            let prev = settings::load(tx)?;
            if prev.enabled != next.enabled && any_claude_turn(tx)? {
                return Ok(Err(ApiError::conflict(TOGGLE_WHILE_RUNNING)));
            }
            settings::store(tx, &next, helm_core::now_ms())?;
            Ok::<_, rusqlite::Error>(Ok(next))
        })
        .map_err(sql)??;
    ctx.events.emit(AppEvent::new("workflow_settings"));
    to_value(&saved)
}

fn chat_mode_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatModeArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let v = db
        .snapshot(|tx| {
            let s = settings::load(tx)?;
            mode::view(tx, &a.chat_id, &s)
        })
        .map_err(sql)?
        .ok_or_else(|| ApiError::new("not_found", "No such chat."))?;
    to_value(&v)
}

fn chat_mode_set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatModeSetArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    let out = db
        .write(|tx| {
            let s = settings::load(tx)?;
            let changed = match mode::user_set(tx, &a.chat_id, a.mode, &s, now)? {
                Ok(c) => c,
                Err(e) => return Ok(Err(ApiError::conflict(e))),
            };
            let view = mode::view(tx, &a.chat_id, &s)?.expect("the chat exists");
            Ok::<_, rusqlite::Error>(Ok((view, changed, s)))
        })
        .map_err(sql)??;
    let (view, changed, s) = out;
    if let Some(c) = &changed {
        if let Some(rt) = ctx.services.get::<helm_chats::ChatRuntime>() {
            let d = crate::notes::plan(&a.chat_id, c, view.running, false, &s, now);
            crate::notes::deliver(&rt, &a.chat_id, d);
        }
        ctx.events
            .emit(AppEvent::new("workflow_chat_mode").with("chatId", json!(a.chat_id)));
    }
    to_value(&ChatModeSetResult {
        view,
        row: changed.map(|c| c.row),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_ipc::{OpCtx, RecordingSink, Services};
    use std::sync::Arc;

    fn ctx(db: Database, sink: Arc<RecordingSink>) -> OpCtx {
        let mut s = Services::new();
        s.insert(Arc::new(db));
        OpCtx {
            services: Arc::new(s),
            events: sink,
        }
    }

    #[test]
    fn settings_save_validates_and_refuses_a_toggle_mid_turn() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude")]);
        let sink = Arc::new(RecordingSink::default());
        let c = ctx(db, sink.clone());
        let e = settings_save(&c, json!({"settings": {"bogus": true}})).unwrap_err();
        assert!(e.message.contains("bogus"), "{}", e.message);
        let got =
            settings_save(&c, json!({"settings": {"enabled": true, "moveFast": true}})).unwrap();
        assert_eq!(got["enabled"], true);
        assert_eq!(got["moveFast"], true);
        assert_eq!(settings_get(&c, json!({})).unwrap()["moveFast"], true);
        assert_eq!(sink.kinds(), ["workflow_settings"]);
        let db = c.services.need::<Database>().unwrap();
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute("UPDATE chats SET status = 'running'", [])?;
            Ok(())
        })
        .unwrap();
        // A gate-only change is fine mid-turn; the master switch is not.
        assert!(settings_save(&c, json!({"settings": {"enabled": true}})).is_ok());
        let e = settings_save(&c, json!({"settings": {"enabled": false}})).unwrap_err();
        assert_eq!(e.message, TOGGLE_WHILE_RUNNING);
        assert_eq!(settings_get(&c, json!({})).unwrap()["enabled"], true);
    }

    #[test]
    fn the_chat_mode_ops_read_and_flip_one_chat() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude")]);
        let sink = Arc::new(RecordingSink::default());
        let c = ctx(db, sink.clone());
        let v = chat_mode_get(&c, json!({"chatId": "a"})).unwrap();
        assert_eq!(v["mode"], "on");
        assert_eq!(v["inEffect"], false, "the master switch is off by default");
        assert!(v["unavailable"].as_str().unwrap().contains("Settings"));
        assert!(chat_mode_set(&c, json!({"chatId": "a", "mode": "off"})).is_err());
        settings_save(&c, json!({"settings": {"enabled": true}})).unwrap();
        let r = chat_mode_set(&c, json!({"chatId": "a", "mode": "off"})).unwrap();
        assert_eq!(r["view"]["mode"], "off");
        assert_eq!(r["view"]["setBy"], "user");
        assert_eq!(r["row"], "You turned Orchestrator off for this chat.");
        let again = chat_mode_set(&c, json!({"chatId": "a", "mode": "off"})).unwrap();
        assert!(again.get("row").is_none());
        assert_eq!(sink.kinds(), ["workflow_settings", "workflow_chat_mode"]);
        assert_eq!(
            chat_mode_get(&c, json!({"chatId": "zz"})).unwrap_err().code,
            "not_found"
        );
        assert!(chat_mode_set(&c, json!({"chatId": "a", "mode": "sideways"})).is_err());
    }
}
