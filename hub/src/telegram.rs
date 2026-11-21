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
//! - **One bot, several people.** Each person gets a [`Bridge`] of their own: their messages go
//!   to that person's own Telegram user only, and down the line [`Lines::route_for`] picks for
//!   them (the PC they chose with `/pc`, else the one that took their last message, else their
//!   own PCs). [`Telegram`] hands each update to the bridge that allows its sender.
//! - **Who is allowed, and who they are.** The allow-list is the hub's `telegram_users` table:
//!   every Telegram id linked to a person. It starts from `HUB_TELEGRAM_ALLOWED_USERS`
//!   (`<telegram id>:<person>`; an id with no person belongs to the hub's legacy person), which
//!   stays valid and wins for the ids it lists, and grows when a person links their own account
//!   from Helm (below). A person with no bridge yet gets one the moment their first account is
//!   linked.
//! - **Linking a person's own account.** Helm asks for a link (`tg_link`, see `relay.rs`); the
//!   hub answers a `t.me/<bot>?start=<code>` address and the person taps it once on their phone.
//!   The code — one use, [`LINK_CODE_MINUTES`] minutes, the pairing code's alphabet — is what
//!   proves which Telegram account is the person's: without it anyone at any PC could point a
//!   person's replies at a stranger's Telegram. A wrong, used or expired code gets the same
//!   silence as any stranger; a right one binds the account, moves it off whoever had it, and
//!   answers "Linked to Helm as <name>." `tg_unlink` removes a link again, `tg_status` says
//!   whether there is one.
//! - **Which PC receives a person.** `/pc` lists the PCs that can take their Telegram and marks
//!   the one that does; `/pc <name>` pins that PC, `/pc auto` leaves it to the hub. The pin is
//!   the hub's to keep (see `Lines::pin`), and survives a restart.
//! - **Deny by default.** A message whose sender is not on the allow-list is
//!   dropped: nothing is forwarded to Helm and nothing is sent back, so the bot
//!   does not even confirm it exists. The one exception is a private `/start <code>` whose code
//!   is a live link code: that is how an account joins the allow-list.
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
//! - **Helm offline.** What a queued message is told depends on what the hub can do about the PC
//!   (see `launcher.rs`): a PC whose launcher is up is asked to start Helm by itself, a PC the hub
//!   can wake gets a Wake button (a tap, never automatic), and otherwise the sender is given the one
//!   setting that would let the hub start Helm. `/starthelm` and `/wake` do the same on request.
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
//! - **Long messages arrive whole.** Telegram splits a long message (at 4,096 UTF-16 units) into
//!   several that reach the hub in a burst. A text message of [`LONG_PART`] units or more is
//!   therefore held for [`JOIN_WINDOW`]: a text message that follows it, while it is still the
//!   last in the queue, joins it with nothing put between the parts, and the window starts again
//!   while the parts keep being that long. A part shorter than that ends the burst and the joined
//!   message goes at once. An ordinary message is never held. Only text joins; a command, a
//!   picture or a recording is handled as before, and anything after a held message waits behind
//!   it, in order. A burst gathers at most [`JOIN_LIMIT`] units, and a joined message of more than
//!   [`MAX_JOINED`] units goes as a `message.txt` attachment instead of as text. Each person's bot
//!   has its own queue, so a burst joins only within one person's messages.
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

use crate::auth::digest;
use crate::heal::{HELD_AUDIO_MAX, HELD_MAX_MS, VOICE_HELD};
use crate::images;
use crate::launcher::{LaunchOutcome, Plan, Reason, WakeOutcome};
use crate::lines::{Lines, PersonRoute, PersonUpstream};
use crate::registry::{new_code, normalize_code};
use crate::relay::{valid_ask_id, Channel, Line, Route, Upstream, OFFLINE};
use crate::stt::{
    agent_with, blocking, retry_transcribing, Stt, SttHealed, SttHealth, Transcribe,
    STT_RETRY_AFTER,
};
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

pub use crate::stt::{SttConfig, SttError, SttFuture, VoiceFuture, STT_CONNECT, STT_TIMEOUT};

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
/// The most memory the files held for Helm may take at once (waiting to go, or sent and not yet
/// acknowledged), counted as base64, which is what is held. One budget for the whole household:
/// every person's bot counts what the others hold.
pub const MAX_QUEUED_BYTES: usize = 150 * 1024 * 1024;
/// A flush sends frames until this much base64 of files has gone down the line, then leaves the
/// rest for the next tick, so a reconnect never builds every queued frame at once.
pub const FLUSH_BYTES: usize = 64 * 1024 * 1024;
/// How many files are fetched (and transcribed) at once, so the bytes held while fetching stay
/// bounded too.
pub const MAX_DOWNLOADS: usize = 2;
/// How long after the last file of an album more may join it.
pub const ALBUM_WINDOW: Duration = Duration::from_millis(1500);
/// A text message this long (in UTF-16 units, which is how Telegram counts) is probably the first
/// part of a split one: Telegram cuts at 4,096.
pub const LONG_PART: usize = 3500;
/// How long after a long part more text may join it.
pub const JOIN_WINDOW: Duration = Duration::from_millis(2000);
/// The longest text that goes to Helm as text, in UTF-16 units. A joined message above it goes as
/// a `message.txt` attachment.
pub const MAX_JOINED: usize = 100_000;
/// The most a burst gathers (UTF-16 units); text past it starts a message of its own. Kept low
/// enough that the attachment, as base64, stays inside the largest frame Helm takes.
pub const JOIN_LIMIT: usize = 300_000;
/// How long a file download from Telegram may take in all.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(100);
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
/// How long a link code works: long enough to tap the link a person was sent, short enough that a
/// forgotten one is not a standing invitation.
pub const LINK_CODE_MINUTES: i64 = 15;

/// A message queued while Helm is offline and the hub cannot start it, so its sender is given
/// the one step that changes that.
pub const QUEUED: &str = "Helm is offline and the hub can't start it. Your message is queued and \
     goes when Helm is open. To let the hub start Helm for you next time, turn on \"Start Helm \
     when a message arrives\" in Helm's Settings → Hub. /cancel drops the queue.";
pub const TEXT_ONLY: &str =
    "Helm takes text, files, pictures, videos and recordings; this kind of message was not sent.";
pub const HELLO: &str =
    "Helm is listening. Messages here go to the active Telegram chat in Helm. /stop stops the \
     running turn; /cancel drops messages queued while Helm is offline. When Helm is offline \
     /starthelm starts it on your PC and /wake wakes the PC, where the hub can.";
pub const VOICE_OFF: &str = "Voice notes are not transcribed on this hub, so the recording went \
                             to Helm without its text.";
pub const FILES_OFF: &str = "Files are off on this hub.";
/// The reply when the speech-to-text server is configured but fails, in front of the reason.
pub const STT_DOWN: &str = "Transcription is unavailable right now: ";
/// Behind the reason: what happened to the recording instead.
pub const STT_DOWN_AFTER: &str = ". The recording went to Helm without its text.";
/// How long a transcription runs before the chat is told it is still going.
pub const SLOW_AFTER: Duration = Duration::from_secs(30);
/// Sent once to a chat whose voice note is still being transcribed; removed when it is done.
pub const STILL_TRANSCRIBING: &str = "Still transcribing your voice note…";
pub const TAP_SENT: &str = "Sent to Helm";
pub const TAP_OFFLINE: &str = "Helm is offline; nothing was sent.";
pub const TAP_STALE: &str = "This request is no longer waiting.";
/// The `/pc` answers.
pub const PC_NONE: &str = "No PC can take your Telegram right now. Open Helm on the PC you want \
                           and turn on \"Take my Telegram on this PC\" in its settings.";
/// What `/starthelm` answers when no launcher is up on any of the person's PCs.
pub const START_NONE: &str = "The hub can't start Helm: no PC of yours has its launcher \
     connected. Open Helm on the PC once and turn on \"Start Helm when a message arrives\" in \
     Settings → Hub.";
/// What `/wake` answers when waking is not set up on the hub.
pub const WAKE_OFF: &str = "Waking a PC isn't set up on this hub.";
/// What `/wake` answers when waking is on but no PC has said how it is woken.
pub const WAKE_NONE: &str = "The hub doesn't know how to wake any of your PCs yet. It learns \
     that from a PC whose \"Start Helm when a message arrives\" is on, once that PC has been \
     connected.";
pub const PC_CURRENT: &str = "Your Telegram goes to: ";
pub const PC_PINNED: &str = " (pinned by you)";
pub const PC_AUTO: &str = "Other PCs that could take it: ";
pub const PC_SET: &str = "Your Telegram now goes to ";
pub const PC_CLEARED: &str = "Your Telegram goes to whichever PC Helm picks.";
pub const PC_UNKNOWN: &str = "No PC by that name can take your Telegram. /pc lists the ones \
                              that can.";
pub const PC_ROUTING_OFF: &str = "This hub cannot choose a PC for your Telegram.";
/// What a person is told once their own account is bound to them.
pub const LINKED: &str = "Linked to Helm as ";
/// Why a link question cannot be answered here at all.
pub const NO_BOT: &str = "this hub has no Telegram bot";

/// The answer to a `tg_link` / `tg_unlink` / `tg_status` from a hub that has no bot. It carries
/// the id the question was asked with and the matching result type, so the profile can say why
/// rather than wait for its own timeout.
pub fn no_bot_answer(u: &Upstream) -> Value {
    match u {
        Upstream::TgAdmin { id, op, .. } => json!({
            "type": format!("tg_{op}_result"), "id": id.clone(), "ok": false, "error": NO_BOT,
        }),
        _ => json!({}),
    }
}

pub type ApiFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, ApiError>> + Send + 'a>>;

/// One Bot API method call. The real one is [`HttpApi`]; tests use a recording fake.
pub trait TelegramApi: Send + Sync {
    fn call<'a>(&'a self, method: &'a str, body: Value) -> ApiFuture<'a>;
}

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
    ) -> SttFuture<'a, String>;
    /// Where the transcription calls go, for the log line. `""` when there is no real server.
    fn endpoint(&self) -> &str {
        ""
    }
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

#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token: Option<String>,
    /// Every allowed Telegram user id, named or not.
    pub allowed: Vec<i64>,
    /// The allowed ids that name a person: `(telegram id, person)`. The rest are the owner's.
    pub people: Vec<(i64, String)>,
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
        let (allowed, people) =
            parse_allowed_people(&var("HUB_TELEGRAM_ALLOWED_USERS").unwrap_or_default())?;
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
            people,
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
    parse_allowed_people(spec).map(|(ids, _)| ids)
}

