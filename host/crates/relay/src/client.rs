//! The line itself: one outbound WebSocket from the PC to the hub, kept open.
//!
//! It tries the LAN address first and the fallback second, reconnecting with a backoff (1 s
//! doubling to 60 s). The token goes in the `Authorization` header of the upgrade request only.
//! Frames are the hub's line protocol (`hub/src/relay.rs`).
//!
//! - `call`: the gate ([`crate::gate::admit`]) first; a refusal is answered at once as "nothing
//!   changed" and the tool never runs. An admitted call runs through the [`Executor`]. A call with
//!   `"scope": "remote"` came from the hub's Remote page and goes through
//!   [`crate::gate::admit_remote`], which also lets the page's own two tools in.
//! - `telegram`: handed to the [`TelegramSink`]; acknowledged only once the sink took it, so a
//!   message is never lost to a drop between the hub and the chat.
//! - `telegram_stop`: the sink stops the running turn.
//! - `peer_call`: another person's Helm, through the hub, asks this one for its shared chat list,
//!   a chat's messages or to deliver a message. [`crate::household::admit_peer`] checks the
//!   sharing switches and the arguments first; the answer is a `result` frame like a call's.
//! - `peer_result`: the hub's answer to a `peer` request this Helm sent ([`LineHandle::peer`]).
//! - `voice`: one turn from the phone's voice screen, handed to the [`VoiceSink`] and acknowledged
//!   (`voice_ack`) only once the sink took it, like Telegram's. `voice_stop`, `voice_heard`,
//!   `voice_presence` and `voice_fresh` reach the same sink; an old Helm never sees them and the
//!   hub never needs them answered.
//! - `devices`, `dev_call`, `dev_result` and `dev_push`: the household's devices (`crate::devlink`).
//!   They are handed to the [`DeviceSink`] as they came, and ignored until one is attached.
//! - The first frame up on every line is `sharing` (the household switches), and another goes
//!   whenever the user changes one. The device announces itself right after it.
//! - Upward, [`LineHandle`] sends the Telegram status and final answer, and alerts Helm starts
//!   ([`crate::notify`]), and for the voice chat its working line and answers.

use crate::config::RelayConfig;
use crate::gate;
use crate::household::{self, Sharing};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A tool failure: the text, and whether anything may have changed.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolFailure {
    pub text: String,
    pub changed: bool,
}

/// Runs an admitted call on the PC. `caller` is the hub's client name for the token.
pub trait Executor: Send + Sync {
    fn run<'a>(
        &'a self,
        caller: &'a str,
        call: gate::Admitted,
    ) -> BoxFut<'a, Result<Value, ToolFailure>>;

    /// Runs an admitted household call on the PC. `person` is the asking person's name, from the
    /// hub. An executor that serves no household refuses.
    fn run_peer<'a>(
        &'a self,
        person: &'a str,
        call: gate::Admitted,
    ) -> BoxFut<'a, Result<Value, ToolFailure>> {
        let _ = (person, call);
        Box::pin(async {
            Err(ToolFailure {
                text: "Household calls are not available here.".into(),
                changed: false,
            })
        })
    }
}

/// A picture or file that came with a Telegram message: base64, as the hub sent it. It prints as
/// its name and size only, so no log line can carry its bytes.
#[derive(Clone, PartialEq)]
pub struct TelegramFile {
    pub name: String,
    pub media_type: String,
    pub data: String,
}

impl std::fmt::Debug for TelegramFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TelegramFile({}, {}, {} chars)",
            self.name,
            self.media_type,
            self.data.len()
        )
    }
}

/// One Telegram message as the hub forwarded it.
#[derive(Clone, Debug, PartialEq)]
pub struct TelegramMessage {
    pub id: String,
    pub text: String,
    /// Pictures sent with it that the chat shows (a photo, or an image sent as a file).
    pub images: Vec<TelegramFile>,
    /// Any other files sent with it (a document, a video, a recording, a picture the chat cannot
    /// show).
    pub files: Vec<TelegramFile>,
    pub sent_at: i64,
    pub age_minutes: Option<i64>,
    pub bot: Option<String>,
}

/// Where Telegram messages go on the PC.
pub trait TelegramSink: Send + Sync {
    /// `Ok` once the message is safely in Helm (it is then acknowledged to the hub).
    fn deliver<'a>(&'a self, msg: TelegramMessage) -> BoxFut<'a, Result<(), String>>;
    fn stop<'a>(&'a self) -> BoxFut<'a, ()>;
    /// The user tapped button `index` of the approval ask `id` ([`LineHandle::telegram_ask`]).
    fn answer<'a>(&'a self, id: String, index: usize) -> BoxFut<'a, ()>;
}

/// One turn for the voice chat as the hub forwarded it.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceTurn {
    /// The phone's id for the turn.
    pub id: String,
    /// `conversation` (heard) or `chat` (read).
    pub mode: String,
    pub text: String,
    /// `speech` or `typed`.
    pub source: String,
    pub sent_at: i64,
    pub age_minutes: Option<i64>,
    /// The turn this one carries on, when it was spoken as a continuation.
    pub continues: Option<String>,
}

/// Where voice turns go on the PC.
pub trait VoiceSink: Send + Sync {
    /// `Ok` once the turn is safely in Helm (it is then acknowledged to the hub).
    fn deliver<'a>(&'a self, turn: VoiceTurn) -> BoxFut<'a, Result<(), String>>;
    /// The phone cancelled this turn after Helm took it.
    fn stop<'a>(&'a self, turn: String) -> BoxFut<'a, ()>;
    /// What the phone did and did not play: the hub's reports, as sent.
    fn heard<'a>(&'a self, _reports: Vec<Value>) -> BoxFut<'a, ()> {
        Box::pin(async {})
    }
    /// The phone's screen is in front (`live`) or not, in this mode.
    fn presence(&self, _live: bool, _mode: &str) {}
    /// Start the next voice message in a new chat.
    fn fresh<'a>(&'a self) -> BoxFut<'a, ()> {
        Box::pin(async {})
    }
}

