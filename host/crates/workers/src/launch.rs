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
    /// Resume `resumeSessionId` as a new session (`--fork-session`); absent unless set.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fork_session: bool,
    /// Native Claude Code MCP servers off (`--strict-mcp-config`): only Helm's own server loads.
    /// Absent unless the user turned native MCP off.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub strict_mcp_config: bool,
    /// Native MCP servers the CLI must not use (their tools are disallowed); absent when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled_mcp_servers: Vec<String>,
    /// Claude Code's own worktree tools (EnterWorktree, ExitWorktree) may be offered. Absent
    /// unless the user turned them on in Settings > Runtime.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub worktree_tools: bool,
    /// Turns (tool calls) a sub-agent may make before it is told to hand off and stop; 0 means no
    /// sub-agent limits in this chat.
    pub subagent_soft_turns: u32,
    /// Past 30% over the limit, further sub-agent tool calls are refused (otherwise only noted).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub subagent_hard_stop: bool,
    /// Cut very long outputs of Helm's own tools to a head and a tail.
    pub tool_output_cap: bool,
    /// Claude Code's own schedule tools may be offered (every call is refused with directions to
    /// Helm's schedules). Absent unless the user turned them on in Settings > Runtime.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub schedule_tools: bool,
    /// The worker compares the checkout before and after each Bash call, to list the files a
    /// shell command changed on the turn's change card.
    pub shell_changes: bool,
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
    /// The host tools this launch lists (`helm_hosttools::CatalogueEntry`); the
    /// worker serves them through its in-process `helm` MCP server. Absent when host tools are
    /// not wired (tests), so such a launch is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_tools: Option<Value>,
    /// The folder the worker makes one private request folder in per host-tool call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_tools_directory: Option<String>,
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

/// `<profile>/claude-runtime.json` (settings-schema): `{executable, skipPermissions, fastMode, worktreeTools, scheduleTools}`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaudeRuntime {
    /// Empty means "discover" (the worker's resolver).
    #[serde(default)]
    pub executable: String,
    #[serde(default = "yes")]
    pub skip_permissions: bool,
    /// Claude Code's fast mode. On by default: a missing key means on, an explicit
    /// `false` turns it off.
    #[serde(default = "yes")]
    pub fast_mode: bool,
    /// Let Claude Code's own worktree tools through. Off by default: a missing key means off.
    #[serde(default)]
    pub worktree_tools: bool,
    /// Which chats get sub-agent turn and context limits. A missing key means the default,
    /// Orchestrator chats only.
    #[serde(default)]
    pub subagent_limits: SubagentScope,
    /// Refuse a sub-agent's tool calls past the hard limit. Off by default: it is only told.
    #[serde(default)]
    pub subagent_hard_stop: bool,
    /// Turns before a sub-agent is told to hand off (the hard stop comes 30% later).
    #[serde(default = "default_soft_turns")]
    pub subagent_soft_turns: u32,
    /// Cut very long outputs of Helm's own tools. On by default.
    #[serde(default = "yes")]
    pub tool_output_cap: bool,
    /// Let Claude Code's own schedule tools be offered (calls are redirected to Helm's
    /// schedules). Off by default: a missing key means off.
    #[serde(default)]
    pub schedule_tools: bool,
}

/// Which chats the sub-agent limits apply to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SubagentScope {
    Off,
    /// Chats with Orchestrator on.
    #[default]
    Orchestrator,
    All,
}

impl ClaudeRuntime {
    /// The sub-agent turn limit for a chat (`orchestrator_on`: Orchestrator is on for it); 0: none.
    pub fn subagent_turns(&self, orchestrator_on: bool) -> u32 {
        let applies = match self.subagent_limits {
            SubagentScope::Off => false,
            SubagentScope::Orchestrator => orchestrator_on,
            SubagentScope::All => true,
        };
        if applies {
            self.subagent_soft_turns
                .clamp(MIN_SOFT_TURNS, MAX_SOFT_TURNS)
        } else {
            0
        }
    }
}

pub const DEFAULT_SOFT_TURNS: u32 = 100;
pub const MIN_SOFT_TURNS: u32 = 20;
pub const MAX_SOFT_TURNS: u32 = 1000;

fn default_soft_turns() -> u32 {
    DEFAULT_SOFT_TURNS
}

fn yes() -> bool {
    true
}

impl Default for ClaudeRuntime {
    fn default() -> Self {
        Self {
            executable: String::new(),
            skip_permissions: true,
            fast_mode: true,
            worktree_tools: false,
            subagent_limits: SubagentScope::Orchestrator,
            subagent_hard_stop: false,
            subagent_soft_turns: DEFAULT_SOFT_TURNS,
            tool_output_cap: true,
            schedule_tools: false,
        }
    }
}

