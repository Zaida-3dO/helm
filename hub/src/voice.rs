//! The voice line: a phone talks to its owner's Helm through the hub.
//!
//! This module is the hub's whole memory of that conversation, and it does no I/O of its own but the
//! speech-to-text call and the frames it puts on the relay line and on the phone's live socket.
//! The HTTP routes and the socket loop (`voice_http.rs`) call into [`VoiceBridge`]; Helm's frames
//! arrive through [`VoiceBridge::run`] from the receiver [`Lines::voice_receiver`] hands out.
//!
//! State is kept per person, so one person's phone never sees another person's replies, and in
//! memory only: a restart drops it. The outbox's `seq` then starts from the clock (epoch
//! milliseconds when the bridge was made), so it is above any `lastSeq` a phone kept from before
//! and the phone is sent every new entry. The live socket is kept per phone, one for each voice
//! token: a newer socket of the same token replaces the older, and two phones of one person each
//! keep their own socket, their own acked `seq` and their own screen state.
//!
//! - **Turn queue** (phone to Helm). A turn is made by the phone with an id of its own. Spoken
//!   turns wait in `transcribing` until the speech-to-text result arrives ([`Transcript`]); a turn
//!   still transcribing holds back the ones behind it, so Helm sees them in upload order. A ready
//!   turn goes down the person's best relay line as a `voice` frame and is sent again every
//!   [`RESEND_AFTER_MS`] (or at once on a new connection) until Helm sends `voice_ack`. A turn Helm
//!   has not taken within the queue time expires. At most `queue_max` turns wait at once.
//! - **Outbox** (Helm to phone). Replies, news and the final states of turns get the next `seq`
//!   and wait there until the phone acks a `seq` (cumulative) or they outlive the outbox time,
//!   whichever is first. A reconnecting phone is sent everything above the `lastSeq` it names. An
//!   entry is kept until every phone that has connected lately has acked it.
//!   Attachment bytes are capped; the oldest bytes go first and the attachment is marked dropped.
//! - **Held attachments.** A `voice_attachment` arrives before its reply and waits up to
//!   [`HELD_MS`] for it.
//! - **Heard reports.** The latest report per reply is kept (a later `all` replaces an earlier
//!   `partial`) and forwarded to Helm as `voice_heard` whenever a line is up.
//! - **Presence.** The phones' screen state is forwarded as `voice_presence` (live when any phone
//!   is in front, with the mode of the one that changed last) and repeated to every new relay
//!   connection.
//!
//! Every time-dependent step takes `now` (epoch milliseconds), so tests drive time by hand.
//! [`VoiceBridge::tick`] runs the steps that depend on time or on a line coming up; it runs every
//! [`TICK`] under [`VoiceBridge::run`] and after every call that changes state.

use crate::lines::{Lines, PersonUpstream};
use crate::relay::{Line, Upstream};
use crate::stt::Transcribe;
use crate::telegram::now_ms;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// The voice protocol version.
pub const VERSION: u64 = 1;
/// How often the bridge looks for work that depends on time or on a line coming up.
pub const TICK: Duration = Duration::from_millis(500);
/// A turn sent down the line and not acknowledged this long after is sent again.
pub const RESEND_AFTER_MS: i64 = 45_000;
/// How long an attachment waits for its reply.
pub const HELD_MS: i64 = 60_000;
/// The fewest milliseconds between two working lines passed on to the phone.
pub const STATUS_MIN_MS: i64 = 2_500;
/// The most base64 bytes of attachments held for replies that have not arrived, for the whole hub.
pub const HELD_MAX_BYTES: usize = 6 * 1024 * 1024;
/// The most bytes of recordings the whole hub holds while they wait to be transcribed. A
/// recording that would go over it is refused with [`BUSY`] (429), so uploads in parallel cannot
/// run the container out of memory.
pub const AUDIO_BUDGET: usize = 16 * 1024 * 1024;
/// Replies remembered, so a reply sent again is stored once.
pub const SEEN_REPLIES: usize = 500;
/// Heard reports kept.
pub const HEARD_MAX: usize = 200;
/// Reports one post and one `voice_heard` frame carry.
pub const MAX_REPORTS: usize = 50;

/// Typed text, and the longest transcript kept.
pub const MAX_TEXT_CHARS: usize = 8000;
/// Sentences in a spoken reply, and characters in each.
pub const MAX_SENTENCES: usize = 40;
pub const MAX_SENTENCE_CHARS: usize = 400;
/// A chat-mode reply.
pub const MAX_CHAT_CHARS: usize = 30_000;
/// The working line.
pub const MAX_STATUS_CHARS: usize = 300;
/// Attachments per reply, and the limits on each.
pub const MAX_ATTACHMENTS: usize = 4;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_MARKDOWN_CHARS: usize = 16_000;
pub const MAX_URL_CHARS: usize = 2000;
/// An image or file of 1 MiB, as base64.
pub const MAX_ATTACHMENT_B64: usize = 1_398_104;
/// The longest recording the hub accepts, in seconds.
pub const MAX_AUDIO_SECONDS: f64 = 130.0;
/// Frames from the phone. Frames to it have no cap of their own: the phone takes frames far
/// larger than a reply with four attachments.
pub const MAX_PHONE_FRAME: usize = 16 * 1024;

/// What the phone is told when the speech-to-text server fails; the reason goes to the log only.
pub const STT_FAILED: &str = "The speech-to-text server didn't answer.";
pub const STT_OFF: &str = "Speech-to-text isn't set up on the hub.";
/// What a recording is told when the hub already holds [`AUDIO_BUDGET`] of them.
pub const BUSY: &str = "The hub is busy; try again in a moment.";
/// What a turn is told when too many turns wait.
pub const QUEUE_FULL: &str = "Too many turns are waiting; try again in a moment.";
/// What a turn is told when too many turns wait and Helm is offline.
pub const QUEUE_FULL_OFFLINE: &str = "Helm is offline and the queue is full, so this wasn't kept.";
/// What a turn Helm never took is told.
pub const EXPIRED: &str = "Helm didn't take this one in time.";

const IMAGE_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];
const KINDS: [&str; 6] = ["image", "table", "list", "link", "text", "file"];

/// How a reply is formatted: spoken sentences, or markdown for the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Conversation,
    Chat,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "conversation" => Some(Mode::Conversation),
            "chat" => Some(Mode::Chat),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Conversation => "conversation",
            Mode::Chat => "chat",
        }
    }
}

/// An id made by the phone or by Helm: 8 to 64 letters, digits, `_` or `-`.
pub fn valid_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The first `max` characters of `s`.
fn clamp(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_owned)
}

// ---------------------------------------------------------------------------------------------
// What Helm sends up
// ---------------------------------------------------------------------------------------------

/// One sentence of a spoken reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Sentence {
    pub n: u32,
    pub text: String,
}

/// One attachment Helm sent before its reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub reply: String,
    pub n: u8,
    pub kind: String,
    pub title: String,
    pub media_type: Option<String>,
    /// Base64.
    pub data: Option<String>,
    pub markdown: Option<String>,
    pub url: Option<String>,
}

impl Attachment {
    /// A `voice_attachment` frame, or `None` when it is malformed or over a limit. A picture is a
    /// PNG, JPEG or WebP; a link is `https://` only.
    pub fn parse(v: &Value) -> Option<Attachment> {
        let reply = str_of(v, "reply").filter(|r| valid_id(r))?;
        let n = v
            .get("n")?
            .as_u64()
            .filter(|n| (1..=MAX_ATTACHMENTS as u64).contains(n))? as u8;
        let kind = str_of(v, "kind").filter(|k| KINDS.contains(&k.as_str()))?;
        let title = clamp(str_of(v, "title")?.trim(), MAX_TITLE_CHARS);
        if title.is_empty() {
            return None;
        }
        let (mut media_type, mut data, mut markdown, mut url) = (None, None, None, None);
        match kind.as_str() {
            "image" | "file" => {
                let mt = str_of(v, "mediaType")?.trim().to_owned();
                let d = str_of(v, "data")?;
                let typed = if kind == "image" {
                    IMAGE_TYPES.contains(&mt.as_str())
                } else {
                    !mt.is_empty() && mt.len() <= 100
                };
                if !typed || d.is_empty() || d.len() > MAX_ATTACHMENT_B64 {
                    return None;
                }
                media_type = Some(mt);
                data = Some(d);
            }
            "table" | "list" | "text" => {
                let m = str_of(v, "markdown")?;
                if m.trim().is_empty() || m.chars().count() > MAX_MARKDOWN_CHARS {
                    return None;
                }
                markdown = Some(m);
            }
            _ => {
                let u = str_of(v, "url")?.trim().to_owned();
                let plain = !u.chars().any(|c| c.is_whitespace() || c.is_control());
                if !u.starts_with("https://") || u.len() == 8 || u.len() > MAX_URL_CHARS || !plain {
                    return None;
                }
                url = Some(u);
            }
        }
        Some(Attachment {
            reply,
            n,
            kind,
            title,
            media_type,
            data,
            markdown,
            url,
        })
    }

    /// The attachment as the phone gets it inside a `reply` frame.
    fn payload(&self) -> Value {
        let mut a = json!({ "kind": self.kind, "title": self.title });
        for (k, v) in [
            ("mediaType", &self.media_type),
            ("data", &self.data),
            ("markdown", &self.markdown),
            ("url", &self.url),
        ] {
            if let Some(v) = v {
                a[k] = json!(v);
            }
        }
        a
    }
}

/// A `voice_final` (the answer to a turn) or a `voice_notify` (news Helm starts).
#[derive(Debug, Clone, PartialEq)]
pub struct ReplyIn {
    pub id: String,
    /// The turn it answers; `None` for news.
    pub turn: Option<String>,
    pub mode: Mode,
    /// Conversation mode: 1 to 40 numbered sentences of at most 400 characters. Chat mode: none.
    pub sentences: Vec<Sentence>,
    pub text: String,
    pub truncated: bool,
    pub conversation: String,
    /// When Helm finished it; 0 when it did not say.
    pub at: i64,
    /// How many attachments Helm sent for it.
    pub attachments: usize,
    /// For news: the title of the chat it is about.
    pub about: Option<String>,
}

