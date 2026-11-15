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
//! - `kn_files`, `kn_put_result`, `kn_changed` and `person_result`: the household's knowledge
//!   files and a asked-for hub person (`crate::knowledge_line`), handed to the [`KnowledgeSink`]
//!   as they came and ignored until one
//!   is attached.
//! - The first frame up on every line is `sharing` (the household switches), and another goes
//!   whenever the user changes one. The device announces itself right after it.
//! - Upward, [`LineHandle`] sends the Telegram status and final answer, and alerts Helm starts
//!   ([`crate::notify`]), and for the voice chat its working line and answers.

use crate::config::RelayConfig;
use crate::gate;
use crate::household::{self, Sharing};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
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

/// Runs an admitted call on the PC. `caller` is the hub's client name for the token;
/// `person` is the hub person the hub verified the call is for (`None`: an older hub named
/// none, and the PC answers for its own person as before).
pub trait Executor: Send + Sync {
    fn run<'a>(
        &'a self,
        caller: &'a str,
        person: Option<&'a str>,
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
    /// The hub person this message is for, verified by the hub. `None`: an older hub, which
    /// means the PC's own (home) person.
    pub person: Option<String>,
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
    /// `/stop` from the person's Telegram. `person` `None`: an older hub, the PC's own person.
    fn stop<'a>(&'a self, person: Option<String>) -> BoxFut<'a, ()>;
    /// The user tapped button `index` of the approval ask `id` ([`LineHandle::telegram_ask`]).
    fn answer<'a>(&'a self, person: Option<String>, id: String, index: usize) -> BoxFut<'a, ()>;
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
    /// The hub person this turn is for, verified by the hub. `None`: an older hub, which
    /// means the PC's own (home) person.
    pub person: Option<String>,
}

/// Where voice turns go on the PC. `person` `None` is an older hub's frame, meaning the PC's
/// own (home) person.
pub trait VoiceSink: Send + Sync {
    /// `Ok` once the turn is safely in Helm (it is then acknowledged to the hub).
    fn deliver<'a>(&'a self, turn: VoiceTurn) -> BoxFut<'a, Result<(), String>>;
    /// The phone cancelled this turn after Helm took it.
    fn stop<'a>(&'a self, person: Option<String>, turn: String) -> BoxFut<'a, ()>;
    /// What the phone did and did not play: the hub's reports, as sent.
    fn heard<'a>(&'a self, _person: Option<String>, _reports: Vec<Value>) -> BoxFut<'a, ()> {
        Box::pin(async {})
    }
    /// The phone's screen is in front (`live`) or not, in this mode.
    fn presence(&self, _person: Option<String>, _live: bool, _mode: &str) {}
    /// Start the next voice message in a new chat.
    fn fresh<'a>(&'a self, _person: Option<String>) -> BoxFut<'a, ()> {
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

/// Where the hub's knowledge frames go on the PC (`crate::knowledge_line`).
pub trait KnowledgeSink: Send + Sync {
    /// A `kn_files`, `kn_put_result` or `kn_changed` frame, as the hub sent it.
    fn frame(&self, frame: Value);
    /// A new line is up: knowledge may be exchanged now.
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
    /// Frames for the hub, each with the line it belongs to ([`ANY_LINE`] for any).
    up: mpsc::UnboundedSender<(u64, Value)>,
    /// Which line is up now: it counts up by one with every new connection.
    generation: AtomicU64,
    /// Requests to other people's Helms waiting for the hub's answer, by request id.
    peers: std::sync::Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
    /// The voice frames sent lately, with when. Taking a frame into the queue is not the hub
    /// receiving it: a line that drops can lose what was on its way, so a new line offers these
    /// again. The hub keeps the reply ids it has seen and stores each reply once.
    recent_voice: std::sync::Mutex<VecDeque<(Instant, Value)>>,
    /// Why the last attempt to connect failed (a fixed sentence, never the request), cleared
    /// once a line is up.
    last_refusal: std::sync::Mutex<String>,
}

/// A frame that may go up on any line, the next one included.
const ANY_LINE: u64 = 0;
/// How long a voice answer is offered again on a new line. A dead line can take
/// [`SILENCE`] to notice, so this is longer than that.
const VOICE_REOFFER: Duration = Duration::from_secs(150);
/// The most recent voice frames kept to offer again.
const VOICE_KEPT: usize = 16;

/// How long a household request may wait for the hub's answer.
const PEER_TIMEOUT: Duration = Duration::from_secs(30);

