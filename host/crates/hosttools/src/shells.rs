//! Tool shells for attention and saved items. Both delegate to the owning crate's `api` ops, so
//! the tool and the UI see exactly the same rules.
//!
//! - Attention: `app_list_attention` reads the chats crate's attention summary; `app_update_attention`
//!   marks a chat addressed or snoozes it, through the chats crate's own ops.
//! - Saved items: the three tools work over the saved crate's ops — save, list (state, search,
//!   paging), and update or remove.

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Value};

fn open(props: Value) -> Value {
    json!({"type": "object", "properties": props, "additionalProperties": false})
}

async fn list_attention(ctx: ToolCtx, _a: Value) -> ToolResult {
    let summary = ctx.op("attention_get", json!({})).await?;
    Ok(json!({
        "attention": summary,
        "note": "Chats that need the user, as Helm's sidebar shows them.",
    }))
}

async fn update_attention(ctx: ToolCtx, a: Value) -> ToolResult {
    let chat = a
        .get("chatId")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::refused("chatId is required."))?
        .to_owned();
    match a.get("action").and_then(Value::as_str) {
        Some("address") => {
            ctx.op("attention_address", json!({"chatId": chat})).await?;
        }
        Some("snooze") => {
            let minutes = a
                .get("minutes")
                .and_then(Value::as_i64)
                .filter(|m| (1..=60 * 24 * 7).contains(m))
                .ok_or_else(|| ToolError::refused("minutes must be 1-10080 for a snooze."))?;
            let until = helm_core::now_ms() + minutes * 60_000;
            ctx.op("attention_snooze", json!({"chatId": chat, "until": until}))
                .await?;
        }
        _ => return Err(ToolError::refused("action must be address or snooze.")),
    }
    Ok(json!({"chatId": chat, "updated": true}))
}

const EXPLICIT: &str = " Act only when the user explicitly asked; report what changed.";

async fn list_saved(ctx: ToolCtx, a: Value) -> ToolResult {
    let mut args = serde_json::Map::new();
    for k in ["state", "query", "page"] {
        if let Some(v) = a.get(k) {
            args.insert(k.to_owned(), v.clone());
        }
    }
    let page = ctx.op("saved_list", Value::Object(args)).await?;
    Ok(json!({
        "items": page.get("items").cloned().unwrap_or_else(|| json!([])),
        "total": page.get("total"),
        "page": page.get("page"),
        "pageSize": page.get("pageSize"),
    }))
}

async fn save_saved(ctx: ToolCtx, a: Value) -> ToolResult {
    let title = a
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ToolError::refused("title is required."))?;
    let body = a
        .get("body")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            ToolError::refused(
                "body is required: write down the thing itself, not just where it was said.",
            )
        })?;
    let mut args = json!({"title": title, "body": body});
    for (from, to) in [
        ("messageId", "messageId"),
        ("captureKey", "captureKey"),
        ("remindAt", "remindAt"),
    ] {
        if let Some(v) = a.get(from) {
            args[to] = v.clone();
        }
    }
    // A message from this chat is deduplicated by the pair, so the chat id rides along with it.
    if args.get("messageId").is_some() {
        args["chatId"] = json!(ctx.caller_chat_id);
    }
    ctx.op("saved_save", args).await
}

