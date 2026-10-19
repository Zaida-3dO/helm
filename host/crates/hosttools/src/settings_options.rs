//! What each agent-visible setting accepts: its shape, its default, and a check that refuses a
//! wrong value with a sentence an agent can act on.
//!
//! `app_get_settings` reports [`describe`] under `options`, so an agent does not have to read the
//! app's source to learn that `sendShortcut` takes `enter` or `mod-enter`. `app_update_settings`
//! runs [`check`] before the write. `null` is never checked: it deletes the row, which restores the
//! default. The core's own validators (`helm_core::prefs::validate`) still run after this one.

use serde_json::{json, Map, Value};

/// A value's shape.
enum Kind {
    Bool,
    /// One of these strings.
    Choice(&'static [&'static str]),
    /// A whole number in `min..=max`, a multiple of `step` counted from `min`.
    Int {
        min: i64,
        max: i64,
        step: i64,
    },
    /// A string of at most `max` bytes.
    Text {
        max: usize,
    },
    /// A string of at most `max` bytes or null.
    TextOrNull {
        max: usize,
    },
    /// A list of at most `max` items, each of kind `of`.
    List {
        of: &'static Kind,
        max: usize,
        distinct: bool,
    },
    /// An object that may hold any subset of these fields (and nothing else).
    Fields(&'static [(&'static str, &'static Kind)]),
    /// An object whose keys are among `keys` (or any key when empty) and whose values are `of`.
    Map {
        keys: &'static [&'static str],
        of: &'static Kind,
        max: usize,
    },
    /// An object with no checked fields (the owning feature validates it).
    AnyObject,
    /// Any one of these.
    Either(&'static [&'static Kind]),
}

const BOOL: Kind = Kind::Bool;
const STR_256: Kind = Kind::Text { max: 256 };
const ID_128: Kind = Kind::Text { max: 128 };
const ID_200: Kind = Kind::Text { max: 200 };

/// What a setting is, its default (`null` when the default is Helm's built-in behaviour rather
/// than a stored value) and a sentence on what it does.
struct Entry {
    key: &'static str,
    kind: Kind,
    default: fn() -> Value,
    about: &'static str,
}

fn null() -> Value {
    Value::Null
}

