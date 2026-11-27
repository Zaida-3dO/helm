//! Whether a profile's chats load the signed-in browser server, as the `browserServer` key of
//! `app_get_settings` and `app_update_settings`.
//!
//! The browser server drives the sign-ins of the person it was set up for, so it is Personal by
//! default: it is loaded for the home profile's chats and left out of every other profile's until
//! that profile switches it on. Any profile may: the switch is Direct (the chat changes it with
//! the Undo notice), and a call can name another person's profile (`person`) to read or change
//! theirs. It takes effect when a chat's worker next starts.

use helm_core::prefs;
use helm_hosttools::{JsonKey, SettingsExtension, StagedPart};
use rusqlite::{OptionalExtension, Transaction};
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const KEY: &str = "browserServer";
/// `{profileId: true or false}`: the profiles that chose, either way.
const PREF: &str = "browserProfiles";

const NONE: &[StagedPart] = &[];

fn home_id(tx: &Transaction<'_>) -> Option<String> {
    tx.query_row("SELECT id FROM profiles WHERE is_home = 1", [], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .ok()
    .flatten()
}

fn chosen(tx: &rusqlite::Connection) -> Map<String, Value> {
    match prefs::read(tx, PREF) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Whether the chats of `profile` (`None`: the home profile, or no profile at all) load the
/// browser server: what the profile chose, else only the home profile does.
pub fn allowed(tx: &rusqlite::Connection, profile: Option<&str>) -> bool {
    let Some(id) = profile else {
        return true;
    };
    match chosen(tx).get(id).and_then(Value::as_bool) {
        Some(b) => b,
        None => tx
            .query_row("SELECT is_home FROM profiles WHERE id = ?1", [id], |r| {
                r.get::<_, i64>(0)
            })
            .map(|h| h != 0)
            .unwrap_or(true),
    }
}

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension() -> Arc<dyn SettingsExtension> {
    JsonKey::new(
        KEY,
        "mcp_browser",
        json!({
            "type": "boolean",
            "about": "Whether this person's chats load the signed-in browser server (the browser tools, with the person's own signed-in sessions). On by default only for the home profile, whose sign-ins it is; any other profile switches it on for itself. Name person (a profile name or id) to read or change another person's. It applies when a chat's worker next starts. null restores the default.",
        }),
        || Value::Null,
        |tx, who| {
            let who = match who {
                Some(w) => Some(w.to_owned()),
                None => home_id(tx),
            };
            Ok(json!(allowed(tx, who.as_deref())))
        },
        |tx, who, v| {
            let id = match who {
                Some(w) => w.to_owned(),
                None => home_id(tx).ok_or("There is no profile yet.")?,
            };
            let mut map = chosen(tx);
            match v {
                Value::Null => {
                    map.remove(&id);
                }
                Value::Bool(b) => {
                    map.insert(id.clone(), Value::Bool(*b));
                }
                _ => return Err("browserServer is true or false (null: the default).".into()),
            }
            prefs::set_raw(tx, PREF, &Value::Object(map)).map_err(|e| e.to_string())?;
            Ok(json!(allowed(tx, Some(&id))))
        },
    )
    .per_person()
    .staged(NONE)
    .extension()
}
