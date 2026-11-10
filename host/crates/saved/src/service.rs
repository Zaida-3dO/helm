//! The reminder sweep. A timer in `lib.rs` calls [`SavedService::sweep`] every few seconds; each
//! due reminder is reported once per run of Helm as it comes due, and the UI decides what to do
//! with it (the notifications area shows one notice for what is due). Nothing is marked in the
//! table: a reminder that came due while no window was listening (Helm closed, a reload) is
//! asked for by the window when it starts ([`SavedService::due_now`]) and stays on the Saved
//! pane until the user deals with it.

use crate::store;
use helm_core::db::Database;
use helm_ipc::{AppEvent, EventSink};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub struct SavedService {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    /// The reminders already announced in this run, so a sweep says each one only once.
    announced: Mutex<HashSet<String>>,
}

impl SavedService {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>) -> Self {
        Self {
            db,
            events,
            announced: Mutex::new(HashSet::new()),
        }
    }

    /// Every open item whose reminder is due, announced or not: what a window that has just
    /// started needs to hear about.
    pub fn due_now(&self, now: i64) -> Vec<Value> {
        let due = self
            .db
            .snapshot(|tx| store::due(tx, now))
            .unwrap_or_default();
        due.iter()
            .filter_map(|i| serde_json::to_value(i).ok())
            .collect()
    }

    /// Delete for good the items removed more than a week ago. A host hiccup is left for the
    /// next pass.
    pub fn purge(&self, now: i64) {
        let _ = self.db.write(|tx| store::purge(tx, now));
    }

    /// Announce the reminders that have come due since the last sweep. A host hiccup is left for
    /// the next tick.
    pub fn sweep(&self, now: i64) {
        let Ok(due) = self.db.snapshot(|tx| store::due(tx, now)) else {
            return;
        };
        let fresh: Vec<_> = {
            let mut seen = self.announced.lock().unwrap_or_else(|p| p.into_inner());
            // One that is no longer due (cleared, moved, finished) may be announced again later.
            seen.retain(|id| due.iter().any(|i| &i.id == id));
            due.into_iter()
                .filter(|i| seen.insert(i.id.clone()))
                .collect()
        };
        if fresh.is_empty() {
            return;
        }
        let items: Vec<Value> = fresh
            .iter()
            .filter_map(|i| serde_json::to_value(i).ok())
            .collect();
        self.events
            .emit(AppEvent::new("saved_due").with("items", json!(items)));
    }
}