impl LineHandle {
    pub fn state(&self) -> LineState {
        LineState::from_u8(self.state.load(Ordering::SeqCst))
    }
    /// The reason the last connection attempt failed; empty while the line is up or untried.
    pub fn last_refusal(&self) -> String {
        self.last_refusal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    fn set_refusal(&self, text: &str) {
        *self.last_refusal.lock().unwrap_or_else(|p| p.into_inner()) = text.to_owned();
    }
    fn set(&self, s: LineState) {
        self.state.store(s as u8, Ordering::SeqCst);
    }

    /// Queue a frame for the hub; `false` when the queue is gone.
    fn push(&self, line: u64, frame: Value) -> bool {
        self.up.send((line, frame)).is_ok()
    }

    /// The line up now, for a frame that means nothing on any other.
    fn this_line(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Queue a voice frame while the line is up, and keep it to offer again on the next line.
    fn push_voice(&self, frame: Value) -> bool {
        if self.state() != LineState::Online {
            return false;
        }
        {
            let mut kept = self.recent_voice.lock().unwrap_or_else(|p| p.into_inner());
            kept.push_back((Instant::now(), frame.clone()));
            while kept.len() > VOICE_KEPT {
                kept.pop_front();
            }
        }
        self.push(ANY_LINE, frame)
    }

    /// The voice frames to offer again on a new line: those sent lately, oldest first.
    fn voice_to_reoffer(&self) -> Vec<Value> {
        let mut kept = self.recent_voice.lock().unwrap_or_else(|p| p.into_inner());
        kept.retain(|(at, _)| at.elapsed() < VOICE_REOFFER);
        kept.iter().map(|(_, f)| f.clone()).collect()
    }

    /// Ask the hub for something about the household (`op`: `people`, `list_chats`, `read_chat`
    /// or `send_message`) and wait for its answer. Fails at once while the line is down, and when
    /// the line drops or the hub is silent for 30 seconds before it answers.
    pub async fn peer(&self, op: &str, args: Value) -> Result<Value, String> {
        self.request(json!({ "type": "peer", "op": op, "args": args }))
            .await
    }

    /// Send one request frame to the hub and wait for the answer that carries the same id.
    /// The id is made here, so the caller's frame must not set one.
    pub async fn request(&self, mut frame: Value) -> Result<Value, String> {
        if self.state() != LineState::Online {
            return Err("Helm is not connected to the hub.".into());
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (tx, rx) = oneshot::channel();
        self.peers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.clone(), tx);
        frame["id"] = json!(id);
        if !self.push(self.this_line(), frame) {
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
    /// Tell the hub that this PC wants to carry a person's channel ("Use this PC" in Settings).
    /// Fire and forget: the hub answers the next messages that way, and the bot's own commands
    /// report the route. `false` when the line is down and it was not sent.
    pub fn route_pin(&self, person_id: &str, channel: &str) -> bool {
        self.state() == LineState::Online
            && self.push(
                self.this_line(),
                json!({ "type": "route_pin", "personId": person_id, "channel": channel }),
            )
    }

    /// A device frame for the hub. `false` when the line is down and it was not sent: device
    /// requests are never queued for a later line, and one still queued when its line drops is
    /// dropped with it.
    pub fn device_frame(&self, frame: Value) -> bool {
        self.state() == LineState::Online && self.push(self.this_line(), frame)
    }
    /// The one live status message on Telegram, edited in place by the hub.
    pub fn telegram_status(&self, person: &str, text: &str) {
        let _ = self.push(
            ANY_LINE,
            named("telegram_status", person, json!({ "text": text })),
        );
    }
    /// A message Helm starts (an alert). Sent only while the line is up, so it never waits in
    /// the queue for a later line; `false` when it was not sent.
    pub fn telegram_notify(&self, person: &str, text: &str) -> bool {
        self.state() == LineState::Online
            && self.push(
                ANY_LINE,
                named("telegram_notify", person, json!({ "text": text })),
            )
    }
    /// An approval ask: a new message with one button per option. Only a tap on one of them
    /// comes back (`telegram_answer`); `false` when it was not sent.
    pub fn telegram_ask(&self, person: &str, id: &str, text: &str, options: &[String]) -> bool {
        self.state() == LineState::Online
            && self.push(
                ANY_LINE,
                named(
                    "telegram_ask",
                    person,
                    json!({ "id": id, "text": text, "options": options }),
                ),
            )
    }
    /// The ask was settled in Helm or went away: the hub takes its buttons down.
    pub fn telegram_ask_done(&self, person: &str, id: &str, text: &str) {
        let _ = self.push(
            ANY_LINE,
            named(
                "telegram_ask_done",
                person,
                json!({ "id": id, "text": text }),
            ),
        );
    }
    /// The turn's answer, sent by the hub as a new message; the status message goes.
    /// `false` when the line is down and it was not sent, so the mirror can try again.
    pub fn telegram_final(&self, person: &str, text: &str) -> bool {
        self.state() == LineState::Online
            && self.push(
                ANY_LINE,
                named("telegram_final", person, json!({ "text": text })),
            )
    }

    /// The voice chat's working line for a turn. The hub passes it to the phone if it is
    /// listening and drops it otherwise, so it is sent only while the line is up.
    pub fn voice_status(&self, person: &str, turn: &str, text: &str) {
        if self.state() == LineState::Online {
            let _ = self.push(
                self.this_line(),
                named(
                    "voice_status",
                    person,
                    json!({ "turn": turn, "text": text }),
                ),
            );
        }
    }
    /// An answer to a turn (`voice_final`), or news (`voice_notify`). `false` when the line is
    /// down and it was not sent, so the mirror can try again. One that was taken is offered
    /// again on the next line for a while, in case the line dropped with it on the way.
    pub fn voice_reply(&self, person: &str, reply: &crate::voice::Reply) -> bool {
        self.push_voice(named_from(person, reply.frame()))
    }
    /// One attachment, sent just before the reply it belongs to. `false` when it was not sent.
    pub fn voice_attachment(&self, person: &str, frame: Value) -> bool {
        self.push_voice(named_from(person, frame))
    }
}

/// A frame of `kind` with `fields`, naming the hub person `person` it acts for. An empty
/// `person` names none — an older hub, or a profile not linked yet — and the hub then uses
/// the client's own person, as before.
fn named(kind: &str, person: &str, mut fields: Value) -> Value {
    fields["type"] = json!(kind);
    named_from(person, fields)
}

/// Add the person to a frame that is already built (a voice reply or attachment).
fn named_from(person: &str, mut frame: Value) -> Value {
    if !person.is_empty() {
        frame["personId"] = json!(person);
    }
    frame
}

pub struct Client {
    config: Arc<dyn Fn() -> RelayConfig + Send + Sync>,
    executor: Arc<dyn Executor>,
    sink: Arc<dyn TelegramSink>,
    /// Where voice turns go; frames for it are ignored until it is attached.
    voice: std::sync::OnceLock<Arc<dyn VoiceSink>>,
    /// Where the household's device frames go; they are ignored until it is attached.
    devices: std::sync::OnceLock<Arc<dyn DeviceSink>>,
    /// Where the hub's knowledge frames go; they are ignored until it is attached.
    knowledge: std::sync::OnceLock<Arc<dyn KnowledgeSink>>,
    /// Does this PC act for that hub person (a profile of theirs is linked here)? Set while
    /// the services are wired; until then every named person passes, as before.
    person_known: std::sync::OnceLock<PersonCheck>,
    handle: Arc<LineHandle>,
    up_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<(u64, Value)>>,
    /// Hub ids of Telegram messages being delivered now: the hub's resend of one of them is
    /// dropped, since the delivery under way acknowledges it.
    delivering: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// What the household may see of this install; told to the hub on every new line.
    sharing: std::sync::Mutex<Sharing>,
    /// Woken when the saved connection changes, so a new address or token applies at once.
    pub reconfigured: tokio::sync::Notify,
    /// How often the idle line says something itself, in milliseconds (see [`KEEPALIVE_EVERY`]).
    keepalive_ms: AtomicU64,
    /// How long the line may go without proof of either direction, in milliseconds.
    silence_ms: AtomicU64,
}

/// The most pictures, and the most other files, taken from one message.
const MAX_FILES: usize = 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// The hub drops a line that sends nothing for 90 s, and not every proxy carries WebSocket
/// pings across — so the PC says something itself every [`KEEPALIVE_EVERY`], well inside that
/// window, and reads its own sent frame as the line being alive in both directions.
const SILENCE: Duration = Duration::from_secs(100);
/// A third of the hub's 90-second silence window.
const KEEPALIVE_EVERY: Duration = Duration::from_secs(30);
/// The most one frame from the hub may hold: a Telegram message can carry up to 50 MB of files,
/// which is a third larger as base64, plus its text.
const MAX_FRAME: usize = 80 * 1024 * 1024;

/// Does this PC act for that hub person? Set once while the services are wired.
type PersonCheck = Arc<dyn Fn(&str) -> bool + Send + Sync>;

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
            knowledge: std::sync::OnceLock::new(),
            person_known: std::sync::OnceLock::new(),
            handle: Arc::new(LineHandle {
                state: AtomicU8::new(LineState::Off as u8),
                up,
                generation: AtomicU64::new(ANY_LINE),
                peers: Default::default(),
                recent_voice: Default::default(),
                last_refusal: Default::default(),
            }),
            up_rx: tokio::sync::Mutex::new(up_rx),
            delivering: Arc::default(),
            sharing: Default::default(),
            reconfigured: tokio::sync::Notify::new(),
            keepalive_ms: AtomicU64::new(KEEPALIVE_EVERY.as_millis() as u64),
            silence_ms: AtomicU64::new(SILENCE.as_millis() as u64),
        })
    }

