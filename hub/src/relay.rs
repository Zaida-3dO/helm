//! The relay line.
//!
//! Each machine running Helm opens one outbound WebSocket to `/relay`, authenticated with
//! its own client token (the `relay` scope), and keeps it open. Everything the hub needs Helm
//! for travels down that line: forwarded tool calls from MCP callers, household calls from other
//! people's Helms and messages from the Telegram bot. The PC needs no inbound port.
//!
//! The hub keeps one [`Line`] per client (see `lines.rs`): a newer connection from the *same*
//! client replaces the older one, and different clients' lines coexist.
//!
//! Line protocol: one JSON object per text frame, `type` names it.
//!
//! | direction | type | fields |
//! |---|---|---|
//! | hub → Helm | `welcome` | `version`, and who this line is: `personId`, `person`, `machineId`, `machine` |
//! | hub → Helm | `call` | `id`, `client`, `tool`, `args`. `client` names the caller: a client's name, or `peer:<person>` for a household member's call |
//! | hub → Helm | `telegram` | `id`, `text`, `sentAt` (ms), `ageMinutes` (null when fresh), `bot`. A voice note's `text` is `[Voice note] <transcript>`; a reply or quote puts `Replying to: "<quoted text>"` on the first line. With files: `images` (pictures Helm shows) and `files` (anything else), each a list of `name`, `mediaType` and base64 `data`; at most 10 files and 50 MB in all |
//! | hub → Helm | `telegram_stop` | — |
//! | hub → Helm | `telegram_answer` | `id`, `index`: the user tapped button `index` (from 0) of the ask `id`. Sent once per ask |
//! | Helm → hub | `sharing` | `chats`, `transcripts`, `messages` (booleans, absent means false): what the household may do with this machine's chats. Sent on connect and whenever a switch changes; reset to all false when the line drops |
//! | Helm → hub | `peer` | `id`, `op` (`people`, `list_chats`, `read_chat` or `send_message`), `args` (`personId`, optional `machineId`, and the op's own): a question about the household, answered with `peer_result` (see `peer.rs`) |
//! | hub → Helm | `peer_result` | `id` (the `peer` frame's), `ok`, `result` or `error` (a plain sentence) |
//! | hub → Helm | `peer_call` | `id`, `person` (the asker's display name), `op` (`list_chats`, `read_chat` or `send_message`), `args` (`limit`, `offset` / `chatId`, `beforeMessageId`, `limit` / `chatId`, `message`): another person's Helm asking about this machine's chats. Answered with an ordinary `result` frame |
//! | Helm → hub | `result` | `id`, `ok`, `result` or `error`, `changed` (`no` or `maybe`) |
//! | Helm → hub | `telegram_ack` | `id` |
//! | Helm → hub | `telegram_status` | `text`: the one live status message, edited in place |
//! | Helm → hub | `telegram_final` | `text`: the turn's answer, sent as a new message |
//! | Helm → hub | `telegram_notify` | `text`: a message Helm starts (an alert), sent as a new message; the live status is left alone |
//! | Helm → hub | `telegram_ask` | `id` (16-32 lowercase hex), `text`, `options` (1-4 strings): a new message with one button per option |
//! | Helm → hub | `telegram_ask_done` | `id`, `text`: the ask was settled in Helm; its message gains `text` and loses its buttons |
//! | hub → Helm | `voice` | `id` (the turn), `mode` (`conversation` or `chat`), `text` (at most 8000 characters), `source` (`speech` or `typed`), `sentAt` (ms), `ageMinutes` (null when fresh), `continues` (a turn id or null). One turn for the voice chat; sent again every 45 s until `voice_ack` |
//! | hub → Helm | `voice_stop` | `turn`: the phone cancelled a turn Helm may already have taken |
//! | hub → Helm | `voice_heard` | `reports`: what the phone did and did not play (see `voice.rs`) |
//! | hub → Helm | `voice_presence` | `live`, `mode`: whether the phone's talk screen is open, and in which mode |
//! | hub → Helm | `voice_fresh` | — : start a new voice conversation |
//! | Helm → hub | `voice_ack` | `id`: the turn is placed in the voice chat |
//! | Helm → hub | `voice_status` | `turn`, `text` (at most 300 characters): the working line, shown and never spoken |
//! | Helm → hub | `voice_attachment` | `reply`, `n` (1-4), `kind`, `title`, and by kind `mediaType` + `data` (base64), `markdown` or `url`: one attachment, sent before its reply and held for 60 s |
//! | Helm → hub | `voice_final` | `id` (the reply), `turn`, `mode`, `sentences`, `text`, `truncated`, `conversation`, `at`, `attachments` (a count): the answer to a turn |
//! | Helm → hub | `voice_notify` | as `voice_final`, with `turn` null and `about` (`{"chat": title}` or null): news Helm starts |
//! | Helm → hub | `device` | `uid`, `name`, `run` (whether other devices may run this Helm's chats): sent after `sharing` on every line-up and on change. Only a token with the `peer` scope takes part; the hub answers every `peer` line with a fresh `devices` |
//! | hub → Helm | `devices` | `list`: every device of the household, `{uid, name, personId, person, online, lastSeenAt, run, self}`; sent to each `peer` line when a device frame arrives and whenever a line comes up or drops |
//! | Helm → hub | `dev` | `id`, `to` (a device uid), `op`, `args`: a call for another device, answered with `dev_result` (see `devices.rs`) |
//! | hub → Helm | `dev_call` | `id` (the hub's), `from` (`{uid, name, personId, person}`), `op`, `args`: another device's call for this one |
//! | Helm → hub | `dev_result` | `id` (the `dev_call`'s), `ok`, `result` or `error` (`{code, message}`): the answer to a `dev_call` |
//! | hub → Helm | `dev_result` | `id` (the `dev` frame's), `ok`, `result` or `error` (`{code, message}`). The hub's own errors: `offline`, `timeout`, `scope`, `too_large`, `bad_request` |
//! | Helm → hub | `dev_push` | `to` (a device uid), `sub`, `seq`, `event`: handed on to that device, or dropped when it is not up |
//! | hub → Helm | `dev_push` | `from` (the sender's device uid), `sub`, `seq`, `event` |
//! | Helm → hub | `kn_pull` | `id`, `since` (a hub sequence), `limit` (at most 200): the knowledge files changed since (see `knowledge.rs`) |
//! | hub → Helm | `kn_files` | `id`, `files` (`{layer, path, seq, sha256, size, deleted, body}`, body in base64 and left out for a deleted file), `cursor`, `more`; or `id` and `error` |
//! | Helm → hub | `kn_put` | `id`, `layer`, `path`, `baseSeq` (0 for a new file), `sha256`, `deleted`, `body` (base64) |
//! | hub → Helm | `kn_put_result` | `id`, `ok`, `seq`; or `ok` false with `conflict` (`{seq, sha256}`) or `error` |
//! | hub → Helm | `kn_changed` | `seq`: some device changed a knowledge file; sent to every `peer` line |
//!
//! New frames go in the table above, a `match` arm in [`Line::receive`] and, for what Helm sends
//! up that is not a call result, an [`Upstream`] variant.
//!
//! A newer line from the same client replaces an older one (the PC reconnected before the old
//! socket timed out). When the line drops, calls waiting on it are answered
//! "may or may not have run": they were sent, so the hub cannot say nothing
//! changed.

