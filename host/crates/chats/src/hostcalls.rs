//! Host tools inside the chat runtime.
//!
//! - A worker's `chat_request` line is gated and run by [`helm_hosttools::HostTools`] off the
//!   actor, and the answer is written to `response.json` in the request's folder.
//! - `chat_cancel` aborts a host task that is still running.
//! - Every other line the actor does not handle goes to the worker-message registry
//!   ([`helm_hosttools::WorkerLines`]), and each worker gets the registry's start lines once it is
//!   ready (plan-review change 2).
//!
//! The seam is attached once the app's services exist ([`crate::ChatRuntime::attach_host`]);
//! before that (and in tests that never attach it) a request is answered with a refusal and
//! other lines are dropped, as before.

use crate::runtime::Shared;
use helm_hosttools::dispatch::{response, write_response, Caller, ChatRequest};
use helm_hosttools::{HostTools, ToolError, WorkerLineCtx, WorkerLines};
use helm_ipc::OpCtx;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::AbortHandle;

/// What the runtime needs to serve host tools and registered worker lines.
#[derive(Clone)]
pub struct HostSeam {
    /// The app's services and event sink, as op handlers see them.
    pub ctx: OpCtx,
    pub tools: Arc<HostTools>,
    pub lines: Arc<WorkerLines>,
}

/// The chat's host-tool request folders: `<profile>/workers/<chat>/tools`.
pub fn tools_dir(profile_dir: &std::path::Path, chat_id: &str) -> PathBuf {
    profile_dir.join("workers").join(chat_id).join("tools")
}

/// A `chat_request` from this chat's worker.
pub(crate) fn on_request(
    sh: &Arc<Shared>,
    chat_id: &str,
    calls: &mut HashMap<String, AbortHandle>,
    line: &Value,
) {
    calls.retain(|_, h| !h.is_finished());
    let req: ChatRequest = match serde_json::from_value(line.clone()) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("chat {chat_id}: bad chat_request: {e}");
            return;
        }
    };
    let dir = tools_dir(&sh.cfg.profile_dir, chat_id);
    let caller = Caller {
        chat_id: chat_id.to_owned(),
        restricted: None, // no restricted chats yet
    };
    let seam = sh.host.get().cloned();
    let id = req.id.clone();
    let task = sh.handle.spawn(async move {
        let body = match seam {
            Some(s) => {
                s.tools
                    .call(s.ctx.services.clone(), s.ctx.events.clone(), &caller, &req)
                    .await
            }
            None => response(
                &req.id,
                Err(ToolError::refused("Helm's host tools are not ready yet.")),
            ),
        };
        match write_response(&dir, &req.id, &body) {
            Ok(true) => {}
            Ok(false) => log::warn!(
                "host tool {}: no request folder for {} (the worker gave up, or the folders differ)",
                req.operation,
                req.id
            ),
            Err(e) => log::warn!("host tool {}: could not write the answer: {e}", req.operation),
        }
    });
    calls.insert(id, task.abort_handle());
}

/// A `chat_cancel`: the worker stopped waiting, so stop the host task if it is still running.
pub(crate) fn on_cancel(calls: &mut HashMap<String, AbortHandle>, line: &Value) {
    if let Some(id) = line.get("id").and_then(Value::as_str) {
        if let Some(h) = calls.remove(id) {
            h.abort();
        }
    }
}

/// Any other line: the worker-message registry's, if a crate registered its type.
pub(crate) fn other_line(sh: &Arc<Shared>, chat_id: &str, line: &Value) {
    let Some(seam) = sh.host.get() else {
        return;
    };
    let ctx = WorkerLineCtx {
        chat_id,
        services: &seam.ctx.services,
        events: &seam.ctx.events,
    };
    seam.lines.handle(&ctx, line);
}

/// The lines a newly ready worker gets.
pub(crate) fn start_lines(sh: &Arc<Shared>, chat_id: &str) -> Vec<Value> {
    sh.host
        .get()
        .map(|s| s.lines.start_lines(chat_id, &s.ctx.services))
        .unwrap_or_default()
}

/// The host-tool part of a launch: the catalogue for this chat's launch keys and the MCP gate's
/// flags. `None` until the seam is attached.
pub(crate) fn launch(
    sh: &Arc<Shared>,
    chat_id: &str,
    cwd: &str,
) -> Option<crate::runtime::HostLaunch> {
    let seam = sh.host.get()?;
    let keys = sh
        .db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(launch_keys(tx)))
        .unwrap_or_default();
    let mcp = seam
        .ctx
        .services
        .get::<helm_hosttools::McpLaunchGate>()
        .map(|g| g.0.for_launch(chat_id, std::path::Path::new(cwd)))
        .unwrap_or(helm_hosttools::McpLaunch {
            native: true,
            ..Default::default()
        });
    let keys = helm_hosttools::LaunchKeys {
        mcp_enabled: mcp.helm_servers,
        ..keys
    };
    let tools = serde_json::to_value(seam.tools.registry().for_launch(&keys)).ok()?;
    Some(crate::runtime::HostLaunch {
        tools,
        strict_mcp: !mcp.native,
        disabled_mcp_servers: mcp.disabled,
    })
}

fn truthy(v: Option<Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => b,
        Some(Value::Object(o)) => o.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        _ => false,
    }
}

