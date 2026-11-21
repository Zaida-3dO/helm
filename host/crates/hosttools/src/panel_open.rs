//! `app_open_panel`: show the user a file or a pinned page in the side panel.
//!
//! The tool opens nothing itself. It checks the target through the same ops the panel uses (a
//! file through `path_resolve` against the chat's folder, a page through the chat's own Pages
//! list) and answers with what to open. The window showing the chat opens it when that answer
//! arrives while the chat is in view, on whichever PC the user is looking at it from, so the same
//! result works for a chat watched from another PC. A result read back from history opens nothing;
//! its card keeps an Open button.

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Value};

/// The longest line number taken: past it, the file is opened at its top.
const MAX_LINE: u64 = 10_000_000;

/// A link-safe form of `s`: every byte but the unreserved URL characters is percent-encoded, so
/// the path's own `:` and `\` never read as a line suffix or a separator.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The link that opens a file in the panel, optionally at a line.
pub(crate) fn file_link(path: &str, line: Option<u64>) -> String {
    match line {
        Some(l) => format!("helm://files/{}:{l}", percent_encode(path)),
        None => format!("helm://files/{}", percent_encode(path)),
    }
}

/// The link that opens a pinned page.
pub(crate) fn page_link(id: &str) -> String {
    format!("helm://pages/{id}")
}

/// A pin id as Helm makes them: letters, digits and dashes, 1 to 64 of them.
pub(crate) fn valid_pin_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A network share or a device path: never shown from a tool.
fn is_share(path: &str) -> bool {
    let p = path.trim().replace('/', "\\");
    p.starts_with("\\\\")
}

fn text_arg<'a>(a: &'a Value, k: &str) -> Result<Option<&'a str>, ToolError> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(ToolError::refused(format!("{k} must be a string."))),
    }
}

