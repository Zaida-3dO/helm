//! The side panel's `api` ops (panel crate rows).
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `file_read` / `dir_list` | heavy | `{chatId?, path}` → [`FileView`](crate::viewer::FileView) |
//! | `default_handler` | blocking | `{path}` → `{handler: {name, appPath} \| null, refusal: {step, reason} \| null}` |
//! | `file_open_default` | maintenance | `{path}` → `null`: re-runs every check, then opens |
//! | `file_reveal` | maintenance | `{path}` → `null`: Explorer with the item selected |
//! | `chat_folder` | blocking | `{chatId}` → `{path \| null}`: the chat's project folder |
//! | `panel_tabs_get` / `panel_tabs_save` | blocking / maintenance | `{}` → `panelTabs` v1 / `{value}` → `null` |
//!
//! Arguments are parsed strictly. Refusals come back as `{code: "refused", message}`; the message
//! is written for the user. Every path passes the lexical path gate before any filesystem call.

use crate::handler::{self, Decision};
use crate::private::PrivateRoots;
use crate::viewer::{self, ReadError};
use crate::{pathgate, tabs};
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use rusqlite::OptionalExtension;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("file_read", OpClass::Heavy, file_read)
        .blocking("dir_list", OpClass::Heavy, file_read)
        .blocking("default_handler", OpClass::Blocking, default_handler)
        .blocking("file_open_default", OpClass::Maintenance, file_open_default)
        .blocking("file_reveal", OpClass::Maintenance, file_reveal)
        .blocking("chat_folder", OpClass::Blocking, chat_folder)
        .blocking("panel_tabs_get", OpClass::Blocking, panel_tabs_get)
        .blocking("panel_tabs_save", OpClass::Maintenance, panel_tabs_save);
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, ApiError> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

pub fn refused(message: impl Into<String>) -> ApiError {
    ApiError::new("refused", message)
}

fn read_error(e: ReadError) -> ApiError {
    match e {
        ReadError::NotFound => ApiError::new("not_found", e.message()),
        ReadError::Io(m) => ApiError::internal(m),
        other => refused(other.message()),
    }
}

/// The profile folder (data, sessions, backups, caches) and the default one, whichever is in use.
pub fn private_roots(ctx: &OpCtx) -> PrivateRoots {
    let mut roots = Vec::new();
    if let Some(p) = ctx.services.get::<Profile>() {
        roots.push(p.dir.clone());
    }
    if let Ok(d) = helm_core::profile::default_profile_dir() {
        roots.push(d);
    }
    PrivateRoots::new(&roots)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PathArgs {
    #[serde(default)]
    #[allow(dead_code)]
    chat_id: Option<String>,
    path: String,
}

fn file_read(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PathArgs = parse(args)?;
    let view = viewer::read(&a.path, &private_roots(ctx)).map_err(read_error)?;
    serde_json::to_value(view).map_err(|e| ApiError::internal(e.to_string()))
}

fn default_handler(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PathArgs = parse(args)?;
    let trusted = handler::trusted_roots();
    Ok(
        match handler::check(&a.path, &private_roots(ctx), &trusted, &handler::os_lookup) {
            Ok(Decision::Folder(_)) => {
                json!({"handler": {"name": "File Explorer", "appPath": ""}, "refusal": null})
            }
            Ok(Decision::File(_, h)) => json!({"handler": h, "refusal": null}),
            Err(r) => json!({"handler": null, "refusal": r}),
        },
    )
}

fn file_open_default(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PathArgs = parse(args)?;
    let trusted = handler::trusted_roots();
    let decision = handler::check(&a.path, &private_roots(ctx), &trusted, &handler::os_lookup)
        .map_err(|r| refused(r.reason().to_owned()))?;
    handler::open(&decision).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Value::Null)
}

fn file_reveal(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PathArgs = parse(args)?;
    let path = viewer::admit(&a.path, &private_roots(ctx)).map_err(read_error)?;
    if std::fs::symlink_metadata(&path).is_err() {
        return Err(ApiError::new("not_found", "The file was not found."));
    }
    handler::reveal(&path).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Value::Null)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ChatArgs {
    chat_id: String,
}

fn chat_folder(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let path: Option<String> = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT p.path FROM chats c JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
                [&a.chat_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
        })
        .map_err(|e| ApiError::internal(e.to_string()))?
        .flatten();
    // A folder the gate would refuse is reported as none, so the UI never lists it.
    let path = path.filter(|p| pathgate::check(p).is_ok());
    Ok(json!({ "path": path }))
}

fn panel_tabs_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    db.snapshot(tabs::load)
        .map_err(|e| ApiError::internal(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    value: Value,
}

fn panel_tabs_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SaveArgs = parse(args)?;
    let sets = tabs::validate(&a.value).map_err(ApiError::bad_args)?;
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| tabs::save(tx, &sets))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Value::Null)
}
