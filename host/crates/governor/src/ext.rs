//! The usage governor as a key of `app_get_settings` and `app_update_settings`: `usageGovernor`.
//!
//! A chat reads it and changes it itself, with the Undo notice. The value is the Settings page's
//! own: `{enabled, selectivePct, windDownPct, stopPct, atStop}`; a value names only the fields
//! to change, and the merged result goes through the page's strict check.

use crate::bands::{self, GovernorSettings};
use crate::service::Governor;
use crate::store;
use helm_hosttools::{SettingsExtension, StagedPart};
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

pub const KEY: &str = "usageGovernor";

struct GovernorKey(Arc<Governor>);

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn settings_extension(governor: Arc<Governor>) -> Arc<dyn SettingsExtension> {
    Arc::new(GovernorKey(governor))
}

fn as_json(s: &GovernorSettings) -> Value {
    serde_json::to_value(s).expect("settings serialise")
}

fn sql(e: rusqlite::Error) -> String {
    format!("The governor settings could not be read or saved: {e}")
}

/// Check `value` (a patch to the stored settings, or null for the defaults) and store it; answers
/// the settings before and after.
fn put(tx: &Transaction<'_>, value: &Value) -> Result<(Value, Value), String> {
    let before = as_json(&store::load(tx).map_err(sql)?);
    let wanted = if value.is_null() {
        as_json(&GovernorSettings::default())
    } else if let Some(patch) = value.as_object() {
        let mut merged = before.clone();
        if let Some(m) = merged.as_object_mut() {
            m.extend(patch.clone());
        }
        merged
    } else {
        return Err(format!(
            "{KEY} is an object with only the fields to change, or null."
        ));
    };
    let after = bands::parse_strict(&wanted)?;
    store::save(tx, &after).map_err(sql)?;
    Ok((before, as_json(&after)))
}

impl SettingsExtension for GovernorKey {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        &[]
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({
                "type": "object",
                "merge": true,
                "default": as_json(&GovernorSettings::default()),
                "about": "Settings > Governor: near the plan's usage limit, Helm tells running chats to wrap up and then stops them. enabled turns it on; selectivePct, windDownPct and stopPct (whole percents, 1 to 100, rising) are where each band begins; atStop is \"stop\" (stop the chats) or \"wrapUp\" (only tell them). Send only the fields to change.",
            })
        })
    }

    fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
        as_json(&store::load(tx).unwrap_or_default())
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        put(tx, value)
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let s = bands::parse_strict(value)?;
        store::save(tx, &s).map_err(sql)
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("governor")
    }

    fn after_commit(&self, _key: &str) {
        // The new thresholds act at once, not at the next pass.
        self.0.tick(helm_core::now_ms());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    /// The settings go through the same strict check as the Settings page, a patch merges, and
    /// Undo puts the old value back. (The service is not needed to check or store.)
    #[test]
    fn a_staged_governor_change_is_merged_checked_and_restored() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        reg.initializer("governor", store::init);
        let db = Database::open(&dir.path().join("t.db"), &reg).unwrap();
        let go = |patch: Value| {
            db.write(|tx| {
                let r = put(tx, &patch);
                Ok::<_, rusqlite::Error>(r)
            })
            .unwrap()
        };
        let (before, after) = go(json!({"enabled": true, "stopPct": 98})).unwrap();
        assert_eq!(before["stopPct"], 95);
        assert_eq!(
            (after["enabled"].clone(), after["stopPct"].clone()),
            (json!(true), json!(98))
        );
        assert_eq!(after["windDownPct"], 85, "the others keep their values");
        for (patch, part) in [
            (json!({"stopPct": 50}), "must rise"),
            (json!({"atStop": "halt"}), "atStop"),
            (json!({"loud": true}), "no setting called"),
            (json!({"enabled": "yes"}), "true or false"),
            (json!("on"), "only the fields to change"),
        ] {
            let why = go(patch).unwrap_err();
            assert!(why.contains(part), "{why}");
        }
        let (_, reset) = go(Value::Null).unwrap();
        assert_eq!(reset, as_json(&GovernorSettings::default()));
        db.write(|tx| {
            let s = bands::parse_strict(&after).unwrap();
            store::save(tx, &s)
        })
        .unwrap();
        let now = db.snapshot(store::load).unwrap();
        assert_eq!((now.enabled, now.stop_pct), (true, 98));
    }
}
