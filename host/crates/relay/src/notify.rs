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
//! status message.

use crate::client::{LineHandle, LineState};
use crate::telegram;
use helm_chats::types::{AttentionRow, AttentionSummary};
use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The most messages chats may send in any hour, so a looping agent cannot flood the phone.
pub const MAX_PER_HOUR: usize = 20;
/// The longest text a chat may send.
pub const MAX_TEXT: usize = 2000;
/// How often the away watcher looks at the attention summary.
const TICK: Duration = Duration::from_secs(20);
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

/// What the away watcher remembers between looks.
#[derive(Default)]
pub struct Away {
    /// The attention rows already seen in this away span; `None` while the user is present.
    seen: Option<HashSet<(String, String)>>,
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

impl Away {
    /// One look. `away` is whether the user is away with alerts on; `skip` is the Telegram chat.
    /// Returns the message to send, if anything is new. The first look of an away span only
    /// records what is already waiting.
    pub fn step(
        &mut self,
        away: bool,
        rows: &[AttentionRow],
        skip: Option<&str>,
    ) -> Option<String> {
        if !away {
            self.seen = None;
            return None;
        }
        let now: HashSet<(String, String)> = rows
            .iter()
            .map(|r| (r.chat_id.clone(), format!("{:?}", r.kind)))
            .collect();
        let Some(seen) = self.seen.replace(now) else {
            return None;
        };
        let mut lines: Vec<String> = vec![];
        let mut chats: HashSet<&str> = HashSet::new();
        for r in rows {
            let key = (r.chat_id.clone(), format!("{:?}", r.kind));
            if seen.contains(&key) || Some(r.chat_id.as_str()) == skip {
                continue;
            }
            if chats.insert(&r.chat_id) {
                lines.push(format!("• {}", compose(&r.title, &r.reason)));
            }
        }
        if lines.is_empty() {
            return None;
        }
        Some(format!("While you're away:\n{}", lines.join("\n")))
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

/// Watch for away alerts on the app's runtime, until the app exits.
pub fn watch_away(
    handle: &tokio::runtime::Handle,
    rt: ChatRuntime,
    afk: Arc<helm_afk::AfkService>,
    line: Arc<LineHandle>,
    profile_dir: PathBuf,
) {
    handle.spawn(async move {
        let mut away = Away::default();
        let mut every = tokio::time::interval(TICK);
        loop {
            every.tick().await;
            let (rt, afk, dir) = (rt.clone(), afk.clone(), profile_dir.clone());
            let online = line.state() == LineState::Online;
            let Ok(look) = tokio::task::spawn_blocking(move || {
                let on = online
                    && afk.open_source().is_some()
                    && telegram::load_away_alerts(&telegram::policy_path(&dir));
                let rows = if on {
                    rt.attention().map(|a| alertable(&a)).unwrap_or_default()
                } else {
                    vec![]
                };
                let skip = telegram::load_tag(&telegram::tag_path(&dir)).map(|t| t.chat_id);
                (on, rows, skip)
            })
            .await
            else {
                continue;
            };
            let (on, rows, skip) = look;
            if let Some(text) = away.step(on, &rows, skip.as_deref()) {
                line.telegram_notify(&text);
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
        let mut a = Away::default();
        // (away, rows, expected message)
        let steps: Vec<(bool, Vec<AttentionRow>, Option<&str>)> = vec![
            (false, vec![docs.clone()], None),
            // Going away records what already waits.
            (true, vec![docs.clone()], None),
            (true, vec![docs.clone()], None),
            // New while away; the Telegram chat is left to its own mirror.
            (
                true,
                vec![docs.clone(), build.clone(), tg.clone()],
                Some("While you're away:\n• Build: Waiting for an approval"),
            ),
            // The same chat in a new state is new again.
            (
                true,
                vec![
                    row("c1", Question, "Docs", "Asked a question"),
                    build.clone(),
                ],
                Some("While you're away:\n• Docs: Asked a question"),
            ),
            // Back, then away again: a fresh baseline.
            (false, vec![], None),
            (true, vec![build.clone()], None),
        ];
        for (i, (away, rows, want)) in steps.into_iter().enumerate() {
            assert_eq!(a.step(away, &rows, Some("tg")).as_deref(), want, "step {i}");
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
