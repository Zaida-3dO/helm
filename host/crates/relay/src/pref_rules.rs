//! Rules for the preference keys this crate owns, applied whenever a setting changes through the
//! settings tools or an approved staged change: what the generic writer cannot know about the
//! key's value.
//!
//! - `updateSource` must name an existing folder (an absolute path or a share, not a link), and
//!   Helm looks in it at once when it changes.
//! - `householdSharing` keeps a switch only while the one it depends on is on.

use crate::household::Sharing;
use helm_hosttools::PrefRule;
use helm_ipc::Services;
use rusqlite::Transaction;
use serde_json::Value;
use std::sync::Arc;

struct UpdateSource(Arc<helm_updates::UpdateService>);

impl PrefRule for UpdateSource {
    fn key(&self) -> &'static str {
        "updateSource"
    }

    fn prepare(
        &self,
        _tx: &Transaction<'_>,
        _before: Option<&Value>,
        value: &Value,
    ) -> Result<Value, String> {
        let bad = || {
            "The update folder must be an existing folder (an absolute path or a share, not a link), or null for none."
                .to_owned()
        };
        let text = value.as_str().ok_or_else(bad)?;
        let folder = helm_updates::checked_source(Some(text)).map_err(|_| bad())?;
        match folder {
            Some(f) => {
                crate::setup::plain_path(&f, "The update folder")?;
                Ok(Value::String(f))
            }
            None => Ok(Value::Null),
        }
    }

    fn after_commit(&self, _after: Option<&Value>) {
        self.0.check();
    }
}

struct HouseholdSharing;

impl PrefRule for HouseholdSharing {
    fn key(&self) -> &'static str {
        "householdSharing"
    }

    fn prepare(
        &self,
        _tx: &Transaction<'_>,
        _before: Option<&Value>,
        value: &Value,
    ) -> Result<Value, String> {
        let s: Sharing = serde_json::from_value(value.clone())
            .map_err(|_| "householdSharing is {chats, transcripts, messages}.".to_owned())?;
        serde_json::to_value(s.normalised()).map_err(|e| e.to_string())
    }
}

/// This crate's rules, for the services the app builds.
pub fn pref_rules(services: &Services) -> Vec<Arc<dyn PrefRule>> {
    let mut rules: Vec<Arc<dyn PrefRule>> = vec![Arc::new(HouseholdSharing)];
    if let Some(svc) = services.get::<helm_updates::UpdateService>() {
        rules.push(Arc::new(UpdateSource(svc)));
    }
    rules
}
