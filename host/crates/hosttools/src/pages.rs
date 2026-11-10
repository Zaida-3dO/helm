//! The page tools: pin a local HTML page for the user, and list what is pinned.
//!
//! Both are thin wrappers over the pages crate's own ops, so an agent's pin passes exactly the
//! checks the app's own Pin button does (an `.html` or `.htm` file, a regular file under 10 MiB,
//! outside Helm's own folders). A page runs in the side panel inside a sandbox: it has no access
//! to Helm, the user's files or the network unless the user switches the network on for that
//! open. An agent updates a page by rewriting its file; the open tab reloads by itself.

use crate::registry::{HostToolRegistry, ToolCtx, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Map, Value};

const EXPLICIT: &str =
    " Act only when the user asked for a page or to pin one; report what you pinned.";

const API: &str = "A pinned page is one local .html file that runs its own scripts in a sealed frame in the side panel (inline CSS and JS; images, styles and scripts beside it are served from its folder). It is offline and has no cookies or localStorage. It gets a `helm` object: `await helm.ready`; `helm.state` and `await helm.setState(json)` (up to 256 KiB, kept across restarts: use it instead of localStorage); `helm.theme`, `helm.on('themechange', fn)` and the CSS variables --helm-bg, --helm-text, --helm-surface, --helm-muted, --helm-accent, --helm-border; `await helm.sendMessage({text, data?, title?})`, which asks the user to confirm and then posts the text into this chat as a message from the page, never as the user's own words; `helm.openLink(url)`. To change a page, rewrite its file: the open tab reloads and keeps its saved state. A page cannot start work by itself.";

fn chat_arg(ctx: &ToolCtx, mut args: Map<String, Value>) -> Value {
    args.insert("chatId".into(), json!(ctx.caller_chat_id));
    Value::Object(args)
}

async fn pin(ctx: ToolCtx, a: Value) -> ToolResult {
    let mut args = Map::new();
    for k in ["path", "title"] {
        if let Some(v) = a.get(k).filter(|v| v.is_string()) {
            args.insert(k.into(), v.clone());
        }
    }
    let r = ctx.op("page_pin", chat_arg(&ctx, args)).await?;
    let page = &r["page"];
    Ok(json!({
        "pinned": {"id": page["id"], "title": page["title"], "path": page["path"]},
        "created": r["created"],
        "note": "The page is in the user's Pages list. It opens in the side panel when they click it; \
                 rewrite the file to change what they see.",
    }))
}

async fn list(ctx: ToolCtx, _a: Value) -> ToolResult {
    let r = ctx.op("page_list", chat_arg(&ctx, Map::new())).await?;
    let pages: Vec<Value> = r["pages"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| {
                    json!({
                        "id": p["id"],
                        "title": p["title"],
                        "path": p["path"],
                        "linkedChat": p["chatId"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({"pages": pages}))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            "app_pin_page",
            format!(
                "Pin a local HTML page so the user can open it from Helm's Pages list and keep using it: a dashboard, tracker, checklist or approval page you built as an .html file. It is linked to this chat, which is where the page's messages arrive. Give the path of the file (absolute, or relative to this chat's folder) and optionally a title. Pinning the same file again updates its title.{EXPLICIT}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The .html or .htm file."},
                    "title": {"type": "string", "description": "A short name for the Pages list; defaults to the file name."}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
        .guidelines(API)
        .self_locking(),
        pin,
    )
    .async_tool(
        ToolSpec::read(
            "app_list_pages",
            "List the pages pinned for the user's profile: id, title, file path and the chat each is linked to. Use it to find a page you built earlier before changing its file.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        )
        .self_locking(),
        list,
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
            let page =
                json!({"id": "p1", "title": "Plan", "path": "C:\\w\\plan.html", "chatId": "me"});
            let out = match op {
                "page_pin" if args["path"] == "bad.txt" => Err(helm_ipc::ApiError::new(
                    "refused",
                    "Only .html and .htm files run as pages.",
                )),
                "page_pin" => Ok(json!({"page": page, "created": true})),
                _ => Ok(json!({"pages": [page]})),
            };
            Box::pin(async move { out })
        }
    }

    async fn call(fake: &Arc<Fake>, name: &str, args: Value) -> ToolResult {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let ToolHandler::Async(f) = r.get(name).unwrap().handler.clone() else {
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
    async fn the_tools_pin_for_the_calling_chat_and_pass_the_ops_refusals_on() {
        let fake = Arc::new(Fake(Mutex::new(vec![])));
        let last = || fake.0.lock().unwrap().last().cloned().unwrap();

        // Pin: only the known arguments go to the op, with the caller's chat added by the host.
        let p = call(
            &fake,
            "app_pin_page",
            json!({"path": "plan.html", "title": "Plan", "chatId": "other", "profileId": "x"}),
        )
        .await
        .unwrap();
        assert_eq!(p["pinned"]["id"], "p1");
        let (op, args) = last();
        assert_eq!(op, "page_pin");
        assert_eq!(
            args,
            json!({"path": "plan.html", "title": "Plan", "chatId": "me"})
        );

        // List: ids, titles and links for the calling chat's profile.
        let l = call(&fake, "app_list_pages", json!({})).await.unwrap();
        assert_eq!(l["pages"][0]["linkedChat"], "me");
        assert_eq!(last().1, json!({"chatId": "me"}));

        // The op's refusal reaches the agent, and nothing changed.
        let e = call(&fake, "app_pin_page", json!({"path": "bad.txt"}))
            .await
            .unwrap_err();
        assert!(e.public.contains(".html") && e.changed == crate::Changed::No);
    }

    #[test]
    fn the_page_guidance_fits_and_names_the_whole_page_api() {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let spec = &r.get("app_pin_page").unwrap().spec;
        assert!(spec.problem().is_none());
        let g = spec.guidelines.as_deref().unwrap();
        for api in [
            "helm.ready",
            "helm.state",
            "setState",
            "sendMessage",
            "openLink",
            "themechange",
        ] {
            assert!(g.contains(api), "{api}");
        }
    }
}
