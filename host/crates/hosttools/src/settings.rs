//! The settings tools, the change ring and Undo.
//!
//! - `app_get_settings` reads the agent-visible keys; `app_update_settings` changes 1–16 of them,
//!   applied alphabetically through the same validators as the UI and stopping at the first
//!   failure. Each applied change goes into `settings_changes` (a 100-entry ring) and the UI gets
//!   `preferences` plus `external_settings {chatId, changes}`, which shows the 8 s toast with Undo
//!   and Open chat.
//! - Ops `settings_changes_list` and `settings_undo {changeIds[1–16]}`: all or nothing, and only
//!   if every value is still what the chat set.

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::settings_allow::{agent_keys, refusal};
use crate::settings_options;
use crate::spec::ToolSpec;
use helm_core::db::Database;
use helm_core::prefs::{self, PrefKey};
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use rusqlite::{params, Transaction};
use serde_json::{json, Map, Value};

pub const RING: i64 = 100;
pub const MAX_CHANGES: usize = 16;
/// Largest single value an agent may set, serialised.
pub const MAX_VALUE_BYTES: usize = 16 * 1024;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings_changes (
             id        INTEGER PRIMARY KEY AUTOINCREMENT,
             chat_id   TEXT NOT NULL,
             key       TEXT NOT NULL,
             before    TEXT,
             after     TEXT,
             at        INTEGER NOT NULL,
             undone    INTEGER NOT NULL DEFAULT 0
         );",
    )
}

fn all_settings(tx: &rusqlite::Connection) -> bool {
    matches!(prefs::read(tx, "agentSettingsAll"), Some(Value::Bool(true)))
}

fn sql(e: rusqlite::Error) -> ToolError {
    ToolError::maybe(e.to_string())
}

fn db(ctx: &ToolCtx) -> Result<std::sync::Arc<Database>, ToolError> {
    ctx.services
        .get::<Database>()
        .ok_or_else(|| ToolError::refused("Settings are not available here."))
}

fn get_settings(ctx: &ToolCtx, a: Value) -> ToolResult {
    let db = db(ctx)?;
    db.snapshot(|tx| {
        let all = all_settings(tx);
        let visible = agent_keys(all);
        let wanted: Vec<String> = match a.get("keys").and_then(Value::as_array) {
            Some(k) => k
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            None => visible.iter().map(|k| k.to_string()).collect(),
        };
        let mut settings = Map::new();
        let mut options = Map::new();
        let mut unavailable = Map::new();
        for k in &wanted {
            if let Some(why) = refusal(k, all) {
                unavailable.insert(k.clone(), json!(why));
            } else {
                settings.insert(k.clone(), prefs::read(tx, k).unwrap_or(Value::Null));
                if let Some(o) = settings_options::describe(k) {
                    options.insert(k.clone(), o);
                }
            }
        }
        Ok::<_, rusqlite::Error>(json!({
            "settings": settings,
            "options": options,
            "unavailable": unavailable,
            "note": "null means the default is in use: options.<key>.default says what that is, and sending null in app_update_settings restores it. options.<key> gives the type and the allowed values or limits.",
        }))
    })
    .map_err(sql)
}

