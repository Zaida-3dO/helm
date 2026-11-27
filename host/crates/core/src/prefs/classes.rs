//! Preference classification.
//!
//! Every preference key belongs to exactly one class. Keys are declared with
//! [`PrefKey::classified`], a `const fn` that fails compilation for a key missing from
//! [`CLASSIFIED`]: an unclassified key fails the build. Writes by string key (the generic
//! `preference` op) are checked at runtime by the single writer instead.
//!
//! A crate adding a preference adds one line here (its registration line) and declares the key as a
//! `const` in its own code. The line must say how a chat reaches the setting: [`Access`] has
//! exactly two values, and there is no way to write "chats may not touch it".
//!
//! **A new preference is directly changeable by default**: give it `DIRECT` and chats change it
//! themselves, with a notice and Undo. `approval("...")` (a chat stages it and the user clicks
//! Approve) needs a one-line reason that names a concrete danger; the bar is very high.
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

/// How a chat reaches a setting. These are the only two ways: a chat either changes the setting
/// itself, or stages the change and the user clicks Approve. Neither is a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The chat applies it; the user sees a notice with Undo.
    Direct,
    /// The chat stages it (`app_stage_settings`) and it applies when the user clicks Approve. The
    /// text is the one-line reason the card shows; build it with [`Access::approval`].
    Approval(&'static str),
}

impl Access {
    /// The longest reason, in bytes: it has to fit on one line of the Approve card.
    pub const MAX_REASON: usize = 130;

