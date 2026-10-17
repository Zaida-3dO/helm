//! Preferences.
//!
//! `preferences` maps a key to a JSON value. Writes are strict: the key must be classified, and the
//! generic writer accepts only [`GENERIC_ALLOWLIST`], validating each value. Reads are lenient: a
//! missing row or one whose JSON does not parse reads as `None`, which every reader maps to its
//! default.

pub mod classes;
mod store;

pub use classes::{classify, PrefClass, PrefKey, Tier};

use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum PrefError {
    #[error("unknown preference")]
    UnknownPreference,
    #[error("unclassified preference key {0:?}")]
    Unclassified(String),
    #[error("invalid value for {key}: {why}")]
    Invalid {
        key: &'static str,
        why: &'static str,
    },
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

pub const SIDEBAR_VISIBLE: PrefKey = PrefKey::classified("sidebarVisible");
pub const INSPECTOR_VISIBLE: PrefKey = PrefKey::classified("inspectorVisible");
pub const SIDEBAR_WIDTH: PrefKey = PrefKey::classified("sidebarWidth");
pub const INSPECTOR_WIDTH: PrefKey = PrefKey::classified("inspectorWidth");
pub const SIDEBAR_VIEW: PrefKey = PrefKey::classified("sidebarView");
pub const TERMINAL_HEIGHT: PrefKey = PrefKey::classified("terminalHeight");
pub const THEME: PrefKey = PrefKey::classified("theme");
pub const COMPACT_SIDEBAR: PrefKey = PrefKey::classified("compactSidebar");

/// Keys the generic `preference` op accepts. Every other key has its own feature operation.
/// The list starts with the layout keys and grows it
/// as features land.
pub const GENERIC_ALLOWLIST: &[PrefKey] = &[
    SIDEBAR_VISIBLE,
    INSPECTOR_VISIBLE,
    SIDEBAR_WIDTH,
    INSPECTOR_WIDTH,
    SIDEBAR_VIEW,
    TERMINAL_HEIGHT,
    THEME,
    COMPACT_SIDEBAR,
];

/// The stored value, or `None` when the row is missing or its JSON does not parse.
pub fn read(conn: &Connection, key: &str) -> Option<Value> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM preferences WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
        .ok()
        .flatten();
    raw.and_then(|s| serde_json::from_str(&s).ok())
}

/// The stored value as `T`, or `default` when missing, unparseable or the wrong shape.
pub fn get_or<T: DeserializeOwned>(conn: &Connection, key: PrefKey, default: T) -> T {
    read(conn, key.name())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or(default)
}

/// Generic validation; other keys are validated by their owning feature.
pub fn validate(key: PrefKey, value: &Value) -> Result<(), PrefError> {
    let k = key.name();
    let int_in = |lo: i64, hi: i64, why: &'static str| match value.as_i64() {
        Some(n) if (lo..=hi).contains(&n) => Ok(()),
        _ => Err(PrefError::Invalid { key: k, why }),
    };
    match k {
        "sidebarView" => match value.as_str() {
            Some("projects" | "activity") => Ok(()),
            _ => Err(PrefError::Invalid {
                key: k,
                why: "must be projects or activity",
            }),
        },
        "sidebarVisible" | "inspectorVisible" | "compactSidebar" => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err(PrefError::Invalid {
                    key: k,
                    why: "must be true or false",
                })
            }
        }
        "sidebarWidth" => int_in(200, 4096, "must be 200 to 4096"),
        "inspectorWidth" => int_in(280, 4096, "must be 280 to 4096"),
        "terminalHeight" => int_in(80, 4096, "must be 80 to 4096"),
        _ => Ok(()),
    }
}

/// Write a known key through the single writer. `Value::Null` deletes the row.
pub fn set(tx: &Transaction<'_>, key: PrefKey, value: &Value) -> Result<(), PrefError> {
    if value.is_null() {
        return store::delete_row(tx, key.name());
    }
    validate(key, value)?;
    store::put_row(tx, key.name(), value)
}

/// The generic writer behind the `preference` op: allowlisted keys only.
pub fn set_generic(tx: &Transaction<'_>, key: &str, value: &Value) -> Result<PrefKey, PrefError> {
    let key = GENERIC_ALLOWLIST
        .iter()
        .copied()
        .find(|k| k.name() == key)
        .ok_or(PrefError::UnknownPreference)?;
    set(tx, key, value)?;
    Ok(key)
}

/// Write a dynamic or otherwise runtime-named key. Unclassified keys are refused by the writer
/// (a panic in debug builds).
pub fn set_raw(tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), PrefError> {
    if value.is_null() {
        store::delete_row(tx, key)
    } else {
        store::put_row(tx, key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{tests::temp_db, MigrationRegistry};
    use serde_json::json;

    #[test]
    fn round_trip_and_delete() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        db.write(|tx| set(tx, SIDEBAR_WIDTH, &json!(320))).unwrap();
        let w: i64 = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(get_or(tx, SIDEBAR_WIDTH, 0)))
            .unwrap();
        assert_eq!(w, 320);
        db.write(|tx| set(tx, SIDEBAR_WIDTH, &Value::Null)).unwrap();
        let w: i64 = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(get_or(tx, SIDEBAR_WIDTH, 250)))
            .unwrap();
        assert_eq!(w, 250);
    }

    #[test]
    fn bad_json_reads_as_the_default() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        // Plant a corrupt row directly (simulating an older or damaged profile).
        db.write(|tx| {
            tx.execute(
                "INSERT INTO preferences(key, value) VALUES ('sidebarWidth', '{not json')",
                [],
            )
        })
        .unwrap();
        let w: i64 = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(get_or(tx, SIDEBAR_WIDTH, 250)))
            .unwrap();
        assert_eq!(w, 250);
        db.write(|tx| set(tx, SIDEBAR_WIDTH, &json!(300))).unwrap();
        // A readable row of the wrong shape also falls back.
        let wrong_shape: bool = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(get_or(tx, SIDEBAR_WIDTH, false)))
            .unwrap();
        assert!(!wrong_shape);
    }

    #[test]
    fn generic_writer_refuses_unknown_keys_and_bad_values() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let r = db.write(|tx| set_generic(tx, "agentSettingsAll", &json!(true)));
        assert!(matches!(r, Err(PrefError::UnknownPreference)));
        let r = db.write(|tx| set_generic(tx, "sidebarWidth", &json!(10)));
        assert!(matches!(r, Err(PrefError::Invalid { .. })));
        let r = db.write(|tx| set_generic(tx, "sidebarView", &json!("tree")));
        assert!(matches!(r, Err(PrefError::Invalid { .. })));
        assert!(db
            .write(|tx| set_generic(tx, "sidebarView", &json!("activity")))
            .is_ok());
    }

    #[test]
    #[should_panic(expected = "unclassified preference key")]
    fn writing_an_unclassified_key_panics_in_debug() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let _ = db.write(|tx| set_raw(tx, "neverClassified", &json!(1)));
    }

    #[test]
    fn dynamic_keys_are_writable() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        db.write(|tx| set_raw(tx, "autocompleteBudget:chat-1", &json!([1, 2])))
            .unwrap();
        let v = db.snapshot(|tx| Ok::<_, rusqlite::Error>(read(tx, "autocompleteBudget:chat-1")));
        assert_eq!(v.unwrap(), Some(json!([1, 2])));
    }
}
