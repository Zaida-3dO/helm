//! The host side of the request channel.
//!
//! A worker writes `chat_request {id, operation, args, caller?}` and creates a private folder for
//! the id; the host gates the call, runs it off the actor, and writes `response.json` into that
//! folder. Gate order: the restricted gate, then the restricted-only tools check
//! (none are registered), then the dispatcher, which re-validates
//! its own rules (size, concurrency, timeout, response size).

use crate::registry::{CallOrigin, Changed};
use crate::registry::{HostToolRegistry, RegisteredTool, ToolCtx, ToolError, ToolHandler};
use helm_ipc::{EventSink, Services};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;

/// Calls in flight per worker; the worker enforces the same number.
pub const MAX_CONCURRENT: usize = 8;
/// Largest result, serialised.
pub const MAX_RESPONSE_BYTES: usize = 50 * 1024;
/// Read-heavy tools share this many permits.
pub const INSPECTION_PERMITS: usize = 2;

/// The fixed refusal for a parent-only tool called by a child.
pub fn parent_only(name: &str) -> String {
    format!("{name} is only available to the parent chat.")
}

/// One `chat_request` line.
#[derive(Clone, Debug, Deserialize)]
pub struct ChatRequest {
    pub id: String,
    pub operation: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub caller: Option<CallerBinding>,
}

/// What the worker attributes a call to.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallerBinding {
    #[serde(default)]
    pub child: bool,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// The calling chat, from the host's own record of the worker (never from the line).
#[derive(Clone, Debug)]
pub struct Caller {
    pub chat_id: String,
    /// `Some(list)` for a restricted chat: only these tools, default deny.
    pub restricted: Option<Vec<String>>,
}

/// The host side of the channel: gates, limits and execution.
pub struct HostTools {
    registry: Arc<HostToolRegistry>,
    inflight: Mutex<HashMap<String, usize>>,
    maintenance: Arc<tokio::sync::Mutex<()>>,
    inspections: Arc<Semaphore>,
    ops: Option<Arc<dyn crate::registry::OpInvoker>>,
}

struct Slot<'a> {
    map: &'a Mutex<HashMap<String, usize>>,
    chat: String,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut m = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(n) = m.get_mut(&self.chat) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.chat);
            }
        }
    }
}

impl HostTools {
    pub fn new(registry: Arc<HostToolRegistry>) -> Self {
        Self {
            registry,
            inflight: Mutex::new(HashMap::new()),
            maintenance: Arc::new(tokio::sync::Mutex::new(())),
            inspections: Arc::new(Semaphore::new(INSPECTION_PERMITS)),
            ops: None,
        }
    }

    /// The `api` ops tools may act through ([`ToolCtx::op`]).
    pub fn with_ops(mut self, ops: Arc<dyn crate::registry::OpInvoker>) -> Self {
        self.ops = Some(ops);
        self
    }

    /// Share the app-wide maintenance lock with the op dispatcher, so mutating tools never overlap
    /// maintenance ops.
    pub fn with_maintenance(mut self, lock: Arc<tokio::sync::Mutex<()>>) -> Self {
        self.maintenance = lock;
        self
    }

    /// Whether tools that act through `api` ops can run.
    pub fn has_ops(&self) -> bool {
        self.ops.is_some()
    }

    pub fn registry(&self) -> &Arc<HostToolRegistry> {
        &self.registry
    }

    /// The gates, in order. `Ok` names the tool to run.
    pub fn gate(&self, caller: &Caller, req: &ChatRequest) -> Result<&RegisteredTool, ToolError> {
        let name = req.operation.as_str();
        // 1. Restricted gate (authoritative, default deny).
        if let Some(allowed) = &caller.restricted {
            if !allowed.iter().any(|a| a == name) {
                return Err(ToolError::refused(format!(
                    "This chat is restricted and cannot use {name}."
                )));
            }
        }
        // 2. Restricted-only tools: none.
        // 3. Dispatcher.
        let Some(tool) = self.registry.get(name) else {
            return Err(ToolError::refused(format!("There is no host tool {name}.")));
        };
        // A request without a binding counts as a child's: only the worker's attribution makes a
        // call the parent's.
        let child = req.caller.as_ref().is_none_or(|b| b.child);
        if child && !tool.spec.availability.children {
            return Err(ToolError::refused(parent_only(name)));
        }
        let size = serde_json::to_vec(&req.args)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        let limit = tool.spec.request_limit.bytes();
        if size > limit {
            return Err(ToolError::refused(format!(
                "The request is {size} bytes; {name} takes at most {} KiB.",
                limit / 1024
            )));
        }
        if !(req.args.is_object() || req.args.is_null()) {
            return Err(ToolError::refused("Arguments must be an object."));
        }
        Ok(tool)
    }

