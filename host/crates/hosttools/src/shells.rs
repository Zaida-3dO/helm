//! Tool shells for attention, saved items and schedules.
//!
//! - Attention: `app_list_attention` reads the chats crate's attention summary; `app_update_attention`
//!   marks a chat addressed or snoozes it, through the chats crate's own ops.
//! - Saved items and schedules are not built yet. Their tools are registered so agents
//!   see the shape, and each answers a fixed refusal that names when the feature comes and that
//!   nothing was changed (the readiness rule: every refusal for an unavailable feature says why
//!   and where the user acts).

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Value};

pub const SCHEDULES_LATER: &str = "Schedules are not available in Helm yet; nothing was created or changed. Ask the user to set a reminder outside Helm for now.";
pub const SAVED_LATER: &str = "Saved items are not built in Helm yet; nothing was saved or changed. Tell the user what you would have saved instead.";

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

fn refuse(msg: &'static str) -> impl Fn(&ToolCtx, Value) -> ToolResult + Send + Sync + 'static {
    move |_, _| Err(ToolError::refused(msg))
}

const EXPLICIT: &str = " Act only when the user explicitly asked; report what changed.";

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
    let saved = [
        (
            "app_list_saved_items",
            "List the items the user saved for later.",
            false,
        ),
        (
            "app_save_item",
            "Save an item for the user to come back to.",
            true,
        ),
        (
            "app_update_saved_item",
            "Change or remove a saved item.",
            true,
        ),
    ];
    for (name, desc, write) in saved {
        let spec = if write {
            ToolSpec::write(
                name,
                format!("{desc} Not built yet: it answers why.{EXPLICIT}"),
                open(json!({})),
            )
        } else {
            ToolSpec::read(
                name,
                format!("{desc} Not built yet: it answers why."),
                open(json!({})),
            )
        };
        r.blocking(spec, refuse(SAVED_LATER));
    }
    let schedules = [
        ("app_list_schedules", "List the user's schedules.", false),
        (
            "app_create_schedule",
            "Create a schedule that sends a message to a chat later.",
            true,
        ),
        ("app_update_schedule", "Change a schedule.", true),
        ("app_cancel_schedule", "Cancel a schedule.", true),
    ];
    for (name, desc, write) in schedules {
        let spec = if write {
            ToolSpec::write(
                name,
                format!("{desc} Not built yet: it answers why.{EXPLICIT}"),
                open(json!({})),
            )
            .large()
        } else {
            ToolSpec::read(
                name,
                format!("{desc} Not built yet: it answers why."),
                open(json!({})),
            )
        };
        r.blocking(spec, refuse(SCHEDULES_LATER));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolHandler;

    #[test]
    fn shells_answer_why_and_change_nothing() {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let ctx = ToolCtx {
            caller_chat_id: "c".into(),
            is_child: false,
            tool_call_id: None,
            origin: crate::registry::CallOrigin::Agent,
            services: std::sync::Arc::new(helm_ipc::Services::new()),
            events: std::sync::Arc::new(helm_ipc::NullSink),
            ops: None,
        };
        for (name, why) in [
            ("app_create_schedule", "not available"),
            ("app_save_item", "not built"),
        ] {
            let ToolHandler::Blocking(f) = r.get(name).unwrap().handler.clone() else {
                panic!()
            };
            let e = f(&ctx, json!({})).unwrap_err();
            assert!(
                e.public.contains(why) && e.public.contains("nothing was"),
                "{name}: {}",
                e.public
            );
            assert_eq!(e.changed, crate::Changed::No);
        }
        assert!(r.contains("app_list_attention") && r.contains("app_update_attention"));
    }
}