use crate::voice::{Attachment, ReplyIn, MAX_STATUS_CHARS};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub const PROTOCOL_VERSION: u64 = 1;

/// The fixed answer when the line is down: nothing was sent, so nothing changed.
pub const OFFLINE: &str = "Helm offline, nothing sent or changed.";

/// How long a forwarded call may wait for Helm's answer.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Largest frame accepted from Helm: one voice attachment holds an image of up to 1 MiB, which is
/// about 1.4 MB as base64, and one knowledge file of up to 2 MiB is about 2.8 MB.
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

/// Largest `dev`, `dev_result` or `dev_push` frame the hub passes between devices.
pub const MAX_DEV_FRAME: usize = 1024 * 1024;

/// What a `dev_result` that was over [`MAX_DEV_FRAME`] is turned into on its way to the waiting
/// call, so the router can answer the sender with `too_large`.
pub const TOO_LARGE: &str = "too_large";

/// What Helm sent up the line that is not a call result.
#[derive(Debug, Clone, PartialEq)]
pub enum Upstream {
    TelegramAck(String),
    TelegramStatus(String),
    TelegramFinal(String),
    TelegramNotify(String),
    TelegramAsk {
        id: String,
        text: String,
        options: Vec<String>,
    },
    TelegramAskDone {
        id: String,
        text: String,
    },
    /// A question about the household. The line's owner (`lines.rs`) answers it; the Telegram
    /// bridge never sees it.
    Peer {
        id: Value,
        op: String,
        args: Value,
    },
    /// The turn is placed in the voice chat. The voice frames go to the voice bridge, never to
    /// the Telegram one.
    VoiceAck(String),
    VoiceStatus {
        turn: String,
        text: String,
    },
    VoiceAttachment(Attachment),
    VoiceFinal(ReplyIn),
    VoiceNotify(ReplyIn),
    /// The line's Helm announced the device it is. Answered by `devices.rs`.
    Device {
        uid: String,
        name: String,
        run: bool,
    },
    /// A call for another device. `size` is the frame's length, for the `too_large` check.
    Dev {
        id: Value,
        to: String,
        op: String,
        args: Value,
        size: usize,
    },
    DevPush {
        to: String,
        sub: Value,
        seq: Value,
        event: Value,
    },
    /// Knowledge frames, answered by `knowledge.rs`.
    KnPull {
        id: Value,
        since: i64,
        limit: usize,
    },
    KnPut {
        id: Value,
        layer: String,
        path: String,
        base_seq: i64,
        sha256: String,
        deleted: bool,
        body: Option<String>,
    },
}

impl Upstream {
    /// A frame for the voice bridge.
    pub fn is_voice(&self) -> bool {
        matches!(
            self,
            Upstream::VoiceAck(_)
                | Upstream::VoiceStatus { .. }
                | Upstream::VoiceAttachment(_)
                | Upstream::VoiceFinal(_)
                | Upstream::VoiceNotify(_)
        )
    }
}

/// An ask's id: 16 to 32 lowercase hex characters, so it fits a button's callback data.
pub fn valid_ask_id(id: &str) -> bool {
    (16..=32).contains(&id.len()) && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The outcome of a forwarded call, as the MCP caller sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum CallOutcome {
    Ok(Value),
    /// A refusal or failure from Helm; `changed` is false when Helm says nothing changed.
    Err {
        text: String,
        changed: bool,
    },
    /// The line is down: nothing was sent.
    Offline,
    /// Sent, but no answer came back (timeout or the line dropped mid-call).
    Unknown(String),
}

/// What the household may do with one machine's chats. All off until Helm says otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Sharing {
    /// Others may list this machine's chats.
    pub chats: bool,
    /// Others may read their transcripts.
    pub transcripts: bool,
    /// Others may send messages into them.
    pub messages: bool,
}

