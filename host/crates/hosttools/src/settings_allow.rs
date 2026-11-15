//! Which settings an agent may change, and how.
//!
//! Every setting (a preference key, or a key an extension owns) has exactly one class:
//!
//! - **Low**: a chat changes it itself with `app_update_settings`, and the user sees a notice with
//!   Undo. The All-tier ones only while "Chats can change all settings" is on.
//! - **High**: a chat may only stage the change with `app_stage_settings`; it applies when the
//!   user clicks Approve on the card that lists exactly what changes. A chat can read these
//!   settings, and it never gets Undo for an approved change: only the user does.
//! - **User**: no tool at all. Kept for the few settings that change what approval means, and for
//!   a secret, which must not pass through a chat. Each entry carries a one-line reason.
//!
//! A preference key's class is its line in `helm_core::prefs::classes::CLASSIFIED`. The other
//! names (extension keys, and settings that live outside the preference table) are in [`NAMED`]
//! below. A new preference is agent-settable by default; a test pins that every key has one class.

use helm_core::prefs::classes::{classify, PrefClass, Tier, CLASSIFIED};

/// How risky a change is, and so who may make it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    /// A chat changes it, with Undo.
    Low,
    /// A chat stages it; the user approves.
    High,
    /// Only the user.
    User,
}

/// The settings that are not preference keys, each with its class and the one-line reason a
/// High or User entry needs (Low entries leave it empty). Extension keys are listed here; their
/// All tier, where they have one, is the extension's own.
pub const NAMED: &[(&str, Risk, &str)] = &[
    // Extension keys a chat changes itself.
    ("orchestrator", Risk::Low, ""),
    ("repositories", Risk::Low, ""),
    ("knowledge", Risk::Low, ""),
    ("mcpUnencryptedServers", Risk::Low, ""),
    ("webSearchInClaudeChats", Risk::Low, ""),
    ("telegram", Risk::Low, ""),
    ("voice", Risk::Low, ""),
    ("localOllamaContext", Risk::Low, ""),
    ("localKeepAliveMinutes", Risk::Low, ""),
    ("localServersEnabled", Risk::Low, ""),
    ("localSpeechReserveGb", Risk::Low, ""),
    ("localToolsSetup", Risk::Low, ""),
    ("localCompareModel", Risk::Low, ""),
    ("localGameLiftMinutes", Risk::Low, ""),
    // Extension keys a chat stages for the user to approve.
    (
        "localGames",
        Risk::High,
        "The programs that unload your local models decide what Helm stops, so you approve them.",
    ),
    (
        "localRestartTask",
        Risk::High,
        "Helm ends and starts this scheduled task on its own, so you approve which one.",
    ),
    (
        "localPauseFile",
        Risk::High,
        "Helm writes this file while local models are unloaded, so you approve where.",
    ),
    // The Claude Code runtime switches (Settings > Runtime), kept in a file Helm's workers read.
    (
        "shellChanges",
        Risk::High,
        "Keeps the record of what a chat's shell commands changed, so you approve switching it off.",
    ),
    ("lowPriority", Risk::Low, ""),
    ("toolOutputCap", Risk::Low, ""),
    ("subagentSoftTurns", Risk::Low, ""),
    (
        "fastMode",
        Risk::High,
        "Changes how every chat runs, so you approve it.",
    ),
    (
        "worktreeTools",
        Risk::High,
        "Lets Claude Code's own worktree tools through to chats, so you approve it.",
    ),
    (
        "scheduleTools",
        Risk::High,
        "Lets Claude Code's own schedule tools through to chats, so you approve it.",
    ),
    (
        "skipPermissions",
        Risk::High,
        "Decides whether chats launch with permissions bypassed, so you approve it.",
    ),
    (
        "subagentLimits",
        Risk::High,
        "Turns the limits on sub-agents on or off, so you approve it.",
    ),
    (
        "subagentHardStop",
        Risk::High,
        "Decides whether a sub-agent past its limit is stopped, so you approve it.",
    ),
    (
        "claudeExecutable",
        Risk::High,
        "Decides which program Helm runs as Claude Code, so you approve it.",
    ),
    // Settings in a crate's own table that a chat stages for the user to approve.
    (
        "dictation",
        Risk::High,
        "Where a recording of your voice is sent decides who hears it, so you approve it.",
    ),
    (
        "usageGovernor",
        Risk::High,
        "The governor can stop your chats near the plan's limit, so you approve its settings.",
    ),
    // Secrets: the credential store behind the MCP servers, search and the board.
    (
        "credentials",
        Risk::User,
        "A secret typed into a chat stays in its transcript, so you enter it in Settings.",
    ),
];

