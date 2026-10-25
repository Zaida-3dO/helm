//! Which settings an agent may change.
//!
//! The default is allow: every Registry key is agent-settable, the All-tier ones
//! only while "Chats can change all settings" is on. The user-only floor below is never
//! agent-settable; a refusal names the Settings page where the user acts (there is no Allow kind
//! for these). Entries that are not preference keys (credentials, MCP approvals, paths) are
//! listed so a refusal for them reads the same; the tests pin the rest against the preference
//! classes.

use helm_core::prefs::classes::{classify, PrefClass, Tier, CLASSIFIED};

/// The user-only floor: (setting, the Settings page where the user changes it).
pub const USER_ONLY: &[(&str, &str)] = &[
    ("fastMode", "Settings > Runtime"),
    ("worktreeTools", "Settings > Runtime"),
    ("notifications", "Settings > Notifications"),
    ("loadGate", "Settings > Chat (resource limits)"),
    ("afk", "Settings > Away"),
    ("afkSchedule", "Settings > Away"),
    ("boardToken", "Settings > Projects (Agent Standup)"),
    ("credentials", "Settings > MCP"),
    ("dictationMic", "Settings > Shell"),
    ("nativeMcp", "Settings > MCP"),
    ("agentSettingsAll", "Settings > General"),
    ("mcpPrivateHttp", "Settings > MCP"),
    ("claudeExecutable", "Settings > Chat"),
    ("helperPaths", "Settings > Chat"),
    ("projectMcpApprovals", "Settings > MCP"),
    ("mcpDenyEntries", "Settings > MCP"),
    ("mcpLoadInAppChats", "Settings > MCP"),
    ("responseVerbosity", "Settings > Chat"),
    ("globalHotkey", "Settings > Shell"),
    ("terminalShell", "Settings > Shell"),
    ("sendBehavior", "Settings > Chat"),
];

/// Where the user changes a user-only setting, or `None` when it is not user-only.
pub fn user_only_page(key: &str) -> Option<&'static str> {
    USER_ONLY.iter().find(|(k, _)| *k == key).map(|(_, p)| *p)
}

/// The keys an agent may change: Base always, All with the switch on, never the floor.
pub fn agent_keys(all_settings: bool) -> Vec<&'static str> {
    CLASSIFIED
        .iter()
        .filter(|(k, c)| {
            user_only_page(k).is_none()
                && match c {
                    PrefClass::Registry(Tier::Base) => true,
                    PrefClass::Registry(Tier::All) => all_settings,
                    _ => false,
                }
        })
        .map(|(k, _)| *k)
        .collect()
}

/// The refusal for a key that needs "Chats can change all settings" while that is off.
pub fn needs_all_refusal(key: &str) -> String {
    format!(
        "{key} needs \"Chats can change all settings\", which only the user can turn on in Settings > General."
    )
}

/// Why `key` cannot be changed by an agent, or `None` when it can.
pub fn refusal(key: &str, all_settings: bool) -> Option<String> {
    if let Some(page) = user_only_page(key) {
        return Some(format!(
            "{key} is yours to change only: ask the user to change it in {page}."
        ));
    }
    match classify(key) {
        Some(PrefClass::Registry(Tier::Base)) => None,
        Some(PrefClass::Registry(Tier::All)) if all_settings => None,
        Some(PrefClass::Registry(Tier::All)) => Some(needs_all_refusal(key)),
        Some(PrefClass::UserOnly(why)) => Some(format!("{why} Change it in Settings.")),
        _ => Some(format!("{key} is not a setting chats can change.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_key_is_both_agent_settable_and_user_only() {
        for k in agent_keys(true) {
            assert!(user_only_page(k).is_none(), "{k}");
            assert!(
                matches!(classify(k), Some(PrefClass::Registry(_))),
                "{k} is agent-settable but not a Registry key"
            );
        }
    }

    #[test]
    fn user_only_preference_keys_are_classified_user_only() {
        for (k, _) in USER_ONLY {
            match classify(k) {
                None => {} // not a preference key (credentials, approvals, paths)
                Some(PrefClass::UserOnly(_)) => {}
                Some(c) => panic!("{k} is user-only here but classified {c:?}"),
            }
        }
    }

    #[test]
    fn the_tiers_and_the_floor() {
        // Base 35 (responseVerbosity left Base for user-only), plus keybindings.
        assert_eq!(agent_keys(false).len(), 35);
        assert_eq!(agent_keys(true).len(), 36);
        assert!(agent_keys(true).contains(&"keybindings"));
        assert!(!agent_keys(false).contains(&"keybindings"));
        assert!(refusal("theme", false).is_none());
        assert!(refusal("keybindings", false)
            .unwrap()
            .contains("all settings"));
        assert!(refusal("keybindings", true).is_none());
        let r = refusal("responseVerbosity", true).unwrap();
        assert!(r.contains("Settings > Chat"), "{r}");
        assert!(refusal("fastMode", true)
            .unwrap()
            .contains("Settings > Runtime"));
        assert!(refusal("selection", true)
            .unwrap()
            .contains("not a setting"));
        assert!(refusal("nonsense", true).is_some());
    }
}
