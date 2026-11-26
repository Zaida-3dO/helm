//! Settings → Runtime: the runtime status and the
//! defaults new chats start with.
//!
//! | Op | Class | Args → result |
//! |---|---|---|
//! | `runtime_status` | blocking | `{}` → `{node, claude, problems}` ([`helm_workers::status::Found`] each) |
//! | `runtime_settings_get` | blocking | `{profileId?}` → [`RuntimeSettings`] |
//! | `runtime_settings_save` | maintenance | a patch of [`RuntimeSettings`] (and `profileId?`) → the settings as stored |
//!
//! Where each setting lives:
//! - `executable`, `skipPermissions` (the launch permission mode: Bypass or Asks) and `fastMode`:
//!   `claude-runtime.json`, as does `worktreeTools` (let Claude Code's own worktree tools through,
//!   off by default) and `lowPriority` (run agents and helpers at below-normal priority, on by
//!   default). A worker reads it when it starts, so a change reaches a chat at its
//!   next worker start. `fastMode` is only the default new chats start with: a chat with its own
//!   pick (`chats.fast_mode`, set by the composer's lightning toggle through `chat_set_fast_mode`)
//!   uses that instead, and keeps it across a change of the default.
//! - `model` and `effort` for new chats, and `responseVerbosity`: kept per profile in the
//!   `profileDefaults` preference, for the profile named by `profileId` (the chat's owner, from the
//!   composer's star) or, with none named, the one acting now ([`helm_core::prefs::acting_profile`]).
//!   A field the profile has not set reads from the install-wide `newChat` preference
//!   (`{model, effort}`) and `responseVerbosity`, which are also what is saved where no profile
//!   exists yet. `chat_create` reads them through [`new_chat_defaults`]; an unset effort is the
//!   catalogue's default and an unset reply length is low.
//! - `permissionMode`: the mode the user last picked with the composer's chip in any chat
//!   ([`default_permission_mode`]), kept in the preference `newChatPermissionMode`, which a chat can
//!   only stage for the user to approve. A chat the user starts from the new-chat page starts in it, and it
//!   survives a restart. Unset, a chat starts in the launch mode (`skipPermissions`). A
//!   `permissionMode` left inside `newChat` by an earlier version is ignored.
//!
//! `fastMode` and `permissionMode` are install-wide, not per profile.

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
const PERMISSION_MODE: &str = "newChatPermissionMode";
const VERBOSITY: &str = "responseVerbosity";
const VERBOSITIES: [&str; 3] = ["low", "medium", "high"];
const MAX_MODEL_BYTES: usize = 200;

/// A stored model name that can be used: trimmed, not empty and not too long.
fn usable_model(m: &str) -> Option<String> {
    let m = m.trim();
    (!m.is_empty() && m.len() <= MAX_MODEL_BYTES).then(|| m.to_owned())
}

/// The model (`None`: Claude Code's own default) and effort a new chat starts with, for the
/// profile acting now.
pub fn new_chat_defaults(conn: &rusqlite::Connection) -> (Option<String>, String) {
    new_chat_defaults_for(conn, helm_core::prefs::acting_profile(conn).as_deref())
}

/// [`new_chat_defaults`] for one profile: what it has chosen, else the install-wide default.
pub fn new_chat_defaults_for(
    conn: &rusqlite::Connection,
    profile: Option<&str>,
) -> (Option<String>, String) {
    let v = helm_core::prefs::read(conn, NEW_CHAT).unwrap_or(Value::Null);
    let mut model = v
        .get("model")
        .and_then(Value::as_str)
        .and_then(usable_model);
    let mut effort = v
        .get("effort")
        .and_then(Value::as_str)
        .filter(|e| catalogue::is_effort(e))
        .unwrap_or(catalogue::DEFAULT_EFFORT)
        .to_owned();
    // A null model is the profile's own choice of Claude Code's default.
    match helm_core::prefs::profile_default(conn, profile, "model") {
        Some(Value::Null) => model = None,
        Some(Value::String(m)) => model = usable_model(&m).or(model),
        _ => {}
    }
    if let Some(e) = helm_core::prefs::profile_default(conn, profile, "effort")
        .as_ref()
        .and_then(Value::as_str)
        .filter(|e| catalogue::is_effort(e))
    {
        effort = e.to_owned();
    }
    (model, effort)
}

