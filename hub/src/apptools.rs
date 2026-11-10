//! The Helm tools the hub offers over MCP and forwards down the relay line.
//!
//! The hub keeps its own fixed list: a name not on it is never forwarded,
//! whatever Helm would accept. Helm checks every call again on the PC (its
//! external-caller deny-list and argument refusals), so this list is the outer
//! wall, not the only one. Only clients whose token carries the `app` scope see these, and a call
//! only ever goes down a line of the caller's own person (see `server.rs`): the most recently
//! active one, or the machine named by the optional `machine` argument, which is removed before
//! the call is forwarded.

use crate::relay::{CallOutcome, Line, OFFLINE};
use serde_json::{json, Value};

/// Every tool name the hub forwards. Read-only first, then the two that act.
pub const FORWARDED: &[&str] = &[
    "app_list_chats",
    "app_read_chat",
    "app_list_projects",
    "app_list_attention",
    "app_send_message",
    "app_stop_chat",
];

/// The two tools only the Remote page may call: what waits for the user, and an answer to it.
/// They are not offered over MCP (`FORWARDED` and [`tool_list`] stay as they were), so an agent
/// cannot answer a prompt through the hub; Helm refuses them for any call the hub did not stamp
/// as coming from the page.
pub const REMOTE_ONLY: &[&str] = &["app_list_waiting", "app_answer_waiting"];

const NOTE: &str = " Runs in Helm on one of the caller's own computers through the hub. When Helm \
    is offline the call answers that nothing was sent or changed.";

pub fn tool_list() -> Vec<Value> {
    let obj = |mut props: Value, required: &[&str]| {
        props["machine"] = json!({
            "type": "string",
            "description": "Which of your computers; default the one used most recently."
        });
        json!({ "type": "object", "properties": props, "required": required })
    };
    let chat_id = json!({ "type": "string", "description": "A Helm chat id from app_list_chats." });
    vec![
        json!({
            "name": "app_list_chats",
            "description": format!("List or search Helm chats (titles, snippets, state).{NOTE}"),
            "inputSchema": obj(json!({
                "query": { "type": "string" },
                "projectId": { "type": "string" },
                "archived": { "type": "boolean" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50 },
                "offset": { "type": "integer", "minimum": 0 }
            }), &[]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "app_read_chat",
            "description": format!("Read recent messages of one Helm chat without opening it.{NOTE}"),
            "inputSchema": obj(json!({
                "chatId": chat_id,
                "beforeMessageId": { "type": "integer", "description": "From a previous answer, for older messages." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
            }), &["chatId"]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "app_list_projects",
            "description": format!("List Helm projects.{NOTE}"),
            "inputSchema": obj(json!({ "query": { "type": "string" } }), &[]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "app_list_attention",
            "description": format!("List the Helm chats that need the user's attention.{NOTE}"),
            "inputSchema": obj(json!({}), &[]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "app_send_message",
            "description": format!("Send a message to a Helm chat. It arrives framed as sent by \
                another agent, never as the user and never as approval. mode is steer or queue; queue now means \nsteer: the message is delivered into the running turn at the next tool boundary.{NOTE}"),
            "inputSchema": obj(json!({
                "chatId": chat_id,
                "message": { "type": "string" },
                "mode": { "type": "string", "enum": ["steer", "queue"] }
            }), &["chatId", "message", "mode"])
        }),
        json!({
            "name": "app_stop_chat",
            "description": format!("Stop a Helm chat's running turn, like its Stop button.{NOTE}"),
            "inputSchema": obj(json!({ "chatId": chat_id }), &["chatId"]),
            "annotations": { "destructiveHint": true }
        }),
    ]
}

pub fn is_forwarded(name: &str) -> bool {
    FORWARDED.contains(&name)
}

/// Whether the Remote page may call `name`: a forwarded tool, or one of the page's own two.
pub fn is_remote_callable(name: &str) -> bool {
    is_forwarded(name) || REMOTE_ONLY.contains(&name)
}

/// Run one forwarded call and shape the MCP tool result.
pub async fn call(line: &Line, client: &str, name: &str, args: &Value) -> Value {
    if !is_forwarded(name) {
        return error(format!("unknown tool {name:?}"));
    }
    if !(args.is_object() || args.is_null()) {
        return error("arguments must be an object".to_owned());
    }
    let args = if args.is_null() {
        json!({})
    } else {
        args.clone()
    };
    match line.call(client, name, &args).await {
        CallOutcome::Ok(v) => {
            // structuredContent must be an object.
            let v = if v.is_object() {
                v
            } else {
                json!({ "result": v })
            };
            json!({
            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
            "structuredContent": v,
            "isError": false
            })
        }
        CallOutcome::Err { text, changed } => {
            let text = if changed {
                text
            } else {
                format!("{text} Nothing was changed.")
            };
            error(text)
        }
        CallOutcome::Offline => error(OFFLINE.to_owned()),
        CallOutcome::Unknown(text) => error(text),
    }
}

/// The answer when the caller has no connected machine to run the call on.
pub fn offline_result() -> Value {
    error(OFFLINE.to_owned())
}

fn error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}
