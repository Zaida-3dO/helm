//! Which settings an agent may change, and how.
//!
//! Every setting (a preference key, or a key an extension owns) has exactly one [`Access`], and
//! it has two values and no third:
//!
//! - **Direct**: a chat changes it itself with `app_update_settings`, and the user sees a notice
//!   with Undo.
//! - **Approval(reason)**: a chat stages the change with `app_stage_settings`; it applies when
//!   the user clicks Approve on the card that lists exactly what changes. A chat can read these
//!   settings, and it never gets Undo for an approved change: only the user does. The reason is
//!   one line naming the danger, and the card shows it. The list is short on purpose: a test caps
//!   it, so adding to it is a deliberate decision.
//!
//! There is no class that means "only the user", and no function here that turns a chat away
//! without saying what to call next. [`route`] answers [`Route::Apply`] or [`Route::Stage`], and
//! [`stage_message`] is the one place that words the second.
//!
//! A preference key's access is its line in `helm_core::prefs::classes::CLASSIFIED`. The other
//! names (extension keys, and settings that live outside the preference table) are in [`NAMED`]
//! below, and leaving the access out there does not compile. A part of a Direct extension key
//! that needs approval is declared by the extension (`SettingsExtension::staged_parts`).

use crate::settings_ext::StagedPart;
use helm_core::prefs::classes::{classify, PrefClass, CLASSIFIED};
pub use helm_core::prefs::Access;

/// The most settings that may need approval, preference keys and named ones together. Raising
/// it is a decision, and the commit says why.
pub const MAX_APPROVAL_KEYS: usize = 9;

/// The key a chat stages to ask the user to type a secret. It carries no secret: only which
/// credential, and Approve opens the Settings page where the user types it.
pub const CREDENTIALS_KEY: &str = "credentials";

/// The settings that are not preference keys, each with its access. Extension keys are listed
/// here. An approval entry carries its one-line reason through [`Access::approval`].
pub const NAMED: &[(&str, Access)] = &[
    // Extension keys a chat changes itself.
    ("orchestrator", Access::Direct),
    ("repositories", Access::Direct),
    ("knowledge", Access::Direct),
    ("mcpUnencryptedServers", Access::Direct),
    ("webSearchInClaudeChats", Access::Direct),
    ("telegram", Access::Direct),
    ("voice", Access::Direct),
    ("speaker", Access::Direct),
    ("localOllamaContext", Access::Direct),
    ("localKeepAliveMinutes", Access::Direct),
    ("localServersEnabled", Access::Direct),
    ("localSpeechReserveGb", Access::Direct),
    ("localToolsSetup", Access::Direct),
    ("localCompareModel", Access::Direct),
    ("localGameLiftMinutes", Access::Direct),
    ("localGames", Access::Direct),
    ("localRestartTask", Access::Direct),
    ("localPauseFile", Access::Direct),
    // The Claude Code runtime switches (Settings > Runtime), kept in a file Helm's workers read.
    ("shellChanges", Access::Direct),
    ("lowPriority", Access::Direct),
    ("toolOutputCap", Access::Direct),
    ("subagentSoftTurns", Access::Direct),
    ("fastMode", Access::Direct),
    ("worktreeTools", Access::Direct),
    ("scheduleTools", Access::Direct),
    ("subagentLimits", Access::Direct),
    ("subagentHardStop", Access::Direct),
    // Settings in a crate's own table.
    ("usageGovernor", Access::Direct),
    // The few a chat stages for the user to approve.
    (
        "workmode",
        Access::approval("It sets how much a chat may decide on its own, so you approve it."),
    ),
    (
        "skipPermissions",
        Access::approval("Chats would run with permissions bypassed, so you approve it."),
    ),
    (
        "claudeExecutable",
        Access::approval("Helm would run this program as Claude Code, so you approve it."),
    ),
    (
        "dictation",
        Access::approval(
            "Where a recording of your voice is sent decides who hears it, so you approve it.",
        ),
    ),
    (
        "localServers",
        Access::approval(
            "A server's folder and preset make Helm start a program, so you approve them.",
        ),
    ),
    (
        CREDENTIALS_KEY,
        Access::approval(
            "A secret must never pass through a chat, so you type it yourself on the page this opens.",
        ),
    ),
];

/// The secrets a person types in Settings: (the page's id, its title, what it holds). A staged
/// `credentials` change names one of the ids; on Approve Helm opens that page.
pub const CREDENTIALS: &[(&str, &str, &str)] = &[
    ("mcp", "MCP", "an MCP server's secret or sign-in"),
    ("localmodels", "Local models", "a local model server's key"),
    ("websearch", "Web search", "the web search key"),
    ("projects", "Projects", "the board's access token"),
    ("github", "GitHub", "the GitHub token"),
    ("hub", "Hub", "the hub connection's token"),
];

/// Read-only facts listed beside the settings: they report, they are not set. What they report
/// is changed through the setting they come from, which their own text names.
pub const READ_ONLY: &[&str] = &["hubConnection"];