/// The permission mode the user last picked in a chat's chip, if they have picked one.
pub fn default_permission_mode(conn: &rusqlite::Connection) -> Option<String> {
    helm_core::prefs::read(conn, PERMISSION_MODE)?
        .as_str()
        .filter(|m| *m != "plan" && crate::submit::PERMISSION_MODES.contains(m))
        .map(str::to_owned)
}

/// Remembers a chip pick as the mode new chats start in. Plan mode is for planning in one chat,
/// so it is never carried to new chats (the earlier default stays).
pub fn save_default_permission_mode(
    tx: &rusqlite::Transaction<'_>,
    mode: &str,
) -> rusqlite::Result<()> {
    if mode == "plan" {
        return Ok(());
    }
    helm_core::prefs::set_raw(tx, PERMISSION_MODE, &json!(mode)).map_err(|e| match e {
        helm_core::prefs::PrefError::Sql(e) => e,
        other => rusqlite::Error::InvalidParameterName(other.to_string()),
    })
}

/// Gives a chat just created the permission mode the user last picked, for its first worker.
pub fn start_in_default_mode(db: &Database, chat_id: &str) {
    if let Some(mode) = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(default_permission_mode(tx)))
        .ok()
        .flatten()
    {
        crate::submit::seed_mode(chat_id, &mode);
    }
}

/// The reply length new chats start with for one profile: what it has chosen, else the
/// install-wide default (`low` when unset).
pub fn verbosity_for(conn: &rusqlite::Connection, profile: Option<&str>) -> String {
    let valid = |v: &Value| {
        v.as_str()
            .filter(|v| VERBOSITIES.contains(v))
            .map(str::to_owned)
    };
    helm_core::prefs::profile_default(conn, profile, "responseVerbosity")
        .as_ref()
        .and_then(valid)
        .or_else(|| {
            helm_core::prefs::read(conn, VERBOSITY)
                .as_ref()
                .and_then(valid)
        })
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
    /// Record which files shell commands change, for the turn's change card.
    pub shell_changes: bool,
    /// Run agents, their tools and Helm's helpers at below-normal priority.
    pub low_priority: bool,
    /// `None`: Claude Code's default model.
    pub model: Option<String>,
    pub effort: String,
    pub response_verbosity: String,
    /// The mode the user last picked with a chat's permission chip; new chats start in it.
    /// `None`: new chats start in the launch mode.
    pub permission_mode: Option<String>,
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
    pub shell_changes: Option<bool>,
    pub low_priority: Option<bool>,
    /// An empty string clears it (Claude Code's default).
    pub model: Option<String>,
    pub effort: Option<String>,
    pub response_verbosity: Option<String>,
    /// The profile the model, effort and reply length are saved for (the chat's owner). Without
    /// it they are saved for the profile acting now.
    pub profile_id: Option<String>,
}

/// `runtime_settings_get` arguments: the profile whose new-chat defaults to read.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetArgs {
    profile_id: Option<String>,
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

/// The profile a call names, checked: empty is none, and one too long or with control characters is
/// refused.
fn named_profile(named: Option<&str>) -> Result<Option<String>, ApiError> {
    match named.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) if p.len() > 64 || p.chars().any(char::is_control) => {
            Err(ApiError::bad_args("That profile is not valid."))
        }
        other => Ok(other.map(str::to_owned)),
    }
}

/// The profile whose defaults a call is about: the one it names, else the one acting now. `None`
/// before profiles exist, when the install-wide defaults are all there is.
fn defaults_profile(conn: &rusqlite::Connection, named: Option<&str>) -> Option<String> {
    named
        .map(str::to_owned)
        .or_else(|| helm_core::prefs::acting_profile(conn))
}

