//! The local-model settings chats may read and change through `app_get_settings` and
//! `app_update_settings`.
//!
//! | Key | Value |
//! |---|---|
//! | `localOllamaContext` | Ollama context window for Helm's variants, 8192 to 262144 tokens |
//! | `localKeepAliveMinutes` | how long Ollama keeps a model after a turn, 1 to 240 |
//! | `localServersEnabled` | `{serverId: true or false}`: which servers Helm uses |
//! | `localToolsSetup` | `lean` or `full`: the tools a local chat is offered |
//! | `localCompareModel` | `opus`, `sonnet` or `haiku`: the Claude model local usage is priced against |
//!
//! These are in the base tier: each is bounded, shows in "Changed by chats" with Undo, and none
//! can reach outside this machine. **Never here:** a server's address, name or key. Those are the
//! user's, in Settings > Local models.

use crate::settings::{self, Changes, CompareModel, LocalSettings, ToolsSetup};
use helm_hosttools::SettingsExtension;
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

pub const CONTEXT: &str = "localOllamaContext";
pub const KEEP_ALIVE: &str = "localKeepAliveMinutes";
pub const ENABLED: &str = "localServersEnabled";
pub const TOOLS: &str = "localToolsSetup";
pub const COMPARE: &str = "localCompareModel";

const PAGE: &str = "Settings > Local models";

pub struct LocalKeys;

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn extension() -> Arc<dyn SettingsExtension> {
    Arc::new(LocalKeys)
}

fn value_of(s: &LocalSettings, key: &str) -> Value {
    match key {
        CONTEXT => json!(s.ollama_context),
        KEEP_ALIVE => json!(s.keep_alive_minutes),
        ENABLED => Value::Object(settings::enabled_map(s)),
        TOOLS => serde_json::to_value(s.tools).unwrap_or(Value::Null),
        _ => serde_json::to_value(s.compare_with).unwrap_or(Value::Null),
    }
}

fn default_of(key: &str) -> Value {
    let d = LocalSettings::default();
    if key == ENABLED {
        // Every server on; the map's keys are the servers there are.
        return Value::Null;
    }
    value_of(&d, key)
}

fn sql(e: rusqlite::Error) -> String {
    format!("The local model settings could not be read or saved: {e}")
}

/// The change a value for `key` stands for, checked against the current settings.
fn change(key: &str, cur: &LocalSettings, value: &Value) -> Result<Changes, String> {
    let v = if value.is_null() {
        if key == ENABLED {
            json!(settings::enabled_map(&LocalSettings {
                servers: cur
                    .servers
                    .iter()
                    .map(|s| settings::Server { enabled: true, ..s.clone() })
                    .collect(),
                ..cur.clone()
            }))
        } else {
            default_of(key)
        }
    } else {
        value.clone()
    };
    let mut c = Changes::default();
    match key {
        CONTEXT => {
            c.ollama_context = Some(
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| format!("{key} is a whole number of tokens from {} to {}.", settings::MIN_CONTEXT, settings::MAX_CONTEXT))?,
            )
        }
        KEEP_ALIVE => {
            c.keep_alive_minutes = Some(
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| format!("{key} is a whole number of minutes from {} to {}.", settings::MIN_KEEP_ALIVE, settings::MAX_KEEP_ALIVE))?,
            )
        }
        TOOLS => {
            c.tools = Some(
                serde_json::from_value::<ToolsSetup>(v)
                    .map_err(|_| format!("{key} is \"lean\" or \"full\"."))?,
            )
        }
        COMPARE => {
            c.compare_with = Some(
                serde_json::from_value::<CompareModel>(v)
                    .map_err(|_| format!("{key} is \"opus\", \"sonnet\" or \"haiku\"."))?,
            )
        }
        ENABLED => {
            let map = v
                .as_object()
                .ok_or_else(|| format!("{key} is an object of {{serverId: true or false}}."))?;
            let mut servers: Vec<settings::ServerInput> = Vec::new();
            for s in &cur.servers {
                let enabled = match map.get(&s.id) {
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(format!("{key}.{} is true or false.", s.id)),
                    None => s.enabled,
                };
                servers.push(settings::ServerInput {
                    id: Some(s.id.clone()),
                    name: s.name.clone(),
                    kind: s.kind,
                    base_url: s.base_url.clone(),
                    enabled,
                    model: s.model.clone(),
                });
            }
            if let Some(unknown) = map.keys().find(|k| !cur.servers.iter().any(|s| &s.id == *k)) {
                let ids: Vec<&str> = cur.servers.iter().map(|s| s.id.as_str()).collect();
                return Err(format!(
                    "{key}.{unknown}: there is no such server. The servers are {}. Adding or changing one is the user's, in {PAGE}.",
                    ids.join(", ")
                ));
            }
            c.servers = Some(servers);
        }
        _ => return Err(format!("{key} is not a local model setting.")),
    }
    Ok(c)
}