    /// An approval access with its reason. In a `const` or a table a missing or over-long reason
    /// is a compile error.
    pub const fn approval(reason: &'static str) -> Self {
        assert!(!reason.is_empty(), "an approval needs its one-line reason");
        assert!(
            reason.len() <= Self::MAX_REASON,
            "the reason must fit on one line of the card"
        );
        Self::Approval(reason)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefClass {
    /// A setting. [`Access`] says how a chat changes it.
    Setting(Access),
    /// App state, not a setting: no tool lists or changes it.
    Internal,
}

use PrefClass::{Internal, Setting};

/// A setting a chat changes itself.
const DIRECT: PrefClass = Setting(Access::Direct);

/// A setting a chat stages for the user to approve, with the one-line reason.
const fn approval(reason: &'static str) -> PrefClass {
    Setting(Access::approval(reason))
}

/// Every fixed preference key and its class. Later features add their keys here.
pub const CLASSIFIED: &[(&str, PrefClass)] = &[
    ("theme", DIRECT),
    ("interfaceTheme", DIRECT),
    ("appearance", DIRECT),
    ("codeThemes", DIRECT),
    ("fontSizes", DIRECT),
    ("fontFamilies", DIRECT),
    ("dopamine", DIRECT),
    ("animationControls", DIRECT),
    ("themeEffects", DIRECT),
    ("cursorTrail", DIRECT),
    ("customCursors", DIRECT),
    // Settings > Appearance > Colours: an accent hue, a tint and a contrast choice, per profile.
    ("customColours", DIRECT),
    ("pauseAnimationsOnScroll", DIRECT),
    ("compactSidebar", DIRECT),
    ("hiddenSidebarItems", DIRECT),
    ("moveCompletedTasksToTop", DIRECT),
    ("reviewWrap", DIRECT),
    ("zoomLevel", DIRECT),
    ("sounds", DIRECT),
    ("keepAwake", DIRECT),
    ("hiddenSkills", DIRECT),
    ("sidebarVisible", DIRECT),
    ("inspectorVisible", DIRECT),
    ("sidebarWidth", DIRECT),
    ("inspectorWidth", DIRECT),
    ("collapsedProjects", DIRECT),
    ("expandedChats", DIRECT),
    ("sendShortcut", DIRECT),
    ("defaultHarness", DIRECT),
    ("newChat", DIRECT),
    ("autocomplete", DIRECT),
    ("taskTriage", DIRECT),
    ("chatElements", DIRECT),
    ("desktopPet", DIRECT),
    ("projectsMode", DIRECT),
    ("tasks", DIRECT),
    ("keybindings", DIRECT),
    // Layout and chat-behaviour keys.
    ("sidebarView", DIRECT),
    ("terminalHeight", DIRECT),
    // Minutes without output before a running chat is stalled.
    ("stallMinutes", DIRECT),
    // How many of our chats one local model server may carry at once (0 switches the cap off).
    ("localChatCap", DIRECT),
    // Settings > Chat and Shell: how the user's own window and keyboard behave. A chat changes
    // these itself, with the Undo notice.
    ("scrollOnSend", DIRECT),
    ("doubleShift", DIRECT),
    // Whether Helm archives a chat that has sent its completed report and gone quiet.
    ("archiveFinishedChats", DIRECT),
    ("sendBehavior", DIRECT),
    ("responseVerbosity", DIRECT),
    ("notifications", DIRECT),
    ("dictationMic", DIRECT),
    // The board chats with no other home are shown on (a board project id, or "ask").
    ("defaultBoard", DIRECT),
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
    // Tuning that a chat changes itself.
    // The percent of the Claude plan windows at which a new Claude chat waits for the reset
    // (0 switches it off). The default lives in `helm-chats` with the queue.
    ("claudeHoldPct", DIRECT),
    ("globalHotkey", DIRECT),
    ("terminalShell", DIRECT),
    ("backgroundModel", DIRECT),
    ("titlesLocalViaClaude", DIRECT),
    ("ideContext", DIRECT),
    // "Use my Claude Code MCP servers", default on.
    ("nativeMcp", DIRECT),
    // The MCP master switch, default on. Off: no chat loads any MCP server.
    ("mcpEnabled", DIRECT),
    // The pull request view: whether Helm reads and writes GitHub for you.
    ("githubPrView", DIRECT),
    // Whether other devices may run chats on this PC (on when missing).
    ("deviceRunFromOthers", DIRECT),
    // What other people in the household may see of this install's chats; all off by default.
    ("householdSharing", DIRECT),
    // Context size at which an idle chat is asked to roll over into a fresh one (0 = never).
    ("rolloverTokens", DIRECT),
    // The few settings a chat stages for the user to approve. Each reason names a concrete
    // danger; the list is kept short on purpose (a test caps it).
    (
        "newChatPermissionMode",
        approval("New chats may start with permissions bypassed, so you approve it."),
    ),
    (
        "updateSource",
        approval("Helm installs the update it finds in this folder, so you approve which folder."),
    ),
    // The switch that opens the repository tools to chats without asking.
    (
        "agentSettingsAll",
        approval("Lets chats use tools that act on your repositories without asking, so you approve it."),
    ),
    // How chats work (run to completion or check in): kept here, changed through the `workmode`
    // key, which also covers a board's own mode.
    ("workingMode", Internal),
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
    fn the_approval_list_is_short_and_each_entry_says_why() {
        let count = |want: fn(&PrefClass) -> bool| CLASSIFIED.iter().filter(|(_, c)| want(c)).count();
        assert_eq!(count(|c| matches!(c, Setting(Access::Approval(_)))), 3);
        assert_eq!(count(|c| *c == Internal), 5);
        for (k, c) in CLASSIFIED {
            if let Setting(Access::Approval(why)) = c {
                assert!(why.ends_with('.') && why.len() <= Access::MAX_REASON, "{k}");
            }
        }
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
    fn dynamic_prefixes_need_a_suffix() {
        assert_eq!(classify("autocompleteBudget:chat-1"), Some(Internal));
        assert_eq!(classify("autocompleteBudget:"), None);
        assert_eq!(classify("sidebarWidth"), Some(DIRECT));
        assert_eq!(classify("sidebarWidthX"), None);
    }
}