const COLOUR_MODES: &[&str] = &["system", "light", "dark"];
const SIDEBAR_ITEMS: &[&str] = &[
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
const THEME_EFFECTS: &[&str] = &[
    "trail",
    "cursors",
    "cursorGlow",
    "edgeLights",
    "particles",
    "gloss",
    "frames",
    "typing",
    "results",
];

const SIDEBAR_ITEM: Kind = Kind::Choice(SIDEBAR_ITEMS);
const FONT_SIZE_RESPONSE: Kind = Kind::Int {
    min: 11,
    max: 24,
    step: 1,
};
const FONT_SIZE_REVIEW: Kind = Kind::Int {
    min: 9,
    max: 20,
    step: 1,
};
const FONT_SIZE_INTERFACE: Kind = Kind::Int {
    min: 11,
    max: 20,
    step: 1,
};
const VOLUME: Kind = Kind::Int {
    min: 0,
    max: 100,
    step: 1,
};
const DELAY_MINUTES: Kind = Kind::Int {
    min: 0,
    max: 1440,
    step: 1,
};
const SHOWN_OR_HIDDEN: Kind = Kind::Choice(&["shown", "hidden"]);
const CHORD: Kind = Kind::TextOrNull { max: 100 };
const LEVELS: Kind = Kind::Choice(&["zen", "standard", "brainrot", "custom"]);
const ANY_OBJECT: Kind = Kind::AnyObject;

fn table() -> Vec<Entry> {
    vec![
        Entry {
            key: "theme",
            kind: Kind::Choice(COLOUR_MODES),
            default: || json!("system"),
            about: "Not read by Helm: the colour mode is the appearance setting. Change appearance instead.",
        },
        Entry {
            key: "appearance",
            kind: Kind::Choice(COLOUR_MODES),
            default: || json!("system"),
            about: "Colour mode. system follows the operating system's light or dark setting.",
        },
        Entry {
            key: "interfaceTheme",
            kind: Kind::Choice(&[
                "default",
                "windows-xp",
                "slack",
                "chrome",
                "terminal",
                "intellij",
                "discord",
                "reddit",
                "wikipedia",
            ]),
            default: || json!("default"),
            about: "The interface skin.",
        },
        Entry {
            key: "codeThemes",
            kind: Kind::AnyObject,
            default: null,
            about: "Code colour themes: {defaults: {light, dark}, languages: {...}}. Null uses the bundled themes.",
        },
        Entry {
            key: "fontSizes",
            kind: Kind::Fields(&[
                ("response", &FONT_SIZE_RESPONSE),
                ("review", &FONT_SIZE_REVIEW),
                ("interface", &FONT_SIZE_INTERFACE),
            ]),
            default: || json!({"response": 14, "review": 10, "interface": 13}),
            about: "Text sizes in pixels. Send the whole object: it replaces the stored one.",
        },
        Entry {
            key: "fontFamilies",
            kind: Kind::Fields(&[("ui", &STR_256), ("code", &STR_256)]),
            default: || json!({"ui": "", "code": ""}),
            about: "Font names for the interface and for code; an empty string is the system font. Send the whole object: it replaces the stored one.",
        },
        Entry {
            key: "dopamine",
            kind: Kind::Either(&[&LEVELS, &ANY_OBJECT]),
            default: || json!("standard"),
            about: "How much motion and flair the interface shows: a level, or {version, mode, custom} for a custom set.",
        },
        Entry {
            key: "animationControls",
            kind: Kind::AnyObject,
            default: null,
            about: "Per-effect animation overrides. Null uses the level chosen by dopamine.",
        },
        Entry {
            key: "themeEffects",
            kind: Kind::Map {
                keys: THEME_EFFECTS,
                of: &BOOL,
                max: 16,
            },
            default: || json!({}),
            about: "Turns single theme effects on or off by id; an id left out follows the skin. Send the whole object.",
        },
        Entry {
            key: "cursorTrail",
            kind: Kind::Bool,
            default: || json!(true),
            about: "Allow the cursor trail effect.",
        },
        Entry {
            key: "customCursors",
            kind: Kind::Bool,
            default: || json!(true),
            about: "Allow custom cursors.",
        },
        Entry {
            key: "pauseAnimationsOnScroll",
            kind: Kind::Bool,
            default: || json!(false),
            about: "Pause animations while scrolling.",
        },
        Entry {
            key: "compactSidebar",
            kind: Kind::Bool,
            default: || json!(false),
            about: "A denser sidebar.",
        },
        Entry {
            key: "hiddenSidebarItems",
            kind: Kind::List {
                of: &SIDEBAR_ITEM,
                max: 9,
                distinct: true,
            },
            default: || json!([]),
            about: "Sidebar entries to hide, by id. An empty list shows them all.",
        },
        Entry {
            key: "moveCompletedTasksToTop",
            kind: Kind::Bool,
            default: || json!(false),
            about: "Show completed tasks at the top of the task list.",
        },
        Entry {
            key: "reviewWrap",
            kind: Kind::Bool,
            default: || json!(true),
            about: "Wrap long lines in diffs and review views.",
        },
        Entry {
            key: "zoomLevel",
            kind: Kind::Int {
                min: 50,
                max: 200,
                step: 10,
            },
            default: || json!(100),
            about: "Interface zoom as a percentage, in steps of 10.",
        },
        Entry {
            key: "sounds",
            kind: Kind::Fields(&[
                ("enabled", &BOOL),
                ("volume", &VOLUME),
                ("themed", &BOOL),
            ]),
            default: || json!({"enabled": true, "volume": 45, "themed": true}),
            about: "Interface sounds. Send the whole object: it replaces the stored one.",
        },
        Entry {
            key: "keepAwake",
            kind: Kind::Bool,
            default: || json!(true),
            about: "Keep the computer awake while a chat is running.",
        },
        Entry {
            key: "hiddenSkills",
            kind: Kind::List {
                of: &ID_128,
                max: 512,
                distinct: true,
            },
            default: || json!([]),
            about: "Skill names to hide from the skills list, without a leading $.",
        },
        Entry {
            key: "sidebarVisible",
            kind: Kind::Bool,
            default: || json!(true),
            about: "Whether the sidebar is shown.",
        },
        Entry {
            key: "inspectorVisible",
            kind: Kind::Bool,
            default: || json!(false),
            about: "Whether the side panel is shown.",
        },
        Entry {
            key: "sidebarWidth",
            kind: Kind::Int {
                min: 200,
                max: 4096,
                step: 1,
            },
            default: || json!(480),
            about: "Sidebar width in pixels.",
        },
        Entry {
            key: "inspectorWidth",
            kind: Kind::Int {
                min: 280,
                max: 4096,
                step: 1,
            },
            default: || json!(370),
            about: "Side panel width in pixels.",
        },
        Entry {
            key: "collapsedProjects",
            kind: Kind::List {
                of: &ID_200,
                max: 4096,
                distinct: false,
            },
            default: || json!([]),
            about: "Project ids collapsed in the sidebar. An empty list leaves them all expanded.",
        },
        Entry {
            key: "expandedChats",
            kind: Kind::List {
                of: &ID_200,
                max: 500,
                distinct: false,
            },
            default: || json!([]),
            about: "Chat ids whose sub-chats are expanded in the sidebar.",
        },
        Entry {
            key: "sendShortcut",
            kind: Kind::Choice(&["enter", "mod-enter"]),
            default: || json!("mod-enter"),
            about: "How the composer sends. enter sends on Enter (Shift+Enter is a new line); mod-enter sends on Ctrl+Enter (Enter is a new line). Takes effect at once.",
        },
        Entry {
            key: "defaultHarness",
            kind: Kind::Choice(&["claude", "codex"]),
            default: || json!("claude"),
            about: "The agent a new chat starts with. Only Claude Code runs chats today.",
        },
        Entry {
            key: "newChat",
            kind: Kind::AnyObject,
            default: null,
            about: "New-chat defaults: {harness, provider, model, effort}. Null uses each harness's own default.",
        },
        Entry {
            key: "autocomplete",
            kind: Kind::Fields(&[("enabled", &BOOL)]),
            default: || json!({"enabled": false}),
            about: "Composer autocomplete: {enabled: true or false}.",
        },
        Entry {
            key: "taskTriage",
            kind: Kind::Fields(&[
                ("enabled", &BOOL),
                ("delayMinutes", &DELAY_MINUTES),
                ("prompt", &Kind::TextOrNull { max: 8000 }),
            ]),
            default: || json!({"enabled": true, "delayMinutes": 15, "prompt": null}),
            about: "Task triage: {enabled, delayMinutes, prompt}. A null prompt is the built-in one. Send the whole object: it replaces the stored one.",
        },
        Entry {
            key: "chatElements",
            kind: Kind::Map {
                keys: &["tokens", "turnTime", "fileChanges", "timeline"],
                of: &SHOWN_OR_HIDDEN,
                max: 4,
            },
            default: || json!({}),
            about: "Which transcript elements show: ids tokens, turnTime, fileChanges, timeline, each shown or hidden. An id left out is shown. Send the whole object.",
        },
        Entry {
            key: "desktopPet",
            kind: Kind::AnyObject,
            default: null,
            about: "The desktop pet's settings. Null leaves the pet off.",
        },
        Entry {
            key: "projectsMode",
            kind: Kind::Bool,
            default: || json!(false),
            about: "Projects mode (repositories and the task board). Chats see its extra tools from their next launch.",
        },
        Entry {
            key: "tasks",
            kind: Kind::AnyObject,
            default: null,
            about: "The task-board connection. Change it only when the user explicitly asks.",
        },
        Entry {
            key: "keybindings",
            kind: Kind::Map {
                keys: &[],
                of: &CHORD,
                max: 500,
            },
            default: || json!({}),
            about: "Keyboard shortcuts as {commandId: chord}; a null chord clears that shortcut. Needs the all-settings switch.",
        },
    ]
}

fn quoted(items: &[&str]) -> String {
    let q: Vec<String> = items.iter().map(|s| format!("\"{s}\"")).collect();
    match q.len() {
        0 => String::new(),
        1 => q[0].clone(),
        n => format!("{} or {}", q[..n - 1].join(", "), q[n - 1]),
    }
}

impl Kind {
    /// The shape as JSON, for `app_get_settings`.
    fn describe(&self) -> Value {
        match self {
            Kind::Bool => json!({"type": "boolean"}),
            Kind::Choice(c) => json!({"type": "enum", "allowed": c}),
            Kind::Int { min, max, step } => {
                let mut o = json!({"type": "integer", "min": min, "max": max});
                if *step > 1 {
                    o["step"] = json!(step);
                }
                o
            }
            Kind::Text { max } => json!({"type": "string", "maxLength": max}),
            Kind::TextOrNull { max } => json!({"type": "string or null", "maxLength": max}),
            Kind::List { of, max, distinct } => {
                let mut o = json!({"type": "list", "items": of.describe(), "maxItems": max});
                if *distinct {
                    o["distinct"] = json!(true);
                }
                o
            }
            Kind::Fields(f) => {
                let fields: Map<String, Value> = f
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.describe()))
                    .collect();
                json!({"type": "object", "fields": fields, "note": "any of these fields; no others"})
            }
            Kind::Map { keys, of, max } => {
                let mut o = json!({"type": "object", "values": of.describe(), "maxEntries": max});
                if !keys.is_empty() {
                    o["keys"] = json!(keys);
                }
                o
            }
            Kind::AnyObject => json!({"type": "object"}),
            Kind::Either(k) => json!({"oneOf": k.iter().map(|k| k.describe()).collect::<Vec<_>>()}),
        }
    }

    /// What the value must be, as a phrase after "must be".
    fn expect(&self) -> String {
        match self {
            Kind::Bool => "true or false".into(),
            Kind::Choice(c) => format!("one of {}", quoted(c)),
            Kind::Int { min, max, step } if *step > 1 => {
                format!("a whole number from {min} to {max} in steps of {step}")
            }
            Kind::Int { min, max, .. } => format!("a whole number from {min} to {max}"),
            Kind::Text { max } => format!("text of at most {max} bytes"),
            Kind::TextOrNull { max } => format!("text of at most {max} bytes, or null"),
            Kind::List { max, .. } => format!("a list of at most {max} items"),
            Kind::Fields(f) => {
                let names: Vec<&str> = f.iter().map(|(k, _)| *k).collect();
                format!("an object with only these fields: {}", names.join(", "))
            }
            Kind::Map { keys: [], .. } => "an object".into(),
            Kind::Map { keys, .. } => format!("an object keyed by {}", quoted(keys)),
            Kind::AnyObject => "an object".into(),
            Kind::Either(k) => k
                .iter()
                .map(|k| k.expect())
                .collect::<Vec<_>>()
                .join(" or "),
        }
    }

    /// `Err(problem)` names the offending path and what it must be.
    fn check(&self, path: &str, v: &Value) -> Result<(), String> {
        let bad =
            |k: &Kind| -> Result<(), String> { Err(format!("{path} must be {}.", k.expect())) };
        match self {
            Kind::Bool if v.is_boolean() => Ok(()),
            Kind::Bool => bad(self),
            Kind::Choice(c) => match v.as_str() {
                Some(s) if c.contains(&s) => Ok(()),
                _ => bad(self),
            },
            Kind::Int { min, max, step } => match v.as_i64() {
                Some(n) if (*min..=*max).contains(&n) && (n - min) % step == 0 => Ok(()),
                _ => bad(self),
            },
            Kind::Text { max } => match v.as_str() {
                Some(s) if s.len() <= *max && !s.chars().any(char::is_control) => Ok(()),
                _ => bad(self),
            },
            Kind::TextOrNull { max } => match v {
                Value::Null => Ok(()),
                Value::String(s) if !s.is_empty() && s.len() <= *max => Ok(()),
                _ => bad(self),
            },
            Kind::List { of, max, distinct } => {
                let Some(items) = v.as_array() else {
                    return bad(self);
                };
                if items.len() > *max {
                    return bad(self);
                }
                for (i, item) in items.iter().enumerate() {
                    of.check(&format!("{path}[{i}]"), item)?;
                    if *distinct && items[..i].contains(item) {
                        return Err(format!("{path} lists {item} more than once."));
                    }
                }
                Ok(())
            }
            Kind::Fields(fields) => {
                let Some(o) = v.as_object() else {
                    return bad(self);
                };
                for (k, val) in o {
                    match fields.iter().find(|(name, _)| *name == k.as_str()) {
                        Some((_, kind)) => kind.check(&format!("{path}.{k}"), val)?,
                        None => {
                            let names: Vec<&str> = fields.iter().map(|(n, _)| *n).collect();
                            return Err(format!(
                                "{path} has no field {k:?}; its fields are {}.",
                                names.join(", ")
                            ));
                        }
                    }
                }
                Ok(())
            }
            Kind::Map { keys, of, max } => {
                let Some(o) = v.as_object() else {
                    return bad(self);
                };
                if o.len() > *max {
                    return Err(format!("{path} has more than {max} entries."));
                }
                for (k, val) in o {
                    if !keys.is_empty() && !keys.contains(&k.as_str()) {
                        return Err(format!(
                            "{path} has no entry {k:?}; the ids are {}.",
                            quoted(keys)
                        ));
                    }
                    of.check(&format!("{path}.{k}"), val)?;
                }
                Ok(())
            }
            Kind::AnyObject if v.is_object() => Ok(()),
            Kind::AnyObject => bad(self),
            Kind::Either(kinds) => {
                if kinds.iter().any(|k| k.check(path, v).is_ok()) {
                    Ok(())
                } else {
                    bad(self)
                }
            }
        }
    }
}

