//! The queue's host tools: who is waiting to open a turn, and moving one waiting start.
//!
//! `app_queue_status` reads what the queue panel shows: the held starts, what each local model
//! server carries, and what the Claude plan windows say. `app_queue_action` acts on one held
//! start — to the top, up, down, parked by hand, released, or sent back to where it came from.
//! A chat may act only on a start of the same person's chats: changing another person's queue
//! is the user's own decision, so the tool refuses and says to ask.

use crate::queue;
use crate::tools::{chat_profile, obj, refuse, rt, EXPLICIT};
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

pub fn register(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            "app_queue_status",
            "The queue at a glance: every turn start waiting to open (chat id, title, class, why it waits, how long, pushed-to-top or not), what each local model server carries (working and queued), and what the Claude plan windows say (used up, and until when). No chat is started or stopped by this.".to_owned(),
            obj(json!({}), &[]),
        )
        .guidelines(
            "entries is the whole line front to back; servers counts working and queued chats per local server; claudeFull/claudeUntil say whether the plan windows are holding Claude starts. Queued is never stalled: a waiting chat has a turn saved and will open on its own.",
        ),
        status,
    )
    .blocking(
        ToolSpec::write(
            "app_queue_action",
            format!(
                "Act on one chat waiting in the queue: top (push to the front of its line), up, down, hold (park it by hand until released), release (end a hand-parked wait) or cancel (give the waiting send back: typed text returns to the draft). Returns the fresh queue. Only chats of the same person as this one; another person's queue is the user's to change.{EXPLICIT}"
            ),
            obj(
                json!({
                    "chatId": {"type": "string", "minLength": 36, "maxLength": 36},
                    "action": {
                        "type": "string",
                        "enum": ["top", "up", "down", "hold", "release", "cancel"]
                    },
                }),
                &["chatId", "action"],
            ),
        )
        .guidelines(
            "chatId must name a chat waiting in the queue right now (check app_queue_status first); top, up and down refuse a chat that is not waiting or was parked by hand, and cancel on a chat that is not waiting refuses with its state. Never act on another person's chat: that is the user's, and the tool refuses — ask instead.",
        ),
        act,
    );
}

fn status(ctx: &ToolCtx, _a: Value) -> ToolResult {
    let rt = rt(ctx)?;
    serde_json::to_value(queue::summary(rt.db(), &rt))
        .map_err(|e| ToolError::maybe(format!("The queue could not be read: {e}")))
}

