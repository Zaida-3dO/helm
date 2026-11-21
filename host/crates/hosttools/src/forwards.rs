//! `app_forward_port`: let the user open a local web server of this PC from the PC they type on.
//!
//! The tool asks the devices layer to offer the port for the calling chat
//! (`devices_forward_offer`); every check (the port policy, something answering on it, the switch
//! that lets other PCs use this one's chats) is the op's. The offer reaches the other PC as this
//! tool's result: the window showing the chat there opens the forward by itself, on that PC's own
//! loopback address. When the user types on this PC, nothing needs forwarding and the answer
//! says so.

use crate::registry::{screen_label, HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Value};

/// The longest title taken for a forward.
const MAX_TITLE: usize = 80;

async fn forward_port(ctx: ToolCtx, a: Value) -> ToolResult {
    let port = match a.get("port").and_then(Value::as_u64) {
        Some(p) if (1..=65_535).contains(&p) => p,
        _ => {
            return Err(ToolError::refused(
                "port must be a whole number from 1 to 65535.",
            ))
        }
    };
    let mut args = json!({"chatId": ctx.caller_chat_id, "port": port});
    match a.get("title") {
        None | Some(Value::Null) => {}
        Some(Value::String(t)) if !t.trim().is_empty() => {
            let t = t.trim();
            if t.chars().count() > MAX_TITLE {
                return Err(ToolError::refused(format!(
                    "The title is at most {MAX_TITLE} characters."
                )));
            }
            screen_label("The title", t)?;
            args["title"] = json!(t);
        }
        Some(Value::String(_)) => {}
        Some(_) => return Err(ToolError::refused("title must be a string.")),
    }
    let r = ctx.op("devices_forward_offer", args).await?;
    let port = r["port"].as_u64().unwrap_or(port);
    let typed_on = r["typedOn"]["name"].as_str().map(str::to_owned);
    let same_pc = r["samePc"] == json!(true);
    let note = if same_pc {
        format!(
            "The user is on this PC: http://localhost:{port} works as is, so nothing was forwarded. Give them that address."
        )
    } else {
        let whom = typed_on
            .as_deref()
            .map(|n| format!("on {n}"))
            .unwrap_or_else(|| "on the PC they view this chat from".to_owned());
        format!(
            "Offered to the user {whom}: it opens there by itself as http://localhost:{port} while they view this chat (another port if that one is taken there; Helm tells them which). They can stop it from the chat's bar."
        )
    };
    Ok(json!({
        "forward": {"port": port, "title": r["title"]},
        "typedOn": typed_on,
        "samePc": same_pc,
        "note": note,
    }))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            "app_forward_port",
            "Let the user open a web server running on this PC (a dev server on localhost) from the other PC they are typing on: give the port and optionally a short title. Their messages start with a [Typed on <PC>] line when they type on another PC. Only offer one when they work from another PC or ask for it; on this PC, localhost works as is.",
            json!({
                "type": "object",
                "properties": {
                    "port": {"type": "integer", "minimum": 1, "maximum": 65535, "description": "The local port the server listens on."},
                    "title": {"type": "string", "description": "A short name for the forward, such as the app's name."}
                },
                "required": ["port"],
                "additionalProperties": false
            }),
        )
        .self_locking(),
        forward_port,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{CallOrigin, OpInvoker, ToolHandler};
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
            let title = args["title"].clone();
            let out = match args["port"].as_u64() {
                Some(3389) => Err(helm_ipc::ApiError::new(
                    "refused",
                    "Remote desktop ports are never forwarded.",
                )),
                Some(3000) => Ok(json!({
                    "port": 3000, "title": title, "typedOn": null, "samePc": true
                })),
                Some(p) => Ok(json!({
                    "port": p,
                    "title": title,
                    "typedOn": {"deviceUid": "d-2", "name": "Laptop"},
                    "samePc": false
                })),
                None => Err(helm_ipc::ApiError::bad_args("port")),
            };
            Box::pin(async move { out })
        }
    }

    async fn call(fake: &Arc<Fake>, args: Value) -> ToolResult {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let ToolHandler::Async(f) = r.get("app_forward_port").unwrap().handler.clone() else {
            panic!()
        };
        f(
            ToolCtx {
                caller_chat_id: "me".into(),
                is_child: false,
                tool_call_id: Some("tc1".into()),
                origin: CallOrigin::Agent,
                services: Arc::new(helm_ipc::Services::new()),
                events: Arc::new(helm_ipc::NullSink),
                ops: Some(fake.clone()),
            },
            args,
        )
        .await
    }

    #[tokio::test]
    async fn the_offer_goes_to_the_op_for_this_chat_and_its_answer_or_refusal_comes_back() {
        let fake = Arc::new(Fake(Mutex::new(vec![])));

        // Offered to the PC the user types on: the op gets this chat, the port and the title
        // (never a chat the agent names), and the answer names that PC.
        let r = call(
            &fake,
            json!({"port": 5173, "title": "Shop preview", "chatId": "other"}),
        )
        .await
        .unwrap();
        assert_eq!(
            fake.0.lock().unwrap().last().cloned().unwrap(),
            (
                "devices_forward_offer".to_owned(),
                json!({"chatId": "me", "port": 5173, "title": "Shop preview"})
            )
        );
        assert_eq!(r["forward"], json!({"port": 5173, "title": "Shop preview"}));
        assert_eq!(r["typedOn"], "Laptop");
        assert_eq!(r["samePc"], false);
        assert!(r["note"].as_str().unwrap().contains("on Laptop"));

        // The user is on this PC: localhost works as is.
        let r = call(&fake, json!({"port": 3000})).await.unwrap();
        assert_eq!(r["samePc"], true);
        assert_eq!(r["typedOn"], Value::Null);
        assert!(r["note"]
            .as_str()
            .unwrap()
            .contains("http://localhost:3000 works as is"));

        // The op's refusal reaches the agent, and nothing changed.
        let e = call(&fake, json!({"port": 3389})).await.unwrap_err();
        assert!(e.public.contains("Remote desktop"));
        assert_eq!(e.changed, crate::Changed::No);

        // Bad arguments are refused before the op runs.
        let before = fake.0.lock().unwrap().len();
        for args in [
            json!({}),
            json!({"port": 0}),
            json!({"port": 70000}),
            json!({"port": "5173"}),
            json!({"port": 5173, "title": "x".repeat(81)}),
            json!({"port": 5173, "title": "a\u{202e}b"}),
            json!({"port": 5173, "title": 5}),
        ] {
            assert!(call(&fake, args.clone()).await.is_err(), "{args}");
        }
        assert_eq!(fake.0.lock().unwrap().len(), before);
    }
}
