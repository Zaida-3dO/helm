//! The reminder sweep. A timer in `lib.rs` calls [`SavedService::sweep`] every few seconds; each
//! due reminder is marked and reported exactly once, and the UI decides what to do with it (the
//! notifications area shows a notice).

use crate::store;
use helm_core::db::Database;
use helm_ipc::{AppEvent, EventSink};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct SavedService {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
}

impl SavedService {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>) -> Self {
        Self { db, events }
    }

    /// Mark every reminder due by `now` and emit them. A host hiccup is left for the next tick.
    pub fn sweep(&self, now: i64) {
        let Ok(due) = self.db.write(|tx| store::take_due(tx, now)) else {
            return;
        };
        if due.is_empty() {
            return;
        }
        let items: Vec<Value> = due
            .iter()
            .filter_map(|i| serde_json::to_value(i).ok())
            .collect();
        self.events
            .emit(AppEvent::new("saved_due").with("items", json!(items)));
    }
}
