//! Preferences.
//!
//! `preferences` maps a key to a JSON value. Writes are strict: the key must be classified, and the
//! generic writer accepts only [`GENERIC_ALLOWLIST`], validating each value. Reads are lenient: a
//! missing row or one whose JSON does not parse reads as `None`, which every reader maps to its
//! default.

pub mod classes;
mod store;

pub use classes::{classify, PrefClass, PrefKey, Tier};

use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum PrefError {
    #[error("unknown preference")]
    UnknownPreference,
    #[error("unclassified preference key {0:?}")]
    Unclassified(String),
    #[error("invalid value for {key}: {why}")]
    Invalid {
        key: &'static str,
        why: &'static str,
    },
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

pub const SIDEBAR_VISIBLE: PrefKey = PrefKey::classified("sidebarVisible");
pub const INSPECTOR_VISIBLE: PrefKey = PrefKey::classified("inspectorVisible");
pub const SIDEBAR_WIDTH: PrefKey = PrefKey::classified("sidebarWidth");
pub const INSPECTOR_WIDTH: PrefKey = PrefKey::classified("inspectorWidth");
pub const SIDEBAR_VIEW: PrefKey = PrefKey::classified("sidebarView");
pub const TERMINAL_HEIGHT: PrefKey = PrefKey::classified("terminalHeight");
pub const THEME: PrefKey = PrefKey::classified("theme");
pub const COMPACT_SIDEBAR: PrefKey = PrefKey::classified("compactSidebar");
/// Settings > General text sizes (`{interface, response, review}`) and zoom (50 to 200, steps of 10).
pub const FONT_SIZES: PrefKey = PrefKey::classified("fontSizes");
pub const ZOOM_LEVEL: PrefKey = PrefKey::classified("zoomLevel");
/// Settings > Chat: how the composer sends ("mod-enter" or "enter").
pub const SEND_SHORTCUT: PrefKey = PrefKey::classified("sendShortcut");
/// Settings > MCP: whether chats load the user's own Claude Code MCP servers.
pub const NATIVE_MCP: PrefKey = PrefKey::classified("nativeMcp");
/// Send behaviour. Stored as the keys that differ from the defaults, which
/// live in `helm-chats` (`send_behavior::HOME_DEFAULTS`); validated here by shape.
pub const SEND_BEHAVIOR: PrefKey = PrefKey::classified("sendBehavior");
/// The selected chat and project (`{chatId, projectId}`, each a string or null).
pub const SELECTION: PrefKey = PrefKey::classified("selection");
/// Colour mode: "system", "light" or "dark".
pub const APPEARANCE: PrefKey = PrefKey::classified("appearance");
/// The user's key map: command id → chord, or null for a cleared binding.
pub const KEYBINDINGS: PrefKey = PrefKey::classified("keybindings");
/// Minutes without worker output before a running chat shows as stalled (1 to 1440, default 5).
pub const STALL_MINUTES: PrefKey = PrefKey::classified("stallMinutes");
/// Transcript element toggles: `{tokens, turnTime, fileChanges, timeline}` each
/// `shown` or `hidden`; strict on write, a missing key reads as shown.
pub const CHAT_ELEMENTS: PrefKey = PrefKey::classified("chatElements");
/// Composer autocomplete: `{enabled}`, off when missing.
pub const AUTOCOMPLETE: PrefKey = PrefKey::classified("autocomplete");
/// The idle-chat check: `{enabled, delayMinutes, prompt}`, on with a 15 minute delay when missing.
pub const TASK_TRIAGE: PrefKey = PrefKey::classified("taskTriage");
/// Settings > General: the sidebar entries the user hides, by id.
pub const HIDDEN_SIDEBAR_ITEMS: PrefKey = PrefKey::classified("hiddenSidebarItems");
/// Settings > Chat: skill names hidden from the `/` menu, without a leading `$`.
pub const HIDDEN_SKILLS: PrefKey = PrefKey::classified("hiddenSkills");

/// Keys the generic `preference` op accepts. Every other key has its own feature operation.
/// The list starts with the layout keys and grows it
/// as features land.
pub const GENERIC_ALLOWLIST: &[PrefKey] = &[
    SIDEBAR_VISIBLE,
    INSPECTOR_VISIBLE,
    SIDEBAR_WIDTH,
    INSPECTOR_WIDTH,
    SIDEBAR_VIEW,
    TERMINAL_HEIGHT,
    THEME,
    COMPACT_SIDEBAR,
    FONT_SIZES,
    ZOOM_LEVEL,
    SEND_SHORTCUT,
    NATIVE_MCP,
    SEND_BEHAVIOR,
    SELECTION,
    APPEARANCE,
    KEYBINDINGS,
    STALL_MINUTES,
    CHAT_ELEMENTS,
    AUTOCOMPLETE,
    TASK_TRIAGE,
    HIDDEN_SIDEBAR_ITEMS,
    HIDDEN_SKILLS,
];

/// The stored value, or `None` when the row is missing or its JSON does not parse.
pub fn read(conn: &Connection, key: &str) -> Option<Value> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM preferences WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
        .ok()
        .flatten();
    raw.and_then(|s| serde_json::from_str(&s).ok())
}

