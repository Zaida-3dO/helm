//! The Claude Code runtime settings (Settings → Runtime's launch switches) as keys of
//! `app_get_settings` and `app_update_settings`.
//!
//! They live in `claude-runtime.json`, which a worker reads when it starts, so a change reaches a
//! chat at its next worker start.
//!
//! | Key | Value | Class |
//! |---|---|---|
//! | `shellChanges` | record which files shell commands change, for the turn's change card | chat changes it |
//! | `lowPriority` | run agents, their tools and Helm's helpers at below-normal priority | chat changes it |
//! | `toolOutputCap` | cut very long outputs of Helm's own tools | chat changes it |
//! | `subagentSoftTurns` | turns before a sub-agent is told to hand off, 20 to 1000 | chat changes it |
//! | `fastMode` | Claude Code's fast mode | needs approval |
//! | `worktreeTools` | let Claude Code's own worktree tools through | needs approval |
//! | `scheduleTools` | let Claude Code's own schedule tools be offered | needs approval |
//! | `skipPermissions` | launch chats with permissions bypassed, or asking | needs approval |
//! | `subagentLimits` | which chats get sub-agent limits: `off`, `orchestrator` or `all` | needs approval |
//! | `subagentHardStop` | refuse a sub-agent's tool calls past the hard limit | needs approval |
//! | `claudeExecutable` | the program Helm runs as Claude Code (an absolute path; empty finds it) | needs approval |
//!
//! The class of each key is in the risk table of `helm_hosttools::settings_allow`; this extension
//! only reads, checks and writes the file. Checking a staged change never writes it.

use helm_hosttools::SettingsExtension;
use helm_ipc::AppEvent;
use helm_workers::launch::{ClaudeRuntime, SubagentScope, MAX_SOFT_TURNS, MIN_SOFT_TURNS};
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub const SHELL_CHANGES: &str = "shellChanges";
pub const LOW_PRIORITY: &str = "lowPriority";
pub const TOOL_OUTPUT_CAP: &str = "toolOutputCap";
pub const SOFT_TURNS: &str = "subagentSoftTurns";
pub const FAST_MODE: &str = "fastMode";
pub const WORKTREE_TOOLS: &str = "worktreeTools";
pub const SCHEDULE_TOOLS: &str = "scheduleTools";
pub const SKIP_PERMISSIONS: &str = "skipPermissions";
pub const SUBAGENT_LIMITS: &str = "subagentLimits";
pub const HARD_STOP: &str = "subagentHardStop";
pub const EXECUTABLE: &str = "claudeExecutable";

const KEYS: [&str; 11] = [
    SHELL_CHANGES,
    LOW_PRIORITY,
    TOOL_OUTPUT_CAP,
    SOFT_TURNS,
    FAST_MODE,
    WORKTREE_TOOLS,
    SCHEDULE_TOOLS,
    SKIP_PERMISSIONS,
    SUBAGENT_LIMITS,
    HARD_STOP,
    EXECUTABLE,
];

const PAGE: &str = "Settings > Runtime";

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension(profile_dir: PathBuf) -> Arc<dyn SettingsExtension> {
    Arc::new(RuntimeKeys { dir: profile_dir })
}

struct RuntimeKeys {
    dir: PathBuf,
}

fn value_of(rt: &ClaudeRuntime, key: &str) -> Value {
    match key {
        SHELL_CHANGES => json!(rt.shell_changes),
        LOW_PRIORITY => json!(rt.low_priority),
        TOOL_OUTPUT_CAP => json!(rt.tool_output_cap),
        SOFT_TURNS => json!(rt.subagent_soft_turns),
        FAST_MODE => json!(rt.fast_mode),
        WORKTREE_TOOLS => json!(rt.worktree_tools),
        SCHEDULE_TOOLS => json!(rt.schedule_tools),
        SKIP_PERMISSIONS => json!(rt.skip_permissions),
        SUBAGENT_LIMITS => serde_json::to_value(rt.subagent_limits).unwrap_or(Value::Null),
        HARD_STOP => json!(rt.subagent_hard_stop),
        _ => json!(rt.executable),
    }
}

