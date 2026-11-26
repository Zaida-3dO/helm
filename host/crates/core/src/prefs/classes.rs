//! Preference classification.
//!
//! Every preference key belongs to exactly one class. Keys are declared with
//! [`PrefKey::classified`], a `const fn` that fails compilation for a key missing from
//! [`CLASSIFIED`]: an unclassified key fails the build. Writes by string key (the generic
//! `preference` op) are checked at runtime by the single writer instead.
//!
//! A crate adding a preference adds one line here (its registration line) and declares the key as a
//! `const` in its own code.
//!
//! **A new preference is agent-settable by default**: classify it `Registry(Base)` and chats change
//! it themselves, with the Undo notice. `High` (a chat stages it and the user approves) and
//! `UserOnly` (no tool at all, kept for what changes what approval means) each carry a one-line
//! reason, and need a good one.
//!
//! ```
//! use helm_core::prefs::PrefKey;
//! const WIDTH: PrefKey = PrefKey::classified("sidebarWidth");
//! assert_eq!(WIDTH.name(), "sidebarWidth");
//! ```
//!
//! ```compile_fail
//! use helm_core::prefs::PrefKey;
//! const NOPE: PrefKey = PrefKey::classified("notARealPreference");
//! ```

/// Agent tiers for Registry keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Always offered to agents.
    Base,
    /// Only while "Chats can change all settings" is on.
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefClass {
    /// Changeable through the generic agent settings tools.
    Registry(Tier),
    /// Changed by agents only through its own host tool.
    Tool,
    /// Agents stage a change (`app_stage_settings`) and it applies only after the user approves
    /// it. Carries what the approval is for (≤ 130 bytes).
    High(&'static str),
    /// Only the user, in Settings: no tool changes it at all. Carries the reason (≤ 130 bytes).
    UserOnly(&'static str),
    /// App state, not a setting.
    Internal,
    /// Harmless, not yet exposed. Nothing is listed here now: a new preference is agent-settable
    /// by default.
    Later,
}

use PrefClass::*;
use Tier::*;

/// Every fixed preference key and its class. Seeded with the Base tier,
/// the All-tier `keybindings`, the generic-validated layout keys, and the all-settings
/// switch. Later features add their keys here.
pub const CLASSIFIED: &[(&str, PrefClass)] = &[
    // Base tier.
    ("theme", Registry(Base)),
    ("interfaceTheme", Registry(Base)),
    ("appearance", Registry(Base)),
    ("codeThemes", Registry(Base)),
    ("fontSizes", Registry(Base)),
    ("fontFamilies", Registry(Base)),
    ("dopamine", Registry(Base)),
    ("animationControls", Registry(Base)),
    ("themeEffects", Registry(Base)),
    ("cursorTrail", Registry(Base)),
    ("customCursors", Registry(Base)),
    // Settings > Appearance > Colours: an accent hue, a tint and a contrast choice, per profile.
    ("customColours", Registry(Base)),
    ("pauseAnimationsOnScroll", Registry(Base)),
    ("compactSidebar", Registry(Base)),
    ("hiddenSidebarItems", Registry(Base)),
    ("moveCompletedTasksToTop", Registry(Base)),
    ("reviewWrap", Registry(Base)),
    ("zoomLevel", Registry(Base)),
    ("sounds", Registry(Base)),
    ("keepAwake", Registry(Base)),
    ("hiddenSkills", Registry(Base)),
    ("sidebarVisible", Registry(Base)),
    ("inspectorVisible", Registry(Base)),
    ("sidebarWidth", Registry(Base)),
    ("inspectorWidth", Registry(Base)),
    ("collapsedProjects", Registry(Base)),
    ("expandedChats", Registry(Base)),
    ("sendShortcut", Registry(Base)),
    ("defaultHarness", Registry(Base)),
    ("newChat", Registry(Base)),
    ("autocomplete", Registry(Base)),
    ("taskTriage", Registry(Base)),
    ("chatElements", Registry(Base)),
    ("desktopPet", Registry(Base)),
    ("projectsMode", Registry(Base)),
    ("tasks", Registry(Base)),
    // All tier.
    ("keybindings", Registry(All)),
    // Layout and chat-behaviour keys.
    ("sidebarView", Registry(Base)),
    ("terminalHeight", Registry(Base)),
    // Minutes without output before a running chat is stalled.
    ("stallMinutes", Registry(Base)),
    // How many of our chats one local model server may carry at once (0 switches the cap off).
    ("localChatCap", Registry(Base)),
    // Settings > Chat and Shell: how the user's own window and keyboard behave. A chat changes
    // these itself, with the Undo notice.
    ("scrollOnSend", Registry(Base)),
    ("doubleShift", Registry(Base)),
    // Whether Helm archives a chat that has sent its completed report and gone quiet.
    ("archiveFinishedChats", Registry(Base)),
    ("sendBehavior", Registry(Base)),
    ("responseVerbosity", Registry(Base)),
    ("notifications", Registry(Base)),
    ("dictationMic", Registry(Base)),
    // The board chats with no other home are shown on (a board project id, or "ask").
    ("defaultBoard", Registry(Base)),
    // The open chat and project, restored at launch.
    ("selection", Internal),
    // The profile whose chats the sidebar shows (a profile id, or "all").
    ("activeProfile", Internal),
    // The model, effort and reply length new chats start with, kept per profile
    // (`{profileId: {model, effort, responseVerbosity}}`); `newChat` and `responseVerbosity`
    // are what a profile with no entry of its own falls back to.
    ("profileDefaults", Internal),
    // The device whose chats New creates and the sidebar toggle shows (a device uid); unset is
    // this device.
    ("selectedDevice", Internal),
    // Higher risk: a chat stages the change and it applies only when the user approves it.
    // The percent of the Claude plan windows at which a new Claude chat waits for the reset
    // (0 switches it off). The default lives in `helm-chats` with the queue.
    (
        "claudeHoldPct",
        High("Decides when Claude chats wait for the plan to reset, so you approve it."),
    ),
    (
        "globalHotkey",
        High("A system-wide shortcut works in every app, so you approve it."),
    ),
    (
        "terminalShell",
        High("The shell your terminal runs is a program Helm starts, so you approve it."),
    ),
    (
        "backgroundModel",
        High("The model that reads your chats for titles and checks is yours to approve."),
    ),
    (
        "titlesLocalViaClaude",
        High("Whether a local chat's first message may go to Claude for a title is yours to approve."),
    ),
    (
        "ideContext",
        High("Lets agents read your editor's open file and selection, so you approve it."),
    ),
    (
        "newChatPermissionMode",
        High("The permission mode new chats start in sets what they may do, so you approve it."),
    ),
    // "Use my Claude Code MCP servers", default on.
    (
        "nativeMcp",
        High("Which of your own MCP servers chats load is yours to approve."),
    ),
    // The MCP master switch, default on. Off: no chat loads any MCP server.
    (
        "mcpEnabled",
        High("Whether chats may use MCP servers at all is yours to approve."),
    ),
    (
        "updateSource",
        High("Decides which program Helm runs to update itself, so you approve it."),
    ),
    // The pull request view: whether Helm reads and writes GitHub for you.
    (
        "githubPrView",
        High("Whether Helm reads and writes GitHub for you is yours to approve."),
    ),
    // What the household may see of this install's chats; all three switches default to off.
    (
        "householdSharing",
        High("What other people in your household may see of your chats is yours to approve."),
    ),
    // Whether other devices may run chats on this PC (on when missing).
    (
        "deviceRunFromOthers",
        High("Whether other devices may run chats on this PC is yours to approve."),
    ),
    // The switch that governs what chats may change without asking.
    (
        "agentSettingsAll",
        UserOnly("Lets chats change every setting without asking, so only you can turn it on."),
    ),
    // How chats work: run to completion, or check in (check in when missing).
    (
        "workingMode",
        UserOnly("How your chats work, run to completion or check in, is yours to set; no brief can change it."),
    ),
    // Context size at which an idle chat is asked to roll over into a fresh one (0 = never). A
    // chat could set a tiny threshold and force itself and every other chat to keep rolling
    // over, so only the user sets it.
    (
        "rolloverTokens",
        UserOnly("A chat could use a low threshold to force rollovers, so when chats roll over is yours to set."),
    ),
];

/// Dynamic keys under fixed prefixes: all internal.
pub const DYNAMIC_PREFIXES: &[(&str, PrefClass)] = &[
    ("autocompleteBudget:", Internal),
    ("migration:", Internal),
    ("diagnosis:", Internal),
];

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn starts_with(s: &str, prefix: &str) -> bool {
    let (s, p) = (s.as_bytes(), prefix.as_bytes());
    if s.len() < p.len() {
        return false;
    }
    let mut i = 0;
    while i < p.len() {
        if s[i] != p[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The class of a key, or `None` if it is unclassified. Usable in `const` context.
pub const fn classify(key: &str) -> Option<PrefClass> {
    let mut i = 0;
    while i < CLASSIFIED.len() {
        if str_eq(CLASSIFIED[i].0, key) {
            return Some(CLASSIFIED[i].1);
        }
        i += 1;
    }
    let mut j = 0;
    while j < DYNAMIC_PREFIXES.len() {
        if starts_with(key, DYNAMIC_PREFIXES[j].0) && key.len() > DYNAMIC_PREFIXES[j].0.len() {
            return Some(DYNAMIC_PREFIXES[j].1);
        }
        j += 1;
    }
    None
}

/// A preference key known to be classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrefKey(&'static str);

impl PrefKey {
    /// Declare a key. In a `const`, an unclassified name is a compile error.
    pub const fn classified(name: &'static str) -> Self {
        if classify(name).is_none() {
            panic!("unclassified preference key: add it to prefs::classes::CLASSIFIED");
        }
        Self(name)
    }

    pub const fn name(self) -> &'static str {
        self.0
    }

    pub fn class(self) -> PrefClass {
        classify(self.0).expect("checked at construction")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tiers_and_the_floor() {
        let count =
            |want: fn(&PrefClass) -> bool| CLASSIFIED.iter().filter(|(_, c)| want(c)).count();
        assert_eq!(count(|c| *c == Registry(Base)), 48);
        assert_eq!(count(|c| *c == Registry(All)), 1);
        // The user-only floor: the switch that governs what chats may change, how chats work,
        // and when they roll themselves over.
        let user: Vec<_> = CLASSIFIED
            .iter()
            .filter(|(_, c)| matches!(c, UserOnly(_)))
            .map(|(k, _)| *k)
            .collect();
        assert_eq!(user, ["agentSettingsAll", "workingMode", "rolloverTokens"]);
        // Nothing is parked: a new preference is agent-settable unless it says why not.
        assert_eq!(count(|c| *c == Later), 0);
    }

    #[test]
    fn keys_are_unique() {
        let mut names: Vec<_> = CLASSIFIED.iter().map(|(k, _)| *k).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }

    #[test]
    fn user_only_and_high_reasons_fit_130_bytes() {
        for (k, c) in CLASSIFIED {
            if let UserOnly(reason) | High(reason) = c {
                assert!(reason.len() <= 130, "{k}: reason too long");
            }
        }
    }

    #[test]
    fn dynamic_prefixes_need_a_suffix() {
        assert_eq!(classify("autocompleteBudget:chat-1"), Some(Internal));
        assert_eq!(classify("autocompleteBudget:"), None);
        assert_eq!(classify("sidebarWidth"), Some(Registry(Base)));
        assert_eq!(classify("sidebarWidthX"), None);
    }
}