fn update_settings(ctx: &ToolCtx, a: Value) -> ToolResult {
    let changes = a
        .get("changes")
        .and_then(Value::as_object)
        .ok_or_else(|| ToolError::refused("changes must be an object of {key: value}."))?;
    if changes.is_empty() || changes.len() > MAX_CHANGES {
        return Err(ToolError::refused("changes takes 1-16 keys."));
    }
    let mut keys: Vec<&String> = changes.keys().collect();
    keys.sort();
    let db = db(ctx)?;
    let now = helm_core::now_ms();
    let chat = ctx.caller_chat_id.clone();
    // One transaction per key, alphabetically, stopping at the first failure.
    let mut applied = Map::new();
    let mut recorded = Vec::new();
    let mut error: Option<String> = None;
    for k in keys {
        let v = &changes[k];
        let r = db.write(|tx| {
            if let Some(why) = refusal(k, all_settings(tx)) {
                return Ok(Err(why));
            }
            if serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX) > MAX_VALUE_BYTES {
                return Ok(Err(format!("The value for {k} is too large.")));
            }
            if let Err(why) = settings_options::check(k, v) {
                return Ok(Err(why));
            }
            let key = agent_keys(true)
                .into_iter()
                .find(|x| x == k)
                .map(PrefKey::classified)
                .expect("refusal() passed only agent keys");
            let before = prefs::read(tx, k);
            if let Err(e) = prefs::set(tx, key, v) {
                return Ok(Err(e.to_string()));
            }
            let after = prefs::read(tx, k);
            tx.execute(
                "INSERT INTO settings_changes(chat_id, key, before, after, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    chat,
                    k,
                    before.as_ref().map(Value::to_string),
                    after.as_ref().map(Value::to_string),
                    now
                ],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "DELETE FROM settings_changes WHERE id NOT IN
                     (SELECT id FROM settings_changes ORDER BY id DESC LIMIT ?1)",
                [RING],
            )?;
            Ok::<_, rusqlite::Error>(Ok((id, before, after)))
        });
        match r {
            Ok(Ok((id, before, after))) => {
                applied.insert(
                    k.clone(),
                    json!({"before": before, "after": after, "changeId": id}),
                );
                recorded.push(json!({"changeId": id, "key": k, "before": before, "after": after}));
            }
            Ok(Err(why)) => {
                error = Some(why);
                break;
            }
            Err(e) => {
                error = Some(format!("{k} may not have been saved: {e}"));
                break;
            }
        }
    }
    if !recorded.is_empty() {
        let changed: Map<String, Value> = recorded
            .iter()
            .map(|c| {
                (
                    c["key"].as_str().unwrap_or_default().to_owned(),
                    c["after"].clone(),
                )
            })
            .collect();
        ctx.events
            .emit(AppEvent::new("preferences").with("changed", Value::Object(changed)));
        ctx.events.emit(
            AppEvent::new("external_settings")
                .with("chatId", chat)
                .with("changes", Value::Array(recorded)),
        );
    }
    let mut out = json!({ "applied": applied });
    if let Some(e) = error {
        if applied.is_empty() {
            return Err(ToolError::refused(e));
        }
        out["error"] = json!(e);
    }
    Ok(out)
}