/// Where the Telegram bridge sends frames and learns whether anyone can receive them: one
/// [`Line`], or all of a person's machines (see `lines.rs`).
pub trait Route: Send + Sync {
    fn is_up(&self) -> bool;
    /// Identifies the connection frames go down, so an unacknowledged message can be resent when
    /// the connection changes.
    fn generation(&self) -> Option<u64>;
    fn send(&self, frame: &Value) -> bool;
    /// Whether the connection `generation` is still up on this route. A person's route has one
    /// per machine, so a message stays with the machine it went to while that one is up.
    fn holds(&self, generation: u64) -> bool {
        self.generation() == Some(generation)
    }
    /// Send down the connection `generation` only; false when it is not up.
    fn send_on(&self, generation: u64, frame: &Value) -> bool {
        self.holds(generation) && self.send(frame)
    }
    /// Ask this route's Helm to free the graphics memory speech-to-text needs, waiting at most
    /// `wait` and ignoring any failure (see [`crate::stt::make_room_on`]). A route that is one
    /// bare [`Line`] does nothing; a person's route (see `lines.rs`) asks their Helm.
    fn make_room(&self, _wait: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

/// Orders lines by how recently they did something worth routing to.
static ACTIVITY: AtomicU64 = AtomicU64::new(0);

fn next_activity() -> u64 {
    ACTIVITY.fetch_add(1, Ordering::SeqCst) + 1
}

struct Conn {
    generation: u64,
    tx: mpsc::UnboundedSender<String>,
}

#[derive(Default)]
struct Inner {
    conn: Option<Conn>,
    pending: HashMap<String, (u64, oneshot::Sender<CallOutcome>)>,
}

/// The hub's end of the line. One per hub.
pub struct Line {
    inner: Mutex<Inner>,
    /// Keeps generations of different clients' lines apart: the high 32 bits of every one.
    namespace: u64,
    generation: AtomicU64,
    /// When this line last attached or handled a Telegram frame ([`next_activity`]); 0 never.
    activity: AtomicU64,
    sharing: Mutex<Sharing>,
    upstream: mpsc::UnboundedSender<Upstream>,
    /// Told `true` when a connection attaches and `false` when the current one ends (the
    /// household's device list follows it).
    on_change: Mutex<Option<StateHook>>,
    /// Woken when a line comes up (the Telegram queue flushes on it).
    pub up: tokio::sync::Notify,
}

type StateHook = std::sync::Arc<dyn Fn(bool) + Send + Sync>;

impl Line {
    /// The line, plus the receiver for what Helm sends that is not a call result.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Upstream>) {
        Self::in_namespace(0)
    }

    /// A line whose generations never equal another namespace's (one namespace per client).
    pub fn in_namespace(namespace: u64) -> (Self, mpsc::UnboundedReceiver<Upstream>) {
        let (upstream, rx) = mpsc::unbounded_channel();
        (
            Line {
                inner: Mutex::new(Inner::default()),
                namespace: namespace & 0x7fff_ffff,
                generation: AtomicU64::new(0),
                activity: AtomicU64::new(0),
                sharing: Mutex::new(Sharing::default()),
                upstream,
                on_change: Mutex::new(None),
                up: tokio::sync::Notify::new(),
            },
            rx,
        )
    }

    /// Call `hook` with `true` after a connection attaches and `false` after the current one
    /// ends. It runs with no lock of the line held.
    pub fn on_change(&self, hook: impl Fn(bool) + Send + Sync + 'static) {
        *self.on_change.lock().unwrap_or_else(|p| p.into_inner()) = Some(std::sync::Arc::new(hook));
    }

    fn changed(&self, up: bool) {
        let hook = self
            .on_change
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook(up);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn is_up(&self) -> bool {
        self.lock().conn.as_ref().is_some_and(|c| !c.tx.is_closed())
    }

    /// The current connection's generation, or `None` when the line is down.
    pub fn generation(&self) -> Option<u64> {
        self.lock()
            .conn
            .as_ref()
            .filter(|c| !c.tx.is_closed())
            .map(|c| c.generation)
    }

    /// What the household may do with this machine's chats, as the current line last said.
    pub fn sharing(&self) -> Sharing {
        *self.sharing.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn set_sharing(&self, s: Sharing) {
        *self.sharing.lock().unwrap_or_else(|p| p.into_inner()) = s;
    }

    /// How recently this line attached or handled a Telegram frame; higher is later.
    pub fn activity(&self) -> u64 {
        self.activity.load(Ordering::SeqCst)
    }

    fn bump(&self) {
        self.activity.store(next_activity(), Ordering::SeqCst);
    }

    /// Install a new connection; returns its generation. Replaces (and so closes) any older one.
    pub fn attach(&self, tx: mpsc::UnboundedSender<String>) -> u64 {
        self.attach_as(tx, json!({}))
    }

    /// [`Line::attach`], with `who` (an object) merged into the welcome frame.
    pub fn attach_as(&self, tx: mpsc::UnboundedSender<String>, who: Value) -> u64 {
        let generation =
            (self.namespace << 32) | (self.generation.fetch_add(1, Ordering::SeqCst) + 1);
        let mut welcome = json!({ "type": "welcome", "version": PROTOCOL_VERSION });
        if let (Some(w), Some(extra)) = (welcome.as_object_mut(), who.as_object()) {
            for (k, v) in extra {
                w.insert(k.clone(), v.clone());
            }
        }
        let _ = tx.send(welcome.to_string());
        // A new connection starts private until its Helm says what is shared.
        self.set_sharing(Sharing::default());
        self.bump();
        let old = {
            let mut g = self.lock();
            let old = g.conn.replace(Conn { generation, tx });
            // Calls sent down the old connection can no longer be answered.
            let stale: Vec<String> = g
                .pending
                .iter()
                .filter(|(_, (gen, _))| *gen != generation)
                .map(|(id, _)| id.clone())
                .collect();
            let senders: Vec<_> = stale
                .into_iter()
                .filter_map(|id| g.pending.remove(&id))
                .collect();
            (old, senders)
        };
        for (_, s) in old.1 {
            let _ = s.send(CallOutcome::Unknown(dropped_text()));
        }
        drop(old.0);
        self.up.notify_waiters();
        self.changed(true);
        generation
    }

    /// The connection of `generation` ended. A newer connection is left alone.
    pub fn detach(&self, generation: u64) {
        let senders = {
            let mut g = self.lock();
            if g.conn.as_ref().is_none_or(|c| c.generation != generation) {
                return;
            }
            g.conn = None;
            self.set_sharing(Sharing::default());
            let ids: Vec<String> = g
                .pending
                .iter()
                .filter(|(_, (gen, _))| *gen == generation)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| g.pending.remove(&id))
                .collect::<Vec<_>>()
        };
        for (_, s) in senders {
            let _ = s.send(CallOutcome::Unknown(dropped_text()));
        }
        self.changed(false);
    }

    /// Send one frame down the current line. False when the line is down.
    pub fn send(&self, frame: &Value) -> bool {
        let g = self.lock();
        match &g.conn {
            Some(c) => c.tx.send(frame.to_string()).is_ok(),
            None => false,
        }
    }

    /// Forward one tool call and wait for Helm's answer.
    pub async fn call(&self, client: &str, tool: &str, args: &Value) -> CallOutcome {
        self.call_with_timeout(client, tool, args, CALL_TIMEOUT)
            .await
    }

    pub async fn call_with_timeout(
        &self,
        client: &str,
        tool: &str,
        args: &Value,
        timeout: Duration,
    ) -> CallOutcome {
        self.request(
            timeout,
            |id| json!({ "type": "call", "id": id, "client": client, "tool": tool, "args": args }),
        )
        .await
    }

    /// [`Line::call`] for a call made on the Remote page: the frame says `"scope": "remote"`, which
    /// is what lets Helm's gate admit the page's own tools. Only the hub sets it, and only for a
    /// token with the `remote` scope.
    pub async fn call_remote(&self, client: &str, tool: &str, args: &Value) -> CallOutcome {
        self.request(CALL_TIMEOUT, |id| {
            json!({ "type": "call", "id": id, "client": client, "scope": "remote",
                "tool": tool, "args": args })
        })
        .await
    }

    /// Ask this machine's Helm a household question for `person` and wait for its answer.
    pub async fn peer_call(
        &self,
        person: &str,
        op: &str,
        args: &Value,
        timeout: Duration,
    ) -> CallOutcome {
        self.request(
            timeout,
            |id| json!({ "type": "peer_call", "id": id, "person": person, "op": op, "args": args }),
        )
        .await
    }

    /// Pass another device's call to this one and wait for its `dev_result`, which comes back
    /// whole (`CallOutcome::Ok` holds the frame) so the caller can read its `ok`, `result` and
    /// `error {code, message}`. A frame over [`MAX_DEV_FRAME`] comes back as
    /// `CallOutcome::Err` with [`TOO_LARGE`].
    pub async fn dev_call(
        &self,
        from: &Value,
        op: &str,
        args: &Value,
        timeout: Duration,
    ) -> CallOutcome {
        self.request(
            timeout,
            |id| json!({ "type": "dev_call", "id": id, "from": from, "op": op, "args": args }),
        )
        .await
    }

    /// Send the frame `build` makes for a fresh id and wait for the `result` frame with that id.
    async fn request(&self, timeout: Duration, build: impl FnOnce(&str) -> Value) -> CallOutcome {
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut g = self.lock();
            let Some(conn) = g.conn.as_ref() else {
                return CallOutcome::Offline;
            };
            let frame = build(&id);
            let generation = conn.generation;
            if conn.tx.send(frame.to_string()).is_err() {
                return CallOutcome::Offline;
            }
            g.pending.insert(id.clone(), (generation, tx));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => CallOutcome::Unknown(dropped_text()),
            Err(_) => {
                self.lock().pending.remove(&id);
                CallOutcome::Unknown(format!(
                    "Helm did not answer within {} s; the call may or may not have run. \
                     Check the current state before trying again.",
                    timeout.as_secs()
                ))
            }
        }
    }

    /// One frame from Helm. Malformed frames are dropped (and reported as false).
    pub fn receive(&self, generation: u64, text: &str) -> bool {
        if text.len() > MAX_FRAME {
            return false;
        }
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return false;
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        match v.get("type").and_then(Value::as_str) {
            Some("result") => {
                let Some(id) = s("id") else { return false };
                let sender = {
                    let mut g = self.lock();
                    match g.pending.get(&id) {
                        // Only the connection the call went down may answer it.
                        Some((gen, _)) if *gen == generation => g.pending.remove(&id),
                        _ => None,
                    }
                };
                let Some((_, sender)) = sender else {
                    return false;
                };
                let outcome = if v.get("ok") == Some(&Value::Bool(true)) {
                    CallOutcome::Ok(v.get("result").cloned().unwrap_or(Value::Null))
                } else {
                    CallOutcome::Err {
                        text: s("error").unwrap_or_else(|| "Helm refused the call.".into()),
                        changed: v.get("changed").and_then(Value::as_str) != Some("no"),
                    }
                };
                let _ = sender.send(outcome);
                true
            }
            Some("sharing") => {
                let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
                self.set_sharing(Sharing {
                    chats: flag("chats"),
                    transcripts: flag("transcripts"),
                    messages: flag("messages"),
                });
                true
            }
            Some("peer") => match (v.get("id"), s("op")) {
                (Some(id), Some(op)) if id.is_string() || id.is_number() => {
                    let args = match v.get("args") {
                        Some(a) if a.is_object() => a.clone(),
                        _ => json!({}),
                    };
                    self.upstream
                        .send(Upstream::Peer {
                            id: id.clone(),
                            op,
                            args,
                        })
                        .is_ok()
                }
                _ => false,
            },
            Some("telegram_ack") => match s("id") {
                Some(id) => {
                    self.bump();
                    self.upstream.send(Upstream::TelegramAck(id)).is_ok()
                }
                None => false,
            },
            Some("telegram_status") => match s("text") {
                Some(t) => {
                    self.bump();
                    self.upstream.send(Upstream::TelegramStatus(t)).is_ok()
                }
                None => false,
            },
            Some("telegram_final") => match s("text") {
                Some(t) => {
                    self.bump();
                    self.upstream.send(Upstream::TelegramFinal(t)).is_ok()
                }
                None => false,
            },
            Some("telegram_notify") => match s("text") {
                Some(t) => {
                    self.bump();
                    self.upstream.send(Upstream::TelegramNotify(t)).is_ok()
                }
                None => false,
            },
            Some("telegram_ask") => {
                // Telegram refuses an empty message or button, so such an ask is malformed.
                let options: Option<Vec<String>> = v
                    .get("options")
                    .and_then(Value::as_array)
                    .and_then(|a| a.iter().map(|o| o.as_str().map(str::to_owned)).collect());
                match (s("id"), s("text"), options) {
                    (Some(id), Some(text), Some(options))
                        if valid_ask_id(&id)
                            && !text.trim().is_empty()
                            && (1..=4).contains(&options.len())
                            && options.iter().all(|o| !o.trim().is_empty()) =>
                    {
                        self.bump();
                        self.upstream
                            .send(Upstream::TelegramAsk { id, text, options })
                            .is_ok()
                    }
                    _ => false,
                }
            }
            Some("telegram_ask_done") => match (s("id"), s("text")) {
                (Some(id), Some(text)) => self
                    .upstream
                    .send(Upstream::TelegramAskDone { id, text })
                    .is_ok(),
                _ => false,
            },
            Some("voice_ack") => match s("id") {
                Some(id) => {
                    self.bump();
                    self.upstream.send(Upstream::VoiceAck(id)).is_ok()
                }
                None => false,
            },
            Some("voice_status") => match (s("turn"), s("text")) {
                (Some(turn), Some(text)) => self
                    .upstream
                    .send(Upstream::VoiceStatus {
                        turn,
                        text: text.chars().take(MAX_STATUS_CHARS).collect(),
                    })
                    .is_ok(),
                _ => false,
            },
            Some("voice_attachment") => match Attachment::parse(&v) {
                Some(a) => self.upstream.send(Upstream::VoiceAttachment(a)).is_ok(),
                None => false,
            },
            Some("voice_final") => match ReplyIn::parse(&v, false) {
                Some(r) => {
                    self.bump();
                    self.upstream.send(Upstream::VoiceFinal(r)).is_ok()
                }
                None => false,
            },
            Some("voice_notify") => match ReplyIn::parse(&v, true) {
                Some(r) => {
                    self.bump();
                    self.upstream.send(Upstream::VoiceNotify(r)).is_ok()
                }
                None => false,
            },
            Some("device") => match (s("uid"), s("name")) {
                (Some(uid), Some(name)) => self
                    .upstream
                    .send(Upstream::Device {
                        uid,
                        name,
                        run: v.get("run").and_then(Value::as_bool).unwrap_or(false),
                    })
                    .is_ok(),
                _ => false,
            },
            Some("dev") => match (v.get("id"), s("to"), s("op")) {
                (Some(id), Some(to), Some(op)) if id.is_string() || id.is_number() => {
                    let args = match v.get("args") {
                        Some(a) if a.is_object() => a.clone(),
                        _ => json!({}),
                    };
                    self.upstream
                        .send(Upstream::Dev {
                            id: id.clone(),
                            to,
                            op,
                            args,
                            size: text.len(),
                        })
                        .is_ok()
                }
                _ => false,
            },
            Some("dev_result") => {
                let Some(id) = s("id") else { return false };
                let Some(sender) = self.take_pending(&id, generation) else {
                    return false;
                };
                let outcome = if text.len() > MAX_DEV_FRAME {
                    CallOutcome::Err {
                        text: TOO_LARGE.into(),
                        changed: true,
                    }
                } else {
                    CallOutcome::Ok(v.clone())
                };
                let _ = sender.send(outcome);
                true
            }
            Some("dev_push") => match (s("to"), v.get("sub"), v.get("seq"), v.get("event")) {
                (Some(to), Some(sub), Some(seq), Some(event)) if text.len() <= MAX_DEV_FRAME => {
                    self.upstream
                        .send(Upstream::DevPush {
                            to,
                            sub: sub.clone(),
                            seq: seq.clone(),
                            event: event.clone(),
                        })
                        .is_ok()
                }
                _ => false,
            },
            Some("kn_pull") => match v.get("id") {
                Some(id) if id.is_string() || id.is_number() => self
                    .upstream
                    .send(Upstream::KnPull {
                        id: id.clone(),
                        since: v.get("since").and_then(Value::as_i64).unwrap_or(0).max(0),
                        limit: v
                            .get("limit")
                            .and_then(Value::as_u64)
                            .map_or(200, |n| n.clamp(1, 200) as usize),
                    })
                    .is_ok(),
                _ => false,
            },
            Some("kn_put") => match (v.get("id"), s("layer"), s("path")) {
                (Some(id), Some(layer), Some(path)) if id.is_string() || id.is_number() => self
                    .upstream
                    .send(Upstream::KnPut {
                        id: id.clone(),
                        layer,
                        path,
                        base_seq: v.get("baseSeq").and_then(Value::as_i64).unwrap_or(0),
                        sha256: s("sha256").unwrap_or_default(),
                        deleted: v.get("deleted").and_then(Value::as_bool).unwrap_or(false),
                        body: s("body"),
                    })
                    .is_ok(),
                _ => false,
            },
            _ => false,
        }
    }

    /// The caller waiting on `id`, if the connection of `generation` is the one the call went
    /// down.
    fn take_pending(&self, id: &str, generation: u64) -> Option<oneshot::Sender<CallOutcome>> {
        let mut g = self.lock();
        match g.pending.get(id) {
            Some((gen, _)) if *gen == generation => g.pending.remove(id).map(|(_, s)| s),
            _ => None,
        }
    }
}

impl Route for Line {
    fn is_up(&self) -> bool {
        Line::is_up(self)
    }