/// The stored value as `T`, or `default` when missing, unparseable or the wrong shape.
pub fn get_or<T: DeserializeOwned>(conn: &Connection, key: PrefKey, default: T) -> T {
    read(conn, key.name())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or(default)
}

/// Generic validation; other keys are validated by their owning feature.
pub fn validate(key: PrefKey, value: &Value) -> Result<(), PrefError> {
    let k = key.name();
    let int_in = |lo: i64, hi: i64, why: &'static str| match value.as_i64() {
        Some(n) if (lo..=hi).contains(&n) => Ok(()),
        _ => Err(PrefError::Invalid { key: k, why }),
    };
    match k {
        "sidebarView" => match value.as_str() {
            Some("projects" | "activity") => Ok(()),
            _ => Err(PrefError::Invalid {
                key: k,
                why: "must be projects or activity",
            }),
        },
        "sidebarVisible" | "inspectorVisible" | "compactSidebar" => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err(PrefError::Invalid {
                    key: k,
                    why: "must be true or false",
                })
            }
        }
        "nativeMcp" => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err(PrefError::Invalid {
                    key: k,
                    why: "must be true or false",
                })
            }
        }
        "zoomLevel" => match value.as_i64() {
            Some(n) if (50..=200).contains(&n) && n % 10 == 0 => Ok(()),
            _ => Err(PrefError::Invalid {
                key: k,
                why: "must be 50 to 200 in steps of 10",
            }),
        },
        "sendShortcut" => match value.as_str() {
            Some("enter" | "mod-enter") => Ok(()),
            _ => Err(PrefError::Invalid {
                key: k,
                why: "must be enter or mod-enter",
            }),
        },
        "fontSizes" => match value.as_object() {
            Some(m)
                if m.iter().all(|(name, v)| match (name.as_str(), v.as_i64()) {
                    ("interface", Some(n)) => (11..=20).contains(&n),
                    ("response", Some(n)) => (11..=24).contains(&n),
                    ("review", Some(n)) => (9..=20).contains(&n),
                    _ => false,
                }) =>
            {
                Ok(())
            }
            _ => Err(PrefError::Invalid {
                key: k,
                why: "maps interface (11-20), response (11-24) and review (9-20) to whole sizes",
            }),
        },
        "sidebarWidth" => int_in(200, 4096, "must be 200 to 4096"),
        "inspectorWidth" => int_in(280, 4096, "must be 280 to 4096"),
        "terminalHeight" => int_in(80, 4096, "must be 80 to 4096"),
        "stallMinutes" => int_in(1, 1440, "must be 1 to 1440"),
        "autocomplete" => match value.as_object() {
            Some(m) if m.len() == 1 && m.get("enabled").is_some_and(Value::is_boolean) => Ok(()),
            _ => Err(PrefError::Invalid {
                key: k,
                why: "is {enabled: true or false}",
            }),
        },
        "taskTriage" => match value.as_object() {
            Some(m)
                if m.iter().all(|(k, v)| match k.as_str() {
                    "enabled" => v.is_boolean(),
                    "delayMinutes" => v.as_i64().is_some_and(|n| (0..=1440).contains(&n)),
                    "prompt" => v.is_null() || v.as_str().is_some_and(|s| s.len() <= 8000),
                    _ => false,
                }) =>
            {
                Ok(())
            }
            _ => Err(PrefError::Invalid {
                key: k,
                why: "is {enabled, delayMinutes (0 to 1440), prompt (text or null)}",
            }),
        },
        "chatElements" => match value.as_object() {
            Some(m)
                if m.iter().all(|(k, v)| {
                    matches!(
                        k.as_str(),
                        "tokens" | "turnTime" | "fileChanges" | "timeline"
                    ) && matches!(v.as_str(), Some("shown" | "hidden"))
                }) =>
            {
                Ok(())
            }
            _ => Err(PrefError::Invalid {
                key: k,
                why: "maps tokens, turnTime, fileChanges and timeline to shown or hidden",
            }),
        },
        "sendBehavior" => validate_send_behavior(value),
        "hiddenSidebarItems" => validate_name_list(
            value,
            "hiddenSidebarItems",
            9,
            |n| SIDEBAR_ITEM_IDS.contains(&n),
            "lists sidebar entries by id, each once",
        ),
        "hiddenSkills" => validate_name_list(
            value,
            "hiddenSkills",
            512,
            |n| !n.is_empty() && n.len() <= 128 && !n.starts_with('$'),
            "lists skill names without a leading $, each once",
        ),
        "selection" => validate_selection(value),
        "appearance" => match value.as_str() {
            Some("system" | "light" | "dark") => Ok(()),
            _ => Err(PrefError::Invalid {
                key: k,
                why: "must be system, light or dark",
            }),
        },
        "keybindings" => validate_keybindings(value),
        _ => Ok(()),
    }
}

