//! The queue's host tools: who is waiting to open a turn, and moving one waiting start.
//!
//! `app_queue_status` reads what the queue panel shows: the held starts, what each local model
//! server carries, and what the Claude plan windows say. `app_queue_action` acts on one held
//! start — to the top, up, down, parked by hand, released, or sent back to where it came from.
//! Any chat of the household may act on any waiting start; parking and releasing a chat stay the
//! user's own clicks.

use crate::queue;
use crate::tools::{obj, refuse, rt, EXPLICIT};
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
            "entries is the whole line front to back; servers counts working and queued chats per local server; claudeFull/claudeUntil say whether the plan windows are holding Claude starts; signIn lists chats stopped by a failed Claude token refresh (gaveUp true: only a sign-in gets them going). Queued is never stalled: a waiting chat has a turn saved and will open on its own.",
        ),
        status,
    )
    .blocking(
        ToolSpec::write(
            "app_queue_action",
            format!(
                "Act on one chat waiting in the queue: top (push to the front of its line), up, down or cancel (give the waiting send back: typed text returns to the draft). Parking and releasing a chat are the user's own clicks (hold and release are refused here). Returns the fresh queue. Any chat in the line may be acted on, whoever it belongs to.{EXPLICIT}"
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
            "chatId must name a chat waiting in the queue right now (check app_queue_status first); top, up and down refuse a chat that is not waiting or was parked by hand, and cancel on a chat that is not waiting refuses with its state. hold and release are the user's alone.",
        ),
        act,
    );
}

fn status(ctx: &ToolCtx, _a: Value) -> ToolResult {
    let rt = rt(ctx)?;
    let summary = queue::summary(rt.db(), &rt);
    serde_json::to_value(summary)
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
    // Parking a chat and ending a park are the user's own clicks: a park may be the user's, and
    // the tool cannot tell whose it is.
    if matches!(action, "hold" | "release") {
        return Err(refuse(
            "ask the user: parking a chat and releasing a park are theirs to do in the queue panel.",
        ));
    }
    // Cancelling a hand-parked chat would end the park as well.
    if action == "cancel" && queue::is_manual(db, chat_id) {
        return Err(refuse(
            "ask the user: a chat parked by hand stays parked until they release it in the queue panel.",
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
    async fn the_queue_tools_see_the_whole_line_and_act_on_any_chat_in_it() {
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

        // A chat that is not waiting cannot be pushed or cancelled.
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
        // The status lists every waiting chat, whoever it belongs to.
        queue::manual_hold(&h.db, &other, "normal", helm_core::now_ms());
        let r = call(&h, &t, &mine, "app_queue_status", json!({})).await;
        let mut ids: Vec<&str> = r["result"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["chatId"].as_str())
            .collect();
        ids.sort_unstable();
        let mut both = [mine.as_str(), other.as_str()];
        both.sort_unstable();
        assert_eq!(ids, both, "{r}");
        // Parking and releasing are the user's own clicks, even on the caller's own chat: the
        // park may be the user's.
        for action in ["hold", "release", "cancel"] {
            let r = call(
                &h,
                &t,
                &other,
                "app_queue_action",
                json!({"chatId": other, "action": action}),
            )
            .await;
            assert_eq!(r["ok"], false, "{action}: {r}");
            assert!(
                r["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("ask the user"),
                "{action}: {r}"
            );
        }
        // A hand-parked chat has no line to be pushed to the front of.
        let r = call(
            &h,
            &t,
            &other,
            "app_queue_action",
            json!({"chatId": other, "action": "top"}),
        )
        .await;
        assert_eq!(r["ok"], false, "{r}");
        assert!(
            r["error"]
                .as_str()
                .unwrap_or_default()
                .contains("hand-held"),
            "{r}"
        );
        // A move that shifts another person's chat in the line is made like any other. Two chats
        // of mine and one of theirs wait on one line (the Claude windows: no server).
        let mine2 = h.create().await;
        h.db.write(|tx| tx.execute("UPDATE chats SET profile_id = 'me' WHERE id = ?1", [&mine2]))
            .unwrap();
        let wait = |id: &str, seq: i64| {
            h.db.write(|tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO chat_dispatch_holds(chat_id, reason, class, server_id, seq, since, top)
                     VALUES (?1, 'claude_capacity', 'normal', NULL, ?2, 0, 0)",
                    rusqlite::params![id, seq],
                )
            })
            .unwrap();
        };
        queue::clear_hold(&h.db, &mine);
        queue::clear_hold(&h.db, &other);
        wait(&mine, 1);
        wait(&other, 2);
        wait(&mine2, 3);
        for (who, action, ok) in [
            (&mine2, "up", true),  // swaps with their chat
            (&mine2, "top", true), // passes their chat
            (&mine, "down", true), // swaps with their chat
            (&mine, "up", true),   // swaps back
        ] {
            let r = call(
                &h,
                &t,
                who,
                "app_queue_action",
                json!({"chatId": who, "action": action}),
            )
            .await;
            assert_eq!(r["ok"], ok, "{action}: {r}");
        }
        queue::clear_hold(&h.db, &other);
        queue::clear_hold(&h.db, &mine2);
        queue::manual_hold(&h.db, &mine, "normal", helm_core::now_ms());
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