/// Where the household's device frames go on the PC (`crate::devlink`).
pub trait DeviceSink: Send + Sync {
    /// A `devices`, `dev_call`, `dev_result` or `dev_push` frame, as the hub sent it.
    fn frame(&self, frame: Value);
    /// A new line is up and the household switches have been sent: frames may go up now.
    fn line_up(&self) {}
    /// The line is gone.
    fn line_down(&self) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineState {
    Off = 0,
    Connecting = 1,
    Online = 2,
    Offline = 3,
}

impl LineState {
    pub fn as_str(self) -> &'static str {
        match self {
            LineState::Off => "off",
            LineState::Connecting => "connecting",
            LineState::Online => "online",
            LineState::Offline => "offline",
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => LineState::Connecting,
            2 => LineState::Online,
            3 => LineState::Offline,
            _ => LineState::Off,
        }
    }
}

/// Shared with the rest of the app: the line's state, and the way up to the hub.
pub struct LineHandle {
    state: AtomicU8,
    up: mpsc::UnboundedSender<Value>,
    /// Requests to other people's Helms waiting for the hub's answer, by request id.
    peers: std::sync::Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
}

/// How long a household request may wait for the hub's answer.
const PEER_TIMEOUT: Duration = Duration::from_secs(30);

impl LineHandle {
    pub fn state(&self) -> LineState {
        LineState::from_u8(self.state.load(Ordering::SeqCst))
    }
    fn set(&self, s: LineState) {
        self.state.store(s as u8, Ordering::SeqCst);
    }

    /// Ask the hub for something about the household (`op`: `people`, `list_chats`, `read_chat`
    /// or `send_message`) and wait for its answer. Fails at once while the line is down, and when
    /// the line drops or the hub is silent for 30 seconds before it answers.
    pub async fn peer(&self, op: &str, args: Value) -> Result<Value, String> {
        if self.state() != LineState::Online {
            return Err("Helm is not connected to the hub.".into());
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (tx, rx) = oneshot::channel();
        self.peers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.clone(), tx);
        if self
            .up
            .send(json!({ "type": "peer", "id": id, "op": op, "args": args }))
            .is_err()
        {
            self.peers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            return Err("Helm is not connected to the hub.".into());
        }
        match tokio::time::timeout(PEER_TIMEOUT, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err("The hub line dropped before the answer came.".into()),
            Err(_) => {
                self.peers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
                Err("The hub did not answer in time.".into())
            }
        }
    }

    fn resolve_peer(&self, id: &str, answer: Result<Value, String>) {
        let tx = self
            .peers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
        if let Some(tx) = tx {
            let _ = tx.send(answer);
        }
    }

    /// The line is gone: every request still waiting fails now.
    fn fail_peers(&self) {
        let waiting: Vec<_> = self
            .peers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .collect();
        for (_, tx) in waiting {
            let _ = tx.send(Err("The hub line dropped before the answer came.".into()));
        }
    }
    /// A device frame for the hub. `false` when the line is down and it was not sent: device
    /// requests are never queued for a later line.
    pub fn device_frame(&self, frame: Value) -> bool {
        self.state() == LineState::Online && self.up.send(frame).is_ok()
    }
    /// The one live status message on Telegram, edited in place by the hub.
    pub fn telegram_status(&self, text: &str) {
        let _ = self
            .up
            .send(json!({ "type": "telegram_status", "text": text }));
    }
    /// A message Helm starts (an alert). Sent only while the line is up, so it never waits in
    /// the queue for a later line; `false` when it was not sent.
    pub fn telegram_notify(&self, text: &str) -> bool {
        self.state() == LineState::Online
            && self
                .up
                .send(json!({ "type": "telegram_notify", "text": text }))
                .is_ok()
    }
    /// An approval ask: a new message with one button per option. Only a tap on one of them
    /// comes back (`telegram_answer`); `false` when it was not sent.
    pub fn telegram_ask(&self, id: &str, text: &str, options: &[String]) -> bool {
        self.state() == LineState::Online
            && self
                .up
                .send(json!({ "type": "telegram_ask", "id": id, "text": text, "options": options }))
                .is_ok()
    }
    /// The ask was settled in Helm or went away: the hub takes its buttons down.
    pub fn telegram_ask_done(&self, id: &str, text: &str) {
        let _ = self
            .up
            .send(json!({ "type": "telegram_ask_done", "id": id, "text": text }));
    }
    /// The turn's answer, sent by the hub as a new message; the status message goes.
    /// `false` when the line is down and it was not sent, so the mirror can try again.
    pub fn telegram_final(&self, text: &str) -> bool {
        self.state() == LineState::Online
            && self
                .up
                .send(json!({ "type": "telegram_final", "text": text }))
                .is_ok()
    }

    /// The voice chat's working line for a turn. The hub passes it to the phone if it is
    /// listening and drops it otherwise, so it is sent only while the line is up.
    pub fn voice_status(&self, turn: &str, text: &str) {
        if self.state() == LineState::Online {
            let _ = self
                .up
                .send(json!({ "type": "voice_status", "turn": turn, "text": text }));
        }
    }
    /// An answer to a turn (`voice_final`), or news (`voice_notify`). `false` when the line is
    /// down and it was not sent, so the mirror can try again.
    pub fn voice_reply(&self, reply: &crate::voice::Reply) -> bool {
        self.state() == LineState::Online && self.up.send(reply.frame()).is_ok()
    }
    /// One attachment, sent just before the reply it belongs to. `false` when it was not sent.
    pub fn voice_attachment(&self, frame: Value) -> bool {
        self.state() == LineState::Online && self.up.send(frame).is_ok()
    }
}

