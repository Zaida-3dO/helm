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
//! - **Files.** Every message that carries a file (a document of any type, a photo, a video, an
//!   animation, a video note, an audio file or a voice note) is fetched with `getFile` and goes
//!   down the line with the file attached and its caption as the text. A picture Helm can show
//!   (still PNG, JPEG or WebP, at most 20 megapixels, judged by the file's own bytes) goes as an
//!   image; anything else, including a GIF or HEIC picture, goes as a plain file under its own
//!   name, and the text says what came in (`[File] report.pdf, 1.2 MB`). The Bot API lets a bot
//!   download at most 20 MB, so a larger file gets a plain reply and is left out. Several files
//!   sent together (an album: one `media_group_id`) are gathered for [`ALBUM_WINDOW`] after the
//!   last one and go as one message; a message carries at most [`MAX_FILES`] files and
//!   [`MAX_MESSAGE_BYTES`] in all, and a file over either is left out with a reply. A message
//!   keeps its place in the queue while its files are fetched: messages sent after it wait.
//! - **Recordings.** A voice note, an audio file or a video note also goes to the speech-to-text
//!   server, when one is configured, and its transcript goes in the text beside the attached
//!   recording (`[Voice note] <transcript>`). Long recordings may take a while: the server has
//!   [`SttConfig::timeout`] to answer. A server that is down, errs or runs out of time gets a
//!   plain reply, and the recording still goes, marked as not transcribed. A video note the server
//!   cannot transcribe just goes as a file.
//! - **Quoted replies.** A reply to a message, or a quote from one, is
//!   forwarded with `Replying to: "<quoted text>"` on its own first line.
//! - **Approval buttons.** `telegram_ask` arrives as a new message with one
//!   inline button per option. A tap goes to Helm once, as `telegram_answer`.
//!   The hub checks only the Telegram side (an allowed user, a private chat, an
//!   ask that still waits); Helm checks the rest. Text never answers an ask.
//!   `telegram_ask_done` (settled in Helm) closes the message.
//!
//! The queue and the waiting asks live in memory: a container restart drops them.

use crate::images;
use crate::relay::{valid_ask_id, Line, Upstream, OFFLINE};
use base64::Engine as _;
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
/// The largest file the Bot API lets a bot download, so the largest file passed on.
pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// The most files one message carries to Helm (an album holds at most 10).
pub const MAX_FILES: usize = 10;
/// The most file bytes one message carries to Helm. As base64 it stays well inside the largest
/// frame Helm takes.
pub const MAX_MESSAGE_BYTES: usize = 50 * 1024 * 1024;
/// The most file bytes held for Helm at once (waiting to go, or sent and not yet acknowledged).
pub const MAX_QUEUED_BYTES: usize = 150 * 1024 * 1024;
/// How many files are fetched (and transcribed) at once, so the bytes held while fetching stay
/// bounded too.
pub const MAX_DOWNLOADS: usize = 2;
/// How long after the last file of an album more may join it.
pub const ALBUM_WINDOW: Duration = Duration::from_millis(1500);
/// How long a connection may take to open, and a file download from Telegram in all.
pub const STT_CONNECT: Duration = Duration::from_secs(5);
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(100);
/// How long the speech-to-text server may take to answer by default. It splits long recordings
/// into pieces and can take minutes on an hour of audio.
pub const STT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A message whose files have not all arrived this long after it came (it never should: each
/// HTTP step has its own timeout) gives up its place in the queue.
const GATHERING_MAX_MS: i64 = 75 * 60_000;
/// A quoted message is context for the reply, so it is cut to this many characters.
pub const MAX_QUOTE: usize = 300;
/// How many asks wait for a tap at once; a newer one pushes out the oldest.
pub const MAX_ASKS: usize = 50;
/// The longest button label.
pub const MAX_OPTION: usize = 40;
/// An ask's text leaves room in the message for the line that closes it.
pub const MAX_ASK_TEXT: usize = MAX_TEXT - 100;

pub const QUEUED: &str =
    "Helm is offline. Queued; it will be delivered when Helm reconnects. /cancel drops the queue.";
pub const TEXT_ONLY: &str =
    "Helm takes text, files, pictures, videos and recordings; this kind of message was not sent.";
pub const HELLO: &str =
    "Helm is listening. Messages here go to the active Telegram chat in Helm. /stop stops the \
     running turn; /cancel drops messages queued while Helm is offline.";
pub const VOICE_OFF: &str = "Voice notes are not transcribed on this hub, so the recording went \
                             to Helm without its text.";
pub const FILES_OFF: &str = "Files are off on this hub.";
/// The reply when the speech-to-text server is configured but fails, in front of the reason.
pub const STT_DOWN: &str = "Transcription is unavailable right now: ";
/// Behind the reason: what happened to the recording instead.
pub const STT_DOWN_AFTER: &str = ". The recording went to Helm without its text.";
pub const TAP_SENT: &str = "Sent to Helm";
pub const TAP_OFFLINE: &str = "Helm is offline; nothing was sent.";
pub const TAP_STALE: &str = "This request is no longer waiting.";

pub type ApiFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, ApiError>> + Send + 'a>>;

/// One Bot API method call. The real one is [`HttpApi`]; tests use a recording fake.
pub trait TelegramApi: Send + Sync {
    fn call<'a>(&'a self, method: &'a str, body: Value) -> ApiFuture<'a>;
}

/// A voice step's result; the error is a reason in plain words for the user.
pub type VoiceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// Fetching a voice note's audio and turning it into text. The real one is [`HttpVoice`]; tests
/// use a fake. No error may carry the bot token.
pub trait Voice: Send + Sync {
    /// The file that `getFile` placed at `file_path`.
    fn download<'a>(&'a self, file_path: &'a str) -> VoiceFuture<'a, Vec<u8>>;
    /// The transcript of `audio`, uploaded as the file `name` of type `mime`.
    fn transcribe<'a>(
        &'a self,
        audio: Vec<u8>,
        name: &'a str,
        mime: &'a str,
    ) -> VoiceFuture<'a, String>;
}

