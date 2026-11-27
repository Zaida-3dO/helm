//! The people of this Helm as keys of `app_get_settings` and `app_update_settings`.
//!
//! - `profiles`: every profile, `[{id, name, colour, about, hubPersonId, isHome, needsName}]`. A
//!   chat adds one (an entry without an id) or changes one (an entry with its id) itself, with the
//!   Undo notice; entries it leaves out stay as they are. Undo of a change that added a profile
//!   takes the new profile out again, and its chats go to the home profile.
//! - `deleteProfile`: `{id}` removes a profile. It is the one thing a chat stages: the profile's
//!   chats move to the home profile and the profile itself is gone for good, so the user
//!   approves it. It has no Undo.

use crate::store::{self, Patch, ProfileError};
use helm_hosttools::{JsonKey, SettingsExtension, StagedPart};
use helm_ipc::Services;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

pub const KEY: &str = "profiles";
pub const DELETE_KEY: &str = "deleteProfile";

const NONE: &[StagedPart] = &[];

fn why(e: ProfileError) -> String {
    e.message()
}

fn list_value(tx: &Transaction<'_>) -> Result<Value, String> {
    let all = store::list(tx).map_err(|e| e.to_string())?;
    Ok(Value::Array(
        all.iter()
            .map(|p| serde_json::to_value(p).expect("profiles serialise"))
            .collect(),
    ))
}

/// The names of the profiles, with their ids: what `deleteProfile` shows before and after.
fn names(tx: &Transaction<'_>) -> Result<Value, String> {
    let names: Vec<String> = store::list(tx)
        .map_err(|e| e.to_string())?
        .iter()
        .map(|p| format!("{} ({})", p.name, p.id))
        .collect();
    Ok(json!({"id": null, "profiles": names}))
}

/// Add or change the profiles `v` lists. With `prune`, a profile `v` does not list is removed
/// (what Undo of an addition needs).
fn save(tx: &Transaction<'_>, v: &Value, prune: bool) -> Result<Value, String> {
    let list = v
        .as_array()
        .ok_or("profiles is a list of {id?, name, colour, about, hubPersonId}.")?;
    let now = helm_core::now_ms();
    let mut kept: Vec<String> = Vec::new();
    for e in list {
        let o = e.as_object().ok_or("Each profile is an object.")?;
        for k in o.keys() {
            if ![
                "id",
                "name",
                "colour",
                "about",
                "hubPersonId",
                "isHome",
                "needsName",
            ]
            .contains(&k.as_str())
            {
                return Err(format!(
                    "profiles has no field {k:?}; its fields are id, name, colour, about and hubPersonId."
                ));
            }
        }
        let text = |k: &str| -> Result<Option<String>, String> {
            match o.get(k) {
                None => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(format!("profiles.{k} is text.")),
            }
        };
        let person = match o.get("hubPersonId") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            Some(_) => return Err("profiles.hubPersonId is text, or null to unlink.".into()),
        };
        match o.get("id").and_then(Value::as_str) {
            Some(id) => {
                // Echoing a profile back unchanged changes nothing.
                store::update(
                    tx,
                    id,
                    Patch {
                        name: text("name")?,
                        colour: text("colour")?,
                        hub_person_id: person,
                        about: text("about")?,
                    },
                )
                .map_err(why)?;
                kept.push(id.to_owned());
            }
            None => {
                let name = text("name")?.ok_or("A new profile needs a name.")?;
                let made = store::create(
                    tx,
                    &name,
                    text("colour")?.as_deref(),
                    person.flatten().as_deref(),
                    now,
                )
                .map_err(why)?;
                if let Some(about) = text("about")? {
                    store::update(
                        tx,
                        &made.id,
                        Patch {
                            about: Some(about),
                            ..Patch::default()
                        },
                    )
                    .map_err(why)?;
                }
                kept.push(made.id);
            }
        }
    }
    if prune {
        for p in store::list(tx).map_err(|e| e.to_string())? {
            if !p.is_home && !kept.contains(&p.id) {
                store::delete(tx, &p.id).map_err(why)?;
            }
        }
    }
    list_value(tx)
}

/// The extensions to register.
pub fn settings_extensions(services: &Services) -> Vec<Arc<dyn SettingsExtension>> {
    let poke = services.get::<helm_skills::sync::Trigger>();
    let poke_again = poke.clone();
    let profiles = JsonKey::new(
        KEY,
        "profiles_changed",
        json!({
            "type": "list",
            "maxItems": 16,
            "about": "The people of this Helm. Each is {id, name (1-40 characters, unique), colour, about (how to address and write for them, at most 2000 characters), hubPersonId (the hub person it is linked to; null unlinks), isHome and needsName (read only)}. Send entries to add (no id, a name needed) or to change (an id and the fields to change); profiles you leave out stay. Removing a profile is deleteProfile.",
        }),
        || json!([]),
        |tx, _| list_value(tx),
        |tx, _, v| save(tx, v, false),
    )
    .restoring(|tx, _, v| save(tx, v, true))
    .staged(NONE)
    .after_commit(move || {
        if let Some(t) = &poke {
            t.poke();
        }
    })
    .extension();

    let delete = JsonKey::new(
        DELETE_KEY,
        "profiles_changed",
        json!({
            "type": "object",
            "fields": {"id": {"type": "string", "about": "The profile to remove; never the home profile."}},
            "about": "Removes a profile: its chats move to the home profile and the profile is gone for good. Stage it with app_stage_settings, the user approves it, and it has no Undo. Read the profile ids from the profiles key.",
        }),
        || json!({"id": null, "profiles": []}),
        |tx, _| names(tx),
        |tx, _, v| {
            let id = v
                .get("id")
                .and_then(Value::as_str)
                .ok_or("deleteProfile takes {id}: the profile to remove.")?;
            store::delete(tx, id).map_err(why)?;
            names(tx)
        },
    )
    .restoring(|_, _, _| {
        Err("A removed profile cannot be brought back: its chats are with the home profile now.".into())
    })
    .after_commit(move || {
        if let Some(t) = &poke_again {
            t.poke();
        }
    })
    .extension();
    vec![profiles, delete]
}
