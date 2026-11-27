//! `app_forward_port`: let the user open a local web server of this PC from the PC they type on.
//!
//! The tool asks the devices layer to offer the port for the calling chat
//! (`devices_forward_offer`); every check (the port policy, something answering on it, the switch
//! that lets other PCs use this one's chats) is the op's. The offer reaches the other PC as this
//! tool's result: the window showing the chat there opens the forward by itself, on that PC's own
//! loopback address. When the user types on this PC, nothing needs forwarding and the answer
//! says so.
//!
//! `app_list_forwards` answers what is forwarded now: every port forwarded between PCs of the
//! household, whoever it belongs to, with the PC and port of the server, the PC and address it
//! opens on, and whether the two ends agree and answer (`devices_forwards_report`). The tool adds
//! a note that names the next call for any forward that is wrong.

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

/// The longest PC name or chat id taken as a filter.
const MAX_FILTER: usize = 120;

/// A filter argument: absent, null or blank means none.
fn filter(a: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) if t.trim().is_empty() => Ok(None),
        Some(Value::String(t)) if t.trim().chars().count() <= MAX_FILTER => {
            Ok(Some(t.trim().to_owned()))
        }
        Some(Value::String(_)) => Err(ToolError::refused(format!(
            "{key} is at most {MAX_FILTER} characters."
        ))),
        Some(_) => Err(ToolError::refused(format!("{key} must be a string."))),
    }
}

/// What to do about each forward that is not `ok`, and about PCs that could not be checked.
fn forwards_note(r: &Value, narrowed: bool) -> String {
    let list = |k: &str| r[k].as_array().cloned().unwrap_or_default();
    let (forwards, unreachable) = (list("forwards"), list("unreachable"));
    let mut lines: Vec<String> = Vec::new();
    for f in &forwards {
        let (src, dst) = (&f["source"]["address"], &f["destination"]["address"]);
        let (src, dst) = (src.as_str().unwrap_or("?"), dst.as_str().unwrap_or("?"));
        let why = f["why"].as_str().unwrap_or_default();
        let next = match f["state"].as_str() {
            Some("ok") => continue,
            Some("source_not_answering") => "Start the server again; if it now listens on another port, close this forward from the chat bar and offer the right port with app_forward_port.",
            Some("destination_not_listening") => "Close it from the chat bar and offer it again with app_forward_port.",
            Some("mismatch") => "Close it from the chat bar and offer it again with app_forward_port.",
            _ => "Ask again in a moment, or offer it again with app_forward_port.",
        };
        lines.push(format!("{src} -> {dst}: {why} {next}"));
    }
    let mut note = if forwards.is_empty() {
        let none = if narrowed {
            "No forward matches."
        } else {
            "No ports are forwarded between PCs right now."
        };
        format!("{none} To let the user open a server of this PC from another one, offer it with app_forward_port {{port}}.")
    } else if lines.is_empty() {
        format!(
            "{} forward{} active and every end answers.",
            forwards.len(),
            if forwards.len() == 1 { " is" } else { "s are" }
        )
    } else {
        lines.join(" ")
    };
    if !unreachable.is_empty() {
        let who: Vec<String> = unreachable
            .iter()
            .map(|u| {
                format!(
                    "{} ({})",
                    u["name"].as_str().unwrap_or("a PC"),
                    u["reason"].as_str().unwrap_or("not reachable")
                )
            })
            .collect();
        note.push_str(&format!(
            " Not checked, so their forwards are not listed: {}.",
            who.join("; ")
        ));
    }
    note
}