impl ReplyIn {
    /// A `voice_final` (`notify` false: it names its turn) or `voice_notify` frame. Sizes over a
    /// limit are cut rather than refused; a frame with no id, mode or text is dropped.
    pub fn parse(v: &Value, notify: bool) -> Option<ReplyIn> {
        let id = str_of(v, "id").filter(|i| valid_id(i))?;
        let turn = if notify {
            None
        } else {
            Some(str_of(v, "turn").filter(|t| valid_id(t))?)
        };
        let mode = Mode::parse(&str_of(v, "mode")?)?;
        let mut sentences: Vec<Sentence> = Vec::new();
        if mode == Mode::Conversation {
            for s in v
                .get("sentences")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if sentences.len() == MAX_SENTENCES {
                    break;
                }
                let text = clamp(
                    s.get("text").and_then(Value::as_str).unwrap_or("").trim(),
                    MAX_SENTENCE_CHARS,
                );
                if !text.is_empty() {
                    sentences.push(Sentence {
                        n: sentences.len() as u32 + 1,
                        text,
                    });
                }
            }
        }
        let mut text = str_of(v, "text").unwrap_or_default();
        if text.trim().is_empty() {
            text = sentences
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
        }
        let text = clamp(&text, MAX_CHAT_CHARS);
        if text.trim().is_empty() {
            return None;
        }
        // A spoken reply always has a sentence to speak.
        if mode == Mode::Conversation && sentences.is_empty() {
            sentences.push(Sentence {
                n: 1,
                text: clamp(text.trim(), MAX_SENTENCE_CHARS),
            });
        }
        Some(ReplyIn {
            id,
            turn,
            mode,
            sentences,
            text,
            truncated: v.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            conversation: clamp(&str_of(v, "conversation").unwrap_or_default(), 64),
            at: v.get("at").and_then(Value::as_i64).unwrap_or(0),
            attachments: v
                .get("attachments")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(MAX_ATTACHMENTS as u64) as usize,
            about: v
                .pointer("/about/chat")
                .and_then(Value::as_str)
                .map(|c| clamp(c, 200)),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// What the phone sends
// ---------------------------------------------------------------------------------------------

/// The first frame on the live socket.
#[derive(Debug, Clone, PartialEq)]
pub struct Hello {
    pub last_seq: u64,
    /// The talk screen is open and in front.
    pub live: bool,
    pub mode: Mode,
    pub app: String,
}

/// One frame from the phone on the live socket.
#[derive(Debug, Clone, PartialEq)]
pub enum PhoneFrame {
    Hello(Hello),
    Ack(u64),
    Presence { live: bool, mode: Mode },
    Cancel(String),
}

impl PhoneFrame {
    /// The frame, or `None` when it is too big, malformed or of a kind the hub does not know.
    pub fn parse(text: &str) -> Option<PhoneFrame> {
        if text.len() > MAX_PHONE_FRAME {
            return None;
        }
        let v: Value = serde_json::from_str(text).ok()?;
        let mode = || Mode::parse(&str_of(&v, "mode")?);
        let live = v.get("live").and_then(Value::as_bool);
        match v.get("type")?.as_str()? {
            "hello" => Some(PhoneFrame::Hello(Hello {
                last_seq: v.get("lastSeq").and_then(Value::as_u64).unwrap_or(0),
                live: live?,
                mode: mode()?,
                app: clamp(&str_of(&v, "app").unwrap_or_default(), 64),
            })),
            "ack" => Some(PhoneFrame::Ack(v.get("seq")?.as_u64()?)),
            "presence" => Some(PhoneFrame::Presence {
                live: live?,
                mode: mode()?,
            }),
            "cancel" => Some(PhoneFrame::Cancel(
                str_of(&v, "turn").filter(|t| valid_id(t))?,
            )),
            _ => None,
        }
    }
}

/// How much of a reply the phone heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Played {
    All,
    Partial,
    Never,
}

impl Played {
    fn parse(s: &str) -> Option<Played> {
        match s {
            "all" => Some(Played::All),
            "partial" => Some(Played::Partial),
            "none" => Some(Played::Never),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Played::All => "all",
            Played::Partial => "partial",
            Played::Never => "none",
        }
    }
}

/// What the phone did or did not play of one reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub reply: String,
    pub state: Played,
    pub sentence: Option<u32>,
    pub char_offset: Option<u32>,
    pub replay: bool,
    pub at: i64,
}

impl Report {
    fn parse(v: &Value, now: i64) -> Option<Report> {
        let small = |k: &str| {
            v.get(k)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        Some(Report {
            reply: str_of(v, "reply").filter(|r| valid_id(r))?,
            state: Played::parse(v.get("state")?.as_str()?)?,
            sentence: small("sentence").filter(|n| *n >= 1),
            char_offset: small("charOffset"),
            replay: v.get("replay").and_then(Value::as_bool).unwrap_or(false),
            at: v.get("at").and_then(Value::as_i64).unwrap_or(now),
        })
    }

    /// The `reports` array of a `POST /voice/heard` body: 1 to 50 well-formed reports, or `None`.
    pub fn parse_all(v: &Value, now: i64) -> Option<Vec<Report>> {
        let list = v.get("reports")?.as_array()?;
        if list.is_empty() || list.len() > MAX_REPORTS {
            return None;
        }
        list.iter().map(|r| Report::parse(r, now)).collect()
    }

    fn to_value(&self) -> Value {
        let mut r = json!({ "reply": self.reply, "state": self.state.as_str(), "at": self.at });
        if let Some(s) = self.sentence {
            r["sentence"] = json!(s);
        }
        if let Some(o) = self.char_offset {
            r["charOffset"] = json!(o);
        }
        if self.replay {
            r["replay"] = json!(true);
        }
        r
    }
}

// ---------------------------------------------------------------------------------------------
// Recordings
// ---------------------------------------------------------------------------------------------

/// The length of a WAV or OGG/Opus recording, read from its header, or `None` when it cannot be
/// told cheaply. A WAV needs its `fmt ` and `data` chunks; an OGG file's length is the last
/// page's position at 48 kHz.
pub fn audio_seconds(bytes: &[u8]) -> Option<f64> {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        let (mut at, mut byte_rate) = (12usize, 0u32);
        while at + 8 <= bytes.len() {
            let id = &bytes[at..at + 4];
            let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
            let body = at + 8;
            if id == b"fmt " && body + 12 <= bytes.len() {
                byte_rate = u32::from_le_bytes(bytes[body + 8..body + 12].try_into().ok()?);
            } else if id == b"data" {
                // A stream may leave the size unset: the rest of the file is the data.
                let len = size.min(bytes.len() - body);
                return (byte_rate > 0).then(|| len as f64 / f64::from(byte_rate));
            }
            at = body.checked_add(size)?.checked_add(size & 1)?;
        }
        return None;
    }
    if bytes.starts_with(b"OggS") {
        let tail = &bytes[bytes.len().saturating_sub(64 * 1024)..];
        let page = tail.windows(4).rposition(|w| w == b"OggS")?;
        let granule = i64::from_le_bytes(tail.get(page + 6..page + 14)?.try_into().ok()?);
        return (granule > 0).then(|| granule as f64 / 48_000.0);
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceConfig {
    /// How long a turn waits for Helm to take it.
    pub queue_lifetime: Duration,
    /// How many turns wait at once.
    pub queue_max: usize,
    /// How long a reply waits for the phone to ack it.
    pub outbox_lifetime: Duration,
    /// Entries kept for the phone.
    pub outbox_max: usize,
    /// Base64 bytes of attachments kept for the phone, in all.
    pub outbox_attachment_bytes: usize,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        VoiceConfig {
            queue_lifetime: Duration::from_secs(30 * 60),
            queue_max: 20,
            outbox_lifetime: Duration::from_secs(120 * 60),
            outbox_max: 200,
            outbox_attachment_bytes: 24 * 1024 * 1024,
        }
    }
}

impl VoiceConfig {
    /// Reads `HUB_VOICE` (`on`, the default, or `off`), `HUB_VOICE_QUEUE_MINUTES` (1-1440,
    /// default 30), `HUB_VOICE_QUEUE_MAX` (1-200, default 20) and `HUB_VOICE_OUTBOX_MINUTES`
    /// (1-10080, default 120). `None`: the voice line is off.
    pub fn from_env() -> Result<Option<VoiceConfig>, String> {
        Self::from_vars(&|k: &str| std::env::var(k).ok())
    }

    pub fn from_vars(get: &dyn Fn(&str) -> Option<String>) -> Result<Option<VoiceConfig>, String> {
        let var = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        match var("HUB_VOICE").as_deref() {
            None | Some("on") => {}
            Some("off") => return Ok(None),
            Some(_) => return Err("HUB_VOICE must be on or off".into()),
        }
        let number = |k: &str, range: std::ops::RangeInclusive<u64>, default: u64| match var(k) {
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| range.contains(n))
                .ok_or(format!("{k} must be {}-{}", range.start(), range.end())),
            None => Ok(default),
        };
        let defaults = VoiceConfig::default();
        Ok(Some(VoiceConfig {
            queue_lifetime: Duration::from_secs(
                number("HUB_VOICE_QUEUE_MINUTES", 1..=1440, 30)? * 60,
            ),
            queue_max: number("HUB_VOICE_QUEUE_MAX", 1..=200, 20)? as usize,
            outbox_lifetime: Duration::from_secs(
                number("HUB_VOICE_OUTBOX_MINUTES", 1..=10_080, 120)? * 60,
            ),
            ..defaults
        }))
    }
}

// ---------------------------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------------------------

/// What the live socket's loop is handed to write.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// A JSON text frame.
    Frame(String),
    /// Close the socket with this code and reason.
    Close(u16, &'static str),
}

/// The speech-to-text result of a spoken turn.
#[derive(Debug, Clone, PartialEq)]
pub enum Transcript {
    Text(String),
    /// No speech was heard.
    Empty,
    /// The reason, for the phone.
    Failed(String),
}

/// A turn the phone starts.
#[derive(Debug, Clone, PartialEq)]
pub struct NewTurn {
    pub id: String,
    pub mode: Mode,
    /// The turn this one carries on (the hands-free rejoin).
    pub continues: Option<String>,
    /// `Some` for a typed turn; `None` for a recording that is about to be transcribed.
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    BadId,
    BadText,
    UnknownContinues,
    /// Too many turns wait; `helm_online` is whether a relay line is up now.
    QueueFull {
        helm_online: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Begin {
    /// Accepted; `helm_online` is whether a relay line is up now.
    New {
        helm_online: bool,
    },
    /// The id was seen before and is not failed or expired: its state now.
    Already(&'static str),
    Rejected(Reject),
}

/// What `GET /voice/status` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub helm_online: bool,
    pub stt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnState {
    Transcribing,
    /// Ready for Helm: waiting for a line or for its ack.
    Queued,
    Delivered,
    Empty,
    Failed,
    Expired,
    Cancelled,
}

impl TurnState {
    fn name(self) -> &'static str {
        match self {
            TurnState::Transcribing => "transcribing",
            TurnState::Queued => "queued",
            TurnState::Delivered => "delivered",
            TurnState::Empty => "empty",
            TurnState::Failed => "failed",
            TurnState::Expired => "expired",
            TurnState::Cancelled => "cancelled",
        }
    }

    fn finished(self) -> bool {
        !matches!(self, TurnState::Transcribing | TurnState::Queued)
    }
}

struct TurnRec {
    id: String,
    mode: Mode,
    continues: Option<String>,
    typed: bool,
    text: String,
    /// When the hub got it.
    at: i64,
    state: TurnState,
    changed: i64,
    /// The connection and time of the last send to Helm.
    sent: Option<(u64, i64)>,
    /// The connection it last went down, kept once Helm took it, so a stop reaches that PC.
    went: Option<u64>,
}

struct OutEntry {
    seq: u64,
    at: i64,
    frame: Value,
}

struct HeardEntry {
    report: Report,
    sent: bool,
}

struct Held {
    at: i64,
    attachment: Attachment,
}

struct Live {
    gen: u64,
    tx: UnboundedSender<Out>,
}

/// One phone, which is one voice token. A person may have several.
#[derive(Default)]
struct Phone {
    live: Option<Live>,
    /// Everything up to this `seq` is stored on the phone.
    acked: u64,
    /// Whether the talk screen is in front, its mode, and when that was set (an order, not a time).
    screen: Option<(bool, Mode, u64)>,
    /// When the phone last connected, disconnected or acked.
    last: i64,
}

#[derive(Default)]
struct Person {
    turns: HashMap<String, TurnRec>,
    /// Turn ids in upload order.
    order: VecDeque<String>,
    outbox: VecDeque<OutEntry>,
    next_seq: u64,
    held: Vec<Held>,
    seen: VecDeque<String>,
    heard: VecDeque<HeardEntry>,
    /// What Helm is told: the screen state of the phones together.
    presence: Option<(bool, Mode)>,
    /// The connection the presence was last sent down.
    presence_gen: Option<u64>,
    /// By the registry id of the voice token.
    phones: HashMap<i64, Phone>,
    screen_clock: u64,
    helm_up: Option<bool>,
    last_status: i64,
    /// What the hub-wide caps leave this person, set by the bridge before each call.
    room: Room,
    /// The person's relay lines that are up, set by the bridge before each call. A turn stays
    /// with the PC it went to while that PC's line is up, so two PCs never both answer it.
    lines_up: Vec<Arc<Line>>,
}

/// Attachment bytes one person may hold: the hub-wide caps less what everyone else holds.
#[derive(Default, Clone, Copy)]
struct Room {
    held: usize,
    outbox: usize,
}

fn turn_frame(turn: &str, state: &str) -> Value {
    json!({ "type": "turn", "turn": turn, "state": state })
}

/// Base64 bytes of attachments held for replies that have not arrived.
fn held_bytes(held: &[Held]) -> usize {
    held.iter()
        .map(|h| h.attachment.data.as_ref().map_or(0, String::len))
        .sum()
}

/// Base64 bytes of attachments in a reply frame.
fn attachment_bytes(frame: &Value) -> usize {
    frame
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|a| a.get("data").and_then(Value::as_str).map_or(0, str::len))
        .sum()
}

impl Person {
    /// Send a frame down every open socket of the person's phones.
    fn send_live(&self, frame: &Value) {
        let text = frame.to_string();
        for l in self.phones.values().filter_map(|p| p.live.as_ref()) {
            let _ = l.tx.send(Out::Frame(text.clone()));
        }
    }

