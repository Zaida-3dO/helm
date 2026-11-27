//! The settings tools, the change ring and Undo.
//!
//! - `app_get_settings` reads every setting a chat can reach, the directly changeable ones and
//!   the ones it stages; `app_update_settings` changes 1-16 of them, applied alphabetically
//!   through the same validators as the UI and stopping at the first failure. A setting that
//!   needs approval, or the part of one that does, changes nothing and answers with the call to
//!   make instead (`app_stage_settings`): see `settings_allow::route`. Each applied change goes
//!   into `settings_changes` (a 100-entry ring) and the UI gets `preferences` plus
//!   `external_settings {chatId, changes}`, which shows the 8 s toast with Undo and Open chat.
//! - Ops `settings_changes_list` and `settings_undo {changeIds[1-16]}`: all or nothing, and only
//!   if every value is still what the chat set.
//! - Op `settings_access_list`: every setting's access (`direct` or `approval`) with the reason an
//!   approval one carries, and the parts of a direct extension key that need approval. The
//!   Settings pages read it to say which settings need the user's approval.

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::settings_allow::{
    access, approval_pref_keys, direct_message, direct_pref_keys, not_a_setting, reason, route,
    stage_message, Access, Route, CREDENTIALS, CREDENTIALS_KEY, NAMED,
};
use crate::settings_ext::{PrefRules, SettingsExtension, SettingsExtensions, StagedPart};
use crate::settings_options;
use crate::spec::ToolSpec;
use helm_core::db::Database;
use helm_core::prefs::classes::{PrefClass, CLASSIFIED};
use helm_core::prefs::{self, PrefKey};
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use rusqlite::{params, Transaction};
use serde_json::{json, Map, Value};
use std::sync::Arc;

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

/// The entry of `customColours` this device is showing: the active profile's id, or "default"
/// while none is chosen (or all of them are shown). The interface reads the same rule.
fn colour_profile(tx: &rusqlite::Connection) -> String {
    match prefs::read(tx, "activeProfile") {
        Some(Value::String(id)) if !id.is_empty() && id != "all" && id.len() <= 64 => id,
        _ => settings_options::DEFAULT_PROFILE.to_owned(),
    }
}

/// The profile whose colours a chat changes: the one the chat belongs to (the home profile when
/// it has none), else the one the sidebar shows. The profiles crate depends on this one, so its
/// tables are read directly.
fn chat_colour_profile(tx: &rusqlite::Connection, chat_id: &str) -> String {
    tx.query_row(
        "SELECT COALESCE(
             (SELECT p.id FROM profiles p
               WHERE p.id = (SELECT NULLIF(c.profile_id, '') FROM chats c WHERE c.id = ?1)),
             (SELECT id FROM profiles WHERE is_home = 1))",
        [chat_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .filter(|id| !id.is_empty() && id.len() <= 64)
    .unwrap_or_else(|| colour_profile(tx))
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

/// The extension for the person who owns `chat` when the setting is theirs alone, else `x`.
fn bound(
    tx: &Transaction<'_>,
    x: &Arc<dyn SettingsExtension>,
    chat: &str,
) -> Arc<dyn SettingsExtension> {
    x.for_chat(tx, chat).unwrap_or_else(|| x.clone())
}

/// The value at a dotted path, if there is one.
fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, k| v.get(k))
}

/// The staged part of `key` that `patch` would change: a field a chat stages rather than changes,
/// named by the patch at a value other than the one it has now. Repeating a value is no change.
fn touched_part(
    tx: &Transaction<'_>,
    x: &dyn SettingsExtension,
    key: &str,
    patch: &Value,
) -> Option<StagedPart> {
    if !patch.is_object() {
        return None;
    }
    let now = x.read(tx, key);
    x.staged_parts(key)
        .iter()
        .find(|p| at(patch, p.path).is_some_and(|new| Some(new) != at(&now, p.path)))
        .copied()
}

/// What `app_get_settings` says about `credentials`: it holds no value, only the pages a chat can
/// ask the user to open to type a secret.
fn credentials_options() -> Value {
    let pages: Vec<Value> = CREDENTIALS
        .iter()
        .map(|(id, title, what)| json!({"for": id, "page": title, "holds": what}))
        .collect();
    json!({
        "type": "object",
        "fields": {"for": {"type": "enum", "allowed": CREDENTIALS.iter().map(|(id, _, _)| *id).collect::<Vec<_>>()}},
        "pages": pages,
        "about": "Not a value: a secret must never pass through a chat. Stage {for: <page>} with app_stage_settings and, on Approve, Helm opens that page of Settings so the user types the secret there. Nothing is stored and nothing is sent back to you.",
    })
}

