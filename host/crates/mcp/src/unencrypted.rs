//! "Allow unencrypted on my network", as a chat changes it.
//!
//! A chat may add or edit a plain-http server on a private address and turn the opt-in on (the
//! admin tools in [`crate::admin`]). The user must be told and must be able to take it back, so
//! each such change goes into the settings change list under [`KEY`], which shows the notice with
//! Undo and lists the change in Settings → General → Changed by chats.
//!
//! The value of that key is a map from server id to `{url, unencrypted}`: the servers that have
//! the opt-in on, plus, in a change's `before`, the touched server's earlier address. Undo
//! restores it: a server the change added is removed, and a server the change edited gets its
//! earlier address and opt-in back. The key cannot be set through `app_update_settings`.

use crate::config::McpStore;
use crate::types::{McpServer, TransportKind};
use helm_hosttools::SettingsExtension;
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const KEY: &str = "mcpUnencryptedServers";

fn entry(s: &McpServer) -> Value {
    json!({"url": s.transport.url, "unencrypted": s.allow_private_http})
}

/// The servers that have the opt-in on, by id.
pub fn opted_in(servers: &[McpServer]) -> Value {
    Value::Object(
        servers
            .iter()
            .filter(|s| s.allow_private_http && s.transport.kind != TransportKind::Stdio)
            .map(|s| (s.id.clone(), entry(s)))
            .collect::<Map<_, _>>(),
    )
}

/// The change to record when a call touched server `id`: its value before and after, or `None`
/// when the server does not have the opt-in afterwards or nothing about the opt-in changed.
pub fn change_for(before: &[McpServer], after: &[McpServer], id: &str) -> Option<(Value, Value)> {
    let now = after.iter().find(|s| s.id == id)?;
    if !now.allow_private_http {
        return None;
    }
    let was = before.iter().find(|s| s.id == id);
    if was.is_some_and(|w| {
        w.allow_private_http
            && w.transport.url == now.transport.url
            && w.transport.kind == now.transport.kind
    }) {
        return None;
    }
    let mut b = opted_in(before);
    if let (Some(w), Some(m)) = (was, b.as_object_mut()) {
        if w.transport.kind != TransportKind::Stdio {
            m.insert(id.to_owned(), entry(w));
        }
    }
    Some((b, opted_in(after)))
}

struct Unencrypted {
    store: Arc<McpStore>,
}

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension(store: Arc<McpStore>) -> Arc<dyn SettingsExtension> {
    Arc::new(Unencrypted { store })
}

impl SettingsExtension for Unencrypted {
    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({
                "type": "object",
                "default": {},
                "about": "Read only here: the MCP servers that have \"Allow unencrypted on my network\" on, with their urls. Turn it on or off with allowPrivateHttp in app_add_mcp_server or app_edit_mcp_server; that is for plain http to a private-network address only, and the user is shown a notice with Undo. Undo removes a server the change added and puts back the address of one it edited.",
                "hasEffect": true,
            })
        })
    }

    fn read(&self, _tx: &Transaction<'_>, _key: &str) -> Value {
        opted_in(&self.store.load().unwrap_or_default())
    }

    fn apply(
        &self,
        _tx: &Transaction<'_>,
        _key: &str,
        _value: &Value,
    ) -> Result<(Value, Value), String> {
        Err(
            "Use allowPrivateHttp in app_add_mcp_server or app_edit_mcp_server to change this."
                .into(),
        )
    }

    fn restore(&self, _tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let target = value.as_object().cloned().unwrap_or_default();
        self.store
            .mutate(None, |list| {
                list.retain(|s| {
                    // A server that is not in the earlier state was added by the change.
                    !(s.allow_private_http
                        && s.transport.kind != TransportKind::Stdio
                        && !target.contains_key(&s.id))
                });
                for s in list.iter_mut() {
                    if let Some(t) = target.get(&s.id) {
                        s.transport.url = t["url"].as_str().map(str::to_owned);
                        s.allow_private_http = t["unencrypted"].as_bool().unwrap_or(false);
                    }
                }
                Ok(())
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("mcp_config")
    }
}