/// Fetching a file Telegram holds. The real one is [`HttpFiles`]; tests use a fake. No error may
/// carry the bot token.
pub trait Files: Send + Sync {
    /// The file that `getFile` placed at `file_path`.
    fn download<'a>(&'a self, file_path: &'a str) -> VoiceFuture<'a, Vec<u8>>;
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

/// The OpenAI-compatible speech-to-text server voice notes go to.
#[derive(Debug, Clone, PartialEq)]
pub struct SttConfig {
    /// The transcriptions endpoint, e.g. `http://<host>:<port>/v1/audio/transcriptions`.
    pub url: String,
    pub model: String,
    pub language: String,
    /// How long one transcription may take.
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token: Option<String>,
    pub allowed: Vec<i64>,
    pub api_base: String,
    pub queue_lifetime: Duration,
    pub queue_max: usize,
    pub receive: Receive,
    /// `None`: recordings are passed on without a transcript.
    pub stt: Option<SttConfig>,
}

impl TelegramConfig {
    /// Reads `HUB_TELEGRAM_BOT_TOKEN`, `HUB_TELEGRAM_ALLOWED_USERS` (comma-separated numeric
    /// ids), `HUB_TELEGRAM_API` (default the public Bot API), `HUB_TELEGRAM_QUEUE_MINUTES`
    /// (default 120), `HUB_TELEGRAM_QUEUE_MAX` (default 50), `HUB_TELEGRAM_RECEIVE`
    /// (`poll`, the default, or `off`) and, for voice notes, `HUB_TELEGRAM_STT_URL` (unset:
    /// off), `HUB_TELEGRAM_STT_MODEL` (default `large-v3-turbo`), `HUB_TELEGRAM_STT_LANGUAGE`
    /// (default `en`) and `HUB_TELEGRAM_STT_TIMEOUT_MINUTES` (1-60, default 10).
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
        let setting = |k: &str, default: &str| {
            var(k).map_or_else(|| default.to_owned(), |v| v.trim().to_owned())
        };
        let stt_timeout = match var("HUB_TELEGRAM_STT_TIMEOUT_MINUTES") {
            Some(m) => Duration::from_secs(
                m.trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|m| (1..=60).contains(m))
                    .ok_or("HUB_TELEGRAM_STT_TIMEOUT_MINUTES must be 1-60")?
                    * 60,
            ),
            None => STT_TIMEOUT,
        };
        let stt = match var("HUB_TELEGRAM_STT_URL").map(|u| u.trim().to_owned()) {
            Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
                Some(SttConfig {
                    url,
                    model: setting("HUB_TELEGRAM_STT_MODEL", "large-v3-turbo"),
                    language: setting("HUB_TELEGRAM_STT_LANGUAGE", "en"),
                    timeout: stt_timeout,
                })
            }
            Some(_) => return Err("HUB_TELEGRAM_STT_URL must be an http:// or https:// URL".into()),
            None => None,
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
            stt,
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
    /// `None` while its files are being fetched: it and everything after it wait.
    text: Option<String>,
    sent_at_ms: i64,
    /// The line generation it went down, while it waits for Helm's acknowledgement.
    sent_on: Option<u64>,
    /// When it last went down that line, for the resend timer.
    sent_down_ms: i64,
    /// Pictures Helm shows, and plain files, that go with it.
    images: Vec<Blob>,
    files: Vec<Blob>,
    /// While its files are being fetched.
    gather: Option<Gather>,
}

impl Queued {
    /// The bytes it holds, or has set aside for files still being fetched, for the cap on what is
    /// held.
    fn held(&self) -> usize {
        let reserved = self.gather.as_ref().map_or(0, |g| g.reserved);
        reserved + self.parts_held()
    }

    fn parts_held(&self) -> usize {
        let parts = self
            .gather
            .iter()
            .flat_map(|g| g.parts.iter().map(|(_, p)| &p.blob));
        self.images
            .iter()
            .chain(&self.files)
            .chain(parts)
            .map(|b| b.len)
            .sum()
    }
}

/// A file on its way to Helm: base64, ready for the frame. Its bytes are never printed.
#[derive(Clone)]
struct Blob {
    name: String,
    mime: String,
    data: Arc<str>,
    /// The size of the file itself, for the caps.
    len: usize,
}

impl std::fmt::Debug for Blob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Blob({} {}, {} bytes)", self.name, self.mime, self.len)
    }
}

/// What kind of file a message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Media {
    Photo,
    /// An image sent as a document.
    ImageFile,
    Document,
    Video,
    Animation,
    VideoNote,
    Audio,
    Voice,
}

impl Media {
    /// What the text calls it.
    fn label(self) -> &'static str {
        match self {
            Media::Photo => "[Photo]",
            Media::ImageFile => "[Image]",
            Media::Document => "[File]",
            Media::Video => "[Video]",
            Media::Animation => "[Animation]",
            Media::VideoNote => "[Video note]",
            Media::Audio => "[Audio]",
            Media::Voice => "[Voice note]",
        }
    }

    fn picture(self) -> bool {
        matches!(self, Media::Photo | Media::ImageFile)
    }

    /// A recording the speech-to-text server is asked for.
    fn heard(self) -> bool {
        matches!(self, Media::Voice | Media::Audio | Media::VideoNote)
    }

    fn handled(self) -> Handled {
        match self {
            Media::Photo | Media::ImageFile => Handled::Image,
            m if m.heard() => Handled::Voice,
            _ => Handled::File,
        }
    }

    /// The reply's subject when it is not passed on.
    fn noun(self) -> &'static str {
        match self {
            Media::Photo | Media::ImageFile => "That picture",
            Media::Video | Media::Animation | Media::VideoNote => "That video",
            Media::Audio | Media::Voice => "That recording",
            Media::Document => "That file",
        }
    }

    fn default_name(self) -> &'static str {
        match self {
            Media::Photo | Media::ImageFile => "photo",
            Media::Document => "file",
            Media::Video => "video.mp4",
            Media::Animation => "animation.mp4",
            Media::VideoNote => "video_note.mp4",
            Media::Audio => "audio",
            Media::Voice => "voice.ogg",
        }
    }
}

/// One file to fetch, as the message describes it.
#[derive(Debug, Clone)]
struct Job {
    media: Media,
    file_id: Option<String>,
    /// The file's own name, when it has one.
    name: Option<String>,
    mime: Option<String>,
    /// The size the message gives for it (0 when it gives none), set aside before the fetch.
    size: usize,
    /// Why it cannot be fetched at all, judged from the message alone.
    problem: Option<String>,
}

