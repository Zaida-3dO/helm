//! Settings → Runtime: the runtime status and the
//! defaults new chats start with.
//!
//! | Op | Class | Args → result |
//! |---|---|---|
//! | `runtime_status` | blocking | `{}` → `{node, claude, problems}` ([`helm_workers::status::Found`] each) |
//! | `runtime_settings_get` | blocking | `{}` → [`RuntimeSettings`] |
//! | `runtime_settings_save` | maintenance | a patch of [`RuntimeSettings`] → the settings as stored |
//!
//! Where each setting lives:
//! - `executable`, `skipPermissions` (the launch permission mode: Bypass or Asks) and `fastMode`:
//!   `claude-runtime.json`, as does `worktreeTools` (let Claude Code's own worktree tools through,
//!   off by default). A worker reads it when it starts, so a change reaches a chat at its
//!   next worker start.
//! - `model` and `effort` for new chats: the `newChat` preference (`{model, effort}`), read by
//!   `chat_create` through [`new_chat_defaults`]; an unset effort is the catalogue's default.
//! - `responseVerbosity`: the user-only preference of that name (low by default).

use crate::catalogue;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use helm_workers::launch::{
    test_claude, ClaudeRuntime, SubagentScope, MAX_SOFT_TURNS, MIN_SOFT_TURNS,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("runtime_status", OpClass::Blocking, runtime_status)
        .blocking(
            "runtime_settings_get",
            OpClass::Blocking,
            runtime_settings_get,
        )
        .blocking(
            "runtime_settings_save",
            OpClass::Maintenance,
            runtime_settings_save,
        );
}

const NEW_CHAT: &str = "newChat";
const VERBOSITY: &str = "responseVerbosity";
const VERBOSITIES: [&str; 3] = ["low", "medium", "high"];
const MAX_MODEL_BYTES: usize = 200;

/// The model (`None`: Claude Code's own default) and effort a new chat starts with.
pub fn new_chat_defaults(conn: &rusqlite::Connection) -> (Option<String>, String) {
    let v = helm_core::prefs::read(conn, NEW_CHAT).unwrap_or(Value::Null);
    let model = v
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty() && m.len() <= MAX_MODEL_BYTES)
        .map(str::to_owned);
    let effort = v
        .get("effort")
        .and_then(Value::as_str)
        .filter(|e| catalogue::is_effort(e))
        .unwrap_or(catalogue::DEFAULT_EFFORT)
        .to_owned();
    (model, effort)
}

fn verbosity(conn: &rusqlite::Connection) -> String {
    helm_core::prefs::read(conn, VERBOSITY)
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|v| VERBOSITIES.contains(&v.as_str()))
        .unwrap_or_else(|| "low".into())
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSettings {
    /// Empty: discovered.
    pub executable: String,
    pub skip_permissions: bool,
    pub fast_mode: bool,
    /// Claude Code's own worktree tools (EnterWorktree, ExitWorktree) may be offered to chats.
    pub worktree_tools: bool,
    /// Which chats get turn and context limits on their sub-agents: `off`, `orchestrator`
    /// (chats with Orchestrator on, the default) or `all`.
    pub subagent_limits: SubagentScope,
    /// Refuse a sub-agent's tool calls past the hard limit (otherwise it is only told).
    pub subagent_hard_stop: bool,
    /// Turns before a sub-agent is told to hand off.
    pub subagent_soft_turns: u32,
    /// Cut very long outputs of Helm's own tools.
    pub tool_output_cap: bool,
    /// Claude Code's own schedule tools may be offered; each call is redirected to Helm's schedules.
    pub schedule_tools: bool,
    /// `None`: Claude Code's default model.
    pub model: Option<String>,
    pub effort: String,
    pub response_verbosity: String,
    /// `claude-runtime.json` could not be read; the defaults apply until it is saved again.
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeSettingsPatch {
    pub executable: Option<String>,
    pub skip_permissions: Option<bool>,
    pub fast_mode: Option<bool>,
    pub worktree_tools: Option<bool>,
    pub subagent_limits: Option<SubagentScope>,
    pub subagent_hard_stop: Option<bool>,
    pub subagent_soft_turns: Option<u32>,
    pub tool_output_cap: Option<bool>,
    pub schedule_tools: Option<bool>,
    /// An empty string clears it (Claude Code's default).
    pub model: Option<String>,
    pub effort: Option<String>,
    pub response_verbosity: Option<String>,
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn read_settings(ctx: &OpCtx) -> Result<RuntimeSettings, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let db = ctx.services.need::<Database>()?;
    let (rt, problem) = ClaudeRuntime::load(&profile.dir);
    let ((model, effort), response_verbosity) = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>((new_chat_defaults(tx), verbosity(tx))))
        .map_err(sql)?;
    Ok(RuntimeSettings {
        executable: rt.executable,
        skip_permissions: rt.skip_permissions,
        fast_mode: rt.fast_mode,
        worktree_tools: rt.worktree_tools,
        subagent_limits: rt.subagent_limits,
        subagent_hard_stop: rt.subagent_hard_stop,
        subagent_soft_turns: rt.subagent_soft_turns,
        tool_output_cap: rt.tool_output_cap,
        schedule_tools: rt.schedule_tools,
        model,
        effort,
        response_verbosity,
        problem,
    })
}

