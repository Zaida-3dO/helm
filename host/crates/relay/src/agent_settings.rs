//! Settings → Hub for chats: the `telegram` key of `app_get_settings` and `app_update_settings`.
//!
//! The value is the whole Telegram policy file: the tag limits, the away-alerts switch, whether
//! approval prompts can be answered from Telegram, and the model and effort a new Telegram chat
//! opens with. A chat reads all of it but may change only the last two: the limits and the two
//! switches decide what leaves the machine unattended, so they stay the user's, changed only on
//! the Settings page. An update that would move one of them is refused whole, even though it may
//! repeat those fields at their current value.
//!
//! An update names only the fields to change; the merged file goes through the same checks as
//! the Settings page (`PolicyFile::check` and the effort set), so a bad value refuses the whole
//! update and stores nothing. `null` resets `model` and `effort` to unset, which leaves new
//! Telegram chats on the app's usual new-chat settings. The file holds no secrets: the bot
//! credentials live in the credential store.

use crate::telegram::{self, PolicyFile};
use helm_hosttools::SettingsExtension;
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub const KEY: &str = "telegram";
const PAGE: &str = "Settings > Hub";
const EVENT: &str = "relay_telegram";
/// Model ids are short (a Claude model, or `local/<server>/<model>`); this bounds the stored
/// string without rejecting any id the model picker offers.
const MAX_MODEL_CHARS: usize = 120;
/// The effort set as said to a chat, matching the app's one list of efforts.
const EFFORT_LIST: &str = "low, medium, high, xhigh or max";

pub struct TelegramPolicy {
    path: PathBuf,
}

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension(profile_dir: &std::path::Path) -> std::sync::Arc<dyn SettingsExtension> {
    std::sync::Arc::new(TelegramPolicy {
        path: telegram::policy_path(profile_dir),
    })
}

fn as_json(f: &PolicyFile) -> Value {
    serde_json::to_value(f).expect("the policy serialises")
}

/// The policy as a value, or a sentence naming what is wrong with it.
fn from_json(v: &Value) -> Result<PolicyFile, String> {
    serde_json::from_value(v.clone()).map_err(|e| {
        format!("Expected maxAgeHours, maxTokens, awayAlerts, approvals, and optional model and effort: {e}")
    })
}

/// `v` without the fields a chat may change: what is left must not differ across an update.
fn locked(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("model");
        o.remove("effort");
    }
    v
}

/// The path of the first difference between `a` and `b`, if any.
fn first_diff(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match (x.get(k), y.get(k)) {
                    (Some(l), Some(r)) => first_diff(l, r, &p),
                    _ => Some(p),
                }
            })
        }
        _ if a == b => None,
        _ => Some(path.to_owned()),
    }
}

/// Merge `patch` into `base`: objects field by field, anything else replaces.
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

/// `model` and `effort` back to unset on top of `current`.
fn reset_changeable(current: &Value) -> Value {
    let mut out = current.clone();
    if let Some(o) = out.as_object_mut() {
        o.remove("model");
        o.remove("effort");
    }
    out
}

/// The two fields a chat may set, checked; a sentence naming the problem, if any.
fn check_changeable(patch: &Value) -> Result<(), String> {
    if let Some(m) = patch.get("model") {
        if !m.is_null() {
            let s = m.as_str().ok_or_else(|| format!("{KEY}.model must be a model id or null (null leaves new Telegram chats on the app's usual new-chat settings)."))?;
            if s.trim().is_empty() {
                return Ok(());
            }
            if s.chars().count() > MAX_MODEL_CHARS || s.chars().any(char::is_control) {
                return Err(format!(
                    "{KEY}.model must be a model id from the model picker (one line, at most {MAX_MODEL_CHARS} characters), or null."
                ));
            }
        }
    }
    if let Some(e) = patch.get("effort") {
        if !e.is_null() {
            let s = e
                .as_str()
                .ok_or_else(|| format!("{KEY}.effort must be one of {EFFORT_LIST} or null."))?;
            if !s.trim().is_empty() && !helm_chats::catalogue::is_effort(s.trim()) {
                return Err(format!(
                    "{KEY}.effort must be one of {EFFORT_LIST} or null."
                ));
            }
        }
    }
    Ok(())
}

fn io_err(e: std::io::Error) -> String {
    format!("The Telegram policy could not be saved: {e}")
}

