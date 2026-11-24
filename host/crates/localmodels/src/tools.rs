//! `app_gpu`: the graphics card as an agent tool — read the card's lock (status), unload the
//! local models to free the card (unload), and bring them back (reload).
//!
//! The three actions are exactly the ops the Settings > Local models card uses, so the tool adds
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
        other => Err(ToolError::refused(format!(
            "\"{other}\" is not an action: use status, unload or reload."
        ))),
    }
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            "app_gpu",
            "Share the graphics card with the local model server: read the lock (status), unload \
             the loaded models to free the card (unload), or bring them back (reload).",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["status", "unload", "reload"]},
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
        assert_eq!(f(ctx(&ops), json!({"action": "status"})).await.unwrap()["state"], "on");
        // An unload without the user's say-so never reaches the op.
        let why = f(ctx(&ops), json!({"action": "unload"})).await.unwrap_err().public;
        assert!(why.contains("confirm true"), "{why}");
        assert_eq!(
            f(ctx(&ops), json!({"action": "unload", "confirm": true}))
                .await
                .unwrap()["working"],
            true
        );
        assert_eq!(f(ctx(&ops), json!({"action": "reload"})).await.unwrap()["state"], "on");
        let why = f(ctx(&ops), json!({"action": "defrost"})).await.unwrap_err().public;
        assert!(why.contains("status, unload or reload"), "{why}");
        let got = ops.0.lock().unwrap().clone();
        assert_eq!(
            got.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(),
            vec!["local_lock_state", "local_unload", "local_reload"]
        );
    }
}
