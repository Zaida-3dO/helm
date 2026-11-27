//! The question gatekeeper as the `gatekeeper` key of `app_get_settings` and
//! `app_update_settings`: each person's own record, the one Settings > Gatekeeper shows.
//!
//! A chat changes the model itself, with the Undo notice. `enabled`, `askFrequency` and `policy`
//! decide when Helm answers a chat's question for the user, so a chat stages them and the user
//! approves.

use crate::settings::{parse_strict, GateSettings};
use crate::store;
use helm_hosttools::{JsonKey, SettingsExtension, StagedPart};
use serde_json::json;
use std::sync::Arc;

pub const KEY: &str = "gatekeeper";

const STAGED: &[StagedPart] = &[
    StagedPart::new(
        "enabled",
        "The gatekeeper answers chats' questions for you, so you approve turning it on or off.",
    ),
    StagedPart::new(
        "askFrequency",
        "How often the gatekeeper asks you instead of answering decides what it settles alone, so you approve it.",
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
