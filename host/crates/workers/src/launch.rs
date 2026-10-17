//! The launch description (`launch.json`) and what feeds it: the Claude runtime
//! file (`claude-runtime.json`) and the debug-only fake-claude seam.
//!
//! **API-key mode is never set.** `ClaudeLaunch` has no `apiKeyMode` field at all, so the
//! worker always strips the Anthropic credential variables from the claude child.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLaunch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub skip_permissions: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_session_id: Option<String>,
    /// Launch contributors' system-prompt fragments ([`crate::contrib`]); absent when none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<String>,
    /// Claude Code's fast mode from `claude-runtime.json`; always sent (`--settings`).
    pub fast_mode: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSendBehavior {
    /// `off`, `warn` or `redirect`.
    pub long_commands: String,
    pub opt_in: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub chat_id: String,
    pub harness: &'static str,
    pub cwd: String,
    pub claude: ClaudeLaunch,
    pub send_behavior: LaunchSendBehavior,
    /// Test-only worker limits; production launches omit them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<Value>,
}

impl Launch {
    /// Write `launch.json` into the chat's worker folder (owner-only, atomic).
    pub fn write(&self, worker_dir: &Path) -> std::io::Result<std::path::PathBuf> {
        helm_core::privfs::create_private_dir_all(worker_dir)?;
        let path = worker_dir.join("launch.json");
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        helm_core::privfs::write_atomic(&path, &bytes)?;
        Ok(path)
    }
}

/// `<profile>/claude-runtime.json` (settings-schema): `{executable, skipPermissions, fastMode}`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaudeRuntime {
    /// Empty means "discover" (the worker's resolver).
    #[serde(default)]
    pub executable: String,
    #[serde(default = "yes")]
    pub skip_permissions: bool,
    /// Claude Code's fast mode. Off by default.
    #[serde(default)]
    pub fast_mode: bool,
}

fn yes() -> bool {
    true
}

impl Default for ClaudeRuntime {
    fn default() -> Self {
        Self {
            executable: String::new(),
            skip_permissions: true,
            fast_mode: false,
        }
    }
}

impl ClaudeRuntime {
    /// Read the file strictly. Missing means the defaults; an unreadable or invalid file is
    /// reported and also means the defaults (Settings → Runtime will list the problem later).
    pub fn load(profile_dir: &Path) -> (Self, Option<String>) {
        let path = profile_dir.join("claude-runtime.json");
        match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (Self::default(), Some(format!("claude-runtime.json: {e}"))),
            Ok(bytes) if bytes.len() > 64 * 1024 => (
                Self::default(),
                Some("claude-runtime.json is larger than 64 KiB".into()),
            ),
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(rt) => (rt, None),
                Err(e) => (Self::default(), Some(format!("claude-runtime.json: {e}"))),
            },
        }
    }
}

/// `HELM_TEST_CLAUDE = {"command": "<node.exe>", "args": ["<fake-claude.ts>", "<scenario.json>"]}`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestClaude {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// The fake-claude seam, honoured **only** when `debug` is true (callers pass
/// `cfg!(debug_assertions)`). A release build ignores the variable entirely.
pub fn test_claude(debug: bool, raw: Option<&str>) -> Option<TestClaude> {
    if !debug {
        return None;
    }
    let raw = raw?;
    match serde_json::from_str(raw) {
        Ok(t) => Some(t),
        Err(e) => {
            log::error!("HELM_TEST_CLAUDE is not valid ({e}); ignoring it");
            None
        }
    }
}

/// Fill the claude section from the runtime file and the test seam (the seam wins).
pub fn claude_launch(
    runtime: &ClaudeRuntime,
    test: Option<&TestClaude>,
    model: Option<String>,
    effort: Option<String>,
    resume_session_id: Option<String>,
) -> ClaudeLaunch {
    let mut c = ClaudeLaunch {
        command: (!runtime.executable.is_empty()).then(|| runtime.executable.clone()),
        command_args: None,
        model,
        effort,
        skip_permissions: runtime.skip_permissions,
        resume_session_id,
        append_system_prompt: None,
        fast_mode: runtime.fast_mode,
    };
    if let Some(t) = test {
        c.command = Some(t.command.clone());
        c.command_args = Some(t.args.clone());
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_builds_ignore_the_test_seam() {
        let raw = r#"{"command": "node.exe", "args": ["fake.ts", "s.json"]}"#;
        assert_eq!(test_claude(false, Some(raw)), None);
        let t = test_claude(true, Some(raw)).unwrap();
        assert_eq!(t.command, "node.exe");
        assert_eq!(test_claude(true, Some("{nope")), None);
    }

    #[test]
    fn runtime_file_defaults_and_strictness() {
        let dir = tempfile::tempdir().unwrap();
        let (rt, err) = ClaudeRuntime::load(dir.path());
        assert_eq!((rt.clone(), err), (ClaudeRuntime::default(), None));
        assert!(rt.skip_permissions, "skip permission prompts defaults on");
        std::fs::write(
            dir.path().join("claude-runtime.json"),
            r#"{"skipPermissions": false}"#,
        )
        .unwrap();
        assert!(!ClaudeRuntime::load(dir.path()).0.skip_permissions);
        std::fs::write(
            dir.path().join("claude-runtime.json"),
            r#"{"skipPermissions": false, "extra": 1}"#,
        )
        .unwrap();
        let (rt, err) = ClaudeRuntime::load(dir.path());
        assert!(rt.skip_permissions, "an invalid file reads as the defaults");
        assert!(err.is_some());
    }

    #[test]
    fn the_launch_never_carries_api_key_mode() {
        let rt = ClaudeRuntime::default();
        let test = TestClaude {
            command: "node.exe".into(),
            args: vec!["fake.ts".into()],
        };
        let launch = Launch {
            chat_id: "c".into(),
            harness: "claude",
            cwd: "C:/w".into(),
            claude: claude_launch(&rt, Some(&test), Some("opus".into()), None, None),
            send_behavior: LaunchSendBehavior {
                long_commands: "redirect".into(),
                opt_in: false,
            },
            limits: None,
        };
        let v = serde_json::to_value(&launch).unwrap();
        assert_eq!(v["claude"]["command"], "node.exe");
        assert_eq!(v["claude"]["commandArgs"][0], "fake.ts");
        assert_eq!(v["claude"]["skipPermissions"], true);
        assert_eq!(v["sendBehavior"]["longCommands"], "redirect");
        assert_eq!(
            v["claude"]["fastMode"], false,
            "fast mode is explicit and off by default"
        );
        assert!(!v.to_string().contains("apiKeyMode"));
        assert!(
            !v.to_string().contains("appendSystemPrompt"),
            "absent unless a contributor adds a fragment"
        );
    }
}