    fn generation(&self) -> Option<u64> {
        Line::generation(self)
    }

    fn send(&self, frame: &Value) -> bool {
        Line::send(self, frame)
    }
}

fn dropped_text() -> String {
    "The line to Helm dropped during the call; it may or may not have run. Check the current \
     state before trying again."
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<Value> {
        let mut out = vec![];
        while let Ok(f) = rx.try_recv() {
            out.push(serde_json::from_str(&f).unwrap());
        }
        out
    }

    #[tokio::test]
    async fn a_call_with_no_line_sends_nothing() {
        let (line, _up) = Line::new();
        assert!(!line.is_up());
        let out = line.call("remote", "app_list_chats", &json!({})).await;
        assert_eq!(out, CallOutcome::Offline);
    }

    #[tokio::test]
    async fn a_call_is_answered_by_its_own_line_only() {
        let (line, _up) = Line::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let gen = line.attach(tx);
        assert_eq!(frames(&mut rx)[0]["type"], "welcome");
        let line = std::sync::Arc::new(line);
        let l2 = line.clone();
        let task =
            tokio::spawn(async move { l2.call("remote", "app_list_chats", &json!({})).await });
        tokio::task::yield_now().await;
        let call = loop {
            if let Some(f) = frames(&mut rx).pop() {
                break f;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(call["type"], "call");
        assert_eq!(call["client"], "remote");
        assert!(
            call.get("scope").is_none(),
            "an ordinary call carries no scope"
        );
        let id = call["id"].as_str().unwrap().to_owned();
        // A stale generation cannot answer.
        let answer =
            json!({"type": "result", "id": id, "ok": true, "result": {"n": 1}}).to_string();
        assert!(!line.receive(gen + 7, &answer));
        assert!(line.receive(gen, &answer));
        assert_eq!(task.await.unwrap(), CallOutcome::Ok(json!({"n": 1})));
    }

    #[tokio::test]
    async fn a_dropped_line_answers_waiting_calls_as_unknown() {
        let (line, _up) = Line::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let gen = line.attach(tx);
        let line = std::sync::Arc::new(line);
        let l2 = line.clone();
        let task =
            tokio::spawn(async move { l2.call("remote", "app_stop_chat", &json!({})).await });
        tokio::task::yield_now().await;
        line.detach(gen);
        assert!(matches!(task.await.unwrap(), CallOutcome::Unknown(_)));
        assert!(!line.is_up());
    }

    #[tokio::test]
    async fn a_refusal_that_changed_nothing_says_so() {
        let (line, _up) = Line::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let gen = line.attach(tx);
        let line = std::sync::Arc::new(line);
        let l2 = line.clone();
        let task = tokio::spawn(async move { l2.call("remote", "app_x", &json!({})).await });
        let call = loop {
            if let Some(f) = frames(&mut rx).into_iter().find(|f| f["type"] == "call") {
                break f;
            }
            tokio::task::yield_now().await;
        };
        let r = json!({"type": "result", "id": call["id"], "ok": false, "error": "denied", "changed": "no"});
        assert!(line.receive(gen, &r.to_string()));
        assert_eq!(
            task.await.unwrap(),
            CallOutcome::Err {
                text: "denied".into(),
                changed: false
            }
        );
    }

    #[tokio::test]
    async fn telegram_frames_go_upstream_and_junk_is_dropped() {
        let (line, mut up) = Line::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let gen = line.attach(tx);
        assert!(line.receive(gen, r#"{"type":"telegram_status","text":"Running tests"}"#));
        assert!(line.receive(gen, r#"{"type":"telegram_final","text":"Done"}"#));
        assert!(line.receive(gen, r#"{"type":"telegram_ack","id":"m1"}"#));
        assert!(line.receive(gen, r#"{"type":"telegram_notify","text":"Build finished"}"#));
        let id = "0123456789abcdef";
        let ask = |id: &str, options: Value| {
            json!({"type": "telegram_ask", "id": id, "text": "Run it?", "options": options})
                .to_string()
        };
        assert!(line.receive(gen, &ask(id, json!(["Allow", "Deny"]))));
        assert!(line.receive(
            gen,
            &json!({"type": "telegram_ask_done", "id": id, "text": "Allowed in Helm"}).to_string()
        ));
        // A bad id, no options, too many, an empty or a non-string option: dropped.
        let long = "a".repeat(33);
        for (id, options) in [
            ("0123456789ABCDEF", json!(["Allow"])),
            ("0123456789abcde", json!(["Allow"])),
            (long.as_str(), json!(["Allow"])),
            (id, json!([])),
            (id, json!(["1", "2", "3", "4", "5"])),
            (id, json!(["Allow", " "])),
            (id, json!(["Allow", 2])),
            (id, json!("Allow")),
        ] {
            assert!(
                !line.receive(gen, &ask(id, options.clone())),
                "{id} {options}"
            );
        }
        assert!(!line.receive(gen, "not json"));
        assert!(!line.receive(gen, r#"{"type":"call","id":"x"}"#));
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::TelegramStatus("Running tests".into())
        );
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::TelegramFinal("Done".into())
        );
        assert_eq!(up.try_recv().unwrap(), Upstream::TelegramAck("m1".into()));
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::TelegramNotify("Build finished".into())
        );
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::TelegramAsk {
                id: id.into(),
                text: "Run it?".into(),
                options: vec!["Allow".into(), "Deny".into()]
            }
        );
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::TelegramAskDone {
                id: id.into(),
                text: "Allowed in Helm".into()
            }
        );
        assert!(up.try_recv().is_err(), "nothing malformed went upstream");
    }

    #[tokio::test]
    async fn voice_frames_go_upstream_and_junk_is_dropped() {
        let (line, mut up) = Line::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let gen = line.attach(tx);
        let reply = |frame_type: &str, extra: Value| {
            let mut v = json!({
                "type": frame_type, "id": "reply-0001", "turn": "turn-0001",
                "mode": "conversation", "text": "Synthetic.", "attachments": 1
            });
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            v.to_string()
        };
        let attachment = json!({
            "type": "voice_attachment", "reply": "reply-0001", "n": 1, "kind": "link",
            "title": "The page", "url": "https://example.test/p"
        });
        assert!(line.receive(gen, r#"{"type":"voice_ack","id":"turn-0001"}"#));
        assert!(line.receive(
            gen,
            r#"{"type":"voice_status","turn":"turn-0001","text":"Thinking"}"#
        ));
        assert!(line.receive(gen, &attachment.to_string()));
        assert!(line.receive(gen, &reply("voice_final", json!({}))));
        assert!(line.receive(
            gen,
            &reply(
                "voice_notify",
                json!({"turn": null, "about": {"chat": "Build"}})
            )
        ));
        // A missing id, a bad mode, an answer with no turn, a link that is not https, an empty
        // text: dropped.
        assert!(!line.receive(gen, r#"{"type":"voice_ack"}"#));
        assert!(!line.receive(gen, r#"{"type":"voice_status","turn":"turn-0001"}"#));
        assert!(!line.receive(gen, &reply("voice_final", json!({"mode": "loud"}))));
        assert!(!line.receive(gen, &reply("voice_final", json!({"turn": null}))));
        assert!(!line.receive(gen, &reply("voice_final", json!({"text": " "}))));
        let insecure = attachment.to_string().replace("https://", "http://");
        assert!(!line.receive(gen, &insecure));
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::VoiceAck("turn-0001".into())
        );
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::VoiceStatus {
                turn: "turn-0001".into(),
                text: "Thinking".into()
            }
        );
        let Upstream::VoiceAttachment(a) = up.try_recv().unwrap() else {
            panic!("expected the attachment");
        };
        assert_eq!(
            (a.reply.as_str(), a.n, a.kind.as_str()),
            ("reply-0001", 1, "link")
        );
        let Upstream::VoiceFinal(f) = up.try_recv().unwrap() else {
            panic!("expected the answer");
        };
        assert_eq!((f.turn.as_deref(), f.attachments), (Some("turn-0001"), 1));
        let Upstream::VoiceNotify(n) = up.try_recv().unwrap() else {
            panic!("expected the news");
        };
        assert_eq!((n.turn, n.about.as_deref()), (None, Some("Build")));
        assert!(up.try_recv().is_err(), "nothing malformed went upstream");
        assert!(Upstream::VoiceAck(String::new()).is_voice());
        assert!(!Upstream::TelegramAck(String::new()).is_voice());
    }

    #[tokio::test]
    async fn household_frames_sharing_peer_questions_and_peer_calls() {
        let (line, mut up) = Line::in_namespace(5);
        let line = std::sync::Arc::new(line);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let gen = line.attach_as(tx, json!({ "person": "Sam" }));
        assert_eq!(gen >> 32, 5, "generations carry their client's namespace");
        let welcome = frames(&mut rx).remove(0);
        assert_eq!(
            (welcome["type"].as_str(), welcome["person"].as_str()),
            (Some("welcome"), Some("Sam"))
        );

        // Nothing is shared until Helm says so; a drop takes it back.
        assert_eq!(line.sharing(), Sharing::default());
        assert!(line.receive(gen, r#"{"type":"sharing","chats":true,"messages":true}"#));
        let shared = Sharing {
            chats: true,
            transcripts: false,
            messages: true,
        };
        assert_eq!(line.sharing(), shared);
        line.detach(gen);
        assert_eq!(line.sharing(), Sharing::default());

        // A household question goes upstream with its id as sent; malformed ones are dropped.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let gen = line.attach(tx);
        assert!(line.receive(
            gen,
            r#"{"type":"peer","id":7,"op":"people","args":{"personId":3}}"#
        ));
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::Peer {
                id: json!(7),
                op: "people".into(),
                args: json!({ "personId": 3 })
            }
        );
        assert!(!line.receive(gen, r#"{"type":"peer","id":"q"}"#));
        assert!(!line.receive(gen, r#"{"type":"peer","id":[1],"op":"people"}"#));
        assert!(up.try_recv().is_err());

        // A peer_call is answered by an ordinary result frame.
        let l2 = line.clone();
        let task = tokio::spawn(async move {
            l2.peer_call(
                "Bea",
                "list_chats",
                &json!({ "limit": 5 }),
                Duration::from_secs(5),
            )
            .await
        });
        let call = loop {
            if let Some(f) = frames(&mut rx)
                .into_iter()
                .find(|f| f["type"] == "peer_call")
            {
                break f;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(
            (
                call["person"].as_str(),
                call["op"].as_str(),
                call["args"].clone()
            ),
            (Some("Bea"), Some("list_chats"), json!({ "limit": 5 }))
        );
        let r =
            json!({ "type": "result", "id": call["id"], "ok": true, "result": { "chats": [] } });
        assert!(line.receive(gen, &r.to_string()));
        assert_eq!(task.await.unwrap(), CallOutcome::Ok(json!({ "chats": [] })));
    }

    #[tokio::test]
    async fn a_newer_line_replaces_the_old_one() {
        let (line, _up) = Line::new();
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let g1 = line.attach(tx1);
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        let g2 = line.attach(tx2);
        // The old socket closing later does not take the new line down.
        line.detach(g1);
        assert!(line.is_up());
        assert!(line.send(&json!({"type": "telegram_stop"})));
        let f = frames(&mut rx2);
        assert_eq!(f.last().unwrap()["type"], "telegram_stop");
        line.detach(g2);
        assert!(!line.is_up());
    }

    #[tokio::test]
    async fn device_and_knowledge_frames_go_upstream_and_a_device_call_waits_for_its_answer() {
        let (line, mut up) = Line::new();
        let line = std::sync::Arc::new(line);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let gen = line.attach(tx);
        let _ = frames(&mut rx);
        let changes = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen = changes.clone();
        line.on_change(move |now_up| seen.lock().unwrap().push(now_up));

        assert!(line.receive(
            gen,
            r#"{"type":"device","uid":"u1","name":"Desk","run":true}"#
        ));
        assert!(line.receive(gen, r#"{"type":"dev","id":"c1","to":"u2","op":"hello"}"#));
        assert!(line.receive(
            gen,
            r#"{"type":"dev_push","to":"u2","sub":"s","seq":3,"event":{"k":1}}"#
        ));
        assert!(line.receive(gen, r#"{"type":"kn_pull","id":1,"since":-5,"limit":999}"#));
        assert!(line.receive(
            gen,
            r#"{"type":"kn_put","id":"p","layer":"everyone","path":"rules/a.md","baseSeq":2,"deleted":true}"#
        ));
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::Device {
                uid: "u1".into(),
                name: "Desk".into(),
                run: true
            }
        );
        let Upstream::Dev {
            id,
            to,
            op,
            args,
            size,
        } = up.try_recv().unwrap()
        else {
            panic!("expected the call");
        };
        assert_eq!(
            (id, to.as_str(), op.as_str(), args, size > 0),
            (json!("c1"), "u2", "hello", json!({}), true)
        );
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::DevPush {
                to: "u2".into(),
                sub: json!("s"),
                seq: json!(3),
                event: json!({ "k": 1 })
            }
        );
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::KnPull {
                id: json!(1),
                since: 0,
                limit: 200
            }
        );
        assert_eq!(
            up.try_recv().unwrap(),
            Upstream::KnPut {
                id: json!("p"),
                layer: "everyone".into(),
                path: "rules/a.md".into(),
                base_seq: 2,
                sha256: String::new(),
                deleted: true,
                body: None
            }
        );
        // A bad id, a missing field and an oversized push are dropped.
        let big = "x".repeat(MAX_DEV_FRAME);
        for bad in [
            r#"{"type":"device","uid":"u1"}"#.to_owned(),
            r#"{"type":"dev","id":[1],"to":"u2","op":"hello"}"#.to_owned(),
            r#"{"type":"dev","id":"c","op":"hello"}"#.to_owned(),
            r#"{"type":"dev_push","to":"u2","sub":"s","seq":1}"#.to_owned(),
            format!(r#"{{"type":"dev_push","to":"u2","sub":"s","seq":1,"event":"{big}"}}"#),
            r#"{"type":"kn_pull"}"#.to_owned(),
            r#"{"type":"kn_put","id":"p","layer":"everyone"}"#.to_owned(),
            r#"{"type":"dev_result","id":"nobody-asked","ok":true}"#.to_owned(),
        ] {
            assert!(!line.receive(gen, &bad), "{}", &bad[..bad.len().min(60)]);
        }
        assert!(up.try_recv().is_err(), "nothing malformed went upstream");

        // A device call goes out as a dev_call and its dev_result comes back whole, an answer
        // that is too big as too_large, and silence as unknown.
        let ask = |wait: u64| {
            let line = line.clone();
            tokio::spawn(async move {
                let from = json!({ "uid": "u1" });
                line.dev_call(
                    &from,
                    "hello",
                    &json!({ "n": 1 }),
                    Duration::from_millis(wait),
                )
                .await
            })
        };
        let mut answer = |result: Value| {
            let mut rx_frames = frames(&mut rx);
            let call = rx_frames.pop().filter(|f| f["type"] == "dev_call");
            call.map(|c| {
                assert_eq!(
                    (c["op"].as_str(), c["from"]["uid"].as_str()),
                    (Some("hello"), Some("u1"))
                );
                json!({ "type": "dev_result", "id": c["id"], "ok": true, "result": result })
            })
        };
        let task = ask(5000);
        let reply = loop {
            tokio::task::yield_now().await;
            if let Some(r) = answer(json!({ "device": "Desk" })) {
                break r;
            }
        };
        assert!(line.receive(gen, &reply.to_string()));
        let CallOutcome::Ok(frame) = task.await.unwrap() else {
            panic!("expected the answer");
        };
        assert_eq!(
            (frame["ok"].as_bool(), frame["result"].clone()),
            (Some(true), json!({ "device": "Desk" }))
        );

        let task = ask(5000);
        let reply = loop {
            tokio::task::yield_now().await;
            if let Some(r) = answer(json!(big)) {
                break r;
            }
        };
        assert!(line.receive(gen, &reply.to_string()));
        assert_eq!(
            task.await.unwrap(),
            CallOutcome::Err {
                text: TOO_LARGE.into(),
                changed: true
            }
        );

        assert!(matches!(ask(20).await.unwrap(), CallOutcome::Unknown(_)));

        // The change hook hears of a drop, and of a new connection.
        line.detach(gen);
        let (tx, _rx) = mpsc::unbounded_channel();
        line.attach(tx);
        assert_eq!(*changes.lock().unwrap(), [false, true]);
    }
}
