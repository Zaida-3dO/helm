//! The one event channel: signal-then-fetch.
//!
//! Every event is an object with a `kind` field plus that kind's fields. Most kinds are signals:
//! the UI re-reads through `api`. The app emits them on the Tauri event named [`EVENT_CHANNEL`],
//! to the main window only.

use serde::Serialize;
use serde_json::{Map, Value};
use std::sync::Mutex;

/// Tauri event name carrying every [`AppEvent`].
pub const EVENT_CHANNEL: &str = "app-event";

#[derive(Clone, Debug, PartialEq)]
pub struct AppEvent {
    pub kind: String,
    pub fields: Map<String, Value>,
}

impl AppEvent {
    pub fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            fields: Map::new(),
        }
    }

    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        assert_ne!(key, "kind", "kind is the tag");
        self.fields.insert(key.to_owned(), value.into());
        self
    }

    /// The wire shape: `{"kind": ..., ...fields}`.
    pub fn to_value(&self) -> Value {
        let mut m = self.fields.clone();
        m.insert("kind".into(), Value::String(self.kind.clone()));
        Value::Object(m)
    }
}

impl Serialize for AppEvent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: AppEvent);
}

/// Drops everything (headless tools).
pub struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _event: AppEvent) {}
}

/// Keeps everything (tests).
#[derive(Default)]
pub struct RecordingSink {
    pub events: Mutex<Vec<AppEvent>>,
}
impl EventSink for RecordingSink {
    fn emit(&self, event: AppEvent) {
        self.events.lock().unwrap().push(event);
    }
}
impl RecordingSink {
    pub fn kinds(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.kind.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape_is_tagged_by_kind() {
        let e = AppEvent::new("refresh")
            .with("chatId", "c1")
            .with("revision", 3);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"kind": "refresh", "chatId": "c1", "revision": 3})
        );
    }
}
