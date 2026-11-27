//! The question gatekeeper as the `gatekeeper` key of `app_get_settings` and
//! `app_update_settings`: each person's own record, the one Settings > Gatekeeper shows.
//!
//! A chat changes the model, and anything that makes the gatekeeper answer less to the user,
//! itself, with the Undo notice. Turning it on, asking less often and a new policy decide when
//! Helm answers a chat's question for the user, so a chat stages those and the user approves.

use crate::settings::{parse_strict, GateSettings};
use crate::store;
use helm_hosttools::{JsonKey, SettingsExtension, StagedPart};
use serde_json::{json, Value};
use std::sync::Arc;

pub const KEY: &str = "gatekeeper";

fn asks_less(before: &Value, after: &Value) -> bool {
    let rank = |v: &Value| match v.as_str() {
        Some("less") => 0,
        Some("more") => 2,
        _ => 1,
    };
    rank(after) < rank(before)
}

/// What decides when the gatekeeper answers for the user is staged when it moves toward answering
/// more: turned on, asking less often, or a new policy. The other way is the chat's own.
const STAGED: &[StagedPart] = &[
    StagedPart::loosening(
        "enabled",
        "Turning the gatekeeper on lets it answer chats' questions for you, so you approve it.",
        helm_hosttools::turns_on,
    ),
    StagedPart::loosening(
        "askFrequency",
        "Asking you less often means it settles more alone, so you approve it.",
        asks_less,
    ),
    StagedPart::new(
        "policy",
        "The policy words are what the gatekeeper answers by on your behalf, so you approve them.",
    ),
];

fn value(s: &GateSettings) -> serde_json::Value {
    serde_json::to_value(s).expect("settings serialise")
}

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension() -> Arc<dyn SettingsExtension> {
    let about = json!({
        "type": "object",
        "merge": true,
        "about": "Settings > Gatekeeper, for the person who owns the chat: enabled, askFrequency (\"less\", \"balanced\" or \"more\"), policy (the person's own words; null: the built-in policy), model (a Claude model name, one line). enabled, askFrequency and policy decide what the gatekeeper answers for the user, so they are staged with app_stage_settings. Send only the fields to change; null restores the defaults.",
        "default": value(&GateSettings::default()),
    });
    JsonKey::new(
        KEY,
        "gate",
        about,
        || value(&GateSettings::default()),
        |tx, who| {
            let id = who
                .map(str::to_owned)
                .unwrap_or_else(|| store::active_profile(tx));
            store::load_settings(tx, &id)
                .map(|s| value(&s))
                .map_err(|e| e.to_string())
        },
        |tx, who, v| {
            let s = parse_strict(v)?;
            let id = who
                .map(str::to_owned)
                .unwrap_or_else(|| store::active_profile(tx));
            store::save_settings(tx, &id, &s).map_err(|e| e.to_string())?;
            Ok(value(&s))
        },
    )
    .per_person()
    .staged(STAGED)
    .extension()
}