fn runtime_settings_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    serde_json::to_value(read_settings(ctx)?).map_err(|e| ApiError::internal(e.to_string()))
}

fn runtime_settings_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let p: RuntimeSettingsPatch = crate::ops::parse_args(args)?;
    if let Some(e) = p.effort.as_deref() {
        if !catalogue::is_effort(e) {
            return Err(ApiError::bad_args(
                "Effort must be low, medium, high, xhigh or max.",
            ));
        }
    }
    if let Some(v) = p.response_verbosity.as_deref() {
        if !VERBOSITIES.contains(&v) {
            return Err(ApiError::bad_args(
                "Response length must be low, medium or high.",
            ));
        }
    }
    if let Some(n) = p.subagent_soft_turns {
        if !(MIN_SOFT_TURNS..=MAX_SOFT_TURNS).contains(&n) {
            return Err(ApiError::bad_args(format!(
                "Sub-agent turns must be between {MIN_SOFT_TURNS} and {MAX_SOFT_TURNS}."
            )));
        }
    }
    if let Some(m) = p.model.as_deref() {
        if m.len() > MAX_MODEL_BYTES || m.chars().any(char::is_control) {
            return Err(ApiError::bad_args("That model name is not valid."));
        }
    }
    if let Some(x) = p.executable.as_deref() {
        let x = x.trim();
        if !x.is_empty() {
            let path = std::path::Path::new(x);
            if !path.is_absolute() {
                return Err(ApiError::bad_args(
                    "The Claude Code executable must be an absolute path, or empty to find it.",
                ));
            }
            helm_workers::status::resolve_claude(&Default::default(), x, None)
                .map_err(ApiError::bad_args)?;
        }
    }
    let profile = ctx.services.need::<Profile>()?;
    let db = ctx.services.need::<Database>()?;
    if p.executable.is_some()
        || p.skip_permissions.is_some()
        || p.fast_mode.is_some()
        || p.worktree_tools.is_some()
        || p.subagent_limits.is_some()
        || p.subagent_hard_stop.is_some()
        || p.subagent_soft_turns.is_some()
        || p.tool_output_cap.is_some()
        || p.schedule_tools.is_some()
    {
        let (mut rt, _) = ClaudeRuntime::load(&profile.dir);
        if let Some(x) = p.executable {
            rt.executable = x.trim().to_owned();
        }
        if let Some(s) = p.skip_permissions {
            rt.skip_permissions = s;
        }
        if let Some(f) = p.fast_mode {
            rt.fast_mode = f;
        }
        if let Some(w) = p.worktree_tools {
            rt.worktree_tools = w;
        }
        if let Some(v) = p.subagent_limits {
            rt.subagent_limits = v;
        }
        if let Some(v) = p.subagent_hard_stop {
            rt.subagent_hard_stop = v;
        }
        if let Some(v) = p.subagent_soft_turns {
            rt.subagent_soft_turns = v;
        }
        if let Some(v) = p.tool_output_cap {
            rt.tool_output_cap = v;
        }
        if let Some(w) = p.schedule_tools {
            rt.schedule_tools = w;
        }
        rt.save(&profile.dir)
            .map_err(|e| ApiError::internal(format!("claude-runtime.json not saved: {e}")))?;
    }
    if p.model.is_some() || p.effort.is_some() || p.response_verbosity.is_some() {
        db.write(|tx| {
            if p.model.is_some() || p.effort.is_some() {
                let (model, effort) = new_chat_defaults(tx);
                let model = match p.model.as_deref().map(str::trim) {
                    Some("") => None,
                    Some(m) => Some(m.to_owned()),
                    None => model,
                };
                let effort = p.effort.clone().unwrap_or(effort);
                // Merge: other fields of the preference (harness, provider) are kept.
                let mut stored = helm_core::prefs::read(tx, NEW_CHAT)
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
                stored["model"] = json!(model);
                stored["effort"] = json!(effort);
                helm_core::prefs::set_raw(tx, NEW_CHAT, &stored).map_err(|e| match e {
                    helm_core::prefs::PrefError::Sql(e) => e,
                    other => rusqlite::Error::InvalidParameterName(other.to_string()),
                })?;
            }
            if let Some(v) = p.response_verbosity.as_deref() {
                helm_core::prefs::set_raw(tx, VERBOSITY, &json!(v)).map_err(|e| match e {
                    helm_core::prefs::PrefError::Sql(e) => e,
                    other => rusqlite::Error::InvalidParameterName(other.to_string()),
                })?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .map_err(sql)?;
    }
    let s = read_settings(ctx)?;
    ctx.events.emit(
        helm_ipc::AppEvent::new("preferences").with(
            "changed",
            json!({ NEW_CHAT: {"model": s.model, "effort": s.effort}, VERBOSITY: s.response_verbosity }),
        ),
    );
    serde_json::to_value(s).map_err(|e| ApiError::internal(e.to_string()))
}

fn runtime_status(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let env = helm_workers::env::base_environment();
    let (rt, problem) = ClaudeRuntime::load(&profile.dir);
    let test = test_claude(
        cfg!(debug_assertions),
        std::env::var("HELM_TEST_CLAUDE").ok().as_deref(),
    );
    let home = env
        .get("USERPROFILE")
        .or_else(|| env.get("HOME"))
        .map(std::path::PathBuf::from);
    let node = helm_workers::status::node(&env);
    let claude = helm_workers::status::claude(&env, &rt, test.as_ref(), home.as_deref());
    let problems: Vec<String> = problem
        .into_iter()
        .chain(node.problem.iter().map(|p| format!("Node: {p}")))
        .chain(claude.problem.iter().map(|p| format!("Claude Code: {p}")))
        .collect();
    Ok(json!({"node": node, "claude": claude, "problems": problems}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{harness, main};
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settings_save_to_the_runtime_file_and_preferences_and_new_chats_use_them() {
        let h = harness();
        let caller = main();
        let api = |op: &'static str, args: Value| h.api.call(&caller, op, args);
        let s = api("runtime_settings_get", json!({})).await.unwrap();
        assert_eq!(s["fastMode"], true, "fast mode defaults on");
        assert_eq!(
            s["worktreeTools"], false,
            "Claude Code's worktree tools default off"
        );
        assert_eq!(
            s["scheduleTools"], false,
            "Claude Code's schedule tools default off"
        );
        assert_eq!(s["effort"], catalogue::DEFAULT_EFFORT);
        assert_eq!(s["model"], Value::Null);
        assert_eq!(s["responseVerbosity"], "low");
        assert_eq!(
            (
                &s["subagentLimits"],
                &s["subagentHardStop"],
                &s["subagentSoftTurns"],
                &s["toolOutputCap"]
            ),
            (
                &json!("orchestrator"),
                &json!(false),
                &json!(100),
                &json!(true)
            ),
            "the limits default to Orchestrator chats, notes only"
        );
        let bad = api("runtime_settings_save", json!({"subagentLimits": "some"}))
            .await
            .unwrap_err();
        assert_eq!(bad.code, "bad_args");
        let bad = api("runtime_settings_save", json!({"subagentSoftTurns": 3}))
            .await
            .unwrap_err();
        assert_eq!(bad.code, "bad_args");
        let s = api(
            "runtime_settings_save",
            json!({"subagentLimits": "all", "subagentHardStop": true, "subagentSoftTurns": 60, "toolOutputCap": false}),
        )
        .await
        .unwrap();
        assert_eq!(
            (
                &s["subagentLimits"],
                &s["subagentSoftTurns"],
                &s["toolOutputCap"]
            ),
            (&json!("all"), &json!(60), &json!(false))
        );
        let bad = api("runtime_settings_save", json!({"effort": "turbo"}))
            .await
            .unwrap_err();
        assert_eq!(bad.code, "bad_args");
        let bad = api(
            "runtime_settings_save",
            json!({"executable": "relative/claude.exe"}),
        )
        .await
        .unwrap_err();
        assert_eq!(bad.code, "bad_args");
        let s = api(
            "runtime_settings_save",
            json!({"fastMode": false, "worktreeTools": true, "scheduleTools": true, "skipPermissions": false, "model": "sonnet", "effort": "high", "responseVerbosity": "medium"}),
        )
        .await
        .unwrap();
        assert_eq!(
            (
                &s["fastMode"],
                &s["skipPermissions"],
                &s["model"],
                &s["effort"],
                &s["responseVerbosity"]
            ),
            (
                &json!(false),
                &json!(false),
                &json!("sonnet"),
                &json!("high"),
                &json!("medium")
            )
        );
        let file: Value = serde_json::from_slice(
            &std::fs::read(h._dir.path().join("prof/claude-runtime.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(file["fastMode"], false);
        assert_eq!(file["worktreeTools"], true);
        assert_eq!(file["scheduleTools"], true);
        assert_eq!(file["skipPermissions"], false);
        // A new chat starts with the defaults; explicit arguments still win.
        let c = api("chat_create", json!({"cwd": h.work.to_string_lossy()}))
            .await
            .unwrap();
        assert_eq!(
            (&c["model"], &c["effort"]),
            (&json!("sonnet"), &json!("high"))
        );
        let c = api(
            "chat_create",
            json!({"cwd": h.work.to_string_lossy(), "effort": "low"}),
        )
        .await
        .unwrap();
        assert_eq!(c["effort"], "low");
        // An empty model goes back to Claude Code's default.
        let s = api("runtime_settings_save", json!({"model": ""}))
            .await
            .unwrap();
        assert_eq!(s["model"], Value::Null);
        assert_eq!(s["effort"], "high", "the effort is kept");
        // The catalogue's default effort (what a new chat's picker shows) is the same default.
        let cat = api("model_catalogue", json!({"harness": "claude"}))
            .await
            .unwrap();
        assert_eq!(cat["defaultEffort"], "high");
        // Other fields of the preference survive a save.
        h.db.write(|tx| {
            helm_core::prefs::set_raw(
                tx,
                NEW_CHAT,
                &json!({"harness": "claude", "model": null, "effort": "high"}),
            )
            .map_err(|_| rusqlite::Error::InvalidQuery)
        })
        .unwrap();
        api("runtime_settings_save", json!({"effort": "low"}))
            .await
            .unwrap();
        let stored =
            h.db.snapshot(|tx| Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, NEW_CHAT)))
                .unwrap()
                .unwrap();
        assert_eq!(
            stored,
            json!({"harness": "claude", "model": null, "effort": "low"})
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_status_reports_node_and_claude() {
        let h = harness();
        let s = h
            .api
            .call(&main(), "runtime_status", json!({}))
            .await
            .unwrap();
        assert!(
            s["node"]["version"]
                .as_str()
                .is_some_and(|v| v.starts_with('v')),
            "{s}"
        );
        assert!(s["problems"].is_array());
        assert!(
            s["claude"]["path"].is_string() || s["claude"]["problem"].is_string(),
            "either found or why not: {s}"
        );
    }
}
