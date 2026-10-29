//! `webSearchInClaudeChats`: whether Claude chats are offered web search, as a setting chats can
//! read and change through `app_get_settings` and `app_update_settings`.
//!
//! The setting is in the base tier: it is bounded (a switch), shows in "Changed by chats" with
//! Undo, and takes effect only for chats that start afterwards. The Brave key itself is not a
//! setting here. Only the user enters, imports or forgets it, in Settings > Web search, and
//! nothing an agent can call reads it.

use crate::store::{self, Settings};
use crate::WebSearch;
use helm_hosttools::SettingsExtension;
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

pub const KEY: &str = "webSearchInClaudeChats";
/// The event that tells Settings > Web search to read again.
pub const EVENT: &str = "web_search";

struct WebSearchKeys {
    web: Arc<WebSearch>,
}

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension(web: Arc<WebSearch>) -> Arc<dyn SettingsExtension> {
    Arc::new(WebSearchKeys { web })
}

fn put(tx: &Transaction<'_>, on: bool) -> Result<(), String> {
    let mut s: Settings = store::load(tx);
    s.offer_in_claude_chats = on;
    store::save(tx, &s).map_err(|e| e.to_string())
}

impl SettingsExtension for WebSearchKeys {
    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({
                "type": "boolean",
                "default": false,
                "about": "Settings > Web search, \"Offer in Claude chats\". On: Claude chats get the app_web_search tool (Brave Search) as well, using the user's own Brave key and allowance. Off: only chats running on a local model get it, because they have no web search of their own. It does nothing until the user has entered a Brave key (details says whether one is set), and a chat sees the change from its next start. The key is the user's to enter and is never shown or changed through these tools.",
            })
        })
    }

    fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
        json!(store::load(tx).offer_in_claude_chats)
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        if key != KEY {
            return Err(format!("{key} is not a web search setting."));
        }
        let before = store::load(tx).offer_in_claude_chats;
        let after = match value {
            Value::Null => false,
            Value::Bool(b) => *b,
            _ => return Err(format!("{KEY} is true or false.")),
        };
        put(tx, after)?;
        Ok((json!(before), json!(after)))
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        put(tx, value.as_bool().unwrap_or(false))
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new(EVENT)
    }

    fn details(&self, _tx: &Transaction<'_>, _key: &str) -> Option<Value> {
        Some(json!({
            "braveKeySet": self.web.has_key(),
            "note": "The Brave search key is entered by the user in Settings > Web search; no tool reads or changes it.",
        }))
    }
}