/// The access of a setting, or `None` when the name is no setting (an internal row, a read-only
/// fact, or nothing Helm knows: a typo).
pub fn access(key: &str) -> Option<Access> {
    match classify(key) {
        Some(PrefClass::Setting(a)) => Some(a),
        Some(PrefClass::Internal) => None,
        None => NAMED.iter().find(|(k, _)| *k == key).map(|(_, a)| *a),
    }
}

/// The one-line reason a setting needs approval, or `None` when a chat changes it directly.
pub fn reason(key: &str) -> Option<&'static str> {
    match access(key)? {
        Access::Approval(why) => Some(why),
        Access::Direct => None,
    }
}

/// The preference keys a chat changes itself.
pub fn direct_pref_keys() -> Vec<&'static str> {
    CLASSIFIED
        .iter()
        .filter(|(_, c)| matches!(c, PrefClass::Setting(Access::Direct)))
        .map(|(k, _)| *k)
        .collect()
}

/// The preference keys a chat stages for the user to approve.
pub fn approval_pref_keys() -> Vec<&'static str> {
    CLASSIFIED
        .iter()
        .filter(|(_, c)| matches!(c, PrefClass::Setting(Access::Approval(_))))
        .map(|(k, _)| *k)
        .collect()
}

/// Every key a chat stages: the approval preference keys and the approval named keys.
pub fn approval_keys() -> Vec<&'static str> {
    approval_pref_keys()
        .into_iter()
        .chain(
            NAMED
                .iter()
                .filter(|(_, a)| matches!(a, Access::Approval(_)))
                .map(|(k, _)| *k),
        )
        .collect()
}

/// How a chat's change goes: applied by the chat, or staged for the user. There is no third way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// The chat applies it, with a notice and Undo.
    Apply,
    /// The chat stages it. `part` is the part of the key that needs it (none: the whole key).
    Stage {
        part: Option<&'static str>,
        reason: &'static str,
    },
}

/// The one decision. `touched` is the staged part of an extension key that the change would alter.
pub fn route(access: Access, touched: Option<&StagedPart>) -> Route {
    match access {
        Access::Approval(reason) => Route::Stage { part: None, reason },
        Access::Direct => match touched {
            Some(p) => Route::Stage {
                part: Some(p.path),
                reason: p.reason,
            },
            None => Route::Apply,
        },
    }
}

/// What a chat is told when its change needs approval: nothing was changed, and the exact next
/// call. This is the only chat-facing text for the approval case.
pub fn stage_message(key: &str, part: Option<&str>, reason: &str) -> String {
    let place = match part {
        Some(p) => format!("{key}.{p}"),
        None => key.to_owned(),
    };
    format!(
        "{place} needs the user's approval, so nothing was changed. Stage it instead: call app_stage_settings with changes {{\"{key}\": <the value you tried>}} and a reason, and the user clicks Approve on one card; the outcome reaches you as a note. {reason}"
    )
}

/// What a chat is told when it stages a setting it can change itself: the call to make instead.
pub fn direct_message(key: &str) -> String {
    format!(
        "{key} needs no approval: change it yourself with app_update_settings (changes {{\"{key}\": <value>}}); the user sees a notice with Undo."
    )
}

/// What a chat is told for a name that is no setting at all (a typo, or an internal row).
pub fn not_a_setting(key: &str) -> String {
    let shown: String = key.chars().take(64).collect();
    format!("{shown} is not a Helm setting: app_get_settings lists the ones that exist.")
}

