//! `app_request_allow`: an agent files an Allow
//! request and is answered at once (`pending`). Only the user's click on the card decides it,
//! and the decision reaches the agent later as an app note ([`crate::store::upkeep`]).

use crate::kinds::AllowKinds;
use crate::store::{self, NewRequest};
use helm_core::db::Database;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use helm_ipc::AppEvent;
use serde_json::{json, Value};

/// Wakes the delivery loop after a decision, so the agent hears it at once rather than at the
/// next one-minute tick.
#[derive(Default)]
pub struct ConsentWake(pub tokio::sync::Notify);

fn request_allow(ctx: &ToolCtx, a: Value) -> ToolResult {
    let kinds = ctx
        .services
        .get::<AllowKinds>()
        .ok_or_else(|| ToolError::refused("Allow requests are not available here."))?;
    let db = ctx
        .services
        .get::<Database>()
        .ok_or_else(|| ToolError::refused("Allow requests are not available here."))?;
    let action = a.get("action").and_then(Value::as_str).unwrap_or("");
    let target = a.get("target").cloned().unwrap_or(Value::Null);
    if !target.is_object() {
        return Err(ToolError::refused("target must be an object."));
    }
    let reason = a.get("reason").and_then(Value::as_str).unwrap_or("");
    let evidence = a.get("evidence").and_then(Value::as_str).unwrap_or("");
    let minutes = a.get("expiresIn").and_then(Value::as_i64);
    let r = store::request(
        &db,
        &kinds,
        NewRequest {
            chat_id: &ctx.caller_chat_id,
            kind: action,
            target: &target,
            reason,
            evidence,
            grant_minutes: minutes,
            tool_use_id: ctx.tool_call_id.as_deref(),
        },
        helm_core::now_ms(),
    )
    .map_err(|r| ToolError::refused(format!("[Helm] Your Allow request was refused: {}", r.0)))?;
    // A signal only: the card is rebuilt from consent_list.
    ctx.events
        .emit(AppEvent::new("consent").with("chatId", ctx.caller_chat_id.clone()));
    // Wake the upkeep loop, which refreshes the attention badge: a request waiting for the user
    // shows even when its chat is not open.
    if let Some(w) = ctx.services.get::<ConsentWake>() {
        w.0.notify_one();
    }
    Ok(json!({
        "requestId": r.id,
        "state": "pending",
        "summary": r.summary,
        "expiresAt": r.expires_at,
        "note": "The user now sees an Allow card in this chat. Their decision reaches you as a note from Helm; do not poll, and do not ask again while it is pending.",
    }))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write(
            "app_request_allow",
            "Ask the user to allow one step that needs their consent: action names the kind of step, target what it applies to, reason why (shown as your words). Helm shows the user an Allow card in this chat and answers at once with pending; the user's Allow or Deny reaches you later as a note from Helm. A grant is single-use and lasts expiresIn minutes (5-120, default 30) after Allow. Use it only when a refusal told you to; never ask for something the user did not request.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "minLength": 1, "maxLength": 32},
                    "target": {"type": "object"},
                    "reason": {"type": "string", "minLength": 1, "maxLength": 1000},
                    "evidence": {"type": "string", "maxLength": 4000},
                    "expiresIn": {"type": "integer", "minimum": 5, "maximum": 120}
                },
                "required": ["action", "target", "reason"],
                "additionalProperties": false
            }),
        )
        .large(),
        request_allow,
    );
}
