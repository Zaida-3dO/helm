//! Dictation as a key of `app_get_settings` and `app_update_settings`: `dictation`.
//!
//! A chat reads the setting and changes the model, language and mode itself, with the Undo
//! notice. Where a recording of the user's voice is sent decides who hears it, so the address
//! (`sttUrl`) and the switch that lets it be a private-network one (`allowPrivateNetwork`) are
//! staged (`app_stage_settings`) for the user to approve. The value is
//! `{sttUrl, allowPrivateNetwork, model, language, mode}`; a staged value names only the fields to
//! change, and the result passes the same address rule as the Settings page.

use crate::settings::{self, Settings};
use helm_hosttools::{SettingsExtension, StagedPart};
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

pub const KEY: &str = "dictation";

struct DictationKey;

/// Where the voice goes: the address (any change) and whether a private network may be one.
const STAGED: &[StagedPart] = &[
    StagedPart::new(
        "sttUrl",
        "Where a recording of your voice is sent decides who hears it, so you approve the address.",
    ),
    StagedPart::loosening(
        "allowPrivateNetwork",
        "A speech server on your network can then be chosen, so you approve turning it on.",
        helm_hosttools::turns_on,
    ),
];

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension() -> Arc<dyn SettingsExtension> {
    Arc::new(DictationKey)
}

fn as_json(s: &Settings) -> Value {
    json!({
        "sttUrl": s.stt_url,
        "allowPrivateNetwork": s.allow_private_network,
        "model": s.model,
        "language": s.language,
        "mode": s.mode,
    })
}

/// `s` with the fields `patch` names set, or why one is refused.
fn patched(s: &Settings, patch: &Value) -> Result<Settings, String> {
    let Some(o) = patch.as_object() else {
        return Err(format!(
            "{KEY} is an object with only the fields to change, or null."
        ));
    };
    let mut out = s.clone();
    for (k, v) in o {
        match k.as_str() {
            "sttUrl" => {
                out.stt_url = match v {
                    Value::Null => None,
                    Value::String(u) if u.trim().is_empty() => None,
                    Value::String(u) => Some(u.trim().to_owned()),
                    _ => return Err("sttUrl is an address, or null for none.".into()),
                }
            }
            "allowPrivateNetwork" => {
                out.allow_private_network =
                    v.as_bool().ok_or("allowPrivateNetwork is true or false.")?;
            }
            "model" => {
                let m = v.as_str().map(str::trim).unwrap_or_default();
                if !settings::model_ok(m) {
                    return Err(
                        "The model name may hold letters, digits and . _ - : / only.".into(),
                    );
                }
                out.model = m.to_owned();
            }
            "language" => {
                let l = v.as_str().map(str::trim).unwrap_or_default();
                if !settings::language_ok(l) {
                    return Err("The language is a code like en or fr, or auto.".into());
                }
                out.language = l.to_owned();
            }
            "mode" => {
                let m = v.as_str().map(str::trim).unwrap_or_default();
                if !settings::mode_ok(m) {
                    return Err("mode is live or record.".into());
                }
                out.mode = m.to_owned();
            }
            other => {
                return Err(format!(
                    "{KEY} has no field called \"{}\".",
                    other.chars().take(40).collect::<String>()
                ))
            }
        }
    }
    // Whatever is saved passes the address rule.
    if let Some(u) = &out.stt_url {
        out.stt_url = Some(settings::check_stt_url(u, out.allow_private_network)?);
    }
    Ok(out)
}

/// Check `value` (a patch, or null for the defaults) and store it; answers before and after.
fn put(tx: &Transaction<'_>, value: &Value) -> Result<(Value, Value), String> {
    let before = settings::load(tx);
    let after = if value.is_null() {
        Settings::default()
    } else {
        patched(&before, value)?
    };
    settings::save(tx, &after)
        .map_err(|e| format!("The dictation settings could not be saved: {e}"))?;
    Ok((as_json(&before), as_json(&after)))
}

impl SettingsExtension for DictationKey {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        STAGED
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({
                "type": "object",
                "merge": true,
                "default": as_json(&Settings::default()),
                "about": "Settings > Shell > Dictation: where the microphone button sends a recording to be turned into text. sttUrl is the speech-to-text server's address (http://, on this computer, or on the user's own network when allowPrivateNetwork is true; null for none), model and language are what it is asked for. mode is live (words as you speak, using Windows' own speech service when it can start, the server otherwise) or record (record, then send to the server). Send only the fields to change.",
            })
        })
    }

    fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
        as_json(&settings::load(tx))
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        put(tx, value)
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let s = patched(&Settings::default(), value)?;
        settings::save(tx, &s).map_err(|e| e.to_string())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new(crate::EVENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    #[test]
    fn a_staged_dictation_change_is_merged_checked_and_restored() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        reg.initializer("dictation", settings::init);
        let db = Database::open(&dir.path().join("t.db"), &reg).unwrap();
        let go = |patch: Value| {
            db.write(|tx| Ok::<_, rusqlite::Error>(put(tx, &patch)))
                .unwrap()
        };
        let (before, after) =
            go(json!({"sttUrl": "http://127.0.0.1:8000/v1", "model": "tiny"})).unwrap();
        assert_eq!(before["sttUrl"], Value::Null);
        assert_eq!(
            after["sttUrl"], "http://127.0.0.1:8000",
            "the address is normalised"
        );
        assert_eq!(
            (after["model"].clone(), after["language"].clone()),
            (json!("tiny"), json!("en"))
        );
        assert_eq!(after["mode"], "live", "live is the default way");
        let (_, rec) = go(json!({"mode": "record"})).unwrap();
        assert_eq!(rec["mode"], "record");
        // The address rule is the page's: a public or unticked network address is refused.
        for (patch, part) in [
            (json!({"sttUrl": "http://8.8.8.8:8000"}), "network"),
            (json!({"sttUrl": "http://192.168.1.5:8000"}), "network"),
            (json!({"sttUrl": "https://127.0.0.1"}), "http://"),
            (json!({"model": "bad model"}), "letters"),
            (json!({"language": "English"}), "code"),
            (json!({"mode": "loud"}), "live or record"),
            (json!({"allowPrivateNetwork": "yes"}), "true or false"),
            (json!({"loud": 1}), "no field"),
            (json!(5), "only the fields to change"),
        ] {
            let why = go(patch).unwrap_err();
            assert!(why.contains(part), "{why}");
        }
        // The private-network tick and the address go in one change; null resets everything.
        let (_, lan) =
            go(json!({"allowPrivateNetwork": true, "sttUrl": "http://192.168.1.5:8000"})).unwrap();
        assert_eq!(lan["sttUrl"], "http://192.168.1.5:8000");
        let (_, reset) = go(Value::Null).unwrap();
        assert_eq!(reset, as_json(&Settings::default()));
        // Undo puts a whole earlier value back.
        db.write(|tx| {
            let ext = DictationKey;
            Ok::<_, rusqlite::Error>(ext.restore(tx, KEY, &lan))
        })
        .unwrap()
        .unwrap();
        let now = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(settings::load(tx)))
            .unwrap();
        assert_eq!(now.stt_url.as_deref(), Some("http://192.168.1.5:8000"));
        assert!(now.allow_private_network);
    }
}