    fn has_live(&self) -> bool {
        self.phones.values().any(|p| p.live.is_some())
    }

    /// The line whose current connection is `gen`, while it is up.
    fn line_on(&self, gen: Option<u64>) -> Option<&Arc<Line>> {
        let gen = gen?;
        self.lines_up.iter().find(|l| l.generation() == Some(gen))
    }

    /// Store a frame for the phone under the next `seq`, and send it if a socket is open.
    fn keep(&mut self, mut frame: Value, now: i64) {
        self.next_seq += 1;
        frame["seq"] = json!(self.next_seq);
        self.send_live(&frame);
        self.outbox.push_back(OutEntry {
            seq: self.next_seq,
            at: now,
            frame,
        });
    }

    /// A turn reaches a final state the phone is told about and keeps being told.
    fn end(&mut self, id: &str, state: TurnState, now: i64, extra: &[(&str, Value)]) {
        if let Some(t) = self.turns.get_mut(id) {
            t.state = state;
            t.changed = now;
            t.text.clear();
            t.sent = None;
        }
        let mut f = turn_frame(id, state.name());
        for (k, v) in extra {
            f[*k] = v.clone();
        }
        self.keep(f, now);
    }

    fn forget(&mut self, id: &str) {
        self.turns.remove(id);
        self.order.retain(|o| o != id);
    }

    /// A phone's screen opened, closed or changed mode.
    fn set_screen(&mut self, phone: i64, live: bool, mode: Mode) {
        self.screen_clock += 1;
        let at = self.screen_clock;
        self.phones.entry(phone).or_default().screen = Some((live, mode, at));
        self.refresh_presence();
    }

    /// Helm is told "live" when any phone is in front, with the mode of the phone that changed
    /// last among them; otherwise the phone that changed last.
    fn refresh_presence(&mut self) {
        let newest = |only_live: bool| {
            self.phones
                .values()
                .filter_map(|p| p.screen)
                .filter(|s| s.0 || !only_live)
                .max_by_key(|s| s.2)
                .map(|s| (s.0, s.1))
        };
        let now = newest(true).or_else(|| newest(false));
        if self.presence != now {
            self.presence = now;
            self.presence_gen = None;
        }
    }

    /// The phone stored everything up to `seq`. An entry goes when every phone has it.
    fn ack_seq(&mut self, phone: i64, seq: u64, now: i64) {
        let ph = self.phones.entry(phone).or_default();
        ph.acked = ph.acked.max(seq);
        ph.last = now;
        let floor = self.phones.values().map(|p| p.acked).min().unwrap_or(0);
        self.outbox.retain(|e| e.seq > floor);
    }

    fn record(&mut self, r: Report) {
        if let Some(pos) = self.heard.iter().position(|e| e.report.reply == r.reply) {
            if self.heard[pos].report.at > r.at {
                return;
            }
            self.heard.remove(pos);
        }
        self.heard.push_back(HeardEntry {
            report: r,
            sent: false,
        });
        while self.heard.len() > HEARD_MAX {
            self.heard.pop_front();
        }
    }

    fn ack(&mut self, id: &str, now: i64) {
        let Some(t) = self.turns.get_mut(id) else {
            return;
        };
        if t.state != TurnState::Queued {
            return;
        }
        t.state = TurnState::Delivered;
        t.changed = now;
        t.text.clear();
        t.sent = None;
        self.send_live(&turn_frame(id, "delivered"));
    }

    fn status(&mut self, turn: &str, text: &str, now: i64) {
        if !self.has_live() || now - self.last_status < STATUS_MIN_MS {
            return;
        }
        self.last_status = now;
        self.send_live(&json!({ "type": "status", "turn": turn, "text": text }));
    }

    fn hold(&mut self, a: Attachment, now: i64) {
        if self.seen.contains(&a.reply) {
            // Its reply already came; a late attachment has nowhere to go.
            return;
        }
        self.held
            .retain(|h| !(h.attachment.reply == a.reply && h.attachment.n == a.n));
        if held_bytes(&self.held) + a.data.as_ref().map_or(0, String::len) > self.room.held {
            return;
        }
        self.held.push(Held {
            at: now,
            attachment: a,
        });
    }

    fn reply(&mut self, r: ReplyIn, notify: bool, now: i64) {
        if self.seen.contains(&r.id) {
            return;
        }
        self.seen.push_back(r.id.clone());
        while self.seen.len() > SEEN_REPLIES {
            self.seen.pop_front();
        }
        let mut atts: Vec<Attachment> = Vec::new();
        self.held.retain(|h| {
            if h.attachment.reply == r.id {
                atts.push(h.attachment.clone());
                false
            } else {
                true
            }
        });
        atts.sort_by_key(|a| a.n);
        atts.truncate(MAX_ATTACHMENTS);
        let sentences: Vec<Value> = r
            .sentences
            .iter()
            .map(|s| json!({ "n": s.n, "text": s.text }))
            .collect();
        let frame = json!({
            "type": "reply",
            "id": r.id,
            "turn": r.turn,
            "kind": if notify { "notify" } else { "reply" },
            "mode": r.mode.as_str(),
            "sentences": sentences,
            "text": r.text,
            "truncated": r.truncated,
            "attachments": atts.iter().map(Attachment::payload).collect::<Vec<_>>(),
            "about": r.about.as_ref().map_or(Value::Null, |c| json!({ "chat": c })),
            "conversation": r.conversation,
            "at": if r.at > 0 { r.at } else { now },
        });
        // An answer proves Helm took the turn, whether or not its ack got through.
        if let Some(turn) = &r.turn {
            self.ack(turn, now);
        }
        self.keep(frame, now);
    }

    fn cancel(&mut self, turn: &str, line: &Option<Arc<Line>>, now: i64) -> bool {
        let Some((state, sent, went)) = self
            .turns
            .get(turn)
            .map(|t| (t.state, t.sent.is_some(), t.went))
        else {
            return false;
        };
        // The stop goes to the PC that has the turn while its line is up, else to the best line.
        let to = self.line_on(went).or(line.as_ref()).cloned();
        let stop = || {
            if let Some(l) = &to {
                l.send(&json!({ "type": "voice_stop", "turn": turn }));
            }
        };
        match state {
            TurnState::Transcribing => self.end(turn, TurnState::Cancelled, now, &[]),
            TurnState::Queued => {
                self.end(turn, TurnState::Cancelled, now, &[]);
                // Helm may have taken it with the ack lost on the way.
                if sent {
                    stop();
                }
            }
            TurnState::Delivered => stop(),
            _ => return false,
        }
        true
    }

