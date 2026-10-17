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
    /// Only the user, in Settings. Carries the reason (≤ 130 bytes).
    UserOnly(&'static str),
    /// App state, not a setting.
    Internal,
    /// Harmless, not yet exposed. This list may only shrink.
    Later,
}

use PrefClass::*;
use Tier::*;

/// Every fixed preference key and its class. Seeded with the 36-key Base tier,
/// the All-tier `keybindings`, the generic-validated layout keys, and the all-settings
/// switch. Later features add their keys here.
pub const CLASSIFIED: &[(&str, PrefClass)] = &[
    // Base tier (36).
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
    ("responseVerbosity", Registry(Base)),
    ("taskTriage", Registry(Base)),
    ("chatElements", Registry(Base)),
    ("desktopPet", Registry(Base)),
    ("projectsMode", Registry(Base)),
    ("tasks", Registry(Base)),
    // All tier.
    ("keybindings", Registry(All)),
    // Layout keys the generic writer validates but agents are not offered yet.
    ("sidebarView", Later),
    ("terminalHeight", Later),
    // Minutes without output before a running chat is stalled.
    ("stallMinutes", Later),
    // The open chat and project, restored at launch.
    ("selection", Internal),
    // User-only: they change what Helm does outside its window.
    (
        "globalHotkey",
        UserOnly("A system-wide shortcut works in every app, so only you set it."),
    ),
    (
        "notifications",
        UserOnly(
            "Where notices about your chats appear, including outside Helm, is yours to choose.",
        ),
    ),
    (
        "dictationMic",
        UserOnly("Which microphone Helm listens on is yours to choose."),
    ),
    (
        "terminalShell",
        UserOnly("The shell your terminal runs is yours to choose."),
    ),
    // User-only.
    (
        "agentSettingsAll",
        UserOnly("Lets chats change every setting, so only you can turn it on."),
    ),
    // An internal setting that only the user sets.
    (
        "sendBehavior",
        UserOnly("Changes how your own messages reach a running agent, so only you set it."),
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
    fn base_tier_has_36_keys() {
        let base = CLASSIFIED
            .iter()
            .filter(|(_, c)| *c == Registry(Base))
            .count();
        assert_eq!(base, 36);
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
    fn user_only_reasons_fit_130_bytes() {
        for (k, c) in CLASSIFIED {
            if let UserOnly(reason) = c {
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