async fn update_saved(ctx: ToolCtx, a: Value) -> ToolResult {
    let id = a
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::refused("id is required (from the list)."))?;
    let mut args = json!({"id": id});
    for k in ["title", "body", "state", "remindAt", "remove"] {
        if let Some(v) = a.get(k) {
            args[k] = v.clone();
        }
    }
    if args.as_object().is_some_and(|m| m.len() == 1) {
        return Err(ToolError::refused(
            "name at least one field to change, or remove: true.",
        ));
    }
    ctx.op("saved_update", args).await
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::read(
            "app_list_attention",
            "List the chats that need the user's attention (finished, waiting for an answer, failed or stalled), as Helm's sidebar shows them.",
            open(json!({})),
        )
        .self_locking(),
        list_attention,
    )
    .async_tool(
        ToolSpec::write(
            "app_update_attention",
            format!("Mark a chat's attention as addressed, or snooze it for some minutes (1-10080).{EXPLICIT}"),
            json!({
                "type": "object",
                "properties": {
                    "chatId": {"type": "string"},
                    "action": {"type": "string", "enum": ["address", "snooze"]},
                    "minutes": {"type": "integer", "minimum": 1, "maximum": 10080}
                },
                "required": ["chatId", "action"],
                "additionalProperties": false
            }),
        )
        .self_locking(),
        update_attention,
    );
    r.async_tool(
        ToolSpec::read(
            "app_list_saved_items",
            "List the items the user saved for later (newest change first, 50 per page), with their state and any reminder.",
            open(json!({
                "state": {"type": "string", "enum": ["saved", "filed", "done", "dismissed"]},
                "query": {"type": "string", "description": "A case-insensitive substring of a title, body or note."},
                "page": {"type": "integer", "minimum": 1}
            })),
        )
        .self_locking(),
        list_saved,
    )
    .async_tool(
        ToolSpec::write(
            "app_save_item",
            format!(
                "Save an item for the user to come back to: a title and the thing itself written down. \
                 Save the same transcript message (messageId, from this chat) or the same captureKey \
                 twice and the existing item is returned, not a second copy. remindAt is an epoch-ms \
                 time for a one-time reminder.{EXPLICIT}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "messageId": {"type": "integer", "description": "A row id in this chat to save; saving it again returns the item already made."},
                    "captureKey": {"type": "string", "description": "An optional stable key so the same thing is never saved twice."},
                    "remindAt": {"type": "integer", "description": "Epoch milliseconds for a one-time reminder."}
                },
                "required": ["title", "body"],
                "additionalProperties": false
            }),
        )
        .self_locking(),
        save_saved,
    )
    .async_tool(
        ToolSpec::write(
            "app_update_saved_item",
            format!(
                "Change or remove a saved item (its id comes from the list). state moves it between saved, filed, done and dismissed; \
                 set remindAt to a new epoch-ms time to arm a reminder, or to null to clear it.{EXPLICIT}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "state": {"type": "string", "enum": ["saved", "filed", "done", "dismissed"]},
                    "remindAt": {"type": ["integer", "null"], "description": "Epoch milliseconds for a reminder, or null to clear it."},
                    "remove": {"type": "boolean"}
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        )
        .self_locking(),
        update_saved,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolHandler;

    #[test]
    fn attention_and_saved_tools_are_registered_and_delegate_to_ops() {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        assert!(r.contains("app_list_attention") && r.contains("app_update_attention"));
        // The saved tools reach the saved crate through api ops: they run on the runtime, take
        // their own lock, and mark the writes as mutating. (The saved crate's own tests cover
        // what the ops do with the args.)
        for (name, mutating) in [
            ("app_list_saved_items", false),
            ("app_save_item", true),
            ("app_update_saved_item", true),
        ] {
            let t = r.get(name).unwrap();
            assert_eq!(t.spec.mutating, mutating, "{name}");
            assert!(
                t.spec.self_locking,
                "{name} calls ops that lock for themselves"
            );
            assert!(
                matches!(t.handler, ToolHandler::Async(_)),
                "{name} runs on the runtime"
            );
        }
        // A write with nothing to change is refused before any op runs.
        let ToolHandler::Async(f) = r.get("app_update_saved_item").unwrap().handler.clone() else {
            unreachable!()
        };
        let ctx = ToolCtx {
            caller_chat_id: "c".into(),
            is_child: false,
            tool_call_id: None,
            origin: crate::registry::CallOrigin::Agent,
            services: std::sync::Arc::new(helm_ipc::Services::new()),
            events: std::sync::Arc::new(helm_ipc::NullSink),
            ops: None,
        };
        // Polling the boxed future to a refusal without an executor: the no-field check runs before
        // any op call, so the first poll completes it.
        let mut fut = f(ctx, json!({"id": "x"}));
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(r) => {
                let e = r.unwrap_err();
                assert!(e.public.contains("at least one field"));
                assert_eq!(e.changed, crate::Changed::No);
            }
            std::task::Poll::Pending => panic!("the refusal should settle at once"),
        }
    }
}