/// `keybindings`: at most 500 entries, ids and chords of at most 100 bytes; a null chord is a
/// cleared binding. Chord grammar and conflicts are checked by the UI's registry.
fn validate_keybindings(value: &Value) -> Result<(), PrefError> {
    let bad = |why: &'static str| PrefError::Invalid {
        key: "keybindings",
        why,
    };
    let Some(map) = value.as_object() else {
        return Err(bad("must be an object"));
    };
    if map.len() > 500 {
        return Err(bad("has more than 500 entries"));
    }
    for (k, v) in map {
        let ok_chord = v.is_null() || v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 100);
        if k.is_empty() || k.len() > 100 || !ok_chord {
            return Err(bad("entries must map an id to a chord or null"));
        }
    }
    Ok(())
}

/// `selection` is `{chatId, projectId}`; each is null or an id of at most 200 bytes.
fn validate_selection(value: &Value) -> Result<(), PrefError> {
    let bad = |why: &'static str| PrefError::Invalid {
        key: "selection",
        why,
    };
    let Some(map) = value.as_object() else {
        return Err(bad("must be an object"));
    };
    for (k, v) in map {
        if !matches!(k.as_str(), "chatId" | "projectId") {
            return Err(bad("has an unknown key"));
        }
        let ok = v.is_null() || v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 200);
        if !ok {
            return Err(bad("ids must be null or 1 to 200 bytes"));
        }
    }
    Ok(())
}

/// The sidebar entries that can be hidden.
const SIDEBAR_ITEM_IDS: &[&str] = &[
    "newChat",
    "search",
    "swarm",
    "attention",
    "tasks",
    "saved",
    "schedules",
    "archived",
    "coder",
];

/// A list of distinct strings, at most `max` long, each accepted by `ok`.
fn validate_name_list(
    value: &Value,
    key: &'static str,
    max: usize,
    ok: impl Fn(&str) -> bool,
    why: &'static str,
) -> Result<(), PrefError> {
    let bad = PrefError::Invalid { key, why };
    let Some(items) = value.as_array() else {
        return Err(bad);
    };
    let mut seen = std::collections::HashSet::new();
    let fine = items.len() <= max
        && items
            .iter()
            .all(|v| v.as_str().is_some_and(|n| ok(n) && seen.insert(n)));
    if fine {
        Ok(())
    } else {
        Err(bad)
    }
}