fn options() -> Value {
    json!({
        "type": "object",
        "about": "Settings > Hub: the Telegram chat's tag limits and switches, and the model and effort a new Telegram chat opens with. Read all of it. You may change only model and effort; the limits and both switches are the user's and an update that touches them is refused. Send only the fields to change: the rest must repeat at their current value or stay out. null resets model and effort to unset.",
        "default": as_json(&PolicyFile::default()),
        "merge": true,
        "fields": {
            "maxAgeHours": {"type": "integer", "min": 1, "max": 720, "about": "How long a message may wait for the chat to be back before it is dropped."},
            "maxTokens": {"type": "integer", "min": 10_000, "max": 5_000_000, "about": "The running token cap for the Telegram chat."},
            "awayAlerts": {"type": "boolean", "about": "Telegram alerts while the user is away."},
            "approvals": {"type": "boolean", "about": "Approval prompts can be answered with Telegram buttons."},
            "model": {"type": "string or null", "rule": "a model id from the model picker (a Claude model, or local/<server>/<model>), at most 120 characters; null or blank: the app's usual new-chat model", "about": "The model a new Telegram chat opens with."},
            "effort": {"type": "enum", "allowed": helm_chats::catalogue::EFFORTS, "nullable": true, "about": "The effort a new Telegram chat opens with; null: the app's usual new-chat effort."},
        },
    })
}

