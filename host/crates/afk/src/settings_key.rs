//! AFK as the `afk` key of `app_get_settings` and `app_update_settings`.
//!
//! The value is the Settings page's own record. A chat changes any field itself, with the Undo
//! notice, except `planApproval`: set to "approve" it signs plans off for the user while they are
//! away, so a chat stages it and the user approves.

use crate::service::AfkService;
use crate::{settings, store};
use helm_hosttools::{JsonKey, SettingsExtension, StagedPart};
use serde_json::json;
use std::sync::Arc;

pub const KEY: &str = "afk";

const STAGED: &[StagedPart] = &[StagedPart::new(
    "planApproval",
    "While you are away Helm would sign plans off for you, so you approve turning that on.",
)];

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension(svc: Arc<AfkService>) -> Arc<dyn SettingsExtension> {
    settings_extension_with(move || svc.settings_committed(helm_core::now_ms()))
}

/// The same, with what to do once a change has committed (the service's own hook, or nothing).
pub fn settings_extension_with(
    committed: impl Fn() + Send + Sync + 'static,
) -> Arc<dyn SettingsExtension> {
    let about = json!({
        "type": "object",
        "merge": true,
        "about": "Settings > AFK: what Helm does while the user is away. enabled turns it on; appliesTo is \"all\"; answer is the text Helm answers a waiting chat with (null: the built-in text); planApproval is \"defer\" or \"approve\" (signs plans off for the user: staged with app_stage_settings); digestPrompt is the summary prompt (null: built in); auto is {screenLock, lockGraceSeconds 0-600, sleep, unansweredMinutes null or 5-240, idleMinutes null or 5-240, end onReturn|onReturnAutoOnly|manual}. Send only the fields to change; null restores the defaults.",
        "default": serde_json::to_value(settings::AfkSettings::default()).expect("settings serialise"),
    });
    JsonKey::new(
        KEY,
        // Not "afk": that event carries the whole view, which `settings_committed` publishes.
        "afk_settings",
        about,
        || serde_json::to_value(settings::AfkSettings::default()).expect("settings serialise"),
        |tx, _| {
            let s = store::load_settings(tx).map_err(|e| e.to_string())?;
            Ok(serde_json::to_value(s).expect("settings serialise"))
        },
        |tx, _, v| {
            let s = settings::parse_strict(v)?;
            store::save_settings(tx, &s).map_err(|e| e.to_string())?;
            Ok(serde_json::to_value(s).expect("settings serialise"))
        },
    )
    .staged(STAGED)
    .after_commit(committed)
    .extension()
}