/// Parts of a Low extension key that only the user changes: (key, the parts, why). The extension
/// refuses an update that touches them, in words. The class above says what a chat may do with
/// the key's other parts; this list is for the pages that show which settings need the user.
pub const USER_PARTS: &[(&str, &str, &str)] = &[
    (
        "orchestrator",
        "every field but newChats and each stage's agent",
        "They decide what may land without you.",
    ),
    (
        "telegram",
        "everything but the model and effort a new chat opens with",
        "They decide who can reach you and what an answer from Telegram may approve.",
    ),
    (
        "voice",
        "everything but the model and effort a new chat opens with",
        "They decide what the phone's voice line may do and approve.",
    ),
    (
        "localServersEnabled",
        "a server's address, name, key, start folder and preset",
        "Where a model server is, and what Helm starts for it, is yours to set.",
    ),
];

/// Read-only facts listed beside the settings: no tool changes them, so they have no class.
pub const READ_ONLY: &[&str] = &["hubConnection"];

/// The class of a setting, or `None` when the name is no setting (an internal row, a read-only
/// fact, or nothing Helm knows).
pub fn risk(key: &str) -> Option<Risk> {
    match classify(key) {
        Some(PrefClass::Registry(_)) => Some(Risk::Low),
        Some(PrefClass::High(_)) => Some(Risk::High),
        Some(PrefClass::UserOnly(_)) => Some(Risk::User),
        Some(_) => None,
        None => NAMED.iter().find(|(k, ..)| *k == key).map(|(_, r, _)| *r),
    }
}

/// The reason a High or User setting needs the user, one line.
pub fn reason(key: &str) -> Option<&'static str> {
    match classify(key) {
        Some(PrefClass::High(why) | PrefClass::UserOnly(why)) => Some(why),
        Some(_) => None,
        None => NAMED
            .iter()
            .find(|(k, _, why)| *k == key && !why.is_empty())
            .map(|(_, _, why)| *why),
    }
}

/// The preference keys a chat changes itself: Base always, All with the switch on.
pub fn agent_keys(all_settings: bool) -> Vec<&'static str> {
    CLASSIFIED
        .iter()
        .filter(|(_, c)| match c {
            PrefClass::Registry(Tier::Base) => true,
            PrefClass::Registry(Tier::All) => all_settings,
            _ => false,
        })
        .map(|(k, _)| *k)
        .collect()
}

/// The preference keys a chat may only stage for the user to approve.
pub fn high_pref_keys() -> Vec<&'static str> {
    CLASSIFIED
        .iter()
        .filter(|(_, c)| matches!(c, PrefClass::High(_)))
        .map(|(k, _)| *k)
        .collect()
}

/// Every key a chat may stage: High preference keys and High named keys.
pub fn high_keys() -> Vec<&'static str> {
    high_pref_keys()
        .into_iter()
        .chain(
            NAMED
                .iter()
                .filter(|(_, r, _)| *r == Risk::High)
                .map(|(k, ..)| *k),
        )
        .collect()
}

/// The refusal for a key that needs "Chats can change all settings" while that is off.
pub fn needs_all_refusal(key: &str) -> String {
    format!(
        "{key} needs \"Chats can change all settings\", which only the user can turn on in Settings > General."
    )
}

/// What a chat is told when it tries to change a High setting itself.
pub fn high_refusal(key: &str) -> String {
    let why = reason(key).map(|w| format!(" {w}")).unwrap_or_default();
    format!(
        "{key} needs the user's approval, so you cannot change it here: stage it with app_stage_settings and the user approves it.{why}"
    )
}

/// What a chat is told when it tries to change a User setting.
pub fn user_refusal(key: &str) -> String {
    let why = reason(key).unwrap_or("Only the user changes it.");
    format!("{key} is yours to change only: ask the user to change it in Settings. {why}")
}

