//! Helm speaking first on Telegram.
//!
//! - **`app_telegram_send`.** A chat sends the user a short Telegram message through the hub's
//!   bot, for example that its work is finished. It arrives as a new message, so the phone
//!   notifies, headed by the chat's title. It is refused from the Telegram chat itself (its
//!   answers already go to Telegram), while the line is down ("nothing was sent"), and past
//!   [`MAX_PER_HOUR`] messages in an hour.
//! - **Away alerts.** While the user is away (AFK) and the line is up, a chat that newly needs
//!   them (a question, an approval, a plan, a stall) or newly finished a reply they have not read
//!   gets one line on Telegram. What already needed them when they went away is not repeated, and
//!   the Telegram chat is left to its own mirror. Settings → Hub turns them off.
//!
//! Both go up as `telegram_notify`, which the hub sends as a new message without touching a live
//! status message, and both are written into the Telegram chat: a line in its transcript, and a
//! note its agent gets with the user's next message, so a reply to an alert makes sense there.
//!
//! The same watcher sends approval asks when approving from Telegram is on ([`crate::approve`]).

use crate::approve;
use crate::client::{LineHandle, LineState};
use crate::sink::ChatSink;
use crate::telegram;
use helm_chats::types::{AttentionKind, AttentionRow, AttentionSummary, ContextKind};
use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The most messages chats may send in any hour, so a looping agent cannot flood the phone.
pub const MAX_PER_HOUR: usize = 20;
/// The longest text a chat may send.
pub const MAX_TEXT: usize = 2000;
/// How often the watcher looks at the attention summary (away alerts and approval asks).
const TICK: Duration = Duration::from_secs(5);
const HOUR_MS: i64 = 60 * 60 * 1000;

/// A chat's message as the phone shows it: the chat's title, then the text.
pub fn compose(title: &str, text: &str) -> String {
    let title = title.trim();
    let title = if title.is_empty() {
        "A Helm chat"
    } else {
        title
    };
    format!("{title}: {}", text.trim())
}

/// A sliding one-hour window over the messages sent.
#[derive(Default)]
pub struct Budget(Mutex<VecDeque<i64>>);

impl Budget {
    /// Take one send, or `false` when [`MAX_PER_HOUR`] were already sent in the last hour.
    pub fn take(&self, now: i64) -> bool {
        let mut q = self.0.lock().unwrap_or_else(|p| p.into_inner());
        while q.front().is_some_and(|t| now - t >= HOUR_MS) {
            q.pop_front();
        }
        if q.len() >= MAX_PER_HOUR {
            return false;
        }
        q.push_back(now);
        true
    }
}

static BUDGET: Budget = Budget(Mutex::new(VecDeque::new()));

/// One chat in one attention state ("Docs, asked a question").
type Key = (String, String);

/// What the away watcher remembers across one away span.
#[derive(Default)]
pub struct Away {
    /// What already waited when the span began, plus what has been alerted since; `None` while
    /// the user is present. Kept for the whole span, so a chat that keeps finishing turns is
    /// alerted once, not after every turn.
    seen: Option<HashSet<Key>>,
}

/// An alert ready to send, and the rows it covers (marked seen only once it was sent).
#[derive(Debug, PartialEq)]
pub struct Alert {
    pub text: String,
    pub keys: Vec<Key>,
}

/// The rows an away alert is about: chats that need the user, have stalled, or finished a reply
/// they have not read.
pub fn alertable(a: &AttentionSummary) -> Vec<AttentionRow> {
    a.needs_you
        .iter()
        .chain(&a.stalled)
        .chain(&a.unread)
        .cloned()
        .collect()
}

fn key(r: &AttentionRow) -> Key {
    (r.chat_id.clone(), format!("{:?}", r.kind))
}

impl Away {
    /// The user is back (or alerts are off): the next away span starts afresh.
    pub fn reset(&mut self) {
        self.seen = None;
    }

