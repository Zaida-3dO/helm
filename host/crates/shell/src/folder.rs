//! `folder_pick`: the native "choose a folder" dialog behind the new-chat page's "Choose another
//! folder…", Add project and Primary folder….
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `folder_pick` | blocking | [`FolderPickArgs`] → [`FolderPickResult`] (`path: null` when cancelled) |
//!
//! The dialog runs on a thread of its own: it needs a single-threaded COM apartment, and a pooled
//! thread may already have joined the multi-threaded one. The op class is `Blocking`, never
//! `Maintenance`, so an open dialog does not hold the maintenance lock. Tests swap the dialog for a
//! [`FolderPicker`] service.

use helm_ipc::{ApiError, OpCtx};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ts_rs::TS;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FolderPickArgs {
    /// The dialog's title; a default is used when missing or blank.
    #[serde(default)]
    #[ts(optional)]
    pub title: Option<String>,
    /// The folder the dialog opens in, when it exists.
    #[serde(default)]
    #[ts(optional)]
    pub start_in: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FolderPickResult {
    /// The chosen folder, absolute; `null` when the user cancelled.
    pub path: Option<String>,
}

/// What the dialog is asked for, after defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickRequest {
    pub title: String,
    pub start_in: Option<PathBuf>,
}

/// Shows the dialog: `Ok(None)` when cancelled, `Err` when no dialog could be shown.
pub type PickFn = dyn Fn(&PickRequest) -> Result<Option<PathBuf>, String> + Send + Sync;

/// A replacement for the native dialog (tests). Without one the op shows the real dialog.
#[derive(Clone)]
pub struct FolderPicker(pub Arc<PickFn>);

pub const DEFAULT_TITLE: &str = "Choose a folder";
const MAX_TITLE: usize = 200;

/// Defaults and checks: a blank title gets the default one, an over-long one is cut, and a start
/// folder that is not an existing absolute directory is dropped rather than refused.
pub fn request(a: &FolderPickArgs) -> PickRequest {
    let title = a
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.chars()
                .filter(|c| !c.is_control())
                .take(MAX_TITLE)
                .collect()
        })
        .unwrap_or_else(|| DEFAULT_TITLE.to_owned());
    let start_in = a
        .start_in
        .as_deref()
        .map(str::trim)
        .map(Path::new)
        .filter(|p| p.is_absolute() && p.is_dir())
        .map(Path::to_path_buf);
    PickRequest { title, start_in }
}

pub(crate) fn folder_pick(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let args = if args.is_null() {
        Value::Object(Default::default())
    } else {
        args
    };
    let a: FolderPickArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let req = request(&a);
    let picked = match ctx.services.get::<FolderPicker>() {
        Some(p) => (p.0)(&req),
        None => native(&req),
    }
    .map_err(|e| ApiError::new("unavailable", e))?;
    let path = match picked {
        None => None,
        Some(p) if p.is_absolute() && p.is_dir() => Some(p.to_string_lossy().into_owned()),
        Some(_) => return Err(ApiError::bad_args("That is not a folder.")),
    };
    serde_json::to_value(FolderPickResult { path }).map_err(|e| ApiError::internal(e.to_string()))
}

#[cfg(windows)]
fn native(req: &PickRequest) -> Result<Option<PathBuf>, String> {
    let req = req.clone();
    std::thread::Builder::new()
        .name("helm-folder-pick".into())
        .spawn(move || {
            let mut d = rfd::FileDialog::new().set_title(&req.title);
            if let Some(dir) = &req.start_in {
                d = d.set_directory(dir);
            }
            d.pick_folder()
        })
        .map_err(|e| format!("Could not open the folder dialog: {e}"))?
        .join()
        .map_err(|_| "The folder dialog failed.".to_owned())
}

#[cfg(not(windows))]
fn native(_req: &PickRequest) -> Result<Option<PathBuf>, String> {
    Err("The folder dialog is only available on Windows; type the path instead.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_ipc::{NullSink, Services};
    use std::sync::Mutex;

    fn ctx_with(picker: FolderPicker) -> OpCtx {
        let mut services = Services::new();
        services.insert(Arc::new(picker));
        OpCtx {
            services: Arc::new(services),
            events: Arc::new(NullSink),
        }
    }

    #[test]
    fn a_blank_title_gets_the_default_and_a_missing_start_folder_is_dropped() {
        let r = request(&FolderPickArgs {
            title: Some("   ".into()),
            start_in: Some("Z:/surely/not/here/at/all".into()),
        });
        assert_eq!(r.title, DEFAULT_TITLE);
        assert_eq!(r.start_in, None);
        let dir = tempfile::tempdir().unwrap();
        let r = request(&FolderPickArgs {
            title: Some(" Add project \u{7} ".into()),
            start_in: Some(dir.path().to_string_lossy().into_owned()),
        });
        assert_eq!(r.title, "Add project ");
        assert_eq!(r.start_in.as_deref(), Some(dir.path()));
        // A relative start folder is never handed to the dialog.
        let r = request(&FolderPickArgs {
            title: None,
            start_in: Some(".".into()),
        });
        assert_eq!(r.start_in, None);
    }

    #[test]
    fn the_op_returns_the_chosen_folder_or_null_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let chosen = dir.path().to_path_buf();
        let seen = Arc::new(Mutex::new(Vec::<PickRequest>::new()));
        let log = seen.clone();
        let ctx = ctx_with(FolderPicker(Arc::new(move |r: &PickRequest| {
            log.lock().unwrap().push(r.clone());
            Ok(Some(chosen.clone()))
        })));
        let v = folder_pick(&ctx, serde_json::json!({ "title": "Primary folder" })).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "path": dir.path().to_string_lossy() })
        );
        assert_eq!(seen.lock().unwrap()[0].title, "Primary folder");

        let ctx = ctx_with(FolderPicker(Arc::new(|_: &PickRequest| Ok(None))));
        assert_eq!(
            folder_pick(&ctx, Value::Null).unwrap(),
            serde_json::json!({ "path": null })
        );
    }

    #[test]
    fn a_picked_file_unknown_args_and_a_failed_dialog_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let ctx = ctx_with(FolderPicker(Arc::new(move |_: &PickRequest| {
            Ok(Some(file.clone()))
        })));
        assert_eq!(folder_pick(&ctx, Value::Null).unwrap_err().code, "bad_args");
        assert_eq!(
            folder_pick(&ctx, serde_json::json!({ "multiple": true }))
                .unwrap_err()
                .code,
            "bad_args"
        );
        let ctx = ctx_with(FolderPicker(Arc::new(|_: &PickRequest| {
            Err("no desktop".into())
        })));
        let e = folder_pick(&ctx, Value::Null).unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.message.as_str()),
            ("unavailable", "no desktop")
        );
    }
}
