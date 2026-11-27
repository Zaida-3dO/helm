//! This computer's name as the `deviceName` key of `app_get_settings` and
//! `app_update_settings`. The name is how the household's other devices list this PC; a chat
//! changes it itself, with the Undo notice.

use crate::identity;
use helm_hosttools::{JsonKey, SettingsExtension, StagedPart};
use serde_json::{json, Value};
use std::sync::Arc;

pub const KEY: &str = "deviceName";

const NONE: &[StagedPart] = &[];

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension() -> Arc<dyn SettingsExtension> {
    JsonKey::new(
        KEY,
        "device_changed",
        json!({
            "type": "string",
            "maxLength": 40,
            "about": "This computer's name (1 to 40 characters), as the household's other devices list it. Null puts back the name the computer has itself.",
        }),
        || json!(identity::default_name()),
        |tx, _| {
            identity::ensure(tx)
                .map(|(_, name)| json!(name))
                .map_err(|e| e.to_string())
        },
        |tx, _, v| {
            let name = match v {
                Value::String(s) => s.clone(),
                _ => return Err("deviceName is text of 1 to 40 characters.".into()),
            };
            identity::rename(tx, &name)
                .map(|(_, n)| json!(n))
                .map_err(|e| e.message())
        },
    )
    .staged(NONE)
    .extension()
}
