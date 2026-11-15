//! Rules for the preference keys this crate owns, applied whenever a setting changes through the
//! settings tools or an approved staged change: what the generic writer cannot know about the
//! key's value.
//!
//! - `updateSource` must name an existing folder (an absolute path or a share, not a link), and
//!   Helm looks in it at once when it changes.
//! - `householdSharing` keeps a switch only while the one it depends on is on, and once it is
//!   saved the live line serves (and tells the hub) the new switches at once.

use crate::client::Client;
use crate::household::Sharing;
use helm_hosttools::PrefRule;
use helm_ipc::{AppEvent, EventSink, Services};
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

struct HouseholdSharing {
    /// Puts the saved switches in force on the live line.
    live: Arc<dyn Fn(Sharing) + Send + Sync>,
}

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

    /// An approved or undone change must reach the line that serves the household: it keeps its
    /// own copy of the switches. No value means the default, which is everything off.
    fn after_commit(&self, after: Option<&Value>) {
        let s: Sharing = after
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        (self.live)(s.normalised());
    }
}

/// This crate's rules, for the services the app builds.
pub fn pref_rules(services: &Services, events: &Arc<dyn EventSink>) -> Vec<Arc<dyn PrefRule>> {
    let client = services.get::<Client>();
    let events = events.clone();
    let live: Arc<dyn Fn(Sharing) + Send + Sync> = Arc::new(move |s| {
        if let Some(c) = &client {
            c.set_sharing(s);
        }
        events.emit(AppEvent::new("household_changed"));
    });
    let mut rules: Vec<Arc<dyn PrefRule>> = vec![Arc::new(HouseholdSharing { live })];
    if let Some(svc) = services.get::<helm_updates::UpdateService>() {
        rules.push(Arc::new(UpdateSource(svc)));
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn a_saved_household_change_reaches_the_live_line() {
        let seen: Arc<Mutex<Vec<Sharing>>> = Arc::default();
        let log = seen.clone();
        let rule = HouseholdSharing {
            live: Arc::new(move |s| log.lock().unwrap().push(s)),
        };
        rule.after_commit(Some(&json!({"chats": true, "transcripts": true})));
        // Undo to the default, and a value that no longer reads: everything goes off.
        rule.after_commit(None);
        rule.after_commit(Some(&json!("junk")));
        let seen = seen.lock().unwrap();
        assert_eq!(
            *seen,
            vec![
                Sharing {
                    chats: true,
                    transcripts: true,
                    messages: false
                },
                Sharing::default(),
                Sharing::default()
            ]
        );
    }
}