    fn take_slot(&self, chat: &str) -> Option<Slot<'_>> {
        let mut m = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
        let n = m.entry(chat.to_owned()).or_insert(0);
        if *n >= MAX_CONCURRENT {
            return None;
        }
        *n += 1;
        Some(Slot {
            map: &self.inflight,
            chat: chat.to_owned(),
        })
    }

    /// Gate and run one request; returns the `response.json` body.
    pub async fn call(
        &self,
        services: Arc<Services>,
        events: Arc<dyn EventSink>,
        caller: &Caller,
        req: &ChatRequest,
    ) -> Value {
        let out = self.run(services, events, caller, req).await;
        response(&req.id, out)
    }

    async fn run(
        &self,
        services: Arc<Services>,
        events: Arc<dyn EventSink>,
        caller: &Caller,
        req: &ChatRequest,
    ) -> Result<Value, ToolError> {
        let tool = self.gate(caller, req)?;
        let Some(_slot) = self.take_slot(&caller.chat_id) else {
            return Err(ToolError::refused(format!(
                "This chat already has {MAX_CONCURRENT} host-tool calls running."
            )));
        };
        let binding = req.caller.clone().unwrap_or(CallerBinding {
            child: true,
            tool_call_id: None,
        });
        let ctx = ToolCtx {
            caller_chat_id: caller.chat_id.clone(),
            is_child: binding.child,
            tool_call_id: binding.tool_call_id,
            origin: if binding.child {
                CallOrigin::Subagent
            } else {
                CallOrigin::Agent
            },
            services,
            events,
            ops: self.ops.clone(),
        };
        let args = if req.args.is_null() {
            json!({})
        } else {
            req.args.clone()
        };
        let mutating = tool.spec.mutating;
        let self_locking = tool.spec.self_locking;
        let timeout = Duration::from_millis(tool.spec.timeout_ms);
        let handler = tool.handler.clone();
        let fut = async move {
            match handler {
                ToolHandler::Blocking(f) => tokio::task::spawn_blocking(move || f(&ctx, args))
                    .await
                    .unwrap_or_else(|e| Err(ToolError::maybe(format!("The tool failed: {e}")))),
                ToolHandler::Async(f) => f(ctx, args).await,
            }
        };
        // The timeout counts the wait for the lock or permit too, so a queued call never starts
        // after the worker has given up on it.
        let (maintenance, inspections) = (self.maintenance.clone(), self.inspections.clone());
        let result = tokio::time::timeout(timeout, async move {
            if self_locking {
                fut.await
            } else if mutating {
                let _g = maintenance.lock_owned().await;
                fut.await
            } else {
                let _p = inspections.acquire_owned().await;
                fut.await
            }
        })
        .await;
        let value = match result {
            Ok(r) => r?,
            Err(_) => {
                return Err(if mutating {
                    ToolError::maybe(format!(
                        "{} did not finish within {} s.",
                        req.operation,
                        timeout.as_secs()
                    ))
                } else {
                    ToolError::refused(format!(
                        "{} did not finish within {} s.",
                        req.operation,
                        timeout.as_secs()
                    ))
                })
            }
        };
        let size = serde_json::to_vec(&value)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if size > MAX_RESPONSE_BYTES {
            let msg = format!("The result is {size} bytes, over the 50 KiB limit; ask for less.");
            return Err(if mutating {
                ToolError::maybe(msg)
            } else {
                ToolError::refused(msg)
            });
        }
        Ok(value)
    }
}

/// The `response.json` body for an outcome.
pub fn response(id: &str, out: Result<Value, ToolError>) -> Value {
    match out {
        Ok(result) => json!({"id": id, "ok": true, "result": result}),
        Err(e) => json!({
            "id": id,
            "ok": false,
            "error": e.text(),
            "changed": match e.changed { Changed::No => "no", Changed::Maybe => "maybe" },
        }),
    }
}

