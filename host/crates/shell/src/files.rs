//! `files_pick`: the native "choose files" dialog behind the composer's Attach files
//! button. It sits beside the [`crate::folder`] dialog and works the same
//! way: its own thread for the COM apartment, the `Blocking` class, and a [`FilesPicker`] service
//! in place of the dialog for tests.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `files_pick` | blocking | `{title?}` → `{paths: string[]}` (empty when cancelled) |

use helm_ipc::{ApiError, OpCtx};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub const DEFAULT_TITLE: &str = "Attach files";
const MAX_TITLE: usize = 200;
/// The composer's per-message limit; a longer pick is cut, not refused.
pub const MAX_FILES: usize = 20;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesPickArgs {
    #[serde(default)]
    title: Option<String>,
}

/// Shows the dialog: `Ok(vec![])` when cancelled, `Err` when no dialog could be shown.
pub type PickFilesFn = dyn Fn(&str) -> Result<Vec<PathBuf>, String> + Send + Sync;

/// A replacement for the native dialog (tests). Without one the op shows the real dialog.
#[derive(Clone)]
pub struct FilesPicker(pub Arc<PickFilesFn>);

pub(crate) fn files_pick(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let args = if args.is_null() {
        Value::Object(Default::default())
    } else {
        args
    };
    let a: FilesPickArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let title: String = a
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
    let picked = match ctx.services.get::<FilesPicker>() {
        Some(p) => (p.0)(&title),
        None => native(&title),
    }
    .map_err(|e| ApiError::new("unavailable", e))?;
    let paths: Vec<String> = picked
        .into_iter()
        .filter(|p| p.is_absolute() && p.is_file())
        .take(MAX_FILES)
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    Ok(json!({ "paths": paths }))
}

#[cfg(windows)]
fn native(title: &str) -> Result<Vec<PathBuf>, String> {
    let title = title.to_owned();
    std::thread::Builder::new()
        .name("helm-files-pick".into())
        .spawn(move || {
            rfd::FileDialog::new()
                .set_title(&title)
                .pick_files()
                .unwrap_or_default()
        })
        .map_err(|e| format!("Could not open the file dialog: {e}"))?
        .join()
        .map_err(|_| "The file dialog failed.".to_owned())
}

#[cfg(not(windows))]
fn native(_title: &str) -> Result<Vec<PathBuf>, String> {
    Err(
        "The file dialog is only available on Windows; drop the files on the composer instead."
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_ipc::{NullSink, Services};
    use std::sync::Mutex;

    fn ctx_with(picker: FilesPicker) -> OpCtx {
        let mut services = Services::new();
        services.insert(Arc::new(picker));
        OpCtx {
            services: Arc::new(services),
            events: Arc::new(NullSink),
        }
    }

    #[test]
    fn the_op_returns_the_chosen_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.png");
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "x").unwrap();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        let (pa, pb, folder) = (a.clone(), b.clone(), dir.path().to_path_buf());
        let ctx = ctx_with(FilesPicker(Arc::new(move |t: &str| {
            log.lock().unwrap().push(t.to_owned());
            // A folder or a relative path is never handed back.
            Ok(vec![
                pa.clone(),
                folder.clone(),
                PathBuf::from("rel.txt"),
                pb.clone(),
            ])
        })));
        let v = files_pick(&ctx, Value::Null).unwrap();
        assert_eq!(
            v,
            json!({ "paths": [a.to_string_lossy(), b.to_string_lossy()] })
        );
        assert_eq!(seen.lock().unwrap()[0], DEFAULT_TITLE);
    }

    #[test]
    fn cancel_is_empty_and_a_failed_dialog_or_unknown_args_are_refused() {
        let ctx = ctx_with(FilesPicker(Arc::new(|_: &str| Ok(vec![]))));
        assert_eq!(
            files_pick(&ctx, Value::Null).unwrap(),
            json!({ "paths": [] })
        );
        assert_eq!(
            files_pick(&ctx, json!({ "multiple": true }))
                .unwrap_err()
                .code,
            "bad_args"
        );
        let ctx = ctx_with(FilesPicker(Arc::new(|_: &str| Err("no desktop".into()))));
        assert_eq!(
            files_pick(&ctx, Value::Null).unwrap_err().code,
            "unavailable"
        );
    }
}
