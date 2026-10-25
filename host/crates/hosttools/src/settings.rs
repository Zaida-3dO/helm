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
use crate::settings_allow::{agent_keys, needs_all_refusal, refusal};
use crate::settings_ext::SettingsExtensions;
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

fn extensions(services: &helm_ipc::Services) -> SettingsExtensions {
    services
        .get::<SettingsExtensions>()
        .map(|e| (*e).clone())
        .unwrap_or_default()
}

fn get_settings(ctx: &ToolCtx, a: Value) -> ToolResult {
    let db = db(ctx)?;
    let ext = extensions(&ctx.services);
    db.snapshot(|tx| {
        let all = all_settings(tx);
        let mut visible = agent_keys(all);
        visible.extend(ext.keys().into_iter().filter(|k| {
            all || !ext.owner(k).is_some_and(|x| x.needs_all_settings(k))
        }));
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
            if let Some(x) = ext.owner(k) {
                if x.needs_all_settings(k) && !all {
                    unavailable.insert(k.clone(), json!(needs_all_refusal(k)));
                    continue;
                }
                settings.insert(k.clone(), x.read(tx, k));
                if let Some(o) = x.describe(k) {
                    options.insert(k.clone(), o);
                }
            } else if let Some(why) = refusal(k, all) {
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
            "note": "null means the default is in use: options.<key>.default says what that is, and sending null in app_update_settings restores it. options.<key> gives the type and the allowed values or limits. An option with hasEffect false is stored but nothing in Helm reads it yet, so changing it changes nothing.",
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
    let ext = extensions(&ctx.services);
    let now = helm_core::now_ms();
    let chat = ctx.caller_chat_id.clone();
    // One transaction per key, alphabetically, stopping at the first failure.
    let mut applied = Map::new();
    let mut recorded = Vec::new();
    let mut error: Option<String> = None;
    for k in keys {
        let v = &changes[k];
        let r = db.write(|tx| {
            if serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX) > MAX_VALUE_BYTES {
                return Ok(Err(format!("The value for {k} is too large.")));
            }
            let (before, after) = if let Some(x) = ext.owner(k) {
                if x.needs_all_settings(k) && !all_settings(tx) {
                    return Ok(Err(needs_all_refusal(k)));
                }
                match x.apply(tx, k, v) {
                    Ok((b, a)) => (Some(b), Some(a)),
                    Err(why) => return Ok(Err(why)),
                }
            } else {
                if let Some(why) = refusal(k, all_settings(tx)) {
                    return Ok(Err(why));
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
                let stored = settings_options::to_store(k, before.as_ref(), v);
                if let Err(e) = prefs::set(tx, key, &stored) {
                    return Ok(Err(e.to_string()));
                }
                (before, prefs::read(tx, k))
            };
            // Nothing changed: no ring entry, so no Undo and no notice for it.
            if before == after {
                return Ok(Ok((None, before, after)));
            }
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
            Ok::<_, rusqlite::Error>(Ok((Some(id), before, after)))
        });
        match r {
            Ok(Ok((id, before, after))) => {
                let mut entry = match id {
                    Some(id) => json!({"before": before, "after": after, "changeId": id}),
                    None => json!({"before": before, "after": after, "unchanged": true}),
                };
                if settings_options::is_not_read(k) {
                    entry["note"] = json!("Stored, but nothing in Helm reads this setting yet, so it has no visible effect.");
                }
                applied.insert(k.clone(), entry);
                if let Some(id) = id {
                    recorded
                        .push(json!({"changeId": id, "key": k, "before": before, "after": after}));
                }
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
        let key_of = |c: &Value| c["key"].as_str().unwrap_or_default().to_owned();
        let changed: Map<String, Value> = recorded
            .iter()
            .filter(|c| ext.owner(&key_of(c)).is_none())
            .map(|c| (key_of(c), c["after"].clone()))
            .collect();
        if !changed.is_empty() {
            ctx.events
                .emit(AppEvent::new("preferences").with("changed", Value::Object(changed)));
        }
        for c in &recorded {
            if let Some(x) = ext.owner(&key_of(c)) {
                ctx.events.emit(x.changed_event(&key_of(c)));
            }
        }
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
    let ext = extensions(&ctx.services);
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
                let now = match ext.owner(key) {
                    Some(x) => Some(x.read(tx, key)),
                    None => prefs::read(tx, key),
                };
                if now != after.as_ref().and_then(|a| serde_json::from_str(a).ok()) {
                    return Ok(Err(ApiError::conflict(format!(
                        "{key} has changed since; nothing was undone."
                    ))));
                }
            }
            let mut changed = Map::new();
            for (id, key, before, _) in plan {
                match ext.owner(&key) {
                    Some(x) => x
                        .restore(tx, &key, &before)
                        .map_err(rusqlite::Error::InvalidParameterName)?,
                    None => prefs::set_raw(tx, &key, &before)
                        .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?,
                }
                tx.execute("UPDATE settings_changes SET undone = 1 WHERE id = ?1", [id])?;
                changed.insert(key, before);
            }
            Ok::<_, rusqlite::Error>(Ok(changed))
        })
        .map_err(|e| ApiError::internal(e.to_string()))??;
    let prefs_changed: Map<String, Value> = restored
        .iter()
        .filter(|(k, _)| ext.owner(k).is_none())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !prefs_changed.is_empty() {
        ctx.events
            .emit(AppEvent::new("preferences").with("changed", Value::Object(prefs_changed)));
    }
    for k in restored.keys() {
        if let Some(x) = ext.owner(k) {
            ctx.events.emit(x.changed_event(k));
        }
    }
    Ok(json!({ "restored": restored }))
}

/// `settings_all_set {enabled}`: the user's switch for "Chats can change all settings". It is
/// its own op, not a generic preference write, so the one switch that widens what a chat may
/// change cannot be reached through the settings tools.
pub(crate) fn all_settings_set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let enabled = args
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| ApiError::bad_args("enabled must be true or false."))?;
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| {
        prefs::set_raw(tx, "agentSettingsAll", &json!(enabled))
            .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))
    })
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ctx.events
        .emit(AppEvent::new("preferences").with("changed", json!({ "agentSettingsAll": enabled })));
    Ok(Value::Null)
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("settings_changes_list", OpClass::Blocking, changes_list)
        .blocking("settings_undo", OpClass::Maintenance, undo)
        .blocking("settings_all_set", OpClass::Maintenance, all_settings_set);
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
            "Change 1-16 Helm settings live: changes is {key: value}. Keys are applied alphabetically and the call stops at the first failure; the answer has each applied key's before and after. The user sees a notice with Undo. Read app_get_settings first: its options say what each key accepts, and a value outside them is refused with the allowed values. A key's value replaces the stored one whole, except fontSizes, sounds and orchestrator, where you send only the fields to change and the others keep their values; null restores the default. An option with hasEffect false is read by nothing yet, so changing it does nothing visible. The orchestrator key is Settings > Orchestrator: options.orchestrator says which of its fields are yours to change, and the rest are the user's alone and are refused. Act only when the user explicitly asked for this change, and report what changed. If the reply is lost, check the current values before trying again and never retry automatically.",
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
        assert!(r["error"].as_str().unwrap().contains("Settings > Runtime"));
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
    fn the_all_settings_switch_is_the_users_op_and_opens_the_all_tier() {
        let e = env();
        let blocked = update_settings(&ctx(&e), json!({"changes": {"keybindings": {}}}));
        assert!(blocked.unwrap_err().public.contains("all settings"));
        assert!(all_settings_set(&op(&e), json!({"enabled": "yes"})).is_err());
        all_settings_set(&op(&e), json!({"enabled": true})).unwrap();
        assert_eq!(e.sink.kinds(), ["preferences"]);
        assert!(update_settings(&ctx(&e), json!({"changes": {"keybindings": {}}})).is_ok());
        // The generic preference writer still refuses it.
        assert!(e
            .db
            .write(|tx| prefs::set_generic(tx, "agentSettingsAll", &json!(false)))
            .is_err());
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
            .contains("Settings > Runtime"));
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
    fn font_sizes_and_sounds_merge_field_by_field() {
        let e = env();
        let read = |k: &str| {
            e.db.snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, k)))
                .unwrap()
        };
        // Nothing stored: the default fills in what the update leaves out.
        let r = update_settings(
            &ctx(&e),
            json!({"changes": {"fontSizes": {"interface": 15}}}),
        )
        .unwrap();
        assert_eq!(
            r["applied"]["fontSizes"]["after"],
            json!({"response": 14, "review": 10, "interface": 15})
        );
        update_settings(
            &ctx(&e),
            json!({"changes": {"fontSizes": {"response": 18}}}),
        )
        .unwrap();
        assert_eq!(
            read("fontSizes"),
            Some(json!({"response": 18, "review": 10, "interface": 15}))
        );
        update_settings(&ctx(&e), json!({"changes": {"sounds": {"volume": 30}}})).unwrap();
        assert_eq!(
            read("sounds"),
            Some(json!({"enabled": true, "volume": 30, "themed": true}))
        );
        // A wrong field is still refused, and the stored value is untouched.
        assert!(update_settings(&ctx(&e), json!({"changes": {"sounds": {"loud": 1}}})).is_err());
        assert_eq!(read("sounds").unwrap()["volume"], 30);
        // Null restores the default.
        update_settings(&ctx(&e), json!({"changes": {"fontSizes": null}})).unwrap();
        assert_eq!(read("fontSizes"), None);
    }

    #[test]
    fn hidden_skills_with_a_leading_dollar_are_refused_on_write() {
        let e = env();
        let r = update_settings(
            &ctx(&e),
            json!({"changes": {"hiddenSkills": ["review", "$deploy"]}}),
        );
        let why = r.unwrap_err().public;
        assert!(
            why.contains("hiddenSkills[1] must not start with $"),
            "{why}"
        );
        let stored =
            e.db.snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, "hiddenSkills")))
                .unwrap();
        assert_eq!(stored, None);
        let r =
            update_settings(&ctx(&e), json!({"changes": {"hiddenSkills": ["review"]}})).unwrap();
        assert_eq!(r["applied"]["hiddenSkills"]["after"], json!(["review"]));
        // A key something reads carries no warning; one nothing reads yet says so.
        assert!(r["applied"]["hiddenSkills"].get("note").is_none());
        let g = get_settings(&ctx(&e), json!({"keys": ["hiddenSkills", "cursorTrail"]})).unwrap();
        assert!(g["options"]["hiddenSkills"].get("hasEffect").is_none());
        assert_eq!(g["options"]["cursorTrail"]["hasEffect"], false);
        let r = update_settings(&ctx(&e), json!({"changes": {"cursorTrail": true}})).unwrap();
        assert!(r["applied"]["cursorTrail"]["note"]
            .as_str()
            .unwrap()
            .contains("nothing in Helm reads"));
    }

    /// A stand-in extension: one key holding `{open, level}`; chats may change only `level`.
    struct Fake(bool);
    impl crate::SettingsExtension for Fake {
        fn needs_all_settings(&self, _key: &str) -> bool {
            self.0
        }
        fn keys(&self) -> Vec<&'static str> {
            vec!["fake"]
        }
        fn describe(&self, key: &str) -> Option<Value> {
            (key == "fake")
                .then(|| json!({"type": "object", "default": {"open": false, "level": 1}}))
        }
        fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
            prefs::read(tx, "desktopPet")
                .and_then(|v| v.get("fake").cloned())
                .unwrap_or_else(|| json!({"open": false, "level": 1}))
        }
        fn apply(
            &self,
            tx: &Transaction<'_>,
            key: &str,
            value: &Value,
        ) -> Result<(Value, Value), String> {
            let before = self.read(tx, key);
            if value.get("open").is_some() {
                return Err("fake.open is yours to change only.".into());
            }
            let mut after = before.clone();
            after["level"] = value["level"].clone();
            prefs::set_raw(tx, "desktopPet", &json!({"fake": after})).unwrap();
            Ok((before, after))
        }
        fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
            prefs::set_raw(tx, "desktopPet", &json!({"fake": value})).map_err(|e| e.to_string())
        }
        fn changed_event(&self, _key: &str) -> AppEvent {
            AppEvent::new("fake_changed")
        }
    }

    #[test]
    fn an_extension_key_is_listed_changed_recorded_and_undone() {
        let e = env();
        let mut s = Services::new();
        s.insert(e.db.clone());
        s.insert(Arc::new(crate::SettingsExtensions(vec![Arc::new(Fake(
            false,
        ))])));
        let mut c = ctx(&e);
        c.services = Arc::new(s);
        let g = get_settings(&c, json!({})).unwrap();
        assert_eq!(g["settings"]["fake"], json!({"open": false, "level": 1}));
        assert_eq!(g["options"]["fake"]["default"]["level"], 1);
        assert_eq!(
            g["options"].as_object().unwrap().len(),
            g["settings"].as_object().unwrap().len()
        );
        // The extension refuses what is not the chat's; nothing is recorded or announced.
        let r = update_settings(&c, json!({"changes": {"fake": {"open": true}}}));
        assert!(r.unwrap_err().public.contains("yours to change only"));
        assert!(e.sink.kinds().is_empty());
        let r = update_settings(&c, json!({"changes": {"fake": {"level": 3}}})).unwrap();
        assert_eq!(r["applied"]["fake"]["before"]["level"], 1);
        assert_eq!(r["applied"]["fake"]["after"]["level"], 3);
        // Its own event, not a preference change.
        assert_eq!(e.sink.kinds(), ["fake_changed", "external_settings"]);
        let id = r["applied"]["fake"]["changeId"].as_i64().unwrap();
        let mut o = op(&e);
        o.services = c.services.clone();
        let u = undo(&o, json!({"changeIds": [id]})).unwrap();
        assert_eq!(u["restored"]["fake"]["level"], 1);
        let g = get_settings(&c, json!({"keys": ["fake"]})).unwrap();
        assert_eq!(g["settings"]["fake"]["level"], 1);
        assert_eq!(
            e.sink.kinds().last().map(String::as_str),
            Some("fake_changed")
        );
    }

    fn with_fake(e: &Env, all_tier: bool) -> ToolCtx {
        let mut s = Services::new();
        s.insert(e.db.clone());
        s.insert(Arc::new(crate::SettingsExtensions(vec![Arc::new(Fake(
            all_tier,
        ))])));
        let mut c = ctx(e);
        c.services = Arc::new(s);
        c
    }

    #[test]
    fn an_all_tier_extension_key_obeys_the_all_settings_switch() {
        let e = env();
        let c = with_fake(&e, true);
        // Switch off: not listed, refused to read and to change, like keybindings.
        let g = get_settings(&c, json!({})).unwrap();
        assert!(g["settings"].get("fake").is_none());
        let g = get_settings(&c, json!({"keys": ["fake", "keybindings"]})).unwrap();
        assert!(g["unavailable"]["fake"]
            .as_str()
            .unwrap()
            .contains("all settings"));
        assert_eq!(
            g["unavailable"]["fake"],
            g["unavailable"]["keybindings"]
                .as_str()
                .unwrap()
                .replace("keybindings", "fake")
        );
        let r = update_settings(&c, json!({"changes": {"fake": {"level": 3}}}));
        assert!(r.unwrap_err().public.contains("all settings"));
        assert!(e.sink.kinds().is_empty());
        // Switch on: listed, readable and changeable.
        e.db.write(|tx| {
            prefs::set_raw(tx, "agentSettingsAll", &json!(true))
                .map_err(|_| rusqlite::Error::InvalidQuery)
        })
        .unwrap();
        let g = get_settings(&c, json!({})).unwrap();
        assert_eq!(g["settings"]["fake"]["level"], 1);
        let r = update_settings(&c, json!({"changes": {"fake": {"level": 3}}})).unwrap();
        assert_eq!(r["applied"]["fake"]["after"]["level"], 3);
        // A base-tier extension key never needed the switch.
        let e2 = env();
        let c2 = with_fake(&e2, false);
        assert!(update_settings(&c2, json!({"changes": {"fake": {"level": 2}}})).is_ok());
    }

    #[test]
    fn an_update_that_changes_nothing_writes_no_ring_entry_and_no_notice() {
        let e = env();
        update_settings(&ctx(&e), json!({"changes": {"sidebarWidth": 300}})).unwrap();
        e.sink.events.lock().unwrap().clear();
        // The same value again, and null on a key with nothing stored.
        let r = update_settings(
            &ctx(&e),
            json!({"changes": {"sidebarWidth": 300, "compactSidebar": null}}),
        )
        .unwrap();
        assert_eq!(r["applied"]["sidebarWidth"]["unchanged"], true);
        assert!(r["applied"]["sidebarWidth"].get("changeId").is_none());
        assert_eq!(r["applied"]["compactSidebar"]["unchanged"], true);
        assert!(e.sink.kinds().is_empty(), "no notice");
        let list = changes_list(&op(&e), json!({})).unwrap();
        assert_eq!(list["changes"].as_array().unwrap().len(), 1);
        // A merge that sends the stored sub-values is a no-op too.
        update_settings(
            &ctx(&e),
            json!({"changes": {"fontSizes": {"interface": 15}}}),
        )
        .unwrap();
        let before = changes_list(&op(&e), json!({})).unwrap()["changes"]
            .as_array()
            .unwrap()
            .len();
        let r = update_settings(
            &ctx(&e),
            json!({"changes": {"fontSizes": {"interface": 15}}}),
        )
        .unwrap();
        assert_eq!(r["applied"]["fontSizes"]["unchanged"], true);
        let after = changes_list(&op(&e), json!({})).unwrap()["changes"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(before, after);
        // A real change after a no-op still records.
        let r = update_settings(&ctx(&e), json!({"changes": {"sidebarWidth": 310}})).unwrap();
        assert!(r["applied"]["sidebarWidth"]["changeId"].is_i64());
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
