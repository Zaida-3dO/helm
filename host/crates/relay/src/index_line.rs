//! The hub's search index over the line.
//!
//! [`IndexLine`] is the production [`IndexHub`]: a request goes up as a frame with a fresh id and
//! the call waits (on the indexer's own thread, never an async one) for the frame that carries the
//! same id back. Nothing is queued for a later line: with the line down a request fails at once,
//! and the indexer starts again from what the hub says it holds when the next line comes up.
//!
//! The hub also asks this PC two things, and [`IndexLine`] answers them through the
//! [`Responder`] the indexer installs: `ix_embed` (embed a query, or say the card is reserved)
//! and `ix_trace` (search one chat's work trace).
//!
//! The hub says in its welcome which features it has; one that does not list `index` gets no
//! index frames, and the status says the hub needs updating.

use crate::client::{IndexSink, LineHandle, LineState};
use helm_memindex::hub::{Embedded, HubStatus, IndexHub, Responder};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a request may wait for the hub's answer.
const TIMEOUT: Duration = Duration::from_secs(60);

pub struct IndexLine {
    line: Arc<LineHandle>,
    pending: Mutex<HashMap<String, SyncSender<Value>>>,
    epoch: AtomicU64,
    has_index: AtomicBool,
    listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    responder: Mutex<Option<Arc<dyn Responder>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl IndexLine {
    pub fn new(line: Arc<LineHandle>) -> Arc<Self> {
        Arc::new(IndexLine {
            line,
            pending: Mutex::new(HashMap::new()),
            epoch: AtomicU64::new(0),
            has_index: AtomicBool::new(false),
            listeners: Mutex::new(Vec::new()),
            responder: Mutex::new(None),
        })
    }

    fn changed(&self) {
        let fs: Vec<_> = lock(&self.listeners).clone();
        for f in fs {
            f();
        }
    }

    /// Answer a question from the hub on its own thread: the indexer's work must not wait for it,
    /// and the line's reader must not wait for the model.
    fn answer(self: &Arc<Self>, frame: Value) {
        let Some(responder) = lock(&self.responder).clone() else {
            return;
        };
        let me = self.clone();
        std::thread::spawn(move || {
            let id = frame.get("id").cloned().unwrap_or(Value::Null);
            let reply = match frame.get("type").and_then(Value::as_str) {
                Some("ix_embed") => {
                    let text = frame.get("text").and_then(Value::as_str).unwrap_or("");
                    match responder.embed(text) {
                        Embedded::Vector(v, model) => {
                            json!({"type": "ix_embedded", "id": id, "vec": v, "model": model})
                        }
                        Embedded::Locked => {
                            json!({"type": "ix_embedded", "id": id, "locked": true})
                        }
                        Embedded::Failed => {
                            json!({"type": "ix_embedded", "id": id, "error": "Embedding failed."})
                        }
                    }
                }
                Some("ix_trace") => {
                    let mut body = responder.trace(&frame);
                    body["type"] = json!("ix_trace_result");
                    body["id"] = id;
                    body
                }
                _ => return,
            };
            me.line.device_frame(reply);
        });
    }
}

/// The frames that answer a request (the rest are the hub's own questions).
fn is_answer(kind: &str) -> bool {
    matches!(
        kind,
        "ix_state"
            | "ix_manifest_page"
            | "ix_put_result"
            | "ix_delete_result"
            | "ix_pending_list"
            | "ix_vectors_result"
    )
}

/// The sink the client holds is the `Arc`; answering a question needs the `Arc` again.
pub struct IndexSinkHandle(pub Arc<IndexLine>);

impl IndexSink for IndexSinkHandle {
    fn frame(&self, frame: Value) {
        let kind = frame.get("type").and_then(Value::as_str).unwrap_or("");
        if is_answer(kind) {
            let id = frame.get("id").and_then(Value::as_str).unwrap_or("");
            if let Some(tx) = lock(&self.0.pending).remove(id) {
                let _ = tx.try_send(frame);
            }
        } else {
            self.0.answer(frame);
        }
    }