/// What no text a chat reads may say: each is a dead end that sends the chat to the user instead
/// of to a call it can make.
#[cfg(test)]
pub(crate) const DEAD_ENDS: &[&str] = &[
    "yours to change only",
    "ask the user to change it",
    "ask the user to change that",
    "user's to change",
    "is not a setting chats can change",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Every name Helm knows as a setting, with its access.
    fn every_setting() -> Vec<(&'static str, Access)> {
        CLASSIFIED
            .iter()
            .filter_map(|(k, c)| match c {
                PrefClass::Setting(a) => Some((*k, *a)),
                PrefClass::Internal => None,
            })
            .chain(NAMED.iter().copied())
            .collect()
    }

    /// Every setting resolves to Apply or Stage, the stage text names the key and the next call,
    /// and nothing is listed twice.
    #[test]
    fn every_setting_is_applied_or_staged_and_never_refused() {
        let mut seen = std::collections::HashSet::new();
        for (k, a) in every_setting() {
            assert!(seen.insert(k), "{k} is listed twice");
            assert_eq!(access(k), Some(a), "{k}");
            match route(a, None) {
                Route::Apply => assert_eq!(a, Access::Direct, "{k}"),
                Route::Stage { part, reason } => {
                    let said = stage_message(k, part, reason);
                    assert!(
                        said.contains("app_stage_settings") && said.contains(k),
                        "{said}"
                    );
                    assert!(
                        reason.ends_with('.') && reason.len() <= Access::MAX_REASON,
                        "{k}"
                    );
                    for dead in DEAD_ENDS {
                        assert!(!said.to_lowercase().contains(dead), "{said}");
                    }
                }
            }
        }
        for k in READ_ONLY {
            assert!(seen.insert(k) && access(k).is_none(), "{k}");
        }
        // A part of a Direct key that needs approval stages too, naming the part.
        let part = StagedPart::new("approvals", "It can approve things for you.");
        match route(Access::Direct, Some(&part)) {
            Route::Stage {
                part: Some("approvals"),
                reason,
            } => {
                assert!(stage_message("telegram", Some("approvals"), reason)
                    .contains("telegram.approvals"))
            }
            other => panic!("{other:?}"),
        }
        assert!(direct_message("theme").contains("app_update_settings"));
        assert!(not_a_setting("selection").contains("not a Helm setting"));
        assert!(access("nonsense").is_none() && access("hubConnection").is_none());
        assert!(
            access("selection").is_none(),
            "internal rows are no setting"
        );
    }

    /// The approval list stays short and each entry names its danger. Adding one means raising
    /// the cap, which is a decision.
    #[test]
    fn the_approval_list_is_short_and_each_entry_says_why() {
        let keys = approval_keys();
        assert!(keys.len() <= MAX_APPROVAL_KEYS, "{keys:?}");
        for k in &keys {
            assert!(
                reason(k).is_some_and(|w| !w.is_empty() && w.ends_with('.')),
                "{k}"
            );
        }
        for must in [
            "agentSettingsAll",
            "workmode",
            "skipPermissions",
            "dictation",
            "credentials",
        ] {
            assert!(keys.contains(&must), "{must}");
        }
        // Tuning is Direct, and so is what the old tiers held back.
        for k in [
            "theme",
            "keybindings",
            "fastMode",
            "usageGovernor",
            "localGames",
            "rolloverTokens",
        ] {
            assert_eq!(access(k), Some(Access::Direct), "{k}");
        }
        assert!(direct_pref_keys().len() > 50);
        for (id, title, what) in CREDENTIALS {
            assert!(!id.is_empty() && !title.is_empty() && !what.is_empty());
        }
    }

    fn files(dir: &Path, want: &dyn Fn(&str) -> bool, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                if !["target", "tests", "gen", "node_modules", "generated", "dev"]
                    .contains(&name.as_str())
                {
                    files(&path, want, out);
                }
            } else if want(&name) {
                out.push(path);
            }
        }
    }

    /// The non-test part of a Rust source: whole `#[cfg(test)]` items are dropped.
    fn without_tests(src: &str) -> String {
        let lines: Vec<&str> = src.lines().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < lines.len() {
            if lines[i].trim_start().starts_with("#[cfg(test)]") && !lines[i].starts_with(' ') {
                // Skip the attributes, then the item: to the closing brace at column 0, or the `;`.
                i += 1;
                while i < lines.len() && lines[i].trim_start().starts_with("#[") {
                    i += 1;
                }
                while i < lines.len() {
                    let l = lines[i];
                    i += 1;
                    if l == "}" || l.ends_with(';') {
                        break;
                    }
                }
                continue;
            }
            out.push_str(lines[i]);
            out.push('\n');
            i += 1;
        }
        out
    }

    /// No text anywhere in the app tells a chat to ask the user to do what it could not: the
    /// only alternative to changing a setting is staging it. This reads every non-test Rust and
    /// interface source, so writing a dead end again fails here.
    #[test]
    fn no_source_text_sends_a_chat_to_the_user_instead_of_a_call() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..");
        let mut rust = Vec::new();
        files(
            &root.join("host"),
            &|n| n.ends_with(".rs") && n != "tests.rs" && !n.ends_with("_tests.rs"),
            &mut rust,
        );
        let mut ui = Vec::new();
        files(
            &root.join("ui").join("src"),
            &|n| {
                (n.ends_with(".ts") || n.ends_with(".tsx"))
                    && !n.contains(".test.")
                    && !n.ends_with(".d.ts")
            },
            &mut ui,
        );
        assert!(
            rust.len() > 100 && ui.len() > 50,
            "the scan found too few files: wrong root?"
        );
        let mut found = Vec::new();
        for f in rust.iter().chain(ui.iter()) {
            // This file lists the phrases it forbids.
            if f.ends_with("hosttools/src/settings_allow.rs") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(f) else {
                continue;
            };
            let text = if f.extension().is_some_and(|e| e == "rs") {
                without_tests(&raw)
            } else {
                raw
            };
            let low = text.to_lowercase();
            for dead in DEAD_ENDS {
                if low.contains(dead) {
                    found.push(format!("{}: {dead}", f.display()));
                }
            }
        }
        assert!(
            found.is_empty(),
            "text that sends a chat to the user:\n{}",
            found.join("\n")
        );
        // The scanner itself: a dropped test module hides its text, a live line does not.
        let src = "fn a() { \"yours to change only\" }\n#[cfg(test)]\nmod tests {\n    \"ask the user to change it\"\n}\n";
        let kept = without_tests(src);
        assert!(kept.contains("yours to change only") && !kept.contains("ask the user"));
    }
}