impl SettingsExtension for TelegramPolicy {
    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(options)
    }

    fn read(&self, _tx: &Transaction<'_>, _key: &str) -> Value {
        as_json(&telegram::load_file(&self.path))
    }

    fn apply(
        &self,
        _tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        let before = as_json(&telegram::load_file(&self.path));
        let mut wanted = if value.is_null() {
            reset_changeable(&before)
        } else if value.is_object() {
            check_changeable(value)?;
            let mut m = before.clone();
            merge(&mut m, value);
            m
        } else {
            return Err(format!(
                "{KEY} must be an object with only the fields to change (see options.{KEY}), or null."
            ));
        };
        // Blank model/effort read as unset, so a chat can clear them the way the page's pickers do.
        if let Some(o) = wanted.as_object_mut() {
            for k in ["model", "effort"] {
                let blank = o
                    .get(k)
                    .and_then(Value::as_str)
                    .is_none_or(|s| s.trim().is_empty());
                if blank {
                    o.remove(k);
                }
            }
        }
        let merged = from_json(&wanted)?;
        merged.check()?;
        let after = as_json(&merged);
        if let Some(path) = first_diff(&locked(&before), &locked(&after), "") {
            return Err(format!(
                "{KEY}.{path} is yours to change only: ask the user to change it in {PAGE}. You may change model and effort."
            ));
        }
        telegram::save_policy(&self.path, merged).map_err(io_err)?;
        Ok((before, after))
    }

    fn restore(&self, _tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let f = from_json(value)?;
        f.check()?;
        telegram::save_policy(&self.path, f).map_err(io_err)
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new(EVENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};
    use serde_json::json;

    struct Env {
        db: Database,
        ext: TelegramPolicy,
        // Declared last so it outlives the open database when the test ends.
        dir: tempfile::TempDir,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &MigrationRegistry::new()).unwrap();
        let ext = TelegramPolicy {
            path: telegram::policy_path(&dir.path().join("prof")),
        };
        Env { db, ext, dir }
    }

    impl Env {
        fn read(&self) -> Value {
            self.db
                .snapshot(|tx| Ok::<_, rusqlite::Error>(self.ext.read(tx, KEY)))
                .unwrap()
        }
        fn apply(&self, v: Value) -> Result<(Value, Value), String> {
            self.db
                .write(|tx| Ok::<_, rusqlite::Error>(self.ext.apply(tx, KEY, &v)))
                .expect("the policy file save is not a sql error")
        }
        fn restore(&self, v: Value) -> Result<(), String> {
            self.db
                .write(|tx| Ok::<_, rusqlite::Error>(self.ext.restore(tx, KEY, &v)))
                .expect("the policy file save is not a sql error")
        }
    }

    #[test]
    fn a_chat_sets_the_new_telegram_chat_model_and_effort_and_undo_puts_them_back() {
        let e = env();
        assert_eq!(e.ext.keys(), vec![KEY]);
        assert!(e.ext.describe(KEY).is_some());
        assert!(e.ext.describe("theme").is_none());
        assert!(!e.ext.needs_all_settings(KEY));
        assert_eq!(e.ext.changed_event(KEY).kind, EVENT);
        // The said list is the app's own, phrased with an "or".
        let efforts = helm_chats::catalogue::EFFORTS;
        assert_eq!(
            EFFORT_LIST,
            format!(
                "{} or {}",
                efforts[..efforts.len() - 1].join(", "),
                efforts[efforts.len() - 1]
            )
        );

        // Unset to start; the whole policy reads, with its defaults.
        let off = e.read();
        assert_eq!(off["maxAgeHours"], json!(24));
        assert!(off.get("model").is_none());
        assert!(off.get("effort").is_none());

        // A change names only the two fields a chat may set.
        let (before, after) = e
            .apply(json!({"model": "local/strata/qwen-small", "effort": "low"}))
            .unwrap();
        assert!(before.get("model").is_none());
        assert_eq!(after["model"], json!("local/strata/qwen-small"));
        assert_eq!(after["effort"], json!("low"));
        // What the file itself holds, as the page would read it back.
        let saved = telegram::load_file(&e.ext.path);
        assert_eq!(
            saved.new_chat(),
            (Some("local/strata/qwen-small"), Some("low"))
        );
        // Undo (and the page) still see the user's fields untouched.
        assert_eq!(e.read()["maxAgeHours"], json!(24));
        assert_eq!(e.read()["approvals"], json!(false));

        // A later field change merges with the earlier one; the effort set is the app's.
        let (_, after) = e.apply(json!({"effort": "xhigh"})).unwrap();
        assert_eq!(after["model"], json!("local/strata/qwen-small"));
        assert_eq!(after["effort"], json!("xhigh"));
        for bad in ["ultra", "ULTRA", "HIGH"] {
            let err = e.apply(json!({"effort": bad})).unwrap_err();
            assert!(
                err.contains("low, medium, high, xhigh or max"),
                "{bad}: {err}"
            );
        }
        // (Blank effort is a clear, checked below; it is not a refusal.)

        // Model: a string of the picker's shape, or null; not a number, not absurdly long.
        assert!(e
            .apply(json!({"model": 3}))
            .unwrap_err()
            .contains("model id"));
        let long = "x".repeat(121);
        assert!(e
            .apply(json!({"model": long}))
            .unwrap_err()
            .contains("at most 120"));
        assert!(e
            .apply(json!({"model": "ha\nku"}))
            .unwrap_err()
            .contains("at most 120"));

        // The user-only fields: refused whole when they would move, allowed when repeated as-is.
        let err = e.apply(json!({"approvals": true})).unwrap_err();
        assert!(
            err.contains("approvals") && err.contains("Settings > Hub"),
            "{err}"
        );
        assert!(e
            .apply(json!({"approvals": false, "maxAgeHours": 48}))
            .unwrap_err()
            .contains("maxAgeHours"));
        assert!(e
            .apply(json!({"approvals": false, "model": "opus"}))
            .is_ok_and(|(_, a)| a["model"] == json!("opus") && a["approvals"] == json!(false)));
        // Nothing was stored by the refused updates.
        assert!(!telegram::load_file(&e.ext.path).approvals);

        // The merged file goes through the page's own range checks all the same.
        assert!(e
            .apply(json!({"maxAgeHours": 1000, "model": "opus"}))
            .unwrap_err()
            .contains("age limit"));

        // A field outside the policy refuses the whole update.
        assert!(e
            .apply(json!({"token": "abc"}))
            .unwrap_err()
            .contains("maxAgeHours"));
        assert!(e.apply(json!("opus")).unwrap_err().contains("object"));

        // The page's pickers save through the same file: a UI save still round-trips.
        telegram::save_policy(
            &e.ext.path,
            PolicyFile {
                max_age_hours: 72,
                max_tokens: 50_000,
                away_alerts: false,
                approvals: true,
                model: Some("sonnet".into()),
                effort: Some("medium".into()),
            },
        )
        .unwrap();
        let now = e.read();
        assert_eq!(now["maxAgeHours"], json!(72));
        assert_eq!(now["model"], json!("sonnet"));
        // A chat repeating the user's fields at their current values may still set effort.
        let (_, after) = e
            .apply(json!({
                "maxAgeHours": 72, "maxTokens": 50_000, "awayAlerts": false, "approvals": true,
                "model": "sonnet", "effort": "max",
            }))
            .unwrap();
        assert_eq!(after["effort"], json!("max"));

        // Undo: the before-value from an earlier apply goes straight back.
        let (before, after) = e.apply(json!({"model": null})).unwrap();
        assert!(after.get("model").is_none());
        e.restore(before).unwrap();
        assert_eq!(e.read()["model"], json!("sonnet"));
        e.restore(after).unwrap();
        assert!(e.read().get("model").is_none());

        // null resets only the two fields a chat may set.
        e.apply(json!({"model": "opus", "effort": "high"})).unwrap();
        e.apply(json!({"awayAlerts": false, "model": "opus", "effort": "high"}))
            .unwrap(); // repeat as-is
        e.apply(json!(null)).unwrap();
        let back = e.read();
        assert!(back.get("model").is_none());
        assert!(back.get("effort").is_none());
        assert_eq!(back["awayAlerts"], json!(false));
        // Blank string clears, the way the picker's "App default" option saves.
        e.apply(json!({"model": "opus"})).unwrap();
        e.apply(json!({"model": ""})).unwrap();
        assert!(e.read().get("model").is_none());
        // Everything lived under the temp folder's profile path; nothing touched the real one.
        assert!(telegram::policy_path(&e.dir.path().join("prof")).is_file());
    }
}
