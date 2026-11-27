//! The local-model settings chats may read and change through `app_get_settings` and
//! `app_update_settings`.
//!
//! | Key | Value |
//! |---|---|
//! | `localOllamaContext` | Ollama context window for Helm's variants, 8192 to 262144 tokens |
//! | `localKeepAliveMinutes` | how long Ollama keeps a model after a turn, 1 to 240 |
//! | `localServersEnabled` | `{serverId: true or false}`: which servers Helm uses |
//! | `localSpeechReserveGb` | graphics memory (GB, 0 to 24) also freed for speech-to-text when a model load has to unload others |
//! | `localToolsSetup` | `lean` or `full`: the tools a local chat is offered |
//! | `localCompareModel` | `opus`, `sonnet` or `haiku`: the Claude model local usage is priced against |
//! | `localGameLiftMinutes` | minutes after the last listed game closes before local models come back, 1 to 60 |
//! | `localGames` | the programs that unload local models while they run (a list of names like `game.exe`) |
//! | `localRestartTask` | the Windows scheduled task Helm ends while local models are unloaded and starts after |
//! | `localPauseFile` | the file Helm writes while local models are unloaded |
//! | `localServers` | **needs approval:** the servers themselves: name, address, folder, preset (a list of records) |
//!
//! The first ten are changed by a chat itself: each is bounded and shows in "Changed by chats"
//! with Undo. `localServers` is the one a chat stages (`app_stage_settings`) for the user to
//! approve: a server's start folder and preset make Helm start a program. A server's key is a
//! secret and never passes through a chat: stage `credentials` for the local models page and the
//! user types it there.

use crate::settings::{self, Changes, CompareModel, LocalSettings, ToolsSetup};
use helm_hosttools::{SettingsExtension, StagedPart};
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