/// `rt` with `value` (null: the key's default) set for `key`, or why that is refused.
fn changed(rt: &ClaudeRuntime, key: &str, value: &Value) -> Result<ClaudeRuntime, String> {
    let mut out = rt.clone();
    let v = if value.is_null() {
        value_of(&ClaudeRuntime::default(), key)
    } else {
        value.clone()
    };
    let flag = || {
        v.as_bool()
            .ok_or_else(|| format!("{key} is true or false."))
    };
    match key {
        SHELL_CHANGES => out.shell_changes = flag()?,
        LOW_PRIORITY => out.low_priority = flag()?,
        TOOL_OUTPUT_CAP => out.tool_output_cap = flag()?,
        FAST_MODE => out.fast_mode = flag()?,
        WORKTREE_TOOLS => out.worktree_tools = flag()?,
        SCHEDULE_TOOLS => out.schedule_tools = flag()?,
        SKIP_PERMISSIONS => out.skip_permissions = flag()?,
        HARD_STOP => out.subagent_hard_stop = flag()?,
        SOFT_TURNS => {
            let n = v
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| (MIN_SOFT_TURNS..=MAX_SOFT_TURNS).contains(n))
                .ok_or_else(|| {
                    format!(
                        "{key} is a whole number of turns from {MIN_SOFT_TURNS} to {MAX_SOFT_TURNS}."
                    )
                })?;
            out.subagent_soft_turns = n;
        }
        SUBAGENT_LIMITS => {
            out.subagent_limits = serde_json::from_value::<SubagentScope>(v)
                .map_err(|_| format!("{key} is \"off\", \"orchestrator\" or \"all\"."))?;
        }
        EXECUTABLE => {
            let text = v
                .as_str()
                .ok_or_else(|| format!("{key} is an absolute path, or empty to find it."))?
                .trim();
            if !text.is_empty() {
                if !std::path::Path::new(text).is_absolute() {
                    return Err(format!(
                        "{key} must be an absolute path to the Claude Code program, or empty to find it."
                    ));
                }
                helm_workers::status::resolve_claude(&Default::default(), text, None)?;
            }
            out.executable = text.to_owned();
        }
        _ => return Err(format!("{key} is not a runtime setting.")),
    }
    Ok(out)
}

impl RuntimeKeys {
    fn load(&self) -> ClaudeRuntime {
        ClaudeRuntime::load(&self.dir).0
    }

    /// Check the change; with `store`, write it too. Answers the value before and after.
    fn put(&self, key: &str, value: &Value, store: bool) -> Result<(Value, Value), String> {
        let before = self.load();
        let after = changed(&before, key, value)?;
        if store {
            after
                .save(&self.dir)
                .map_err(|e| format!("claude-runtime.json was not saved: {e}"))?;
        }
        Ok((value_of(&before, key), value_of(&after, key)))
    }
}

