//! The `app_link_item` host tool and the `itemlinks` prompt section.
//!
//! An agent registers its own chat against a task-board item, so the user can find the chat from
//! the item. The caller is always the calling chat (never an argument). Nothing is written to the
//! board: the link lives only in Helm.

use crate::store;
use crate::types::LinkedBy;
use helm_core::db::Database;
use helm_hosttools::{AddendumSection, HostToolRegistry, ToolError, ToolSpec};
use helm_ipc::AppEvent;
use helm_workers::contrib::LaunchCtx;
use serde_json::{json, Value};

pub const DESCRIPTION: &str = "Link THIS chat to a task-board item, so the user can find the chat from the item. itemId is the item's id: the full id, or its first 8 or more hex characters. main: true makes this chat the item's main chat, the one \"open this item\" goes to; do that when this chat becomes the place the item is being worked on. The first chat linked to an item is its main chat. unlink: true removes the link. Linking again changes nothing. It is stored in Helm only; the board is not changed.";

pub const SECTION_TEXT: &str = "When a task-board item becomes what this chat is working on, call app_link_item with the item's id and main: true, so the user can open the item and land here. Link once per item; do not link items you only read or mention.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "itemId": {"type": "string", "description": "The item's full id, or its first 8 or more hex characters."},
            "main": {"type": "boolean", "description": "Make this chat the item's main chat."},
            "unlink": {"type": "boolean", "description": "Remove this chat's link to the item instead."}
        },
        "required": ["itemId"],
        "additionalProperties": false
    })
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write("app_link_item", DESCRIPTION, schema()),
        |ctx, args| {
            let item = args
                .get("itemId")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::refused("itemId is required."))?;
            let main = args.get("main").and_then(Value::as_bool).unwrap_or(false);
            let unlink = args.get("unlink").and_then(Value::as_bool).unwrap_or(false);
            if unlink && main {
                return Err(ToolError::refused("Use main or unlink, not both."));
            }
            let db = ctx
                .services
                .get::<Database>()
                .ok_or_else(|| ToolError::refused("Item links are not available here."))?;
            let chat = ctx.caller_chat_id.clone();
            let out = db
                .write(|tx| {
                    Ok::<_, rusqlite::Error>(if unlink {
                        store::unlink(tx, &chat, item).map(|id| {
                            (id.clone(), json!({"unlinked": id.is_some()}))
                        })
                    } else {
                        store::link(tx, &chat, item, main, LinkedBy::Agent, helm_core::now_ms())
                            .map(|l| {
                                (
                                    Some(l.item_id.clone()),
                                    json!({"linked": true, "itemId": l.item_id, "main": l.main, "chatsLinkedToItem": l.chats}),
                                )
                            })
                    })
                })
                .map_err(|e| ToolError::maybe(e.to_string()))?
                .map_err(ToolError::refused)?;
            if let (Some(id), _) = &out {
                ctx.events
                    .emit(AppEvent::new("item_links").with("itemIds", json!([id])));
            }
            Ok(out.1)
        },
    );
}

/// The prompt-addendum section that tells agents when to link (id `itemlinks`).
pub struct ItemLinksSection;

impl AddendumSection for ItemLinksSection {
    fn id(&self) -> &'static str {
        "itemlinks"
    }
    fn order(&self) -> i32 {
        40
    }
    fn text(&self, _ctx: &LaunchCtx<'_>) -> Option<String> {
        Some(SECTION_TEXT.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::db;
    use helm_hosttools::{CallOrigin, ToolCtx, ToolHandler};
    use helm_ipc::{RecordingSink, Services};
    use std::sync::Arc;

    fn ctx(db: Arc<Database>, chat: &str, sink: Arc<RecordingSink>) -> ToolCtx {
        let mut s = Services::new();
        s.insert(db);
        ToolCtx {
            caller_chat_id: chat.into(),
            is_child: false,
            tool_call_id: None,
            origin: CallOrigin::Agent,
            services: Arc::new(s),
            events: sink,
            ops: None,
        }
    }

    fn call(r: &HostToolRegistry, c: &ToolCtx, args: Value) -> Result<Value, ToolError> {
        let ToolHandler::Blocking(f) = &r.get("app_link_item").unwrap().handler else {
            panic!("blocking")
        };
        f(c, args)
    }

    #[test]
    fn an_agent_links_its_own_chat_and_the_ui_is_told() {
        let (_d, db) = db(&["a", "b"]);
        let db = Arc::new(db);
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let sink = Arc::new(RecordingSink::default());
        let a = ctx(db.clone(), "a", sink.clone());
        let out = call(&r, &a, json!({"itemId": "A1B2C3D4"})).unwrap();
        assert_eq!(out["main"], true);
        assert_eq!(out["chatsLinkedToItem"], 1);
        let b = ctx(db.clone(), "b", sink.clone());
        let out = call(&r, &b, json!({"itemId": "a1b2c3d4", "main": true})).unwrap();
        assert_eq!(out["main"], true);
        assert_eq!(out["chatsLinkedToItem"], 2);
        assert_eq!(sink.kinds(), ["item_links", "item_links"]);
        let out = call(&r, &b, json!({"itemId": "a1b2c3d4", "unlink": true})).unwrap();
        assert_eq!(out["unlinked"], true);
        let again = call(&r, &b, json!({"itemId": "a1b2c3d4", "unlink": true})).unwrap();
        assert_eq!(again["unlinked"], false);
        assert_eq!(
            sink.kinds().len(),
            3,
            "an unlink that found nothing signals nothing"
        );
    }

    #[test]
    fn bad_input_is_refused_before_anything_changes() {
        let (_d, db) = db(&["a"]);
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let sink = Arc::new(RecordingSink::default());
        let c = ctx(Arc::new(db), "a", sink.clone());
        for bad in [
            json!({}),
            json!({"itemId": "short"}),
            json!({"itemId": "../../etc/passwd"}),
            json!({"itemId": "a1b2c3d4", "main": true, "unlink": true}),
        ] {
            let e = call(&r, &c, bad).unwrap_err();
            assert!(e.text().ends_with("Nothing changed."), "{}", e.text());
        }
        assert!(sink.kinds().is_empty());
    }

    #[test]
    fn the_tool_never_names_the_board_and_the_section_tells_agents_when_to_link() {
        assert!(!DESCRIPTION.to_lowercase().contains("transition"));
        let s = ItemLinksSection;
        assert_eq!(s.id(), "itemlinks");
        assert!(s
            .text(&LaunchCtx {
                chat_id: "c",
                cwd: "C:/w"
            })
            .unwrap()
            .contains("app_link_item"));
    }
}