fn put(tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(LocalSettings, LocalSettings), String> {
    let before = settings::load(tx).map_err(sql)?;
    let after = settings::apply_changes(&before, &change(key, &before, value)?)?;
    settings::store(tx, &after).map_err(sql)?;
    Ok((before, after))
}

impl SettingsExtension for LocalKeys {
    fn keys(&self) -> Vec<&'static str> {
        vec![CONTEXT, KEEP_ALIVE, ENABLED, TOOLS, COMPARE]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        let d = LocalSettings::default();
        match key {
            CONTEXT => Some(json!({
                "type": "integer",
                "min": settings::MIN_CONTEXT,
                "max": settings::MAX_CONTEXT,
                "default": d.ollama_context,
                "about": format!("{PAGE}, Ollama context size: the context window, in tokens, of the copy of an Ollama model Helm makes for its chats. It applies when a chat's worker next starts. Larger windows use more graphics memory. Nothing on Ollama itself changes."),
            })),
            KEEP_ALIVE => Some(json!({
                "type": "integer",
                "min": settings::MIN_KEEP_ALIVE,
                "max": settings::MAX_KEEP_ALIVE,
                "default": d.keep_alive_minutes,
                "about": format!("{PAGE}, Ollama keep-alive: minutes Ollama keeps a model loaded after a turn of a Helm chat. Other uses of Ollama keep its own timer."),
            })),
            ENABLED => Some(json!({
                "type": "object",
                "merge": true,
                "about": format!("{PAGE}: whether Helm uses each local model server, by server id (read it to see the ids). Send only the servers to change. A server that is off shows no models and refuses new turns. Adding, removing or re-addressing a server, and its key, are the user's, in {PAGE}."),
                "valueType": "boolean",
                "default": "every server on",
            })),
            TOOLS => Some(json!({
                "type": "enum",
                "allowed": ["lean", "full"],
                "default": "lean",
                "about": format!("{PAGE}, Tools in local chats: lean turns off Claude Code's own MCP servers in chats on a local model (Helm's own tools stay), which small models cope with better; full offers what a Claude chat has. It applies when a chat's worker next starts."),
            })),
            COMPARE => Some(json!({
                "type": "enum",
                "allowed": ["opus", "sonnet", "haiku"],
                "default": "sonnet",
                "about": format!("{PAGE}, Compare local chats with: the Claude model the cost report prices local tokens against (\"would have cost about $X on Sonnet\"). Local chats themselves cost nothing and never count toward the plan's usage."),
            })),
            _ => None,
        }
    }

    fn read(&self, tx: &Transaction<'_>, key: &str) -> Value {
        value_of(&settings::load(tx).unwrap_or_default(), key)
    }

    fn apply(&self, tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(Value, Value), String> {
        let (before, after) = put(tx, key, value)?;
        Ok((value_of(&before, key), value_of(&after, key)))
    }

    fn restore(&self, tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), String> {
        put(tx, key, value).map(|_| ())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("local_models")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    #[test]
    fn a_chat_changes_the_bounded_switches_and_undo_puts_them_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        reg.initializer("local-models", settings::init);
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        let ext = LocalKeys;
        let apply = |key: &str, v: Value| db.write(|tx| Ok::<_, rusqlite::Error>(ext.apply(tx, key, &v))).unwrap();
        let read = |key: &str| db.snapshot(|tx| Ok::<_, rusqlite::Error>(ext.read(tx, key))).unwrap();
        // The keys, their descriptions and their defaults.
        assert_eq!(ext.keys().len(), 5);
        for k in ext.keys() {
            assert!(ext.describe(k).is_some(), "{k}");
            assert!(!ext.needs_all_settings(k), "{k}");
        }
        assert!(ext.describe("theme").is_none());
        assert_eq!(read(CONTEXT), json!(65_536));
        assert_eq!(read(ENABLED), json!({"strata": true, "ollama": true}));
        assert_eq!(read(TOOLS), json!("lean"));
        assert_eq!(read(COMPARE), json!("sonnet"));
        // Changes validate with the Settings page's own rules.
        let (before, after) = apply(CONTEXT, json!(32_768)).unwrap();
        assert_eq!((before, after), (json!(65_536), json!(32_768)));
        assert!(apply(CONTEXT, json!(1000)).unwrap_err().contains("8192"));
        assert!(apply(CONTEXT, json!("big")).unwrap_err().contains("whole number"));
        assert!(apply(KEEP_ALIVE, json!(0)).unwrap_err().contains("keep-alive"));
        apply(KEEP_ALIVE, json!(60)).unwrap();
        apply(TOOLS, json!("full")).unwrap();
        assert!(apply(TOOLS, json!("heavy")).unwrap_err().contains("lean"));
        apply(COMPARE, json!("haiku")).unwrap();
        // Servers: only enabled flags, by id, merged; others are the user's.
        let (_, after) = apply(ENABLED, json!({"ollama": false})).unwrap();
        assert_eq!(after, json!({"strata": true, "ollama": false}));
        assert!(apply(ENABLED, json!({"rogue": true})).unwrap_err().contains("no such server"));
        assert!(apply(ENABLED, json!({"strata": "yes"})).unwrap_err().contains("true or false"));
        let s = db.snapshot(settings::load).unwrap();
        assert_eq!(s.servers[1].base_url, settings::OLLAMA_URL, "an address never changes here");
        assert_eq!(
            (s.ollama_context, s.keep_alive_minutes, s.tools, s.compare_with),
            (32_768, 60, ToolsSetup::Full, CompareModel::Haiku)
        );
        // null resets one setting; Undo restores the value from before.
        apply(ENABLED, Value::Null).unwrap();
        assert_eq!(read(ENABLED), json!({"strata": true, "ollama": true}));
        apply(CONTEXT, Value::Null).unwrap();
        assert_eq!(read(CONTEXT), json!(65_536));
        let (before, _) = apply(KEEP_ALIVE, json!(5)).unwrap();
        db.write(|tx| Ok::<_, rusqlite::Error>(ext.restore(tx, KEEP_ALIVE, &before))).unwrap().unwrap();
        assert_eq!(read(KEEP_ALIVE), json!(60));
        assert_eq!(ext.changed_event(CONTEXT).kind, "local_models");
    }
}
