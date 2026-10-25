//! Helm's dedicated Telegram bot, run by the hub.
//!
//! - **Off until configured.** The bot runs only when the NAS env file holds a
//!   bot token *and* at least one allowed user id. Either missing: no polling.
//! - **Two ways in.** By default the hub long-polls `getUpdates` itself. Only
//!   one process may consume a bot's updates, so when another service already
//!   runs the bot, `HUB_TELEGRAM_RECEIVE=off` stops the polling and that
//!   service posts what it received to `/telegram/update` instead. Either way
//!   each update goes through [`Bridge::on_update`], which handles an
//!   `update_id` once (a forwarder's retry, or the two inputs overlapping).
//!   `/helm <text>` is read as `<text>`, so a forwarder on a shared bot can
//!   route Helm's messages by that one command.
//! - **Deny by default.** A message whose sender is not on the allow-list is
//!   dropped: nothing is forwarded to Helm and nothing is sent back, so the bot
//!   does not even confirm it exists.
//! - **Private chats only.** A message in a group is dropped too, even from an
//!   allowed user: replies and live status go to the chat the user last wrote
//!   in, and in a group other members would read them.
//! - **Forwarding.** An allowed message goes down the relay line as a
//!   `telegram` frame and is held until Helm acknowledges it. Line down: the
//!   message is queued in order and the sender is told at once. A queued
//!   message older than the queue lifetime is dropped (and the user is told how
//!   many). On reconnect the queue is delivered in order, each carrying its age
//!   so Helm can mark it "sent N min ago". A message Helm has not acknowledged
//!   within [`RESEND_AFTER`] goes down the line again (Helm gives up on a busy
//!   chat sooner than that), until it is acknowledged or outlives the queue
//!   lifetime. `/cancel` drops the queue; `/stop` asks Helm to stop the
//!   running turn.
//! - **Helm can speak first.** `telegram_notify` (an alert: a chat finished, or something needs
//!   the user while they are away) arrives as a new message and leaves the live status alone.
//! - **Never silent while working.** Helm sends the live status as
//!   `telegram_status`; the bot keeps ONE status message and edits it in place,
//!   at most once per [`EDIT_INTERVAL`]. `telegram_final` arrives as a NEW
//!   message (so the phone notifies) and the status message is then deleted.
//!
//! The queue lives in memory: a container restart drops it.

use crate::relay::{Line, Upstream, OFFLINE};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The fewest milliseconds between two edits of the status message. Telegram allows about one
/// message per second in a chat; a slower pace leaves room for the final answer.
pub const EDIT_INTERVAL: Duration = Duration::from_secs(3);
/// Telegram's message limit is 4096 characters; a little headroom.
pub const MAX_TEXT: usize = 4000;
/// The status message is a summary, not a transcript.
pub const MAX_STATUS: usize = 1000;
/// How often the bridge flushes its queue and its pending status edit.
pub const TICK: Duration = Duration::from_millis(500);
/// A message sent down the line and not acknowledged this long after is sent again on the same
/// line. Helm retries a busy chat for less than this before giving up without an ack.
pub const RESEND_AFTER: Duration = Duration::from_secs(45);
/// How long one `getUpdates` long poll waits for a message. Kept short: a home router may drop
/// a connection that is idle much longer, and then the poll hangs until the request timeout, so a
/// message sent meanwhile waits for the next poll.
pub const POLL_SECONDS: u64 = 25;
/// How many recent `update_id`s are remembered, so an update is handled once.
pub const SEEN_UPDATES: usize = 256;

pub const QUEUED: &str =
    "Helm is offline. Queued; it will be delivered when Helm reconnects. /cancel drops the queue.";
pub const TEXT_ONLY: &str = "Only text messages reach Helm for now.";
pub const HELLO: &str =
    "Helm is listening. Messages here go to the active Telegram chat in Helm. /stop stops the \
     running turn; /cancel drops messages queued while Helm is offline.";

pub type ApiFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, ApiError>> + Send + 'a>>;

