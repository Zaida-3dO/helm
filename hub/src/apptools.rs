//! The Helm tools the hub offers over MCP and forwards down the relay line.
//!
//! The hub keeps its own fixed list: a name not on it is never forwarded,
//! whatever Helm would accept. Helm checks every call again on the PC (its
//! external-caller deny-list and argument refusals), so this list is the outer
//! wall, not the only one. Only clients whose token carries `+app` see these.

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

const NOTE: &str = " Runs in Helm on the user's PC through the hub. When Helm is offline the \
    call answers that nothing was sent or changed.";

pub fn tool_list() -> Vec<Value> {
    let obj = |props: Value, required: &[&str]| json!({ "type": "object", "properties": props, "required": required });
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

fn error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}