/// What the speech-to-text server made of a recording.
#[derive(Debug, Clone)]
enum Heard {
    Text(String),
    /// No server is configured.
    Off,
    /// The server is configured but failed; why, in plain words.
    Failed(String),
}

/// One fetched file.
#[derive(Debug, Clone)]
struct Part {
    media: Media,
    blob: Blob,
    /// A picture Helm shows; otherwise a plain file.
    viewable: bool,
    heard: Option<Heard>,
}

/// A message whose files are being fetched.
#[derive(Debug, Clone)]
struct Gather {
    chat: i64,
    quote: Option<String>,
    caption: String,
    /// An album's `media_group_id`: later files with it join this message.
    group: Option<String>,
    /// Fetches not yet ended.
    pending: usize,
    /// The sizes the message gives for those fetches, set aside under the caps.
    reserved: usize,
    /// More files of the album may join until then.
    open_until_ms: i64,
    next_index: usize,
    /// Fetched files, with their place in the message.
    parts: Vec<(usize, Part)>,
    /// Replies owed to the user (a file left out, a failed transcription), sent once.
    notes: Vec<String>,
}

/// An approval ask on the phone: the message that carries its buttons.
#[derive(Debug, Clone)]
struct Ask {
    id: String,
    chat: i64,
    message_id: i64,
    text: String,
    options: Vec<String>,
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
    /// Asks whose buttons still wait for a tap, oldest first.
    asks: VecDeque<Ask>,
}

/// What [`Bridge::on_update`] did, for tests and the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    /// Not a message, or a sender not on the allow-list: nothing forwarded, nothing sent.
    Ignored,
    Forwarded,
    Queued,
    Command,
    /// A recording now being fetched and transcribed; it is forwarded or queued once it has
    /// its text.
    Voice,
    /// A picture now being fetched; it is forwarded or queued once it has arrived.
    Image,
    /// Any other file now being fetched, likewise.
    File,
    /// A tap on a button, from an allowed user in a private chat.
    Button,
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
            Handled::Voice => "voice",
            Handled::Image => "image",
            Handled::File => "file",
            Handled::Button => "button",
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
    /// `None`: recordings go without a transcript.
    voice: Option<Arc<dyn Voice>>,
    /// `None`: files are fetched through `voice` alone, and with neither they are off.
    files: Option<Arc<dyn Files>>,
    state: Mutex<State>,
    /// Serialises Telegram sends, so a status edit and the final answer never interleave.
    ops: tokio::sync::Mutex<()>,
    /// Turns to fetch a file: at most [`MAX_DOWNLOADS`] at once.
    downloads: tokio::sync::Semaphore,
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

/// `s` in at most `max` characters, ending in an ellipsis when it was cut.
fn cut(s: &str, max: usize) -> String {
    if s.chars().nth(max).is_none() {
        return s.to_owned();
    }
    let end = s
        .char_indices()
        .nth(max.saturating_sub(1))
        .map_or(s.len(), |(i, _)| i);
    format!("{}…", &s[..end])
}

/// What the message replies to: the part quoted, else the whole replied-to message, flattened
/// onto one line and cut. `None`: not a reply, or a reply to a message without text.
fn quoted(msg: &Value) -> Option<String> {
    let text = |p: &str| msg.pointer(p).and_then(Value::as_str);
    let q = text("/quote/text")
        .or_else(|| text("/reply_to_message/text"))
        .or_else(|| text("/reply_to_message/caption"))?;
    let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
    (!q.is_empty()).then(|| cut(&q, MAX_QUOTE))
}

fn with_quote(quote: Option<&str>, text: &str) -> String {
    match quote {
        Some(q) => format!("Replying to: \"{q}\"\n{text}"),
        None => text.to_owned(),
    }
}

/// Why a picture was not passed on, in front of the reason.
pub const IMAGE_FAILED: &str = "That picture was not sent to Helm: ";
const TOO_LARGE: &str = "it is larger than 20 MB, the most a Telegram bot can download";

/// The file in `msg` and how to fetch it; `None` when it carries none.
fn media_of(msg: &Value) -> Option<Job> {
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let job = |v: &Value, media: Media| {
        let too_large = v
            .get("file_size")
            .and_then(Value::as_u64)
            .is_some_and(|n| n > MAX_FILE_BYTES);
        let file_id = str_of(v, "file_id");
        Job {
            media,
            problem: if too_large {
                Some(TOO_LARGE.to_owned())
            } else if file_id.is_none() {
                Some("Telegram sent no file with it".to_owned())
            } else {
                None
            },
            file_id,
            name: str_of(v, "file_name"),
            mime: str_of(v, "mime_type"),
            size: v.get("file_size").and_then(Value::as_u64).unwrap_or(0) as usize,
        }
    };
    if let Some(sizes) = msg.get("photo").and_then(Value::as_array) {
        // Telegram lists a photo at several sizes: take the largest that Helm can show, else the
        // largest that can be downloaded (it then goes as a plain file).
        let num = |p: &Value, k: &str| p.get(k).and_then(Value::as_u64);
        let area = |p: &Value| num(p, "width").unwrap_or(0) * num(p, "height").unwrap_or(0);
        let small = |p: &&Value| num(p, "file_size").is_none_or(|n| n <= MAX_FILE_BYTES);
        let mut sizes: Vec<&Value> = sizes.iter().collect();
        sizes.sort_by_key(|p| std::cmp::Reverse(area(p)));
        let best = (sizes.iter().copied())
            .filter(small)
            .find(|p| area(p) <= images::MAX_PIXELS)
            .or_else(|| sizes.iter().copied().find(small));
        let mut j = job(
            best.or(sizes.first().copied()).unwrap_or(&Value::Null),
            Media::Photo,
        );
        if best.is_none() {
            j.problem = Some(TOO_LARGE.to_owned());
        }
        return Some(j);
    }
    // An animation also comes as a document, so it is looked at first.
    for (key, media) in [
        ("animation", Media::Animation),
        ("video", Media::Video),
        ("video_note", Media::VideoNote),
        ("audio", Media::Audio),
        ("voice", Media::Voice),
    ] {
        if let Some(v) = msg.get(key) {
            return Some(job(v, media));
        }
    }
    let doc = msg.get("document")?;
    let mime = doc.get("mime_type").and_then(Value::as_str).unwrap_or("");
    Some(job(
        doc,
        if mime.starts_with("image/") {
            Media::ImageFile
        } else {
            Media::Document
        },
    ))
}