pub const CONTEXT: &str = "localOllamaContext";
pub const KEEP_ALIVE: &str = "localKeepAliveMinutes";
pub const ENABLED: &str = "localServersEnabled";
pub const SPEECH_RESERVE: &str = "localSpeechReserveGb";
pub const TOOLS: &str = "localToolsSetup";
pub const COMPARE: &str = "localCompareModel";
pub const GAME_LIFT: &str = "localGameLiftMinutes";
pub const GAMES: &str = "localGames";
pub const RESTART_TASK: &str = "localRestartTask";
pub const PAUSE_FILE: &str = "localPauseFile";
pub const SERVERS: &str = "localServers";

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
        SPEECH_RESERVE => json!(s.speech_reserve_gb),
        TOOLS => serde_json::to_value(s.tools).unwrap_or(Value::Null),
        GAME_LIFT => json!(s.game_lift_minutes),
        GAMES => json!(s.games),
        SERVERS => json!(s.servers),
        // Empty text is "none".
        RESTART_TASK => json!(s.restart_task.clone().unwrap_or_default()),
        PAUSE_FILE => json!(s.pause_file.clone().unwrap_or_default()),
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
                    .map(|s| settings::Server {
                        enabled: true,
                        ..s.clone()
                    })
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
            c.ollama_context = Some(v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(
                || {
                    format!(
                        "{key} is a whole number of tokens from {} to {}.",
                        settings::MIN_CONTEXT,
                        settings::MAX_CONTEXT
                    )
                },
            )?)
        }
        KEEP_ALIVE => {
            c.keep_alive_minutes = Some(v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(
                || {
                    format!(
                        "{key} is a whole number of minutes from {} to {}.",
                        settings::MIN_KEEP_ALIVE,
                        settings::MAX_KEEP_ALIVE
                    )
                },
            )?)
        }
        SPEECH_RESERVE => {
            c.speech_reserve_gb = Some(v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(
                || {
                    format!(
                        "{key} is a whole number of GB from {} to {}.",
                        settings::MIN_SPEECH_RESERVE_GB,
                        settings::MAX_SPEECH_RESERVE_GB
                    )
                },
            )?)
        }
        TOOLS => {
            c.tools = Some(
                serde_json::from_value::<ToolsSetup>(v)
                    .map_err(|_| format!("{key} is \"lean\" or \"full\"."))?,
            )
        }
        GAME_LIFT => {
            c.game_lift_minutes = Some(v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(
                || {
                    format!(
                        "{key} is a whole number of minutes from {} to {}.",
                        settings::MIN_GAME_LIFT,
                        settings::MAX_GAME_LIFT
                    )
                },
            )?)
        }
        GAMES => {
            let list = v
                .as_array()
                .ok_or_else(|| format!("{key} is a list of program names like game.exe."))?;
            c.games = Some(
                list.iter()
                    .map(|g| {
                        g.as_str().map(str::to_owned).ok_or_else(|| {
                            format!("{key} is a list of program names like game.exe.")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        RESTART_TASK | PAUSE_FILE => {
            let text = match &v {
                Value::Null => String::new(),
                Value::String(t) => t.clone(),
                _ => return Err(format!("{key} is text, or empty for none.")),
            };
            if key == RESTART_TASK {
                c.restart_task = Some(text);
            } else {
                c.pause_file = Some(text);
            }
        }
        COMPARE => {
            c.compare_with = Some(
                serde_json::from_value::<CompareModel>(v)
                    .map_err(|_| format!("{key} is \"opus\", \"sonnet\" or \"haiku\"."))?,
            )
        }
        SERVERS => {
            let list: Vec<settings::ServerInput> = serde_json::from_value(v).map_err(|e| {
                format!("{key} is a list of servers {{id?, name, kind, baseUrl, enabled?, model?, autoStart?, folder?, preset?, chatCap?}}: {e}")
            })?;
            c.servers = Some(list);
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
                    auto_start: s.auto_start,
                    folder: s.folder.clone(),
                    preset: s.preset.clone(),
                    chat_cap: s.chat_cap,
                });
            }
            if let Some(unknown) = map
                .keys()
                .find(|k| !cur.servers.iter().any(|s| &s.id == *k))
            {
                let ids: Vec<&str> = cur.servers.iter().map(|s| s.id.as_str()).collect();
                return Err(format!(
                    "{key}.{unknown}: there is no such server. The servers are {}. To add or change one, stage localServers with app_stage_settings (the user approves it).",
                    ids.join(", ")
                ));
            }
            c.servers = Some(servers);
        }
        _ => return Err(format!("{key} is not a local model setting.")),
    }
    Ok(c)
}

fn put(
    tx: &Transaction<'_>,
    key: &str,
    value: &Value,
) -> Result<(LocalSettings, LocalSettings), String> {
    let before = settings::load(tx).map_err(sql)?;
    let after = settings::apply_changes(&before, &change(key, &before, value)?)?;
    settings::store(tx, &after).map_err(sql)?;
    Ok((before, after))
}

impl SettingsExtension for LocalKeys {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        &[]
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![
            CONTEXT,
            KEEP_ALIVE,
            ENABLED,
            SPEECH_RESERVE,
            TOOLS,
            COMPARE,
            GAME_LIFT,
            GAMES,
            RESTART_TASK,
            PAUSE_FILE,
            SERVERS,
        ]
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
                "about": format!("{PAGE}: whether Helm uses each local model server, by server id (read it to see the ids). Send only the servers to change. A server that is off shows no models and refuses new turns. Adding, removing or re-addressing a server is localServers, which you stage with app_stage_settings; a server's key is typed by the user (stage credentials for the local models page)."),
                "valueType": "boolean",
                "default": "every server on",
            })),
            SPEECH_RESERVE => Some(json!({
                "type": "integer",
                "min": settings::MIN_SPEECH_RESERVE_GB,
                "max": settings::MAX_SPEECH_RESERVE_GB,
                "default": d.speech_reserve_gb,
                "about": format!("{PAGE} and Settings > Shell > Dictation, Keep GPU memory for speech: graphics memory, in GB, that Helm frees as well when it has to unload other local models to load one, so speech-to-text (dictation, and the voice line's transcription) still fits beside it. Speech-to-text never unloads a model, and this alone never does either. 0 keeps none. It applies to the next model load."),
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
            GAME_LIFT => Some(json!({
                "type": "integer",
                "min": settings::MIN_GAME_LIFT,
                "max": settings::MAX_GAME_LIFT,
                "default": d.game_lift_minutes,
                "about": format!("{PAGE}, Wait after a game closes: minutes after the last listed game closes before local models come back."),
            })),
            SERVERS => Some(json!({
                "type": "array",
                "maxItems": settings::MAX_SERVERS,
                "default": d.servers,
                "about": format!("{PAGE}: the model servers Helm uses, each {{id, name, kind (strata or ollama), baseUrl (an address on this computer), enabled, model, autoStart, folder (Strata's install folder), preset (Strata's launch preset), chatCap}}. A server's folder and preset make Helm start a program, so a change is staged with app_stage_settings for the user to approve; leave out id for a new server, and a server left out of the list is removed. A server's key is not here: stage credentials for the local models page."),
            })),
            GAMES => Some(json!({
                "type": "array",
                "valueType": "string",
                "maxItems": settings::MAX_GAMES,
                "default": [],
                "about": format!("{PAGE}, Games: program names (like game.exe, no folder) that unload local models while they run. An empty list watches nothing. The list is replaced whole."),
            })),
            RESTART_TASK => Some(json!({
                "type": "string",
                "maxLength": settings::MAX_TASK,
                "default": "",
                "about": format!("{PAGE}, Restart task: the name of a Windows scheduled task that restarts a model server. Helm ends it while local models are unloaded and starts it again after. Empty means none."),
            })),
            PAUSE_FILE => Some(json!({
                "type": "string",
                "maxLength": settings::MAX_PAUSE_FILE,
                "default": "",
                "about": format!("{PAGE}, Pause file: a full path of a file Helm writes while local models are unloaded, for the user's own restart script to check. Empty means none."),
            })),
            _ => None,
        }
    }

    fn read(&self, tx: &Transaction<'_>, key: &str) -> Value {
        value_of(&settings::load(tx).unwrap_or_default(), key)
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
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
        let apply = |key: &str, v: Value| {
            db.write(|tx| Ok::<_, rusqlite::Error>(ext.apply(tx, key, &v)))
                .unwrap()
        };
        let read = |key: &str| {
            db.snapshot(|tx| Ok::<_, rusqlite::Error>(ext.read(tx, key)))
                .unwrap()
        };
        // The keys, their descriptions and their defaults.
        assert_eq!(ext.keys().len(), 11);
        for k in ext.keys() {
            assert!(ext.describe(k).is_some(), "{k}");
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
        assert!(apply(CONTEXT, json!("big"))
            .unwrap_err()
            .contains("whole number"));
        assert!(apply(KEEP_ALIVE, json!(0))
            .unwrap_err()
            .contains("keep-alive"));
        apply(KEEP_ALIVE, json!(60)).unwrap();
        assert_eq!(read(SPEECH_RESERVE), json!(2));
        assert_eq!(
            apply(SPEECH_RESERVE, json!(4)).unwrap(),
            (json!(2), json!(4))
        );
        assert!(apply(SPEECH_RESERVE, json!(30))
            .unwrap_err()
            .contains("kept for speech"));
        assert!(apply(SPEECH_RESERVE, json!("lots"))
            .unwrap_err()
            .contains("whole number"));
        apply(TOOLS, json!("full")).unwrap();
        assert!(apply(TOOLS, json!("heavy")).unwrap_err().contains("lean"));
        apply(COMPARE, json!("haiku")).unwrap();
        // Servers: only enabled flags, by id, merged; others are the user's.
        let (_, after) = apply(ENABLED, json!({"ollama": false})).unwrap();
        assert_eq!(after, json!({"strata": true, "ollama": false}));
        let why = apply(ENABLED, json!({"rogue": true})).unwrap_err();
        assert!(why.contains("no such server") && why.contains("stage localServers"), "{why}");
        assert!(apply(ENABLED, json!({"strata": "yes"}))
            .unwrap_err()
            .contains("true or false"));
        let s = db.snapshot(settings::load).unwrap();
        assert_eq!(
            s.servers[1].base_url,
            settings::OLLAMA_URL,
            "the enabled flags never move an address"
        );
        // The servers themselves are one record each; a new one joins the list, a bad address
        // is refused, and what was left out is gone. (A chat stages this; approval applies it.)
        let mut list = read(SERVERS);
        assert_eq!(list.as_array().unwrap().len(), 2);
        list.as_array_mut().unwrap().push(
            json!({"name": "Mine", "kind": "ollama", "baseUrl": "http://127.0.0.1:11500"}),
        );
        let (_, after) = apply(SERVERS, list.clone()).unwrap();
        assert_eq!(after.as_array().unwrap().len(), 3);
        assert_eq!(after[2]["id"], "mine");
        list.as_array_mut().unwrap()[2]["baseUrl"] = json!("http://example.com");
        assert!(apply(SERVERS, list).unwrap_err().contains("Server 3"));
        assert!(apply(SERVERS, json!("strata"))
            .unwrap_err()
            .contains("list of servers"));
        apply(SERVERS, Value::Null).unwrap();
        assert_eq!(read(SERVERS).as_array().unwrap().len(), 2);
        assert_eq!(
            (
                s.ollama_context,
                s.keep_alive_minutes,
                s.tools,
                s.compare_with
            ),
            (32_768, 60, ToolsSetup::Full, CompareModel::Haiku)
        );
        // null resets one setting; Undo restores the value from before.
        apply(ENABLED, Value::Null).unwrap();
        assert_eq!(read(ENABLED), json!({"strata": true, "ollama": true}));
        apply(CONTEXT, Value::Null).unwrap();
        assert_eq!(read(CONTEXT), json!(65_536));
        let (before, _) = apply(KEEP_ALIVE, json!(5)).unwrap();
        db.write(|tx| Ok::<_, rusqlite::Error>(ext.restore(tx, KEEP_ALIVE, &before)))
            .unwrap()
            .unwrap();
        assert_eq!(read(KEEP_ALIVE), json!(60));
        // The wait, and the three a chat only stages: the games, the restart task, the pause file.
        assert_eq!(apply(GAME_LIFT, json!(10)).unwrap(), (json!(5), json!(10)));
        assert!(apply(GAME_LIFT, json!(99)).unwrap_err().contains("minutes"));
        let (before, after) = apply(GAMES, json!(["Some.EXE", " other.exe ", "some.exe"])).unwrap();
        assert_eq!(
            (before, after),
            (json!([]), json!(["Some.EXE", "other.exe"]))
        );
        assert!(apply(GAMES, json!(["C:\\Games\\x.exe"]))
            .unwrap_err()
            .contains("program name"));
        assert!(apply(GAMES, json!("game.exe"))
            .unwrap_err()
            .contains("list"));
        assert_eq!(
            apply(RESTART_TASK, json!(" Model Restart ")).unwrap(),
            (json!(""), json!("Model Restart"))
        );
        assert!(apply(RESTART_TASK, json!("a\"b"))
            .unwrap_err()
            .contains("task"));
        assert_eq!(apply(RESTART_TASK, Value::Null).unwrap().1, json!(""));
        assert_eq!(
            apply(PAUSE_FILE, json!("D:\\models\\paused.flag"))
                .unwrap()
                .1,
            json!("D:\\models\\paused.flag")
        );
        assert!(apply(PAUSE_FILE, json!("paused.flag"))
            .unwrap_err()
            .contains("full path"));
        assert_eq!(read(GAMES), json!(["Some.EXE", "other.exe"]));
        assert_eq!(ext.changed_event(CONTEXT).kind, "local_models");
    }
}