fn act(ctx: &ToolCtx, a: Value) -> ToolResult {
    let rt = rt(ctx)?;
    let chat_id = a["chatId"].as_str().unwrap_or_default();
    let action = a["action"].as_str().unwrap_or_default();
    let db = rt.db();
    let known = db
        .snapshot(|tx| {
            tx.query_row("SELECT 1 FROM chats WHERE id = ?1", [chat_id], |_| Ok(()))
                .optional()
        })
        .map_err(|e| ToolError::maybe(e.to_string()))?
        .is_some();
    if !known {
        return Err(refuse("There is no chat with that id."));
    }
    // The same-person gate: the profiles of the caller and the target chat, both resolved the
    // way the other chat tools do. A database without profiles (or a chat predating them) has
    // one person, so it passes.
    let same_person = db
        .snapshot(|tx| {
            Ok::<_, rusqlite::Error>(
                match (
                    chat_profile(tx, &ctx.caller_chat_id),
                    chat_profile(tx, chat_id),
                ) {
                    (Some(c), Some(t)) => c.0 == t.0,
                    _ => true,
                },
            )
        })
        .map_err(|e| ToolError::maybe(e.to_string()))?;
    if !same_person {
        return Err(refuse(
            "ask the user: queue changes to another person's chats are theirs to make.",
        ));
    }
    let summary = queue::action(&rt, chat_id, action)
        .map_err(|e| ToolError::refused(format!("{}: {}", e.code, e.message)))?;
    serde_json::to_value(summary)
        .map_err(|e| ToolError::maybe(format!("The queue could not be read: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_tests::{harness, H};
    use crate::tools::register_host_tools;
    use helm_hosttools::dispatch::{Caller, CallerBinding, ChatRequest};
    use helm_hosttools::HostTools;
    use serde_json::json;
    use std::sync::Arc;

    fn tools(h: &H) -> HostTools {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        HostTools::new(Arc::new(r)).with_ops(h.api.clone())
    }

    async fn call(h: &H, t: &HostTools, caller: &str, op: &str, args: Value) -> Value {
        let ctx = h.api.ctx();
        let req = ChatRequest {
            id: uuid::Uuid::new_v4().to_string(),
            operation: op.into(),
            args,
            caller: Some(CallerBinding {
                child: false,
                tool_call_id: None,
            }),
        };
        let caller = Caller {
            chat_id: caller.into(),
            restricted: None,
        };
        t.call(ctx.services.clone(), ctx.events.clone(), &caller, &req)
            .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_queue_tools_see_the_line_and_act_within_one_person() {
        let h = harness();
        crate::runtime_tests::attach_ops_host(&h);
        let t = tools(&h);
        let mine = h.create().await;
        let other = h.create().await;
        let idle = h.create().await;
        // Two people: the profiles row belongs to another crate, so the test makes it by hand,
        // the same way the store would.
        h.db
            .write(|tx| {
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS profiles (id TEXT PRIMARY KEY, name TEXT, is_home INTEGER NOT NULL DEFAULT 0);
                     INSERT INTO profiles(id, name, is_home) VALUES ('me', 'Me', 1), ('them', 'Them', 0);",
                )?;
                tx.execute("UPDATE chats SET profile_id = 'me' WHERE id = ?1", [&mine])?;
                tx.execute("UPDATE chats SET profile_id = 'them' WHERE id = ?1", [&other])
            })
            .unwrap();

        // Nothing waits: an empty line that still answers the servers and the windows.
        let r = call(&h, &t, &mine, "app_queue_status", json!({})).await;
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["result"]["entries"].as_array().unwrap().len(), 0);
        assert_eq!(r["result"]["claudeFull"], json!(false));

        // Park one of my chats and act on it; cancel answers the queue's own refusal first
        // because parking is a hold, not a cancel.
        queue::manual_hold(&h.db, &mine, "normal", helm_core::now_ms());
        let r = call(&h, &t, &mine, "app_queue_status", json!({})).await;
        let entries = r["result"]["entries"].as_array().unwrap().clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["chatId"].as_str(), Some(mine.as_str()));
        assert_eq!(entries[0]["reason"].as_str(), Some("manual"));

        // A chat that is not waiting cannot be pushed or cancelled (an idle chat of the same
        // person: no profile set, so the gate lets it through to the queue's own refusal).
        for (op, args) in [
            ("top", json!({"chatId": idle, "action": "top"})),
            ("cancel", json!({"chatId": idle, "action": "cancel"})),
        ] {
            let r = call(&h, &t, &mine, "app_queue_action", args).await;
            assert_eq!(r["ok"], false, "{op}");
            assert!(
                r["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("not waiting"),
                "{op}: {r}"
            );
        }
        // Acting on another person's chat is refused before anything moves.
        queue::manual_hold(&h.db, &other, "normal", helm_core::now_ms());
        let r = call(
            &h,
            &t,
            &mine,
            "app_queue_action",
            json!({"chatId": other, "action": "release"}),
        )
        .await;
        assert_eq!(r["ok"], false);
        assert!(
            r["error"]
                .as_str()
                .unwrap_or_default()
                .contains("another person's"),
            "{r}"
        );
        // The same person's chat acts fine, and release ends the hand-parked wait.
        let r = call(
            &h,
            &t,
            &other,
            "app_queue_action",
            json!({"chatId": other, "action": "release"}),
        )
        .await;
        assert_eq!(r["ok"], true, "{r}");
        let held: Vec<String> =
            h.db.snapshot(|tx| {
                let mut st = tx
                    .prepare("SELECT chat_id FROM chat_dispatch_holds ORDER BY chat_id")
                    .unwrap();
                let rows = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, rusqlite::Error>(rows)
            })
            .unwrap();
        assert_eq!(held, [mine.as_str()], "only my parked chat still waits");
        // An unknown action and an unknown chat are refused in words.
        let r = call(
            &h,
            &t,
            &mine,
            "app_queue_action",
            json!({"chatId": mine, "action": "sideways"}),
        )
        .await;
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap_or_default().contains("sideways"));
    }
}
