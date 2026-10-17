//! The single preference writer.
//!
//! These two functions are the only code that holds SQL inserting, updating or deleting
//! preference rows. `tests::only_this_file_writes_preferences` scans every non-test source file in
//! the host to enforce it. Both refuse an unclassified key: a panic in debug and test builds, an
//! error in release.

use super::classes::classify;
use super::PrefError;
use rusqlite::Transaction;
use serde_json::Value;

fn guard(key: &str) -> Result<(), PrefError> {
    if classify(key).is_some() {
        return Ok(());
    }
    if cfg!(debug_assertions) {
        panic!("write to unclassified preference key {key:?}");
    }
    Err(PrefError::Unclassified(key.to_owned()))
}

/// Insert or replace one preference row.
pub(crate) fn put_row(tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), PrefError> {
    guard(key)?;
    tx.execute(
        "INSERT INTO preferences(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value.to_string()),
    )?;
    Ok(())
}

/// Delete one preference row (a missing row reads as the default).
pub(crate) fn delete_row(tx: &Transaction<'_>, key: &str) -> Result<(), PrefError> {
    guard(key)?;
    tx.execute("DELETE FROM preferences WHERE key = ?1", [key])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// Normalised (lowercase, single-spaced) phrases that write preference rows.
    fn write_phrases() -> Vec<String> {
        let table = "preferences";
        [
            "insert into ",
            "insert or replace into ",
            "replace into ",
            "update ",
            "delete from ",
        ]
        .iter()
        .map(|verb| format!("{verb}{table}"))
        .collect()
    }

    /// Lowercase, whitespace collapsed, with everything from the first `#[cfg(test)]` dropped.
    fn non_test_text(src: &str) -> String {
        let cut = src.find("#[cfg(test)]").map_or(src, |i| &src[..i]);
        cut.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(crate) fn writes_preferences(src: &str) -> bool {
        let text = non_test_text(src);
        write_phrases().iter().any(|p| text.contains(p.as_str()))
    }

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "target" && name != "tests" && name != "gen" {
                    rust_files(&path, out);
                }
            } else if name.ends_with(".rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn the_scanner_catches_a_second_writer() {
        assert!(writes_preferences(
            "fn f() { db.execute(\"UPDATE  Preferences SET x\") }"
        ));
        assert!(writes_preferences(
            "const Q: &str = \"delete\n from preferences\";"
        ));
        assert!(!writes_preferences(
            "fn f() { \"SELECT value FROM preferences\" }"
        ));
        assert!(!writes_preferences(
            "#[cfg(test)] mod t { \"DELETE FROM preferences\" }"
        ));
    }

    #[test]
    fn only_this_file_writes_preferences() {
        let host = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let mut files = Vec::new();
        rust_files(&host, &mut files);
        assert!(files.len() > 10, "scan found too few files; wrong root?");
        let me = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("prefs")
            .join("store.rs");
        let me = std::fs::canonicalize(me).unwrap();
        let mut writers = Vec::new();
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            if writes_preferences(&src) {
                writers.push(std::fs::canonicalize(&f).unwrap());
            }
        }
        assert_eq!(
            writers,
            vec![me],
            "only prefs/store.rs may write preference rows"
        );
    }
}
