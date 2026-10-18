//! Running an admitted outside call inside Helm, after [`crate::gate::admit`].
//!
//! - **Reads and stop** (`app_list_chats`, `app_read_chat`, `app_list_projects`,
//!   `app_list_attention`, `app_stop_chat`) go through Helm's own host-tool dispatcher as a
//!   **restricted** caller named `external:<client>`: the restricted gate is default-deny and
//!   lists only these five, and the dispatcher re-checks sizes, timeouts and the result limit.
//! - **`app_send_message`** is Helm's own send with the gate's frame ("sent by another agent,
//!   not approval"), origin `other_chat`, gated like any cross-chat message. The caller cannot
//!   word the frame. Sends are rate-limited per outside caller.
//!   Each outside caller also gets the chats crate's per-target limit (the same count and window
//!   as one chat messaging another), and a send counts against either limit only once it is
//!   stored, so a refused send never uses up the allowance.
//! - **Telegram** delivery into chats is [`crate::sink::ChatSink`].

use crate::client::{BoxFut, Executor, ToolFailure};
use crate::gate::{self, Admitted};
use helm_chats::submit::{host_send, HostSend, HostSendMode};
use helm_chats::types::SubmissionOrigin;
use helm_hosttools::{Caller, CallerBinding, ChatRequest, HostTools};
use helm_ipc::{EventSink, Services};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// At most this many messages per outside caller per minute.
pub const SENDS_PER_MINUTE: usize = 20;

/// What the executor needs from the app, filled in once the op dispatcher exists.
pub struct Wiring {
    pub services: Arc<Services>,
    pub events: Arc<dyn EventSink>,
    pub tools: Arc<HostTools>,
}

/// One stored relayed send: when, the outside caller, the target chat.
type SendLog = VecDeque<(Instant, String, String)>;

#[derive(Default)]
pub struct AppExecutor {
    wiring: OnceLock<Wiring>,
    sends: Mutex<SendLog>,
}

fn refused(text: impl Into<String>) -> ToolFailure {
    ToolFailure {
        text: text.into(),
        changed: false,
    }
}

/// The caller id an outside client runs under. Never a real chat id.
pub fn external_id(client: &str) -> String {
    let clean: String = client
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(64)
        .collect();
    format!("external:{clean}")
}

impl AppExecutor {
    pub fn wire(&self, w: Wiring) {
        let _ = self.wiring.set(w);
    }

    /// What the app wired in, once it has.
    pub(crate) fn wiring(&self) -> Option<&Wiring> {
        self.wiring.get()
    }

    /// Whether `caller` may send to `to` now: at most [`SENDS_PER_MINUTE`] a minute, and the chats
    /// crate's per-target limit ([`helm_chats::tools::PER_PAIR`] per
    /// [`helm_chats::tools::RATE_WINDOW`]). Checks only; [`Self::record_send`] counts a send.
    fn check_send(
        log: &mut SendLog,
        caller: &str,
        to: &str,
        now: Instant,
    ) -> Result<(), ToolFailure> {
        let window = helm_chats::tools::RATE_WINDOW.max(Duration::from_secs(60));
        while log
            .front()
            .is_some_and(|(t, _, _)| now.duration_since(*t) >= window)
        {
            log.pop_front();
        }
        let minute = log
            .iter()
            .filter(|(t, c, _)| c == caller && now.duration_since(*t) < Duration::from_secs(60))
            .count();
        if minute >= SENDS_PER_MINUTE {
            return Err(refused(format!(
                "At most {SENDS_PER_MINUTE} messages a minute from outside Helm; try again shortly."
            )));
        }
        let pair = log
            .iter()
            .filter(|(t, c, d)| {
                c == caller && d == to && now.duration_since(*t) < helm_chats::tools::RATE_WINDOW
            })
            .count();
        if pair >= helm_chats::tools::PER_PAIR {
            return Err(refused(format!(
                "{} messages have gone to that chat from outside Helm in the last 10 minutes; wait before sending more.",
                helm_chats::tools::PER_PAIR
            )));
        }
        Ok(())
    }

    fn record_send(log: &mut SendLog, caller: &str, to: &str, now: Instant) {
        log.push_back((now, caller.to_owned(), to.to_owned()));
    }

