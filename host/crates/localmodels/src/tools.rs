//! `app_gpu`: the graphics card as an agent tool — read the card's lock (status), unload the
//! local models to free the card (unload), and bring them back (reload).
//!
//! `hold` and `release` set and clear the graphics-card hold (see `hold`): a note that makes the
//! composers of local-model chats warn before a send reloads the model server. The first three
//! actions are exactly the ops the Settings > Local models card uses, so the tool adds
//! no path of its own: the ops already refuse an unload when nothing is loaded and a reload when
//! the models are already on. Unload is the one with a consequence — it stops the chats running
//! on local models and keeps new ones from loading until a reload — so it asks for an explicit
//! `confirm`, the same say-so the button's own confirm dialog demands.

use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use serde_json::{json, Value};

async fn gpu(ctx: ToolCtx, a: Value) -> ToolResult {
    let action = a.get("action").and_then(Value::as_str).unwrap_or("");
    match action {
        "status" => ctx.op("local_lock_state", json!({})).await,
        "unload" => {
            if a.get("confirm").and_then(Value::as_bool) != Some(true) {
                return Err(ToolError::refused(
                    "unload stops the chats running on local models on this PC and keeps models \
                     from loading until a reload: pass confirm true only when the user asked.",
                ));
            }
            ctx.op("local_unload", json!({})).await
        }
        "reload" => ctx.op("local_reload", json!({})).await,
        "hold" => {
            let reason = a.get("reason").and_then(Value::as_str).unwrap_or("");
            if reason.trim().is_empty() {
                return Err(ToolError::refused(
                    "hold needs a reason: one short line the composers show, such as \"Benchmark running\".",
                ));
            }
            let mut args = json!({"reason": reason});
            if let Some(h) = a.get("hours") {
                args["hours"] = h.clone();
            }
            // The hold belongs to the chat that set it: the tool layer says so, and an agent
            // cannot name another chat.
            args["ownerChatId"] = json!(ctx.caller_chat_id);
            ctx.op("local_hold_set", args).await
        }
        "release" => {
            ctx.op("local_hold_clear", json!({"byChatId": ctx.caller_chat_id}))
                .await
        }
        other => Err(ToolError::refused(format!(
            "\"{other}\" is not an action: use status, unload, reload, hold or release."
        ))),
    }
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            "app_gpu",
            "Share the graphics card with the local model server: read the lock (status), unload \
             the loaded models to free the card (unload), bring them back (reload), or put a warning on \
             sending to local-model chats while you use the card (hold, release).",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["status", "unload", "reload", "hold", "release"]},
                    "reason": {
                        "type": "string",
                        "description": "With hold: one short line the composers show, such as \"Benchmark running\"."
                    },
                    "hours": {
                        "type": "number",
                        "description": "With hold: hours the note lasts; 2 when left out, 8 at most."
                    },
                    "confirm": {
                        "type": "boolean",
                        "description": "With unload: true when the user asked to free the card."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        )
        .self_locking()
        .guidelines(
            "status: no arguments; who holds the card now and what the local models are doing.\n\
             unload: pass confirm true only when the user asked for it — it stops the chats that \
             run on local models and refuses new loads until a reload; while a game holds the \
             card it waits for the game instead. It answers at once and the work continues; the \
             card's state in Settings follows.\n\
             reload: brings the models back after an unload; safe, and refused when they are \
             already on.\n\
             hold: while you use the card for something a model load would spoil (a benchmark, a \
             long GPU job), set a hold with a short reason. Every composer of a local-model chat \
             shows the reason with \"sending will reload the local model\" and asks once before \
             the first send; a message from a schedule, a wait or another chat to a local-model \
             chat is saved and waits until the hold ends instead of waking the model, and so does \
             the first message of a new local-model chat. The hold belongs to your chat: only you \
             can release it, and it lifts by itself after hours (2 by default, 8 at most) or when \
             your chat ends.\n\
             release: clears your own hold now; another chat's hold is refused. Chats on Claude \
             models are never held.\n\
             Never unload on your own: it takes the card away from other working chats.",
        ),
        gpu,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_hosttools::{CallOrigin, OpInvoker, ToolHandler};
    use std::sync::{Arc, Mutex};

    struct Fake(Mutex<Vec<(String, Value)>>);
    impl OpInvoker for Fake {
        fn invoke<'a>(
            &'a self,
            op: &'a str,
            args: Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Value, helm_ipc::ApiError>> + Send + 'a>,
        > {
            self.0.lock().unwrap().push((op.to_owned(), args.clone()));
            let out = match op {
                "local_lock_state" => Ok(json!({"state": "on", "working": false})),
                "local_unload" => Ok(json!({"state": "manual", "working": true})),
                "local_reload" => Ok(json!({"state": "on", "working": false})),
                "local_hold_set" => Ok(json!({"state": "on", "holdReason": args["reason"]})),
                "local_hold_clear" => Ok(json!({"state": "on"})),
                other => Err(helm_ipc::ApiError::new("not_found", other.to_owned())),
            };
            Box::pin(async move { out })
        }
    }

    fn ctx(ops: &Arc<Fake>) -> ToolCtx {
        ToolCtx {
            caller_chat_id: "me".into(),
            is_child: false,
            tool_call_id: Some("tc1".into()),
            origin: CallOrigin::Agent,
            services: Arc::new(helm_ipc::Services::new()),
            events: Arc::new(helm_ipc::NullSink),
            ops: Some(ops.clone()),
        }
    }

    #[tokio::test]
    async fn app_gpu_gates_the_unload_and_passes_the_rest_through() {
        let mut reg = HostToolRegistry::new();
        register_host_tools(&mut reg);
        let ToolHandler::Async(f) = reg.get("app_gpu").unwrap().handler.clone() else {
            panic!()
        };
        let ops = Arc::new(Fake(Mutex::new(Vec::new())));
        assert_eq!(
            f(ctx(&ops), json!({"action": "status"})).await.unwrap()["state"],
            "on"
        );
        // An unload without the user's say-so never reaches the op.
        let why = f(ctx(&ops), json!({"action": "unload"}))
            .await
            .unwrap_err()
            .public;
        assert!(why.contains("confirm true"), "{why}");
        assert_eq!(
            f(ctx(&ops), json!({"action": "unload", "confirm": true}))
                .await
                .unwrap()["working"],
            true
        );
        assert_eq!(
            f(ctx(&ops), json!({"action": "reload"})).await.unwrap()["state"],
            "on"
        );
        let why = f(ctx(&ops), json!({"action": "defrost"}))
            .await
            .unwrap_err()
            .public;
        assert!(
            why.contains("status, unload, reload, hold or release"),
            "{why}"
        );
        // A hold needs a reason before it reaches the op, and passes the hours on only when given.
        let why = f(ctx(&ops), json!({"action": "hold", "reason": "  "}))
            .await
            .unwrap_err()
            .public;
        assert!(why.contains("needs a reason"), "{why}");
        assert_eq!(
            f(
                ctx(&ops),
                json!({"action": "hold", "reason": "Benchmark running"})
            )
            .await
            .unwrap()["holdReason"],
            "Benchmark running"
        );
        f(
            ctx(&ops),
            json!({"action": "hold", "reason": "Soak test", "hours": 4}),
        )
        .await
        .unwrap();
        f(ctx(&ops), json!({"action": "release"})).await.unwrap();
        let got = ops.0.lock().unwrap().clone();
        assert_eq!(
            got.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(),
            vec![
                "local_lock_state",
                "local_unload",
                "local_reload",
                "local_hold_set",
                "local_hold_set",
                "local_hold_clear"
            ]
        );
        // The hold's owner and the releasing chat ride along with the calls: the tool layer says
        // who is calling, and an agent cannot name a different chat.
        assert_eq!(
            got[3].1,
            json!({"reason": "Benchmark running", "ownerChatId": "me"})
        );
        assert_eq!(
            got[4].1,
            json!({"reason": "Soak test", "hours": 4, "ownerChatId": "me"})
        );
        assert_eq!(got[5].1, json!({"byChatId": "me"}));
    }
}