fn get_settings(ctx: &ToolCtx, a: Value) -> ToolResult {
    let db = db(ctx)?;
    let ext = extensions(&ctx.services);
    db.snapshot(|tx| {
        let mut visible = direct_pref_keys();
        visible.extend(approval_pref_keys());
        visible.extend(ext.keys());
        visible.push(CREDENTIALS_KEY);
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
        let mut details = Map::new();
        for k in &wanted {
            let Some(acc) = access(k) else {
                unavailable.insert(k.clone(), json!(not_a_setting(k)));
                continue;
            };
            if k == CREDENTIALS_KEY {
                settings.insert(k.clone(), Value::Null);
                options.insert(k.clone(), credentials_options());
            } else if let Some(owner) = ext.owner(k) {
                let x = bound(tx, owner, &ctx.caller_chat_id);
                settings.insert(k.clone(), x.read(tx, k));
                if let Some(o) = x.describe(k) {
                    options.insert(k.clone(), o);
                }
                if let Some(d) = x.details(tx, k) {
                    details.insert(k.clone(), d);
                }
                let parts = x.staged_parts(k);
                if !parts.is_empty() {
                    let list: Vec<Value> = parts
                        .iter()
                        .map(|p| json!({"path": p.path, "why": p.reason}))
                        .collect();
                    if let Some(o) = options
                        .entry(k.clone())
                        .or_insert_with(|| json!({}))
                        .as_object_mut()
                    {
                        o.insert("needsApprovalParts".into(), Value::Array(list));
                    }
                }
            } else {
                settings.insert(k.clone(), prefs::read(tx, k).unwrap_or(Value::Null));
                if let Some(o) = settings_options::describe(k) {
                    options.insert(k.clone(), o);
                }
            }
            if let Access::Approval(why) = acc {
                if let Some(o) = options
                    .entry(k.clone())
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                {
                    o.insert("needsApproval".into(), json!(true));
                    o.insert("approvalReason".into(), json!(why));
                }
            }
        }
        let mut out = json!({
            "settings": settings,
            "options": options,
            "unavailable": unavailable,
            "note": "null means the default is in use: options.<key>.default says what that is, and sending null in app_update_settings restores it. options.<key> gives the type and the allowed values or limits. An option with hasEffect false is stored but nothing in Helm reads it yet, so changing it changes nothing. details.<key> holds read-only facts about a key. A key whose options say needsApproval (or a part of one listed under needsApprovalParts) is read here but changed only with app_stage_settings: the user sees one Approve card and Helm applies the change on their click. unavailable lists names that are not settings at all.",
        });
        if !details.is_empty() {
            out["details"] = Value::Object(details);
        }
        Ok::<_, rusqlite::Error>(out)
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
    let rules = rules(&ctx.services);
    let now = helm_core::now_ms();
    let chat = ctx.caller_chat_id.clone();
    // One transaction per key, alphabetically, stopping at the first failure.
    let mut applied = Map::new();
    let mut recorded = Vec::new();
    let mut error: Option<String> = None;
    for k in keys {
        let v = &changes[k];
        let r = db.write(|tx| {
            let (before, after) = match change_key(tx, &ext, &rules, &chat, k, v, By::Chat) {
                Ok(c) => c,
                Err(why) => return Ok(Err(why)),
            };
            // Nothing changed: no ring entry, so no Undo and no notice for it.
            if before == after {
                return Ok(Ok((None, before, after)));
            }
            let id = insert_change(tx, &chat, k, before.as_ref(), after.as_ref(), now)?;
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
                x.after_commit(&key_of(c));
                ctx.events.emit(x.changed_event(&key_of(c)));
            } else if let Some(rule) = rules.for_key(&key_of(c)) {
                rule.after_commit(Some(&c["after"]).filter(|v| !v.is_null()));
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

/// Who is changing a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    /// A chat, with `app_update_settings`: it applies what is Direct and is told how to stage the
    /// rest.
    Chat,
    /// Helm applying what the user approved on a card.
    Approved,
    /// Checking what the user would be asked to approve, in a transaction that is rolled back.
    Check,
}

/// A setting's value before and after a change; `None` is the default in use.
type Change = (Option<Value>, Option<Value>);

/// Check one change against the setting's access and its own validators, and store it in `tx`.
/// `app_update_settings` and an approved change both go through here, so a staged value passes the
/// same checks a chat's own would. The extension's `apply` never sees who is asking: whether a
/// change goes ahead or is staged is decided here, once, by `settings_allow::route`.
fn change_key(
    tx: &Transaction<'_>,
    ext: &SettingsExtensions,
    rules: &PrefRules,
    chat: &str,
    key: &str,
    v: &Value,
    by: By,
) -> Result<Change, String> {
    if serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX) > MAX_VALUE_BYTES {
        return Err(format!("The value for {key} is too large."));
    }
    // A label or shortcut that hides characters can read as something it is not.
    if let Some(at) = helm_core::hidden::find_hidden(v) {
        return Err(hidden_refusal(key, &at));
    }
    let acc = access(key).ok_or_else(|| not_a_setting(key))?;
    let owner = ext.owner(key).map(|x| bound(tx, x, chat));
    match by {
        By::Chat => {
            let touched = owner
                .as_ref()
                .and_then(|x| touched_part(tx, x.as_ref(), key, v));
            if let Route::Stage { part, reason } = route(acc, touched.as_ref()) {
                return Err(stage_message(key, part, reason));
            }
        }
        By::Approved | By::Check => {
            // Only what a chat would have to stage can be staged; the rest it changes itself.
            let has_parts = owner
                .as_ref()
                .is_some_and(|x| !x.staged_parts(key).is_empty());
            if !matches!(acc, Access::Approval(_)) && !has_parts {
                return Err(direct_message(key));
            }
        }
    }
    if key == CREDENTIALS_KEY {
        return Err(format!(
            "{key} stores nothing: stage {{\"{key}\": {{\"for\": <one of {}>}}}} with app_stage_settings and Approve opens that page.",
            CREDENTIALS.iter().map(|(id, _, _)| *id).collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(x) = owner {
        let (b, a) = match by {
            By::Check => x.check_approved(tx, key, v)?,
            By::Chat | By::Approved => x.apply(tx, key, v)?,
        };
        return Ok((Some(b), Some(a)));
    }
    settings_options::check(key, v)?;
    let pref = CLASSIFIED
        .iter()
        .find(|(k, c)| *k == key && matches!(c, PrefClass::Setting(_)))
        .map(|(k, _)| PrefKey::classified(k))
        .ok_or_else(|| not_a_setting(key))?;
    let before = prefs::read(tx, key);
    let mut stored = if key == settings_options::COLOURS_KEY {
        settings_options::merge_colours(before.as_ref(), v, &chat_colour_profile(tx, chat))
    } else {
        settings_options::to_store(key, before.as_ref(), v)
    };
    if !stored.is_null() {
        if let Some(rule) = rules.for_key(key) {
            stored = rule.prepare(tx, before.as_ref(), &stored)?;
        }
    }
    prefs::set(tx, pref, &stored).map_err(|e| e.to_string())?;
    Ok((before, prefs::read(tx, key)))
}

fn rules(services: &helm_ipc::Services) -> PrefRules {
    services
        .get::<PrefRules>()
        .map(|r| (*r).clone())
        .unwrap_or_default()
}

/// What an approved change did to one setting.
#[derive(Clone, Debug, PartialEq)]
pub struct Approved {
    pub key: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    /// The entry in the change list, where the user's Undo is; none when nothing changed.
    pub change_id: Option<i64>,
}

/// Apply approved changes (settings that need approval) together: all or nothing, each recorded in the change
/// list for the user's Undo (a chat never gets it), then announced. With `commit` false nothing
/// is kept: it answers what each setting would become, which is how a staged change is checked
/// up front, with the same validators.
pub fn apply_approved(
    services: &helm_ipc::Services,
    events: &dyn helm_ipc::EventSink,
    chat: &str,
    changes: &[(String, Value)],
    commit: bool,
) -> Result<Vec<Approved>, String> {
    let db = services
        .get::<Database>()
        .ok_or_else(|| "Settings are not available here.".to_owned())?;
    let ext = extensions(services);
    let rules = rules(services);
    let now = helm_core::now_ms();
    let mut done: Vec<Approved> = Vec::new();
    let mut refused: Option<String> = None;
    let r = db.write(|tx| {
        done.clear();
        for (key, v) in changes {
            let by = if commit { By::Approved } else { By::Check };
            let (before, after) = match change_key(tx, &ext, &rules, chat, key, v, by) {
                Ok(c) => c,
                Err(why) => {
                    // The error rolls back what the earlier keys stored.
                    refused = Some(why);
                    return Err(rusqlite::Error::StatementChangedRows(1));
                }
            };
            let change_id = if before == after {
                None
            } else {
                Some(insert_change(
                    tx,
                    chat,
                    key,
                    before.as_ref(),
                    after.as_ref(),
                    now,
                )?)
            };
            done.push(Approved {
                key: key.clone(),
                before,
                after,
                change_id,
            });
        }
        if commit {
            Ok(())
        } else {
            // Not kept: this error rolls the whole transaction back.
            Err(rusqlite::Error::StatementChangedRows(0))
        }
    });
    match r {
        Ok(()) => {}
        Err(_) if refused.is_some() => return Err(refused.unwrap_or_default()),
        Err(rusqlite::Error::StatementChangedRows(0)) if !commit => return Ok(done),
        Err(e) => return Err(format!("The settings could not be saved: {e}")),
    }
    announce(events, &ext, &rules, chat, &done);
    Ok(done)
}

/// Tell the interface and the owners of the keys that changed, once the change is committed.
fn announce(
    events: &dyn helm_ipc::EventSink,
    ext: &SettingsExtensions,
    rules: &PrefRules,
    chat: &str,
    done: &[Approved],
) {
    let changed: Vec<&Approved> = done.iter().filter(|a| a.change_id.is_some()).collect();
    let prefs_changed: Map<String, Value> = changed
        .iter()
        .filter(|a| ext.owner(&a.key).is_none())
        .map(|a| (a.key.clone(), a.after.clone().unwrap_or(Value::Null)))
        .collect();
    if !prefs_changed.is_empty() {
        events.emit(AppEvent::new("preferences").with("changed", Value::Object(prefs_changed)));
    }
    for a in &changed {
        if let Some(x) = ext.owner(&a.key) {
            x.after_commit(&a.key);
            events.emit(x.changed_event(&a.key));
        } else if let Some(rule) = rules.for_key(&a.key) {
            rule.after_commit(a.after.as_ref());
        }
    }
    if !changed.is_empty() {
        let list: Vec<Value> = changed
            .iter()
            .map(|a| {
                json!({"changeId": a.change_id, "key": a.key, "before": a.before, "after": a.after})
            })
            .collect();
        events.emit(
            AppEvent::new("external_settings")
                .with("chatId", chat.to_owned())
                .with("changes", Value::Array(list)),
        );
    }
}

/// Why a value is refused for holding an invisible or direction-changing character.
fn hidden_refusal(key: &str, at: &str) -> String {
    let place = match at {
        "" => String::new(),
        a if a.starts_with('[') => format!(" (at {key}{a})"),
        a => format!(" (at {key}.{a})"),
    };
    format!("{key} cannot hold invisible or direction-changing characters{place}.")
}

/// Write one change into the ring and trim it; answers the change id.
fn insert_change(
    tx: &Transaction<'_>,
    chat: &str,
    key: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    now: i64,
) -> rusqlite::Result<i64> {
    tx.execute(
        "INSERT INTO settings_changes(chat_id, key, before, after, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            chat,
            key,
            before.map(Value::to_string),
            after.map(Value::to_string),
            now
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "DELETE FROM settings_changes WHERE id NOT IN
             (SELECT id FROM settings_changes ORDER BY id DESC LIMIT ?1)",
        [RING],
    )?;
    Ok(id)
}

/// For a tool that changes a setting-extension key itself (rather than through
/// `app_update_settings`): record the change so Settings lists it with Undo, and show the user
/// the notice with Undo. Answers the change id.
pub fn record_external_change(
    ctx: &ToolCtx,
    key: &str,
    before: Value,
    after: Value,
) -> Result<i64, ToolError> {
    let db = db(ctx)?;
    let chat = ctx.caller_chat_id.clone();
    let now = helm_core::now_ms();
    let id = db
        .write(|tx| insert_change(tx, &chat, key, Some(&before), Some(&after), now))
        .map_err(sql)?;
    ctx.events.emit(
        AppEvent::new("external_settings")
            .with("chatId", chat)
            .with(
                "changes",
                json!([{"changeId": id, "key": key, "before": before, "after": after}]),
            ),
    );
    Ok(id)
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
                    "SELECT key, before, after, undone, chat_id FROM settings_changes WHERE id = ?1",
                    [id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, bool>(3)?,
                            r.get::<_, String>(4)?,
                        ))
                    },
                );
                let Ok((key, before, after, undone, chat)) = row else {
                    return Ok(Err(ApiError::conflict("That change is no longer listed.")));
                };
                if undone {
                    return Ok(Err(ApiError::conflict(format!(
                        "{key} was already undone."
                    ))));
                }
                plan.push((*id, key, parse_json(before), after, chat));
            }
            // Two changes to one setting cannot both be undone to a defined value.
            for (i, (_, k, _, _, _)) in plan.iter().enumerate() {
                if plan[..i].iter().any(|(_, x, _, _, _)| x == k) {
                    return Ok(Err(ApiError::bad_args(format!(
                        "Two of these changes are to {k}; undo them one at a time."
                    ))));
                }
            }
            for (_, key, _, after, chat) in &plan {
                let now = match ext.owner(key) {
                    Some(x) => Some(bound(tx, x, chat).read(tx, key)),
                    None => prefs::read(tx, key),
                };
                if now != after.as_ref().and_then(|a| serde_json::from_str(a).ok()) {
                    return Ok(Err(ApiError::conflict(format!(
                        "{key} has changed since; nothing was undone."
                    ))));
                }
            }
            let mut changed = Map::new();
            for (id, key, before, _, chat) in plan {
                match ext.owner(&key) {
                    Some(x) => bound(tx, x, &chat)
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
    let rules = rules(&ctx.services);
    for (k, v) in &restored {
        if let Some(x) = ext.owner(k) {
            x.after_commit(k);
            ctx.events.emit(x.changed_event(k));
        } else if let Some(rule) = rules.for_key(k) {
            rule.after_commit(Some(v).filter(|v| !v.is_null()));
        }
    }
    Ok(json!({ "restored": restored }))
}

/// `settings_all_set {enabled}`: the user's switch for "Chats can change all settings". It is
/// its own op, not a generic preference write. A chat reaches the same switch by staging
/// `agentSettingsAll`, which the user approves.
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

/// `settings_access_list`: every setting's access, for the pages that show which settings need
/// approval, and the parts of a direct extension key that do.
pub(crate) fn access_list(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let name = |a: Access| match a {
        Access::Direct => "direct",
        Access::Approval(_) => "approval",
    };
    let ext = extensions(&ctx.services);
    let settings: Vec<Value> = CLASSIFIED
        .iter()
        .map(|(k, _)| *k)
        .chain(NAMED.iter().map(|(k, _)| *k))
        .filter_map(|k| {
            let a = access(k)?;
            Some(json!({"key": k, "access": name(a), "why": reason(k)}))
        })
        .collect();
    let parts: Vec<Value> = ext
        .0
        .iter()
        .flat_map(|x| {
            x.keys().into_iter().flat_map(|k| {
                x.staged_parts(k)
                    .iter()
                    .map(|p| json!({"key": k, "part": p.path, "why": p.reason}))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    Ok(json!({ "settings": settings, "stagedParts": parts }))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("settings_access_list", OpClass::Blocking, access_list)
        .blocking("settings_changes_list", OpClass::Blocking, changes_list)
        .blocking("settings_undo", OpClass::Maintenance, undo)
        .blocking("settings_all_set", OpClass::Maintenance, all_settings_set);
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            "app_get_settings",
            "Read every Helm setting a chat can reach, with its current value (null means the default in use). options gives each key's type, allowed values or limits, default and what it does. A setting is either one you change yourself (app_update_settings) or one whose options say needsApproval (you stage it with app_stage_settings); there is no setting you are simply refused. keys narrows the list; a name that is no setting at all is listed under unavailable.",
            json!({"type": "object", "properties": {"keys": {"type": "array", "items": {"type": "string"}, "maxItems": 64}}, "additionalProperties": false}),
        ),
        get_settings,
    )
    .blocking(
        ToolSpec::write(
            "app_update_settings",
            "Change 1-16 Helm settings live: changes is {key: value}. Keys are applied alphabetically and the call stops at the first failure; the answer has each applied key's before and after. The user sees a notice with Undo. Read app_get_settings first: its options say what each key accepts, and a value outside them is refused with the allowed values. A key's value replaces the stored one whole, except fontSizes, sounds and orchestrator, where you send only the fields to change and the others keep their values; null restores the default. An option with hasEffect false is read by nothing yet, so changing it does nothing visible. Send the orchestrator key's changes the same way: only the fields to change. A key whose options say needsApproval, or a part of one listed under needsApprovalParts, is not changed here and nothing is applied: the answer is the exact app_stage_settings call to make, and the user approves it on one card. Act only when the user explicitly asked for this change, and report what changed. If the reply is lost, check the current values before trying again and never retry automatically.",
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
            json!({"changes": {"sidebarWidth": 300, "compactSidebar": true, "newChatPermissionMode": "default", "zoomLevel": 1}}),
        )
        .unwrap();
        // compactSidebar, then newChatPermissionMode needs approval: sidebarWidth and zoomLevel are
        // not tried.
        assert_eq!(r["applied"].as_object().unwrap().len(), 1);
        assert_eq!(r["applied"]["compactSidebar"]["after"], true);
        assert!(r["error"].as_str().unwrap().contains("app_stage_settings"));
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
    fn the_all_settings_switch_is_its_own_op_and_a_chat_stages_it() {
        let e = env();
        // Shortcuts are a chat's own to change; the switch no longer holds them back.
        assert!(update_settings(&ctx(&e), json!({"changes": {"keybindings": {}}})).is_ok());
        assert!(all_settings_set(&op(&e), json!({"enabled": "yes"})).is_err());
        let before = e.sink.kinds().len();
        all_settings_set(&op(&e), json!({"enabled": true})).unwrap();
        assert_eq!(e.sink.kinds().len(), before + 1);
        // A chat reaches the switch by staging it, and is told so.
        let why = update_settings(&ctx(&e), json!({"changes": {"agentSettingsAll": false}}))
            .unwrap_err()
            .public;
        assert!(why.contains("app_stage_settings") && why.contains("agentSettingsAll"), "{why}");
        // The generic preference writer still refuses it.
        assert!(e
            .db
            .write(|tx| prefs::set_generic(tx, "agentSettingsAll", &json!(false)))
            .is_err());
    }

    #[test]
    fn validators_apply_and_what_needs_approval_says_how_to_stage() {
        let e = env();
        let r = update_settings(&ctx(&e), json!({"changes": {"sidebarWidth": 10}}));
        assert!(r.unwrap_err().public.contains("200 to 4096"));
        let many: Map<String, Value> = (0..17).map(|i| (format!("k{i}"), json!(1))).collect();
        assert!(update_settings(&ctx(&e), json!({"changes": many})).is_err());
        let g = get_settings(&ctx(&e), json!({"keys": ["theme", "agentSettingsAll", "selection"]}))
            .unwrap();
        assert!(g["settings"].get("theme").is_some());
        assert_eq!(g["options"]["agentSettingsAll"]["needsApproval"], true);
        assert!(g["options"]["agentSettingsAll"]["approvalReason"].is_string());
        // A name that is no setting is said so, not blamed on the user.
        assert!(g["unavailable"]["selection"]
            .as_str()
            .unwrap()
            .contains("not a Helm setting"));
        // A secret never rides in a value: credentials only names the page to open.
        assert_eq!(g["settings"]["agentSettingsAll"], Value::Null);
        let c = get_settings(&ctx(&e), json!({"keys": ["credentials"]})).unwrap();
        assert_eq!(c["options"]["credentials"]["needsApproval"], true);
        assert!(c["options"]["credentials"]["fields"]["for"]["allowed"]
            .as_array()
            .unwrap()
            .contains(&json!("mcp")));
    }

    /// Every preference key is either changed by the chat or answered with the staging call, and
    /// no answer ever tells a chat to go and ask the user. The same for a chat of any person.
    #[test]
    fn every_preference_key_is_changed_or_staged_and_for_every_person_alike() {
        use crate::settings_allow::{approval_pref_keys, direct_pref_keys, DEAD_ENDS};
        let e = env();
        e.db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE profiles (id TEXT PRIMARY KEY, is_home INTEGER);
                 INSERT INTO profiles VALUES ('home', 1), ('p-2', 0);
                 ALTER TABLE chats ADD COLUMN profile_id TEXT;
                 INSERT INTO projects (id, name, created_at) VALUES ('pr', 'P', 1);
                 INSERT INTO chats (id, project_id, created_at, updated_at, profile_id)
                 VALUES ('chat-1', 'pr', 1, 1, 'home'), ('chat-2', 'pr', 1, 1, 'p-2');",
            )
        })
        .unwrap();
        let as_chat = |chat: &str| {
            let mut c = ctx(&e);
            c.caller_chat_id = chat.into();
            c
        };
        let kinds: Vec<(&str, bool)> = direct_pref_keys()
            .into_iter()
            .map(|k| (k, true))
            .chain(approval_pref_keys().into_iter().map(|k| (k, false)))
            .collect();
        assert!(kinds.len() > 50);
        for (key, direct) in kinds {
            let mut said = Vec::new();
            for chat in ["chat-1", "chat-2"] {
                // null restores the default, which every key accepts.
                let r = update_settings(&as_chat(chat), json!({"changes": {key: null}}));
                said.push(match r {
                    Ok(_) => "applied".to_owned(),
                    Err(err) => {
                        assert!(!direct, "{key}: a direct key was turned away: {}", err.public);
                        assert!(err.public.contains("app_stage_settings"), "{key}: {}", err.public);
                        for dead in DEAD_ENDS {
                            assert!(!err.public.to_lowercase().contains(dead), "{key}");
                        }
                        err.public
                    }
                });
                // What is staged is checked with the key's own validators and nothing is kept.
                let staged = apply_approved(
                    &as_chat(chat).services,
                    &*e.sink,
                    chat,
                    &[(key.to_owned(), Value::Null)],
                    false,
                );
                match (direct, staged) {
                    (true, Err(m)) => assert!(m.contains("app_update_settings"), "{key}: {m}"),
                    (false, Ok(done)) => assert_eq!(done.len(), 1, "{key}"),
                    (d, r) => panic!("{key} direct={d}: {r:?}"),
                }
            }
            assert_eq!(said[0], said[1], "{key}: the two people were answered differently");
            assert_eq!(said[0] == "applied", direct, "{key}");
        }
    }

    /// A rule that upper-cases a folder and counts commits, for the generic writer's hooks.
    struct Shout;
    static SHOUTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    impl crate::PrefRule for Shout {
        fn key(&self) -> &'static str {
            "updateSource"
        }
        fn prepare(
            &self,
            _tx: &Transaction<'_>,
            _before: Option<&Value>,
            value: &Value,
        ) -> Result<Value, String> {
            match value.as_str() {
                Some("nope") => Err("That folder is taken.".into()),
                Some(s) => Ok(json!(s.to_uppercase())),
                None => Err("A folder is text.".into()),
            }
        }
        fn after_commit(&self, _after: Option<&Value>) {
            SHOUTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn an_approval_setting_is_read_by_a_chat_and_changed_only_on_approval() {
        let e = env();
        let mut s = Services::new();
        s.insert(e.db.clone());
        s.insert(Arc::new(PrefRules(vec![Arc::new(Shout)])));
        let services = Arc::new(s);
        let mut c = ctx(&e);
        c.services = services.clone();
        let read = |k: &str| {
            e.db.snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, k)))
                .unwrap()
        };
        // The pages read the table: approval keys carry their reason.
        let table = access_list(&op(&e), Value::Null).unwrap();
        let row = |k: &str| {
            table["settings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["key"] == k)
                .cloned()
                .unwrap()
        };
        assert_eq!(row("newChatPermissionMode")["access"], "approval");
        assert!(row("newChatPermissionMode")["why"].is_string());
        assert_eq!(row("fastMode")["access"], "direct");
        assert_eq!(row("theme")["why"], Value::Null);
        // A chat reads it, flagged, and is told how to stage it.
        let g = get_settings(&c, json!({"keys": ["newChatPermissionMode"]})).unwrap();
        assert_eq!(g["options"]["newChatPermissionMode"]["needsApproval"], true);
        assert_eq!(g["options"]["newChatPermissionMode"]["allowed"][1], "default");
        let all = get_settings(&c, json!({})).unwrap();
        assert!(all["settings"].get("newChatPermissionMode").is_some());
        let r = update_settings(&c, json!({"changes": {"newChatPermissionMode": "default"}}));
        let said = r.unwrap_err().public;
        assert!(said.contains("app_stage_settings") && said.contains("newChatPermissionMode"), "{said}");
        // What a chat changes itself is not staged: it is told the call to make.
        let low = [("compactSidebar".to_owned(), json!(true))];
        let never = apply_approved(&services, &*e.sink, "chat-1", &low, true).unwrap_err();
        assert!(never.contains("app_update_settings"), "{never}");
        // Checked up front with the key's own validators, and nothing kept.
        let want = [
            ("updateSource".to_owned(), json!("d:/dist")),
            ("newChatPermissionMode".to_owned(), json!("default")),
        ];
        let dry = apply_approved(&services, &*e.sink, "chat-1", &want, false).unwrap();
        assert_eq!(dry[0].after, Some(json!("D:/DIST")), "the rule's value");
        assert_eq!((read("newChatPermissionMode"), e.sink.kinds().len()), (None, 0));
        let bad = [("newChatPermissionMode".to_owned(), json!("plan"))];
        let why = apply_approved(&services, &*e.sink, "chat-1", &bad, false).unwrap_err();
        assert!(why.contains("\"acceptEdits\""), "{why}");
        // All or nothing: the second change fails, so the first is not kept either.
        let half = [
            ("newChatPermissionMode".to_owned(), json!("default")),
            ("updateSource".to_owned(), json!("nope")),
        ];
        let why = apply_approved(&services, &*e.sink, "chat-1", &half, true).unwrap_err();
        assert!(why.contains("taken"), "{why}");
        assert_eq!(read("newChatPermissionMode"), None);
        // Approved: stored, announced to the interface and the rule, listed for the user's Undo.
        let shouts = SHOUTS.load(std::sync::atomic::Ordering::SeqCst);
        let done = apply_approved(&services, &*e.sink, "chat-1", &want, true).unwrap();
        assert_eq!(read("newChatPermissionMode"), Some(json!("default")));
        assert_eq!(read("updateSource"), Some(json!("D:/DIST")));
        assert_eq!(SHOUTS.load(std::sync::atomic::Ordering::SeqCst), shouts + 1);
        assert_eq!(e.sink.kinds(), ["preferences", "external_settings"]);
        let mut o = op(&e);
        o.services = services.clone();
        let listed = changes_list(&o, Value::Null).unwrap();
        assert_eq!(listed["changes"].as_array().unwrap().len(), 2);
        let ids: Vec<i64> = done.iter().filter_map(|a| a.change_id).collect();
        undo(&o, json!({"changeIds": ids})).unwrap();
        assert_eq!((read("newChatPermissionMode"), read("updateSource")), (None, None));
        assert_eq!(SHOUTS.load(std::sync::atomic::Ordering::SeqCst), shouts + 2);
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
            Some(
                json!({"enabled": true, "volume": 30, "themed": true, "cues": false, "focused": false})
            )
        );
        // A wrong field is still refused, and the stored value is untouched.
        assert!(update_settings(&ctx(&e), json!({"changes": {"sounds": {"loud": 1}}})).is_err());
        assert_eq!(read("sounds").unwrap()["volume"], 30);
        // Null restores the default.
        update_settings(&ctx(&e), json!({"changes": {"fontSizes": null}})).unwrap();
        assert_eq!(read("fontSizes"), None);
    }

    #[test]
    fn own_colours_are_merged_into_the_active_profiles_entry() {
        let e = env();
        let read = || {
            e.db.snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, "customColours")))
                .unwrap()
        };
        // No profile chosen: the "default" entry takes the change, and the screen is told at once.
        let r = update_settings(
            &ctx(&e),
            json!({"changes": {"customColours": {"accentHue": 4}}}),
        )
        .unwrap();
        assert_eq!(
            r["applied"]["customColours"]["after"],
            json!({"default": {"accentHue": 4}})
        );
        assert_eq!(e.sink.kinds(), ["preferences", "external_settings"]);
        // Other fields merge into the same entry.
        update_settings(
            &ctx(&e),
            json!({"changes": {"customColours": {"tint": "soft"}}}),
        )
        .unwrap();
        assert_eq!(
            read(),
            Some(json!({"default": {"accentHue": 4, "tint": "soft"}}))
        );
        // With a profile active, its entry is the one that changes and "default" is left alone.
        e.db.write(|tx| prefs::set_raw(tx, "activeProfile", &json!("p-1")))
            .unwrap();
        update_settings(
            &ctx(&e),
            json!({"changes": {"customColours": {"accentHue": 235}}}),
        )
        .unwrap();
        assert_eq!(
            read(),
            Some(json!({"default": {"accentHue": 4, "tint": "soft"}, "p-1": {"accentHue": 235}}))
        );
        // A hue out of range is refused and nothing is stored; null clears only this profile's entry.
        assert!(update_settings(
            &ctx(&e),
            json!({"changes": {"customColours": {"accentHue": 360}}})
        )
        .is_err());
        update_settings(&ctx(&e), json!({"changes": {"customColours": null}})).unwrap();
        assert_eq!(
            read(),
            Some(json!({"default": {"accentHue": 4, "tint": "soft"}}))
        );
        assert_eq!(
            get_settings(&ctx(&e), json!({"keys": ["customColours"]})).unwrap()["options"]
                ["customColours"]["merge"],
            true
        );
        // A chat that belongs to a profile changes that profile's colours, whichever the sidebar
        // shows.
        e.db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE profiles (id TEXT PRIMARY KEY, is_home INTEGER);
                 INSERT INTO profiles VALUES ('home', 1), ('p-2', 0);
                 ALTER TABLE chats ADD COLUMN profile_id TEXT;
                 INSERT INTO projects (id, name, created_at) VALUES ('pr', 'P', 1);
                 INSERT INTO chats (id, project_id, created_at, updated_at, profile_id)
                 VALUES ('chat-1', 'pr', 1, 1, 'p-2');",
            )
        })
        .unwrap();
        update_settings(
            &ctx(&e),
            json!({"changes": {"customColours": {"accentHue": 120}}}),
        )
        .unwrap();
        assert_eq!(read().unwrap()["p-2"], json!({"accentHue": 120}));
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

    /// The part of the stand-in key a chat stages.
    const OPEN: &[crate::StagedPart] = &[crate::StagedPart::new(
        "open",
        "Opening it lets the stand-in act, so you approve it.",
    )];

    /// A stand-in extension: one key holding `{open, level}`; a chat stages `open` and changes
    /// `level`. Each person has their own copy, as the channel policies do.
    struct Fake(Option<String>);
    impl Fake {
        fn slot(&self) -> String {
            match &self.0 {
                Some(who) => format!("localGameLiftMinutes@{who}"),
                None => "localGameLiftMinutes".to_owned(),
            }
        }
    }
    impl crate::SettingsExtension for Fake {
        fn staged_parts(&self, _key: &str) -> &'static [crate::StagedPart] {
            OPEN
        }
        fn keys(&self) -> Vec<&'static str> {
            vec!["localGameLiftMinutes", "unclassifiedFake"]
        }
        fn describe(&self, key: &str) -> Option<Value> {
            (key == "localGameLiftMinutes" || key == "unclassifiedFake")
                .then(|| json!({"type": "object", "default": {"open": false, "level": 1}}))
        }
        fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
            prefs::read(tx, "desktopPet")
                .and_then(|v| v.get(self.slot()).cloned())
                .unwrap_or_else(|| json!({"open": false, "level": 1}))
        }
        fn apply(
            &self,
            tx: &Transaction<'_>,
            key: &str,
            value: &Value,
        ) -> Result<(Value, Value), String> {
            let before = self.read(tx, key);
            let mut after = before.clone();
            for f in ["open", "level"] {
                if let Some(v) = value.get(f) {
                    after[f] = v.clone();
                }
            }
            let mut all = prefs::read(tx, "desktopPet").unwrap_or_else(|| json!({}));
            all[self.slot()] = after.clone();
            prefs::set_raw(tx, "desktopPet", &all).unwrap();
            Ok((before, after))
        }
        fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
            let mut all = prefs::read(tx, "desktopPet").unwrap_or_else(|| json!({}));
            all[self.slot()] = value.clone();
            prefs::set_raw(tx, "desktopPet", &all).map_err(|e| e.to_string())
        }
        fn for_chat(
            &self,
            _tx: &Transaction<'_>,
            chat: &str,
        ) -> Option<Arc<dyn crate::SettingsExtension>> {
            Some(Arc::new(Fake(Some(chat.to_owned()))))
        }
        fn changed_event(&self, _key: &str) -> AppEvent {
            AppEvent::new("fake_changed")
        }
        fn details(&self, _tx: &Transaction<'_>, _key: &str) -> Option<Value> {
            Some(json!({"queue": []}))
        }
        fn after_commit(&self, _key: &str) {
            COMMITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// How many times the stand-in was told a change had committed.
    static COMMITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn with_fake(e: &Env) -> ToolCtx {
        let mut s = Services::new();
        s.insert(e.db.clone());
        s.insert(Arc::new(crate::SettingsExtensions(vec![Arc::new(Fake(
            None,
        ))])));
        let mut c = ctx(e);
        c.services = Arc::new(s);
        c
    }

    #[test]
    fn an_extension_key_is_listed_changed_staged_recorded_and_undone() {
        let e = env();
        let c = with_fake(&e);
        let g = get_settings(&c, json!({})).unwrap();
        assert_eq!(
            g["settings"]["localGameLiftMinutes"],
            json!({"open": false, "level": 1})
        );
        assert_eq!(g["options"]["localGameLiftMinutes"]["default"]["level"], 1);
        assert_eq!(
            g["options"]["localGameLiftMinutes"]["needsApprovalParts"][0]["path"],
            "open"
        );
        // Read-only facts ride along beside the value, only for keys that have them.
        assert_eq!(g["details"]["localGameLiftMinutes"], json!({"queue": []}));
        assert!(get_settings(&ctx(&e), json!({"keys": ["theme"]})).unwrap()["details"].is_null());
        assert_eq!(
            g["options"].as_object().unwrap().len(),
            g["settings"].as_object().unwrap().len()
        );
        // A part that needs approval changes nothing and answers with the call to make; nothing
        // is recorded or announced.
        let r = update_settings(
            &c,
            json!({"changes": {"localGameLiftMinutes": {"open": true}}}),
        );
        let said = r.unwrap_err().public;
        assert!(
            said.contains("localGameLiftMinutes.open") && said.contains("app_stage_settings"),
            "{said}"
        );
        assert!(e.sink.kinds().is_empty());
        // Repeating the part at its current value is no change, so it goes ahead.
        update_settings(
            &c,
            json!({"changes": {"localGameLiftMinutes": {"open": false, "level": 2}}}),
        )
        .unwrap();
        e.sink.events.lock().unwrap().clear();
        // Staged, it is checked, and on approval it is applied with the part included.
        let staged = [(
            "localGameLiftMinutes".to_owned(),
            json!({"open": true, "level": 3}),
        )];
        let dry = apply_approved(&c.services, &*e.sink, "chat-1", &staged, false).unwrap();
        assert_eq!(dry[0].after.as_ref().unwrap()["open"], true);
        assert!(e.sink.kinds().is_empty(), "checking announces nothing");
        let done = apply_approved(&c.services, &*e.sink, "chat-1", &staged, true).unwrap();
        assert_eq!(done[0].after.as_ref().unwrap()["level"], 3);
        e.sink.events.lock().unwrap().clear();
        // An extension key nobody gave an access to is no setting: a typo, not a policy.
        let r = update_settings(&c, json!({"changes": {"unclassifiedFake": {"level": 3}}}));
        assert!(r.unwrap_err().public.contains("not a Helm setting"));
        assert!(e.sink.kinds().is_empty());
        let commits = COMMITS.load(std::sync::atomic::Ordering::SeqCst);
        let r = update_settings(
            &c,
            json!({"changes": {"localGameLiftMinutes": {"level": 4}}}),
        )
        .unwrap();
        assert!(COMMITS.load(std::sync::atomic::Ordering::SeqCst) > commits);
        assert_eq!(r["applied"]["localGameLiftMinutes"]["before"]["level"], 3);
        assert_eq!(r["applied"]["localGameLiftMinutes"]["after"]["level"], 4);
        // Its own event, not a preference change.
        assert_eq!(e.sink.kinds(), ["fake_changed", "external_settings"]);
        let id = r["applied"]["localGameLiftMinutes"]["changeId"]
            .as_i64()
            .unwrap();
        let mut o = op(&e);
        o.services = c.services.clone();
        let u = undo(&o, json!({"changeIds": [id]})).unwrap();
        assert!(COMMITS.load(std::sync::atomic::Ordering::SeqCst) > commits + 1);
        assert_eq!(u["restored"]["localGameLiftMinutes"]["level"], 3);
        let g = get_settings(&c, json!({"keys": ["localGameLiftMinutes"]})).unwrap();
        assert_eq!(g["settings"]["localGameLiftMinutes"]["level"], 3);
        assert_eq!(
            e.sink.kinds().last().map(String::as_str),
            Some("fake_changed")
        );
    }

    /// A setting that belongs to a person is that person's copy for their chats: a second
    /// person's chat changes theirs, and Undo puts back the copy the change came from.
    #[test]
    fn each_person_changes_their_own_copy_of_a_per_person_key() {
        let e = env();
        let mut c = with_fake(&e);
        let mut set = |chat: &str, level: i64| {
            c.caller_chat_id = chat.into();
            update_settings(
                &c,
                json!({"changes": {"localGameLiftMinutes": {"level": level}}}),
            )
            .unwrap()
        };
        let first = set("chat-1", 5);
        set("chat-2", 7);
        let level = |chat: &str| {
            let mut x = ctx(&e);
            x.services = c.services.clone();
            x.caller_chat_id = chat.into();
            get_settings(&x, json!({"keys": ["localGameLiftMinutes"]})).unwrap()["settings"]
                ["localGameLiftMinutes"]["level"]
                .clone()
        };
        assert_eq!((level("chat-1"), level("chat-2")), (json!(5), json!(7)));
        let mut o = op(&e);
        o.services = c.services.clone();
        let id = first["applied"]["localGameLiftMinutes"]["changeId"]
            .as_i64()
            .unwrap();
        undo(&o, json!({"changeIds": [id]})).unwrap();
        assert_eq!((level("chat-1"), level("chat-2")), (json!(1), json!(7)));
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
    fn labels_and_shortcuts_with_hidden_characters_or_bad_chords_are_refused() {
        let e = env();
        // Anywhere in a value, nested or not, and in an object key.
        for (key, value, at) in [
            (
                "hiddenSkills",
                json!(["review", "re\u{202e}view"]),
                "hiddenSkills[1]",
            ),
            (
                "fontSizes",
                json!({"interface\u{200b}": 15}),
                "fontSizes.interface",
            ),
        ] {
            let why = update_settings(&ctx(&e), json!({"changes": {key: value}}))
                .unwrap_err()
                .public;
            assert!(why.contains("invisible or direction-changing"), "{why}");
            assert!(why.contains(at), "{why}");
        }
        let stored =
            e.db.snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, "hiddenSkills")))
                .unwrap();
        assert_eq!(stored, None);
        // Shortcuts get the chord checks the Settings page applies.
        for (bad, says) in [
            (json!({"a": "Ctrl+\u{200b}K"}), "invisible"),
            (json!({"a\u{2066}": "Ctrl+K"}), "invisible"),
            (json!({"a": "Alt+F4"}), "belongs to Windows"),
            (json!({"a": "Ctrl+K", "b": "mod+k"}), "given to both"),
        ] {
            let why = update_settings(&ctx(&e), json!({"changes": {"keybindings": bad}}))
                .unwrap_err()
                .public;
            assert!(why.contains(says), "{bad}: {why}");
        }
        let ok = update_settings(
            &ctx(&e),
            json!({"changes": {"keybindings": {"a": "Ctrl+K", "b": null}}}),
        );
        assert!(ok.is_ok());
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
