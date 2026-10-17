//! The MCP client's worker lines, through the host-tools worker-line registry:
//!
//! - `mcp_config {revision, servers}` goes to each worker when it starts (a start line), and to
//!   every running worker again after each config save (the [`Broadcaster`]). Secrets are
//!   resolved here, at send time, and travel only in this line.
//! - `mcp_status` from a worker updates the [`McpStatusStore`] and signals `mcp_status` to the UI.

use crate::config::McpStore;
use crate::status::{McpStatusStore, ServerStatus};
use helm_hosttools::lines::{Broadcaster, WorkerLines};
use helm_ipc::{AppEvent, Services};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, Weak};

/// The `mcp_config` line for the current config, or `None` when it cannot be read.
pub fn config_line(store: &McpStore) -> Option<Value> {
    let (revision, servers) = store.resolve_enabled(&|k| std::env::var(k).ok()).ok()?;
    Some(json!({ "type": "mcp_config", "revision": revision, "servers": servers }))
}

/// Remembers the app's services the first time a worker starts, so a config save (which happens
/// in an op, not on a worker line) can reach the [`Broadcaster`].
#[derive(Default)]
pub struct ServicesCell(Mutex<Option<Weak<Services>>>);

impl ServicesCell {
    fn set(&self, s: &Arc<Services>) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::downgrade(s));
    }
    fn get(&self) -> Option<Arc<Services>> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(Weak::upgrade)
    }

    /// Sends the current config to every running worker.
    pub fn broadcast(&self) {
        let Some(s) = self.get() else { return };
        let (Some(store), Some(b)) = (s.get::<McpStore>(), s.get::<Broadcaster>()) else {
            return;
        };
        if let Some(line) = config_line(&store) {
            b.0.broadcast_to_workers(&line);
        }
    }
}

pub fn register_worker_lines(l: &mut WorkerLines) {
    l.register_worker_line("mcp_status", |ctx, v| {
        let Ok(st) = serde_json::from_value::<ServerStatus>(v.clone()) else {
            return;
        };
        if let Some(store) = ctx.services.get::<McpStatusStore>() {
            if store.update(st.clone()) {
                ctx.events.emit(
                    AppEvent::new("mcp_status")
                        .with("serverId", st.server_id)
                        .with("state", serde_json::to_value(st.state).unwrap_or_default()),
                );
            }
        }
    });
    l.register_start_line(|_chat, services| {
        if let Some(cell) = services.get::<ServicesCell>() {
            cell.set(services);
        }
        config_line(&*services.get::<McpStore>()?)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::{hub, store};
    use helm_hosttools::lines::{WorkerBroadcast, WorkerLineCtx};
    use helm_ipc::{EventSink, RecordingSink};

    #[derive(Default)]
    struct Sent(Mutex<Vec<Value>>);
    impl WorkerBroadcast for Sent {
        fn broadcast_to_workers(&self, line: &Value) {
            self.0.lock().unwrap().push(line.clone());
        }
        fn send_to_worker(&self, _: &str, _: &Value) -> bool {
            false
        }
    }

    #[test]
    fn start_line_status_line_and_broadcast() {
        let d = tempfile::tempdir().unwrap();
        let st = Arc::new(store(d.path()));
        st.mutate(None, |l| {
            l.push(hub("tok-123456"));
            Ok(())
        })
        .unwrap();
        let sent = Arc::new(Sent::default());
        let cell = Arc::new(ServicesCell::default());
        let mut services = Services::new();
        services.insert(st.clone());
        services.insert(Arc::new(McpStatusStore::default()));
        services.insert(Arc::new(Broadcaster(sent.clone())));
        services.insert(cell.clone());
        let services = Arc::new(services);

        let mut l = WorkerLines::new();
        register_worker_lines(&mut l);
        let start = l.start_lines("chat", &services);
        assert_eq!(start.len(), 1);
        assert_eq!(start[0]["type"], "mcp_config");
        assert_eq!(
            start[0]["servers"][0]["headers"]["Authorization"],
            "Bearer tok-123456"
        );
        assert_eq!(start[0]["servers"][0]["secrets"][0], "tok-123456");

        let sink = Arc::new(RecordingSink::default());
        let events: Arc<dyn EventSink> = sink.clone();
        let ctx = WorkerLineCtx {
            chat_id: "chat",
            services: &services,
            events: &events,
        };
        let line = json!({"type": "mcp_status", "serverId": "hub", "state": "connected", "toolCount": 4, "at": 5});
        assert!(l.handle(&ctx, &line));
        assert!(l.handle(&ctx, &line), "a repeat is still handled");
        assert_eq!(sink.kinds(), ["mcp_status"], "a repeat emits nothing new");
        assert_eq!(
            services.get::<McpStatusStore>().unwrap().list()[0].tool_count,
            Some(4)
        );

        cell.broadcast();
        let b = sent.0.lock().unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0]["type"], "mcp_config");
    }
}