/// A request id the host will use as a folder name: a lowercase 36-character UUID.
pub fn valid_request_id(id: &str) -> bool {
    id.len() == 36
        && id.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

/// Write `response.json` into the request's folder under the chat's tools directory. Returns
/// false (and writes nothing) when the id is malformed or the folder is gone (the worker gave
/// up and cancelled).
pub fn write_response(tools_dir: &Path, id: &str, body: &Value) -> std::io::Result<bool> {
    if !valid_request_id(id) {
        return Ok(false);
    }
    let dir = tools_dir.join(id);
    // Not followed through a link or junction: the folder must be a real directory.
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
        _ => return Ok(false),
    }
    let bytes = serde_json::to_vec(body).map_err(std::io::Error::other)?;
    let tmp = dir.join("response.json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, dir.join("response.json"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::ToolSpec;
    use helm_ipc::NullSink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn obj() -> Value {
        json!({"type": "object"})
    }

    fn tools() -> HostTools {
        let mut r = HostToolRegistry::new();
        r.blocking(
            ToolSpec::read("app_list_chats", "List.", obj()),
            |ctx, a| Ok(json!({"caller": ctx.caller_chat_id, "child": ctx.is_child, "args": a})),
        )
        .blocking(
            ToolSpec::read("app_mcp_tools", "MCP.", obj()).children(),
            |_, _| Ok(json!("ok")),
        )
        .blocking(
            ToolSpec::write("app_update_settings", "S.", obj()).large(),
            |_, _| Ok(json!("set")),
        )
        .blocking(ToolSpec::read("app_read_chat", "Big.", obj()), |_, _| {
            Ok(json!("x".repeat(MAX_RESPONSE_BYTES)))
        })
        .async_tool(
            ToolSpec::write("app_create_chat", "Slow.", obj()).timeout_ms(50),
            |_, _| async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(json!("late"))
            },
        )
        .async_tool(
            ToolSpec::read("app_list_projects", "Slow read.", obj()).timeout_ms(50),
            |_, _| async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(json!("late"))
            },
        );
        HostTools::new(Arc::new(r))
    }

    fn req(op: &str, args: Value, child: bool) -> ChatRequest {
        ChatRequest {
            id: "00000000-0000-4000-8000-000000000001".into(),
            operation: op.into(),
            args,
            caller: Some(CallerBinding {
                child,
                tool_call_id: Some("tu".into()),
            }),
        }
    }

    fn me() -> Caller {
        Caller {
            chat_id: "chat-1".into(),
            restricted: None,
        }
    }

    async fn call(t: &HostTools, c: &Caller, r: &ChatRequest) -> Value {
        t.call(Arc::new(Services::new()), Arc::new(NullSink), c, r)
            .await
    }

    #[tokio::test]
    async fn the_caller_comes_from_the_host_not_the_args() {
        let t = tools();
        let r = call(
            &t,
            &me(),
            &req("app_list_chats", json!({"callerChatId": "evil"}), false),
        )
        .await;
        assert_eq!(r["ok"], true);
        assert_eq!(r["result"]["caller"], "chat-1");
        assert_eq!(r["id"], "00000000-0000-4000-8000-000000000001");
    }

    #[tokio::test]
    async fn children_get_child_capable_tools_only() {
        let t = tools();
        let r = call(&t, &me(), &req("app_list_chats", json!({}), true)).await;
        assert_eq!(r["ok"], false);
        assert_eq!(
            r["error"],
            "app_list_chats is only available to the parent chat. Nothing changed."
        );
        let r = call(&t, &me(), &req("app_mcp_tools", json!({}), true)).await;
        assert_eq!(r["ok"], true);
    }

    #[tokio::test]
    async fn a_request_without_a_binding_counts_as_a_childs() {
        let t = tools();
        let mut r = req("app_list_chats", json!({}), false);
        r.caller = None;
        let v = call(&t, &me(), &r).await;
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("only available to the parent chat"));
    }

    #[tokio::test]
    async fn the_restricted_gate_comes_first_and_denies_by_default() {
        let t = tools();
        let c = Caller {
            chat_id: "chat-1".into(),
            restricted: Some(vec!["app_mcp_tools".into()]),
        };
        let r = call(&t, &c, &req("app_list_chats", json!({}), false)).await;
        assert!(r["error"].as_str().unwrap().contains("restricted"));
        // Even an unknown tool is refused by the restricted gate, before the dispatcher.
        let r = call(&t, &c, &req("app_nope", json!({}), false)).await;
        assert!(r["error"].as_str().unwrap().contains("restricted"));
        let r = call(&t, &c, &req("app_mcp_tools", json!({}), false)).await;
        assert_eq!(r["ok"], true);
    }

    #[tokio::test]
    async fn request_size_limits_8_and_32_kib() {
        let t = tools();
        let fits = json!({"s": "x".repeat(8 * 1024 - 8)});
        assert_eq!(serde_json::to_vec(&fits).unwrap().len(), 8 * 1024);
        assert_eq!(
            call(&t, &me(), &req("app_list_chats", fits, false)).await["ok"],
            true
        );
        let over = json!({"s": "x".repeat(8 * 1024 - 7)});
        let r = call(&t, &me(), &req("app_list_chats", over.clone(), false)).await;
        assert!(r["error"].as_str().unwrap().contains("at most 8 KiB"));
        assert_eq!(r["changed"], "no");
        // The settings tool takes 32 KiB.
        assert_eq!(
            call(&t, &me(), &req("app_update_settings", over, false)).await["ok"],
            true
        );
        let big = json!({"s": "x".repeat(32 * 1024)});
        let r = call(&t, &me(), &req("app_update_settings", big, false)).await;
        assert!(r["error"].as_str().unwrap().contains("at most 32 KiB"));
    }

    #[tokio::test]
    async fn responses_over_50_kib_are_refused() {
        let t = tools();
        let r = call(&t, &me(), &req("app_read_chat", json!({}), false)).await;
        assert!(r["error"].as_str().unwrap().contains("50 KiB"));
    }

    #[tokio::test]
    async fn timeouts_say_what_may_have_changed() {
        let t = tools();
        let r = call(&t, &me(), &req("app_create_chat", json!({}), false)).await;
        assert_eq!(r["changed"], "maybe");
        assert!(r["error"].as_str().unwrap().contains("may have happened"));
        let r = call(&t, &me(), &req("app_list_projects", json!({}), false)).await;
        assert_eq!(r["changed"], "no");
    }

    #[tokio::test]
    async fn unknown_tools_and_non_object_args_are_refused() {
        let t = tools();
        let r = call(&t, &me(), &req("app_nope", json!({}), false)).await;
        assert_eq!(
            r["error"],
            "There is no host tool app_nope. Nothing changed."
        );
        let r = call(&t, &me(), &req("app_list_chats", json!([1]), false)).await;
        assert!(r["error"].as_str().unwrap().contains("object"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn at_most_8_calls_per_chat_run_at_once() {
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut r = HostToolRegistry::new();
        let (run2, peak2) = (running.clone(), peak.clone());
        r.async_tool(
            ToolSpec::write("app_send_message", "Send.", obj()),
            move |_, _| {
                let (run, peak) = (run2.clone(), peak2.clone());
                async move {
                    let n = run.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    run.fetch_sub(1, Ordering::SeqCst);
                    Ok(json!("sent"))
                }
            },
        );
        let t = Arc::new(HostTools::new(Arc::new(r)));
        let mut joins = Vec::new();
        for _ in 0..10 {
            let t = t.clone();
            joins.push(tokio::spawn(async move {
                call(&t, &me(), &req("app_send_message", json!({}), false)).await
            }));
        }
        let mut refused = 0;
        for j in joins {
            let v = j.await.unwrap();
            if v["ok"] == false {
                assert!(v["error"].as_str().unwrap().contains("8 host-tool calls"));
                refused += 1;
            }
        }
        assert_eq!(refused, 2, "the 9th and 10th concurrent calls are refused");
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "mutating calls hold the maintenance lock"
        );
        // Slots are released: another chat and a later call both get in.
        let v = call(&t, &me(), &req("app_send_message", json!({}), false)).await;
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn request_ids_and_mailbox_writes() {
        assert!(valid_request_id("00000000-0000-4000-8000-000000000001"));
        assert!(!valid_request_id("../../etc/passwd-0000-4000-8000-0000000"));
        assert!(!valid_request_id("00000000-0000-4000-8000-00000000000G"));
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let body = json!({"id": id, "ok": true, "result": 1});
        assert!(
            !write_response(dir.path(), id, &body).unwrap(),
            "no folder: cancelled"
        );
        std::fs::create_dir(dir.path().join(id)).unwrap();
        assert!(write_response(dir.path(), id, &body).unwrap());
        let read: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(id).join("response.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(read, body);
        assert!(!write_response(dir.path(), "..", &body).unwrap());
    }
}
