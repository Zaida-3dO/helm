//! Save a tool's whole output to a file.
//!
//! A tool result longer than the row's text limit keeps its full text in an artifact file
//! ([`crate::session_log`]); the card says so. `message_output_save {chatId, messageId}` asks where
//! to put it, through a [`FileSaver`], and writes the whole text there. It answers `{path}`, or
//! `{path: null}` when the dialog is cancelled. The dialog is a service so that tests can answer
//! it; the app registers the native one.

use crate::ops::{not_found, sql};
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

/// Shows a save dialog with a suggested file name: the chosen path, `Ok(None)` when cancelled.
pub type SaveFn = dyn Fn(&str) -> Result<Option<PathBuf>, String> + Send + Sync;

/// The save dialog, as a service.
#[derive(Clone)]
pub struct FileSaver(pub Arc<SaveFn>);

pub fn register(reg: &mut OpRegistry) {
    reg.blocking(
        "message_output_save",
        OpClass::Blocking,
        message_output_save,
    );
}

/// `bash-output.txt`: the tool's name, cleaned for a file name.
fn suggested_name(tool: Option<&str>) -> String {
    let clean: String = tool
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    if clean.is_empty() {
        "output.txt".to_owned()
    } else {
        format!("{}-output.txt", clean.to_ascii_lowercase())
    }
}

fn message_output_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let chat_id = args
        .get("chatId")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_args("chatId is required."))?
        .to_owned();
    let message_id = args
        .get("messageId")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::bad_args("messageId is required."))?;
    let db = ctx.services.need::<Database>()?;
    let profile = ctx.services.need::<Profile>()?;
    let full = db
        .snapshot(|t| {
            crate::session_log::full_text(t, &profile.path("artifacts"), &chat_id, message_id)
        })
        .map_err(sql)?
        .ok_or_else(not_found)?;
    if full.role != "toolResult" {
        return Err(ApiError::bad_args("Only a tool's output can be saved."));
    }
    let saver = ctx
        .services
        .get::<FileSaver>()
        .ok_or_else(|| ApiError::new("unavailable", "Saving is not available here."))?;
    let chosen = (saver.0)(&suggested_name(full.tool.as_deref()))
        .map_err(|e| ApiError::new("unavailable", e))?;
    let Some(path) = chosen else {
        return Ok(json!({ "path": null }));
    };
    std::fs::write(&path, full.text.as_bytes())
        .map_err(|e| ApiError::new("internal", format!("Could not save the file: {e}")))?;
    Ok(json!({ "path": path.to_string_lossy() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suggested_name_comes_from_the_tool() {
        assert_eq!(suggested_name(Some("Bash")), "bash-output.txt");
        assert_eq!(suggested_name(Some("mcp__a/b c")), "mcp__abc-output.txt");
        assert_eq!(suggested_name(None), "output.txt");
        assert_eq!(suggested_name(Some("../..")), "output.txt");
    }
}
