//! The ops behind Settings > Skills. They only ever touch the profile's own skills folder; the
//! picked source is read, never modified, and errors name what went wrong in plain words.

use crate::{self as skills, agents, examples, SkillEntry};
use helm_core::profile::Profile;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

pub const EVENT: &str = "skills-changed";
pub const AGENTS_EVENT: &str = "agents-changed";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddArgs {
    source: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveArgs {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevealArgs {
    /// One skill's folder; the skills root itself when absent.
    #[serde(default)]
    name: Option<String>,
}

fn profile(ctx: &OpCtx) -> Result<std::path::PathBuf, ApiError> {
    ctx.services
        .need::<Profile>()
        .map(|p| p.dir.clone())
        .map_err(|_| ApiError::internal("Helm has no profile folder."))
}

fn changed(ctx: &OpCtx) {
    ctx.events.emit(AppEvent::new(EVENT));
}

fn to_entry_value(list: Vec<SkillEntry>) -> Value {
    json!({ "skills": serde_json::to_value(list).unwrap_or(Value::Null) })
}

fn list(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let p = profile(ctx)?;
    examples::seed(&p);
    Ok(json!({
        "skills": skills::list(&p),
        "folder": skills::skills_dir(&p).to_string_lossy(),
        "loadedIntoChats": skills::launch_plugin_dir(&p).is_some(),
    }))
}

fn add(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: AddArgs = serde_json::from_value(v)
        .map_err(|_| ApiError::bad_args("Expected a source folder path."))?;
    let source = a.source.trim().to_owned();
    if source.is_empty() {
        return Err(ApiError::bad_args("Pick a skill folder first."));
    }
    let p = profile(ctx)?;
    let added = skills::add(&p, Path::new(&source)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            ApiError::bad_args(format!("That folder cannot be used: {e}"))
        }
        std::io::ErrorKind::AlreadyExists => ApiError::bad_args(e.to_string()),
        std::io::ErrorKind::InvalidInput => ApiError::bad_args(e.to_string()),
        _ => ApiError::internal("The skill folder could not be copied."),
    })?;
    changed(ctx);
    let mut body = to_entry_value(skills::list(&p));
    body.as_object_mut()
        .unwrap()
        .insert("added".into(), json!(added.name));
    Ok(body)
}

fn remove(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: RemoveArgs =
        serde_json::from_value(v).map_err(|_| ApiError::bad_args("Expected a skill name."))?;
    let p = profile(ctx)?;
    skills::remove(&p, &a.name).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            ApiError::bad_args("No skill of that name is in the folder.")
        }
        std::io::ErrorKind::InvalidInput => ApiError::bad_args("That is not a skill name."),
        _ => ApiError::internal("The skill could not be removed."),
    })?;
    changed(ctx);
    Ok(to_entry_value(skills::list(&p)))
}

fn reveal(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: RevealArgs = serde_json::from_value(v)
        .map_err(|_| ApiError::bad_args("Expected an optional skill name."))?;
    let p = profile(ctx)?;
    let path = match a.name {
        Some(n) if !n.trim().is_empty() => {
            if !skills::is_valid_name(n.trim()) {
                return Err(ApiError::bad_args("That is not a skill name."));
            }
            skills::skills_dir(&p).join(n.trim())
        }
        _ => skills::skills_dir(&p),
    };
    if !path.is_dir() {
        return Err(ApiError::bad_args("That folder is not here."));
    }
    reveal_folder(&path).map_err(|_| ApiError::internal("Explorer could not be started."))?;
    Ok(json!({"revealed": true}))
}

fn agents_changed(ctx: &OpCtx) {
    ctx.events.emit(AppEvent::new(AGENTS_EVENT));
}

fn agents_list(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let p = profile(ctx)?;
    examples::seed(&p);
    Ok(json!({
        "agents": agents::list(&p),
        "folder": agents::agents_dir(&p).to_string_lossy(),
    }))
}

fn agents_add(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: AddArgs = serde_json::from_value(v)
        .map_err(|_| ApiError::bad_args("Expected a source file path."))?;
    let source = a.source.trim().to_owned();
    if source.is_empty() {
        return Err(ApiError::bad_args("Pick an agent file first."));
    }
    let p = profile(ctx)?;
    let added = agents::add(&p, Path::new(&source)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound
        | std::io::ErrorKind::AlreadyExists
        | std::io::ErrorKind::InvalidInput => ApiError::bad_args(format!("{e}")),
        _ => ApiError::internal("The agent file could not be copied."),
    })?;
    agents_changed(ctx);
    Ok(json!({ "agents": agents::list(&p), "added": added.names, "skipped": added.skipped }))
}

fn agents_remove(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: RemoveArgs =
        serde_json::from_value(v).map_err(|_| ApiError::bad_args("Expected an agent name."))?;
    let p = profile(ctx)?;
    agents::remove(&p, &a.name).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            ApiError::bad_args("No agent of that name is in the folder.")
        }
        std::io::ErrorKind::InvalidInput => ApiError::bad_args("That is not an agent name."),
        _ => ApiError::internal("The agent could not be removed."),
    })?;
    agents_changed(ctx);
    Ok(json!({ "agents": agents::list(&p) }))
}

fn agents_reveal(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let p = profile(ctx)?;
    let path = agents::agents_dir(&p);
    if !path.is_dir() {
        return Err(ApiError::bad_args("That folder is not here."));
    }
    reveal_folder(&path).map_err(|_| ApiError::internal("Explorer could not be started."))?;
    Ok(json!({"revealed": true}))
}

/// Open a folder in Explorer (Windows) or the file browser elsewhere.
#[cfg(windows)]
fn reveal_folder(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let mut cmd = std::process::Command::new(std::path::PathBuf::from(windir).join("explorer.exe"));
    cmd.arg(path).creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn reveal_folder(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("not supported here"))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("helm_skills_list", OpClass::Blocking, list)
        .blocking("helm_skills_add", OpClass::Maintenance, add)
        .blocking("helm_skills_remove", OpClass::Maintenance, remove)
        .blocking("helm_skills_reveal", OpClass::Blocking, reveal)
        .blocking("helm_agents_list", OpClass::Blocking, agents_list)
        .blocking("helm_agents_add", OpClass::Maintenance, agents_add)
        .blocking("helm_agents_remove", OpClass::Maintenance, agents_remove)
        .blocking("helm_agents_reveal", OpClass::Blocking, agents_reveal);
}