    /// One look while the user is away; `skip` is the Telegram chat. The first look of a span
    /// only records what is already waiting. Later looks return what is new since, once per
    /// chat and state for the whole span.
    pub fn step(&mut self, rows: &[AttentionRow], skip: Option<&str>) -> Option<Alert> {
        let Some(seen) = &self.seen else {
            self.seen = Some(rows.iter().map(key).collect());
            return None;
        };
        let mut lines: Vec<String> = vec![];
        let mut keys: Vec<Key> = vec![];
        let mut chats: HashSet<&str> = HashSet::new();
        for r in rows {
            let k = key(r);
            if seen.contains(&k) || Some(r.chat_id.as_str()) == skip {
                continue;
            }
            if chats.insert(&r.chat_id) {
                lines.push(format!("• {}", compose(&r.title, &r.reason)));
            }
            keys.push(k);
        }
        if lines.is_empty() {
            return None;
        }
        Some(Alert {
            text: format!("While you're away:\n{}", lines.join("\n")),
            keys,
        })
    }

    /// The alert went out: its rows are not alerted again this span.
    pub fn sent(&mut self, keys: Vec<Key>) {
        if let Some(seen) = &mut self.seen {
            seen.extend(keys);
        }
    }
}

fn title_of(db: &Database, chat: &str) -> Option<String> {
    db.snapshot(|tx| {
        tx.query_row("SELECT title FROM chats WHERE id = ?1", [chat], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()
    })
    .ok()
    .flatten()
    .flatten()
}

/// Write what was sent to the user on Telegram into the Telegram chat: a line in its transcript,
/// and a note for its agent with the next message. Not for the Telegram chat's own sends.
fn record_sent(rt: Option<&ChatRuntime>, dir: &Path, from: &str, text: &str, caller: Option<&str>) {
    let Some(tag) = telegram::load_tag(&telegram::tag_path(dir)) else {
        return;
    };
    if Some(tag.chat_id.as_str()) == caller {
        return;
    }
    if let Some(rt) = rt {
        let line: String = text.chars().take(600).collect();
        rt.add_context_row(
            &tag.chat_id,
            ContextKind::Telegram,
            &format!("Sent to you on Telegram by {from}: {line}"),
        );
    }
    if telegram::remember_sent(&telegram::sent_path(dir), from, text).is_err() {
        log::warn!("relay: the note of an alert sent could not be saved");
    }
}

fn profile_dir(ctx: &ToolCtx) -> Option<PathBuf> {
    ctx.services
        .get::<helm_core::profile::Profile>()
        .map(|p| p.dir.clone())
}

fn send(ctx: &ToolCtx, a: Value) -> ToolResult {
    let text = a.get("text").and_then(Value::as_str).unwrap_or("").trim();
    if text.is_empty() {
        return Err(ToolError::refused("text is required."));
    }
    if text.chars().count() > MAX_TEXT {
        return Err(ToolError::refused(format!(
            "text is longer than {MAX_TEXT} characters; send a short summary."
        )));
    }
    if let Some(tag) = profile_dir(ctx).and_then(|d| telegram::load_tag(&telegram::tag_path(&d))) {
        if tag.chat_id == ctx.caller_chat_id {
            return Err(ToolError::refused(
                "This is the Telegram chat: its answers already reach Telegram, so nothing was sent.",
            ));
        }
    }
    let line = ctx
        .services
        .get::<LineHandle>()
        .filter(|l| l.state() == LineState::Online)
        .ok_or_else(|| {
            ToolError::refused(
                "Helm's line to the hub is not connected (Settings → Hub), so nothing was sent.",
            )
        })?;
    if !BUDGET.take(helm_core::now_ms()) {
        return Err(ToolError::refused(format!(
            "{MAX_PER_HOUR} Telegram messages were already sent in the last hour, so this one was not."
        )));
    }
    let title = ctx
        .services
        .get::<Database>()
        .and_then(|db| title_of(&db, &ctx.caller_chat_id))
        .unwrap_or_default();
    if !line.telegram_notify(&compose(&title, text)) {
        return Err(ToolError::refused(
            "Helm's line to the hub dropped, so nothing was sent.",
        ));
    }
    if let Some(dir) = profile_dir(ctx) {
        let rt = ctx.services.get::<ChatRuntime>();
        let from = if title.trim().is_empty() {
            "a Helm chat"
        } else {
            title.trim()
        };
        record_sent(rt.as_deref(), &dir, from, text, Some(&ctx.caller_chat_id));
    }
    Ok(json!({ "sent": true }))
}

/// The `app_telegram_send` host tool.
pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write(
            "app_telegram_send",
            format!(
                "Send the user a short message on Telegram through Helm's bot, for example that \
                 the work they asked you to report on is finished. It arrives as a new message on \
                 their phone, headed by this chat's title; their reply goes to Helm's Telegram \
                 chat, not here. text: at most {MAX_TEXT} characters of plain text. Refused, with \
                 nothing sent, when Helm's line to the hub is not connected or {MAX_PER_HOUR} \
                 messages were sent in the last hour."
            ),
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT }
                },
                "required": ["text"],
                "additionalProperties": false
            }),
        ),
        send,
    );
}