/// The picture type of `bytes` when Helm can show it (its own bytes decide, never what Telegram
/// or the sender says it is), else why not.
fn viewable(bytes: &[u8]) -> Result<images::Kind, String> {
    let kind = match images::sniff(bytes) {
        Some(images::Kind::Gif) => return Err("it is a GIF".into()),
        Some(k) => k,
        None => return Err("it is not a PNG, JPEG or WebP picture".into()),
    };
    let (w, h) = images::dimensions(kind, bytes).ok_or("it is not a readable picture")?;
    if u64::from(w) * u64::from(h) > images::MAX_PIXELS {
        return Err("it is larger than 20 megapixels".into());
    }
    if images::is_animated(kind, bytes) {
        return Err("it is animated".into());
    }
    Ok(kind)
}

/// A file's name as it is passed on: no control characters, at most 120 characters. Helm makes
/// it safe for its own disk.
fn clean_name(name: &str) -> Option<String> {
    let n: String = name.chars().filter(|c| !c.is_control()).take(120).collect();
    (!n.trim().is_empty()).then(|| n.trim().to_owned())
}

/// A size the way a person reads it: `512 bytes`, `12 KB`, `1.2 MB`.
pub fn size_text(n: usize) -> String {
    const KB: f64 = 1024.0;
    match n as f64 {
        b if b < KB => format!("{n} bytes"),
        b if b < KB * KB => format!("{:.0} KB", (b / KB).max(1.0)),
        b => format!("{:.1} MB", b / KB / KB),
    }
}

/// The text of a message whose files have all arrived: its caption, a line per plain file or
/// recording, and what its pictures are when there is no caption.
fn compose(g: &Gather, parts: &[Part]) -> String {
    let mut lines: Vec<String> = vec![];
    let pictures: Vec<&Part> = parts.iter().filter(|p| p.viewable).collect();
    if !g.caption.is_empty() {
        lines.push(g.caption.clone());
    } else if let [one] = pictures.as_slice() {
        lines.push(one.media.label().to_owned());
    } else if !pictures.is_empty() {
        lines.push(format!("[{} pictures]", pictures.len()));
    }
    for p in parts.iter().filter(|p| !p.viewable) {
        // A picture Helm cannot show is a plain file like any other.
        let label = if p.media.picture() {
            "[File]"
        } else {
            p.media.label()
        };
        let head = match p.media {
            Media::Voice => label.to_owned(),
            _ => format!("{label} {}, {}", p.blob.name, size_text(p.blob.len)),
        };
        let unheard = "(not transcribed: transcription was unavailable)";
        lines.push(match (&p.heard, p.media) {
            (Some(Heard::Text(t)), _) if t.is_empty() => format!("{head} (no speech was heard)"),
            (Some(Heard::Text(t)), Media::Voice) => format!("{head} {t}"),
            (Some(Heard::Text(t)), _) => format!("{head}\nTranscript: {t}"),
            (Some(Heard::Off), Media::Voice) => {
                format!("{head} (not transcribed: this hub has no speech-to-text server)")
            }
            (Some(Heard::Failed(_)), Media::Voice | Media::Audio) => format!("{head} {unheard}"),
            _ => head,
        });
    }
    with_quote(g.quote.as_deref(), &lines.join("\n"))
}

/// The reply when a file would take its message past [`MAX_MESSAGE_BYTES`].
fn message_full_note(name: &str) -> String {
    format!(
        "{name} was left out: a message to Helm carries at most {} of files.",
        size_text(MAX_MESSAGE_BYTES)
    )
}

/// The reply when a file would take what waits for Helm past [`MAX_QUEUED_BYTES`].
fn queue_full_note(name: &str) -> String {
    format!(
        "{name} was left out: too many files are already waiting to reach Helm. Try again in a \
         moment."
    )
}

/// The reply when a file is not passed on.
fn not_sent(job: &Job, why: &str) -> String {
    match job.name.as_deref().and_then(clean_name) {
        _ if job.media.picture() => format!("{IMAGE_FAILED}{why}"),
        Some(n) => format!("{n} was not sent to Helm: {why}"),
        None => format!("{} was not sent to Helm: {why}", job.media.noun()),
    }
}