    pub fn handle(&self) -> Arc<LineHandle> {
        self.handle.clone()
    }

    /// The hub person this machine was paired as, if it was paired with a code.
    pub fn paired_person(&self) -> Option<String> {
        let person = (self.config)().person_id;
        (!person.is_empty()).then_some(person)
    }

    /// Give the line the sink for voice turns.
    pub fn attach_voice(&self, sink: Arc<dyn VoiceSink>) {
        let _ = self.voice.set(sink);
    }

    /// Give the line the sink for the household's device frames.
    pub fn attach_devices(&self, sink: Arc<dyn DeviceSink>) {
        let _ = self.devices.set(sink);
    }

    /// Give the line the sink for the hub's knowledge frames.
    pub fn attach_knowledge(&self, sink: Arc<dyn KnowledgeSink>) {
        let _ = self.knowledge.set(sink);
    }

    /// Tell the line which hub persons this PC acts for. A frame naming a person the check
    /// refuses is dropped (or refused, for a call), since nobody here could answer it.
    pub fn set_person_check(&self, f: PersonCheck) {
        let _ = self.person_known.set(f);
    }

    /// The hub person a down frame acts for. `Ok(None)`: it names none (an older hub, or a
    /// person not yet linked) — the home profile's, as before. `Ok(Some)`: that person's.
    /// `Err`: it names a person this PC does not act for, so nobody here could take it.
    fn frame_person(&self, v: &Value) -> Result<Option<String>, ()> {
        let named = v
            .get("personId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned);
        match (named, self.person_known.get()) {
            (None, _) => Ok(None),
            (Some(p), None) => Ok(Some(p)),
            (Some(p), Some(f)) if f(&p) => Ok(Some(p)),
            (Some(_), Some(_)) => Err(()),
        }
    }

    /// Tests: the keepalive and silence intervals, scaled down from production.
    #[cfg(test)]
    pub(crate) fn set_intervals(&self, keepalive: Duration, silence: Duration) {
        self.keepalive_ms
            .store(keepalive.as_millis() as u64, Ordering::SeqCst);
        self.silence_ms
            .store(silence.as_millis() as u64, Ordering::SeqCst);
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
        let _ = self.handle.push(ANY_LINE, s.frame());
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

    /// `run`, watched: if the loop ever ends — a panic or a fall-through that should not
    /// happen — it is started again after a backoff, with the reason logged, so a line can
    /// never stay down for good because its task died.
    pub async fn run_supervised(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let c = self.clone();
            let joined = tokio::task::spawn(std::panic::AssertUnwindSafe(c.run())).await;
            match joined {
                Ok(()) => log::error!("relay: the line task ended unexpectedly; restarting it"),
                Err(e) if e.is_panic() => {
                    log::error!("relay: the line task panicked; restarting it: {e}")
                }
                // The task was cancelled with the app: nothing to restart.
                Err(_) => return,
            }
            // Until the loop proves otherwise the line is down, and nothing may wait on it.
            self.handle.fail_peers();
            self.handle.set(LineState::Offline);
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
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
            let mut refusal = String::new();
            for url in cfg.urls() {
                match Self::connect(&url, &cfg.token).await {
                    Ok(ws) => {
                        connected = Some(ws);
                        break;
                    }
                    Err(e) => {
                        log::info!("relay: {e}");
                        refusal = e;
                    }
                }
            }
            self.handle.set_refusal(&refusal);
            match connected {
                Some(ws) => {
                    backoff = Duration::from_secs(1);
                    // A new line: frames queued for an older one are stale from here on. It is
                    // counted before the line shows as up, so a frame sent for it is never taken
                    // for an old one.
                    self.handle.generation.fetch_add(1, Ordering::SeqCst);
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

    /// Serve one connection until it ends (or the configuration changes). Frames queued for an
    /// earlier line that only meant something on it (device frames, a working line) are dropped.
    pub async fn serve(&self, ws: Ws) {
        let (mut sink_ws, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
        let mut up_rx = self.up_rx.lock().await;
        let line = self.handle.this_line();
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
        if let Some(k) = self.knowledge.get() {
            k.line_up();
        }
        // Answers sent lately may have been lost with the old line; the hub stores each once.
        for v in self.handle.voice_to_reoffer() {
            let _ = out_tx.send(v);
        }
        let silence_every = Duration::from_millis(self.silence_ms.load(Ordering::SeqCst));
        let silence = tokio::time::sleep(silence_every);
        tokio::pin!(silence);
        let mut keepalive = tokio::time::interval(Duration::from_millis(
            self.keepalive_ms.load(Ordering::SeqCst),
        ));
        keepalive.tick().await; // the interval's immediate first tick
        loop {
            tokio::select! {
                frame = stream.next() => {
                    silence.as_mut().reset(tokio::time::Instant::now() + silence_every);
                    match frame {
                        Some(Ok(Message::Text(t))) => self.on_frame(t.as_str(), &out_tx),
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => {} // pings are answered by the library
                    }
                }
                Some(v) = out_rx.recv() => {
                    if sink_ws.send(Message::Text(v.to_string().into())).await.is_err() { break; }
                }
                Some((for_line, v)) = up_rx.recv() => {
                    if for_line != ANY_LINE && for_line != line { continue; }
                    if sink_ws.send(Message::Text(v.to_string().into())).await.is_err() { break; }
                }
                // An idle line still has to reach the hub inside its silence window. A
                // WebSocket ping is not enough: a proxy in between may answer it itself and
                // never pass it on. The household switches are a frame the hub already
                // takes, and re-sending what it already has is harmless.
                _ = keepalive.tick() => {
                    let frame = self.sharing().frame();
                    if sink_ws.send(Message::Text(frame.to_string().into())).await.is_err() { break; }
                    // A frame the hub took is proof the line lives; its own pings may be
                    // stopped by the same proxy, so waiting for one to arrive cannot be the
                    // only liveness check.
                    silence.as_mut().reset(tokio::time::Instant::now() + silence_every);
                }
                _ = &mut silence => break,
                _ = self.reconfigured.notified() => break,
            }
        }
        let _ = sink_ws.close().await;
        if let Some(d) = self.devices.get() {
            d.line_down();
        }
        if let Some(k) = self.knowledge.get() {
            k.line_down();
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
                let Ok(person) = self.frame_person(&v) else {
                    let _ = out.send(refusal(
                        &id,
                        "This PC has no profile linked for that person.".to_owned(),
                    ));
                    return;
                };
                let executor = self.executor.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let admitted = gate::check_caller(&tool, &caller).and_then(|()| {
                        if remote {
                            gate::admit_remote(&tool, &args)
                        } else {
                            gate::admit(&tool, &args)
                        }
                    });
                    let reply = match admitted {
                        Err(text) => refusal(&id, text),
                        Ok(admitted) => answer(
                            &id,
                            executor.run(&caller, person.as_deref(), admitted).await,
                        ),
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
            // The hub's answers to the Telegram link requests; each carries back the id we sent.
            Some("tg_link_result" | "tg_status_result" | "tg_unlink_result") => {
                let Some(id) = s("id") else {
                    return;
                };
                let answer = if v.get("ok").and_then(Value::as_bool) == Some(true) {
                    Ok(v)
                } else {
                    Err(s("error").unwrap_or_else(|| "The hub could not answer.".into()))
                };
                self.handle.resolve_peer(&id, answer);
            }
            Some("telegram") => {
                let (Some(id), Some(text)) = (s("id"), s("text")) else {
                    return;
                };
                let Ok(person) = self.frame_person(&v) else {
                    log::info!("relay: a Telegram frame named a person this PC does not act for");
                    return;
                };
                let delivering = self.delivering.clone();
                let key = match &person {
                    Some(p) => format!("{p}/{id}"),
                    None => id.clone(),
                };
                if !delivering
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(key.clone())
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
                let r_person = person.clone().unwrap_or_default();
                let msg = TelegramMessage {
                    id: id.clone(),
                    person,
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
                        .remove(&key);
                    match r {
                        Ok(()) => {
                            let _ = out.send(named("telegram_ack", &r_person, json!({ "id": id })));
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
                let Ok(person) = self.frame_person(&v) else {
                    log::info!("relay: a voice frame named a person this PC does not act for");
                    return;
                };
                // The same dedupe as Telegram's, kept apart from it by a prefix.
                let r_person = person.clone().unwrap_or_default();
                let key = format!("voice/{r_person}/{id}");
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
                    person,
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
                            let _ = out.send(named("voice_ack", &r_person, json!({ "id": id })));
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
                let Ok(person) = self.frame_person(&v) else {
                    return;
                };
                tokio::spawn(async move { voice.stop(person, turn).await });
            }
            Some("voice_heard") => {
                let (Some(reports), Some(voice)) = (
                    v.get("reports").and_then(Value::as_array).cloned(),
                    self.voice.get().cloned(),
                ) else {
                    return;
                };
                let Ok(person) = self.frame_person(&v) else {
                    return;
                };
                tokio::spawn(async move { voice.heard(person, reports).await });
            }
            Some("voice_presence") => {
                if let Some(voice) = self.voice.get() {
                    let Ok(person) = self.frame_person(&v) else {
                        return;
                    };
                    voice.presence(
                        person,
                        v.get("live").and_then(Value::as_bool).unwrap_or(false),
                        s("mode").as_deref().unwrap_or("conversation"),
                    );
                }
            }
            Some("voice_fresh") => {
                let Some(voice) = self.voice.get().cloned() else {
                    return;
                };
                let Ok(person) = self.frame_person(&v) else {
                    return;
                };
                tokio::spawn(async move { voice.fresh(person).await });
            }
            Some("devices" | "dev_call" | "dev_result" | "dev_push") => {
                if let Some(d) = self.devices.get() {
                    d.frame(v.clone());
                }
            }
            Some("kn_files" | "kn_put_result" | "kn_changed" | "person_result") => {
                if let Some(k) = self.knowledge.get() {
                    k.frame(v.clone());
                }
            }
            Some("telegram_stop") => {
                let sink = self.sink.clone();
                let Ok(person) = self.frame_person(&v) else {
                    return;
                };
                tokio::spawn(async move { sink.stop(person).await });
            }
            Some("telegram_answer") => {
                let (Some(id), Some(index)) = (s("id"), v.get("index").and_then(Value::as_u64))
                else {
                    return;
                };
                let Ok(person) = self.frame_person(&v) else {
                    return;
                };
                let sink = self.sink.clone();
                tokio::spawn(async move { sink.answer(person, id, index as usize).await });
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
        ran: Mutex<Vec<(String, Option<String>, gate::Admitted)>>,
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
            person: Option<&'a str>,
            call: gate::Admitted,
        ) -> BoxFut<'a, Result<Value, ToolFailure>> {
            self.ran
                .lock()
                .unwrap()
                .push((caller.to_owned(), person.map(str::to_owned), call));
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
        fn stop<'a>(&'a self, _person: Option<String>) -> BoxFut<'a, ()> {
            *self.stops.lock().unwrap() += 1;
            Box::pin(async {})
        }
        fn answer<'a>(
            &'a self,
            _person: Option<String>,
            _id: String,
            _index: usize,
        ) -> BoxFut<'a, ()> {
            Box::pin(async {})
        }
    }

    #[derive(Default)]
    struct Voice {
        got: Mutex<Vec<VoiceTurn>>,
        stops: Mutex<Vec<(Option<String>, String)>>,
        heard: Mutex<Vec<Value>>,
        presence: Mutex<Vec<(Option<String>, bool, String)>>,
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
        fn stop<'a>(&'a self, person: Option<String>, turn: String) -> BoxFut<'a, ()> {
            self.stops.lock().unwrap().push((person, turn));
            Box::pin(async {})
        }
        fn heard<'a>(&'a self, _person: Option<String>, reports: Vec<Value>) -> BoxFut<'a, ()> {
            self.heard.lock().unwrap().extend(reports);
            Box::pin(async {})
        }
        fn presence(&self, person: Option<String>, live: bool, mode: &str) {
            self.presence
                .lock()
                .unwrap()
                .push((person, live, mode.to_owned()));
        }
        fn fresh<'a>(&'a self, _person: Option<String>) -> BoxFut<'a, ()> {
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
        {
            let ran = exec.ran.lock().unwrap();
            assert_eq!(ran.len(), 2, "only the stamped call ran");
            assert!(ran[1].2.remote && ran[1].2.tool == "app_list_waiting");
        }

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
        c.handle().telegram_status("", "Running tests");
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

        // The PC acts for its linked people: a frame naming one is answered for them, a frame
        // naming anyone else never reaches Helm, and a call for them is refused.
        c.set_person_check(std::sync::Arc::new(|p: &str| p == "7"));
        ws.send(Message::Text(
            json!({"type": "telegram", "id": "m9", "text": "for Sam", "personId": "7",
                   "sentAt": 5})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "telegram_ack", "id": "m9", "personId": "7"})
        );
        assert_eq!(
            sink.got.lock().unwrap().last().unwrap().person.as_deref(),
            Some("7")
        );
        ws.send(Message::Text(
            json!({"type": "telegram", "id": "m10", "text": "for nobody", "personId": "9",
                   "sentAt": 5})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        ws.send(Message::Text(
            json!({"type": "call", "id": "c9", "client": "x", "tool": "app_list_chats",
                   "args": {}, "personId": "9"})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let refusal = next(&mut ws).await;
        assert_eq!(refusal["id"], "c9");
        assert!(
            refusal["error"]
                .as_str()
                .unwrap_or_default()
                .contains("no profile linked"),
            "{refusal}"
        );
        // Nothing from either unknown-person frame reached Helm.
        assert_eq!(
            sink.got
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.id == "m10")
                .count(),
            0
        );
        let ran = exec.ran.lock().unwrap();
        assert!(!ran
            .iter()
            .any(|(_, _, c)| c.tool == "app_list_chats" && c.args.get("x").is_some()));
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
    async fn the_hub_answers_the_telegram_link_requests_and_a_route_pin_goes_up_at_once() {
        let (url, hub) = fake_hub().await;
        let c = client(
            (&url, ""),
            Arc::new(Exec::default()),
            Arc::new(Sink::default()),
        );
        let task = tokio::spawn(c.clone().run());
        let (_, mut ws) = hub.await.unwrap();
        let send = |v: Value| Message::Text(v.to_string().into());
        next(&mut ws).await; // the switches are the first thing the hub hears

        // Asking about the account: the frame goes up with an id, and the answer that carries
        // that id comes back to the asker as the hub sent it.
        let h = c.handle();
        let ask = tokio::spawn(async move {
            h.request(json!({ "type": "tg_status", "personId": "person-p" }))
                .await
        });
        let req = next(&mut ws).await;
        assert_eq!(
            (req["type"].as_str(), req["personId"].as_str()),
            (Some("tg_status"), Some("person-p"))
        );
        ws.send(send(
            json!({"type": "tg_status_result", "id": req["id"], "ok": true, "linked": true, "handle": "@demo_bot"}),
        ))
        .await
        .unwrap();
        let a = ask.await.unwrap().unwrap();
        assert_eq!(
            (a["linked"].as_bool(), a["handle"].as_str()),
            (Some(true), Some("@demo_bot"))
        );

        // The hub's own refusal comes back as its words, not as a timeout.
        let h = c.handle();
        let ask = tokio::spawn(async move {
            h.request(json!({ "type": "tg_link", "personId": "person-p" }))
                .await
        });
        let req = next(&mut ws).await;
        assert_eq!(req["type"].as_str(), Some("tg_link"));
        ws.send(send(
            json!({"type": "tg_link_result", "id": req["id"], "ok": false, "error": "That account already has a link."}),
        ))
        .await
        .unwrap();
        assert_eq!(
            ask.await.unwrap(),
            Err("That account already has a link.".to_owned())
        );

        // "Use this PC": the pin is fire and forget, and it names the person and the channel.
        assert!(c.handle().route_pin("person-p", "telegram"));
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "route_pin", "personId": "person-p", "channel": "telegram"})
        );
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
        c.handle().telegram_final("", "marker");
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
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "voice_ack", "id": "t1"})
        );
        assert_eq!(
            voice.got.lock().unwrap()[0],
            VoiceTurn {
                id: "t1".into(),
                mode: "chat".into(),
                text: "hello".into(),
                person: None,
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
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "voice_ack", "id": "t2"})
        );
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
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "voice_ack", "id": "t3"})
        );

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
        assert_eq!(*voice.stops.lock().unwrap(), [(None, "t3".to_owned())]);
        assert_eq!(voice.heard.lock().unwrap()[0]["reply"], "r1");
        assert_eq!(
            *voice.presence.lock().unwrap(),
            [(None, true, "chat".to_owned())]
        );

        // Upward: the working line, then an answer and news in the shape the hub expects.
        let h = c.handle();
        h.voice_status("", "t1", "Thinking");
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
        assert!(h.voice_reply("", &reply));
        let f = next(&mut ws).await;
        assert_eq!(
            (f["type"].as_str(), f["id"].as_str()),
            (Some("voice_final"), Some("r1"))
        );
        assert_eq!(
            f["sentences"][1],
            json!({"n": 2, "text": "Want the details?"})
        );
        reply.turn = None;
        assert!(h.voice_reply("", &reply));
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
    async fn device_frames_go_up_after_the_switches_come_down_to_the_sink_and_stay_on_their_line() {
        // A hub that takes the line twice.
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/relay", l.local_addr().unwrap());
        let devices = Arc::new(Devices::default());
        let c = client(
            (&url, ""),
            Arc::new(Exec::default()),
            Arc::new(Sink::default()),
        );
        c.attach_devices(devices.clone());
        let task = tokio::spawn(c.clone().run());
        let accept = || async {
            let (s, _) = tokio::time::timeout(Duration::from_secs(10), l.accept())
                .await
                .unwrap()
                .unwrap();
            tokio_tungstenite::accept_async(s).await.unwrap()
        };
        let ups = |n: usize| {
            let devices = devices.clone();
            async move {
                for _ in 0..200 {
                    if *devices.ups.lock().unwrap() == n {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                assert_eq!(*devices.ups.lock().unwrap(), n);
            }
        };
        let mut ws = accept().await;
        // The switches come first; the device is told the line is up right after them.
        assert_eq!(next(&mut ws).await["type"], "sharing");
        ups(1).await;
        let hello = json!({"type": "device", "uid": "u1", "name": "Desk", "run": true});
        assert!(c.handle().device_frame(hello.clone()));
        assert_eq!(next(&mut ws).await, hello);
        // A voice answer goes up at once.
        let shown = json!({"type": "voice_attachment", "reply": "r1", "n": 1});
        assert!(c.handle().voice_attachment("", shown.clone()));
        assert_eq!(next(&mut ws).await, shown);

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
        assert!(!c.handle().device_frame(hello.clone()));

        // On the next line, a frame left over for the old one never goes up, and the voice answer
        // sent lately is offered again, since the old line may have lost it (the hub stores each
        // reply once).
        let old = c.handle().this_line();
        assert!(c
            .handle()
            .push(old, json!({"type": "dev_result", "id": "stale"})));
        let mut ws = accept().await;
        assert_eq!(next(&mut ws).await["type"], "sharing");
        assert_eq!(next(&mut ws).await, shown);
        ups(2).await;
        assert!(c.handle().device_frame(hello.clone()));
        assert_eq!(next(&mut ws).await, hello);
        task.abort();
    }

    #[tokio::test]
    async fn an_idle_line_says_enough_itself_to_survive_the_hubs_silence_window() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/relay", l.local_addr().unwrap());
        let c = client(
            (&url, ""),
            Arc::new(Exec::default()),
            Arc::new(Sink::default()),
        );
        // The production intervals, scaled: 30 s inside a 90 s hub window.
        c.set_intervals(Duration::from_millis(60), Duration::from_millis(600));
        let task = tokio::spawn(c.clone().run());
        let (s, _) = tokio::time::timeout(Duration::from_secs(10), l.accept())
            .await
            .unwrap()
            .unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        // A hub that reads nothing for 180 ms (three keepalives) drops the line, as the
        // real one does. The hub itself sends nothing down, so the line also has to stay
        // up without ever seeing a frame from the hub.
        let window = Duration::from_millis(180);
        let started = Instant::now();
        let mut keepalives = 0;
        while started.elapsed() < Duration::from_millis(1200) {
            match tokio::time::timeout(window, ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    let v: Value = serde_json::from_str(t.as_str()).unwrap();
                    assert_eq!(
                        v["type"], "sharing",
                        "the keepalive is a frame the hub takes"
                    );
                    keepalives += 1;
                }
                // The hub's window passed with nothing from the line: it would have said
                // "line silent, dropping it".
                r => panic!(
                    "the hub would have dropped the line after {keepalives} keepalives: {r:?}"
                ),
            }
        }
        assert!(keepalives >= 12, "a frame about every 60 ms: {keepalives}");
        assert_eq!(
            c.handle().state(),
            LineState::Online,
            "the line is not dropped from the PC side either"
        );
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
