//! `app_show_images`: put pictures in front of the user as a gallery card in the chat.
//!
//! The tool checks the call's shape and hands the paths to the chats crate's `shown_images_add`
//! op, which reads the files under the local-image rules and keeps a copy of the bytes with the
//! chat, so the card outlives the files. The answer is `{galleryId, count}`; the transcript's
//! gallery card reads the rest from the op `shown_gallery_get`.

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Map, Value};

pub const MAX_IMAGES: usize = 20;
const MAX_TITLE: usize = 120;
const MAX_CAPTION: usize = 300;

/// The lines about the card tools and the checklist that join the prompt addendum.
pub const CARDS_ADDENDUM: &str = "Showing things: to show the user pictures (screenshots, mockups, designs to compare), call app_show_images with the file paths so they appear as a gallery in the chat; never answer with a list of file paths. When the user should choose between options, call app_offer_options (give an option a picture where it helps), then end your turn: their choice arrives as their next message. For work with three or more steps, keep a checklist with the todo tool and keep it current as you go.";

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "maxLength": MAX_TITLE, "description": "A short heading for the gallery."},
            "images": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_IMAGES,
                "description": "The pictures, in the order to show them.",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path of a PNG, JPEG, GIF or WebP file."},
                        "caption": {"type": "string", "maxLength": MAX_CAPTION, "description": "Shown under the picture."}
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["images"],
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

/// The op's arguments from the call's, with only the fields the tool defines.
fn op_args(chat_id: &str, a: &Value) -> Result<Value, ToolError> {
    let allowed = ["title", "images"];
    if let Some(extra) = a
        .as_object()
        .and_then(|o| o.keys().find(|k| !allowed.contains(&k.as_str())))
    {
        return Err(ToolError::refused(format!(
            "{extra} is not a field of app_show_images."
        )));
    }
    let images = a
        .get("images")
        .and_then(Value::as_array)
        .filter(|i| (1..=MAX_IMAGES).contains(&i.len()))
        .ok_or_else(|| ToolError::refused("images must list 1 to 20 pictures."))?;
    let mut out = Vec::with_capacity(images.len());
    for (i, img) in images.iter().enumerate() {
        let which = format!("Picture {}", i + 1);
        let path = text(img, "path", &which)?
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| ToolError::refused(format!("{which}: path is required.")))?;
        let mut o = Map::new();
        o.insert("path".into(), json!(path));
        if let Some(c) = text(img, "caption", &which)? {
            o.insert("caption".into(), json!(c));
        }
        out.push(Value::Object(o));
    }
    let mut args = json!({"chatId": chat_id, "images": out});
    if let Some(t) = text(a, "title", "app_show_images")? {
        args["title"] = json!(t);
    }
    Ok(args)
}

async fn show_images(ctx: ToolCtx, a: Value) -> ToolResult {
    let args = op_args(&ctx.caller_chat_id, &a)?;
    let made = ctx.op("shown_images_add", args).await?;
    Ok(json!({"galleryId": made["galleryId"], "count": made["count"]}))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            "app_show_images",
            "Show the user 1-20 pictures as a gallery card in the chat: they can swipe through them with captions and open each full size. Use it whenever the user should see pictures (screenshots, mockups, generated images, designs to compare) instead of listing file paths. images is [{path, caption?}] with absolute paths of PNG, JPEG, GIF or WebP files (25 MB each, 60 MB a call); title is an optional heading. Helm keeps its own copy of each picture, so the card stays after the files change or go. Answers {galleryId, count}. The card shows itself; do not repeat the pictures' paths in your reply.",
            schema(),
        )
        .large()
        .timeout_ms(60_000),
        show_images,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The call's shape is checked before any file is touched, and only known fields go on.
    #[test]
    fn the_call_is_checked_and_trimmed_to_known_fields() {
        let many: Vec<Value> = (0..21)
            .map(|i| json!({"path": format!("C:/p{i}.png")}))
            .collect();
        let bad = [
            (json!({}), "1 to 20"),
            (json!({"images": []}), "1 to 20"),
            (json!({"images": many}), "1 to 20"),
            (
                json!({"images": [{"path": "C:/a.png"}], "mode": "x"}),
                "mode is not a field",
            ),
            (
                json!({"images": [{"path": " "}]}),
                "Picture 1: path is required",
            ),
            (
                json!({"images": [{"path": "C:/a.png"}, {"path": 4}]}),
                "Picture 2: path must be text",
            ),
            (
                json!({"images": [{"path": "C:/a.png", "caption": 3}]}),
                "Picture 1: caption must be text",
            ),
            (
                json!({"title": 5, "images": [{"path": "C:/a.png"}]}),
                "title must be text",
            ),
        ];
        for (a, says) in bad {
            let e = op_args("c1", &a).unwrap_err();
            assert!(e.public.contains(says), "{e:?} for {a}");
        }
        let got = op_args(
            "c1",
            &json!({"title": "T", "images": [
                {"path": "C:/a.png", "caption": "A", "extra": 1}, {"path": "C:/b.png"}]}),
        )
        .unwrap();
        assert_eq!(
            got,
            json!({"chatId": "c1", "title": "T", "images": [
                {"path": "C:/a.png", "caption": "A"}, {"path": "C:/b.png"}]})
        );
    }
}