    fn welcome(&self, features: Vec<String>) {
        self.0
            .has_index
            .store(features.iter().any(|f| f == "index"), Ordering::SeqCst);
        self.0.changed();
    }

    fn line_up(&self) {
        self.0.has_index.store(false, Ordering::SeqCst);
        self.0.epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn line_down(&self) {
        // Dropping the senders fails every request still waiting.
        lock(&self.0.pending).clear();
        self.0.has_index.store(false, Ordering::SeqCst);
        self.0.changed();
    }
}

impl IndexHub for IndexLine {
    fn ask(&self, mut frame: Value) -> Result<Value, String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        frame["id"] = json!(id);
        let (tx, rx) = sync_channel(1);
        lock(&self.pending).insert(id.clone(), tx);
        if !self.line.device_frame(frame) {
            lock(&self.pending).remove(&id);
            return Err("Helm is not connected to the hub.".into());
        }
        match rx.recv_timeout(TIMEOUT) {
            Ok(answer) => Ok(answer),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.pending).remove(&id);
                Err("The hub did not answer in time.".into())
            }
            Err(_) => Err("The hub line dropped before the answer came.".into()),
        }
    }

    fn status(&self) -> HubStatus {
        if self.line.state() != LineState::Online {
            HubStatus::Down
        } else if self.has_index.load(Ordering::SeqCst) {
            HubStatus::Ready
        } else {
            HubStatus::TooOld
        }
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    fn on_change(&self, f: Arc<dyn Fn() + Send + Sync>) {
        lock(&self.listeners).push(f);
    }

    fn set_responder(&self, r: Arc<dyn Responder>) {
        *lock(&self.responder) = Some(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;
    impl Responder for Fixed {
        fn embed(&self, text: &str) -> Embedded {
            match text {
                "locked" => Embedded::Locked,
                _ => Embedded::Vector(vec![1.0, 2.0], "m".into()),
            }
        }
        fn trace(&self, frame: &Value) -> Value {
            json!({"ok": true, "results": [frame["query"]]})
        }
    }

    #[test]
    fn answers_go_to_their_request_and_the_hubs_questions_to_the_responder() {
        let (handle, mut up) = LineHandle::detached_for_tests();
        let line = IndexLine::new(handle);
        let sink = IndexSinkHandle(line.clone());
        assert_eq!(line.status(), HubStatus::Down, "no line, nothing sent");
        assert!(line.ask(json!({"type": "ix_hello"})).is_err());

        handle_online(&line);
        sink.welcome(vec![]);
        assert_eq!(line.status(), HubStatus::TooOld);
        sink.welcome(vec!["index".into()]);
        assert_eq!(line.status(), HubStatus::Ready);

        // A request waits for the frame with its id.
        let l2 = line.clone();
        let asker = std::thread::spawn(move || l2.ask(json!({"type": "ix_hello", "sources": []})));
        let sent = loop {
            if let Ok((_, v)) = up.try_recv() {
                break v;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(sent["type"], "ix_hello");
        sink.frame(json!({"type": "ix_state", "id": sent["id"], "ok": true}));
        assert_eq!(asker.join().unwrap().unwrap()["ok"], true);

        // The hub's questions are answered by the responder, up the same line.
        line.set_responder(Arc::new(Fixed));
        sink.frame(json!({"type": "ix_embed", "id": "e1", "text": "locked"}));
        sink.frame(json!({"type": "ix_trace", "id": "t1", "query": "q"}));
        let mut got = Vec::new();
        for _ in 0..200 {
            while let Ok((_, v)) = up.try_recv() {
                got.push(v);
            }
            if got.len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        got.sort_by_key(|v| v["id"].as_str().unwrap_or("").to_owned());
        assert_eq!(got[0]["locked"], true);
        assert_eq!(got[1]["type"], "ix_trace_result");
        assert_eq!(got[1]["results"], json!(["q"]));
    }

    fn handle_online(line: &Arc<IndexLine>) {
        line.line.set_online_for_tests();
    }
}