/// Why a chat cannot change `key` with `app_update_settings`, or `None` when it can. An extension
/// key is checked here for its class only; its own tier and value rules come after.
pub fn refusal(key: &str, all_settings: bool) -> Option<String> {
    match risk(key) {
        Some(Risk::High) => Some(high_refusal(key)),
        Some(Risk::User) => Some(user_refusal(key)),
        Some(Risk::Low) => match classify(key) {
            Some(PrefClass::Registry(Tier::All)) if !all_settings => Some(needs_all_refusal(key)),
            _ => None,
        },
        None => Some(format!("{key} is not a setting chats can change.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every classified preference key has exactly one class, and so does every named setting;
    /// no name sits in both lists.
    #[test]
    fn every_key_has_exactly_one_class() {
        let mut seen = std::collections::HashSet::new();
        for (k, c) in CLASSIFIED {
            assert!(seen.insert(*k), "{k} is classified twice");
            let want = match c {
                PrefClass::Registry(_) => Some(Risk::Low),
                PrefClass::High(_) => Some(Risk::High),
                PrefClass::UserOnly(_) => Some(Risk::User),
                PrefClass::Tool | PrefClass::Internal | PrefClass::Later => None,
            };
            assert_eq!(risk(k), want, "{k}");
        }
        for (k, r, why) in NAMED {
            assert!(
                seen.insert(*k),
                "{k} is named twice or is also a preference key"
            );
            assert_eq!(risk(k), Some(*r));
            // A High or User setting says why in one line; a Low one needs no excuse.
            assert_eq!(why.is_empty(), *r == Risk::Low, "{k}");
            assert!(why.len() <= 130 && !why.contains('\n'), "{k}: {why}");
            assert!(!READ_ONLY.contains(k), "{k} is read only");
        }
        for k in READ_ONLY {
            assert!(seen.insert(*k) && risk(k).is_none(), "{k}");
        }
        // The parts only the user changes belong to keys a chat otherwise changes itself.
        for (k, part, why) in USER_PARTS {
            assert_eq!(risk(k), Some(Risk::Low), "{k}");
            assert!(!part.is_empty() && why.ends_with('.'), "{k}");
        }
    }

    /// The user-only list stays tiny: what changes what approval means, and secrets.
    #[test]
    fn the_user_only_list_is_tiny_and_each_entry_says_why() {
        let mut user: Vec<&str> = CLASSIFIED
            .iter()
            .map(|(k, _)| *k)
            .chain(NAMED.iter().map(|(k, ..)| *k))
            .filter(|k| risk(k) == Some(Risk::User))
            .collect();
        user.sort_unstable();
        for k in &user {
            assert!(reason(k).is_some_and(|w| w.ends_with('.')), "{k}");
        }
        assert!(user.contains(&"agentSettingsAll"));
        assert!(user.len() <= 16, "{user:?}");
    }

    #[test]
    fn the_tiers_and_the_refusals() {
        assert_eq!(agent_keys(false).len(), 46);
        assert_eq!(agent_keys(true).len(), 47);
        assert!(agent_keys(true).contains(&"keybindings"));
        assert!(!agent_keys(false).contains(&"keybindings"));
        assert!(agent_keys(false).contains(&"scrollOnSend"));
        for k in agent_keys(true) {
            assert_eq!(risk(k), Some(Risk::Low), "{k}");
        }
        for k in high_pref_keys() {
            assert_eq!(risk(k), Some(Risk::High), "{k}");
            let r = refusal(k, true).unwrap();
            assert!(r.contains("app_stage_settings"), "{r}");
        }
        assert!(refusal("theme", false).is_none());
        assert!(refusal("keybindings", false)
            .unwrap()
            .contains("all settings"));
        assert!(refusal("keybindings", true).is_none());
        assert!(refusal("localOllamaContext", false).is_none());
        let r = refusal("agentSettingsAll", true).unwrap();
        assert!(r.contains("yours to change only"), "{r}");
        assert!(!r.contains("app_stage_settings"), "{r}");
        assert!(refusal("selection", true)
            .unwrap()
            .contains("not a setting"));
        assert!(refusal("nonsense", true).is_some());
        assert!(refusal("hubConnection", true).is_some());
    }
}
