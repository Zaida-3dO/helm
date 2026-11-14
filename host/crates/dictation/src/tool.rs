//! `app_make_room_for_speech`: once the hub's way to ask the PC to free graphics memory before it
//! transcribed. Speech-to-text no longer unloads local models, so the tool does nothing and says
//! so at once (`{freedGb: 0, unloaded: 0}`). It stays registered so a hub that still asks gets a
//! quick answer rather than an unknown-tool error.
//!
//! The tool is unlisted (no chat is offered it) and answers only a caller that came down the hub
//! line.

use helm_hosttools::{Condition, HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use serde_json::{json, Value};

/// Callers that came in from outside Helm over the hub line are named like this.
const HUB_CALLER_PREFIX: &str = "external:";

fn make_room(ctx: &ToolCtx, _args: Value) -> ToolResult {
    if !ctx.caller_chat_id.starts_with(HUB_CALLER_PREFIX) {
        return Err(ToolError::refused(
            "app_make_room_for_speech is for the Helm hub only.",
        ));
    }
    Ok(json!({"freedGb": 0.0, "unloaded": 0}))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            "app_make_room_for_speech",
            "Kept for Helm hubs that ask before they transcribe. Speech-to-text no longer unloads \
             local models, so it unloads nothing and answers at once; chats never see it.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        )
        .when(Condition::Worker)
        .timeout_ms(30_000),
        make_room,
    );
}