/// One Bot API method call. The real one is [`HttpApi`]; tests use a recording fake.
pub trait TelegramApi: Send + Sync {
    fn call<'a>(&'a self, method: &'a str, body: Value) -> ApiFuture<'a>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub code: Option<i64>,
    pub description: String,
}

/// How updates reach the bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receive {
    /// The hub long-polls `getUpdates` itself.
    Poll,
    /// Another service already runs the bot and posts its updates to `/telegram/update`; the
    /// hub never polls, but still sends, edits and deletes messages with the bot token.
    Off,
}

#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token: Option<String>,
    pub allowed: Vec<i64>,
    pub api_base: String,
    pub queue_lifetime: Duration,
    pub queue_max: usize,
    pub receive: Receive,
}

impl TelegramConfig {
    /// Reads `HUB_TELEGRAM_BOT_TOKEN`, `HUB_TELEGRAM_ALLOWED_USERS` (comma-separated numeric
    /// ids), `HUB_TELEGRAM_API` (default the public Bot API), `HUB_TELEGRAM_QUEUE_MINUTES`
    /// (default 120), `HUB_TELEGRAM_QUEUE_MAX` (default 50) and `HUB_TELEGRAM_RECEIVE`
    /// (`poll`, the default, or `off`).
    pub fn from_env() -> Result<Self, String> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let allowed = parse_allowed(&var("HUB_TELEGRAM_ALLOWED_USERS").unwrap_or_default())?;
        let receive = parse_receive(var("HUB_TELEGRAM_RECEIVE").as_deref())?;
        let minutes = match var("HUB_TELEGRAM_QUEUE_MINUTES") {
            Some(m) => m
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|m| (1..=10_080).contains(m))
                .ok_or("HUB_TELEGRAM_QUEUE_MINUTES must be 1-10080")?,
            None => 120,
        };
        let queue_max = match var("HUB_TELEGRAM_QUEUE_MAX") {
            Some(m) => m
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|m| (1..=1000).contains(m))
                .ok_or("HUB_TELEGRAM_QUEUE_MAX must be 1-1000")?,
            None => 50,
        };
        Ok(TelegramConfig {
            token: var("HUB_TELEGRAM_BOT_TOKEN").map(|t| t.trim().to_owned()),
            allowed,
            api_base: var("HUB_TELEGRAM_API")
                .unwrap_or_else(|| "https://api.telegram.org".into())
                .trim_end_matches('/')
                .to_owned(),
            queue_lifetime: Duration::from_secs(minutes * 60),
            queue_max,
            receive,
        })
    }

    /// Why the bot is off, or `None` when it may run.
    pub fn disabled_reason(&self) -> Option<&'static str> {
        if self.token.is_none() {
            Some("no bot token is configured")
        } else if self.allowed.is_empty() {
            Some("no allowed user id is configured, and the bot denies everyone by default")
        } else {
            None
        }
    }
}

pub fn parse_receive(value: Option<&str>) -> Result<Receive, String> {
    match value.map(str::trim) {
        None | Some("poll") => Ok(Receive::Poll),
        Some("off") => Ok(Receive::Off),
        Some(_) => Err("HUB_TELEGRAM_RECEIVE must be poll or off".to_owned()),
    }
}

pub fn parse_allowed(spec: &str) -> Result<Vec<i64>, String> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| "HUB_TELEGRAM_ALLOWED_USERS must be numeric user ids".to_owned())
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Queued {
    id: String,
    text: String,
    sent_at_ms: i64,
    /// The line generation it went down, while it waits for Helm's acknowledgement.
    sent_on: Option<u64>,
    /// When it last went down that line, for the resend timer.
    sent_down_ms: i64,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Queued>,
    reply_chat: Option<i64>,
    status_id: Option<i64>,
    status_text: Option<String>,
    pending_status: Option<String>,
    last_edit_ms: Option<i64>,
    bot_name: Option<String>,
    /// The most recent `update_id`s handled, oldest first.
    seen: VecDeque<i64>,
}

/// What [`Bridge::on_update`] did, for tests and the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    /// Not a message, or a sender not on the allow-list: nothing forwarded, nothing sent.
    Ignored,
    Forwarded,
    Queued,
    Command,
    /// An `update_id` already handled: dropped.
    Duplicate,
}