pub struct Client {
    config: Arc<dyn Fn() -> RelayConfig + Send + Sync>,
    executor: Arc<dyn Executor>,
    sink: Arc<dyn TelegramSink>,
    /// Where voice turns go; frames for it are ignored until it is attached.
    voice: std::sync::OnceLock<Arc<dyn VoiceSink>>,
    /// Where the household's device frames go; they are ignored until it is attached.
    devices: std::sync::OnceLock<Arc<dyn DeviceSink>>,
    handle: Arc<LineHandle>,
    up_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>,
    /// Hub ids of Telegram messages being delivered now: the hub's resend of one of them is
    /// dropped, since the delivery under way acknowledges it.
    delivering: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// What the household may see of this install; told to the hub on every new line.
    sharing: std::sync::Mutex<Sharing>,
    /// Woken when the saved connection changes, so a new address or token applies at once.
    pub reconfigured: tokio::sync::Notify,
}

/// The most pictures, and the most other files, taken from one message.
const MAX_FILES: usize = 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// The hub pings every 30 s; a line silent for 100 s is treated as gone.
const SILENCE: Duration = Duration::from_secs(100);
/// The most one frame from the hub may hold: a Telegram message can carry up to 50 MB of files,
/// which is a third larger as base64, plus its text.
const MAX_FRAME: usize = 80 * 1024 * 1024;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The `result` frame for a call the gate refused: nothing ran, so nothing changed.
fn refusal(id: &str, text: String) -> Value {
    json!({ "type": "result", "id": id, "ok": false, "error": text, "changed": "no" })
}

/// The `result` frame for a call that ran.
fn answer(id: &str, r: Result<Value, ToolFailure>) -> Value {
    match r {
        Ok(r) => json!({ "type": "result", "id": id, "ok": true, "result": r }),
        Err(f) => json!({ "type": "result", "id": id, "ok": false, "error": f.text,
                          "changed": if f.changed { "maybe" } else { "no" } }),
    }
}

impl Client {
    pub fn new(
        config: Arc<dyn Fn() -> RelayConfig + Send + Sync>,
        executor: Arc<dyn Executor>,
        sink: Arc<dyn TelegramSink>,
    ) -> Arc<Self> {
        let (up, up_rx) = mpsc::unbounded_channel();
        Arc::new(Client {
            config,
            executor,
            sink,
            voice: std::sync::OnceLock::new(),
            devices: std::sync::OnceLock::new(),
            handle: Arc::new(LineHandle {
                state: AtomicU8::new(LineState::Off as u8),
                up,
                peers: Default::default(),
            }),
            up_rx: tokio::sync::Mutex::new(up_rx),
            delivering: Arc::default(),
            sharing: Default::default(),
            reconfigured: tokio::sync::Notify::new(),
        })
    }

    pub fn handle(&self) -> Arc<LineHandle> {
        self.handle.clone()
    }

    /// Give the line the sink for voice turns.
    pub fn attach_voice(&self, sink: Arc<dyn VoiceSink>) {
        let _ = self.voice.set(sink);
    }

    /// Give the line the sink for the household's device frames.
    pub fn attach_devices(&self, sink: Arc<dyn DeviceSink>) {
        let _ = self.devices.set(sink);
    }