async fn open_panel(ctx: ToolCtx, a: Value) -> ToolResult {
    let path = text_arg(&a, "path")?;
    let page = text_arg(&a, "pageId")?;
    let line = match a.get("line") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_u64() {
            Some(n) if (1..=MAX_LINE).contains(&n) => Some(n),
            _ => return Err(ToolError::refused("line must be a whole number from 1 up.")),
        },
    };
    match (path, page) {
        (Some(_), Some(_)) | (None, None) => Err(ToolError::refused(
            "Give exactly one of path (a file) or pageId (a pinned page).",
        )),
        (Some(path), None) => {
            if is_share(path) {
                return Err(ToolError::refused(
                    "Network shares and device paths are not shown in the panel.",
                ));
            }
            let r = ctx
                .op(
                    "path_resolve",
                    json!({"chatId": ctx.caller_chat_id, "path": path}),
                )
                .await?;
            let Some(abs) = r["path"].as_str() else {
                return Err(ToolError::refused("That path could not be resolved."));
            };
            if r["kind"] != json!("file") {
                return Err(ToolError::refused(
                    "That is a folder: give a file inside it.",
                ));
            }
            let mut opened = json!({"kind": "file", "path": abs});
            if let Some(l) = line {
                opened["line"] = json!(l);
            }
            Ok(json!({
                "opened": opened,
                "link": file_link(abs, line),
                "note": "The file opens in the side panel wherever the user is looking at this chat; if they are elsewhere, the result keeps an Open button. Do not paste its contents as well.",
            }))
        }
        (None, Some(id)) => {
            if line.is_some() {
                return Err(ToolError::refused("line goes with path only."));
            }
            if !valid_pin_id(id) {
                return Err(ToolError::refused(
                    "That is not a page id: app_list_pages lists them.",
                ));
            }
            let r = ctx
                .op("page_list", json!({"chatId": ctx.caller_chat_id}))
                .await?;
            let found = r["pages"]
                .as_array()
                .and_then(|a| a.iter().find(|p| p["id"] == json!(id)))
                .cloned()
                .ok_or_else(|| {
                    ToolError::refused("No pinned page has that id: app_list_pages lists them.")
                })?;
            Ok(json!({
                "opened": {
                    "kind": "page",
                    "pageId": id,
                    "path": found["path"],
                    "title": found["title"],
                },
                "link": page_link(id),
                "note": "The page opens in the side panel wherever the user is looking at this chat; if they are elsewhere, the result keeps an Open button.",
            }))
        }
    }
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::read(
            "app_open_panel",
            "Show the user a file (path, absolute or relative to this chat's folder, optional line) or a pinned page (pageId) in Helm's side panel, on the PC they view this chat from. Use it instead of pasting what you made or changed. Returns a link for your reply.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "A file to show."},
                    "line": {"type": "integer", "minimum": 1, "description": "The line to show, with path."},
                    "pageId": {"type": "string", "description": "A pinned page's id."}
                },
                "additionalProperties": false
            }),
        )
        .self_locking(),
        open_panel,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{CallOrigin, OpInvoker, ToolHandler};
    use std::sync::{Arc, Mutex};

    struct Fake(Mutex<Vec<(String, Value)>>);
    impl OpInvoker for Fake {
        fn invoke<'a>(
            &'a self,
            op: &'a str,
            args: Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Value, helm_ipc::ApiError>> + Send + 'a>,
        > {
            self.0.lock().unwrap().push((op.to_owned(), args.clone()));
            let out = match (op, args["path"].as_str()) {
                ("path_resolve", Some("src")) => {
                    Ok(json!({"path": "C:\\w\\app\\src", "kind": "dir"}))
                }
                ("path_resolve", Some("secret.txt")) => Err(helm_ipc::ApiError::new(
                    "refused",
                    "Helm's own folders are not shown.",
                )),
                ("path_resolve", Some(p)) => {
                    Ok(json!({"path": format!("C:\\w\\app\\{p}"), "kind": "file"}))
                }
                _ => Ok(json!({"pages": [
                    {"id": "p-1", "title": "Plan", "path": "C:\\w\\plan.html", "chatId": "me"}
                ]})),
            };
            Box::pin(async move { out })
        }
    }

    async fn call(fake: &Arc<Fake>, args: Value) -> ToolResult {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let ToolHandler::Async(f) = r.get("app_open_panel").unwrap().handler.clone() else {
            panic!()
        };
        f(
            ToolCtx {
                caller_chat_id: "me".into(),
                is_child: false,
                tool_call_id: Some("tc1".into()),
                origin: CallOrigin::Agent,
                services: Arc::new(helm_ipc::Services::new()),
                events: Arc::new(helm_ipc::NullSink),
                ops: Some(fake.clone()),
            },
            args,
        )
        .await
    }

    #[tokio::test]
    async fn a_file_or_a_page_is_checked_and_named_and_anything_else_is_refused() {
        let fake = Arc::new(Fake(Mutex::new(vec![])));

        // A file is resolved against the calling chat, and the answer names the absolute path,
        // the line and a link whose path cannot be mistaken for a line suffix.
        let r = call(&fake, json!({"path": "main.rs", "line": 12}))
            .await
            .unwrap();
        assert_eq!(
            r["opened"],
            json!({"kind": "file", "path": "C:\\w\\app\\main.rs", "line": 12})
        );
        assert_eq!(r["link"], "helm://files/C%3A%5Cw%5Capp%5Cmain.rs:12");
        assert_eq!(
            fake.0.lock().unwrap().last().unwrap().1,
            json!({"chatId": "me", "path": "main.rs"})
        );

        // A page is found by id in the chat's own Pages list.
        let r = call(&fake, json!({"pageId": "p-1"})).await.unwrap();
        assert_eq!(r["opened"]["kind"], "page");
        assert_eq!(r["opened"]["title"], "Plan");
        assert_eq!(r["opened"]["path"], "C:\\w\\plan.html");
        assert_eq!(r["link"], "helm://pages/p-1");

        // Refusals: nothing reaches the panel and nothing changed.
        let calls_before = fake.0.lock().unwrap().len();
        for (args, says) in [
            (json!({}), "exactly one"),
            (json!({"path": "a.rs", "pageId": "p-1"}), "exactly one"),
            (json!({"path": "\\\\nas\\share\\a.txt"}), "shares"),
            (json!({"path": "//nas/share/a.txt"}), "shares"),
            (json!({"path": "\\\\?\\C:\\a.txt"}), "shares"),
            (json!({"pageId": "../x"}), "not a page id"),
            (json!({"pageId": "p-1", "line": 3}), "path only"),
            (json!({"path": "a.rs", "line": 0}), "whole number"),
            (json!({"path": 7}), "must be a string"),
        ] {
            let e = call(&fake, args.clone()).await.unwrap_err();
            assert!(e.public.contains(says), "{args}: {}", e.public);
            assert_eq!(e.changed, crate::Changed::No);
        }
        assert_eq!(
            fake.0.lock().unwrap().len(),
            calls_before,
            "refused before any op ran"
        );
        // The ops' own refusals reach the agent: a folder, Helm's own files, an unknown page.
        for (args, says) in [
            (json!({"path": "src"}), "folder"),
            (json!({"path": "secret.txt"}), "Helm's own"),
            (json!({"pageId": "p-2"}), "No pinned page"),
        ] {
            let e = call(&fake, args.clone()).await.unwrap_err();
            assert!(e.public.contains(says), "{args}: {}", e.public);
        }
    }
}