/// Telegram user ids, and the ones that name a person.
pub type AllowedPeople = (Vec<i64>, Vec<(i64, String)>);

/// `id[:person],...`: every id, and the ones that name a person.
pub fn parse_allowed_people(spec: &str) -> Result<AllowedPeople, String> {
    const BAD: &str = "HUB_TELEGRAM_ALLOWED_USERS must be numeric user ids, each optionally \
        followed by :person";
    let (mut ids, mut people) = (Vec::new(), Vec::new());
    for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (id, person) = match part.split_once(':') {
            Some((id, person)) => (id.trim(), Some(person.trim())),
            None => (part, None),
        };
        let id = id
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| BAD.to_owned())?;
        if ids.contains(&id) {
            return Err(format!("Telegram user {id} is listed twice"));
        }
        ids.push(id);
        if let Some(p) = person {
            if p.is_empty() || p.chars().count() > 64 || p.chars().any(char::is_control) {
                return Err(BAD.to_owned());
            }
            people.push((id, p.to_owned()));
        }
    }
    Ok((ids, people))
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
    /// A long text held a moment for the rest of a split message; it becomes `text` then.
    hold: Option<Hold>,
}

/// The text of a message that may be the first part of a long one.
#[derive(Debug, Clone)]
struct Hold {
    text: String,
    /// Its length in UTF-16 units.
    units: usize,
    /// More text may join until then.
    open_until_ms: i64,
}

impl Queued {
    /// Every file it holds, fetched so far.
    fn blobs(&self) -> impl Iterator<Item = &Blob> {
        let parts = self
            .gather
            .iter()
            .flat_map(|g| g.parts.iter().map(|(_, p)| &p.blob));
        self.images.iter().chain(&self.files).chain(parts)
    }

    /// The bytes of files it carries, or has set aside for files still being fetched: what the cap
    /// of [`MAX_MESSAGE_BYTES`] counts.
    fn file_bytes(&self) -> usize {
        let reserved = self.gather.as_ref().map_or(0, |g| g.reserved);
        reserved + self.blobs().map(|b| b.len).sum::<usize>()
    }

    /// The memory its files take, which is their base64 (a third more than the files), counting
    /// what is set aside for files still being fetched the same way: what the cap of
    /// [`MAX_QUEUED_BYTES`] counts.
    fn held(&self) -> usize {
        let reserved = self.gather.as_ref().map_or(0, |g| g.reserved);
        b64_len(reserved) + self.blobs().map(|b| b.data.len()).sum::<usize>()
    }

    /// The base64 of the files that go down the line with it.
    fn data_bytes(&self) -> usize {
        self.images
            .iter()
            .chain(&self.files)
            .map(|b| b.data.len())
            .sum()
    }
}

