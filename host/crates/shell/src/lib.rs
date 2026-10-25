//! The Windows shell around Helm.
//!
//! | op | class | does |
//! |---|---|---|
//! | `shell_settings_get` | blocking | → [`ShellSettingsView`] |
//! | `shell_settings_save` | maintenance | [`ShellSettings`] → [`ShellSettingsView`] (validated; applied at once) |
//! | `audio_inputs` | blocking | → [`AudioInput`]`[]` (active capture devices, plus a gone stored choice) |
//! | `open_external` | maintenance | `{url}`: http, https and mailto only, re-checked here |
//! | `folder_pick` | blocking | the native folder dialog; see [`folder`] |
//! | `files_pick` | blocking | the native multi-file dialog; see [`files`] |
//! | `window_zoom` | maintenance | `{level}` 50-200 in steps of 10: zoom the web view itself |
//! | `ui_error_report` | maintenance | `{kind, message, stack?, source?}`: one line in the renderer error log, see [`errlog`] |
//!
//! The app registers a [`ShellService`] (`register_services`) and taps every event through
//! [`ShellService::observe`]. The tray, hotkey, badge and toasts are drawn by the app
//! (`host/app/src/tray.rs`) through [`ShellHost`].

pub mod bindings;
pub mod errlog;
pub mod files;
pub mod folder;
pub mod notify;
pub mod probe;
pub mod service;
pub mod settings;
pub mod tray;
pub mod types;
pub mod uiwatch;

pub use service::{ShellHost, ShellService};
pub use types::*;

use helm_core::db::Database;
use helm_ipc::{ApiError, EventSink, OpClass, OpCtx, OpRegistry, Services};
use helm_platform::Platform;
use serde_json::Value;
use std::sync::Arc;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("shell_settings_get", OpClass::Blocking, settings_get)
        .blocking("shell_settings_save", OpClass::Maintenance, settings_save)
        .blocking("audio_inputs", OpClass::Blocking, audio_inputs)
        .blocking("open_external", OpClass::Maintenance, open_external)
        .blocking("folder_pick", OpClass::Blocking, folder::folder_pick)
        .blocking("files_pick", OpClass::Blocking, files::files_pick)
        .blocking("window_zoom", OpClass::Maintenance, window_zoom)
        .blocking("ui_error_report", OpClass::Maintenance, ui_error_report);
}

/// Adds the [`ShellService`]. Needs the database and platform already registered.
pub fn register_services(services: &mut Services, _events: &Arc<dyn EventSink>) {
    let (Some(db), Some(platform)) = (services.get::<Database>(), services.get::<Platform>())
    else {
        log::error!("helm-shell: database or platform service missing; shell features are off");
        return;
    };
    let svc = ShellService::new(
        db,
        platform.power.clone(),
        platform.audio.clone(),
        probe::Probe::from_env(cfg!(debug_assertions)),
    );
    services.insert(Arc::new(svc));
    services.insert(Arc::new(helm_chats::FileSaver(Arc::new(
        files::save_native,
    ))));
}

fn settings_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let svc = ctx.services.need::<ShellService>()?;
    to_value(&svc.view())
}

fn settings_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let s: ShellSettings = serde_json::from_value(args)
        .map_err(|e| ApiError::bad_args(format!("shell settings: {e}")))?;
    let svc = ctx.services.need::<ShellService>()?;
    let view = svc.save(s).map_err(ApiError::bad_args)?;
    to_value(&view)
}

/// The page reports an error it hit; the line goes to the diagnostics folder, rate limited.
fn ui_error_report(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<helm_core::profile::Profile>()?;
    let field = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default();
    let outcome = errlog::global()
        .record(
            &profile.path("diagnostics"),
            helm_core::now_ms(),
            field("kind"),
            field("message"),
            field("stack"),
            field("source"),
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(serde_json::json!({ "logged": outcome == errlog::Outcome::Written }))
}

fn window_zoom(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        level: i64,
    }
    let a: A = serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    ctx.services
        .need::<ShellService>()?
        .set_zoom(a.level)
        .map_err(ApiError::bad_args)?;
    Ok(Value::Null)
}

fn audio_inputs(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let svc = ctx.services.need::<ShellService>()?;
    to_value(&svc.audio_inputs())
}

/// The external-link allowlist: only http, https and mailto, checked on the
/// parsed scheme so "C:javascript:" or "file:" never reach the OS.
pub fn external_url_allowed(url: &str) -> bool {
    if url.len() > 8192 || url.chars().any(|c| c.is_control()) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest.starts_with("//") && rest.len() > 2,
        "mailto" => !rest.is_empty(),
        _ => false,
    }
}

fn open_external(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: OpenExternalArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    if !external_url_allowed(&a.url) {
        return Err(ApiError::bad_args(
            "only http, https and mailto links open outside Helm",
        ));
    }
    let platform = ctx.services.need::<Platform>()?;
    platform
        .external
        .open_url(&a.url)
        .map_err(ApiError::internal)?;
    Ok(Value::Null)
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_https_and_mailto_open_externally() {
        for ok in [
            "https://example.com/x",
            "http://localhost:3000",
            "HTTPS://A.B",
            "mailto:me@example.com",
        ] {
            assert!(external_url_allowed(ok), "{ok}");
        }
        for bad in [
            "file:///C:/x",
            "C:javascript:alert(1)",
            "javascript:alert(1)",
            "obsidian://open?vault=v",
            "https:",
            "https:/x",
            "mailto:",
            "ms-settings:privacy",
            "https://a.b/\nx",
            "relative/path",
        ] {
            assert!(!external_url_allowed(bad), "{bad}");
        }
    }
}