impl Handled {
    pub fn as_str(self) -> &'static str {
        match self {
            Handled::Ignored => "ignored",
            Handled::Forwarded => "forwarded",
            Handled::Queued => "queued",
            Handled::Command => "command",
            Handled::Duplicate => "duplicate",
        }
    }
}

pub struct Bridge {
    api: Arc<dyn TelegramApi>,
    line: Arc<Line>,
    allowed: Vec<i64>,
    lifetime_ms: i64,
    queue_max: usize,
    receive: Receive,
    state: Mutex<State>,
    /// Serialises Telegram sends, so a status edit and the final answer never interleave.
    ops: tokio::sync::Mutex<()>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `/helm <text>` (or `/helm@<bot> <text>`) is read as `<text>`: on a bot another service
/// shares, its forwarder can route Helm's messages by that one command. `None`: no such prefix.
fn strip_helm(text: &str) -> Option<&str> {
    let t = text.trim_start();
    let (word, rest) = t.split_at(t.find(char::is_whitespace).unwrap_or(t.len()));
    (word.split('@').next() == Some("/helm")).then(|| rest.trim_start())
}

fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// Split a long answer into Telegram-sized pieces, on character boundaries.
pub fn split_text(s: &str, max: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return vec![];
    }
    chars.chunks(max).map(|c| c.iter().collect()).collect()
}