/// The launch keys the preferences decide: projects mode and "Chats can change all
/// settings".
pub(crate) fn launch_keys(tx: &rusqlite::Connection) -> helm_hosttools::LaunchKeys {
    helm_hosttools::LaunchKeys {
        projects_mode: truthy(helm_core::prefs::read(tx, "projectsMode")),
        all_settings: truthy(helm_core::prefs::read(tx, "agentSettingsAll")),
        ..Default::default()
    }
}

impl helm_hosttools::WorkerBroadcast for crate::ChatRuntime {
    fn broadcast_to_workers(&self, line: &Value) {
        let handles: Vec<_> = self.sh.actors.lock().unwrap().values().cloned().collect();
        for h in handles {
            let _ = h.tx.try_send(crate::actor::Cmd::WorkerLine(line.clone()));
        }
    }

    fn send_to_worker(&self, chat_id: &str, line: &Value) -> bool {
        let h = self.sh.actors.lock().unwrap().get(chat_id).cloned();
        match h {
            Some(h) if h.live.lock().unwrap().has_worker => {
                h.tx.try_send(crate::actor::Cmd::WorkerLine(line.clone()))
                    .is_ok()
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_tests::harness;
    use helm_hosttools::{HostToolRegistry, ToolSpec, WorkerBroadcast};
    use serde_json::json;
    use std::sync::Mutex;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn requests_lines_and_broadcasts_reach_the_host_tools_seam() {
        let mut h = harness();
        let mut reg = HostToolRegistry::new();
        reg.blocking(
            ToolSpec::read("app_list_chats", "List chats.", json!({"type": "object"})),
            |ctx, _| Ok(json!({"caller": ctx.caller_chat_id})),
        );
        let seen = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let seen2 = seen.clone();
        let mut lines = WorkerLines::new();
        lines
            .register_worker_line("plan_usage", move |ctx, v| {
                seen2
                    .lock()
                    .unwrap()
                    .push((ctx.chat_id.to_owned(), v.clone()))
            })
            .register_start_line(|_, _| Some(json!({"type": "mcp_config", "revision": "r1"})));
        let ctx = h.api.ctx().clone();
        h.rt.attach_host(HostSeam {
            ctx,
            tools: Arc::new(HostTools::new(Arc::new(reg))),
            lines: Arc::new(lines),
        });

        let chat = h.create().await;
        h.send(&chat, "hi", "default").await;
        let mut w = h.worker().await;
        let host = w.req.host.clone().expect("the launch lists host tools");
        assert_eq!(host.tools[0]["name"], "app_list_chats");
        assert!(!host.strict_mcp, "native MCP stays on without a gate");
        w.ready().await;
        // The start line goes first after the worker is ready.
        let first = w.next().await;
        assert_eq!(first, json!({"type": "mcp_config", "revision": "r1"}));

        // A chat_request is answered in the request folder, with the caller from the host.
        let id = "00000000-0000-4000-8000-0000000000aa";
        let dir = tools_dir(&h.profile.path(""), &chat).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        w.emit(
            json!({"type": "chat_request", "id": id, "operation": "app_list_chats",
                      "args": {"chatId": "someone-else"}, "caller": {"child": false}}),
        )
        .await;
        let file = dir.join("response.json");
        for _ in 0..200 {
            if file.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let body: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["result"]["caller"], chat);

        // An unknown line type goes to the registry with the right chat.
        w.emit(json!({"type": "plan_usage", "windows": []})).await;
        h.until("the line handler ran", |_| !seen.lock().unwrap().is_empty())
            .await;
        assert_eq!(seen.lock().unwrap()[0].0, chat);

        // Tool rows keep which server the tool came from.
        w.emit(json!({"type": "tool_execution_start", "toolCallId": "tu1", "toolName": "mcp__helm__app_list_chats",
                      "args": {}, "toolSource": {"kind": "host", "server": "Helm", "tool": "app_list_chats"}}))
            .await;
        h.until("the tool row", |h| {
            !h.col("SELECT details FROM messages WHERE tool_call_id = 'tu1'")
                .is_empty()
        })
        .await;
        let d: Value = serde_json::from_str(
            &h.col("SELECT details FROM messages WHERE tool_call_id = 'tu1'")[0],
        )
        .unwrap();
        assert_eq!(d["toolSource"]["kind"], "host");

        // Other crates reach running workers.
        h.rt.broadcast_to_workers(&json!({"type": "mcp_config", "revision": "r2"}));
        assert_eq!(w.expect("mcp_config").await["revision"], "r2");
        assert!(h
            .rt
            .send_to_worker(&chat, &json!({"type": "mcp_config", "revision": "r3"})));
        assert_eq!(w.expect("mcp_config").await["revision"], "r3");
        assert!(!h.rt.send_to_worker("no-such-chat", &json!({"type": "x"})));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_the_seam_a_launch_lists_no_host_tools() {
        let mut h = harness();
        let chat = h.create().await;
        h.send(&chat, "hi", "default").await;
        let w = h.worker().await;
        assert!(w.req.host.is_none());
    }

    #[test]
    fn launch_keys_read_projects_mode_and_the_all_settings_switch() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER);",
        )
        .ok();
        let k = launch_keys(&c);
        assert!(!k.projects_mode && !k.all_settings);
        assert!(truthy(Some(json!(true))));
        assert!(truthy(Some(json!({"enabled": true}))));
        assert!(!truthy(Some(json!({"enabled": false}))));
        assert!(!truthy(Some(json!("yes"))));
    }
}
