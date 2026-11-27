//! The action buttons as the `actionButtons` key of `app_get_settings` and
//! `app_update_settings`. A button sends a fixed message into a chat or starts a chat with it, so
//! a chat edits the list itself, with the Undo notice. The value is the whole list: it replaces
//! the stored one.

use crate::store;
use crate::types::{ActionButton, MAX_BUTTONS};
use helm_hosttools::{JsonKey, SettingsExtension, StagedPart};
use serde_json::json;
use std::sync::Arc;

pub const KEY: &str = "actionButtons";

const NONE: &[StagedPart] = &[];

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension() -> Arc<dyn SettingsExtension> {
    let about = json!({
        "type": "list",
        "maxItems": MAX_BUTTONS,
        "default": [],
        "about": "Settings > Actions: the one-click buttons. Each is {id, label (1-40 characters), kind (\"prompt\" sends the text into the current chat, \"new_chat\" starts a chat in projectId and sends it), text, projectId (null until bound), placement (\"topbar\" or \"overflow\"), hidden, seedId (null for your own)}. The list replaces the stored one whole.",
    });
    JsonKey::new(
        KEY,
        "actions_changed",
        about,
        || json!([]),
        |tx, _| {
            store::buttons_in(tx).map(|b| serde_json::to_value(b).expect("buttons serialise"))
        },
        |tx, _, v| {
            let list: Vec<ActionButton> = serde_json::from_value(v.clone()).map_err(|e| {
                format!("{KEY} is a list of buttons {{id, label, kind, text, projectId, placement, hidden, seedId}}: {e}")
            })?;
            store::set_buttons_in(tx, list)
                .map(|b| serde_json::to_value(b).expect("buttons serialise"))
        },
    )
    .staged(NONE)
    .extension()
}