impl ClaudeRuntime {
    /// Write `claude-runtime.json` (owner-only, atomic). Settings → Runtime saves it; workers read
    /// it at their next start.
    pub fn save(&self, profile_dir: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        helm_core::privfs::write_atomic(&profile_dir.join("claude-runtime.json"), &bytes)
    }

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
        fork_session: false,
        strict_mcp_config: false,
        disabled_mcp_servers: Vec::new(),
        worktree_tools: runtime.worktree_tools,
        // Chats with Orchestrator on get their limit when the launch learns that.
        subagent_soft_turns: runtime.subagent_turns(false),
        subagent_hard_stop: runtime.subagent_hard_stop,
        tool_output_cap: runtime.tool_output_cap,
        schedule_tools: runtime.schedule_tools,
        shell_changes: true,
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
        assert!(rt.fast_mode, "fast mode defaults on");
        assert!(
            !rt.worktree_tools,
            "Claude Code's worktree tools default off"
        );
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
        // An explicit false turns fast mode off; a save reads back as written.
        std::fs::write(
            dir.path().join("claude-runtime.json"),
            r#"{"fastMode": false}"#,
        )
        .unwrap();
        let (rt, err) = ClaudeRuntime::load(dir.path());
        assert!(!rt.fast_mode && err.is_none());
        let mine = ClaudeRuntime {
            executable: "C:/tools/claude.exe".into(),
            skip_permissions: false,
            fast_mode: false,
            worktree_tools: true,
            subagent_limits: SubagentScope::Off,
            subagent_hard_stop: true,
            subagent_soft_turns: 40,
            tool_output_cap: false,
            schedule_tools: true,
        };
        mine.save(dir.path()).unwrap();
        assert_eq!(ClaudeRuntime::load(dir.path()), (mine.clone(), None));
        // The limits reach the launch: a missing key is Orchestrator chats only with no hard
        // stop; "off" is never; "all" is every chat; a value is clamped.
        std::fs::write(
            dir.path().join("claude-runtime.json"),
            r#"{"fastMode": true}"#,
        )
        .unwrap();
        let dflt = ClaudeRuntime::load(dir.path()).0;
        assert_eq!(dflt, ClaudeRuntime::default());
        assert_eq!(dflt.subagent_limits, SubagentScope::Orchestrator);
        assert!(!dflt.subagent_hard_stop && dflt.tool_output_cap);
        assert_eq!(
            dflt.subagent_turns(false),
            0,
            "a chat without Orchestrator is untouched"
        );
        assert_eq!(dflt.subagent_turns(true), 100);
        let launch = claude_launch(&dflt, None, None, None, None);
        assert_eq!(
            (launch.subagent_soft_turns, launch.subagent_hard_stop),
            (0, false)
        );
        let all = ClaudeRuntime {
            subagent_limits: SubagentScope::All,
            subagent_soft_turns: 5,
            ..mine.clone()
        };
        assert_eq!(all.subagent_turns(false), MIN_SOFT_TURNS);
        assert_eq!(mine.subagent_turns(true), 0, "off is off");
        let off = claude_launch(&mine, None, None, None, None);
        assert_eq!(
            (
                off.subagent_soft_turns,
                off.tool_output_cap,
                off.subagent_hard_stop
            ),
            (0, false, true)
        );
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
            host_tools: None,
            chat_tools_directory: None,
        };
        let v = serde_json::to_value(&launch).unwrap();
        assert_eq!(v["claude"]["command"], "node.exe");
        assert_eq!(v["claude"]["commandArgs"][0], "fake.ts");
        assert_eq!(v["claude"]["skipPermissions"], true);
        assert_eq!(v["sendBehavior"]["longCommands"], "redirect");
        assert_eq!(
            v["claude"]["fastMode"], true,
            "fast mode is explicit and on by default"
        );
        assert!(!v.to_string().contains("apiKeyMode"));
        assert!(
            v["claude"].get("worktreeTools").is_none(),
            "absent unless the user turned Claude Code's worktree tools on"
        );
        let on = ClaudeRuntime {
            worktree_tools: true,
            ..ClaudeRuntime::default()
        };
        let v = serde_json::to_value(claude_launch(&on, None, None, None, None)).unwrap();
        assert_eq!(v["worktreeTools"], true);
        assert!(
            v.get("scheduleTools").is_none(),
            "absent unless the user turned Claude Code's schedule tools on"
        );
        let on = ClaudeRuntime {
            schedule_tools: true,
            ..ClaudeRuntime::default()
        };
        let v = serde_json::to_value(claude_launch(&on, None, None, None, None)).unwrap();
        assert_eq!(v["scheduleTools"], true);
        assert_eq!(
            v["shellChanges"], true,
            "shell-command change capture is on for every Claude Code chat"
        );
        assert!(
            !v.to_string().contains("appendSystemPrompt"),
            "absent unless a contributor adds a fragment"
        );
    }
}
