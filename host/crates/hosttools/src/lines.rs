//! The worker-message registry: the seam other crates use to hear
//! new worker-to-host lines and to talk to running workers, without editing the chats actor.
//!
//! - [`WorkerLines::register_worker_line`]: a handler for one worker line `type` the actor does
//!   not handle itself (`mcp_status`, `plan_usage`, ...). The actor hands every unknown line here.
//! - [`WorkerLines::register_start_line`]: a line sent to every worker right after it starts
//!   (`mcp_config`), or nothing.
//! - [`WorkerBroadcast`]: implemented by the chat runtime and shared as the [`Broadcaster`]
//!   service, so a crate can re-send a line to every running worker (`broadcast_to_workers`, the
//!   `mcp_config` re-send on a config change) or to one.
//!
//! Each crate adds one `register_worker_lines` line to `registrations.rs`.

use helm_ipc::{EventSink, Services};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

/// What a line handler sees.
pub struct WorkerLineCtx<'a> {
    /// The chat whose worker sent the line (never taken from the line).
    pub chat_id: &'a str,
    pub services: &'a Arc<Services>,
    pub events: &'a Arc<dyn EventSink>,
}

pub type LineHandler = Arc<dyn Fn(&WorkerLineCtx<'_>, &Value) + Send + Sync>;
pub type StartLine = Arc<dyn Fn(&str, &Arc<Services>) -> Option<Value> + Send + Sync>;

/// Line types the actor itself owns; registering one panics.
pub const ACTOR_LINES: &[&str] = &[
    "response",
    "agent_start",
    "message_start",
    "message_update",
    "message_end",
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "auto_retry_start",
    "auto_retry_end",
    "agent_settled",
    "extension_ui_request",
    "permission_mode",
    "native_session",
    "submission_delivered",
    "background_tasks",
    "compaction_start",
    "compaction_end",
    "notice",
    "long_command",
    "worker_error",
    "chat_request",
    "chat_cancel",
];

#[derive(Clone, Default)]
pub struct WorkerLines {
    handlers: BTreeMap<String, LineHandler>,
    start: Vec<StartLine>,
}

impl WorkerLines {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle worker lines of `kind`. A duplicate, or a kind the actor owns, panics.
    pub fn register_worker_line(
        &mut self,
        kind: &str,
        f: impl Fn(&WorkerLineCtx<'_>, &Value) + Send + Sync + 'static,
    ) -> &mut Self {
        assert!(
            !ACTOR_LINES.contains(&kind),
            "worker line {kind} is the actor's own"
        );
        assert!(
            self.handlers.insert(kind.to_owned(), Arc::new(f)).is_none(),
            "worker line registered twice: {kind}"
        );
        self
    }

    /// A line to send each worker once it starts (for example `mcp_config`), or `None`.
    pub fn register_start_line(
        &mut self,
        f: impl Fn(&str, &Arc<Services>) -> Option<Value> + Send + Sync + 'static,
    ) -> &mut Self {
        self.start.push(Arc::new(f));
        self
    }

    /// Route one line by its `type`. Returns false when no handler is registered for it.
    pub fn handle(&self, ctx: &WorkerLineCtx<'_>, line: &Value) -> bool {
        let Some(kind) = line.get("type").and_then(Value::as_str) else {
            return false;
        };
        match self.handlers.get(kind) {
            Some(h) => {
                h(ctx, line);
                true
            }
            None => false,
        }
    }

    /// The lines a newly started worker gets, in registration order.
    pub fn start_lines(&self, chat_id: &str, services: &Arc<Services>) -> Vec<Value> {
        self.start
            .iter()
            .filter_map(|f| f(chat_id, services))
            .collect()
    }

    pub fn kinds(&self) -> Vec<&str> {
        self.handlers.keys().map(String::as_str).collect()
    }
}

/// Sending to running workers; the chat runtime implements it.
pub trait WorkerBroadcast: Send + Sync {
    /// Send `line` to every running worker.
    fn broadcast_to_workers(&self, line: &Value);
    /// Send `line` to one chat's worker; false when it is not running.
    fn send_to_worker(&self, chat_id: &str, line: &Value) -> bool;
}

/// The shared service wrapping the runtime's [`WorkerBroadcast`].
#[derive(Clone)]
pub struct Broadcaster(pub Arc<dyn WorkerBroadcast>);

#[cfg(test)]
mod tests {
    use super::*;
    use helm_ipc::NullSink;
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn lines_route_by_type_and_unknown_ones_fall_through() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let mut l = WorkerLines::new();
        l.register_worker_line("mcp_status", move |ctx, v| {
            s2.lock()
                .unwrap()
                .push((ctx.chat_id.to_owned(), v["serverId"].clone()));
        });
        let services = Arc::new(Services::new());
        let events: Arc<dyn EventSink> = Arc::new(NullSink);
        let ctx = WorkerLineCtx {
            chat_id: "c1",
            services: &services,
            events: &events,
        };
        assert!(l.handle(&ctx, &json!({"type": "mcp_status", "serverId": "s"})));
        assert!(!l.handle(&ctx, &json!({"type": "plan_usage"})));
        assert!(!l.handle(&ctx, &json!({"no": "type"})));
        assert_eq!(*seen.lock().unwrap(), [("c1".to_owned(), json!("s"))]);
        assert_eq!(l.kinds(), ["mcp_status"]);
    }

    #[test]
    fn start_lines_are_collected_in_order() {
        let mut l = WorkerLines::new();
        l.register_start_line(|chat, _| Some(json!({"type": "a", "chat": chat})))
            .register_start_line(|_, _| None)
            .register_start_line(|_, _| Some(json!({"type": "b"})));
        let services = Arc::new(Services::new());
        assert_eq!(
            l.start_lines("c1", &services),
            [json!({"type": "a", "chat": "c1"}), json!({"type": "b"})]
        );
    }

    #[test]
    #[should_panic(expected = "the actor's own")]
    fn actor_lines_cannot_be_taken() {
        WorkerLines::new().register_worker_line("chat_request", |_, _| {});
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicates_panic() {
        let mut l = WorkerLines::new();
        l.register_worker_line("x", |_, _| {})
            .register_worker_line("x", |_, _| {});
    }
}