fn read_settings(ctx: &OpCtx, named: Option<&str>) -> Result<RuntimeSettings, ApiError> {
    let named = named_profile(named)?;
    let profile = ctx.services.need::<Profile>()?;
    let db = ctx.services.need::<Database>()?;
    let (rt, problem) = ClaudeRuntime::load(&profile.dir);
    let ((model, effort), response_verbosity, permission_mode) = db
        .snapshot(|tx| {
            let who = defaults_profile(tx, named.as_deref());
            Ok::<_, rusqlite::Error>((
                new_chat_defaults_for(tx, who.as_deref()),
                verbosity_for(tx, who.as_deref()),
                default_permission_mode(tx),
            ))
        })
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
        shell_changes: rt.shell_changes,
        low_priority: rt.low_priority,
        model,
        effort,
        response_verbosity,
        permission_mode,
        problem,
    })
}

fn runtime_settings_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: GetArgs = crate::ops::parse_args(args)?;
    serde_json::to_value(read_settings(ctx, a.profile_id.as_deref())?)
        .map_err(|e| ApiError::internal(e.to_string()))
}

fn runtime_settings_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let p: RuntimeSettingsPatch = crate::ops::parse_args(args)?;
    let named = named_profile(p.profile_id.as_deref())?;
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
        || p.shell_changes.is_some()
        || p.low_priority.is_some()
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
        if let Some(v) = p.shell_changes {
            rt.shell_changes = v;
        }
        if let Some(v) = p.low_priority {
            rt.low_priority = v;
        }
        rt.save(&profile.dir)
            .map_err(|e| ApiError::internal(format!("claude-runtime.json not saved: {e}")))?;
    }
    if p.model.is_some() || p.effort.is_some() || p.response_verbosity.is_some() {
        db.write(|tx| {
            let pref_err = |e: helm_core::prefs::PrefError| match e {
                helm_core::prefs::PrefError::Sql(e) => e,
                other => rusqlite::Error::InvalidParameterName(other.to_string()),
            };
            // A profile's own choice: the profile named, else the one acting now. Before any
            // profile exists, the install-wide preferences hold it.
            let who = defaults_profile(tx, named.as_deref());
            if p.model.is_some() || p.effort.is_some() {
                let (model, effort) = new_chat_defaults_for(tx, who.as_deref());
                let model = match p.model.as_deref().map(str::trim) {
                    Some("") => None,
                    Some(m) => Some(m.to_owned()),
                    None => model,
                };
                let effort = p.effort.clone().unwrap_or(effort);
                if let Some(who) = who.as_deref() {
                    helm_core::prefs::set_profile_defaults(
                        tx,
                        who,
                        &[("model", json!(model)), ("effort", json!(effort))],
                    )
                    .map_err(pref_err)?;
                } else {
                    // Merge: other fields of the preference (harness, provider) are kept.
                    let mut stored = helm_core::prefs::read(tx, NEW_CHAT)
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}));
                    stored["model"] = json!(model);
                    stored["effort"] = json!(effort);
                    helm_core::prefs::set_raw(tx, NEW_CHAT, &stored).map_err(pref_err)?;
                }
            }
            if let Some(v) = p.response_verbosity.as_deref() {
                let saved = match who.as_deref() {
                    Some(who) => helm_core::prefs::set_profile_defaults(
                        tx,
                        who,
                        &[("responseVerbosity", json!(v))],
                    ),
                    None => helm_core::prefs::set_raw(tx, VERBOSITY, &json!(v)),
                };
                saved.map_err(pref_err)?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .map_err(sql)?;
    }
    let s = read_settings(ctx, named.as_deref())?;
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
        assert_eq!(s["fastMode"], false, "fast mode defaults off");
        assert_eq!(
            s["worktreeTools"], false,
            "Claude Code's worktree tools default off"
        );
        assert_eq!(
            s["scheduleTools"], false,
            "Claude Code's schedule tools default off"
        );
        assert_eq!(
            s["shellChanges"], true,
            "shell-command change capture defaults on"
        );
        assert_eq!(
            s["lowPriority"], true,
            "agents run at lower priority by default"
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
            json!({"fastMode": false, "worktreeTools": true, "scheduleTools": true, "shellChanges": false, "lowPriority": false, "skipPermissions": false, "model": "sonnet", "effort": "high", "responseVerbosity": "medium"}),
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
        assert_eq!(file["shellChanges"], false);
        assert_eq!(file["lowPriority"], false);
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
        // The chip's pick lives in a preference of its own; one an earlier version kept inside
        // `newChat` is ignored.
        assert_eq!(s["permissionMode"], Value::Null);
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
                &json!({"harness": "claude", "model": null, "effort": "high",
                        "permissionMode": "bypassPermissions"}),
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
            json!({"harness": "claude", "model": null, "effort": "low",
                   "permissionMode": "bypassPermissions"})
        );
        let s = api("runtime_settings_get", json!({})).await.unwrap();
        assert_eq!(s["permissionMode"], Value::Null, "a stale pick is ignored");

        // New-chat defaults are kept per profile; the install-wide ones are what a profile with
        // none of its own falls back to, and stay as they were.
        let install =
            h.db.snapshot(|tx| Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, NEW_CHAT)))
                .unwrap();
        let s = api(
            "runtime_settings_save",
            json!({"profileId": "p-ann", "model": "opus", "effort": "max", "responseVerbosity": "high"}),
        )
        .await
        .unwrap();
        assert_eq!(
            (&s["model"], &s["effort"], &s["responseVerbosity"]),
            (&json!("opus"), &json!("max"), &json!("high"))
        );
        let s = api("runtime_settings_get", json!({"profileId": "p-bo"}))
            .await
            .unwrap();
        assert_eq!(
            (&s["model"], &s["effort"], &s["responseVerbosity"]),
            (&Value::Null, &json!("low"), &json!("medium")),
            "another profile follows the install-wide defaults"
        );
        let set_active = |id: &'static str| {
            h.db.write(|tx| {
                helm_core::prefs::set_raw(tx, "activeProfile", &json!(id))
                    .map_err(|_| rusqlite::Error::InvalidQuery)
            })
            .unwrap()
        };
        // A call that names no profile is for the one acting now, and so is a new chat.
        set_active("p-ann");
        let s = api("runtime_settings_get", json!({})).await.unwrap();
        assert_eq!((&s["model"], &s["effort"]), (&json!("opus"), &json!("max")));
        let c = api("chat_create", json!({"cwd": h.work.to_string_lossy()}))
            .await
            .unwrap();
        assert_eq!((&c["model"], &c["effort"]), (&json!("opus"), &json!("max")));
        set_active("p-bo");
        let c = api("chat_create", json!({"cwd": h.work.to_string_lossy()}))
            .await
            .unwrap();
        assert_eq!((&c["model"], &c["effort"]), (&Value::Null, &json!("low")));
        // A chat made for another profile starts on that profile's defaults.
        let c = api(
            "chat_create",
            json!({"cwd": h.work.to_string_lossy(), "profileId": "p-ann"}),
        )
        .await
        .unwrap();
        assert_eq!((&c["model"], &c["effort"]), (&json!("opus"), &json!("max")));
        // A save with no profile named is the acting profile's: only its entry changes, and an
        // untouched field keeps what that profile was following.
        let s = api("runtime_settings_save", json!({"effort": "high"}))
            .await
            .unwrap();
        assert_eq!((&s["model"], &s["effort"]), (&Value::Null, &json!("high")));
        let s = api("runtime_settings_get", json!({"profileId": "p-ann"}))
            .await
            .unwrap();
        assert_eq!((&s["model"], &s["effort"]), (&json!("opus"), &json!("max")));
        let after =
            h.db.snapshot(|tx| Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, NEW_CHAT)))
                .unwrap();
        assert_eq!(
            after, install,
            "the install-wide default was never rewritten"
        );
        let bad = api(
            "runtime_settings_save",
            json!({"profileId": "x".repeat(65), "effort": "low"}),
        )
        .await
        .unwrap_err();
        assert_eq!(bad.code, "bad_args");
        // The permission-mode default stays install-wide.
        assert_eq!(s["permissionMode"], Value::Null);
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