    /// Everything that depends on time or on a line coming up. Returns the reports it sent to Helm.
    fn sync(&mut self, line: &Option<Arc<Line>>, cfg: &VoiceConfig, now: i64) -> usize {
        let lifetime = cfg.queue_lifetime.as_millis() as i64;
        let expired: Vec<String> = self
            .order
            .iter()
            .filter(|id| {
                self.turns
                    .get(*id)
                    .is_some_and(|t| t.state == TurnState::Queued && now - t.at >= lifetime)
            })
            .cloned()
            .collect();
        for id in expired {
            self.end(&id, TurnState::Expired, now, &[("message", json!(EXPIRED))]);
        }

        let up = line.is_some();
        if self.helm_up.is_some_and(|prev| prev != up) {
            let state = if up { "online" } else { "offline" };
            self.send_live(&json!({ "type": "helm", "state": state }));
        }
        self.helm_up = Some(up);

        let mut forwarded = 0;
        if let Some((line, gen)) = line.as_ref().and_then(|l| Some((l, l.generation()?))) {
            if let Some((live, mode)) = self.presence {
                if self.presence_gen != Some(gen)
                    && line.send(&json!({
                        "type": "voice_presence", "live": live, "mode": mode.as_str()
                    }))
                {
                    self.presence_gen = Some(gen);
                }
            }

            let unsent: Vec<usize> = (0..self.heard.len())
                .filter(|i| !self.heard[*i].sent)
                .collect();
            for chunk in unsent.chunks(MAX_REPORTS) {
                let reports: Vec<Value> = chunk
                    .iter()
                    .map(|i| self.heard[*i].report.to_value())
                    .collect();
                if !line.send(&json!({ "type": "voice_heard", "reports": reports })) {
                    break;
                }
                for i in chunk {
                    self.heard[*i].sent = true;
                }
                forwarded += chunk.len();
            }

            let ids: Vec<String> = self.order.iter().cloned().collect();
            for id in ids {
                let Some(t) = self.turns.get_mut(&id) else {
                    continue;
                };
                match t.state {
                    // A turn still being transcribed holds back the ones after it.
                    TurnState::Transcribing => break,
                    TurnState::Queued => {
                        // A turn sent down a line that is still up is sent again there only; it
                        // moves to the best line when that line drops.
                        let held = t.sent.and_then(|(g, _)| {
                            self.lines_up
                                .iter()
                                .find(|l| l.generation() == Some(g))
                                .map(|l| (l, g))
                        });
                        let (line, gen) = held.unwrap_or((line, gen));
                        let due = t
                            .sent
                            .is_none_or(|(g, at)| g != gen || now - at >= RESEND_AFTER_MS);
                        if !due {
                            continue;
                        }
                        let age = (now - t.at) / 60_000;
                        let frame = json!({
                            "type": "voice",
                            "id": t.id,
                            "mode": t.mode.as_str(),
                            "text": t.text,
                            "source": if t.typed { "typed" } else { "speech" },
                            "sentAt": t.at,
                            "ageMinutes": if age >= 1 { json!(age) } else { Value::Null },
                            "continues": t.continues,
                        });
                        if line.send(&frame) {
                            t.sent = Some((gen, now));
                            t.went = Some(gen);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Housekeeping.
        self.held.retain(|h| now - h.at < HELD_MS);
        let keep_ms = cfg.outbox_lifetime.as_millis() as i64;
        // A phone that has not been seen for the outbox time no longer holds entries back.
        let phones = self.phones.len();
        self.phones
            .retain(|_, p| p.live.is_some() || now - p.last < keep_ms);
        if self.phones.len() != phones {
            self.refresh_presence();
        }
        while self.outbox.front().is_some_and(|e| now - e.at >= keep_ms) {
            self.outbox.pop_front();
        }
        let mut bytes: usize = self.outbox.iter().map(|e| attachment_bytes(&e.frame)).sum();
        for e in self.outbox.iter_mut() {
            if bytes <= self.room.outbox {
                break;
            }
            let own = attachment_bytes(&e.frame);
            if own == 0 {
                continue;
            }
            // Keep the words and say what is missing.
            for a in e
                .frame
                .get_mut("attachments")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if let Some(o) = a.as_object_mut() {
                    if o.remove("data").is_some() {
                        o.insert("dropped".into(), Value::Bool(true));
                    }
                }
            }
            bytes -= own;
        }
        while self.outbox.len() > cfg.outbox_max {
            self.outbox.pop_front();
        }
        let gone: Vec<String> = self
            .turns
            .iter()
            .filter(|(_, t)| t.state.finished() && now - t.changed >= keep_ms)
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            self.forget(&id);
        }
        forwarded
    }
}

/// A share of the hub's recording budget ([`VoiceBridge::reserve_audio`]), given back on drop.
pub struct AudioPermit {
    held: Arc<AtomicUsize>,
    budget: usize,
    bytes: usize,
}

impl AudioPermit {
    /// Hold `bytes` instead. False (and nothing changes) when growing would pass the budget.
    pub fn resize(&mut self, bytes: usize) -> bool {
        if bytes <= self.bytes {
            self.held.fetch_sub(self.bytes - bytes, Ordering::SeqCst);
            self.bytes = bytes;
            return true;
        }
        let more = bytes - self.bytes;
        let mut now = self.held.load(Ordering::SeqCst);
        loop {
            let Some(total) = now.checked_add(more).filter(|t| *t <= self.budget) else {
                return false;
            };
            match self
                .held
                .compare_exchange(now, total, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    self.bytes = bytes;
                    return true;
                }
                Err(seen) => now = seen,
            }
        }
    }
}

impl Drop for AudioPermit {
    fn drop(&mut self) {
        self.held.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

/// The voice line of the whole hub: one [`Person`]'s state per person with a phone or a Helm.
pub struct VoiceBridge {
    lines: Arc<Lines>,
    cfg: VoiceConfig,
    stt: Option<Arc<dyn Transcribe>>,
    /// One recording is transcribed at a time, so the container holds one clip at once and a long
    /// recording never blocks Telegram's own transcriptions.
    stt_gate: tokio::sync::Semaphore,
    people: Mutex<HashMap<i64, Person>>,
    live_gen: AtomicU64,
    /// Bytes of recordings held until they are transcribed, for the whole hub.
    audio_held: Arc<AtomicUsize>,
    audio_budget: usize,
    /// Where every person's outbox `seq` starts: the clock when the bridge was made, so the seqs
    /// of a hub that restarted are above any `lastSeq` a phone kept from before.
    seq_base: u64,
}

impl VoiceBridge {
    pub fn new(lines: Arc<Lines>, cfg: VoiceConfig) -> Self {
        VoiceBridge {
            lines,
            cfg,
            stt: None,
            stt_gate: tokio::sync::Semaphore::new(1),
            people: Mutex::new(HashMap::new()),
            live_gen: AtomicU64::new(0),
            seq_base: u64::try_from(now_ms()).unwrap_or(0),
            audio_held: Arc::new(AtomicUsize::new(0)),
            audio_budget: AUDIO_BUDGET,
        }
    }

    /// Hold at most `bytes` of recordings (instead of [`AUDIO_BUDGET`]).
    pub fn with_audio_budget(mut self, bytes: usize) -> Self {
        self.audio_budget = bytes;
        self
    }

    /// Set aside `bytes` of the hub's recording budget, given back when the permit drops (once
    /// the recording is transcribed, or refused). `None`: the hub holds too much already.
    pub fn reserve_audio(&self, bytes: usize) -> Option<AudioPermit> {
        let mut permit = AudioPermit {
            held: self.audio_held.clone(),
            budget: self.audio_budget,
            bytes: 0,
        };
        permit.resize(bytes).then_some(permit)
    }

    /// Start every outbox `seq` above `base` instead of the clock (tests use small numbers).
    pub fn with_seq_base(mut self, base: u64) -> Self {
        self.seq_base = base;
        self
    }

    /// Spoken turns are transcribed by `stt`.
    pub fn with_stt(mut self, stt: Arc<dyn Transcribe>) -> Self {
        self.stt = Some(stt);
        self
    }

    pub fn has_stt(&self) -> bool {
        self.stt.is_some()
    }

    pub fn config(&self) -> &VoiceConfig {
        &self.cfg
    }

    /// Run `f` on a person's state with their best relay line, then do whatever that made due.
    /// Returns `f`'s result and how many heard reports went to Helm.
    fn with<R>(
        &self,
        person: i64,
        now: i64,
        f: impl FnOnce(&mut Person, &Option<Arc<Line>>) -> R,
    ) -> (R, usize) {
        let line = self.lines.best_up(person);
        let mut people = self.people.lock().unwrap_or_else(|p| p.into_inner());
        // The attachment caps are for the whole hub: this person gets what the others leave.
        let (held, outbox) =
            people
                .iter()
                .filter(|(id, _)| **id != person)
                .fold((0, 0), |(h, o), (_, p)| {
                    let out: usize = p.outbox.iter().map(|e| attachment_bytes(&e.frame)).sum();
                    (h + held_bytes(&p.held), o + out)
                });
        let p = people.entry(person).or_insert_with(|| Person {
            next_seq: self.seq_base,
            ..Person::default()
        });
        p.room = Room {
            held: HELD_MAX_BYTES.saturating_sub(held),
            outbox: self.cfg.outbox_attachment_bytes.saturating_sub(outbox),
        };
        p.lines_up = self
            .lines
            .up_lines_of(person)
            .into_iter()
            .map(|(_, l)| l)
            .collect();
        let r = f(p, &line);
        let forwarded = p.sync(&line, &self.cfg, now);
        (r, forwarded)
    }

    // ----- the phone's calls -----

    /// `GET /voice/status`.
    pub fn status(&self, person: i64) -> Status {
        Status {
            helm_online: self.lines.best_up(person).is_some(),
            stt: self.stt.is_some(),
        }
    }

    /// A new turn from `POST /voice/turn`. A typed turn is ready at once; a spoken one waits for
    /// [`VoiceBridge::transcribe_turn`] (or [`VoiceBridge::heard`]).
    pub fn begin_turn(&self, person: i64, t: NewTurn, now: i64) -> Begin {
        if !valid_id(&t.id) {
            return Begin::Rejected(Reject::BadId);
        }
        let text = match t.text {
            Some(x) => {
                let x = x.trim().to_owned();
                if x.is_empty() || x.chars().count() > MAX_TEXT_CHARS {
                    return Begin::Rejected(Reject::BadText);
                }
                Some(x)
            }
            None => None,
        };
        if t.continues.as_deref().is_some_and(|c| !valid_id(c)) {
            return Begin::Rejected(Reject::UnknownContinues);
        }
        let queue_max = self.cfg.queue_max;
        self.with(person, now, move |p, line| {
            let online = line.is_some();
            if let Some(c) = &t.continues {
                if !p.turns.contains_key(c) {
                    return Begin::Rejected(Reject::UnknownContinues);
                }
            }
            // A failed or expired turn is tried again from the start.
            match p.turns.get(&t.id).map(|r| r.state) {
                Some(TurnState::Failed | TurnState::Expired) => p.forget(&t.id),
                Some(state) => return Begin::Already(state.name()),
                None => {}
            }
            let active = p.turns.values().filter(|r| !r.state.finished()).count();
            if active >= queue_max {
                return Begin::Rejected(Reject::QueueFull {
                    helm_online: online,
                });
            }
            let typed = text.is_some();
            p.turns.insert(
                t.id.clone(),
                TurnRec {
                    id: t.id.clone(),
                    mode: t.mode,
                    continues: t.continues.clone(),
                    typed,
                    text: text.unwrap_or_default(),
                    at: now,
                    state: if typed {
                        TurnState::Queued
                    } else {
                        TurnState::Transcribing
                    },
                    changed: now,
                    sent: None,
                    went: None,
                },
            );
            p.order.push_back(t.id.clone());
            if !typed {
                p.send_live(&turn_frame(&t.id, "transcribing"));
            } else if !online {
                p.send_live(&turn_frame(&t.id, "queued"));
            }
            Begin::New {
                helm_online: online,
            }
        })
        .0
    }

    /// The speech-to-text result of a spoken turn. Ignored unless the turn is still being
    /// transcribed (it may have been cancelled meanwhile).
    pub fn heard(&self, person: i64, turn: &str, result: Transcript, now: i64) {
        self.with(person, now, |p, line| {
            if p.turns.get(turn).map(|t| t.state) != Some(TurnState::Transcribing) {
                return;
            }
            let text = match result {
                Transcript::Text(t) if !t.trim().is_empty() => clamp(t.trim(), MAX_TEXT_CHARS),
                Transcript::Text(_) | Transcript::Empty => {
                    p.end(turn, TurnState::Empty, now, &[]);
                    return;
                }
                Transcript::Failed(why) => {
                    p.end(turn, TurnState::Failed, now, &[("message", json!(why))]);
                    return;
                }
            };
            if let Some(t) = p.turns.get_mut(turn) {
                t.text.clone_from(&text);
                t.state = TurnState::Queued;
                t.changed = now;
            }
            let mut f = turn_frame(turn, "heard");
            f["transcript"] = json!(text);
            p.keep(f, now);
            if line.is_none() {
                p.send_live(&turn_frame(turn, "queued"));
            }
        });
    }

    /// Transcribe a recording on the speech-to-text server (one at a time) and record the result
    /// as [`VoiceBridge::heard`]. A failure is logged here and told to the phone in plain words.
    pub async fn transcribe_turn(&self, person: i64, turn: &str, audio: Vec<u8>, mime: &str) {
        let Some(stt) = self.stt.clone() else {
            self.heard(person, turn, Transcript::Failed(STT_OFF.into()), now_ms());
            return;
        };
        let Ok(_permit) = self.stt_gate.acquire().await else {
            return;
        };
        let waiting = {
            let people = self.people.lock().unwrap_or_else(|p| p.into_inner());
            people
                .get(&person)
                .and_then(|p| p.turns.get(turn))
                .is_some_and(|t| t.state == TurnState::Transcribing)
        };
        if !waiting {
            return;
        }
        let name = if mime.contains("wav") {
            "turn.wav"
        } else {
            "turn.ogg"
        };
        let result = match stt.transcribe(audio, name, mime).await {
            Ok(text) => Transcript::Text(text),
            Err(why) => {
                eprintln!("voice: transcription failed: {why}");
                Transcript::Failed(STT_FAILED.into())
            }
        };
        self.heard(person, turn, result, now_ms());
    }

    /// The phone cancels a turn (the rejoin). True when the hub knew the turn.
    pub fn cancel(&self, person: i64, turn: &str, now: i64) -> bool {
        self.with(person, now, |p, line| p.cancel(turn, line, now))
            .0
    }

    /// `POST /voice/heard`: keep the latest report per reply and forward to Helm what a line can
    /// take. Returns how many reports went to Helm now (0 while the line is down; they go later).
    pub fn record_heard(&self, person: i64, reports: Vec<Report>, now: i64) -> usize {
        self.with(person, now, move |p, _| {
            for r in reports {
                p.record(r);
            }
        })
        .1
    }

    /// `POST /voice/fresh`: tell Helm to start a new conversation. False when the line is down.
    pub fn fresh(&self, person: i64) -> bool {
        self.lines
            .best_up(person)
            .is_some_and(|l| l.send(&json!({ "type": "voice_fresh" })))
    }

    // ----- the live socket -----

    /// A phone opens the live socket: it gets `welcome` and then everything above its `lastSeq`.
    /// `phone` is the registry id of its voice token. An older socket of the same token is closed
    /// with code 4000; the person's other phones keep theirs. Returns this socket's generation,
    /// which the other live calls name.
    pub fn attach_live(
        &self,
        person: i64,
        phone: i64,
        hello: &Hello,
        tx: UnboundedSender<Out>,
        now: i64,
    ) -> u64 {
        let gen = self.live_gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.with(person, now, |p, line| {
            let me = tx.clone();
            let send = |frame: &Value| {
                let _ = me.send(Out::Frame(frame.to_string()));
            };
            let entry = p.phones.entry(phone).or_default();
            if let Some(old) = entry.live.replace(Live { gen, tx }) {
                let _ = old.tx.send(Out::Close(4000, "replaced"));
            }
            p.helm_up = Some(line.is_some());
            send(&json!({
                "type": "welcome",
                "version": VERSION,
                "helm": if line.is_some() { "online" } else { "offline" },
                "seq": p.next_seq,
            }));
            // A phone that remembers a seq the hub never issued met a hub that restarted: it
            // gets everything the hub holds.
            let after = if hello.last_seq > p.next_seq {
                0
            } else {
                hello.last_seq
            };
            for e in p.outbox.iter().filter(|e| e.seq > after) {
                send(&e.frame);
            }
            if let Some(ph) = p.phones.get_mut(&phone) {
                ph.acked = after;
                ph.last = now;
            }
            p.set_screen(phone, hello.live, hello.mode);
        });
        gen
    }

    /// The socket of generation `gen` ended. That phone's screen goes with it; a newer socket of
    /// the same token, and the person's other phones, are left.
    pub fn detach_live(&self, person: i64, phone: i64, gen: u64, now: i64) {
        self.with(person, now, |p, _| {
            let Some(ph) = p.phones.get_mut(&phone) else {
                return;
            };
            if ph.live.as_ref().is_some_and(|l| l.gen == gen) {
                ph.live = None;
                ph.last = now;
                let mode = ph.screen.map_or(Mode::Conversation, |s| s.1);
                p.set_screen(phone, false, mode);
            }
        });
    }

    /// The phone stored everything up to `seq`. The hub drops an entry once every phone has it.
    pub fn live_ack(&self, person: i64, phone: i64, seq: u64, now: i64) {
        self.with(person, now, |p, _| p.ack_seq(phone, seq, now));
    }

    /// The talk screen opened or closed, or the mode changed. Ignored from a replaced socket.
    pub fn live_presence(
        &self,
        person: i64,
        phone: i64,
        gen: u64,
        live: bool,
        mode: Mode,
        now: i64,
    ) {
        self.with(person, now, |p, _| {
            let current = p
                .phones
                .get(&phone)
                .and_then(|ph| ph.live.as_ref())
                .is_some_and(|l| l.gen == gen);
            if current {
                p.set_screen(phone, live, mode);
            }
        });
    }

    // ----- Helm's frames -----

    /// One voice frame Helm sent up the line.
    pub fn on_upstream(&self, person: i64, u: Upstream, now: i64) {
        self.with(person, now, |p, _| match u {
            Upstream::VoiceAck(id) => p.ack(&id, now),
            Upstream::VoiceStatus { turn, text } => p.status(&turn, &text, now),
            Upstream::VoiceAttachment(a) => p.hold(a, now),
            Upstream::VoiceFinal(r) => p.reply(r, false, now),
            Upstream::VoiceNotify(r) => p.reply(r, true, now),
            _ => {}
        });
    }

    /// Do what time or a new line makes due, for everyone.
    pub fn tick(&self, now: i64) {
        let ids: Vec<i64> = self
            .people
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.with(id, now, |_, _| ());
        }
    }

    /// Tick for as long as the hub runs, and handle what the lines send up.
    pub async fn run(self: Arc<Self>, mut upstream: UnboundedReceiver<PersonUpstream>) {
        let ticker = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(TICK);
            loop {
                iv.tick().await;
                ticker.tick(now_ms());
            }
        });
        while let Some((person, u)) = upstream.recv().await {
            self.on_upstream(person, u, now_ms());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Client, Scopes};
    use crate::registry::Registry;
    use crate::stt::VoiceFuture;
    use tokio::sync::mpsc;

    const T0: i64 = 1_800_000_000_000;
    const PERSON: i64 = 10;
    /// The registry id of the phone's voice token.
    const PHONE: i64 = 100;

    fn id(n: u32) -> String {
        format!("synthetic-{n:04}")
    }

    struct Rig {
        bridge: VoiceBridge,
        /// The PC's line, down until [`helm_up`].
        line: Arc<Line>,
    }

    fn rig(cfg: VoiceConfig) -> Rig {
        let registry = Arc::new(Mutex::new(Registry::open_in_memory().unwrap()));
        let (lines, _up) = Lines::new(registry);
        let lines = Arc::new(lines);
        let line = lines.line_for(&Client {
            id: 1,
            name: "pc".into(),
            person_id: PERSON,
            person: "p".into(),
            machine: "pc".into(),
            scopes: Scopes::default(),
        });
        Rig {
            bridge: VoiceBridge::new(lines, cfg).with_seq_base(0),
            line,
        }
    }

    /// Bring the PC's line up; the receiver sees what the hub sends down it.
    fn helm_up(r: &Rig) -> (u64, mpsc::UnboundedReceiver<String>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let gen = r.line.attach(tx);
        assert_eq!(drain_json(&mut rx)[0]["type"], "welcome");
        (gen, rx)
    }

    fn drain_json(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<Value> {
        let mut out = vec![];
        while let Ok(f) = rx.try_recv() {
            out.push(serde_json::from_str(&f).unwrap());
        }
        out
    }

    /// A phone opens the live socket.
    fn phone(r: &Rig, last_seq: u64, live: bool, now: i64) -> (u64, mpsc::UnboundedReceiver<Out>) {
        phone_of(r, PHONE, last_seq, live, now)
    }

    /// The phone with this voice token opens the live socket.
    fn phone_of(
        r: &Rig,
        token: i64,
        last_seq: u64,
        live: bool,
        now: i64,
    ) -> (u64, mpsc::UnboundedReceiver<Out>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let hello = Hello {
            last_seq,
            live,
            mode: Mode::Conversation,
            app: "test/1".into(),
        };
        (r.bridge.attach_live(PERSON, token, &hello, tx, now), rx)
    }

    fn frames(rx: &mut mpsc::UnboundedReceiver<Out>) -> Vec<Value> {
        let mut out = vec![];
        while let Ok(o) = rx.try_recv() {
            if let Out::Frame(f) = o {
                out.push(serde_json::from_str(&f).unwrap());
            }
        }
        out
    }

    /// `type`, or `type:state` for the frames that have a state.
    fn types(fs: &[Value]) -> Vec<String> {
        fs.iter()
            .map(|f| {
                let t = f["type"].as_str().unwrap();
                match f["state"].as_str() {
                    Some(s) => format!("{t}:{s}"),
                    None => t.to_owned(),
                }
            })
            .collect()
    }

    fn typed(n: u32, text: &str) -> NewTurn {
        NewTurn {
            id: id(n),
            mode: Mode::Conversation,
            continues: None,
            text: Some(text.into()),
        }
    }

    fn spoken(n: u32) -> NewTurn {
        NewTurn {
            text: None,
            ..typed(n, "")
        }
    }

    fn final_frame(reply: u32, turn: u32, text: &str) -> Value {
        json!({
            "type": "voice_final", "id": id(reply), "turn": id(turn), "mode": "conversation",
            "sentences": [{"n": 1, "text": text}, {"n": 2, "text": "Want the details?"}],
            "text": format!("{text} Want the details?"), "truncated": false,
            "conversation": "c-synthetic", "at": 0, "attachments": 0
        })
    }

    fn answer(reply: u32, turn: u32) -> Upstream {
        let frame = final_frame(reply, turn, "Synthetic answer.");
        Upstream::VoiceFinal(ReplyIn::parse(&frame, false).unwrap())
    }

    fn picture(reply: u32, n: u8) -> Upstream {
        Upstream::VoiceAttachment(
            Attachment::parse(&json!({
                "reply": id(reply), "n": n, "kind": "image", "title": "A picture",
                "mediaType": "image/png", "data": "A".repeat(100)
            }))
            .unwrap(),
        )
    }

    #[test]
    fn what_the_phone_and_helm_may_send_is_checked() {
        // Phone frames.
        for (text, ok) in [
            (
                r#"{"type":"hello","lastSeq":0,"live":true,"mode":"chat","app":"app/0.3.0"}"#,
                true,
            ),
            (
                r#"{"type":"hello","live":false,"mode":"conversation"}"#,
                true,
            ),
            (r#"{"type":"hello","live":true,"mode":"loud"}"#, false),
            (r#"{"type":"hello","mode":"chat"}"#, false),
            (r#"{"type":"ack","seq":41}"#, true),
            (r#"{"type":"ack","seq":-1}"#, false),
            (r#"{"type":"presence","live":false,"mode":"chat"}"#, true),
            (r#"{"type":"cancel","turn":"synthetic-0001"}"#, true),
            (r#"{"type":"cancel","turn":"x"}"#, false),
            (r#"{"type":"sing"}"#, false),
            ("not json", false),
        ] {
            assert_eq!(PhoneFrame::parse(text).is_some(), ok, "{text}");
        }
        let big = format!(
            r#"{{"type":"ack","seq":1,"pad":"{}"}}"#,
            "x".repeat(MAX_PHONE_FRAME)
        );
        assert!(PhoneFrame::parse(&big).is_none());

        // Attachments: each kind's rules.
        let att = |kind: &str, extra: Value| {
            let mut v = json!({"reply": id(1), "n": 1, "kind": kind, "title": "A caption"});
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            Attachment::parse(&v)
        };
        let b64 = |n: usize| "A".repeat(n);
        assert!(att("image", json!({"mediaType": "image/png", "data": b64(100)})).is_some());
        assert!(att("image", json!({"mediaType": "image/gif", "data": b64(100)})).is_none());
        let huge = json!({"mediaType": "image/png", "data": b64(MAX_ATTACHMENT_B64 + 1)});
        assert!(att("image", huge).is_none());
        assert!(att(
            "file",
            json!({"mediaType": "application/pdf", "data": b64(10)})
        )
        .is_some());
        assert!(att("table", json!({"markdown": "| a |\n|---|\n| 1 |"})).is_some());
        assert!(att("list", json!({"markdown": " "})).is_none());
        let long_md = "x".repeat(MAX_MARKDOWN_CHARS + 1);
        assert!(att("text", json!({"markdown": long_md})).is_none());
        assert!(att("link", json!({"url": "https://example.test/a"})).is_some());
        for bad in [
            "http://example.test",
            "https://",
            "https://a b",
            "javascript:alert(1)",
        ] {
            assert!(att("link", json!({"url": bad})).is_none(), "{bad}");
        }
        assert!(att("poem", json!({"markdown": "x"})).is_none());
        let long_title = "t".repeat(MAX_TITLE_CHARS + 30);
        let cut = att("text", json!({"markdown": "x", "title": long_title})).unwrap();
        assert_eq!(cut.title.chars().count(), MAX_TITLE_CHARS);
        assert!(att("text", json!({"markdown": "x", "title": "  "})).is_none());
        assert!(att("text", json!({"markdown": "x", "n": 5})).is_none());
        assert!(att("text", json!({"markdown": "x", "reply": "short"})).is_none());

        // Replies: sentences numbered afresh, sizes cut, news has no turn.
        let many: Vec<Value> = (0..60)
            .map(|i| json!({"n": i, "text": format!("S{i}")}))
            .collect();
        let reply = ReplyIn::parse(
            &json!({"id": id(2), "turn": id(1), "mode": "conversation", "sentences": many,
                    "text": "", "attachments": 9}),
            false,
        )
        .unwrap();
        assert_eq!(reply.sentences.len(), MAX_SENTENCES);
        assert_eq!(reply.sentences[39].n, 40);
        assert!(
            reply.text.starts_with("S0 S1"),
            "text falls back to sentences"
        );
        assert_eq!(reply.attachments, MAX_ATTACHMENTS);
        let chat = ReplyIn::parse(
            &json!({"id": id(2), "turn": id(1), "mode": "chat",
                    "text": "y".repeat(MAX_CHAT_CHARS + 5),
                    "sentences": [{"n": 1, "text": "ignored"}]}),
            false,
        )
        .unwrap();
        assert!(chat.sentences.is_empty());
        assert_eq!(chat.text.chars().count(), MAX_CHAT_CHARS);
        let news = ReplyIn::parse(
            &json!({"id": id(3), "turn": null, "mode": "conversation", "text": "Done.",
                    "about": {"chat": "Build"}}),
            true,
        )
        .unwrap();
        assert_eq!((news.turn, news.about.as_deref()), (None, Some("Build")));
        for (v, notify) in [
            (json!({"id": id(4), "mode": "chat", "text": "x"}), false), // an answer names its turn
            (
                json!({"id": id(4), "turn": id(1), "mode": "loud", "text": "x"}),
                false,
            ),
            (
                json!({"id": "short", "turn": id(1), "mode": "chat", "text": "x"}),
                false,
            ),
            (
                json!({"id": id(4), "turn": id(1), "mode": "chat", "text": "  "}),
                false,
            ),
            (json!({"id": id(4), "mode": "chat"}), true),
        ] {
            assert!(ReplyIn::parse(&v, notify).is_none(), "{v}");
        }

        // Heard reports.
        let ok = json!({"reports": [
            {"reply": id(1), "state": "partial", "sentence": 2, "charOffset": 5, "at": 7},
            {"reply": id(2), "state": "none"}
        ]});
        let reports = Report::parse_all(&ok, 99).unwrap();
        assert_eq!(reports[0].sentence, Some(2));
        assert_eq!((reports[1].at, reports[1].state), (99, Played::Never));
        let fifty_one: Vec<Value> = (0..51)
            .map(|_| json!({"reply": id(1), "state": "all"}))
            .collect();
        for bad in [
            json!({"reports": []}),
            json!({"reports": [{"reply": id(1), "state": "most"}]}),
            json!({"reports": [{"reply": "x", "state": "all"}]}),
            json!({ "reports": fifty_one }),
            json!({"nope": 1}),
        ] {
            assert!(Report::parse_all(&bad, 0).is_none(), "{bad}");
        }
    }

    #[test]
    fn configuration_and_recording_lengths() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            VoiceConfig::from_vars(&|k: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| (*v).to_owned())
            })
        };
        assert_eq!(vars(&[]), Ok(Some(VoiceConfig::default())));
        assert_eq!(vars(&[("HUB_VOICE", "off")]), Ok(None));
        let c = vars(&[
            ("HUB_VOICE_QUEUE_MINUTES", "5"),
            ("HUB_VOICE_QUEUE_MAX", "3"),
            ("HUB_VOICE_OUTBOX_MINUTES", "60"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            (c.queue_lifetime, c.queue_max, c.outbox_lifetime),
            (Duration::from_secs(300), 3, Duration::from_secs(3600))
        );
        for bad in [
            &[("HUB_VOICE", "maybe")][..],
            &[("HUB_VOICE_QUEUE_MINUTES", "0")],
            &[("HUB_VOICE_QUEUE_MAX", "201")],
            &[("HUB_VOICE_OUTBOX_MINUTES", "soon")],
        ] {
            assert!(vars(bad).is_err(), "{bad:?}");
        }

        // A 16 kHz mono 16-bit WAV: 32000 bytes a second.
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&[1, 0, 1, 0]);
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&32_000u32.to_le_bytes());
        wav.extend_from_slice(&[2, 0, 16, 0]);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&64_000u32.to_le_bytes());
        wav.extend_from_slice(&vec![0u8; 64_000]);
        assert_eq!(audio_seconds(&wav), Some(2.0));
        // An OGG page whose position is 96000 samples at 48 kHz.
        let mut ogg = b"OggS\0\0".to_vec();
        ogg.extend_from_slice(&96_000i64.to_le_bytes());
        ogg.extend_from_slice(&[0; 20]);
        assert_eq!(audio_seconds(&ogg), Some(2.0));
        assert_eq!(audio_seconds(b"hello"), None);
    }

    #[tokio::test]
    async fn a_typed_turn_waits_for_helm_is_resent_and_answered() {
        let r = rig(VoiceConfig {
            queue_max: 2,
            ..VoiceConfig::default()
        });
        let (g1, mut live) = phone(&r, 0, true, T0);
        let first = frames(&mut live);
        assert_eq!(types(&first), ["welcome"]);
        assert_eq!(first[0]["helm"], "offline");
        assert_eq!(first[0]["seq"], 0);

        // Helm is offline: accepted, queued, and the rules of the queue hold.
        let begin = r
            .bridge
            .begin_turn(PERSON, typed(1, "Synthetic question one."), T0);
        assert_eq!(begin, Begin::New { helm_online: false });
        assert_eq!(types(&frames(&mut live)), ["turn:queued"]);
        let again = r.bridge.begin_turn(PERSON, typed(1, "again"), T0);
        assert_eq!(again, Begin::Already("queued"));
        for (turn, want) in [
            (
                NewTurn {
                    id: "short".into(),
                    ..typed(2, "x")
                },
                Reject::BadId,
            ),
            (typed(2, "   "), Reject::BadText),
            (typed(2, &"x".repeat(MAX_TEXT_CHARS + 1)), Reject::BadText),
            (
                NewTurn {
                    continues: Some(id(77)),
                    ..typed(2, "x")
                },
                Reject::UnknownContinues,
            ),
        ] {
            assert_eq!(r.bridge.begin_turn(PERSON, turn, T0), Begin::Rejected(want));
        }
        let second = NewTurn {
            continues: Some(id(1)),
            ..typed(2, "Second.")
        };
        let begin = r.bridge.begin_turn(PERSON, second, T0 + 1);
        assert_eq!(begin, Begin::New { helm_online: false });
        let third = r.bridge.begin_turn(PERSON, typed(3, "third"), T0);
        assert_eq!(
            third,
            Begin::Rejected(Reject::QueueFull { helm_online: false })
        );
        frames(&mut live);
        assert!(!r.bridge.status(PERSON).helm_online);

        // The line comes up two minutes later: the phone hears of it, Helm gets the presence
        // and both turns in order, the first marked as two minutes old.
        let (_gen, mut helm) = helm_up(&r);
        r.bridge.tick(T0 + 120_000);
        assert_eq!(types(&frames(&mut live)), ["helm:online"]);
        let down = drain_json(&mut helm);
        assert_eq!(types(&down), ["voice_presence", "voice", "voice"]);
        assert_eq!(
            (down[0]["live"].as_bool(), down[0]["mode"].as_str()),
            (Some(true), Some("conversation"))
        );
        assert_eq!(down[1]["id"], json!(id(1)));
        assert_eq!(down[1]["text"], "Synthetic question one.");
        assert_eq!(down[1]["source"], "typed");
        assert_eq!(down[1]["ageMinutes"], 2);
        assert_eq!(down[1]["continues"], Value::Null);
        assert_eq!(down[2]["continues"], json!(id(1)));
        assert_eq!(down[2]["sentAt"], T0 + 1);

        // A full queue with Helm online says so: Helm has not taken the two yet.
        let busy = r.bridge.begin_turn(PERSON, typed(3, "third"), T0 + 120_001);
        assert_eq!(
            busy,
            Begin::Rejected(Reject::QueueFull { helm_online: true })
        );

        // Not sent again before 45 s, then sent again.
        r.bridge.tick(T0 + 120_000 + 44_000);
        assert!(drain_json(&mut helm).is_empty());
        r.bridge.tick(T0 + 120_000 + 46_000);
        assert_eq!(types(&drain_json(&mut helm)), ["voice", "voice"]);

        // Helm takes the first: the phone is told, and it is never sent again.
        r.bridge
            .on_upstream(PERSON, Upstream::VoiceAck(id(1)), T0 + 170_000);
        assert_eq!(types(&frames(&mut live)), ["turn:delivered"]);
        r.bridge.tick(T0 + 220_000);
        let resent = drain_json(&mut helm);
        assert_eq!(types(&resent), ["voice"]);
        assert_eq!(resent[0]["id"], json!(id(2)));
        let late = r.bridge.begin_turn(PERSON, typed(1, "late"), T0 + 230_000);
        assert_eq!(late, Begin::Already("delivered"));

        // A working line passes at most every 2.5 s, and only to an open socket.
        for (at, text) in [
            (0, "Thinking"),
            (1_000, "Too soon"),
            (2_600, "Reading files"),
        ] {
            let status = Upstream::VoiceStatus {
                turn: id(2),
                text: text.into(),
            };
            r.bridge.on_upstream(PERSON, status, T0 + 300_000 + at);
        }
        let texts: Vec<String> = frames(&mut live)
            .iter()
            .filter(|f| f["type"] == "status")
            .map(|f| f["text"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(texts, ["Thinking", "Reading files"]);

        // The answer arrives with one attachment sent before it: a reply with the next seq,
        // and an answer proves Helm took the turn even if its ack never came.
        let link = json!({"reply": id(9), "n": 1, "kind": "link", "title": "The page",
                          "url": "https://example.test/p"});
        let link = Upstream::VoiceAttachment(Attachment::parse(&link).unwrap());
        r.bridge.on_upstream(PERSON, link, T0 + 310_000);
        r.bridge.on_upstream(PERSON, answer(9, 2), T0 + 311_000);
        r.bridge.on_upstream(PERSON, answer(9, 2), T0 + 312_000);
        let got = frames(&mut live);
        assert_eq!(
            types(&got),
            ["turn:delivered", "reply"],
            "a resent answer is stored once"
        );
        let reply = &got[1];
        assert_eq!(
            (reply["seq"].as_u64(), reply["kind"].as_str()),
            (Some(1), Some("reply"))
        );
        assert_eq!(reply["turn"], json!(id(2)));
        assert_eq!(
            reply["attachments"],
            json!([{"kind": "link", "title": "The page", "url": "https://example.test/p"}])
        );
        assert_eq!(reply["about"], Value::Null);
        assert_eq!(
            reply["at"],
            T0 + 311_000,
            "the hub dates an answer Helm left undated"
        );

        // News starts with no turn. The phone acks the first reply only, so the second replays.
        let news = json!({"id": id(10), "mode": "conversation", "text": "The build finished.",
                          "sentences": [{"n": 1, "text": "The build finished."}],
                          "about": {"chat": "Build"}});
        let news = Upstream::VoiceNotify(ReplyIn::parse(&news, true).unwrap());
        r.bridge.on_upstream(PERSON, news, T0 + 320_000);
        let got = frames(&mut live);
        assert_eq!(
            (got[0]["kind"].as_str(), got[0]["seq"].as_u64()),
            (Some("notify"), Some(2))
        );
        assert_eq!(got[0]["about"], json!({"chat": "Build"}));
        r.bridge.live_ack(PERSON, PHONE, 1, T0 + 321_000);
        r.bridge.detach_live(PERSON, PHONE, g1, T0 + 322_000);
        let (_g2, mut again) = phone(&r, 0, false, T0 + 330_000);
        let replay = frames(&mut again);
        assert_eq!(types(&replay), ["welcome", "reply"]);
        assert_eq!(
            (replay[0]["seq"].as_u64(), replay[1]["seq"].as_u64()),
            (Some(2), Some(2))
        );
        // Closing the screen reached Helm as presence.
        let presence: Vec<bool> = drain_json(&mut helm)
            .iter()
            .filter(|f| f["type"] == "voice_presence")
            .map(|f| f["live"].as_bool().unwrap())
            .collect();
        assert_eq!(presence, [false]);

        // A second PC of the person comes up and is the newest: a turn the first PC has not
        // acknowledged is sent again to the first only, and its stop goes there too, while that
        // line is up. Once it drops the turn moves to the other PC.
        let laptop = r.bridge.lines.line_for(&Client {
            id: 2,
            name: "laptop".into(),
            person_id: PERSON,
            person: "p".into(),
            machine: "laptop".into(),
            scopes: Scopes::default(),
        });
        let t = T0 + 400_000;
        r.bridge
            .begin_turn(PERSON, typed(20, "Synthetic again."), t);
        assert_eq!(types(&drain_json(&mut helm)), ["voice"]);
        let (tx, mut other) = mpsc::unbounded_channel();
        let laptop_gen = laptop.attach(tx);
        drain_json(&mut other);
        r.bridge.tick(t + 46_000);
        let shown = |fs: Vec<Value>| -> Vec<String> {
            fs.iter()
                .filter(|f| f["type"] != "voice_presence")
                .map(|f| f["type"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(shown(drain_json(&mut helm)), ["voice"]);
        assert!(
            shown(drain_json(&mut other)).is_empty(),
            "the newer PC got nothing"
        );
        r.bridge
            .on_upstream(PERSON, Upstream::VoiceAck(id(20)), t + 47_000);
        assert!(r.bridge.cancel(PERSON, &id(20), t + 48_000));
        assert_eq!(shown(drain_json(&mut helm)), ["voice_stop"]);
        assert!(shown(drain_json(&mut other)).is_empty());
        r.bridge
            .begin_turn(PERSON, typed(21, "Synthetic third."), t + 49_000);
        assert_eq!(
            shown(drain_json(&mut other)),
            ["voice"],
            "new turns go to the newest"
        );
        r.bridge
            .on_upstream(PERSON, Upstream::VoiceAck(id(21)), t + 50_000);
        r.bridge
            .begin_turn(PERSON, typed(22, "Synthetic fourth."), t + 51_000);
        assert_eq!(shown(drain_json(&mut other)), ["voice"]);
        laptop.detach(laptop_gen);
        r.bridge.tick(t + 52_000);
        let moved = drain_json(&mut helm);
        assert_eq!(shown(moved.clone()), ["voice"]);
        assert_eq!(
            moved.last().unwrap()["id"],
            json!(id(22)),
            "moved once its PC dropped"
        );
    }

    #[tokio::test]
    async fn a_spoken_turn_is_transcribed_in_order_and_can_fail_or_be_cancelled() {
        let r = rig(VoiceConfig::default());
        let (_gen, mut helm) = helm_up(&r);
        let (_pg, mut live) = phone(&r, 0, true, T0);
        frames(&mut live);
        r.bridge.tick(T0);
        drain_json(&mut helm);

        // A clip and a typed turn behind it: the typed one waits until the clip is heard.
        let begin = r.bridge.begin_turn(PERSON, spoken(1), T0);
        assert_eq!(begin, Begin::New { helm_online: true });
        r.bridge
            .begin_turn(PERSON, typed(2, "Typed behind it."), T0 + 1);
        assert_eq!(types(&frames(&mut live)), ["turn:transcribing"]);
        r.bridge.tick(T0 + 2);
        assert!(
            drain_json(&mut helm).is_empty(),
            "a turn being transcribed holds back the rest"
        );

        let heard = Transcript::Text(" Synthetic question one. ".into());
        r.bridge.heard(PERSON, &id(1), heard, T0 + 3);
        let got = frames(&mut live);
        assert_eq!(types(&got), ["turn:heard"]);
        assert_eq!(got[0]["transcript"], "Synthetic question one.");
        assert_eq!(got[0]["seq"], 1);
        let down = drain_json(&mut helm);
        assert_eq!(types(&down), ["voice", "voice"]);
        assert_eq!(
            (down[0]["id"].clone(), down[0]["source"].clone()),
            (json!(id(1)), json!("speech"))
        );
        assert_eq!(
            (down[1]["id"].clone(), down[1]["source"].clone()),
            (json!(id(2)), json!("typed"))
        );
        // Helm takes both. A late result for a turn no longer being transcribed is ignored.
        for n in [1, 2] {
            r.bridge
                .on_upstream(PERSON, Upstream::VoiceAck(id(n)), T0 + 3);
        }
        assert_eq!(
            types(&frames(&mut live)),
            ["turn:delivered", "turn:delivered"]
        );
        r.bridge
            .heard(PERSON, &id(1), Transcript::Failed("late".into()), T0 + 4);
        assert!(frames(&mut live).is_empty());

        // Nothing heard, and a failure: both are kept for the phone, and a failure may be retried.
        r.bridge.begin_turn(PERSON, spoken(3), T0 + 10);
        r.bridge.begin_turn(PERSON, spoken(4), T0 + 11);
        frames(&mut live);
        r.bridge
            .heard(PERSON, &id(3), Transcript::Text("  ".into()), T0 + 12);
        r.bridge.heard(
            PERSON,
            &id(4),
            Transcript::Failed(STT_FAILED.into()),
            T0 + 13,
        );
        let got = frames(&mut live);
        assert_eq!(types(&got), ["turn:empty", "turn:failed"]);
        assert_eq!(
            (got[1]["message"].as_str(), got[1]["seq"].as_u64()),
            (Some(STT_FAILED), Some(3))
        );
        let empty_again = r.bridge.begin_turn(PERSON, spoken(3), T0 + 14);
        assert_eq!(empty_again, Begin::Already("empty"));
        let retry = r.bridge.begin_turn(PERSON, spoken(4), T0 + 15);
        assert_eq!(retry, Begin::New { helm_online: true });

        // Cancelled while transcribing, a late result is dropped and nothing reaches Helm.
        assert!(r.bridge.cancel(PERSON, &id(4), T0 + 16));
        r.bridge
            .heard(PERSON, &id(4), Transcript::Text("too late".into()), T0 + 17);
        r.bridge.tick(T0 + 18);
        assert!(drain_json(&mut helm).is_empty());
        assert!(frames(&mut live).iter().any(|f| f["state"] == "cancelled"));

        // Cancelled after Helm may have it, with or without its ack: Helm is told to stop, and
        // the turn is not sent again. Over or unknown turns are not found.
        r.bridge
            .begin_turn(PERSON, typed(5, "sent, unacked"), T0 + 20);
        assert!(r.bridge.cancel(PERSON, &id(5), T0 + 21));
        assert!(!r.bridge.cancel(PERSON, &id(5), T0 + 22), "already over");
        assert!(!r.bridge.cancel(PERSON, &id(99), T0 + 22), "unknown");
        r.bridge.begin_turn(PERSON, typed(6, "taken"), T0 + 30);
        r.bridge
            .on_upstream(PERSON, Upstream::VoiceAck(id(6)), T0 + 31);
        assert!(r.bridge.cancel(PERSON, &id(6), T0 + 32));
        r.bridge.tick(T0 + 100_000);
        let sent = drain_json(&mut helm);
        let stops: Vec<&Value> = sent
            .iter()
            .filter(|f| f["type"] == "voice_stop")
            .map(|f| &f["turn"])
            .collect();
        assert_eq!(stops, [&json!(id(5)), &json!(id(6))]);
        assert_eq!(
            sent.iter().filter(|f| f["type"] == "voice").count(),
            2,
            "5 and 6 once each"
        );

        // A turn Helm never takes expires after the queue time and the phone is told.
        frames(&mut live);
        r.bridge
            .begin_turn(PERSON, typed(7, "never taken"), T0 + 200_000);
        frames(&mut live);
        let lifetime = r.bridge.config().queue_lifetime.as_millis() as i64;
        r.bridge.tick(T0 + 200_000 + lifetime);
        let got = frames(&mut live);
        assert_eq!(types(&got), ["turn:expired"]);
        assert_eq!(got[0]["message"], EXPIRED);
        let retry = r
            .bridge
            .begin_turn(PERSON, typed(7, "try again"), T0 + 201_000 + lifetime);
        assert_eq!(retry, Begin::New { helm_online: true });
    }

    struct FakeStt(Result<String, String>);

    impl Transcribe for FakeStt {
        fn transcribe<'a>(
            &'a self,
            audio: Vec<u8>,
            name: &'a str,
            mime: &'a str,
        ) -> VoiceFuture<'a, String> {
            assert_eq!(
                (audio.as_slice(), name, mime),
                (&b"clip"[..], "turn.ogg", "audio/ogg")
            );
            Box::pin(async move { self.0.clone() })
        }
    }

    #[tokio::test]
    async fn transcription_reports_a_result_or_a_plain_failure() {
        for (stt, want, plain) in [
            (Some(Ok("Synthetic words.".to_owned())), "turn:heard", None),
            (
                Some(Err("the speech-to-text server answered HTTP 500".to_owned())),
                "turn:failed",
                Some(STT_FAILED),
            ),
            (None, "turn:failed", Some(STT_OFF)),
        ] {
            let mut r = rig(VoiceConfig::default());
            if let Some(result) = stt {
                r.bridge = r.bridge.with_stt(Arc::new(FakeStt(result)));
            }
            let (_g, mut live) = phone(&r, 0, true, T0);
            frames(&mut live);
            r.bridge.begin_turn(PERSON, spoken(1), now_ms());
            r.bridge
                .transcribe_turn(PERSON, &id(1), b"clip".to_vec(), "audio/ogg")
                .await;
            let got = frames(&mut live);
            // A transcript with Helm offline is also told "queued" after it.
            assert_eq!(
                types(&got)[..2],
                ["turn:transcribing".to_owned(), want.to_owned()]
            );
            // The server's own words stay in the log.
            assert_eq!(got[1]["message"].as_str(), plain);
        }
    }

    #[tokio::test]
    async fn the_outbox_replays_acks_and_is_capped() {
        let r = rig(VoiceConfig {
            outbox_max: 5,
            outbox_attachment_bytes: 250,
            ..VoiceConfig::default()
        });
        // Three replies with two 100-byte pictures each. The cap keeps 250 bytes, oldest first
        // out, so only the newest reply keeps its pictures.
        for reply in 1..=3u32 {
            r.bridge.on_upstream(PERSON, picture(reply, 1), T0);
            r.bridge.on_upstream(PERSON, picture(reply, 2), T0);
            r.bridge
                .on_upstream(PERSON, answer(reply, 50 + reply), T0 + i64::from(reply));
        }
        let (_g, mut live) = phone(&r, 0, false, T0 + 10);
        let got = frames(&mut live);
        assert_eq!(got[0]["seq"], 3);
        assert_eq!(got.len(), 4);
        let shape = |f: &Value| -> Vec<(bool, bool)> {
            f["attachments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| (a.get("data").is_some(), a["dropped"] == json!(true)))
                .collect()
        };
        assert_eq!(
            shape(&got[1]),
            [(false, true), (false, true)],
            "words kept, bytes gone"
        );
        assert_eq!(shape(&got[2]), [(false, true), (false, true)]);
        assert_eq!(shape(&got[3]), [(true, false), (true, false)]);
        assert_eq!(got[1]["text"], got[3]["text"]);
        assert_eq!(got[1]["attachments"][0]["title"], "A picture");

        // Beyond the entry cap the oldest entries go whole.
        for reply in 4..=9u32 {
            r.bridge.on_upstream(
                PERSON,
                answer(reply, 50 + reply),
                T0 + 20 + i64::from(reply),
            );
        }
        frames(&mut live);
        let seqs = |fs: Vec<Value>| -> Vec<u64> {
            fs.iter()
                .skip(1)
                .map(|f| f["seq"].as_u64().unwrap())
                .collect()
        };
        let (_g2, mut again) = phone(&r, 0, false, T0 + 40);
        assert_eq!(seqs(frames(&mut again)), [5, 6, 7, 8, 9]);
        // A cumulative ack frees everything up to it, and a reconnect names what it holds.
        r.bridge.live_ack(PERSON, PHONE, 7, T0 + 41);
        let (_g3, mut third) = phone(&r, 6, false, T0 + 42);
        assert_eq!(seqs(frames(&mut third)), [8, 9]);
        // A phone that remembers a seq the hub never issued (the hub restarted) gets it all.
        let (_g4, mut fourth) = phone(&r, 500, false, T0 + 43);
        let got = frames(&mut fourth);
        assert_eq!((got[0]["seq"].as_u64(), got.len()), (Some(9), 3));
        // The outbox forgets entries older than its lifetime.
        let keep = r.bridge.config().outbox_lifetime.as_millis() as i64;
        r.bridge.tick(T0 + 100 + keep);
        let (_g5, mut last) = phone(&r, 0, false, T0 + 101 + keep);
        assert_eq!(frames(&mut last).len(), 1);

        // The attachment cap is for the whole hub: another person's 200 bytes leave this person
        // 50, so their own pictures lose their bytes (and the other person's keep theirs).
        let t1 = T0 + 200 + keep;
        for (person, reply) in [(PERSON + 1, 20), (PERSON, 21)] {
            r.bridge.on_upstream(person, picture(reply, 1), t1);
            r.bridge.on_upstream(person, picture(reply, 2), t1);
            r.bridge.on_upstream(person, answer(reply, 70), t1 + 1);
        }
        let (_g6, mut sixth) = phone(&r, 0, false, t1 + 2);
        let got = frames(&mut sixth);
        assert_eq!((got.len(), &got[1]["id"]), (2, &json!(id(21))));
        assert_eq!(shape(&got[1]), [(false, true), (false, true)]);

        // After a restart the hub's seqs start from the clock, above the phone's old lastSeq: the
        // phone keeps its place and is sent every new entry.
        let mut later = rig(VoiceConfig::default());
        later.bridge = later.bridge.with_seq_base(1_000);
        later.bridge.on_upstream(PERSON, answer(1, 51), T0);
        later.bridge.on_upstream(PERSON, answer(2, 52), T0 + 1);
        let (_g7, mut back) = phone(&later, 9, false, T0 + 2);
        let got = frames(&mut back);
        assert_eq!(got[0]["seq"], 1_002, "the welcome is above the old lastSeq");
        assert_eq!(seqs(got), [1_001, 1_002]);
    }

    #[tokio::test]
    async fn attachments_wait_sixty_seconds_for_their_reply() {
        let r = rig(VoiceConfig::default());
        let (_g, mut live) = phone(&r, 0, true, T0);
        frames(&mut live);
        let note = |n: u8| {
            let v = json!({"reply": id(1), "n": n, "kind": "text",
                           "title": format!("Note {n}"), "markdown": "x"});
            Upstream::VoiceAttachment(Attachment::parse(&v).unwrap())
        };
        r.bridge.on_upstream(PERSON, note(2), T0);
        r.bridge.on_upstream(PERSON, note(1), T0 + 30_000);
        r.bridge.tick(T0 + 61_000); // the first is stale now, the second is not
        r.bridge.on_upstream(PERSON, answer(1, 5), T0 + 62_000);
        let got = frames(&mut live);
        let titles: Vec<&str> = got[0]["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, ["Note 1"]);
        // An attachment that comes after its reply has nowhere to go.
        r.bridge.on_upstream(PERSON, note(3), T0 + 63_000);
        r.bridge.on_upstream(PERSON, answer(2, 6), T0 + 64_000);
        assert_eq!(frames(&mut live)[0]["attachments"], json!([]));
    }

    #[tokio::test]
    async fn heard_reports_keep_the_latest_and_follow_the_line() {
        let r = rig(VoiceConfig::default());
        let rep = |reply: u32, state: &str, at: i64| {
            let body = json!({"reports": [
                {"reply": id(reply), "state": state, "sentence": 2, "at": at}
            ]});
            Report::parse_all(&body, 0).unwrap()
        };
        // Line down: kept, nothing forwarded. A later `all` replaces an earlier `partial`, and an
        // older report does not replace a newer one.
        assert_eq!(r.bridge.record_heard(PERSON, rep(1, "partial", 10), T0), 0);
        r.bridge.record_heard(PERSON, rep(1, "all", 20), T0);
        r.bridge.record_heard(PERSON, rep(1, "none", 5), T0);
        r.bridge.record_heard(PERSON, rep(2, "none", 6), T0);
        let (_gen, mut helm) = helm_up(&r);
        r.bridge.tick(T0 + 1);
        let sent = drain_json(&mut helm);
        assert_eq!(types(&sent), ["voice_heard"]);
        let states: Vec<(String, String)> = sent[0]["reports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                (
                    x["reply"].as_str().unwrap().to_owned(),
                    x["state"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            states,
            [(id(1), "all".to_owned()), (id(2), "none".to_owned())]
        );
        // With the line up they go at once, and only once.
        assert_eq!(
            r.bridge.record_heard(PERSON, rep(3, "partial", 30), T0 + 2),
            1
        );
        assert_eq!(drain_json(&mut helm)[0]["reports"][0]["sentence"], 2);
        r.bridge.tick(T0 + 3);
        assert!(drain_json(&mut helm).is_empty());
        // The store holds 200 at most, oldest first out; an unknown reply id is still kept.
        for n in 100..400u32 {
            r.bridge
                .record_heard(PERSON, rep(n, "all", i64::from(n)), T0 + 4);
        }
        let people = r.bridge.people.lock().unwrap();
        assert_eq!(people.get(&PERSON).unwrap().heard.len(), HEARD_MAX);
    }

    #[tokio::test]
    async fn presence_follows_a_new_line_and_a_newer_socket_replaces_the_older() {
        let r = rig(VoiceConfig::default());
        let (_g1, mut first) = phone(&r, 0, true, T0);
        frames(&mut first);
        let (g2, mut second) = phone(&r, 0, true, T0 + 1);
        assert_eq!(first.try_recv().unwrap(), Out::Close(4000, "replaced"));
        assert_eq!(types(&frames(&mut second)), ["welcome"]);

        let (gen, mut helm) = helm_up(&r);
        r.bridge.tick(T0 + 2);
        assert_eq!(types(&drain_json(&mut helm)), ["voice_presence"]);
        // The same connection is not told twice; a screen change is sent; a replaced socket's
        // change is ignored.
        r.bridge.tick(T0 + 3);
        assert!(drain_json(&mut helm).is_empty());
        r.bridge
            .live_presence(PERSON, PHONE, g2, false, Mode::Chat, T0 + 4);
        let f = drain_json(&mut helm);
        assert_eq!(
            (f[0]["live"].as_bool(), f[0]["mode"].as_str()),
            (Some(false), Some("chat"))
        );
        r.bridge
            .live_presence(PERSON, PHONE, g2 - 1, true, Mode::Conversation, T0 + 5);
        assert!(drain_json(&mut helm).is_empty());

        // The PC reconnects: the latest presence is repeated to the new line, and the phone
        // hears the line go and come back.
        r.line.detach(gen);
        r.bridge.tick(T0 + 6);
        let (_gen2, mut helm2) = helm_up(&r);
        r.bridge.tick(T0 + 7);
        let f = drain_json(&mut helm2);
        assert_eq!(types(&f), ["voice_presence"]);
        assert_eq!(f[0]["live"], false);
        let states: Vec<String> = frames(&mut second)
            .iter()
            .filter(|f| f["type"] == "helm")
            .map(|f| f["state"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(states, ["online", "offline", "online"]);

        // A fresh conversation needs a line, and one person's phone never reaches another's.
        assert!(r.bridge.fresh(PERSON));
        assert_eq!(drain_json(&mut helm2)[0]["type"], "voice_fresh");
        assert!(!r.bridge.fresh(PERSON + 1));
        assert!(!r.bridge.status(PERSON + 1).helm_online);

        // Closing the socket takes the presence with it.
        r.bridge
            .live_presence(PERSON, PHONE, g2, true, Mode::Chat, T0 + 8);
        r.bridge.detach_live(PERSON, PHONE, g2, T0 + 9);
        let live: Vec<bool> = drain_json(&mut helm2)
            .iter()
            .map(|f| f["live"].as_bool().unwrap())
            .collect();
        assert_eq!(live, [true, false]);
    }

    #[tokio::test]
    async fn two_phones_of_one_person_keep_their_own_socket_acks_and_screen() {
        let r = rig(VoiceConfig::default());
        let (_gh, mut helm) = helm_up(&r);
        let (_ga, mut a) = phone_of(&r, PHONE, 0, true, T0);
        let (gb, mut b) = phone_of(&r, PHONE + 1, 0, false, T0 + 1);
        assert_eq!(types(&frames(&mut a)), ["welcome"]);
        assert_eq!(types(&frames(&mut b)), ["welcome"]);
        let kept = a.try_recv().is_err();
        assert!(kept, "the second token did not replace the first");
        let up = drain_json(&mut helm);
        assert_eq!(types(&up), ["voice_presence"]);
        assert_eq!(up[0]["live"], true);

        // A reply reaches both. One acking it does not free it for the other.
        r.bridge.on_upstream(PERSON, answer(1, 50), T0 + 2);
        assert_eq!(types(&frames(&mut a)), ["reply"]);
        assert_eq!(types(&frames(&mut b)), ["reply"]);
        r.bridge.live_ack(PERSON, PHONE + 1, 1, T0 + 3);
        let (ga2, mut again) = phone_of(&r, PHONE, 0, true, T0 + 4);
        assert_eq!(a.try_recv().unwrap(), Out::Close(4000, "replaced"));
        assert_eq!(types(&frames(&mut again)), ["welcome", "reply"]);
        r.bridge.live_ack(PERSON, PHONE, 1, T0 + 5);
        let (_gb2, mut other) = phone_of(&r, PHONE + 1, 0, false, T0 + 6);
        assert_eq!(b.try_recv().unwrap(), Out::Close(4000, "replaced"));
        assert_eq!(types(&frames(&mut other)), ["welcome"], "both have it now");

        // Helm hears "live" while any phone is in front, with the mode of the one that moved
        // last, and "not live" only when the last one goes.
        drain_json(&mut helm);
        r.bridge
            .live_presence(PERSON, PHONE + 1, gb, true, Mode::Chat, T0 + 7);
        assert!(
            drain_json(&mut helm).is_empty(),
            "a replaced socket's change is ignored"
        );
        let (gb3, _rx) = phone_of(&r, PHONE + 1, 0, false, T0 + 8);
        r.bridge
            .live_presence(PERSON, PHONE + 1, gb3, true, Mode::Chat, T0 + 9);
        let f = drain_json(&mut helm);
        assert_eq!(
            (f[0]["live"].as_bool(), f[0]["mode"].as_str()),
            (Some(true), Some("chat"))
        );
        r.bridge.detach_live(PERSON, PHONE, ga2, T0 + 10);
        let quiet = drain_json(&mut helm);
        assert!(quiet.is_empty(), "the other phone is in front");
        r.bridge.detach_live(PERSON, PHONE + 1, gb3, T0 + 11);
        let f = drain_json(&mut helm);
        assert_eq!(
            (f[0]["live"].as_bool(), f[0]["mode"].as_str()),
            (Some(false), Some("chat"))
        );
    }
}
