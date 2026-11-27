//! Undo for what a chat changes about another chat that the chat's own row does not record:
//! whether it is muted, which profile it belongs to and which profiles it is shared with.
//!
//! `app_update_chat` reads a snapshot of the chat before and after (the `chat_fields_snapshot`
//! op) and files the pair in the change ring under [`KEY`], so the user sees the usual notice
//! with Undo. The value is `{chatId: snapshot}`; Undo puts each snapshot back and is refused when
//! a chat is no longer as the change left it.

use crate::store;
use helm_hosttools::{SettingsExtension, StagedPart};
use helm_ipc::AppEvent;
use rusqlite::{params, Transaction};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The change-ring key these edits are filed under.
pub const KEY: &str = "chatFields";

const NONE: &[StagedPart] = &[];

/// One chat as these edits see it: muted or not, its profile and the profiles it is shared with.
pub fn snapshot(tx: &Transaction<'_>, chat: &str) -> rusqlite::Result<Value> {
    let profile: String =
        tx.query_row("SELECT profile_id FROM chats WHERE id = ?1", [chat], |r| {
            r.get(0)
        })?;
    let mut shared: Vec<String> = tx
        .prepare("SELECT profile_id FROM chat_shares WHERE chat_id = ?1")?
        .query_map([chat], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    shared.sort();
    Ok(json!({
        "muted": helm_chats::mute::is_muted(tx, chat)?,
        "profileId": profile,
        "sharedWith": shared,
    }))
}

/// Put one chat back as `snap` says. A profile that is gone is left out.
fn put_back(tx: &Transaction<'_>, chat: &str, snap: &Value) -> Result<(), String> {
    let sql = |e: rusqlite::Error| e.to_string();
    let ids = [chat.to_owned()];
    helm_chats::mute::set_muted(
        tx,
        chat,
        snap["muted"].as_bool().unwrap_or(false),
        helm_core::now_ms(),
    )
    .map_err(sql)?;
    if let Some(p) = snap["profileId"].as_str().filter(|p| !p.is_empty()) {
        if store::get(tx, p).map_err(|e| e.to_string())?.is_some() {
            store::assign(tx, &ids, p).map_err(|e| e.message())?;
        }
    }
    tx.execute("DELETE FROM chat_shares WHERE chat_id = ?1", params![chat])
        .map_err(sql)?;
    for p in snap["sharedWith"].as_array().into_iter().flatten() {
        if let Some(p) = p.as_str() {
            if store::get(tx, p).map_err(|e| e.to_string())?.is_some() {
                store::share(tx, &ids, p, true).map_err(|e| e.message())?;
            }
        }
    }
    Ok(())
}

struct ChatFields;

/// The extension to register: it only carries Undo and is not listed among the settings.
pub fn settings_extension() -> Arc<dyn SettingsExtension> {
    Arc::new(ChatFields)
}

impl SettingsExtension for ChatFields {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        NONE
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn listed(&self, _key: &str) -> bool {
        false
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({"type": "object", "about": "Carries Undo for a chat's mute, profile and sharing, which app_update_chat changes."})
        })
    }

    fn read(&self, _tx: &Transaction<'_>, _key: &str) -> Value {
        json!({})
    }

    fn apply(
        &self,
        _tx: &Transaction<'_>,
        _key: &str,
        _value: &Value,
    ) -> Result<(Value, Value), String> {
        Err("A chat is changed with app_update_chat, not here.".into())
    }

    fn unchanged_since(&self, tx: &Transaction<'_>, _key: &str, after: &Value) -> Option<bool> {
        let map = after.as_object()?;
        Some(
            map.iter()
                .all(|(id, snap)| snapshot(tx, id).is_ok_and(|now| &now == snap)),
        )
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let empty = Map::new();
        for (id, snap) in value.as_object().unwrap_or(&empty) {
            put_back(tx, id, snap)?;
        }
        Ok(())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("profiles_changed")
    }
}