/// `sendBehavior` holds a subset of its six keys; unknown keys are refused.
fn validate_send_behavior(value: &Value) -> Result<(), PrefError> {
    let bad = |why: &'static str| PrefError::Invalid {
        key: "sendBehavior",
        why,
    };
    let Some(map) = value.as_object() else {
        return Err(bad("must be an object"));
    };
    for (k, v) in map {
        let ok = match k.as_str() {
            // "queue" is a retired choice; it is still accepted so an old stored value is not an error
            // (it reads as "asap").
            "mode" => matches!(v.as_str(), Some("queue" | "asap" | "interrupt")),
            "showChoices" | "stopSendsWaiting" | "toolStop" => v.is_boolean(),
            "waitLimitSeconds" => {
                v.is_null() || v.as_u64().is_some_and(|n| (5..=3600).contains(&n))
            }
            "longCommands" => matches!(v.as_str(), Some("off" | "warn" | "redirect")),
            _ => return Err(bad("has an unknown key")),
        };
        if !ok {
            return Err(bad("has a value out of range"));
        }
    }
    Ok(())
}

/// Write a known key through the single writer. `Value::Null` deletes the row.
pub fn set(tx: &Transaction<'_>, key: PrefKey, value: &Value) -> Result<(), PrefError> {
    if value.is_null() {
        return store::delete_row(tx, key.name());
    }
    validate(key, value)?;
    store::put_row(tx, key.name(), value)
}

/// The generic writer behind the `preference` op: allowlisted keys only.
pub fn set_generic(tx: &Transaction<'_>, key: &str, value: &Value) -> Result<PrefKey, PrefError> {
    let key = GENERIC_ALLOWLIST
        .iter()
        .copied()
        .find(|k| k.name() == key)
        .ok_or(PrefError::UnknownPreference)?;
    set(tx, key, value)?;
    Ok(key)
}