/// What one look found: the away rows (`None`: not away, or alerts off), the requests an ask
/// could answer (`None`: approving from Telegram is off) and the Telegram chat.
struct Look {
    rows: Option<Option<Vec<AttentionRow>>>,
    waiting: Option<Vec<approve::Waiting>>,
    tg_chat: Option<String>,
}

fn look(rt: &ChatRuntime, afk: &helm_afk::AfkService, dir: &Path) -> Look {
    let policy = telegram::load_file(&telegram::policy_path(dir));
    let away = afk.open_source().is_some();
    let tg_chat = telegram::load_tag(&telegram::tag_path(dir)).map(|t| t.chat_id);
    let summary = (away || policy.approvals)
        .then(|| rt.attention().ok())
        .flatten();
    // A failed read is no look at all, not "nothing waits".
    let rows = (away && policy.away_alerts).then(|| summary.as_ref().map(alertable));
    let waiting = policy.approvals.then(|| {
        let mut out = vec![];
        for r in summary.iter().flat_map(|a| &a.needs_you) {
            let here = away || tg_chat.as_deref() == Some(r.chat_id.as_str());
            if !here || !matches!(r.kind, AttentionKind::Approval | AttentionKind::Plan) {
                continue;
            }
            for i in rt
                .live_thread(&r.chat_id)
                .map(|t| t.interactions)
                .unwrap_or_default()
            {
                out.push(approve::Waiting {
                    chat_id: r.chat_id.clone(),
                    chat_title: r.title.clone(),
                    interaction: i,
                });
            }
        }
        out
    });
    Look {
        rows,
        waiting,
        tg_chat,
    }
}

