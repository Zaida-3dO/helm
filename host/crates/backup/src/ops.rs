//! The four ops.

use crate::types::*;
use crate::{archive, export, restore, user_documents, BackupError};
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::Value;
use std::path::PathBuf;

/// Where backups go unless the caller names a folder, under Documents.
pub const DEFAULT_BACKUP_FOLDER: &str = "Helm Backups";
/// Where exported chats go unless the caller names a folder, under Documents.
pub const DEFAULT_EXPORT_FOLDER: &str = "Helm Exports";

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("backup_create", OpClass::Heavy, create_op)
        .blocking("backup_list", OpClass::Blocking, list_op)
        .blocking("restore_prepare", OpClass::Maintenance, restore_op)
        .blocking("chat_export_markdown", OpClass::Blocking, export_op);
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn parse_args<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    // The UI sends `{}`; a bare null (no arguments at all) means the same.
    let v = if v.is_null() {
        Value::Object(Default::default())
    } else {
        v
    };
    serde_json::from_value(v).map_err(|e| ApiError::bad_args(e.to_string()))
}

/// The folder asked for, or `<Documents>/<fallback>`.
fn folder(asked: Option<&str>, fallback: &str) -> Result<PathBuf, BackupError> {
    match asked.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Ok(PathBuf::from(s)),
        None => Ok(user_documents().ok_or(BackupError::NoDocuments)?.join(fallback)),
    }
}

fn create_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CreateArgs = parse_args(args)?;
    let profile = ctx.services.need::<Profile>()?;
    let root = folder(a.dest_dir.as_deref(), DEFAULT_BACKUP_FOLDER)?;
    std::fs::create_dir_all(&root).map_err(BackupError::from)?;
    let made = archive::create(
        &profile.db_path(),
        &profile.dir,
        &root,
        env!("CARGO_PKG_VERSION"),
        helm_core::now_ms(),
    )?;
    to_json(&Created {
        archive_dir: made.archive_dir.to_string_lossy().into_owned(),
        entries: made.entries,
        db_bytes: made.db_bytes,
    })
}

fn list_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ListArgs = parse_args(args)?;
    let profile = ctx.services.need::<Profile>()?;
    let dir = folder(a.dir.as_deref(), DEFAULT_BACKUP_FOLDER)?;
    to_json(&BackupList {
        default_dir: dir.to_string_lossy().into_owned(),
        backups: archive::list(&dir)?,
        pending: restore::pending_archive(&profile.dir),
    })
}

fn restore_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: RestoreArgs = parse_args(args)?;
    let profile = ctx.services.need::<Profile>()?;
    restore::prepare(&profile.dir, &PathBuf::from(&a.archive_dir), &a.confirm)?;
    to_json(&RestoreStaged {
        archive_dir: a.archive_dir,
        restart_required: true,
    })
}

fn export_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ExportArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let profile = ctx.services.need::<Profile>()?;
    let dest = folder(a.dest_dir.as_deref(), DEFAULT_EXPORT_FOLDER)?;
    let out = db
        .snapshot(|tx| {
            export::export_markdown(
                tx,
                &profile.path("artifacts"),
                &a.chat_id,
                &dest,
                helm_core::now_ms(),
            )
        })
        .map_err(ApiError::from)?;
    to_json(&out)
}