    fn send_message(&self, w: &Wiring, caller: &str, args: &Value) -> Result<Value, ToolFailure> {
        let to = args["chatId"].as_str().unwrap_or("");
        if uuid::Uuid::parse_str(to).is_err() || to.len() != 36 {
            return Err(refused("chatId must be a chat's exact 36-character id."));
        }
        let mode = match args["mode"].as_str() {
            Some("steer") => HostSendMode::Steer,
            _ => HostSendMode::Queue,
        };
        let db = w
            .services
            .get::<helm_core::db::Database>()
            .ok_or_else(|| refused("Helm's database is not available."))?;
        let target: Option<(String, bool)> = db
            .snapshot(|tx| {
                use rusqlite::OptionalExtension;
                tx.query_row(
                    "SELECT title, archived FROM chats WHERE id = ?1",
                    [to],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
                )
                .optional()
            })
            .map_err(|_| refused("The chat could not be looked up."))?;
        let Some((title, archived)) = target else {
            return Err(refused("There is no chat with that id."));
        };
        if archived {
            return Err(refused("That chat is archived."));
        }
        // Held across the send, so two concurrent sends cannot both pass the last free place.
        let mut log = self.sends.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        Self::check_send(&mut log, caller, to, now)?;
        let rt = w
            .services
            .get::<helm_chats::ChatRuntime>()
            .ok_or_else(|| refused("Helm's chats are not available."))?;
        let text = gate::frame(caller, args["message"].as_str().unwrap_or(""));
        let sid = uuid::Uuid::new_v4().to_string();
        let sent = host_send(
            &rt,
            HostSend {
                chat_id: to,
                submission_id: &sid,
                text: &text,
                mode,
                origin: SubmissionOrigin::OtherChat,
                gated: true,
            },
        )
        .map_err(|e| refused(e.message))?;
        // Counted only once it is stored: a refused send never uses up the allowance.
        Self::record_send(&mut log, caller, to, now);
        drop(log);
        let mut out = json!({
            "delivered": sent.delivered,
            "queued": sent.queued,
            "mode": args["mode"],
            "chatId": to,
            "title": title,
        });
        if let Some(p) = sent.waiting {
            out["waitingForSlot"] = json!(true);
            out["position"] = json!(p);
        }
        Ok(out)
    }

    async fn dispatch(
        &self,
        w: &Wiring,
        caller: &str,
        call: Admitted,
    ) -> Result<Value, ToolFailure> {
        let restricted: Vec<String> = gate::allowed_tools()
            .into_iter()
            .filter(|t| *t != "app_send_message")
            .map(str::to_owned)
            .collect();
        let who = Caller {
            chat_id: external_id(caller),
            restricted: Some(restricted),
        };
        let req = ChatRequest {
            id: uuid::Uuid::new_v4().to_string(),
            operation: call.tool,
            args: call.args,
            caller: Some(CallerBinding {
                child: false,
                tool_call_id: None,
            }),
        };
        let r = w
            .tools
            .call(w.services.clone(), w.events.clone(), &who, &req)
            .await;
        if r["ok"] == true {
            Ok(r["result"].clone())
        } else {
            Err(ToolFailure {
                text: r["error"].as_str().unwrap_or("The tool failed.").to_owned(),
                changed: r["changed"].as_str() != Some("no"),
            })
        }
    }
}

impl Executor for AppExecutor {
    fn run<'a>(
        &'a self,
        caller: &'a str,
        call: Admitted,
    ) -> BoxFut<'a, Result<Value, ToolFailure>> {
        Box::pin(async move {
            let Some(w) = self.wiring.get() else {
                return Err(refused("Helm is still starting; try again in a moment."));
            };
            if call.tool == "app_send_message" {
                self.send_message(w, caller, &call.args)
            } else {
                self.dispatch(w, caller, call).await
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_callers_never_get_a_chat_shaped_id() {
        assert_eq!(external_id("remote"), "external:remote");
        assert_eq!(external_id("a b/../c"), "external:abc");
        assert!(uuid::Uuid::parse_str(&external_id("x")).is_err());
    }

    #[test]
    fn sends_are_rate_limited_per_caller_and_per_target_and_counted_once_stored() {
        let mut log = SendLog::new();
        let t0 = Instant::now();
        let ok = |log: &mut SendLog, c: &str, to: &str, t: Instant| {
            AppExecutor::check_send(log, c, to, t).map(|_| AppExecutor::record_send(log, c, to, t))
        };
        // Per target: the chats crate's pair limit.
        for _ in 0..helm_chats::tools::PER_PAIR {
            assert!(ok(&mut log, "remote", "chat-a", t0).is_ok());
        }
        let e = ok(&mut log, "remote", "chat-a", t0).unwrap_err();
        assert!(e.text.contains("to that chat"), "{}", e.text);
        assert!(!e.changed);
        assert!(ok(&mut log, "remote", "chat-b", t0).is_ok(), "per target");
        assert!(ok(&mut log, "other", "chat-a", t0).is_ok(), "per caller");
        // A check that is never recorded (the send was refused) uses up nothing.
        for _ in 0..100 {
            assert!(AppExecutor::check_send(&mut log, "remote", "chat-c", t0).is_ok());
        }
        // Per caller per minute.
        let mut log = SendLog::new();
        for i in 0..SENDS_PER_MINUTE {
            assert!(ok(&mut log, "remote", &format!("c{i}"), t0).is_ok());
        }
        let e = ok(&mut log, "remote", "fresh", t0).unwrap_err();
        assert!(e.text.contains("a minute"), "{}", e.text);
        assert!(ok(&mut log, "remote", "fresh", t0 + Duration::from_secs(61)).is_ok());
        // The pair window is ten minutes, not one.
        let mut log = SendLog::new();
        for _ in 0..helm_chats::tools::PER_PAIR {
            assert!(ok(&mut log, "remote", "a", t0).is_ok());
        }
        assert!(ok(&mut log, "remote", "a", t0 + Duration::from_secs(61)).is_err());
        assert!(ok(&mut log, "remote", "a", t0 + helm_chats::tools::RATE_WINDOW).is_ok());
    }

    #[tokio::test]
    async fn an_unwired_executor_refuses_and_changes_nothing() {
        let e = AppExecutor::default();
        let r = e
            .run("remote", gate::admit("app_list_chats", &json!({})).unwrap())
            .await
            .unwrap_err();
        assert!(!r.changed);
    }
}
