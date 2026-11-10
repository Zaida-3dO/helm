//! `app_make_room_for_speech`: the hub asks the PC to free graphics memory before it transcribes.
//!
//! The tool is unlisted (no chat is offered it) and answers only a caller that came down the hub
//! line. It unloads idle local models and leaves any model that is mid-reply alone, so it can
//! only ever cost a model its warm start.

use helm_hosttools::{Condition, HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use helm_localmodels::LocalService;
use serde_json::{json, Value};

/// Callers that came in from outside Helm over the hub line are named like this.
const HUB_CALLER_PREFIX: &str = "external:";

fn make_room(ctx: &ToolCtx, _args: Value) -> ToolResult {
    if !ctx.caller_chat_id.starts_with(HUB_CALLER_PREFIX) {
        return Err(ToolError::refused(
            "app_make_room_for_speech is for the Helm hub only.",
        ));
    }
    // Without local models there is nothing to free.
    let Some(local) = ctx.services.get::<LocalService>() else {
        return Ok(json!({"freedGb": 0.0, "unloaded": 0}));
    };
    let room = local.free_for_speech();
    Ok(json!({"freedGb": room.freed_gb, "unloaded": room.unloaded}))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            "app_make_room_for_speech",
            "Frees graphics memory for speech-to-text by unloading idle local models. The Helm \
             hub asks it just before it transcribes; chats never see it.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        )
        .when(Condition::Worker)
        // Unloads can take a while on a busy card; the hub gives up sooner and goes ahead.
        .timeout_ms(30_000),
        make_room,
    );
}