fn parse_json(s: Option<String>) -> Value {
    s.and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

pub(crate) fn changes_list(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    db.snapshot(|tx| {
        let mut st = tx.prepare(
            "SELECT id, chat_id, key, before, after, at, undone FROM settings_changes ORDER BY id DESC",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(json!({
                    "changeId": r.get::<_, i64>(0)?,
                    "chatId": r.get::<_, String>(1)?,
                    "key": r.get::<_, String>(2)?,
                    "before": parse_json(r.get(3)?),
                    "after": parse_json(r.get(4)?),
                    "at": r.get::<_, i64>(5)?,
                    "undone": r.get::<_, bool>(6)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>(json!({ "changes": rows }))
    })
    .map_err(|e| ApiError::internal(e.to_string()))
}

/// Undo 1–16 changes: all or nothing, and only where the value is still what the chat set.
pub(crate) fn undo(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let ids: Vec<i64> = args
        .get("changeIds")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if ids.is_empty() || ids.len() > MAX_CHANGES {
        return Err(ApiError::bad_args("changeIds takes 1-16 ids."));
    }
    let db = ctx.services.need::<Database>()?;
    let restored = db
        .write(|tx| {
            let mut plan = Vec::new();
            for id in &ids {
                let row = tx.query_row(
                    "SELECT key, before, after, undone FROM settings_changes WHERE id = ?1",
                    [id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, bool>(3)?,
                        ))
                    },
                );
                let Ok((key, before, after, undone)) = row else {
                    return Ok(Err(ApiError::conflict("That change is no longer listed.")));
                };
                if undone {
                    return Ok(Err(ApiError::conflict(format!(
                        "{key} was already undone."
                    ))));
                }
                plan.push((*id, key, parse_json(before), after));
            }
            // Two changes to one setting cannot both be undone to a defined value.
            for (i, (_, k, _, _)) in plan.iter().enumerate() {
                if plan[..i].iter().any(|(_, x, _, _)| x == k) {
                    return Ok(Err(ApiError::bad_args(format!(
                        "Two of these changes are to {k}; undo them one at a time."
                    ))));
                }
            }
            for (_, key, _, after) in &plan {
                if prefs::read(tx, key) != after.as_ref().and_then(|a| serde_json::from_str(a).ok())
                {
                    return Ok(Err(ApiError::conflict(format!(
                        "{key} has changed since; nothing was undone."
                    ))));
                }
            }
            let mut changed = Map::new();
            for (id, key, before, _) in plan {
                prefs::set_raw(tx, &key, &before)
                    .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;
                tx.execute("UPDATE settings_changes SET undone = 1 WHERE id = ?1", [id])?;
                changed.insert(key, before);
            }
            Ok::<_, rusqlite::Error>(Ok(changed))
        })
        .map_err(|e| ApiError::internal(e.to_string()))??;
    ctx.events
        .emit(AppEvent::new("preferences").with("changed", Value::Object(restored.clone())));
    Ok(json!({ "restored": restored }))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("settings_changes_list", OpClass::Blocking, changes_list)
        .blocking("settings_undo", OpClass::Maintenance, undo);
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            "app_get_settings",
            "Read the Helm settings a chat may change, with their current values (null means the default in use). options gives each key's type, allowed values or limits, default and what it does. keys narrows the list; a key chats cannot change is listed under unavailable with where the user changes it.",
            json!({"type": "object", "properties": {"keys": {"type": "array", "items": {"type": "string"}, "maxItems": 64}}, "additionalProperties": false}),
        ),
        get_settings,
    )
    .blocking(
        ToolSpec::write(
            "app_update_settings",
            "Change 1-16 Helm settings live: changes is {key: value}. Keys are applied alphabetically and the call stops at the first failure; the answer has each applied key's before and after. The user sees a notice with Undo. Read app_get_settings first: its options say what each key accepts, and a value outside them is refused with the allowed values. A key's value replaces the stored one whole; null restores the default. Act only when the user explicitly asked for this change, and report what changed. If the reply is lost, check the current values before trying again and never retry automatically.",
            json!({"type": "object", "properties": {"changes": {"type": "object", "minProperties": 1, "maxProperties": 16}}, "required": ["changes"], "additionalProperties": false}),
        )
        .large(),
        update_settings,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::CallOrigin;
    use helm_core::db::MigrationRegistry;
    use helm_ipc::{RecordingSink, Services};
    use std::sync::Arc;

    struct Env {
        _dir: tempfile::TempDir,
        db: Arc<Database>,
        sink: Arc<RecordingSink>,
        services: Arc<Services>,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        reg.initializer("settings-changes", init);
        let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
        let mut s = Services::new();
        s.insert(db.clone());
        Env {
            _dir: dir,
            db,
            sink: Arc::new(RecordingSink::default()),
            services: Arc::new(s),
        }
    }

    fn ctx(e: &Env) -> ToolCtx {
        ToolCtx {
            caller_chat_id: "chat-1".into(),
            is_child: false,
            tool_call_id: None,
            origin: CallOrigin::Agent,
            services: e.services.clone(),
            events: e.sink.clone(),
            ops: None,
        }
    }

    fn op(e: &Env) -> OpCtx {
        OpCtx {
            services: e.services.clone(),
            events: e.sink.clone(),
        }
    }

    #[test]
    fn updates_apply_alphabetically_and_stop_at_the_first_failure() {
        let e = env();
        let r = update_settings(
            &ctx(&e),
            json!({"changes": {"sidebarWidth": 300, "compactSidebar": true, "fastMode": true, "zoomLevel": 1}}),
        )
        .unwrap();
        // compactSidebar, then fastMode fails: sidebarWidth and zoomLevel are not tried.
        assert_eq!(r["applied"].as_object().unwrap().len(), 1);
        assert_eq!(r["applied"]["compactSidebar"]["after"], true);
        assert!(r["error"].as_str().unwrap().contains("Settings > Chat"));
        let w =
            e.db.snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, "sidebarWidth")))
                .unwrap();
        assert_eq!(w, None);
        assert_eq!(e.sink.kinds(), ["preferences", "external_settings"]);
        let ev = e.sink.events.lock().unwrap()[1].clone();
        assert_eq!(ev.fields["chatId"], "chat-1");
        assert_eq!(ev.fields["changes"][0]["key"], "compactSidebar");
    }

    #[test]
    fn validators_and_tiers_apply() {
        let e = env();
        let r = update_settings(&ctx(&e), json!({"changes": {"sidebarWidth": 10}}));
        assert!(r.unwrap_err().public.contains("200 to 4096"));
        let r = update_settings(&ctx(&e), json!({"changes": {"keybindings": {}}}));
        assert!(r.unwrap_err().public.contains("all settings"));
        let r = update_settings(&ctx(&e), json!({"changes": {"responseVerbosity": "high"}}));
        assert!(r.unwrap_err().public.contains("yours to change only"));
        let many: Map<String, Value> = (0..17).map(|i| (format!("k{i}"), json!(1))).collect();
        assert!(update_settings(&ctx(&e), json!({"changes": many})).is_err());
        let g = get_settings(&ctx(&e), json!({"keys": ["theme", "fastMode"]})).unwrap();
        assert!(g["settings"].get("theme").is_some());
        assert!(g["unavailable"]["fastMode"]
            .as_str()
            .unwrap()
            .contains("Settings > Chat"));
    }

    #[test]
    fn get_reports_each_keys_options_and_default() {
        let e = env();
        let g = get_settings(&ctx(&e), json!({"keys": ["sendShortcut", "zoomLevel"]})).unwrap();
        assert_eq!(g["settings"]["sendShortcut"], Value::Null);
        let o = &g["options"]["sendShortcut"];
        assert_eq!(o["allowed"], json!(["enter", "mod-enter"]));
        assert_eq!(o["default"], "mod-enter");
        assert_eq!(g["options"]["zoomLevel"]["max"], 200);
        // Without keys, every visible key carries its options.
        let all = get_settings(&ctx(&e), json!({})).unwrap();
        assert_eq!(
            all["options"].as_object().unwrap().len(),
            all["settings"].as_object().unwrap().len()
        );
    }

    #[test]
    fn a_value_outside_a_keys_options_is_refused_and_not_stored() {
        let e = env();
        let r = update_settings(&ctx(&e), json!({"changes": {"sendShortcut": "ctrl-enter"}}));
        let why = r.unwrap_err().public;
        assert!(why.contains("\"enter\" or \"mod-enter\""), "{why}");
        let stored =
            e.db.snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, "sendShortcut")))
                .unwrap();
        assert_eq!(stored, None);
        assert!(
            e.sink.kinds().is_empty(),
            "a refused change announces nothing"
        );
        // A valid value applies, and null restores the default.
        let r = update_settings(&ctx(&e), json!({"changes": {"sendShortcut": "enter"}})).unwrap();
        assert_eq!(r["applied"]["sendShortcut"]["after"], "enter");
        let r = update_settings(&ctx(&e), json!({"changes": {"sendShortcut": null}})).unwrap();
        assert_eq!(r["applied"]["sendShortcut"]["after"], Value::Null);
    }

    #[test]
    fn undo_is_all_or_nothing_and_only_if_unchanged() {
        let e = env();
        let r = update_settings(
            &ctx(&e),
            json!({"changes": {"compactSidebar": true, "sidebarWidth": 300}}),
        )
        .unwrap();
        let a = r["applied"]["compactSidebar"]["changeId"].as_i64().unwrap();
        let b = r["applied"]["sidebarWidth"]["changeId"].as_i64().unwrap();
        // The user changes one of them since: nothing is undone.
        e.db.write(|tx| {
            prefs::set_raw(tx, "sidebarWidth", &json!(400))
                .map_err(|_| rusqlite::Error::InvalidQuery)
        })
        .unwrap();
        let err = undo(&op(&e), json!({"changeIds": [a, b]})).unwrap_err();
        assert_eq!(err.code, "conflict");
        let read = |k: &str| {
            e.db.snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, k)))
                .unwrap()
        };
        assert_eq!(read("compactSidebar"), Some(json!(true)), "all or nothing");
        // Undo the unchanged one: back to the default (no row).
        let r = undo(&op(&e), json!({"changeIds": [a]})).unwrap();
        assert_eq!(r["restored"]["compactSidebar"], Value::Null);
        assert_eq!(read("compactSidebar"), None);
        assert_eq!(
            undo(&op(&e), json!({"changeIds": [a]})).unwrap_err().code,
            "conflict"
        );
        let list = changes_list(&op(&e), json!({})).unwrap();
        assert_eq!(list["changes"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn undo_refuses_two_changes_to_one_key() {
        let e = env();
        let a = update_settings(&ctx(&e), json!({"changes": {"sidebarWidth": 300}})).unwrap();
        let b = update_settings(&ctx(&e), json!({"changes": {"sidebarWidth": 400}})).unwrap();
        let ids = [
            a["applied"]["sidebarWidth"]["changeId"].as_i64().unwrap(),
            b["applied"]["sidebarWidth"]["changeId"].as_i64().unwrap(),
        ];
        let err = undo(&op(&e), json!({"changeIds": ids})).unwrap_err();
        assert!(err.message.contains("one at a time"), "{}", err.message);
    }

    #[test]
    fn the_ring_keeps_100() {
        let e = env();
        for i in 0..105 {
            update_settings(&ctx(&e), json!({"changes": {"sidebarWidth": 300 + i}})).unwrap();
        }
        let list = changes_list(&op(&e), json!({})).unwrap();
        assert_eq!(list["changes"].as_array().unwrap().len(), 100);
    }
}
