//! `app_offer_options`: ask the user to choose with an option board in the chat.
//!
//! The tool checks the call's shape and hands it to the chats crate's `option_board_create` op,
//! which reads any option pictures into the same per-chat store `app_show_images` uses. The answer
//! is `{boardId, next}`: the card is on screen and the user's choice arrives as their next message,
//! so the agent ends its turn. The card's tap sends that message and records the choice (ops
//! `option_board_answer` and `option_board_state`).

use crate::registry::{screen_label, HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Map, Value};

pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 12;
const MAX_QUESTION: usize = 300;
const MAX_LABEL: usize = 80;
const MAX_DETAIL: usize = 300;

/// What the agent reads after the board is up.
pub const NEXT: &str = "The board is on screen. The user's choice arrives as their next message: end your turn now and wait for it.";

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "question": {"type": "string", "maxLength": MAX_QUESTION, "description": "What the user is choosing."},
            "options": {
                "type": "array",
                "minItems": MIN_OPTIONS,
                "maxItems": MAX_OPTIONS,
                "items": {
                    "type": "object",
                    "properties": {
                        "label": {"type": "string", "maxLength": MAX_LABEL, "description": "The choice's name; labels must differ."},
                        "detail": {"type": "string", "maxLength": MAX_DETAIL, "description": "A line of explanation under the label."},
                        "image": {"type": "string", "description": "Absolute path of a PNG, JPEG, GIF or WebP picture for this option."}
                    },
                    "required": ["label"],
                    "additionalProperties": false
                }
            },
            "multiSelect": {"type": "boolean", "description": "Let the user pick several; default false."},
            "allowOther": {"type": "boolean", "description": "Offer an Other... choice the user types; default true."}
        },
        "required": ["question", "options"],
        "additionalProperties": false
    })
}

/// A field of `v` that must be text when present.
fn text(v: &Value, key: &str, what: &str) -> Result<Option<String>, ToolError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ToolError::refused(format!("{what}: {key} must be text."))),
    }
}

fn flag(a: &Value, key: &str) -> Result<Option<bool>, ToolError> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(ToolError::refused(format!("{key} must be true or false."))),
    }
}

/// The op's arguments from the call's, with only the fields the tool defines.
fn op_args(chat_id: &str, a: &Value) -> Result<Value, ToolError> {
    let allowed = ["question", "options", "multiSelect", "allowOther"];
    if let Some(extra) = a
        .as_object()
        .and_then(|o| o.keys().find(|k| !allowed.contains(&k.as_str())))
    {
        return Err(ToolError::refused(format!(
            "{extra} is not a field of app_offer_options."
        )));
    }
    let question = text(a, "question", "app_offer_options")?
        .filter(|q| !q.trim().is_empty())
        .ok_or_else(|| ToolError::refused("question is required."))?;
    screen_label("The question", &question)?;
    let options = a
        .get("options")
        .and_then(Value::as_array)
        .filter(|o| (MIN_OPTIONS..=MAX_OPTIONS).contains(&o.len()))
        .ok_or_else(|| ToolError::refused("options must list 2 to 12 choices."))?;
    let mut out = Vec::with_capacity(options.len());
    for (i, opt) in options.iter().enumerate() {
        let which = format!("Option {}", i + 1);
        let label = text(opt, "label", &which)?
            .filter(|l| !l.trim().is_empty())
            .ok_or_else(|| ToolError::refused(format!("{which}: label is required.")))?;
        screen_label(&format!("{which}: label"), &label)?;
        let mut o = Map::new();
        o.insert("label".into(), json!(label));
        for key in ["detail", "image"] {
            if let Some(v) = text(opt, key, &which)?.filter(|v| !v.trim().is_empty()) {
                screen_label(&format!("{which}: {key}"), &v)?;
                o.insert(key.into(), json!(v));
            }
        }
        out.push(Value::Object(o));
    }
    let mut args = json!({"chatId": chat_id, "question": question, "options": out});
    for key in ["multiSelect", "allowOther"] {
        if let Some(b) = flag(a, key)? {
            args[key] = json!(b);
        }
    }
    Ok(args)
}

async fn offer_options(ctx: ToolCtx, a: Value) -> ToolResult {
    let args = op_args(&ctx.caller_chat_id, &a)?;
    let made = ctx.op("option_board_create", args).await?;
    Ok(json!({"boardId": made["boardId"], "next": NEXT}))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            "app_offer_options",
            "Ask the user to choose from 2-12 options on a board in the chat: tiles with an optional picture, label and detail; they tap one (or several with multiSelect) and may type their own answer unless allowOther is false. Use it when a decision is easier to make by seeing the choices (colours, layouts, names, designs). options is [{label, detail?, image?}] with unique labels; image is an absolute path of a PNG, JPEG, GIF or WebP file (25 MB each, 60 MB a board). Answers {boardId} and does not wait: the user's choice arrives as their next message, in the form \"<question>: <choice>\", so end your turn after calling it. Do not repeat the options in your reply.",
            schema(),
        )
        .large()
        .timeout_ms(60_000),
        offer_options,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The call's shape is checked before any file is touched, and only known fields go on.
    #[test]
    fn the_call_is_checked_and_trimmed_to_known_fields() {
        let opts = |n: usize| -> Vec<Value> {
            (0..n).map(|i| json!({"label": format!("L{i}")})).collect()
        };
        let bad = [
            (json!({"options": opts(2)}), "question is required"),
            (json!({"question": "Q", "options": opts(1)}), "2 to 12"),
            (json!({"question": "Q", "options": opts(13)}), "2 to 12"),
            (
                json!({"question": "Q", "options": opts(2), "x": 1}),
                "x is not a field",
            ),
            (
                json!({"question": "Q", "options": [{"label": "A"}, {"detail": "d"}]}),
                "Option 2: label is required",
            ),
            (
                json!({"question": "Q", "options": [{"label": "A"}, {"label": "B", "image": 4}]}),
                "Option 2: image must be text",
            ),
            (
                json!({"question": "Q", "options": opts(2), "multiSelect": "yes"}),
                "multiSelect must be true or false",
            ),
            (
                json!({"question": "Q\u{202e}", "options": opts(2)}),
                "question cannot hold invisible",
            ),
            (
                json!({"question": "Q", "options": [{"label": "A"}, {"label": "B\u{200b}"}]}),
                "Option 2: label cannot hold invisible",
            ),
            (
                json!({"question": "Q", "options": [{"label": "A", "detail": "d\u{2066}"}, {"label": "B"}]}),
                "Option 1: detail cannot hold invisible",
            ),
        ];
        for (a, says) in bad {
            let e = op_args("c1", &a).unwrap_err();
            assert!(e.public.contains(says), "{e:?} for {a}");
        }
        let got = op_args(
            "c1",
            &json!({"question": "Q", "multiSelect": true, "options": [
                {"label": "A", "detail": "d", "image": "C:/a.png", "extra": 1},
                {"label": "B", "detail": " "}]}),
        )
        .unwrap();
        assert_eq!(
            got,
            json!({"chatId": "c1", "question": "Q", "multiSelect": true, "options": [
                {"label": "A", "detail": "d", "image": "C:/a.png"}, {"label": "B"}]})
        );
    }
}