/// Watch for away alerts and approval asks on the app's runtime, until the app exits.
pub fn watch_away(
    handle: &tokio::runtime::Handle,
    rt: ChatRuntime,
    afk: Arc<helm_afk::AfkService>,
    line: Arc<LineHandle>,
    sink: Arc<ChatSink>,
    profile_dir: PathBuf,
) {
    handle.spawn(async move {
        let mut away = Away::default();
        let mut was_online = false;
        let mut every = tokio::time::interval(TICK);
        loop {
            every.tick().await;
            let (rt2, afk, dir) = (rt.clone(), afk.clone(), profile_dir.clone());
            let Ok(found) = tokio::task::spawn_blocking(move || look(&rt2, &afk, &dir)).await
            else {
                continue;
            };
            let online = line.state() == LineState::Online;
            // A line that went away may have taken the hub's buttons with it: ask afresh.
            if online && !was_online {
                sink.asks.clear();
            }
            was_online = online;
            match &found.waiting {
                None => {
                    for id in sink.asks.clear() {
                        line.telegram_ask_done(
                            &id,
                            "Approving from Telegram was turned off in Helm.",
                        );
                    }
                }
                Some(waiting) if online => {
                    let step = sink.asks.step(waiting, approve::new_id);
                    for (id, text, options) in step.ask {
                        if !line.telegram_ask(&id, &text, &options) {
                            sink.asks.unsent(&id);
                        }
                    }
                    for (id, note) in step.done {
                        line.telegram_ask_done(&id, &note);
                    }
                }
                Some(_) => {}
            }
            let Some(rows) = found.rows else {
                away.reset();
                continue;
            };
            // With the line down, wait for it: what arrives meanwhile is alerted once it is back.
            let (Some(rows), true) = (rows, online) else {
                continue;
            };
            if let Some(alert) = away.step(&rows, found.tg_chat.as_deref()) {
                // Alerts share the chats' hourly cap; one not sent is tried again next look.
                if BUDGET.take(helm_core::now_ms()) && line.telegram_notify(&alert.text) {
                    let (rt2, dir, text) = (rt.clone(), profile_dir.clone(), alert.text.clone());
                    let _ = tokio::task::spawn_blocking(move || {
                        record_sent(Some(&rt2), &dir, "Helm, while you were away", &text, None)
                    })
                    .await;
                    away.sent(alert.keys);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_chats::types::AttentionKind;

    fn row(chat: &str, kind: AttentionKind, title: &str, reason: &str) -> AttentionRow {
        AttentionRow {
            chat_id: chat.into(),
            title: title.into(),
            project: "p".into(),
            kind,
            reason: reason.into(),
        }
    }

    #[test]
    fn alerts_say_only_what_is_new_while_away_and_sends_are_capped() {
        use AttentionKind::*;
        let docs = row("c1", Unread, "Docs", "Finished a reply");
        let build = row("c2", Approval, "Build", "Waiting for an approval");
        let tg = row("tg", Question, "Telegram", "Asked a question");
        let asked = row("c1", Question, "Docs", "Asked a question");
        let mut a = Away::default();
        // (away, rows, expected message, whether it was sent)
        let steps: Vec<(bool, Vec<AttentionRow>, Option<&str>, bool)> = vec![
            // Going away records what already waits.
            (true, vec![docs.clone()], None, true),
            (true, vec![docs.clone()], None, true),
            // New while away; the Telegram chat is left to its own mirror. Not sent (the line
            // dropped), so it is offered again.
            (
                true,
                vec![docs.clone(), build.clone(), tg.clone()],
                Some("While you're away:\n• Build: Waiting for an approval"),
                false,
            ),
            (
                true,
                vec![docs.clone(), build.clone()],
                Some("While you're away:\n• Build: Waiting for an approval"),
                true,
            ),
            // The same chat in a new state is new again...
            (
                true,
                vec![asked.clone(), build.clone()],
                Some("While you're away:\n• Docs: Asked a question"),
                true,
            ),
            // ...but a chat that leaves a state and comes back is not alerted twice in one span
            // (a chat finishing turn after turn).
            (true, vec![build.clone()], None, true),
            (true, vec![asked.clone(), build.clone()], None, true),
            // Back, then away again: a fresh baseline.
            (false, vec![], None, true),
            (true, vec![build.clone()], None, true),
        ];
        for (i, (away, rows, want, sent)) in steps.into_iter().enumerate() {
            if !away {
                a.reset();
                continue;
            }
            let got = a.step(&rows, Some("tg"));
            assert_eq!(got.as_ref().map(|g| g.text.as_str()), want, "step {i}");
            if let (Some(g), true) = (got, sent) {
                a.sent(g.keys);
            }
        }
        assert_eq!(compose("  ", " done "), "A Helm chat: done");
        let b = Budget::default();
        for _ in 0..MAX_PER_HOUR {
            assert!(b.take(0));
        }
        assert!(!b.take(HOUR_MS - 1), "full within the hour");
        assert!(b.take(HOUR_MS), "the oldest left the window");
    }
}