    /// What the household may see now.
    pub fn sharing(&self) -> Sharing {
        *self.sharing.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The user changed the household switches: keep them, and tell the hub on the live line (a
    /// line that comes up later is told as it starts).
    pub fn set_sharing(&self, s: Sharing) {
        let s = s.normalised();
        *self.sharing.lock().unwrap_or_else(|p| p.into_inner()) = s;
        let _ = self.handle.up.send(s.frame());
    }

    /// Set the switches at start-up, before any line: nothing to send yet.
    pub fn restore_sharing(&self, s: Sharing) {
        *self.sharing.lock().unwrap_or_else(|p| p.into_inner()) = s.normalised();
    }

    async fn connect(url: &str, token: &str) -> Result<Ws, String> {
        let mut req = url
            .into_client_request()
            .map_err(|_| "The hub address is not valid.".to_owned())?;
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| "The relay token is not valid.".to_owned())?,
        );
        let mut cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
        cfg.max_message_size = Some(MAX_FRAME);
        cfg.max_frame_size = Some(MAX_FRAME);
        match tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio_tungstenite::connect_async_with_config(req, Some(cfg), false),
        )
        .await
        {
            Ok(Ok((ws, _))) => Ok(ws),
            // Never the error's text: it can carry the request.
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(r))) => Err(format!(
                "the hub refused the line (HTTP {})",
                r.status().as_u16()
            )),
            Ok(Err(_)) => Err("the hub could not be reached".into()),
            Err(_) => Err("the hub did not answer in time".into()),
        }
    }

    /// Keep the line up until the task is dropped.
    pub async fn run(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let cfg = (self.config)();
            if !cfg.usable() {
                self.handle.set(LineState::Off);
                tokio::select! {
                    _ = self.reconfigured.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                }
                continue;
            }
            self.handle.set(LineState::Connecting);
            let mut connected = None;
            for url in cfg.urls() {
                match Self::connect(&url, &cfg.token).await {
                    Ok(ws) => {
                        connected = Some(ws);
                        break;
                    }
                    Err(e) => log::info!("relay: {e}"),
                }
            }
            match connected {
                Some(ws) => {
                    backoff = Duration::from_secs(1);
                    self.handle.set(LineState::Online);
                    log::info!("relay: line up");
                    self.serve(ws).await;
                    self.handle.fail_peers();
                    log::info!("relay: line down");
                    self.handle.set(LineState::Offline);
                }
                None => self.handle.set(LineState::Offline),
            }
            tokio::select! {
                _ = self.reconfigured.notified() => backoff = Duration::from_secs(1),
                _ = tokio::time::sleep(backoff) => backoff = (backoff * 2).min(MAX_BACKOFF),
            }
        }
    }

    /// Serve one connection until it ends (or the configuration changes).
    pub async fn serve(&self, ws: Ws) {
        let (mut sink_ws, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
        let mut up_rx = self.up_rx.lock().await;
        // The hub enforces the household switches too, so it hears them before anything else.
        if sink_ws
            .send(Message::Text(self.sharing().frame().to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        // The device announces itself after the switches, so the hub knows what is shared first.
        if let Some(d) = self.devices.get() {
            d.line_up();
        }
        let silence = tokio::time::sleep(SILENCE);
        tokio::pin!(silence);
        loop {
            tokio::select! {
                frame = stream.next() => {
                    silence.as_mut().reset(tokio::time::Instant::now() + SILENCE);
                    match frame {
                        Some(Ok(Message::Text(t))) => self.on_frame(t.as_str(), &out_tx),
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => {} // pings are answered by the library
                    }
                }
                Some(v) = out_rx.recv() => {
                    if sink_ws.send(Message::Text(v.to_string().into())).await.is_err() { break; }
                }
                Some(v) = up_rx.recv() => {
                    if sink_ws.send(Message::Text(v.to_string().into())).await.is_err() { break; }
                }
                _ = &mut silence => break,
                _ = self.reconfigured.notified() => break,
            }
        }
        let _ = sink_ws.close().await;
        if let Some(d) = self.devices.get() {
            d.line_down();
        }
    }

    fn on_frame(&self, text: &str, out: &mpsc::UnboundedSender<Value>) {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        match v.get("type").and_then(Value::as_str) {
            Some("call") => {
                let (Some(id), Some(tool)) = (s("id"), s("tool")) else {
                    return;
                };
                let caller = s("client").unwrap_or_default();
                let args = v.get("args").cloned().unwrap_or(Value::Null);
                // The hub stamps a call that came from its Remote page; only then do the page's
                // own tools get through the gate.
                let remote = s("scope").as_deref() == Some("remote");
                let executor = self.executor.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let admitted = if remote {
                        gate::admit_remote(&tool, &args)
                    } else {
                        gate::admit(&tool, &args)
                    };
                    let reply = match admitted {
                        Err(text) => refusal(&id, text),
                        Ok(admitted) => answer(&id, executor.run(&caller, admitted).await),
                    };
                    let _ = out.send(reply);
                });
            }
            Some("peer_call") => {
                let (Some(id), Some(op)) = (s("id"), s("op")) else {
                    return;
                };
                let person = s("person").unwrap_or_default();
                let args = v.get("args").cloned().unwrap_or(Value::Null);
                // Checked against the switches as they are now, not as the hub last heard them.
                let admitted = household::admit_peer(&op, &args, self.sharing());
                let executor = self.executor.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let reply = match admitted {
                        Err(text) => refusal(&id, text),
                        Ok(admitted) => answer(&id, executor.run_peer(&person, admitted).await),
                    };
                    let _ = out.send(reply);
                });
            }
            Some("peer_result") => {
                let Some(id) = s("id") else {
                    return;
                };
                let answer = if v.get("ok").and_then(Value::as_bool) == Some(true) {
                    Ok(v.get("result").cloned().unwrap_or(Value::Null))
                } else {
                    Err(s("error").unwrap_or_else(|| "The hub could not answer.".into()))
                };
                self.handle.resolve_peer(&id, answer);
            }
            Some("telegram") => {
                let (Some(id), Some(text)) = (s("id"), s("text")) else {
                    return;
                };
                let delivering = self.delivering.clone();
                if !delivering
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone())
                {
                    return;
                }
                let list = |key: &str| -> Vec<TelegramFile> {
                    v.get(key)
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|i| {
                                    let f = |k: &str| i.get(k).and_then(Value::as_str);
                                    Some(TelegramFile {
                                        name: f("name")?.to_owned(),
                                        media_type: f("mediaType")?.to_owned(),
                                        data: f("data")?.to_owned(),
                                    })
                                })
                                .take(MAX_FILES)
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let msg = TelegramMessage {
                    id: id.clone(),
                    text,
                    images: list("images"),
                    files: list("files"),
                    sent_at: v.get("sentAt").and_then(Value::as_i64).unwrap_or(0),
                    age_minutes: v.get("ageMinutes").and_then(Value::as_i64),
                    bot: s("bot"),
                };
                let sink = self.sink.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let r = sink.deliver(msg).await;
                    delivering
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&id);
                    match r {
                        Ok(()) => {
                            let _ = out.send(json!({ "type": "telegram_ack", "id": id }));
                        }
                        // Not acknowledged: the hub keeps it and sends it again on the next line.
                        Err(e) => log::warn!("relay: a Telegram message was not delivered: {e}"),
                    }
                });
            }
            Some("voice") => {
                let (Some(id), Some(text)) = (s("id"), s("text")) else {
                    return;
                };
                let Some(voice) = self.voice.get().cloned() else {
                    return;
                };
                // The same dedupe as Telegram's, kept apart from it by a prefix.
                let key = format!("voice/{id}");
                let delivering = self.delivering.clone();
                if !delivering
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(key.clone())
                {
                    return;
                }
                let turn = VoiceTurn {
                    id: id.clone(),
                    mode: s("mode").unwrap_or_default(),
                    text,
                    source: s("source").unwrap_or_default(),
                    sent_at: v.get("sentAt").and_then(Value::as_i64).unwrap_or(0),
                    age_minutes: v.get("ageMinutes").and_then(Value::as_i64),
                    continues: s("continues"),
                };
                let out = out.clone();
                tokio::spawn(async move {
                    let r = voice.deliver(turn).await;
                    delivering
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&key);
                    match r {
                        Ok(()) => {
                            let _ = out.send(json!({ "type": "voice_ack", "id": id }));
                        }
                        // Not acknowledged: the hub keeps it and sends it again.
                        Err(e) => log::warn!("relay: a voice turn was not delivered: {e}"),
                    }
                });
            }
            Some("voice_stop") => {
                let (Some(turn), Some(voice)) = (s("turn"), self.voice.get().cloned()) else {
                    return;
                };
                tokio::spawn(async move { voice.stop(turn).await });
            }
            Some("voice_heard") => {
                let (Some(reports), Some(voice)) = (
                    v.get("reports").and_then(Value::as_array).cloned(),
                    self.voice.get().cloned(),
                ) else {
                    return;
                };
                tokio::spawn(async move { voice.heard(reports).await });
            }
            Some("voice_presence") => {
                if let Some(voice) = self.voice.get() {
                    voice.presence(
                        v.get("live").and_then(Value::as_bool).unwrap_or(false),
                        s("mode").as_deref().unwrap_or("conversation"),
                    );
                }
            }
            Some("voice_fresh") => {
                let Some(voice) = self.voice.get().cloned() else {
                    return;
                };
                tokio::spawn(async move { voice.fresh().await });
            }
            Some("devices" | "dev_call" | "dev_result" | "dev_push") => {
                if let Some(d) = self.devices.get() {
                    d.frame(v.clone());
                }
            }
            Some("telegram_stop") => {
                let sink = self.sink.clone();
                tokio::spawn(async move { sink.stop().await });
            }
            Some("telegram_answer") => {
                let (Some(id), Some(index)) = (s("id"), v.get("index").and_then(Value::as_u64))
                else {
                    return;
                };
                let sink = self.sink.clone();
                tokio::spawn(async move { sink.answer(id, index as usize).await });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    const TOKEN: &str = "relay-test-token-0123456789abcdef";

    #[derive(Default)]
    struct Exec {
        ran: Mutex<Vec<(String, gate::Admitted)>>,
        peer: Mutex<Vec<(String, gate::Admitted)>>,
    }
    impl Executor for Exec {
        fn run_peer<'a>(
            &'a self,
            person: &'a str,
            call: gate::Admitted,
        ) -> BoxFut<'a, Result<Value, ToolFailure>> {
            self.peer.lock().unwrap().push((person.to_owned(), call));
            Box::pin(async { Ok(json!({ "chats": [] })) })
        }

        fn run<'a>(
            &'a self,
            caller: &'a str,
            call: gate::Admitted,
        ) -> BoxFut<'a, Result<Value, ToolFailure>> {
            self.ran.lock().unwrap().push((caller.to_owned(), call));
            Box::pin(async { Ok(json!({ "chats": [] })) })
        }
    }

    #[derive(Default)]
    struct Sink {
        got: Mutex<Vec<TelegramMessage>>,
        stops: Mutex<usize>,
        refuse: bool,
    }
    impl TelegramSink for Sink {
        fn deliver<'a>(&'a self, msg: TelegramMessage) -> BoxFut<'a, Result<(), String>> {
            self.got.lock().unwrap().push(msg);
            let r = if self.refuse {
                Err("no".to_owned())
            } else {
                Ok(())
            };
            Box::pin(async move { r })
        }
        fn stop<'a>(&'a self) -> BoxFut<'a, ()> {
            *self.stops.lock().unwrap() += 1;
            Box::pin(async {})
        }
        fn answer<'a>(&'a self, _id: String, _index: usize) -> BoxFut<'a, ()> {
            Box::pin(async {})
        }
    }

    #[derive(Default)]
    struct Voice {
        got: Mutex<Vec<VoiceTurn>>,
        stops: Mutex<Vec<String>>,
        heard: Mutex<Vec<Value>>,
        presence: Mutex<Vec<(bool, String)>>,
        fresh: Mutex<usize>,
        refuse: std::sync::atomic::AtomicBool,
        hold: std::sync::atomic::AtomicBool,
        release: tokio::sync::Notify,
    }
    impl VoiceSink for Voice {
        fn deliver<'a>(&'a self, turn: VoiceTurn) -> BoxFut<'a, Result<(), String>> {
            self.got.lock().unwrap().push(turn);
            let hold = self.hold.load(Ordering::SeqCst);
            let refuse = self.refuse.load(Ordering::SeqCst);
            Box::pin(async move {
                if hold {
                    self.release.notified().await;
                }
                if refuse {
                    Err("no".to_owned())
                } else {
                    Ok(())
                }
            })
        }
        fn stop<'a>(&'a self, turn: String) -> BoxFut<'a, ()> {
            self.stops.lock().unwrap().push(turn);
            Box::pin(async {})
        }
        fn heard<'a>(&'a self, reports: Vec<Value>) -> BoxFut<'a, ()> {
            self.heard.lock().unwrap().extend(reports);
            Box::pin(async {})
        }
        fn presence(&self, live: bool, mode: &str) {
            self.presence.lock().unwrap().push((live, mode.to_owned()));
        }
        fn fresh<'a>(&'a self) -> BoxFut<'a, ()> {
            *self.fresh.lock().unwrap() += 1;
            Box::pin(async {})
        }
    }

    #[allow(clippy::result_large_err)] // the handshake callback's error type is the library's
    /// A one-connection fake hub. Returns its URL, the Authorization header it saw, and the
    /// accepted socket.
    async fn fake_hub() -> (
        String,
        tokio::task::JoinHandle<(
            Option<String>,
            tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        )>,
    ) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/relay", l.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let seen = Arc::new(Mutex::new(None));
            let seen2 = seen.clone();
            let ws =
                tokio_tungstenite::accept_hdr_async(s, move |req: &Request, resp: Response| {
                    *seen2.lock().unwrap() = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    Ok(resp)
                })
                .await
                .unwrap();
            let auth = seen.lock().unwrap().clone();
            (auth, ws)
        });
        (url, task)
    }

    async fn next(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Value {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Message::Text(t) = m {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }

    fn client(urls: (&str, &str), exec: Arc<Exec>, sink: Arc<Sink>) -> Arc<Client> {
        let cfg = RelayConfig {
            lan_url: urls.0.to_owned(),
            fallback_url: urls.1.to_owned(),
            token: TOKEN.to_owned(),
            ..Default::default()
        };
        Client::new(Arc::new(move || cfg.clone()), exec, sink)
    }

    #[tokio::test]
    async fn the_line_authenticates_gates_calls_and_acknowledges_telegram() {
        let (url, hub) = fake_hub().await;
        // The LAN address is dead: the fallback is used.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let a = l.local_addr().unwrap();
            drop(l);
            format!("ws://{a}/relay")
        };
        let exec = Arc::new(Exec::default());
        let sink = Arc::new(Sink::default());
        let c = client((&dead, &url), exec.clone(), sink.clone());
        let task = tokio::spawn(c.clone().run());
        let (auth, mut ws) = hub.await.unwrap();
        assert_eq!(auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
        // The household switches come first, all off by default.
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "sharing", "chats": false, "transcripts": false, "messages": false})
        );

        // A denied tool is refused by the PC's gate and never runs.
        ws.send(Message::Text(
            json!({"type": "call", "id": "1", "client": "remote",
            "tool": "app_update_settings", "args": {}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let r = next(&mut ws).await;
        assert_eq!(
            (r["id"].as_str(), r["ok"].as_bool(), r["changed"].as_str()),
            (Some("1"), Some(false), Some("no"))
        );
        assert!(r["error"]
            .as_str()
            .unwrap()
            .contains("not available to callers outside Helm"));
        // A refused argument too.
        ws.send(Message::Text(
            json!({"type": "call", "id": "2", "client": "remote",
            "tool": "app_read_chat", "args": {"chatId": "c", "includeTools": true}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        assert_eq!(next(&mut ws).await["changed"], "no");
        assert!(exec.ran.lock().unwrap().is_empty(), "nothing ran");
        // An allowed one runs, attributed to the hub's client name.
        ws.send(Message::Text(
            json!({"type": "call", "id": "3", "client": "remote",
            "tool": "app_list_chats", "args": {"query": "x"}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let r = next(&mut ws).await;
        assert_eq!(r["ok"], true);
        assert_eq!(exec.ran.lock().unwrap()[0].0, "remote");

        // The Remote page's own tools: refused without the hub's scope stamp (or with another),
        // let through with it. (id, scope stamp, the call runs)
        for (id, scope, runs) in [
            ("4", None, false),
            ("5", Some("telegram"), false),
            ("6", Some("remote"), true),
        ] {
            let mut call = json!({"type": "call", "id": id, "client": "phone",
                "tool": "app_list_waiting", "args": {}});
            if let Some(scope) = scope {
                call["scope"] = json!(scope);
            }
            ws.send(Message::Text(call.to_string().into()))
                .await
                .unwrap();
            let r = next(&mut ws).await;
            assert_eq!(r["ok"].as_bool(), Some(runs), "{id}: {r}");
            if !runs {
                assert!(r["error"]
                    .as_str()
                    .unwrap()
                    .contains("only available from the hub's Remote page"));
            }
        }
        let ran = exec.ran.lock().unwrap();
        assert_eq!(ran.len(), 2, "only the stamped call ran");
        assert!(ran[1].1.remote && ran[1].1.tool == "app_list_waiting");
        drop(ran);

        // Telegram: delivered, then acknowledged.
        ws.send(Message::Text(
            json!({"type": "telegram", "id": "m1", "text": "hi",
            "sentAt": 5, "ageMinutes": 3, "bot": "b"})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let ack = next(&mut ws).await;
        assert_eq!(ack, json!({"type": "telegram_ack", "id": "m1"}));
        assert_eq!(sink.got.lock().unwrap()[0].age_minutes, Some(3));
        // A message with files far larger than any other frame still arrives, files and all (50 MB
        // of them as base64, the most the hub sends).
        let big = "A".repeat(1_000_000);
        let pdf = "B".repeat(50 * 1024 * 1024 / 3 * 4);
        ws.send(Message::Text(
            json!({"type": "telegram", "id": "m2", "text": "look",
            "images": [{"name": "photo.jpg", "mediaType": "image/jpeg", "data": big}],
            "files": [{"name": "report.pdf", "mediaType": "application/pdf", "data": pdf}]})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "telegram_ack", "id": "m2"})
        );
        let got = sink.got.lock().unwrap()[1].images.clone();
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].media_type.as_str(), got[0].data.len()),
            ("image/jpeg", 1_000_000)
        );
        let files = sink.got.lock().unwrap()[1].files.clone();
        assert_eq!(
            files
                .iter()
                .map(|f| (f.name.as_str(), f.data.len()))
                .collect::<Vec<_>>(),
            [("report.pdf", pdf.len())]
        );
        assert!(
            !format!("{got:?}").contains("AAAA"),
            "a picture never prints"
        );
        ws.send(Message::Text(
            json!({"type": "telegram_stop"}).to_string().into(),
        ))
        .await
        .unwrap();
        // Status goes up the line.
        c.handle().telegram_status("Running tests");
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "telegram_status", "text": "Running tests"})
        );
        for _ in 0..100 {
            if *sink.stops.lock().unwrap() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(*sink.stops.lock().unwrap(), 1);
        assert_eq!(c.handle().state(), LineState::Online);
        task.abort();
    }

    #[tokio::test]
    async fn the_household_is_told_the_switches_asks_through_the_hub_and_answers_within_them() {
        let (url, hub) = fake_hub().await;
        let exec = Arc::new(Exec::default());
        let c = client((&url, ""), exec.clone(), Arc::new(Sink::default()));
        c.restore_sharing(Sharing {
            chats: true,
            transcripts: true,
            messages: false,
        });
        let task = tokio::spawn(c.clone().run());
        let (_, mut ws) = hub.await.unwrap();
        let send = |v: Value| Message::Text(v.to_string().into());

        // The switches are the first thing the hub hears.
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "sharing", "chats": true, "transcripts": true, "messages": false})
        );

        // A household call inside the switches runs as the asking person; one outside is refused
        // here, whatever the hub thinks, and runs nothing.
        ws.send(send(
            json!({"type": "peer_call", "id": "p1", "person": "Pat", "op": "list_chats",
                            "args": {"limit": 3}}),
        ))
        .await
        .unwrap();
        let r = next(&mut ws).await;
        assert_eq!(
            (r["id"].as_str(), r["ok"].as_bool()),
            (Some("p1"), Some(true))
        );
        ws.send(send(
            json!({"type": "peer_call", "id": "p2", "person": "Pat", "op": "send_message",
                            "args": {"chatId": "c", "message": "hi"}}),
        ))
        .await
        .unwrap();
        let r = next(&mut ws).await;
        assert_eq!(
            (r["ok"].as_bool(), r["changed"].as_str()),
            (Some(false), Some("no"))
        );
        assert!(r["error"].as_str().unwrap().contains("message their chats"));
        assert_eq!(exec.peer.lock().unwrap().len(), 1);
        assert_eq!(exec.peer.lock().unwrap()[0].0, "Pat");
        assert_eq!(exec.peer.lock().unwrap()[0].1.tool, "app_list_chats");
        assert!(exec.ran.lock().unwrap().is_empty(), "not an outside call");

        // Turning a switch on tells the hub at once and lets the call through.
        c.set_sharing(Sharing {
            chats: true,
            transcripts: true,
            messages: true,
        });
        assert_eq!(next(&mut ws).await["messages"], true);
        ws.send(send(
            json!({"type": "peer_call", "id": "p3", "person": "Pat", "op": "send_message",
                            "args": {"chatId": "c", "message": "hi"}}),
        ))
        .await
        .unwrap();
        assert_eq!(next(&mut ws).await["ok"], true);
        assert_eq!(exec.peer.lock().unwrap()[1].1.args["mode"], "steer");

        // Asking the hub: the answer, or the hub's refusal, comes back to the asker.
        let h = c.handle();
        let ask = tokio::spawn(async move { h.peer("people", json!({})).await });
        let req = next(&mut ws).await;
        assert_eq!(
            (req["type"].as_str(), req["op"].as_str()),
            (Some("peer"), Some("people"))
        );
        ws.send(send(
            json!({"type": "peer_result", "id": req["id"], "ok": true,
                            "result": {"people": []}}),
        ))
        .await
        .unwrap();
        assert_eq!(ask.await.unwrap(), Ok(json!({"people": []})));
        let h = c.handle();
        let ask = tokio::spawn(async move { h.peer("list_chats", json!({"personId": "x"})).await });
        let req = next(&mut ws).await;
        ws.send(send(
            json!({"type": "peer_result", "id": req["id"], "ok": false,
                            "error": "They are offline."}),
        ))
        .await
        .unwrap();
        assert_eq!(ask.await.unwrap(), Err("They are offline.".to_owned()));

        // A request still waiting when the line drops fails at once.
        let h = c.handle();
        let ask = tokio::spawn(async move { h.peer("people", json!({})).await });
        next(&mut ws).await;
        drop(ws);
        assert!(ask.await.unwrap().is_err());
        // And with no line there is nothing to ask.
        for _ in 0..200 {
            if c.handle().state() != LineState::Online {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(c.handle().peer("people", json!({})).await.is_err());
        task.abort();
    }

    #[tokio::test]
    async fn an_undelivered_telegram_message_is_not_acknowledged() {
        let (url, hub) = fake_hub().await;
        let sink = Arc::new(Sink {
            refuse: true,
            ..Default::default()
        });
        let c = client((&url, ""), Arc::new(Exec::default()), sink.clone());
        let task = tokio::spawn(c.clone().run());
        let (_, mut ws) = hub.await.unwrap();
        assert_eq!(next(&mut ws).await["type"], "sharing");
        ws.send(Message::Text(
            json!({"type": "telegram", "id": "m1", "text": "hi"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        // Let the delivery attempt finish before the marker goes up.
        for _ in 0..100 {
            if !sink.got.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.handle().telegram_final("marker");
        // The next frame up is the marker, not an ack.
        assert_eq!(next(&mut ws).await["type"], "telegram_final");
        assert_eq!(sink.got.lock().unwrap().len(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn voice_turns_are_acknowledged_once_delivered_and_the_rest_reach_the_sink() {
        let (url, hub) = fake_hub().await;
        let voice = Arc::new(Voice::default());
        let c = client(
            (&url, ""),
            Arc::new(Exec::default()),
            Arc::new(Sink::default()),
        );
        c.attach_voice(voice.clone());
        let task = tokio::spawn(c.clone().run());
        let (_, mut ws) = hub.await.unwrap();
        assert_eq!(next(&mut ws).await["type"], "sharing");
        let send = |v: Value| Message::Text(v.to_string().into());
        let wait = |n: usize| {
            let voice = voice.clone();
            async move {
                for _ in 0..200 {
                    if voice.got.lock().unwrap().len() >= n {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("the sink never saw {n} turns");
            }
        };

        // Delivered, then acknowledged; every field arrives as sent.
        ws.send(send(
            json!({"type": "voice", "id": "t1", "mode": "chat", "text": "hello", "source": "typed",
                   "sentAt": 5, "ageMinutes": 3, "continues": "t0"}),
        ))
        .await
        .unwrap();
        assert_eq!(next(&mut ws).await, json!({"type": "voice_ack", "id": "t1"}));
        assert_eq!(
            voice.got.lock().unwrap()[0],
            VoiceTurn {
                id: "t1".into(),
                mode: "chat".into(),
                text: "hello".into(),
                source: "typed".into(),
                sent_at: 5,
                age_minutes: Some(3),
                continues: Some("t0".into()),
            }
        );

        // Not taken: no acknowledgement, and the hub's resend is tried again and then answered.
        voice.refuse.store(true, Ordering::SeqCst);
        let again = json!({"type": "voice", "id": "t2", "mode": "conversation", "text": "x"});
        ws.send(send(again.clone())).await.unwrap();
        wait(2).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        voice.refuse.store(false, Ordering::SeqCst);
        ws.send(send(again)).await.unwrap();
        assert_eq!(next(&mut ws).await, json!({"type": "voice_ack", "id": "t2"}));
        assert_eq!(voice.got.lock().unwrap().len(), 3);

        // A resend that arrives while the first is still being delivered is dropped.
        voice.hold.store(true, Ordering::SeqCst);
        let slow = json!({"type": "voice", "id": "t3", "mode": "conversation", "text": "y"});
        ws.send(send(slow.clone())).await.unwrap();
        wait(4).await;
        ws.send(send(slow)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(voice.got.lock().unwrap().len(), 4);
        voice.release.notify_one();
        assert_eq!(next(&mut ws).await, json!({"type": "voice_ack", "id": "t3"}));

        // The rest reach the sink and are never answered.
        for f in [
            json!({"type": "voice_stop", "turn": "t3"}),
            json!({"type": "voice_heard", "reports": [{"reply": "r1", "state": "all", "at": 1}]}),
            json!({"type": "voice_presence", "live": true, "mode": "chat"}),
            json!({"type": "voice_fresh"}),
        ] {
            ws.send(send(f)).await.unwrap();
        }
        for _ in 0..200 {
            if *voice.fresh.lock().unwrap() == 1 && !voice.heard.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(*voice.stops.lock().unwrap(), ["t3".to_owned()]);
        assert_eq!(voice.heard.lock().unwrap()[0]["reply"], "r1");
        assert_eq!(*voice.presence.lock().unwrap(), [(true, "chat".to_owned())]);

        // Upward: the working line, then an answer and news in the shape the hub expects.
        let h = c.handle();
        h.voice_status("t1", "Thinking");
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "voice_status", "turn": "t1", "text": "Thinking"})
        );
        let mut reply = crate::voice::Reply::from_text(
            "r1".into(),
            Some("t1".into()),
            crate::voice::Mode::Conversation,
            "Done. Want the details?",
            "c1".into(),
            9,
        );
        assert!(h.voice_reply(&reply));
        let f = next(&mut ws).await;
        assert_eq!((f["type"].as_str(), f["id"].as_str()), (Some("voice_final"), Some("r1")));
        assert_eq!(f["sentences"][1], json!({"n": 2, "text": "Want the details?"}));
        reply.turn = None;
        assert!(h.voice_reply(&reply));
        assert_eq!(next(&mut ws).await["type"], "voice_notify");
        task.abort();
    }

    #[derive(Default)]
    struct Devices {
        frames: Mutex<Vec<Value>>,
        ups: Mutex<usize>,
        downs: Mutex<usize>,
    }
    impl DeviceSink for Devices {
        fn frame(&self, frame: Value) {
            self.frames.lock().unwrap().push(frame);
        }
        fn line_up(&self) {
            *self.ups.lock().unwrap() += 1;
        }
        fn line_down(&self) {
            *self.downs.lock().unwrap() += 1;
        }
    }

    #[tokio::test]
    async fn device_frames_go_up_after_the_switches_and_come_down_to_the_sink() {
        let (url, hub) = fake_hub().await;
        let devices = Arc::new(Devices::default());
        let c = client(
            (&url, ""),
            Arc::new(Exec::default()),
            Arc::new(Sink::default()),
        );
        c.attach_devices(devices.clone());
        let task = tokio::spawn(c.clone().run());
        let (_, mut ws) = hub.await.unwrap();
        // The switches come first; the device is told the line is up right after them.
        assert_eq!(next(&mut ws).await["type"], "sharing");
        for _ in 0..200 {
            if *devices.ups.lock().unwrap() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(*devices.ups.lock().unwrap(), 1);
        let hello = json!({"type": "device", "uid": "u1", "name": "Desk", "run": true});
        assert!(c.handle().device_frame(hello.clone()));
        assert_eq!(next(&mut ws).await, hello);

        // Device frames reach the sink as sent; other frames and unknown types do not.
        let send = |v: Value| Message::Text(v.to_string().into());
        for f in [
            json!({"type": "devices", "list": []}),
            json!({"type": "dev_call", "id": "h1", "op": "hello"}),
            json!({"type": "dev_result", "id": "r1", "ok": true}),
            json!({"type": "dev_push", "from": "u2", "sub": "chats", "seq": 1, "event": {}}),
            json!({"type": "dev_unheard_of"}),
        ] {
            ws.send(send(f)).await.unwrap();
        }
        for _ in 0..200 {
            if devices.frames.lock().unwrap().len() >= 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let kinds: Vec<_> = devices
            .frames
            .lock()
            .unwrap()
            .iter()
            .map(|f| f["type"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(kinds, ["devices", "dev_call", "dev_result", "dev_push"]);

        // When the line drops the sink is told, and nothing more goes up.
        drop(ws);
        for _ in 0..200 {
            if *devices.downs.lock().unwrap() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(*devices.downs.lock().unwrap(), 1);
        for _ in 0..200 {
            if c.handle().state() != LineState::Online {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!c.handle().device_frame(hello));
        task.abort();
    }

    #[tokio::test]
    async fn without_a_configuration_the_line_stays_off() {
        let c = Client::new(
            Arc::new(RelayConfig::default),
            Arc::new(Exec::default()),
            Arc::new(Sink::default()),
        );
        let task = tokio::spawn(c.clone().run());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(c.handle().state(), LineState::Off);
        task.abort();
    }
}