impl SettingsExtension for RuntimeKeys {
    fn keys(&self) -> Vec<&'static str> {
        KEYS.to_vec()
    }

    fn describe(&self, key: &str) -> Option<Value> {
        let d = ClaudeRuntime::default();
        let about = match key {
            SHELL_CHANGES => "Record which files shell commands change, for the turn's change card.",
            LOW_PRIORITY => "Run agents, their tools and Helm's helpers at below-normal priority, so games and the rest of the PC stay responsive.",
            TOOL_OUTPUT_CAP => "Cut very long outputs of Helm's own tools.",
            FAST_MODE => "Claude Code's fast mode: faster output from the same model.",
            WORKTREE_TOOLS => "Let Claude Code's own worktree tools (EnterWorktree, ExitWorktree) through to chats.",
            SCHEDULE_TOOLS => "Let Claude Code's own schedule tools be offered; each call is redirected to Helm's schedules.",
            SKIP_PERMISSIONS => "Launch chats with permissions bypassed (true) or asking (false).",
            HARD_STOP => "Refuse a sub-agent's tool calls past the hard limit (otherwise it is only told).",
            SOFT_TURNS => "Turns before a sub-agent is told to hand off; the hard stop comes 30% later.",
            SUBAGENT_LIMITS => "Which chats get turn and context limits on their sub-agents: off, orchestrator (chats with Orchestrator on) or all.",
            EXECUTABLE => "The program Helm runs as Claude Code: an absolute path to it, or empty to find it.",
            _ => return None,
        };
        let about = format!("{PAGE}: {about} It reaches a chat when its worker next starts.");
        let mut o = match key {
            SOFT_TURNS => json!({
                "type": "integer",
                "min": MIN_SOFT_TURNS,
                "max": MAX_SOFT_TURNS,
            }),
            SUBAGENT_LIMITS => json!({"type": "enum", "allowed": ["off", "orchestrator", "all"]}),
            EXECUTABLE => json!({"type": "string", "maxLength": 500}),
            _ => json!({"type": "boolean"}),
        };
        o["default"] = value_of(&d, key);
        o["about"] = json!(about);
        Some(o)
    }

    fn read(&self, _tx: &Transaction<'_>, key: &str) -> Value {
        value_of(&self.load(), key)
    }

    fn apply(
        &self,
        _tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.put(key, value, true)
    }

    fn check_approved(
        &self,
        _tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.put(key, value, false)
    }

    fn restore(&self, _tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), String> {
        self.put(key, value, true).map(|_| ())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("runtime_settings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    type Put = Result<(Value, Value), String>;

    #[test]
    fn the_launch_switches_are_read_checked_written_and_restored() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.db"), &MigrationRegistry::new()).unwrap();
        let ext = RuntimeKeys {
            dir: dir.path().to_path_buf(),
        };
        let go = |f: &dyn Fn(&Transaction<'_>) -> Put| {
            db.write(|tx| Ok::<_, rusqlite::Error>(f(tx))).unwrap()
        };
        let read = |key: &str| {
            db.snapshot(|tx| Ok::<_, rusqlite::Error>(ext.read(tx, key)))
                .unwrap()
        };
        for k in ext.keys() {
            let d = ext.describe(k).unwrap();
            assert_eq!(d["default"], value_of(&ClaudeRuntime::default(), k), "{k}");
            assert!(!ext.needs_all_settings(k), "{k}");
        }
        assert_eq!(read(FAST_MODE), json!(true));
        assert_eq!(read(EXECUTABLE), json!(""));
        // Checking a staged change shows the result and writes nothing.
        let (b, a) = go(&|tx| ext.check_approved(tx, FAST_MODE, &json!(false))).unwrap();
        assert_eq!((b, a), (json!(true), json!(false)));
        assert!(!dir.path().join("claude-runtime.json").exists());
        assert_eq!(read(FAST_MODE), json!(true));
        // Applying writes the file; the others keep their values.
        go(&|tx| ext.apply_approved(tx, FAST_MODE, &json!(false))).unwrap();
        go(&|tx| ext.apply(tx, SOFT_TURNS, &json!(50))).unwrap();
        go(&|tx| ext.apply(tx, SUBAGENT_LIMITS, &json!("all"))).unwrap();
        let rt = ClaudeRuntime::load(dir.path()).0;
        assert_eq!(
            (
                rt.fast_mode,
                rt.subagent_soft_turns,
                rt.subagent_limits,
                rt.low_priority
            ),
            (false, 50, SubagentScope::All, true)
        );
        // Values the Settings page would refuse are refused with the same words.
        for (key, bad, part) in [
            (SOFT_TURNS, json!(5), "from 20 to 1000"),
            (SUBAGENT_LIMITS, json!("some"), "\"off\""),
            (FAST_MODE, json!("yes"), "true or false"),
            (EXECUTABLE, json!("claude.exe"), "absolute path"),
            (EXECUTABLE, json!(7), "absolute path"),
        ] {
            let why = go(&|tx| ext.apply(tx, key, &bad)).unwrap_err();
            assert!(why.contains(part), "{key}: {why}");
        }
        // null restores a default; Undo puts a value back.
        let (before, _) = go(&|tx| ext.apply(tx, SOFT_TURNS, &Value::Null)).unwrap();
        assert_eq!(read(SOFT_TURNS), json!(100));
        db.write(|tx| Ok::<_, rusqlite::Error>(ext.restore(tx, SOFT_TURNS, &before)))
            .unwrap()
            .unwrap();
        assert_eq!(read(SOFT_TURNS), json!(50));
        assert_eq!(ext.changed_event(FAST_MODE).kind, "runtime_settings");
    }
}