fn entry(key: &str) -> Option<Entry> {
    table().into_iter().find(|e| e.key == key)
}

/// The keys the table covers, for the test that pins it against the agent-visible list.
pub fn keys() -> Vec<&'static str> {
    table().iter().map(|e| e.key).collect()
}

/// `{type..., default, about}` for one key, or `None` when the table has no entry for it.
pub fn describe(key: &str) -> Option<Value> {
    let e = entry(key)?;
    let mut o = e.kind.describe();
    o["default"] = (e.default)();
    o["about"] = json!(e.about);
    Some(o)
}

/// Refuse a value of the wrong shape, naming what the key takes and its default. `null` always
/// passes: it restores the default. A key with no entry is left to the core's validator.
pub fn check(key: &str, v: &Value) -> Result<(), String> {
    if v.is_null() {
        return Ok(());
    }
    let Some(e) = entry(key) else { return Ok(()) };
    e.kind.check(key, v).map_err(|why| {
        let d = (e.default)();
        let default = if d.is_null() {
            "its default is Helm's own behaviour".to_owned()
        } else {
            format!("its default is {d}")
        };
        format!("{why} ({default}; send null to restore it.)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_allow::agent_keys;

    #[test]
    fn every_agent_key_has_an_entry_and_nothing_else_does() {
        let mut have = keys();
        have.sort_unstable();
        let mut want = agent_keys(true);
        want.sort_unstable();
        assert_eq!(have, want);
    }

    #[test]
    fn every_default_passes_its_own_check() {
        for k in keys() {
            let d = (entry(k).unwrap().default)();
            assert_eq!(check(k, &d), Ok(()), "{k}");
        }
    }

    #[test]
    fn send_shortcut_reports_its_values_and_refuses_others() {
        let d = describe("sendShortcut").unwrap();
        assert_eq!(d["allowed"], json!(["enter", "mod-enter"]));
        assert_eq!(d["default"], "mod-enter");
        assert_eq!(check("sendShortcut", &json!("enter")), Ok(()));
        let e = check("sendShortcut", &json!("ctrl-enter")).unwrap_err();
        assert!(e.contains("\"enter\" or \"mod-enter\""), "{e}");
        assert!(e.contains("default is \"mod-enter\""), "{e}");
        assert!(check("sendShortcut", &json!(true)).is_err());
    }

    #[test]
    fn null_restores_the_default_for_every_key() {
        for k in keys() {
            assert_eq!(check(k, &Value::Null), Ok(()), "{k}");
        }
    }

    #[test]
    fn numbers_lists_and_objects_are_checked() {
        assert!(check("zoomLevel", &json!(110)).is_ok());
        assert!(check("zoomLevel", &json!(115)).is_err());
        assert!(check("zoomLevel", &json!(300)).is_err());
        assert!(check("zoomLevel", &json!("100")).is_err());
        assert!(check("sidebarWidth", &json!(10))
            .unwrap_err()
            .contains("200 to 4096"));
        assert!(check("hiddenSidebarItems", &json!(["search", "saved"])).is_ok());
        let e = check("hiddenSidebarItems", &json!(["search", "nope"])).unwrap_err();
        assert!(e.contains("hiddenSidebarItems[1]"), "{e}");
        assert!(check("hiddenSidebarItems", &json!(["search", "search"])).is_err());
        assert!(check("sounds", &json!({"volume": 30})).is_ok());
        let e = check("sounds", &json!({"volume": 300})).unwrap_err();
        assert!(e.contains("sounds.volume"), "{e}");
        let e = check("sounds", &json!({"loud": true})).unwrap_err();
        assert!(e.contains("no field"), "{e}");
        assert!(check("chatElements", &json!({"tokens": "hidden"})).is_ok());
        assert!(check("chatElements", &json!({"tokens": "off"})).is_err());
        assert!(check("taskTriage", &json!({"prompt": null, "delayMinutes": 0})).is_ok());
        assert!(check("dopamine", &json!("zen")).is_ok());
        assert!(check("dopamine", &json!({"version": 1})).is_ok());
        assert!(check("dopamine", &json!("calm")).is_err());
    }

    #[test]
    fn describe_carries_a_default_and_an_about_for_every_key() {
        for k in keys() {
            let d = describe(k).unwrap();
            assert!(d.get("default").is_some(), "{k}");
            assert!(!d["about"].as_str().unwrap().is_empty(), "{k}");
            assert!(d.get("type").is_some() || d.get("oneOf").is_some(), "{k}");
        }
    }
}