/// The base64 size of `raw` bytes, which is what the queue really holds for a file.
fn b64_len(raw: usize) -> usize {
    raw.div_ceil(3) * 4
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

/// How one fetch ended.
enum Fetched {
    Part(Part),
    /// A recording whose transcription failed with passing trouble; the bridge holds it and its
    /// message waits, until [`Bridge::stt_back`] finishes it off.
    Held,
    /// The fetch itself failed, in plain words.
    Failed(String),
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
    /// The machine that asked, and the connection it asked on: the tap goes back to that
    /// machine, since it is the one holding the card, whoever else carries the person's Telegram.
    origin: Option<(i64, u64)>,
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
    line: Arc<dyn Route>,
    /// The Telegram ids this bridge answers, which are the accounts linked to this bridge's
    /// person. Grows when another of their accounts is linked.
    allowed: Mutex<Vec<i64>>,
    /// The hub person this bridge speaks for, and the lines to route them. `None` for a bridge
    /// made without the household (a test, a single-machine hub): its frames name no person and
    /// `/pc` is not offered.
    person: Option<i64>,
    household_lines: Option<Arc<Lines>>,
    lifetime_ms: i64,
    queue_max: usize,
    receive: Receive,
    /// `None`: recordings go without a transcript.
    voice: Option<Arc<dyn Voice>>,
    /// The speech server's health, so a failure can say since when it has been down.
    stt_health: Option<Arc<SttHealth>>,
    /// A failing transcription is tried once more after this (normally [`STT_RETRY_AFTER`]).
    stt_retry_after: Duration,
    /// `None`: files are fetched through `voice` alone, and with neither they are off.
    files: Option<Arc<dyn Files>>,
    state: Mutex<State>,
    /// Serialises Telegram sends, so a status edit and the final answer never interleave.
    ops: tokio::sync::Mutex<()>,
    /// Turns to fetch a file: at most [`MAX_DOWNLOADS`] at once. A turn is given back once the
    /// file is downloaded, so a long transcription never holds up the downloads behind it.
    downloads: tokio::sync::Semaphore,
    /// One recording is transcribed at a time, as the server's graphics memory allows.
    transcribing: tokio::sync::Semaphore,
    /// How long a transcription runs before the chat is told it is still going.
    slow_after: Duration,
    /// Every other bridge of this bot, so the files they hold share one budget. Grows when a
    /// link makes another bridge.
    siblings: Arc<Mutex<Vec<Weak<Bridge>>>>,
    /// Recordings whose transcription failed with passing trouble; they wait for the server to
    /// come back, and their message waits with them ([`Bridge::stt_back`]).
    stt_held: Mutex<VecDeque<HeldPart>>,
    stt_held_bytes: AtomicUsize,
    /// The bridge's own handle, set once it is shared, so a retry can spawn its transcription.
    me: OnceLock<Weak<Bridge>>,
}

/// One recording waiting out a speech-server outage: everything needed to finish its message
/// later, exactly as the failed fetch left it.
struct HeldPart {
    at: i64,
    slot: String,
    index: usize,
    chat: i64,
    job: Job,
    audio: Vec<u8>,
    name: String,
    mime: String,
    /// The failure it is waiting out, kept for the words it is owed if the wait ends badly.
    why: SttError,
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
pub(crate) fn cut(s: &str, max: usize) -> String {
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

/// A held text, ready to go: as the message's text, or, past [`MAX_JOINED`] UTF-16 units, as the
/// attached `message.txt` with a line saying so.
fn release(q: &mut Queued, h: Hold) {
    if h.units <= MAX_JOINED {
        q.text = Some(h.text);
        return;
    }
    let len = h.text.len();
    let data: Arc<str> = base64::engine::general_purpose::STANDARD
        .encode(h.text.as_bytes())
        .into();
    q.files.push(Blob {
        name: "message.txt".to_owned(),
        mime: "text/plain".to_owned(),
        data,
        len,
    });
    q.text = Some(format!(
        "[Long message] The whole text is in the attached message.txt ({}): it was too long to \
         send as text.",
        size_text(len)
    ));
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

/// A PC button's callback data: `pc:start:<client id>` or `pc:wake:<client id>`.
fn parse_pc_button(data: &str) -> Option<(PcAction, i64)> {
    let (action, client) = data.strip_prefix("pc:")?.split_once(':')?;
    let action = match action {
        "start" => PcAction::Start,
        "wake" => PcAction::Wake,
        _ => return None,
    };
    Some((action, client.parse::<i64>().ok().filter(|c| *c > 0)?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PcAction {
    Start,
    Wake,
}

/// What a person is told about their PC when a message cannot go because Helm is offline.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Told {
    /// Helm is being started on this machine.
    Starting(String),
    /// Helm is running on this machine but has not connected to the hub yet.
    Connecting(String),
    /// This machine looks off or asleep.
    Asleep(String),
    /// A launch for this machine never led to a connection.
    DidntStart(String),
    /// The hub can do nothing about it.
    Unknown,
}

/// The sentence about the PC that goes in front of what the person is told about their message.
fn pc_note(told: &Told) -> Option<String> {
    match told {
        Told::Starting(m) => Some(format!("Helm is starting on {m}.")),
        Told::Connecting(m) => Some(format!(
            "Helm is running on {m} but isn't connected to the hub yet; it reconnects by itself."
        )),
        Told::Asleep(m) => Some(format!("{m} looks off or asleep.")),
        Told::DidntStart(m) => Some(format!("Helm didn't start on {m}.")),
        Told::Unknown => None,
    }
}

/// The reply to a message that waits for Helm.
fn queued_text(told: &Told) -> String {
    let note = pc_note(told).unwrap_or_default();
    match told {
        Told::Starting(_) => {
            format!("{note} Your message is queued and goes as soon as it's up. /cancel drops the queue.")
        }
        Told::Connecting(_) | Told::Asleep(_) | Told::DidntStart(_) => {
            format!("{note} Your message is queued. /cancel drops the queue.")
        }
        Told::Unknown => QUEUED.to_owned(),
    }
}

/// The reply to a message that could not even be queued.
fn full_text(told: &Told) -> String {
    let note = pc_note(told).map(|n| format!(" {n}")).unwrap_or_default();
    format!(
        "Helm is offline and the queue is full, so this message was not kept.{note} Send it \
         again once Helm is back."
    )
}

/// What the person is told, and the buttons under it as `(label, callback data)`.
struct Offline {
    told: Told,
    buttons: Vec<(String, String)>,
}

/// `text` with the reason a launch failed, when the launcher gave one.
fn with_reason(text: &str, error: Option<&str>) -> String {
    match error {
        Some(e) => format!(
            "{}: {}.",
            text.trim_end_matches('.'),
            e.trim().trim_end_matches('.')
        ),
        None => text.to_owned(),
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
        Self::with_route(api, line, cfg)
    }

    /// A bridge whose frames go down `line`: one line, or all of a person's machines.
    pub fn with_route(
        api: Arc<dyn TelegramApi>,
        line: Arc<dyn Route>,
        cfg: &TelegramConfig,
    ) -> Self {
        Bridge {
            api,
            line,
            allowed: Mutex::new(cfg.allowed.clone()),
            person: None,
            household_lines: None,
            lifetime_ms: cfg.queue_lifetime.as_millis() as i64,
            queue_max: cfg.queue_max,
            receive: cfg.receive,
            voice: None,
            stt_health: None,
            stt_retry_after: STT_RETRY_AFTER,
            files: None,
            state: Mutex::new(State::default()),
            ops: tokio::sync::Mutex::new(()),
            downloads: tokio::sync::Semaphore::new(MAX_DOWNLOADS),
            transcribing: tokio::sync::Semaphore::new(1),
            slow_after: SLOW_AFTER,
            siblings: Arc::new(Mutex::new(Vec::new())),
            stt_held: Mutex::new(VecDeque::new()),
            stt_held_bytes: AtomicUsize::new(0),
            me: OnceLock::new(),
        }
    }

    /// Remember the bridge's own shared handle so held recordings can be retried by themselves.
    /// Call it on the `Arc` the bot runs the bridge from.
    pub fn register_self(self: &Arc<Self>) {
        let _ = self.me.set(Arc::downgrade(self));
    }

    fn strong(&self) -> Option<Arc<Bridge>> {
        self.me.get().and_then(|w| w.upgrade())
    }

    /// Say who this bridge speaks for, so its frames name that person and `/pc` can route them.
    pub fn with_person(mut self, person: i64, lines: Arc<Lines>) -> Self {
        self.person = Some(person);
        self.household_lines = Some(lines);
        self
    }

    /// The person this bridge speaks for, when it knows.
    pub fn person_id(&self) -> Option<i64> {
        self.person
    }

    /// Whether Telegram account `id` is one of this bridge's people's accounts.
    pub fn allows(&self, id: i64) -> bool {
        self.allowed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&id)
    }

    /// Let this bridge answer another of its person's accounts.
    pub fn allow(&self, id: i64) {
        let mut allowed = self.allowed.lock().unwrap_or_else(|p| p.into_inner());
        if !allowed.contains(&id) {
            allowed.push(id);
        }
    }

    /// Stop answering an account that has been unlinked (or moved to another person). It loses
    /// everything the bridge owed that chat: its place as the chat answers and alerts go to, the
    /// one live status message, and any ask buttons still waiting for a tap. Until this was done,
    /// an unlinked phone kept receiving that person's answers.
    pub fn unallow(&self, id: i64) {
        self.allowed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|a| *a != id);
        let mut st = self.st();
        if st.reply_chat == Some(id) {
            st.reply_chat = None;
            st.status_id = None;
            st.status_text = None;
            st.pending_status = None;
        }
        st.asks.retain(|ask| ask.chat != id);
    }

    /// The connection a frame came up on, as `(client, generation)`, for an answer that must go
    /// back to the machine that asked.
    fn origin(&self, client: i64) -> Option<(i64, u64)> {
        match &self.household_lines {
            Some(lines) => lines
                .get(client)
                .and_then(|line| line.generation().map(|gen| (client, gen))),
            None => self.line.generation().map(|gen| (0, gen)),
        }
    }

    /// The line of the connection `generation` on `client`, while that very connection is up.
    fn origin_line(&self, client: i64, generation: u64) -> Option<Arc<Line>> {
        self.household_lines
            .as_ref()?
            .get(client)
            .filter(|line| line.generation() == Some(generation))
    }

    /// `personId` for a frame this bridge sends down the line.
    fn tag(&self, frame: &mut Value) {
        if let Some(p) = self.person {
            frame["personId"] = json!(p.to_string());
        }
    }

    /// What the other people's bridges hold of [`MAX_QUEUED_BYTES`]. Each is locked on its own,
    /// never together with this one's state.
    fn held_elsewhere(&self) -> usize {
        self.siblings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|b| !std::ptr::eq(b.as_ref(), self))
            .map(|b| b.st().queue.iter().map(Queued::held).sum::<usize>())
            .sum()
    }

    /// Say "still transcribing" after `wait` (instead of [`SLOW_AFTER`]).
    pub fn with_slow_after(mut self, wait: Duration) -> Self {
        self.slow_after = wait;
        self
    }

    /// Turn voice notes on, transcribed by `voice`.
    pub fn with_voice(mut self, voice: Arc<dyn Voice>) -> Self {
        self.voice = Some(voice);
        self
    }

    /// Keep the speech server's health, so a failure can say since when it has been down.
    pub fn with_stt_health(mut self, health: Arc<SttHealth>) -> Self {
        self.stt_health = Some(health);
        self
    }

    /// Retry a failing transcription after `wait` (instead of [`STT_RETRY_AFTER`]).
    pub fn with_stt_retry(mut self, wait: Duration) -> Self {
        self.stt_retry_after = wait;
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
    /// private chat (whose id is the user id). `None` only for a bridge whose person has no
    /// linked account at all, which has nobody to answer.
    fn reply_chat(&self) -> Option<i64> {
        self.st().reply_chat.or_else(|| {
            self.allowed
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .first()
                .copied()
        })
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
        if !from.is_some_and(|f| self.allows(f)) {
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
            Some("/starthelm") => {
                self.on_start_helm(chat, now).await;
                return Handled::Command;
            }
            Some("/wake") => {
                self.on_wake(chat, now).await;
                return Handled::Command;
            }
            Some("/pc") => {
                let rest = text
                    .split_once(char::is_whitespace)
                    .map(|(_, r)| r.trim().to_owned());
                self.on_pc(chat, rest.as_deref()).await;
                return Handled::Command;
            }
            Some("/stop") => {
                let mut frame = json!({ "type": "telegram_stop" });
                self.tag(&mut frame);
                let sent = self.line.send(&frame);
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
        let units = text.encode_utf16().count();
        let long = units >= LONG_PART;
        let window = JOIN_WINDOW.as_millis() as i64;
        // The next part of a long message joins the one held for it, with nothing between, while
        // that one is still the last in the queue.
        let joined = {
            let mut st = self.st();
            st.queue
                .back_mut()
                .and_then(|q| {
                    let id = q.id.clone();
                    q.hold.as_mut().map(|h| (id, h))
                })
                .filter(|(_, h)| now < h.open_until_ms && h.units + units <= JOIN_LIMIT)
                .map(|(id, h)| {
                    h.text.push_str(text);
                    h.units += units;
                    // Parts that keep being this long keep the burst open; a shorter one ends it.
                    h.open_until_ms = if long { now + window } else { now };
                    id
                })
        };
        if let Some(id) = joined {
            // The sender was told about the queue by the first part; no more replies.
            return if self.flush_sent(now).await.contains(&id) {
                Handled::Forwarded
            } else {
                Handled::Queued
            };
        }
        let text = with_quote(quote.as_deref(), text);
        let units = text.encode_utf16().count();
        let Some(id) = self.push(msg, Some(text), now) else {
            self.reply_full(chat).await;
            return Handled::Queued;
        };
        if long {
            // Hold it for the rest of the burst: it goes when the window ends or a short part
            // joins.
            let mut st = self.st();
            if let Some(q) = st.queue.iter_mut().find(|q| q.id == id) {
                q.hold = q.text.take().map(|text| Hold {
                    text,
                    units,
                    open_until_ms: now + window,
                });
            }
        }
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
            hold: None,
        });
        Some(id)
    }

    async fn reply_full(&self, chat: i64) {
        let offline = self.offline_status(now_ms());
        self.reply_buttons(chat, &full_text(&offline.told), &offline.buttons)
            .await;
    }

    /// `/pc`: the PCs that can take this person's Telegram and the one that does. `/pc <name>`
    /// pins one, `/pc auto` hands the choice back to the hub.
    async fn on_pc(&self, chat: i64, arg: Option<&str>) {
        let (Some(person), Some(lines)) = (self.person, self.household_lines.as_ref()) else {
            self.reply(chat, PC_ROUTING_OFF).await;
            return;
        };
        let candidates = lines.candidate_lines(person, Channel::Telegram);
        if let Some(name) = arg.filter(|a| !a.is_empty()) {
            if name.eq_ignore_ascii_case("auto") {
                lines.clear_pin(person, Channel::Telegram);
                self.reply(chat, PC_CLEARED).await;
                return;
            }
            // The person's own machine wins when two PCs carry the same name.
            match lines.find_by_name(person, Channel::Telegram, name) {
                Some((client_id, machine, _)) => {
                    if lines.pin(person, Channel::Telegram, client_id) {
                        self.reply(chat, &format!("{PC_SET}{machine}.")).await;
                    } else {
                        self.reply(chat, PC_ROUTING_OFF).await;
                    }
                }
                None => self.reply(chat, PC_UNKNOWN).await,
            }
            return;
        }
        if candidates.is_empty() {
            // `/starthelm` only where a launcher is connected to take it.
            let hint = match lines.pc_plan(person, Channel::Telegram, now_ms()) {
                Plan::Launch { machine, .. } => format!(" /starthelm starts Helm on {machine}."),
                Plan::Asleep { machine, .. } => format!(" /wake wakes {machine}."),
                Plan::Starting { .. } | Plan::Connecting { .. } | Plan::Unknown => String::new(),
            };
            self.reply(chat, &format!("{PC_NONE}{hint}")).await;
            return;
        }
        let current = lines.route_for(person, Channel::Telegram);
        let mut text = String::from(PC_CURRENT);
        let mut others: Vec<String> = Vec::new();
        for (client_id, machine, line) in &candidates {
            let here = current.as_ref().is_some_and(|c| Arc::ptr_eq(c, line));
            if here {
                text.push_str(machine);
                if lines.registry().lock().is_ok_and(|r| {
                    r.pinned_client(person, "telegram").ok().flatten() == Some(*client_id)
                }) {
                    text.push_str(PC_PINNED);
                }
                text.push('.');
            } else {
                others.push(machine.clone());
            }
        }
        if !others.is_empty() {
            text.push_str(&format!(" {PC_AUTO}{}.", others.join(", ")));
        }
        text.push_str(" /pc <name> chooses one, /pc auto lets Helm pick.");
        self.reply(chat, &text).await;
    }

    // ----- Helm offline: what the hub can do about the PC -----

    /// This person's machines and the household's lines, when the bridge knows them.
    fn household(&self) -> Option<(i64, &Arc<Lines>)> {
        Some((self.person?, self.household_lines.as_ref()?))
    }

    /// Where a message for an offline Helm stands, doing what the hub may do by itself: a PC
    /// whose launcher is up is asked to start Helm (at most one launch a machine every two
    /// minutes). Waking is never done here: it is a person's button.
    fn offline_status(&self, now: i64) -> Offline {
        let unknown = Offline {
            told: Told::Unknown,
            buttons: Vec::new(),
        };
        let Some((person, lines)) = self.household() else {
            return unknown;
        };
        match lines.pc_plan(person, Channel::Telegram, now) {
            Plan::Unknown => unknown,
            Plan::Starting { machine, .. } => Offline {
                told: Told::Starting(machine),
                buttons: Vec::new(),
            },
            Plan::Connecting { machine, .. } => Offline {
                told: Told::Connecting(machine),
                buttons: Vec::new(),
            },
            Plan::Asleep { client_id, machine } => Offline {
                buttons: vec![(format!("Wake {machine}"), format!("pc:wake:{client_id}"))],
                told: Told::Asleep(machine),
            },
            Plan::Launch { client_id, machine } => {
                let launched = lines.launchers().try_launch(
                    client_id,
                    Reason::Telegram,
                    Some(person),
                    now,
                    false,
                );
                match launched {
                    // A launch a moment ago is still the answer.
                    LaunchOutcome::Sent | LaunchOutcome::RateLimited => Offline {
                        told: Told::Starting(machine),
                        buttons: Vec::new(),
                    },
                    LaunchOutcome::Already => Offline {
                        told: Told::Connecting(machine),
                        buttons: Vec::new(),
                    },
                    LaunchOutcome::GaveUp => Offline {
                        buttons: vec![("Try again".to_owned(), format!("pc:start:{client_id}"))],
                        told: Told::DidntStart(machine),
                    },
                    LaunchOutcome::NoLauncher => unknown,
                }
            }
        }
    }

    /// Answer a message that waits for Helm, in the way the PC allows.
    async fn tell_offline(&self, chat: i64, now: i64) {
        let offline = self.offline_status(now);
        self.reply_buttons(chat, &queued_text(&offline.told), &offline.buttons)
            .await;
    }

    /// The toast for a tap on an ask while Helm is offline.
    fn tap_offline_text(&self, now: i64) -> String {
        match self.offline_status(now).told {
            Told::Starting(m) => {
                format!("{TAP_OFFLINE} Helm is starting on {m}; tap again in a minute.")
            }
            Told::Connecting(m) => {
                format!("{TAP_OFFLINE} Helm is running on {m} but isn't connected yet; tap again in a minute.")
            }
            Told::Asleep(m) => format!("{TAP_OFFLINE} {m} looks off or asleep."),
            Told::DidntStart(m) => format!("{TAP_OFFLINE} Helm didn't start on {m}."),
            Told::Unknown => TAP_OFFLINE.to_owned(),
        }
    }

    /// A message with buttons under it: one button a row.
    async fn reply_buttons(&self, chat: i64, text: &str, buttons: &[(String, String)]) {
        if buttons.is_empty() {
            self.reply(chat, text).await;
            return;
        }
        let rows: Vec<Value> = buttons
            .iter()
            .map(|(label, data)| json!([{ "text": cut(label, MAX_OPTION), "callback_data": data }]))
            .collect();
        let _g = self.ops.lock().await;
        let sent = self
            .api
            .call(
                "sendMessage",
                json!({ "chat_id": chat, "text": text, "disable_notification": false,
                        "reply_markup": { "inline_keyboard": rows } }),
            )
            .await;
        if let Err(e) = sent {
            eprintln!("telegram: reply failed: {}", e.description);
        }
    }

    /// Once a tick: tell the person about a launch that never came up, and start Helm for
    /// messages that waited for a launcher to connect.
    async fn tick_pc(&self, now: i64) {
        let Some((person, lines)) = self.household() else {
            return;
        };
        // A launch that never came up is not worth a word while another of the person's PCs
        // takes their messages.
        let served =
            self.line.is_up() || !lines.candidate_lines(person, Channel::Telegram).is_empty();
        for late in lines.launchers().timeouts(person, now) {
            if served {
                continue;
            }
            let machines = lines.pc_machines(person, Channel::Telegram);
            let here = machines.iter().find(|m| m.client_id == late.client_id);
            let mut buttons = vec![(
                "Try again".to_owned(),
                format!("pc:start:{}", late.client_id),
            )];
            if here.is_some_and(|m| m.launcher.is_none() && m.wake_ready) {
                buttons.push((
                    format!("Wake {}", late.machine),
                    format!("pc:wake:{}", late.client_id),
                ));
            }
            let text = with_reason(
                &format!("Helm didn't start on {}.", late.machine),
                late.error.as_deref(),
            );
            if let Some(chat) = self.reply_chat() {
                self.reply_buttons(chat, &text, &buttons).await;
            }
        }
        // A launcher that came up after a message was queued starts Helm for it.
        if self.line.is_up()
            || !self
                .st()
                .queue
                .iter()
                .any(|q| q.sent_on.is_none() && q.text.is_some())
        {
            return;
        }
        if let Plan::Launch { client_id, machine } = lines.pc_plan(person, Channel::Telegram, now) {
            let launched =
                lines
                    .launchers()
                    .try_launch(client_id, Reason::Telegram, Some(person), now, false);
            if launched == LaunchOutcome::Sent {
                if let Some(chat) = self.reply_chat() {
                    self.reply(chat, &queued_text(&Told::Starting(machine)))
                        .await;
                }
            }
        }
    }

    /// Ask the PC's launcher to start Helm because the person asked. Returns what to tell them
    /// and whether a launch went.
    fn start_sentence(
        &self,
        lines: &Lines,
        person: i64,
        client_id: i64,
        machine: &str,
        now: i64,
    ) -> (String, bool) {
        match lines
            .launchers()
            .try_launch(client_id, Reason::Button, Some(person), now, true)
        {
            LaunchOutcome::Sent => (
                format!(
                    "Helm is starting on {machine}. Your messages are queued and go as soon as \
                     it's up."
                ),
                true,
            ),
            LaunchOutcome::Already => (
                format!("Helm is already running on {machine}; it reconnects by itself."),
                false,
            ),
            LaunchOutcome::RateLimited => (
                format!("Helm was asked to start on {machine} a moment ago. Give it a minute."),
                false,
            ),
            LaunchOutcome::NoLauncher | LaunchOutcome::GaveUp => (
                format!("{machine}'s launcher isn't connected, so the hub can't start Helm there."),
                false,
            ),
        }
    }

    /// Wake the person's PC because they asked. Returns what to tell them and whether it went.
    async fn wake_sentence(
        &self,
        lines: &Lines,
        client_id: i64,
        machine: &str,
        now: i64,
    ) -> (String, bool) {
        match lines.launchers().wake(client_id, now).await {
            WakeOutcome::Sent => (
                format!(
                    "Sent a wake signal to {machine}. Your messages stay queued and go once \
                     Helm is up."
                ),
                true,
            ),
            WakeOutcome::Off => (WAKE_OFF.to_owned(), false),
            WakeOutcome::NoInfo => (
                format!("The hub doesn't know how to wake {machine} yet."),
                false,
            ),
            WakeOutcome::RateLimited => (
                format!("A wake signal went to {machine} a moment ago. Give it a minute."),
                false,
            ),
            WakeOutcome::Failed(why) => (format!("Waking {machine} failed: {why}."), false),
        }
    }

    /// `/starthelm`: start Helm on the person's PC whose launcher is up.
    async fn on_start_helm(&self, chat: i64, now: i64) {
        let Some((person, lines)) = self.household() else {
            self.reply(chat, PC_ROUTING_OFF).await;
            return;
        };
        if self.line.is_up() {
            self.reply(chat, "Helm is already connected.").await;
            return;
        }
        let machines = lines.pc_machines(person, Channel::Telegram);
        let pick = machines
            .iter()
            .find(|m| m.launcher.is_some_and(|l| !l.helm_running))
            .or_else(|| machines.iter().find(|m| m.launcher.is_some()));
        let Some(m) = pick else {
            self.reply(chat, START_NONE).await;
            return;
        };
        let (said, _) = self.start_sentence(lines, person, m.client_id, &m.name, now);
        self.reply(chat, &said).await;
    }

    /// `/wake`: wake the person's PC that has no launcher up and that the hub knows how to wake.
    async fn on_wake(&self, chat: i64, now: i64) {
        let Some((person, lines)) = self.household() else {
            self.reply(chat, PC_ROUTING_OFF).await;
            return;
        };
        if self.line.is_up() {
            self.reply(chat, "Helm is already connected.").await;
            return;
        }
        if !lines.launchers().wake_enabled() {
            self.reply(chat, WAKE_OFF).await;
            return;
        }
        let machines = lines.pc_machines(person, Channel::Telegram);
        let pick = machines
            .iter()
            .find(|m| m.wake_ready && m.launcher.is_none())
            .or_else(|| machines.iter().find(|m| m.wake_ready));
        let Some(m) = pick else {
            self.reply(chat, WAKE_NONE).await;
            return;
        };
        let (said, _) = self.wake_sentence(lines, m.client_id, &m.name, now).await;
        self.reply(chat, &said).await;
    }

    /// A tap on a Start or Wake button. Only a machine of this person's counts.
    async fn on_pc_button(&self, action: PcAction, client_id: i64, query: &str) {
        let now = now_ms();
        let Some((person, lines)) = self.household() else {
            self.answer_query(query, Some(PC_ROUTING_OFF)).await;
            return;
        };
        let machines = lines.pc_machines(person, Channel::Telegram);
        let Some(m) = machines.iter().find(|m| m.client_id == client_id) else {
            self.answer_query(query, Some(TAP_STALE)).await;
            return;
        };
        let (said, went) = match action {
            PcAction::Start => self.start_sentence(lines, person, client_id, &m.name, now),
            PcAction::Wake => self.wake_sentence(lines, client_id, &m.name, now).await,
        };
        self.answer_query(query, Some(&cut(&said, 200))).await;
        if went {
            if let Some(chat) = self.reply_chat() {
                self.reply(chat, &said).await;
            }
        }
    }

    /// Send what may go, and say whether message `id` went with it. A message that stays behind
    /// while the line is up waits for a voice note before it, so only a line that is down earns
    /// the "offline" reply.
    async fn deliver(&self, id: &str, chat: i64, now: i64) -> Handled {
        if self.flush_sent(now).await.iter().any(|s| s == id) {
            return Handled::Forwarded;
        }
        if !self.line.is_up() {
            self.tell_offline(chat, now).await;
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
        let elsewhere = self.held_elsewhere();
        let queue_full = {
            let st = self.st();
            let mine = st.queue.iter().map(Queued::held).sum::<usize>();
            elsewhere + mine + b64_len(job.size) > MAX_QUEUED_BYTES
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
            let mine = q.file_bytes();
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
            match me.fetch_part(&job, chat, &slot, index).await {
                Fetched::Part(p) => me.part_done(&slot, index, &job, Ok(p)).await,
                Fetched::Failed(why) => me.part_done(&slot, index, &job, Err(why)).await,
                // A held recording: `stt_back` completes its message when the server is back.
                Fetched::Held => {}
            }
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

    /// Transcribe a recording, trying once more when the first failure looks passing. When it
    /// takes longer than [`SLOW_AFTER`] in total, the chat is told once that it is still going,
    /// and that message is removed when the transcription ends, either way. The failure keeps its
    /// kind, so the caller can tell a passing trouble from a real one.
    async fn transcribe(
        &self,
        voice: &Arc<dyn Voice>,
        chat: i64,
        audio: Vec<u8>,
        name: &str,
        mime: &str,
    ) -> Result<String, SttError> {
        let mut work = Box::pin(retry_transcribing(
            voice.endpoint(),
            self.stt_retry_after,
            move || {
                let audio = audio.clone();
                voice.transcribe(audio, name, mime)
            },
        ));
        if let Ok(done) = tokio::time::timeout(self.slow_after, &mut work).await {
            return done;
        }
        let notice = {
            let _g = self.ops.lock().await;
            match self.send(chat, STILL_TRANSCRIBING, true).await {
                Ok(sent) => sent.get("message_id").and_then(Value::as_i64),
                Err(e) => {
                    eprintln!(
                        "telegram: the still-transcribing note failed: {}",
                        e.description
                    );
                    None
                }
            }
        };
        let done = work.await;
        if let Some(message_id) = notice {
            let _g = self.ops.lock().await;
            if let Err(e) = self
                .api
                .call(
                    "deleteMessage",
                    json!({ "chat_id": chat, "message_id": message_id }),
                )
                .await
            {
                eprintln!(
                    "telegram: removing the still-transcribing note failed: {}",
                    e.description
                );
            }
        }
        done
    }

    /// A transcription failure's plain words for the chat: the reason, and when the health
    /// probe has seen the server down a while, since when.
    fn failure_words(&self, e: &SttError) -> String {
        match self
            .stt_health
            .as_ref()
            .and_then(|h| h.since_text(now_ms()))
        {
            Some(since) => format!("{}; {since}", e.why),
            None => e.why.clone(),
        }
    }

    /// Set a failed recording aside for [`Bridge::stt_back`]: the message keeps waiting with it.
    /// False when the bridge already holds as much as it may; then this failure is an ordinary
    /// one and the message goes without its text.
    fn hold_recording(
        &self,
        slot: &str,
        index: usize,
        job: &Job,
        chat: i64,
        audio: &[u8],
        name: &str,
        mime: &str,
        why: &SttError,
        at: i64,
    ) -> bool {
        let held = self.stt_held_bytes.load(Ordering::SeqCst);
        if held + audio.len() > HELD_AUDIO_MAX {
            return false;
        }
        self.stt_held_bytes.fetch_add(audio.len(), Ordering::SeqCst);
        self.stt_held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push_back(HeldPart {
                at,
                slot: slot.to_owned(),
                index,
                chat,
                job: job.clone(),
                audio: audio.to_vec(),
                name: name.to_owned(),
                mime: mime.to_owned(),
                why: why.clone(),
            });
        true
    }

    /// `getFile`, the download, then a look at the bytes (is it a picture Helm shows?) and, for a
    /// recording, the speech-to-text server. A recording that fails with passing trouble ends up
    /// `Held`: the bridge keeps it until [`Bridge::stt_back`] puts it through.
    async fn fetch_part(&self, job: &Job, chat: i64, slot: &str, index: usize) -> Fetched {
        // A few at a time, so only those few files' bytes are in memory while they are fetched.
        let turn = match self.downloads.acquire().await {
            Ok(t) => t,
            Err(_) => return Fetched::Failed("the hub is shutting down".to_owned()),
        };
        let file_id = match job.file_id.as_deref() {
            Some(f) => f.to_owned(),
            None => return Fetched::Failed("Telegram sent no file with it".to_owned()),
        };
        let file = match self
            .api
            .call("getFile", json!({ "file_id": file_id }))
            .await
        {
            Ok(f) => f,
            Err(e) => {
                return Fetched::Failed(format!(
                    "Telegram did not hand over the file ({})",
                    e.description
                ))
            }
        };
        if file
            .get("file_size")
            .and_then(Value::as_u64)
            .is_some_and(|n| n > MAX_FILE_BYTES)
        {
            return Fetched::Failed(TOO_LARGE.into());
        }
        let path = match file.get("file_path").and_then(Value::as_str) {
            Some(p) => p.to_owned(),
            None => return Fetched::Failed("Telegram gave no download path for it".to_owned()),
        };
        let bytes = match self.download(&path).await {
            Ok(b) => b,
            Err(e) => return Fetched::Failed(e),
        };
        // The download is done: give the turn back before a transcription, which can take
        // minutes, so the files behind this one are not held up by it.
        drop(turn);
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
                let _slot = match self.transcribing.acquire().await {
                    Ok(s) => s,
                    Err(_) => return Fetched::Failed("the hub is shutting down".to_owned()),
                };
                match self
                    .transcribe(voice, chat, bytes.clone(), &name, &mime)
                    .await
                {
                    Ok(t) => Some(Heard::Text(t.trim().to_owned())),
                    Err(e) => {
                        // Trouble that should pass: keep the recording, tell the chat up front,
                        // and let the message wait for [`Bridge::stt_back`].
                        if e.kind.is_transient()
                            && self.hold_recording(
                                slot,
                                index,
                                job,
                                chat,
                                &bytes,
                                &name,
                                &mime,
                                &e,
                                now_ms(),
                            )
                        {
                            let text = match self
                                .stt_health
                                .as_ref()
                                .and_then(|h| h.since_text(now_ms()))
                            {
                                Some(since) => format!("{VOICE_HELD} It has been {since}."),
                                None => VOICE_HELD.into(),
                            };
                            let _g = self.ops.lock().await;
                            if let Err(e) = self.send(chat, &text, true).await {
                                eprintln!(
                                    "telegram: the held-note reply failed: {}",
                                    e.description
                                );
                            }
                            return Fetched::Held;
                        }
                        Some(Heard::Failed(self.failure_words(&e)))
                    }
                }
            }
        };
        match self
            .finish_part(job, bytes, mime, own, from_path, heard)
            .await
        {
            Ok(p) => Fetched::Part(p),
            Err(why) => Fetched::Failed(why),
        }
    }

    /// The fetched bytes as a part: what kind of picture it is (if one shows in Helm), the name
    /// it goes under, and the base64 the frame carries. `alt` is the name from the download path,
    /// which a later retry no longer has.
    async fn finish_part(
        &self,
        job: &Job,
        bytes: Vec<u8>,
        mime: String,
        own: Option<String>,
        alt: Option<String>,
        heard: Option<Heard>,
    ) -> Result<Part, String> {
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
                .or(alt)
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

    /// Put a held recording back through the server. If that fails with passing trouble again
    /// and its hour is not up it waits on; otherwise the message goes without its text, as a
    /// failed transcription would have taken it from the start.
    async fn retry_held(&self, h: HeldPart) {
        let voice = match self.voice.clone() {
            Some(v) => v,
            None => {
                self.finish_held_failed(h).await;
                return;
            }
        };
        let r = self
            .transcribe(&voice, h.chat, h.audio.clone(), &h.name, &h.mime)
            .await;
        match r {
            Ok(t) => {
                let heard = Some(Heard::Text(t.trim().to_owned()));
                let (slot, index, job) = (h.slot.clone(), h.index, h.job.clone());
                let own = h.job.name.as_deref().and_then(clean_name);
                let part = self
                    .finish_part(&h.job, h.audio, h.mime, own, Some(h.name), heard)
                    .await;
                self.part_done(&slot, index, &job, part).await;
            }
            Err(e) => {
                if e.kind.is_transient()
                    && self.hold_recording(
                        &h.slot, h.index, &h.job, h.chat, &h.audio, &h.name, &h.mime, &e, h.at,
                    )
                {
                    return;
                }
                let h = HeldPart { why: e, ..h };
                self.finish_held_failed(h).await;
            }
        }
    }

    /// A held recording gave up: its message completes with the failure words it is owed, the
    /// way a failed transcription always completes.
    async fn finish_held_failed(&self, h: HeldPart) {
        let words = self.failure_words(&h.why);
        let (slot, index, job) = (h.slot.clone(), h.index, h.job.clone());
        let own = h.job.name.as_deref().and_then(clean_name);
        let part = self
            .finish_part(
                &h.job,
                h.audio,
                h.mime,
                own,
                Some(h.name),
                Some(Heard::Failed(words)),
            )
            .await;
        self.part_done(&slot, index, &job, part).await;
    }

    /// One file of the message held at `slot` has been fetched (or not): keep it, within the caps,
    /// note what the user must be told, and send what may go. A message dropped meanwhile with
    /// /cancel is not brought back.
    async fn part_done(&self, slot: &str, index: usize, job: &Job, got: Result<Part, String>) {
        let elsewhere = self.held_elsewhere();
        {
            let mut st = self.st();
            // What is held besides this file, whose share set aside is given back now.
            let held = st.queue.iter().map(Queued::held).sum::<usize>() + elsewhere;
            let held = held.saturating_sub(b64_len(job.size));
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
                    } else if held + part.blob.data.len() > MAX_QUEUED_BYTES {
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
        let (due, expired, undelivered, gave_up, finished) = {
            let mut st = self.st();
            for q in st.queue.iter_mut() {
                // A message stays with the machine it went to while that line is up, so two of
                // the person's machines never both answer it; once it drops, it goes to the next.
                if q.sent_on.is_some_and(|g| !self.line.holds(g)) {
                    q.sent_on = None;
                }
            }
            // A long text whose window has ended (or that a short part closed) goes as it is, or as
            // an attachment when it is too long to be text.
            for q in st.queue.iter_mut() {
                if q.hold.as_ref().is_some_and(|h| now >= h.open_until_ms) {
                    if let Some(h) = q.hold.take() {
                        release(q, h);
                    }
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
                // When every file failed the caption still goes, as text, beside the notes that
                // say which files were left out; with no caption either there is nothing to send.
                let kept = !parts.is_empty() || !g.caption.is_empty();
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
            st.queue.retain(|q| {
                q.text.is_some() || q.gather.is_some() || q.hold.is_some() || q.sent_on.is_some()
            });
            // Past the lifetime a message goes, waiting or unacknowledged alike: one Helm keeps
            // refusing would otherwise be resent for ever. A message whose files are still
            // being fetched keeps its place for far longer than its HTTP timeouts allow, but not
            // for ever.
            let lifetime = self.lifetime_ms;
            let (mut expired, mut undelivered, mut gave_up) = (0, 0, 0);
            st.queue.retain(|q| {
                let keep = match q.text {
                    None => now - q.sent_down_ms <= GATHERING_MAX_MS,
                    Some(_) => now - q.sent_at_ms <= lifetime,
                };
                match (keep, q.sent_on, &q.text) {
                    // Its files never finished arriving: that is not a wait for Helm.
                    (false, _, None) => gave_up += 1,
                    (false, None, _) => expired += 1,
                    (false, Some(_), _) => undelivered += 1,
                    _ => {}
                }
                keep
            });
            // The messages to send now, in order. Order is kept: nothing overtakes a message
            // still gathering its files.
            let due: Vec<String> = match generation {
                Some(_) => st
                    .queue
                    .iter()
                    .filter(|q| q.sent_on.is_none() || now - q.sent_down_ms >= resend_ms)
                    .map_while(|q| q.text.is_some().then(|| q.id.clone()))
                    .collect(),
                None => vec![],
            };
            (due, expired, undelivered, gave_up, finished)
        };
        // One frame at a time: a frame holds a second, JSON copy of its files' base64, so building
        // them all at once would hold about twice what the queue does. A flush also stops after
        // [`FLUSH_BYTES`] of files; the rest go on the next tick.
        let mut sent: Vec<String> = vec![];
        let mut budget = FLUSH_BYTES;
        for id in due {
            let Some(gen) = generation else { break };
            let frame = {
                let mut st = self.st();
                let bot = st.bot_name.clone();
                // Gone meanwhile (acknowledged, or dropped with /cancel): nothing to send.
                let Some(q) = st.queue.iter_mut().find(|q| q.id == id) else {
                    continue;
                };
                let Some(text) = &q.text else { break };
                let weight = q.data_bytes();
                if !sent.is_empty() && weight > budget {
                    break;
                }
                budget = budget.saturating_sub(weight);
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
                self.tag(&mut frame);
                let on = *q.sent_on.get_or_insert(gen);
                q.sent_down_ms = now;
                (frame, on)
            };
            let (frame, on) = frame;
            if self.line.send_on(on, &frame) {
                sent.push(id);
            } else {
                // The line dropped mid-flush: this one and the rest wait for the next.
                let mut st = self.st();
                for q in st.queue.iter_mut() {
                    if q.id == id || q.sent_on == Some(on) {
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
                self.tell_offline(chat, now).await;
            }
        }
        let mut notices: Vec<String> = Vec::new();
        if expired > 0 {
            notices.push(if expired == 1 {
                "1 queued message expired before Helm reconnected and was dropped. Send it again \
                 if it still matters."
                    .to_owned()
            } else {
                format!(
                    "{expired} queued messages expired before Helm reconnected and were dropped. \
                     Send them again if they still matter."
                )
            });
        }
        if gave_up > 0 {
            notices.push(if gave_up == 1 {
                "1 message with files was dropped: its files did not finish arriving in time."
                    .to_owned()
            } else {
                format!(
                    "{gave_up} messages with files were dropped: their files did not finish \
                     arriving in time."
                )
            });
        }
        if undelivered > 0 {
            notices.push(if undelivered == 1 {
                "1 message could not be delivered to Helm in time and was dropped.".to_owned()
            } else {
                format!(
                    "{undelivered} messages could not be delivered to Helm in time and were \
                     dropped."
                )
            });
        }
        if let Some(chat) = self.reply_chat() {
            for text in notices {
                self.reply(chat, &text).await;
            }
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
        let Some(chat) = self.reply_chat() else {
            return;
        };
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
        let Some(chat) = self.reply_chat() else {
            return;
        };
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
        let Some(chat) = self.reply_chat() else {
            return;
        };
        for piece in split_text(text.trim(), MAX_TEXT) {
            if let Err(e) = self.send(chat, &piece, false).await {
                eprintln!("telegram: alert failed: {}", e.description);
            }
        }
    }

    /// An approval ask: a new (notifying) message with one button per option, one per row. An id
    /// already on the phone is not sent twice.
    pub async fn on_ask(&self, client: i64, id: &str, text: &str, options: &[String]) {
        let _g = self.ops.lock().await;
        if self.st().asks.iter().any(|a| a.id == id) {
            return;
        }
        let Some(chat) = self.reply_chat() else {
            return;
        };
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
                    origin: self.origin(client),
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
        if !from.is_some_and(|f| self.allows(f)) {
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
        if let Some((action, client)) = q
            .get("data")
            .and_then(Value::as_str)
            .and_then(parse_pc_button)
        {
            self.on_pc_button(action, client, query).await;
            return Handled::Button;
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
        let mut frame = json!({ "type": "telegram_answer", "id": id, "index": index });
        self.tag(&mut frame);
        // Back to the machine that asked, while that very connection is up; the routed one only
        // when it is not.
        let asked = ask
            .origin
            .and_then(|(client, gen)| self.origin_line(client, gen));
        let sent = match asked {
            Some(line) => line.send(&frame),
            None => self.line.send(&frame),
        };
        if !sent {
            // Nothing reached Helm, so the buttons stay for another try. A launcher that is up
            // is asked to start Helm, and the person is told to tap again shortly.
            let said = self.tap_offline_text(now_ms());
            self.answer_query(query, Some(&said)).await;
            return Handled::Button;
        }
        self.st().asks.retain(|a| a.id != id);
        self.answer_query(query, Some(TAP_SENT)).await;
        self.edit_closed(&ask, &format!("{}\n\n→ {label}", ask.text))
            .await;
        Handled::Button
    }

    /// Handle one frame `client` sent up its line.
    pub async fn on_upstream(&self, client: i64, u: Upstream) {
        match u {
            Upstream::TelegramAck(id) => self.on_ack(&id),
            Upstream::TelegramStatus(t) => self.on_status(&t),
            Upstream::TelegramFinal(t) => self.on_final(&t).await,
            Upstream::TelegramNotify(t) => self.on_notify(&t).await,
            Upstream::TelegramAsk { id, text, options } => {
                self.on_ask(client, &id, &text, &options).await;
            }
            Upstream::TelegramAskDone { id, text } => self.on_ask_done(&id, &text).await,
            // Household questions are answered where the line is kept, and the voice frames go
            // to the voice bridge; neither reaches this one.
            Upstream::Peer { .. }
            | Upstream::VoiceAck(_)
            | Upstream::VoiceStatus { .. }
            | Upstream::VoiceAttachment(_)
            | Upstream::VoiceFinal(_)
            | Upstream::VoiceNotify(_)
            | Upstream::Device { .. }
            | Upstream::Dev { .. }
            | Upstream::DevPush { .. }
            | Upstream::KnPull { .. }
            | Upstream::KnPut { .. }
            | Upstream::Ix { .. }
            | Upstream::PersonEnsure { .. }
            | Upstream::RoutePin { .. }
            | Upstream::TgAdmin { .. } => {}
        }
    }

    /// Tick the queue and the pending status edit, for as long as the hub runs.
    fn start_ticks(self: &Arc<Self>) {
        let ticker = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(TICK);
            loop {
                iv.tick().await;
                let now = now_ms();
                ticker.flush(now).await;
                ticker.flush_status(now).await;
                ticker.tick_pc(now).await;
            }
        });
    }

    /// Run the bot: tick the queue and status, consume Helm's frames, and long-poll for updates
    /// unless another service that already runs the bot posts them.
    pub async fn run(
        self: Arc<Self>,
        mut upstream: tokio::sync::mpsc::UnboundedReceiver<(i64, Upstream)>,
    ) {
        if let Err(e) = self.identify().await {
            eprintln!("telegram: getMe failed: {}", e.description);
        }
        self.start_ticks();
        let up = self.clone();
        tokio::spawn(async move {
            while let Some((client, u)) = upstream.recv().await {
                up.on_upstream(client, u).await;
            }
        });
        if self.receive == Receive::Off {
            return;
        }
        let mut poller = Poller::default();
        loop {
            for u in poller.next(self.api.as_ref()).await {
                self.on_update(&u, now_ms()).await;
            }
        }
    }
}

/// `getUpdates` with the offset and the back-off after a failure.
struct Poller {
    offset: i64,
    backoff: Duration,
}

impl Default for Poller {
    fn default() -> Self {
        Poller {
            offset: 0,
            backoff: Duration::from_secs(1),
        }
    }
}

impl Poller {
    /// The next batch of updates (empty after an error, once the back-off has passed).
    async fn next(&mut self, api: &dyn TelegramApi) -> Vec<Value> {
        let r = api
            .call(
                "getUpdates",
                json!({ "offset": self.offset, "timeout": POLL_SECONDS,
                        "allowed_updates": ["message", "callback_query"] }),
            )
            .await;
        match r {
            Ok(Value::Array(updates)) => {
                self.backoff = Duration::from_secs(1);
                for u in &updates {
                    if let Some(id) = u.get("update_id").and_then(Value::as_i64) {
                        self.offset = self.offset.max(id + 1);
                    }
                }
                updates
            }
            Ok(_) => Vec::new(),
            Err(e) => {
                eprintln!("telegram: getUpdates failed: {}", e.description);
                tokio::time::sleep(self.backoff).await;
                self.backoff = (self.backoff * 2).min(Duration::from_secs(60));
                Vec::new()
            }
        }
    }
}

impl SttHealed for Bridge {
    /// The speech server answers `/health` again (or the hour is up): every recording still
    /// under its hour goes back through it, inside the one-transcription gate, and the one that
    /// waited too long goes through without its text.
    fn stt_back(&self, at: i64) {
        let Some(me) = self.strong() else { return };
        let drained: Vec<HeldPart> = self
            .stt_held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
            .collect();
        for h in drained {
            self.stt_held_bytes
                .fetch_sub(h.audio.len(), Ordering::SeqCst);
            if at - h.at > HELD_MAX_MS {
                let me = me.clone();
                tokio::spawn(async move { me.finish_held_failed(h).await });
                continue;
            }
            let me = me.clone();
            tokio::spawn(async move {
                let _gate = match me.transcribing.acquire().await {
                    Ok(g) => g,
                    Err(_) => return,
                };
                me.retry_held(h).await;
            });
        }
    }

    /// The outage is still on: a recording that waited its hour goes without its text, so the
    /// held set cannot grow without end and no message sits in the queue past its own lifetime.
    fn stt_expire(&self, at: i64) {
        let me = match self.strong() {
            Some(me) => me,
            None => return,
        };
        let due: Vec<HeldPart> = {
            let mut held = self.stt_held.lock().unwrap_or_else(|p| p.into_inner());
            let mut i = 0;
            let mut due = Vec::new();
            while i < held.len() {
                if at - held[i].at > HELD_MAX_MS {
                    let h = held.remove(i).expect("index within the queue");
                    self.stt_held_bytes
                        .fetch_sub(h.audio.len(), Ordering::SeqCst);
                    due.push(h);
                } else {
                    i += 1;
                }
            }
            due
        };
        for h in due {
            let me = me.clone();
            tokio::spawn(async move { me.finish_held_failed(h).await });
        }
    }
}

impl SttHealed for Telegram {
    fn stt_back(&self, at: i64) {
        for (_, b) in self
            .bridges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
        {
            b.stt_back(at);
        }
    }

    fn stt_expire(&self, at: i64) {
        for (_, b) in self
            .bridges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
        {
            b.stt_expire(at);
        }
    }
}

/// The bot for the whole household: one [`Bridge`] per person who has a Telegram account linked
/// to them, sharing one bot and one poll. An update goes to the bridge that allows its sender, and
/// what a person's machines send up goes to that person's bridge. The allow-list is the hub's
/// table of linked accounts, so a person linked since the hub started has a bridge too: a link
/// that names someone with none makes one on the spot.
pub struct Telegram {
    api: Arc<dyn TelegramApi>,
    receive: Receive,
    /// `(person id, bridge)`.
    bridges: Mutex<Vec<(i64, Arc<Bridge>)>>,
    /// What it takes to make a bridge for a person a link adds. `None` for a bot built around one
    /// bridge (a test, a hub with no household): such a bot cannot gain a person.
    make: Option<BotMaker>,
    /// A link Helm asked for and the person has not tapped yet: the code's digest to
    /// `(person id, expires_ms)`. In memory only: a hub that restarts answers the tap the way it
    /// answers a stranger, and the person asks Helm for another link.
    codes: Mutex<HashMap<[u8; 32], (i64, i64)>>,
    /// The bot's own username (`getMe`), for the address a link is offered on.
    bot_name: Mutex<Option<String>>,
}

/// Everything a new bridge needs, kept so the bot can grow.
struct BotMaker {
    lines: Arc<Lines>,
    cfg: TelegramConfig,
    /// The bridges of this bot, so a new one shares the file budget at once and copies what the
    /// others fetch (files, recordings).
    siblings: Arc<Mutex<Vec<Weak<Bridge>>>>,
}

/// How long a link code the person has been offered still works.
pub fn link_code_lifetime_ms() -> i64 {
    LINK_CODE_MINUTES * 60_000
}

impl Telegram {
    pub fn new(
        api: Arc<dyn TelegramApi>,
        receive: Receive,
        bridges: Vec<(i64, Arc<Bridge>)>,
    ) -> Self {
        Telegram {
            api,
            receive,
            bridges: Mutex::new(bridges),
            make: None,
            codes: Mutex::new(HashMap::new()),
            bot_name: Mutex::new(None),
        }
    }

    /// The bot for one bridge alone (person id 0): a hub with no household, and tests.
    pub fn single(bridge: Arc<Bridge>) -> Self {
        Telegram {
            api: bridge.api.clone(),
            receive: bridge.receive,
            bridges: Mutex::new(vec![(0, bridge)]),
            make: None,
            codes: Mutex::new(HashMap::new()),
            bot_name: Mutex::new(None),
        }
    }

    /// One bridge per person who has a linked Telegram account, from the hub's table.
    ///
    /// `seed_links` first writes what `HUB_TELEGRAM_ALLOWED_USERS` says into that table, which is
    /// why the environment stays valid and wins for the ids it lists; a person named there is made
    /// if the hub has never heard of them (`resolve`), and an id with no person belongs to
    /// `owner`. `decorate` adds files and voice to each bridge.
    pub fn build(
        api: Arc<dyn TelegramApi>,
        lines: &Arc<Lines>,
        cfg: &TelegramConfig,
        owner: i64,
        resolve: &mut dyn FnMut(&str) -> Result<i64, String>,
        decorate: &dyn Fn(Bridge) -> Bridge,
    ) -> Result<Telegram, String> {
        let links = Self::seed_links(lines, cfg, owner, resolve)?;
        let siblings = Arc::new(Mutex::new(Vec::new()));
        let mut groups: HashMap<i64, Vec<i64>> = HashMap::new();
        for link in &links {
            groups
                .entry(link.person_id)
                .or_default()
                .push(link.telegram_id);
        }
        let mut people: Vec<i64> = groups.keys().copied().collect();
        people.sort_unstable();
        let mut bridges: Vec<(i64, Arc<Bridge>)> = Vec::new();
        for person in people {
            let mut ids = groups.remove(&person).unwrap_or_default();
            ids.sort_unstable();
            let c = TelegramConfig {
                allowed: ids,
                people: Vec::new(),
                ..cfg.clone()
            };
            let route = Arc::new(PersonRoute::new(lines.clone(), person));
            let mut bridge = (decorate)(Bridge::with_route(api.clone(), route, &c))
                .with_person(person, lines.clone());
            bridge.siblings = siblings.clone();
            let bridge = Arc::new(bridge);
            bridge.register_self();
            siblings
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(Arc::downgrade(&bridge));
            bridges.push((person, bridge));
        }
        Ok(Telegram {
            api,
            receive: cfg.receive,
            bridges: Mutex::new(bridges),
            make: Some(BotMaker {
                lines: lines.clone(),
                cfg: cfg.clone(),
                siblings,
            }),
            codes: Mutex::new(HashMap::new()),
            bot_name: Mutex::new(None),
        })
    }

    /// Write what the environment lists into the hub's table of Telegram accounts (the listed ids
    /// win over anything the table held) and read back the whole list, which is the allow-list.
    fn seed_links(
        lines: &Arc<Lines>,
        cfg: &TelegramConfig,
        owner: i64,
        resolve: &mut dyn FnMut(&str) -> Result<i64, String>,
    ) -> Result<Vec<crate::registry::TelegramLink>, String> {
        let named: HashMap<i64, String> = cfg.people.iter().cloned().collect();
        // Every person is resolved before the registry is locked: `resolve` locks it itself.
        let mut wanted: Vec<(i64, i64)> = Vec::new();
        for id in &cfg.allowed {
            let person = match named.get(id) {
                Some(name) => resolve(name)?,
                None => owner,
            };
            wanted.push((*id, person));
        }
        let mut reg = lines
            .registry()
            .lock()
            .map_err(|_| "the registry is unavailable".to_owned())?;
        let held = reg.telegram_links().map_err(|e| e.to_string())?;
        for (id, person) in &wanted {
            let was = held.iter().find(|l| l.telegram_id == *id);
            // An id the settings list wins, so say when that overrides a link the person made, or
            // moves an account that was left with someone else.
            if was.is_some_and(|l| l.source != "env" || l.person_id != *person) {
                let name = reg
                    .person_by_id(*person)
                    .ok()
                    .flatten()
                    .map(|p| p.name)
                    .unwrap_or_else(|| person.to_string());
                eprintln!(
                    "telegram: the settings put account {id} with {name}, overriding its earlier link"
                );
            }
            reg.link_telegram(*id, *person, None, "env")
                .map_err(|e| e.to_string())?;
        }
        // An id taken out of the settings is cut off: a row from an earlier start must not
        // outlive the setting that wrote it.
        let ids: Vec<i64> = wanted.iter().map(|(id, _)| *id).collect();
        let pruned = reg.prune_env_links(&ids).map_err(|e| e.to_string())?;
        if pruned > 0 {
            eprintln!(
                "telegram: {pruned} account(s) no longer listed in the settings were unlinked"
            );
        }
        reg.telegram_links().map_err(|e| e.to_string())
    }

    /// Tell the lines where this bot is, so a profile's `tg_link`, `tg_unlink` and `tg_status` can
    /// be answered on the line that asked. [`Telegram::run`] does this itself.
    pub fn attach(self: &Arc<Self>) {
        if let Some(make) = &self.make {
            make.lines.bind_telegram(self);
        }
    }

    /// Learn the bot's own address (`getMe`) and hold it for the link answers. [`Telegram::run`]
    /// does this itself; a caller that never runs the bot (a test, a forwarder-only hub) can too.
    pub async fn prime(&self) {
        for (_, b) in self.all_bridges() {
            if let Err(e) = b.identify().await {
                eprintln!("telegram: getMe failed: {}", e.description);
            }
            if let Some(name) = b.st().bot_name.clone() {
                *self.bot_name.lock().unwrap_or_else(|p| p.into_inner()) = Some(name);
            }
        }
    }

    /// The bridge that allows Telegram account `sender`, if any does.
    fn bridge_of(&self, sender: i64) -> Option<Arc<Bridge>> {
        self.bridges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|(_, b)| b.allows(sender))
            .map(|(_, b)| b.clone())
    }

    /// Every bridge, as they stand now.
    fn all_bridges(&self) -> Vec<(i64, Arc<Bridge>)> {
        self.bridges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The bot's username, from the bridges' own `getMe`, or `None` while none has answered.
    fn bot_username(&self) -> Option<String> {
        {
            let held = self.bot_name.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(name) = held.as_ref() {
                return Some(name.clone());
            }
        }
        let name = self
            .all_bridges()
            .into_iter()
            .find_map(|(_, b)| b.st().bot_name.clone())?;
        *self.bot_name.lock().unwrap_or_else(|p| p.into_inner()) = Some(name.clone());
        Some(name)
    }

    // ----- a person's own link -----

    /// The answer to a profile's `tg_link`, `tg_unlink` or `tg_status`, for the line that asked.
    /// Only a PC that acts for that person may ask about them.
    pub fn answer_admin(&self, lines: &Lines, client_id: i64, u: &Upstream) -> Value {
        let Upstream::TgAdmin { id, op, person_id } = u else {
            return json!({});
        };
        let refused = |why: &str| json!({ "type": format!("tg_{op}_result"), "id": id, "ok": false, "error": why });
        if self.make.is_none() {
            return refused(NO_BOT);
        }
        let Ok(person) = person_id.trim().parse::<i64>() else {
            return refused("that person is not known to this hub");
        };
        if !lines.may_act_as(client_id, person) {
            return refused("this PC does not act for that person");
        }
        let mut reg = match lines.registry().lock() {
            Ok(r) => r,
            Err(_) => return refused("the registry is unavailable"),
        };
        match op.as_str() {
            "link" => {
                // Know the answer is deliverable before keeping a code: a refusal must not
                // leave one stored.
                let name = match self.bot_username() {
                    Some(name) => name,
                    None => {
                        return refused(
                            "the hub does not know its bot's own address yet: try again",
                        )
                    }
                };
                let code = new_code();
                let expires = now_ms() + link_code_lifetime_ms();
                let mut codes = self.codes.lock().unwrap_or_else(|p| p.into_inner());
                // Only the newest code for this person stands, and nothing already
                // out of date is kept: a person who keeps asking cannot pile up codes.
                codes.retain(|_, (who, at)| *who != person || *at >= now_ms());
                codes.insert(digest(&code), (person, expires));
                json!({
                    "type": "tg_link_result", "id": id, "ok": true,
                    "url": format!("https://t.me/{name}?start={code}"),
                    "expiresAt": expires,
                })
            }
            "unlink" => {
                let ids: Vec<i64> = match reg.telegram_links() {
                    Ok(links) => links
                        .into_iter()
                        .filter(|l| l.person_id == person)
                        .map(|l| l.telegram_id)
                        .collect(),
                    Err(e) => return refused(&e.to_string()),
                };
                let mut gone = 0;
                for account in &ids {
                    match reg.unlink_telegram(*account, person) {
                        Ok(true) => gone += 1,
                        Ok(false) => {}
                        Err(e) => return refused(&e.to_string()),
                    }
                }
                drop(reg);
                for (p, b) in self.all_bridges() {
                    if p == person {
                        for account in &ids {
                            b.unallow(*account);
                        }
                    }
                }
                json!({ "type": "tg_unlink_result", "id": id, "ok": true, "unlinked": gone })
            }
            "status" => {
                let link = match reg.telegram_links() {
                    Ok(links) => links.into_iter().find(|l| l.person_id == person),
                    Err(e) => return refused(&e.to_string()),
                };
                drop(reg);
                match link {
                    Some(link) => json!({
                        "type": "tg_status_result", "id": id, "ok": true, "linked": true,
                        "telegramId": link.telegram_id.to_string(),
                        "handle": link.handle,
                    }),
                    None => {
                        json!({ "type": "tg_status_result", "id": id, "ok": true, "linked": false })
                    }
                }
            }
            other => refused(&format!("unknown Telegram op {other:?}")),
        }
    }

    /// A private `/start <code>` whose code is one the hub offered: the person proving which
    /// Telegram account is theirs. Binds the account to that person, moving it off whoever had it,
    /// and answers on the spot. `None` when this is not such an update, which is the ordinary
    /// silence: a wrong, used or expired code is answered exactly like a stranger.
    async fn try_link(&self, update: &Value) -> Option<Handled> {
        let msg = update.get("message")?;
        if msg.pointer("/chat/type").and_then(Value::as_str) != Some("private") {
            return None;
        }
        let from = msg.pointer("/from/id").and_then(Value::as_i64)?;
        let chat = msg.pointer("/chat/id").and_then(Value::as_i64)?;
        let text = msg.get("text").and_then(Value::as_str)?;
        let (word, rest) = text.trim().split_once(char::is_whitespace)?;
        let rest = rest.trim();
        if word.split('@').next() != Some("/start") || rest.is_empty() {
            return None;
        }
        let code = normalize_code(rest)?;
        let hash = digest(&code);
        let now = now_ms();
        let person = {
            let mut codes = self.codes.lock().unwrap_or_else(|p| p.into_inner());
            codes.retain(|_, (_, expires)| *expires >= now);
            codes.remove(&hash).map(|(person, _)| person)?
        };
        let handle = msg
            .pointer("/from/username")
            .and_then(Value::as_str)
            .map(|u| format!("@{}", u.trim_start_matches('@')));
        let name = {
            let mut reg = self.registry_lock()?;
            reg.link_telegram(from, person, handle.as_deref(), "link")
                .ok()?;
            reg.person_by_id(person).ok()??
        }
        .name;
        // The account answers one bridge only: whoever held it lets go, and the person's bridge
        // takes it — made here if this is their first linked account.
        for (_, b) in self.all_bridges() {
            if b.allows(from) {
                b.unallow(from);
            }
        }
        self.add_bridge(person, from).await?;
        self.reply_link(chat, &format!("{LINKED}{name}.")).await;
        Some(Handled::Command)
    }

    fn registry_lock(&self) -> Option<std::sync::MutexGuard<'_, crate::registry::Registry>> {
        let make = self.make.as_ref()?;
        make.lines.registry().lock().ok()
    }

    /// The bridge of `person`, given another account of theirs; made when they had none, and
    /// asked for the bot's own address before it is handed out, so the first message that goes
    /// down it already names the bot. `None` when this bot cannot make bridges.
    async fn add_bridge(&self, person: i64, telegram_id: i64) -> Option<Arc<Bridge>> {
        let make = self.make.as_ref()?;
        // The list is held only while the new bridge is built: asking the bot for its own
        // address happens after the lock is let go.
        let bridge = {
            let mut bridges = self.bridges.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((_, b)) = bridges.iter().find(|(p, _)| *p == person) {
                b.allow(telegram_id);
                return Some(b.clone());
            }
            let c = TelegramConfig {
                allowed: vec![telegram_id],
                people: Vec::new(),
                ..make.cfg.clone()
            };
            let route = Arc::new(PersonRoute::new(make.lines.clone(), person));
            let mut bridge = Bridge::with_route(self.api.clone(), route, &c)
                .with_person(person, make.lines.clone());
            // Files and recordings are the bot's own settings, not each person's: the new bridge
            // fetches what the others do.
            if let Some((_, one)) = bridges.iter().find(|(p, _)| *p != person) {
                if let Some(voice) = one.voice.clone() {
                    bridge = bridge.with_voice(voice);
                }
                if let Some(files) = one.files.clone() {
                    bridge = bridge.with_files(files);
                }
            }
            bridge.siblings = make.siblings.clone();
            let bridge = Arc::new(bridge);
            bridge.register_self();
            make.siblings
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(Arc::downgrade(&bridge));
            bridges.push((person, bridge.clone()));
            bridge
        };
        if let Err(e) = bridge.identify().await {
            eprintln!("telegram: getMe failed: {}", e.description);
        }
        bridge.start_ticks();
        Some(bridge)
    }

    async fn reply_link(&self, chat: i64, text: &str) {
        if let Err(e) = self
            .api
            .call(
                "sendMessage",
                json!({ "chat_id": chat, "text": text, "disable_notification": false }),
            )
            .await
        {
            eprintln!("telegram: the link answer failed: {}", e.description);
        }
    }

    // ----- updates and frames -----

    /// One update, from `getUpdates` or posted to `/telegram/update`. A sender nobody allows gets
    /// nothing, as before — unless they arrive with a link code, which is how an account joins.
    pub async fn on_update(&self, update: &Value, now: i64) -> Handled {
        if let Some(done) = self.try_link(update).await {
            return done;
        }
        let sender = update
            .pointer("/message/from/id")
            .or_else(|| update.pointer("/callback_query/from/id"))
            .and_then(Value::as_i64);
        match sender.and_then(|s| self.bridge_of(s)) {
            Some(b) => b.on_update(update, now).await,
            None => Handled::Ignored,
        }
    }

    /// Handle one frame a person's machine sent up.
    pub async fn on_upstream(&self, person: i64, client: i64, u: Upstream) {
        let bridge = self
            .bridges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|(p, _)| *p == person)
            .map(|(_, b)| b.clone());
        match bridge {
            Some(b) => b.on_upstream(client, u).await,
            // A person with a frame but no bridge has no linked Telegram account: there is nobody
            // to send it to.
            None if u.takes_person() => {
                eprintln!("telegram: a frame for person {person} had no bridge to take it");
            }
            None => {}
        }
    }

    /// Run the bot for everyone: identify, tick every bridge, route what the lines send up, and
    /// poll once for all of them (unless a forwarder posts the updates).
    pub async fn run(
        self: Arc<Self>,
        mut upstream: tokio::sync::mpsc::UnboundedReceiver<PersonUpstream>,
    ) {
        self.attach();
        self.prime().await;
        for (_, b) in self.all_bridges() {
            b.start_ticks();
        }
        let me = self.clone();
        tokio::spawn(async move {
            while let Some((person, client, u)) = upstream.recv().await {
                me.on_upstream(person, client, u).await;
            }
        });
        if self.receive == Receive::Off {
            return;
        }
        let mut poller = Poller::default();
        loop {
            for u in poller.next(self.api.as_ref()).await {
                self.on_update(&u, now_ms()).await;
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

/// Voice notes over HTTP: the file from the Bot API's file host, the transcript from the
/// speech-to-text server (see `stt.rs`).
pub struct HttpVoice {
    files: HttpFiles,
    stt: Stt,
}

impl HttpVoice {
    pub fn new(api_base: &str, token: &str, stt: &SttConfig) -> Self {
        HttpVoice {
            files: HttpFiles::new(api_base, token),
            stt: Stt::new(stt),
        }
    }
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

impl Voice for HttpVoice {
    fn download<'a>(&'a self, file_path: &'a str) -> VoiceFuture<'a, Vec<u8>> {
        self.files.download(file_path)
    }

    fn transcribe<'a>(
        &'a self,
        audio: Vec<u8>,
        name: &'a str,
        mime: &'a str,
    ) -> SttFuture<'a, String> {
        self.stt.transcribe(audio, name, mime)
    }

    fn endpoint(&self) -> &str {
        self.stt.url()
    }
}