/// A button's callback data, `a:<ask id>:<index>`.
fn parse_button(data: &str) -> Option<(&str, usize)> {
    let (id, index) = data.strip_prefix("a:")?.split_once(':')?;
    let index = index.parse::<usize>().ok()?;
    valid_ask_id(id).then_some((id, index))
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
            voice: None,
            files: None,
            state: Mutex::new(State::default()),
            ops: tokio::sync::Mutex::new(()),
            downloads: tokio::sync::Semaphore::new(MAX_DOWNLOADS),
        }
    }

    /// Turn voice notes on, transcribed by `voice`.
    pub fn with_voice(mut self, voice: Arc<dyn Voice>) -> Self {
        self.voice = Some(voice);
        self
    }

    /// Turn pictures on, fetched with `files`.
    pub fn with_files(mut self, files: Arc<dyn Files>) -> Self {
        self.files = Some(files);
        self
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
    pub async fn on_update(self: &Arc<Self>, update: &Value, now: i64) -> Handled {
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
        if let Some(q) = update.get("callback_query") {
            return self.on_button(q).await;
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
        let quote = quoted(msg);
        let Some(text) = msg.get("text").and_then(Value::as_str) else {
            if let Some(job) = media_of(msg) {
                return self.on_media(msg, job, chat, quote, now).await;
            }
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
        let text = with_quote(quote.as_deref(), text);
        let Some(id) = self.push(msg, Some(text), now) else {
            self.reply_full(chat).await;
            return Handled::Queued;
        };
        self.deliver(&id, chat, now).await
    }

    /// Queue one message in order; `None` when the queue of waiting messages is full.
    fn push(&self, msg: &Value, text: Option<String>, now: i64) -> Option<String> {
        let mut st = self.st();
        if st.queue.iter().filter(|q| q.sent_on.is_none()).count() >= self.queue_max {
            return None;
        }
        let id = uuid::Uuid::new_v4().to_string();
        st.queue.push_back(Queued {
            id: id.clone(),
            text,
            sent_at_ms: msg
                .get("date")
                .and_then(Value::as_i64)
                .map(|s| s * 1000)
                .unwrap_or(now),
            sent_on: None,
            sent_down_ms: now,
            images: vec![],
            files: vec![],
            gather: None,
        });
        Some(id)
    }

    async fn reply_full(&self, chat: i64) {
        self.reply(
            chat,
            "Helm is offline and the queue is full, so this message was not kept.",
        )
        .await;
    }

    /// Send what may go, and say whether message `id` went with it. A message that stays behind
    /// while the line is up waits for a voice note before it, so only a line that is down earns
    /// the "offline" reply.
    async fn deliver(&self, id: &str, chat: i64, now: i64) -> Handled {
        if self.flush_sent(now).await.iter().any(|s| s == id) {
            return Handled::Forwarded;
        }
        if !self.line.is_up() {
            self.reply(chat, QUEUED).await;
        }
        Handled::Queued
    }

    /// A message with a file: hold its place in the queue (or join the album it belongs to) and
    /// fetch the file beside the poll loop.
    async fn on_media(
        self: &Arc<Self>,
        msg: &Value,
        job: Job,
        chat: i64,
        quote: Option<String>,
        now: i64,
    ) -> Handled {
        if self.files.is_none() && self.voice.is_none() {
            self.reply(chat, FILES_OFF).await;
            return Handled::Command;
        }
        if let Some(why) = &job.problem {
            self.reply(chat, &not_sent(&job, why)).await;
            return Handled::Command;
        }
        // The caption is the message; `/helm` on it routes it as it does on text.
        let caption = msg.get("caption").and_then(Value::as_str).unwrap_or("");
        let caption = strip_helm(caption).unwrap_or(caption).trim().to_owned();
        let group = msg
            .get("media_group_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let window = ALBUM_WINDOW.as_millis() as i64;
        let label = job.name.as_deref().and_then(clean_name);
        let label = label.as_deref().unwrap_or(job.media.noun());
        // The sizes the message gives are judged before anything is downloaded, so what is held
        // stays under the caps while files are fetched; what arrives is checked again.
        let queue_full = {
            let st = self.st();
            st.queue.iter().map(Queued::held).sum::<usize>() + job.size > MAX_QUEUED_BYTES
        };
        if queue_full {
            self.reply(chat, &queue_full_note(label)).await;
            return Handled::Command;
        }
        // A later file of an album joins the message its first file started, while that message
        // still gathers its files.
        let joined = group.as_ref().and_then(|group| {
            let mut st = self.st();
            let q = st.queue.iter_mut().find(|q| {
                q.gather
                    .as_ref()
                    .is_some_and(|g| g.group.as_ref() == Some(group))
            })?;
            let id = q.id.clone();
            let mine = q.held();
            let g = q.gather.as_mut()?;
            if g.pending + g.parts.len() >= MAX_FILES {
                return Some(Err(format!(
                    "{label} was left out: a message to Helm carries at most {MAX_FILES} files."
                )));
            }
            if mine + job.size > MAX_MESSAGE_BYTES {
                return Some(Err(message_full_note(label)));
            }
            g.pending += 1;
            g.reserved += job.size;
            g.open_until_ms = now + window;
            g.next_index += 1;
            if g.caption.is_empty() {
                g.caption = caption.clone();
            }
            Some(Ok((id, g.next_index - 1)))
        });
        let (slot, index) = match joined {
            Some(Ok(at)) => at,
            Some(Err(said)) => {
                self.reply(chat, &said).await;
                return Handled::Command;
            }
            None => {
                let Some(slot) = self.push(msg, None, now) else {
                    self.reply_full(chat).await;
                    return Handled::Queued;
                };
                let mut st = self.st();
                if let Some(q) = st.queue.iter_mut().find(|q| q.id == slot) {
                    q.gather = Some(Gather {
                        chat,
                        quote,
                        caption,
                        open_until_ms: if group.is_some() { now + window } else { 0 },
                        group,
                        pending: 1,
                        reserved: job.size,
                        next_index: 1,
                        parts: vec![],
                        notes: vec![],
                    });
                }
                (slot, 0)
            }
        };
        let me = self.clone();
        let media = job.media;
        tokio::spawn(async move {
            let got = me.fetch_part(&job).await;
            me.part_done(&slot, index, &job, got).await;
        });
        media.handled()
    }

    /// The file at `file_path`, from whichever fetcher is on.
    async fn download(&self, file_path: &str) -> Result<Vec<u8>, String> {
        match (&self.files, &self.voice) {
            (Some(f), _) => f.download(file_path).await,
            (None, Some(v)) => v.download(file_path).await,
            (None, None) => Err("files are off on this hub".into()),
        }
    }

    /// `getFile`, the download, then a look at the bytes (is it a picture Helm shows?) and, for a
    /// recording, the speech-to-text server.
    async fn fetch_part(&self, job: &Job) -> Result<Part, String> {
        // A few at a time, so only those few files' bytes are in memory while they are fetched.
        let _turn = self
            .downloads
            .acquire()
            .await
            .map_err(|_| "the hub is shutting down".to_owned())?;
        let file_id = job
            .file_id
            .as_deref()
            .ok_or("Telegram sent no file with it")?;
        let file = self
            .api
            .call("getFile", json!({ "file_id": file_id }))
            .await
            .map_err(|e| format!("Telegram did not hand over the file ({})", e.description))?;
        if file
            .get("file_size")
            .and_then(Value::as_u64)
            .is_some_and(|n| n > MAX_FILE_BYTES)
        {
            return Err(TOO_LARGE.into());
        }
        let path = file
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or("Telegram gave no download path for it")?;
        let bytes = self.download(path).await?;
        let own = job.name.as_deref().and_then(clean_name);
        let from_path = path.rsplit('/').next().and_then(clean_name);
        // Telegram sends every photo as a JPEG and names no type for it.
        let mime = job.mime.clone().unwrap_or_else(|| {
            let jpeg = job.media == Media::Photo;
            if jpeg {
                "image/jpeg"
            } else {
                "application/octet-stream"
            }
            .into()
        });
        let heard = match (job.media.heard(), &self.voice) {
            (false, _) => None,
            (true, None) => Some(Heard::Off),
            (true, Some(voice)) => {
                let name = own.clone().or(from_path.clone());
                let name = name.unwrap_or_else(|| job.media.default_name().to_owned());
                Some(match voice.transcribe(bytes.clone(), &name, &mime).await {
                    Ok(t) => Heard::Text(t.trim().to_owned()),
                    Err(why) => Heard::Failed(why),
                })
            }
        };
        let picture = job.media.picture();
        let (kind, data, len) = blocking(move || {
            let kind = if picture { viewable(&bytes).ok() } else { None };
            let data: Arc<str> = base64::engine::general_purpose::STANDARD
                .encode(&bytes)
                .into();
            Ok((kind, data, bytes.len()))
        })
        .await?;
        let name = match kind {
            // A picture shows under the file's own name, else a plain one.
            Some(k) => own.unwrap_or_else(|| format!("photo.{}", k.extension())),
            None => own
                .or(from_path)
                .unwrap_or_else(|| job.media.default_name().to_owned()),
        };
        Ok(Part {
            media: job.media,
            viewable: kind.is_some(),
            blob: Blob {
                name,
                mime: kind.map_or(mime, |k| k.mime().to_owned()),
                data,
                len,
            },
            heard,
        })
    }

    /// One file of the message held at `slot` has been fetched (or not): keep it, within the caps,
    /// note what the user must be told, and send what may go. A message dropped meanwhile with
    /// /cancel is not brought back.
    async fn part_done(&self, slot: &str, index: usize, job: &Job, got: Result<Part, String>) {
        {
            let mut st = self.st();
            // What is held besides this file, whose share set aside is given back now.
            let held = st.queue.iter().map(Queued::held).sum::<usize>();
            let held = held.saturating_sub(job.size);
            let Some(g) = st
                .queue
                .iter_mut()
                .find(|q| q.id == slot)
                .and_then(|q| q.gather.as_mut())
            else {
                return;
            };
            g.pending = g.pending.saturating_sub(1);
            g.reserved = g.reserved.saturating_sub(job.size);
            match got {
                Err(why) => g.notes.push(not_sent(job, &why)),
                Ok(part) => {
                    let name = part.blob.name.clone();
                    let mine: usize = g.parts.iter().map(|(_, p)| p.blob.len).sum();
                    let mine = mine + g.reserved;
                    if mine + part.blob.len > MAX_MESSAGE_BYTES {
                        g.notes.push(message_full_note(&name));
                    } else if held + part.blob.len > MAX_QUEUED_BYTES {
                        g.notes.push(queue_full_note(&name));
                    } else {
                        match (&part.heard, part.media) {
                            (Some(Heard::Off), Media::Voice) => g.notes.push(VOICE_OFF.into()),
                            (Some(Heard::Failed(why)), Media::Voice | Media::Audio) => {
                                g.notes.push(format!("{STT_DOWN}{why}{STT_DOWN_AFTER}"))
                            }
                            _ => {}
                        }
                        g.parts.push((index, part));
                    }
                }
            }
        }
        self.flush(now_ms()).await;
    }

    /// Expire old messages, take back unacknowledged ones from a line that went away, and send
    /// what waits down the current line, in order, along with any message that line has not
    /// acknowledged within [`RESEND_AFTER`]. Returns how many were sent.
    pub async fn flush(&self, now: i64) -> usize {
        self.flush_sent(now).await.len()
    }

    /// [`Bridge::flush`], returning the ids of the messages sent.
    async fn flush_sent(&self, now: i64) -> Vec<String> {
        let generation = self.line.generation();
        let resend_ms = RESEND_AFTER.as_millis() as i64;
        let (frames, expired, undelivered, finished) = {
            let mut st = self.st();
            for q in st.queue.iter_mut() {
                if q.sent_on.is_some() && q.sent_on != generation {
                    q.sent_on = None; // that line is gone; resend on the next
                }
            }
            // A message whose files have all arrived (and whose album has had its window) gets its
            // text; one left with no file at all goes, its replies saying why.
            let mut finished = vec![];
            for q in st.queue.iter_mut() {
                let Some(g) = q
                    .gather
                    .take_if(|g| g.pending == 0 && now >= g.open_until_ms)
                else {
                    continue;
                };
                let mut parts = g.parts.clone();
                parts.sort_by_key(|(i, _)| *i);
                let parts: Vec<Part> = parts.into_iter().map(|(_, p)| p).collect();
                let kept = !parts.is_empty();
                if kept {
                    q.text = Some(compose(&g, &parts));
                    for p in parts {
                        if p.viewable {
                            q.images.push(p.blob);
                        } else {
                            q.files.push(p.blob);
                        }
                    }
                }
                finished.push((q.id.clone(), g.chat, g.notes, kept));
            }
            st.queue
                .retain(|q| q.text.is_some() || q.gather.is_some() || q.sent_on.is_some());
            // Past the lifetime a message goes, waiting or unacknowledged alike: one Helm keeps
            // refusing would otherwise be resent for ever. A message whose files are still
            // being fetched keeps its place for far longer than its HTTP timeouts allow, but not
            // for ever.
            let lifetime = self.lifetime_ms;
            let (mut expired, mut undelivered) = (0, 0);
            st.queue.retain(|q| {
                let keep = match q.text {
                    None => now - q.sent_down_ms <= GATHERING_MAX_MS,
                    Some(_) => now - q.sent_at_ms <= lifetime,
                };
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
                    // Order is kept: nothing overtakes a message still gathering its files.
                    let Some(text) = &q.text else { break };
                    let age = (now - q.sent_at_ms) / 60_000;
                    let mut frame = json!({
                        "type": "telegram",
                        "id": q.id,
                        "text": text,
                        "sentAt": q.sent_at_ms,
                        "ageMinutes": if age >= 1 { json!(age) } else { Value::Null },
                        "bot": bot,
                    });
                    let list = |blobs: &[Blob]| -> Value {
                        blobs
                            .iter()
                            .map(|b| json!({ "name": b.name, "mediaType": b.mime, "data": &*b.data }))
                            .collect()
                    };
                    if !q.images.is_empty() {
                        frame["images"] = list(&q.images);
                    }
                    if !q.files.is_empty() {
                        frame["files"] = list(&q.files);
                    }
                    frames.push((frame, q.id.clone()));
                    q.sent_on = Some(gen);
                    q.sent_down_ms = now;
                }
            }
            (frames, expired, undelivered, finished)
        };
        let mut sent = vec![];
        for (frame, id) in frames {
            if self.line.send(&frame) {
                sent.push(id);
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
        // What the user is owed for a message that just got its files: why any were left out,
        // and the usual reply when it cannot go because Helm is offline.
        for (id, chat, notes, kept) in finished {
            if !notes.is_empty() {
                self.reply(chat, &notes.join("\n")).await;
            }
            if kept && !sent.contains(&id) && !self.line.is_up() {
                self.reply(chat, QUEUED).await;
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

    /// An approval ask: a new (notifying) message with one button per option, one per row. An id
    /// already on the phone is not sent twice.
    pub async fn on_ask(&self, id: &str, text: &str, options: &[String]) {
        let _g = self.ops.lock().await;
        if self.st().asks.iter().any(|a| a.id == id) {
            return;
        }
        let chat = self.reply_chat();
        let text = cut(text.trim(), MAX_ASK_TEXT);
        let options: Vec<String> = options
            .iter()
            .take(4)
            .map(|o| cut(o.trim(), MAX_OPTION))
            .collect();
        let rows: Vec<Value> = options
            .iter()
            .enumerate()
            .map(|(i, o)| json!([{ "text": o, "callback_data": format!("a:{id}:{i}") }]))
            .collect();
        let r = self
            .api
            .call(
                "sendMessage",
                json!({ "chat_id": chat, "text": text, "disable_notification": false,
                        "reply_markup": { "inline_keyboard": rows } }),
            )
            .await;
        match r.map(|v| v.get("message_id").and_then(Value::as_i64)) {
            Ok(Some(message_id)) => {
                let mut st = self.st();
                if st.asks.len() >= MAX_ASKS {
                    st.asks.pop_front();
                }
                st.asks.push_back(Ask {
                    id: id.to_owned(),
                    chat,
                    message_id,
                    text,
                    options,
                });
            }
            Ok(None) => eprintln!("telegram: the ask was sent but no message id came back"),
            Err(e) => eprintln!("telegram: sending an ask failed: {}", e.description),
        }
    }

    /// The ask was settled in Helm: its message gains `text` and loses its buttons.
    pub async fn on_ask_done(&self, id: &str, text: &str) {
        let _g = self.ops.lock().await;
        let ask = {
            let mut st = self.st();
            let i = st.asks.iter().position(|a| a.id == id);
            i.and_then(|i| st.asks.remove(i))
        };
        if let Some(ask) = ask {
            let text = format!("{}\n\n{}", ask.text, text.trim());
            self.edit_closed(&ask, &text).await;
        }
    }

    /// Edit an ask's message to `text`. An edit that carries no keyboard removes the buttons.
    async fn edit_closed(&self, ask: &Ask, text: &str) {
        let body = json!({ "chat_id": ask.chat, "message_id": ask.message_id,
                           "text": cut(text, MAX_TEXT) });
        if let Err(e) = self.api.call("editMessageText", body).await {
            eprintln!("telegram: closing an ask failed: {}", e.description);
        }
    }

    async fn answer_query(&self, query: &str, text: Option<&str>) {
        let mut body = json!({ "callback_query_id": query });
        if let Some(t) = text {
            body["text"] = json!(t);
        }
        if let Err(e) = self.api.call("answerCallbackQuery", body).await {
            eprintln!("telegram: answering a tap failed: {}", e.description);
        }
    }

    /// A tap on a button. Only an allowed user in a private chat gets any answer; a known ask
    /// goes to Helm once, as `telegram_answer`.
    async fn on_button(&self, q: &Value) -> Handled {
        let from = q.pointer("/from/id").and_then(Value::as_i64);
        if !from.is_some_and(|f| self.allowed.contains(&f)) {
            return Handled::Ignored;
        }
        let chat = q.pointer("/message/chat/id").and_then(Value::as_i64);
        let message_id = q.pointer("/message/message_id").and_then(Value::as_i64);
        let query = q.get("id").and_then(Value::as_str);
        let (Some(chat), Some(message_id), Some(query)) = (chat, message_id, query) else {
            return Handled::Ignored;
        };
        if q.pointer("/message/chat/type").and_then(Value::as_str) != Some("private") {
            return Handled::Ignored;
        }
        let _g = self.ops.lock().await;
        let Some((id, index)) = q.get("data").and_then(Value::as_str).and_then(parse_button) else {
            self.answer_query(query, None).await;
            return Handled::Button;
        };
        let ask = {
            let st = self.st();
            st.asks
                .iter()
                .find(|a| a.id == id && a.chat == chat && a.message_id == message_id)
                .cloned()
        };
        let Some(ask) = ask else {
            self.answer_query(query, Some(TAP_STALE)).await;
            let body = json!({ "chat_id": chat, "message_id": message_id });
            if let Err(e) = self.api.call("editMessageReplyMarkup", body).await {
                eprintln!("telegram: removing stale buttons failed: {}", e.description);
            }
            return Handled::Button;
        };
        let Some(label) = ask.options.get(index) else {
            self.answer_query(query, None).await;
            return Handled::Button;
        };
        if !self
            .line
            .send(&json!({ "type": "telegram_answer", "id": id, "index": index }))
        {
            // Nothing reached Helm, so the buttons stay for another try.
            self.answer_query(query, Some(TAP_OFFLINE)).await;
            return Handled::Button;
        }
        self.st().asks.retain(|a| a.id != id);
        self.answer_query(query, Some(TAP_SENT)).await;
        self.edit_closed(&ask, &format!("{}\n\n→ {label}", ask.text))
            .await;
        Handled::Button
    }

    /// Handle one frame Helm sent up the line.
    pub async fn on_upstream(&self, u: Upstream) {
        match u {
            Upstream::TelegramAck(id) => self.on_ack(&id),
            Upstream::TelegramStatus(t) => self.on_status(&t),
            Upstream::TelegramFinal(t) => self.on_final(&t).await,
            Upstream::TelegramNotify(t) => self.on_notify(&t).await,
            Upstream::TelegramAsk { id, text, options } => self.on_ask(&id, &text, &options).await,
            Upstream::TelegramAskDone { id, text } => self.on_ask_done(&id, &text).await,
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
                    json!({ "offset": offset, "timeout": POLL_SECONDS,
                            "allowed_updates": ["message", "callback_query"] }),
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

/// Errors are reported without the URL, which carries the bot token.
fn scrub(text: String, token: &str) -> String {
    text.replace(token, "<token>")
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
        let mut resp = agent.post(url).send_json(body).map_err(|e| ApiError {
            code: None,
            description: scrub(format!("request failed: {e}"), token),
        })?;
        let v: Value = resp.body_mut().read_json().map_err(|e| ApiError {
            code: None,
            description: scrub(format!("unreadable reply: {e}"), token),
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
                    token,
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

/// Voice notes over HTTP: the file from the Bot API's file host, the transcript from an
/// OpenAI-compatible speech-to-text server (a multipart form with `file`, `model`, `language`
/// and `response_format=text`; no auth).
pub struct HttpVoice {
    files: HttpFiles,
    stt: SttConfig,
    /// The speech-to-text server's own, with its longer timeout.
    agent: ureq::Agent,
}

impl HttpVoice {
    pub fn new(api_base: &str, token: &str, stt: &SttConfig) -> Self {
        HttpVoice {
            files: HttpFiles::new(api_base, token),
            stt: stt.clone(),
            agent: agent_with(stt.timeout),
        }
    }
}

/// An HTTP agent that answers an error status as a response and gives up after `timeout`.
fn agent_with(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(STT_CONNECT))
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Files over HTTP, from the Bot API's file host.
pub struct HttpFiles {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl HttpFiles {
    pub fn new(api_base: &str, token: &str) -> Self {
        HttpFiles {
            base: api_base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            agent: agent_with(DOWNLOAD_TIMEOUT),
        }
    }
}

impl Files for HttpFiles {
    fn download<'a>(&'a self, file_path: &'a str) -> VoiceFuture<'a, Vec<u8>> {
        fetch(&self.agent, &self.base, &self.token, file_path)
    }
}

/// A file from the Bot API's file host, at most [`MAX_FILE_BYTES`]. No error carries the token.
fn fetch<'a>(
    agent: &ureq::Agent,
    base: &str,
    token: &str,
    file_path: &str,
) -> VoiceFuture<'a, Vec<u8>> {
    let url = format!("{base}/file/bot{token}/{file_path}");
    let (agent, token) = (agent.clone(), token.to_owned());
    Box::pin(blocking(move || {
        let failed = |e: ureq::Error| match e {
            ureq::Error::BodyExceedsLimit(_) => TOO_LARGE.to_owned(),
            e => scrub(format!("the download from Telegram failed: {e}"), &token),
        };
        let mut resp = agent.get(&url).call().map_err(failed)?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(format!("the download from Telegram failed (HTTP {status})"));
        }
        resp.body_mut()
            .with_config()
            .limit(MAX_FILE_BYTES)
            .read_to_vec()
            .map_err(failed)
    }))
}

/// Run a blocking HTTP exchange on the blocking pool.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(format!("the call did not finish: {e}")))
}

/// A `multipart/form-data` body: the file part, then the plain fields.
fn multipart(boundary: &str, file: (&str, &str, &[u8]), fields: &[(&str, &str)]) -> Vec<u8> {
    let (name, mime, bytes) = file;
    let name = name.replace(['"', '\\', '\r', '\n'], "_");
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: {mime}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    for (k, v) in fields {
        body.extend_from_slice(
            format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}")
                .as_bytes(),
        );
    }
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Why the speech-to-text server gave no answer, in plain words: out of time is said as such.
fn stt_failed(e: ureq::Error, minutes: u64) -> String {
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    match e {
        ureq::Error::Timeout(t)
            if !matches!(t, ureq::Timeout::Connect | ureq::Timeout::Resolve) =>
        {
            format!("the speech-to-text server did not finish within {minutes} {unit}")
        }
        e => format!("the speech-to-text server could not be reached: {e}"),
    }
}

/// The speech-to-text server's answer as a transcript, or why it is not one. The transcript is
/// passed on as the server wrote it, with any markers it puts in (a gap it skipped, a cut).
fn stt_reply(status: u16, body: &str) -> Result<String, String> {
    let body = body.trim();
    let server = "the speech-to-text server";
    if status != 200 {
        return Err(match body {
            "" => format!("{server} answered HTTP {status}"),
            b => format!("{server} answered HTTP {status}: {}", cut(b, 300)),
        });
    }
    if body.starts_with('{') {
        if body.contains("\"error\"") {
            return Err(format!("{server} reported an error: {}", cut(body, 300)));
        }
        // A server that ignored `response_format=text` answers JSON with the text in it.
        if let Some(t) = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v.get("text")?.as_str().map(str::to_owned))
        {
            return Ok(t.trim().to_owned());
        }
    }
    match body {
        "" => Err(format!("{server} returned no text")),
        b => Ok(b.to_owned()),
    }
}

impl Voice for HttpVoice {
    fn download<'a>(&'a self, file_path: &'a str) -> VoiceFuture<'a, Vec<u8>> {
        self.files.download(file_path)
    }

    fn transcribe<'a>(
        &'a self,
        audio: Vec<u8>,
        name: &'a str,
        mime: &'a str,
    ) -> VoiceFuture<'a, String> {
        let boundary = format!("helm-hub-{}", uuid::Uuid::new_v4().simple());
        let body = multipart(
            &boundary,
            (name, mime, audio.as_slice()),
            &[
                ("model", self.stt.model.as_str()),
                ("language", self.stt.language.as_str()),
                ("response_format", "text"),
            ],
        );
        let (agent, url) = (self.agent.clone(), self.stt.url.clone());
        let minutes = self.stt.timeout.as_secs().div_ceil(60);
        Box::pin(blocking(move || {
            let mut resp = agent
                .post(&url)
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .send(&body[..])
                .map_err(|e| stt_failed(e, minutes))?;
            let status = resp.status().as_u16();
            let text = resp
                .body_mut()
                .with_config()
                .limit(1024 * 1024)
                .lossy_utf8(true)
                .read_to_string()
                .map_err(|e| match e {
                    e @ ureq::Error::Timeout(_) => stt_failed(e, minutes),
                    e => format!("the speech-to-text server's answer was unreadable: {e}"),
                })?;
            stt_reply(status, &text)
        }))
    }
}