impl Bridge {
    pub fn new(api: Arc<dyn TelegramApi>, line: Arc<Line>, cfg: &TelegramConfig) -> Self {
        Bridge {
            api,
            line,
            allowed: cfg.allowed.clone(),
            lifetime_ms: cfg.queue_lifetime.as_millis() as i64,
            queue_max: cfg.queue_max,
            receive: cfg.receive,
            state: Mutex::new(State::default()),
            ops: tokio::sync::Mutex::new(()),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The chat replies and status go to: the last allowed chat, else the first allowed user's
    /// private chat (whose id is the user id).
    fn reply_chat(&self) -> i64 {
        self.st().reply_chat.unwrap_or(self.allowed[0])
    }

    pub fn queued_len(&self) -> usize {
        self.st().queue.len()
    }

    async fn send(&self, chat: i64, text: &str, silent: bool) -> Result<Value, ApiError> {
        self.api
            .call(
                "sendMessage",
                json!({ "chat_id": chat, "text": text, "disable_notification": silent }),
            )
            .await
    }

    async fn reply(&self, chat: i64, text: &str) {
        let _g = self.ops.lock().await;
        if let Err(e) = self.send(chat, text, false).await {
            eprintln!("telegram: reply failed: {}", e.description);
        }
    }

    /// Learn the bot's name (for Helm's chat title) with `getMe`.
    pub async fn identify(&self) -> Result<(), ApiError> {
        let me = self.api.call("getMe", json!({})).await?;
        let name = me
            .get("username")
            .or_else(|| me.get("first_name"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.st().bot_name = name;
        Ok(())
    }

    /// One update, from `getUpdates` or posted to `/telegram/update`.
    pub async fn on_update(&self, update: &Value, now: i64) -> Handled {
        if let Some(id) = update.get("update_id").and_then(Value::as_i64) {
            let mut st = self.st();
            if st.seen.contains(&id) {
                return Handled::Duplicate;
            }
            if st.seen.len() >= SEEN_UPDATES {
                st.seen.pop_front();
            }
            st.seen.push_back(id);
        }
        let Some(msg) = update.get("message") else {
            return Handled::Ignored;
        };
        let from = msg.pointer("/from/id").and_then(Value::as_i64);
        // Deny by default: only a sender on the allow-list gets anything at all.
        if !from.is_some_and(|f| self.allowed.contains(&f)) {
            return Handled::Ignored;
        }
        let Some(chat) = msg.pointer("/chat/id").and_then(Value::as_i64) else {
            return Handled::Ignored;
        };
        if msg.pointer("/chat/type").and_then(Value::as_str) != Some("private") {
            return Handled::Ignored;
        }
        self.st().reply_chat = Some(chat);
        let Some(text) = msg.get("text").and_then(Value::as_str) else {
            self.reply(chat, TEXT_ONLY).await;
            return Handled::Command;
        };
        let text = match strip_helm(text) {
            Some("") => {
                self.reply(chat, HELLO).await;
                return Handled::Command;
            }
            Some(rest) => rest,
            None => text,
        };
        let command = text
            .split_whitespace()
            .next()
            .filter(|w| w.starts_with('/'))
            .map(|w| w.split('@').next().unwrap_or(w));
        match command {
            Some("/cancel") => {
                let n = {
                    let mut st = self.st();
                    let before = st.queue.len();
                    // Messages already down the line are Helm's now; only waiting ones go.
                    st.queue.retain(|q| q.sent_on.is_some());
                    before - st.queue.len()
                };
                let text = match n {
                    0 => "Nothing was queued.".to_owned(),
                    1 => "Dropped 1 queued message.".to_owned(),
                    n => format!("Dropped {n} queued messages."),
                };
                self.reply(chat, &text).await;
                return Handled::Command;
            }
            Some("/stop") => {
                let sent = self.line.send(&json!({ "type": "telegram_stop" }));
                let text = if sent {
                    "Stopping the running turn."
                } else {
                    OFFLINE
                };
                self.reply(chat, text).await;
                return Handled::Command;
            }
            Some("/start") | Some("/help") => {
                self.reply(chat, HELLO).await;
                return Handled::Command;
            }
            _ => {}
        }
        let full = {
            let mut st = self.st();
            if st.queue.iter().filter(|q| q.sent_on.is_none()).count() >= self.queue_max {
                true
            } else {
                st.queue.push_back(Queued {
                    id: uuid::Uuid::new_v4().to_string(),
                    text: text.to_owned(),
                    sent_at_ms: msg
                        .get("date")
                        .and_then(Value::as_i64)
                        .map(|s| s * 1000)
                        .unwrap_or(now),
                    sent_on: None,
                    sent_down_ms: now,
                });
                false
            }
        };
        if full {
            self.reply(
                chat,
                "Helm is offline and the queue is full, so this message was not kept.",
            )
            .await;
            return Handled::Queued;
        }
        if self.flush(now).await > 0 {
            Handled::Forwarded
        } else {
            self.reply(chat, QUEUED).await;
            Handled::Queued
        }
    }

    /// Expire old messages, take back unacknowledged ones from a line that went away, and send
    /// what waits down the current line, in order, along with any message that line has not
    /// acknowledged within [`RESEND_AFTER`]. Returns how many were sent.
    pub async fn flush(&self, now: i64) -> usize {
        let generation = self.line.generation();
        let resend_ms = RESEND_AFTER.as_millis() as i64;
        let (frames, expired, undelivered) = {
            let mut st = self.st();
            for q in st.queue.iter_mut() {
                if q.sent_on.is_some() && q.sent_on != generation {
                    q.sent_on = None; // that line is gone; resend on the next
                }
            }
            // Past the lifetime a message goes, waiting or unacknowledged alike: one Helm keeps
            // refusing would otherwise be resent for ever.
            let lifetime = self.lifetime_ms;
            let (mut expired, mut undelivered) = (0, 0);
            st.queue.retain(|q| {
                let keep = now - q.sent_at_ms <= lifetime;
                match (keep, q.sent_on) {
                    (false, None) => expired += 1,
                    (false, Some(_)) => undelivered += 1,
                    _ => {}
                }
                keep
            });
            let bot = st.bot_name.clone();
            let mut frames = vec![];
            if let Some(gen) = generation {
                let due = |q: &Queued| q.sent_on.is_none() || now - q.sent_down_ms >= resend_ms;
                for q in st.queue.iter_mut().filter(|q| due(q)) {
                    let age = (now - q.sent_at_ms) / 60_000;
                    frames.push((
                        json!({
                            "type": "telegram",
                            "id": q.id,
                            "text": q.text,
                            "sentAt": q.sent_at_ms,
                            "ageMinutes": if age >= 1 { json!(age) } else { Value::Null },
                            "bot": bot,
                        }),
                        q.id.clone(),
                    ));
                    q.sent_on = Some(gen);
                    q.sent_down_ms = now;
                }
            }
            (frames, expired, undelivered)
        };
        let mut sent = 0;
        for (frame, id) in frames {
            if self.line.send(&frame) {
                sent += 1;
            } else {
                // The line dropped mid-flush: this one and the rest wait for the next.
                let mut st = self.st();
                for q in st.queue.iter_mut() {
                    if q.id == id || q.sent_on == generation {
                        q.sent_on = None;
                    }
                }
                break;
            }
        }
        if expired > 0 {
            let chat = self.reply_chat();
            let text = if expired == 1 {
                "1 queued message expired before Helm reconnected and was dropped.".to_owned()
            } else {
                format!(
                    "{expired} queued messages expired before Helm reconnected and were dropped."
                )
            };
            self.reply(chat, &text).await;
        }
        if undelivered > 0 {
            let chat = self.reply_chat();
            let text = if undelivered == 1 {
                "1 message could not be delivered to Helm in time and was dropped.".to_owned()
            } else {
                format!(
                    "{undelivered} messages could not be delivered to Helm in time and were \
                     dropped."
                )
            };
            self.reply(chat, &text).await;
        }
        sent
    }

    /// Helm has the message: it leaves the queue for good.
    pub fn on_ack(&self, id: &str) {
        self.st().queue.retain(|q| q.id != id);
    }

    pub fn on_status(&self, text: &str) {
        self.st().pending_status = Some(cut(text.trim(), MAX_STATUS));
    }

    /// Send or edit the status message, at most once per [`EDIT_INTERVAL`].
    pub async fn flush_status(&self, now: i64) {
        let _g = self.ops.lock().await;
        let (text, id) = {
            let st = self.st();
            let Some(text) = st.pending_status.clone() else {
                return;
            };
            if st
                .last_edit_ms
                .is_some_and(|t| now - t < EDIT_INTERVAL.as_millis() as i64)
            {
                return;
            }
            if st.status_text.as_deref() == Some(text.as_str()) {
                drop(st);
                self.st().pending_status = None;
                return;
            }
            (text, st.status_id)
        };
        if text.is_empty() {
            self.st().pending_status = None;
            return;
        }
        let chat = self.reply_chat();
        let r = match id {
            None => self.send(chat, &text, true).await,
            Some(mid) => {
                self.api
                    .call(
                        "editMessageText",
                        json!({ "chat_id": chat, "message_id": mid, "text": text }),
                    )
                    .await
            }
        };
        let mut st = self.st();
        st.last_edit_ms = Some(now);
        match r {
            Ok(v) => {
                if id.is_none() {
                    st.status_id = v.get("message_id").and_then(Value::as_i64);
                }
                st.status_text = Some(text.clone());
                if st.pending_status.as_deref() == Some(text.as_str()) {
                    st.pending_status = None;
                }
            }
            // Rate-limited or refused: the pending text stays and is tried after the interval.
            Err(e) => eprintln!("telegram: status update failed: {}", e.description),
        }
    }

    /// The turn's answer: a new message (so the phone notifies), then the status message goes.
    pub async fn on_final(&self, text: &str) {
        let _g = self.ops.lock().await;
        let chat = self.reply_chat();
        let status = {
            let mut st = self.st();
            st.pending_status = None;
            st.status_text = None;
            st.last_edit_ms = None;
            st.status_id.take()
        };
        for piece in split_text(text.trim(), MAX_TEXT) {
            if let Err(e) = self.send(chat, &piece, false).await {
                eprintln!("telegram: answer failed: {}", e.description);
            }
        }
        if let Some(mid) = status {
            if let Err(e) = self
                .api
                .call(
                    "deleteMessage",
                    json!({ "chat_id": chat, "message_id": mid }),
                )
                .await
            {
                eprintln!(
                    "telegram: removing the status message failed: {}",
                    e.description
                );
            }
        }
    }

    /// A message Helm starts (an alert): a new message, so the phone notifies. Unlike a final
    /// answer it leaves the live status message where it is, since a turn may still be running.
    pub async fn on_notify(&self, text: &str) {
        let _g = self.ops.lock().await;
        let chat = self.reply_chat();
        for piece in split_text(text.trim(), MAX_TEXT) {
            if let Err(e) = self.send(chat, &piece, false).await {
                eprintln!("telegram: alert failed: {}", e.description);
            }
        }
    }

    /// Handle one frame Helm sent up the line.
    pub async fn on_upstream(&self, u: Upstream) {
        match u {
            Upstream::TelegramAck(id) => self.on_ack(&id),
            Upstream::TelegramStatus(t) => self.on_status(&t),
            Upstream::TelegramFinal(t) => self.on_final(&t).await,
            Upstream::TelegramNotify(t) => self.on_notify(&t).await,
        }
    }

    /// Run the bot: tick the queue and status, consume Helm's frames, and long-poll for updates
    /// unless another service that already runs the bot posts them.
    pub async fn run(
        self: Arc<Self>,
        mut upstream: tokio::sync::mpsc::UnboundedReceiver<Upstream>,
    ) {
        if let Err(e) = self.identify().await {
            eprintln!("telegram: getMe failed: {}", e.description);
        }
        let ticker = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(TICK);
            loop {
                iv.tick().await;
                let now = now_ms();
                ticker.flush(now).await;
                ticker.flush_status(now).await;
            }
        });
        let up = self.clone();
        tokio::spawn(async move {
            while let Some(u) = upstream.recv().await {
                up.on_upstream(u).await;
            }
        });
        if self.receive == Receive::Off {
            return;
        }
        let mut offset: i64 = 0;
        let mut backoff = Duration::from_secs(1);
        loop {
            let r = self
                .api
                .call(
                    "getUpdates",
                    json!({ "offset": offset, "timeout": POLL_SECONDS, "allowed_updates": ["message"] }),
                )
                .await;
            match r {
                Ok(Value::Array(updates)) => {
                    backoff = Duration::from_secs(1);
                    for u in updates {
                        if let Some(id) = u.get("update_id").and_then(Value::as_i64) {
                            offset = offset.max(id + 1);
                        }
                        self.on_update(&u, now_ms()).await;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("telegram: getUpdates failed: {}", e.description);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    }
}

/// The Bot API over HTTPS (or plain http for a local fake).
pub struct HttpApi {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl HttpApi {
    pub fn new(base: &str, token: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            // A long poll plus headroom; any other call answers well inside it.
            .timeout_global(Some(Duration::from_secs(POLL_SECONDS + 15)))
            .http_status_as_error(false)
            .build()
            .into();
        HttpApi {
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            agent,
        }
    }

    fn call_blocking(
        agent: &ureq::Agent,
        url: &str,
        token: &str,
        body: &Value,
    ) -> Result<Value, ApiError> {
        // Errors are reported without the URL, which carries the bot token.
        let scrub = |s: String| s.replace(token, "<token>");
        let mut resp = agent.post(url).send_json(body).map_err(|e| ApiError {
            code: None,
            description: scrub(format!("request failed: {e}")),
        })?;
        let v: Value = resp.body_mut().read_json().map_err(|e| ApiError {
            code: None,
            description: scrub(format!("unreadable reply: {e}")),
        })?;
        if v.get("ok") == Some(&Value::Bool(true)) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(ApiError {
                code: v.get("error_code").and_then(Value::as_i64),
                description: scrub(
                    v.get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("the Bot API refused the call")
                        .to_owned(),
                ),
            })
        }
    }
}

impl TelegramApi for HttpApi {
    fn call<'a>(&'a self, method: &'a str, body: Value) -> ApiFuture<'a> {
        let url = format!("{}/bot{}/{}", self.base, self.token, method);
        let agent = self.agent.clone();
        let token = self.token.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || Self::call_blocking(&agent, &url, &token, &body))
                .await
                .unwrap_or_else(|e| {
                    Err(ApiError {
                        code: None,
                        description: format!("the call did not finish: {e}"),
                    })
                })
        })
    }
}
