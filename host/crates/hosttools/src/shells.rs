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
    let items = page
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (items, omitted) = fit_items(items);
    let mut out = json!({
        "items": items,
        "total": page.get("total"),
        "page": page.get("page"),
        "pageSize": page.get("pageSize"),
    });
    if omitted > 0 {
        out["omitted"] = json!(omitted);
        out["note"] = json!(
            "Some items on this page were left out to fit the size limit; narrow the list with state or query."
        );
    }
    Ok(out)
}

/// What the list may carry, a little under the size every tool result is held to.
const LIST_BUDGET_BYTES: usize = 40 * 1024;

/// A body is shown cut to this many characters (the full text is one the user can read in the
/// Saved pane); it shrinks further when a page would still be too big.
const BODY_PREVIEW_CHARS: [usize; 4] = [240, 120, 40, 0];

/// Fit a page of saved items into [`LIST_BUDGET_BYTES`]: bodies are cut to a short preview
/// (`bodyCut: true` marks one that was), the preview shrinks while the page is still too big, and
/// as a last resort trailing items are left out and counted. Full bodies of up to 16 KiB each
/// would otherwise make a normal page of real notes a refusal.
fn fit_items(items: Vec<Value>) -> (Vec<Value>, usize) {
    let size = |v: &[Value]| serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX);
    let mut fitted = items.clone();
    for keep in BODY_PREVIEW_CHARS {
        fitted = items
            .iter()
            .map(|it| {
                let mut it = it.clone();
                if let Some(body) = it.get("body").and_then(Value::as_str) {
                    if body.chars().count() > keep {
                        let cut: String = body.chars().take(keep).collect();
                        it["body"] = json!(cut);
                        it["bodyCut"] = json!(true);
                    }
                }
                it
            })
            .collect();
        if size(&fitted) <= LIST_BUDGET_BYTES {
            return (fitted, 0);
        }
    }
    let total = fitted.len();
    while size(&fitted) > LIST_BUDGET_BYTES && fitted.pop().is_some() {}
    let left = total - fitted.len();
    (fitted, left)
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
    // Remembered so that only this chat can remove the item later.
    args["savedBy"] = json!(ctx.caller_chat_id);
    ctx.op("saved_save", args).await
}

/// The `saved_update` args for a chat's update: the named fields, and for a removal the calling
/// chat, so the op lets it remove only an item it saved itself (the user may remove any).
fn saved_update_args(chat_id: &str, a: &Value) -> Result<Value, ToolError> {
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
    if args.get("remove").and_then(Value::as_bool) == Some(true) {
        args["byChat"] = json!(chat_id);
    }
    Ok(args)
}

async fn update_saved(ctx: ToolCtx, a: Value) -> ToolResult {
    let args = saved_update_args(&ctx.caller_chat_id, &a)?;
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
            "List the items the user saved for later (newest change first, 50 per page), with their state and any reminder. A long body is shown cut (bodyCut: true); the full text is in the Saved pane.",
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
                 set remindAt to a new epoch-ms time (from a day ago to five years ahead) to arm a reminder, or to null to clear it. \
                 remove works only on items this chat saved itself; the user removes the rest.{EXPLICIT}"
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

    /// A page of real saved notes (long bodies, many items) still fits the result limit: bodies
    /// shrink to previews and, past that, trailing items are counted out.
    #[test]
    fn a_page_of_long_saved_items_fits_the_result_limit() {
        let item = |n: usize, body_chars: usize| {
            json!({"id": format!("id-{n}"), "title": "t".repeat(150), "state": "saved",
                   "body": "b".repeat(body_chars), "noteCount": 0})
        };
        for (items, body_chars, omitted, cut) in [
            (50, 100, 0, false),
            (50, 16_000, 0, true),
            (4, 16_000, 0, true),
        ] {
            let page: Vec<Value> = (0..items).map(|n| item(n, body_chars)).collect();
            let (fit, left) = fit_items(page);
            assert_eq!(left, omitted);
            assert_eq!(fit.len(), items - omitted);
            assert!(serde_json::to_vec(&fit).unwrap().len() <= LIST_BUDGET_BYTES);
            assert_eq!(
                fit[0].get("bodyCut").is_some(),
                cut,
                "{items} x {body_chars}"
            );
        }
        // Titles alone past the budget: the tail is counted out, not refused.
        let heavy: Vec<Value> = (0..50)
            .map(|n| json!({"id": format!("id-{n}"), "title": "字".repeat(1500), "body": "x"}))
            .collect();
        let (fit, left) = fit_items(heavy);
        assert!(left > 0 && fit.len() + left == 50);
    }

    /// A chat's update names only the fields it was given; a removal also names the chat, which is
    /// how the op limits a chat to the items it saved.
    #[test]
    fn a_saved_update_names_the_calling_chat_only_for_a_removal() {
        for (given, want) in [
            (
                json!({"id": "x", "state": "done"}),
                Some(json!({"id": "x", "state": "done"})),
            ),
            (
                json!({"id": "x", "remove": true}),
                Some(json!({"id": "x", "remove": true, "byChat": "chat-1"})),
            ),
            (
                json!({"id": "x", "remove": false}),
                Some(json!({"id": "x", "remove": false})),
            ),
            // A chat cannot name itself: the caller field is the host's alone.
            (
                json!({"id": "x", "remove": true, "byChat": "other"}),
                Some(json!({"id": "x", "remove": true, "byChat": "chat-1"})),
            ),
            (json!({"id": "x"}), None),
            (json!({"id": "", "remove": true}), None),
        ] {
            let got = saved_update_args("chat-1", &given).ok();
            assert_eq!(got, want, "{given}");
        }
    }

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
