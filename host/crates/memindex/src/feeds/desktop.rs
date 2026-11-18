//! What the Claude desktop app knows about the sessions it started in its Code tab.
//!
//! The conversation of such a session is an ordinary Claude Code record file; the desktop app
//! keeps only a small file of its own per session (`local_<id>.json`) that names the record file
//! (`cliSessionId`) and holds the title the app shows. Only those two fields are read: the rest of
//! the file is the app's own settings and is never looked at. Whatever cannot be read is simply
//! absent, so this only ever adds a better title.

use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// A session file bigger than this is not read.
const MAX_FILE_BYTES: u64 = 512 * 1024;
/// The most files read in one look.
const MAX_FILES: usize = 20_000;

/// Record file id to the title the desktop app shows for it.
pub fn titles(root: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut seen = 0usize;
    // `<root>/<account>/<organisation>/local_<id>.json`.
    for account in sub_dirs(root) {
        for org in sub_dirs(&account) {
            let Ok(rd) = std::fs::read_dir(&org) else {
                continue;
            };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !(name.starts_with("local_") && name.ends_with(".json")) {
                    continue;
                }
                seen += 1;
                if seen > MAX_FILES {
                    return out;
                }
                let Ok(meta) = e.metadata() else { continue };
                if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(e.path()) else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let id = v.get("cliSessionId").and_then(Value::as_str);
                let title = v.get("title").and_then(Value::as_str).map(str::trim);
                if let (Some(id), Some(title)) = (id, title) {
                    if !id.is_empty() && !title.is_empty() {
                        out.insert(id.to_owned(), title.to_owned());
                    }
                }
            }
        }
    }
    out
}

fn sub_dirs(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}