async fn list_forwards(ctx: ToolCtx, a: Value) -> ToolResult {
    let mut args = json!({});
    if let Some(d) = filter(&a, "device")? {
        args["device"] = json!(d);
    }
    if let Some(c) = filter(&a, "chatId")? {
        args["chatId"] = json!(c);
    }
    let narrowed = args.as_object().is_some_and(|o| !o.is_empty());
    let mut r = ctx.op("devices_forwards_report", args).await?;
    r["note"] = json!(forwards_note(&r, narrowed));
    Ok(r)
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            "app_forward_port",
            "Let the user open a web server running on the chat's home PC (this PC — a dev server on localhost) from their seat, the other PC they are typing on: give the port and optionally a short title. Their messages start with a [Typed on <PC>] line when they type from another PC. Only offer one when they work from another PC or ask for it; on this PC, localhost works as is.",
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
    r.async_tool(
        ToolSpec::read(
            "app_list_forwards",
            "List the ports forwarded between the PCs of the household right now, for every person and every PC: for each, the server's PC and port (source), the PC and address it opens on (destination), the chat and person it belongs to, and a state saying whether it points at the right place (ok, source_not_answering, destination_not_listening, mismatch or unknown). A PC that is offline is listed under unreachable. Use it to check a forward you offered with app_forward_port, or when the user says one does not work. Optional device (a PC's name) or chatId narrows the list.",
            json!({
                "type": "object",
                "properties": {
                    "device": {"type": "string", "description": "Only forwards with this PC at either end (its name)."},
                    "chatId": {"type": "string", "description": "Only the forwards of this chat."}
                },
                "additionalProperties": false
            }),
        )
        .timeout_ms(40_000),
        list_forwards,
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
            if op == "devices_forwards_report" {
                let forward = |state: &str, why: Value| {
                    json!({
                        "state": state, "why": why,
                        "source": {"address": "localhost:5173 on Desk"},
                        "destination": {"address": "localhost:5173 on Laptop"},
                    })
                };
                let out = match args["device"].as_str() {
                    Some("Nobody") => {
                        Err(helm_ipc::ApiError::new("not_found", "No PC called Nobody."))
                    }
                    Some("Empty") => Ok(json!({"forwards": [], "unreachable": []})),
                    Some("Quiet") => Ok(json!({
                        "forwards": [forward("ok", Value::Null)], "unreachable": []
                    })),
                    _ => Ok(json!({
                        "forwards": [
                            forward("ok", Value::Null),
                            forward("source_not_answering", json!("Nothing answers on port 5173 on Desk.")),
                            forward("mismatch", json!("Laptop does not list it.")),
                        ],
                        "unreachable": [{"name": "Tablet", "reason": "Tablet is offline."}],
                    })),
                };
                return Box::pin(async move { out });
            }
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
        call_tool(fake, "app_forward_port", args).await
    }

    async fn call_tool(fake: &Arc<Fake>, tool: &str, args: Value) -> ToolResult {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let ToolHandler::Async(f) = r.get(tool).unwrap().handler.clone() else {
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

    #[tokio::test]
    async fn the_forwards_list_asks_the_op_with_clean_filters_and_names_the_next_call() {
        let fake = Arc::new(Fake(Mutex::new(vec![])));
        let list = |args: Value| {
            let fake = fake.clone();
            async move { call_tool(&fake, "app_list_forwards", args).await }
        };

        // No arguments, blank ones and nulls ask for everything; real ones are trimmed.
        for args in [
            json!({}),
            json!({"device": "  ", "chatId": null}),
            json!({"device": "Desk ", "chatId": " c1"}),
        ] {
            list(args).await.unwrap();
        }
        let sent: Vec<Value> = fake.0.lock().unwrap().iter().map(|c| c.1.clone()).collect();
        assert_eq!(
            sent,
            [
                json!({}),
                json!({}),
                json!({"device": "Desk", "chatId": "c1"})
            ]
        );
        assert!(fake
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.0 == "devices_forwards_report"));

        // Every wrong end is named with the call to make; a PC that could not be asked is named.
        let r = list(json!({})).await.unwrap();
        assert_eq!(r["forwards"].as_array().unwrap().len(), 3);
        let note = r["note"].as_str().unwrap();
        assert!(note.contains("Nothing answers on port 5173 on Desk"));
        assert!(note.contains("Laptop does not list it"));
        assert!(note.contains("app_forward_port"));
        assert!(note.contains("Tablet (Tablet is offline.)"));

        // Nothing wrong, and nothing there: each says so, the empty one with how to offer a port.
        let r = list(json!({"device": "Quiet"})).await.unwrap();
        assert!(r["note"].as_str().unwrap().contains("every end answers"));
        let r = list(json!({"device": "Empty"})).await.unwrap();
        let note = r["note"].as_str().unwrap();
        assert!(note.contains("No forward matches") && note.contains("app_forward_port {port}"));

        // The op's refusal reaches the agent; bad arguments never reach the op.
        let e = list(json!({"device": "Nobody"})).await.unwrap_err();
        assert!(e.public.contains("No PC called Nobody"));
        let before = fake.0.lock().unwrap().len();
        for args in [
            json!({"device": 5}),
            json!({"chatId": ["c1"]}),
            json!({"device": "x".repeat(121)}),
        ] {
            assert!(list(args.clone()).await.is_err(), "{args}");
        }
        assert_eq!(fake.0.lock().unwrap().len(), before);
    }
}