/// Write a dynamic or otherwise runtime-named key. Unclassified keys are refused by the writer
/// (a panic in debug builds).
pub fn set_raw(tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), PrefError> {
    if value.is_null() {
        store::delete_row(tx, key)
    } else {
        store::put_row(tx, key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{tests::temp_db, MigrationRegistry};
    use serde_json::json;

    #[test]
    fn round_trip_and_delete() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        db.write(|tx| set(tx, SIDEBAR_WIDTH, &json!(320))).unwrap();
        let w: i64 = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(get_or(tx, SIDEBAR_WIDTH, 0)))
            .unwrap();
        assert_eq!(w, 320);
        db.write(|tx| set(tx, SIDEBAR_WIDTH, &Value::Null)).unwrap();
        let w: i64 = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(get_or(tx, SIDEBAR_WIDTH, 250)))
            .unwrap();
        assert_eq!(w, 250);
    }

    #[test]
    fn bad_json_reads_as_the_default() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        // Plant a corrupt row directly (simulating an older or damaged profile).
        db.write(|tx| {
            tx.execute(
                "INSERT INTO preferences(key, value) VALUES ('sidebarWidth', '{not json')",
                [],
            )
        })
        .unwrap();
        let w: i64 = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(get_or(tx, SIDEBAR_WIDTH, 250)))
            .unwrap();
        assert_eq!(w, 250);
        db.write(|tx| set(tx, SIDEBAR_WIDTH, &json!(300))).unwrap();
        // A readable row of the wrong shape also falls back.
        let wrong_shape: bool = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(get_or(tx, SIDEBAR_WIDTH, false)))
            .unwrap();
        assert!(!wrong_shape);
    }

    #[test]
    fn generic_writer_refuses_unknown_keys_and_bad_values() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let r = db.write(|tx| set_generic(tx, "agentSettingsAll", &json!(true)));
        assert!(matches!(r, Err(PrefError::UnknownPreference)));
        let r = db.write(|tx| set_generic(tx, "sidebarWidth", &json!(10)));
        assert!(matches!(r, Err(PrefError::Invalid { .. })));
        let r = db.write(|tx| set_generic(tx, "sidebarView", &json!("tree")));
        assert!(matches!(r, Err(PrefError::Invalid { .. })));
        assert!(db
            .write(|tx| set_generic(tx, "sidebarView", &json!("activity")))
            .is_ok());
    }

    #[test]
    fn settings_general_chat_and_mcp_keys_save_through_the_generic_writer() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        for (key, ok, bad) in [
            ("zoomLevel", json!(130), json!(125)),
            ("sendShortcut", json!("enter"), json!("ctrl-enter")),
            ("nativeMcp", json!(false), json!("no")),
            (
                "fontSizes",
                json!({"interface": 14, "response": 16}),
                json!({"interface": 40}),
            ),
        ] {
            assert!(db.write(|tx| set_generic(tx, key, &ok)).is_ok(), "{key}");
            let r = db.write(|tx| set_generic(tx, key, &bad));
            assert!(matches!(r, Err(PrefError::Invalid { .. })), "{key}");
        }
        let z = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(read(tx, "zoomLevel")))
            .unwrap();
        assert_eq!(z, Some(json!(130)));
    }

    #[test]
    fn appearance_and_keybindings_are_generic_and_validated() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        assert!(db
            .write(|tx| set_generic(tx, "appearance", &json!("dark")))
            .is_ok());
        let r = db.write(|tx| set_generic(tx, "appearance", &json!("sepia")));
        assert!(matches!(r, Err(PrefError::Invalid { .. })));
        let ok = json!({"settings.toggle": "Ctrl+,", "panel.toggle": null});
        assert!(db.write(|tx| set_generic(tx, "keybindings", &ok)).is_ok());
        for bad in [
            json!([]),
            json!({"a": 5}),
            json!({"a": ""}),
            json!({"": "Ctrl+K"}),
        ] {
            let r = db.write(|tx| set_generic(tx, "keybindings", &bad));
            assert!(matches!(r, Err(PrefError::Invalid { .. })), "{bad}");
        }
    }

    #[test]
    fn hidden_lists_are_generic_and_validated() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        for (key, ok, bad) in [
            (
                "hiddenSidebarItems",
                json!(["search", "archived"]),
                json!(["search", "nope"]),
            ),
            ("hiddenSidebarItems", json!([]), json!(["search", "search"])),
            (
                "hiddenSkills",
                json!(["review", "deploy-check"]),
                json!(["$deploy"]),
            ),
            ("hiddenSkills", json!([]), json!([""])),
            ("hiddenSkills", json!(["a"]), json!("a")),
        ] {
            assert!(db.write(|tx| set_generic(tx, key, &ok)).is_ok(), "{key}");
            let r = db.write(|tx| set_generic(tx, key, &bad));
            assert!(matches!(r, Err(PrefError::Invalid { .. })), "{key} {bad}");
        }
    }

    #[test]
    fn selection_is_generic_and_validated() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let ok = json!({"chatId": "c1", "projectId": null});
        assert!(db.write(|tx| set_generic(tx, "selection", &ok)).is_ok());
        for bad in [
            json!("c1"),
            json!({"chatId": 5}),
            json!({"chatId": ""}),
            json!({"chatId": "x".repeat(201)}),
            json!({"other": "x"}),
        ] {
            let r = db.write(|tx| set_generic(tx, "selection", &bad));
            assert!(matches!(r, Err(PrefError::Invalid { .. })), "{bad}");
        }
    }

    #[test]
    fn send_behavior_refuses_unknown_keys_and_bad_values() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let ok = json!({"mode": "queue", "waitLimitSeconds": null, "toolStop": false});
        assert!(db.write(|tx| set_generic(tx, "sendBehavior", &ok)).is_ok());
        for bad in [
            json!({"mode": "sideways"}),
            json!({"waitLimitSeconds": 4}),
            json!({"waitLimitSeconds": 3601}),
            json!({"longCommands": "loud"}),
            json!({"toolStop": "yes"}),
            json!({"colour": "red"}),
            json!([1]),
        ] {
            let r = db.write(|tx| set_generic(tx, "sendBehavior", &bad));
            assert!(matches!(r, Err(PrefError::Invalid { .. })), "{bad}");
        }
        assert!(db
            .write(|tx| set_generic(tx, "sendBehavior", &json!({"waitLimitSeconds": 5})))
            .is_ok());
    }

    #[test]
    #[should_panic(expected = "unclassified preference key")]
    fn writing_an_unclassified_key_panics_in_debug() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let _ = db.write(|tx| set_raw(tx, "neverClassified", &json!(1)));
    }

    #[test]
    fn dynamic_keys_are_writable() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        db.write(|tx| set_raw(tx, "autocompleteBudget:chat-1", &json!([1, 2])))
            .unwrap();
        let v = db.snapshot(|tx| Ok::<_, rusqlite::Error>(read(tx, "autocompleteBudget:chat-1")));
        assert_eq!(v.unwrap(), Some(json!([1, 2])));
    }
}
