//! `app_progress`: the agent declares, in one short line, what it is doing right now.
//!
//! The call changes nothing: it answers ok and that is all. The transcript recognises the call's
//! own row and shows its `text` argument as the turn's one progress line, so a progress line is
//! always a declared one: the UI never guesses at prose, and nothing is inferred from message
//! length. Because the line is transient (newer lines replace it, and it is not a message), the
//! addendum says plainly that important information never lives only there.

use crate::registry::{screen_label, HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Value};

pub const MAX_TEXT: usize = 200;

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_TEXT,
                "description": "One short line on what you are doing now, present tense."
            }
        },
        "required": ["text"],
        "additionalProperties": false
    })
}

/// The call's shape, checked before answering: one short line of visible text.
fn check(a: &Value) -> Result<(), ToolError> {
    let Some(s) = a.get("text").and_then(Value::as_str) else {
        return Err(ToolError::refused("text is required: one short line."));
    };
    if s.trim().is_empty() {
        return Err(ToolError::refused("text is required: one short line."));
    }
    screen_label("The progress line", s)
}

fn progress(_ctx: &ToolCtx, a: Value) -> ToolResult {
    check(&a)?;
    Ok(json!({ "ok": true }))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            "app_progress",
            "Declare what you are doing right now, for the one live progress line the chat shows while you work. text is one short line (present tense, e.g. \"Running the tests\"). It is transient: the next call replaces it, older lines fold away, and it is never a message — never put anything there that the reader needs to keep. Call it only at real steps, not for each tool.",
            schema(),
        ),
        progress,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check is a table: a usable line passes, an empty, missing or invisible-laden one does not.
    #[test]
    fn the_progress_line_is_one_short_visible_text() {
        assert!(check(&json!({"text": "Running the tests"})).is_ok());
        for bad in [
            json!({}),
            json!({"text": ""}),
            json!({"text": "   "}),
            json!({"text": 7}),
            json!({"text": "done\u{202e}"}),
        ] {
            assert!(check(&bad).is_err(), "{bad} must be refused");
        }
    }
}
