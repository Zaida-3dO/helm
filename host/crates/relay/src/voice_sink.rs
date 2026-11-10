//! The voice chat: the phone's turns into it, and its answers back out.
//!
//! **In.** [`ChatVoice`] takes each turn the hub forwards and puts it into the voice chat
//! ([`crate::voice::active_chat`]), starting `Voice <date>` when there is none. It is stored
//! through the chats crate's host send with origin `voice`, so it never counts as consent and an
//! approval, plan or trust prompt is never answered by it. On a busy chat it is a steer, read at
//! the next tool boundary. Its submission id is fixed per turn id, so a turn delivered twice is
//! stored once, and it is acknowledged to the hub only once it is stored.
//!
//! **Out.** While a turn runs, a mirror task watches the chat and sends ONE working line up the
//! line, only when it changes and at most every [`STATUS_GAP_MS`]. The turn's answer goes up as
//! soon as its end-of-turn row is in the transcript, as a `voice_final`: cleaned and cut for the
//! ear in conversation mode ([`crate::speakable`]), as written in chat mode. An answer the line
//! will not take is kept and offered again, with the same reply id, until it goes up. A turn that
//! stops at an approval, plan or trust prompt says so once, in a sentence, and the answer follows
//! when the turn ends: those are only ever answered in Helm's window.
//!
//! The mirror answers the latest turn: a turn that arrives while the chat is mid-turn is a steer,
//! and the answer that follows is the one for it.
//!
//! **Whose answer.** The mirror watches every turn of the voice chat, not only those the phone
//! started, and what started a turn decides where its answer goes ([`Route`]): a turn the phone
//! started answers as that turn's `voice_final`; a turn the chat was woken for (a declared wait
//! that fired, a note from the attention watch, a message from another chat) goes up as
//! `voice_notify` news, unless the chat answers exactly `[quiet]`; a turn nobody on the phone
//! started, such as one typed in Helm's window, goes nowhere. Things the chat queued with
//! `app_voice_show` go up just before the answer ([`Outbox`]).
//!
//! The event tap tells the sink whenever the voice chat becomes busy ([`ChatVoice::woken`]), so a
//! turn the chat was woken for is mirrored like any other. [`ChatVoice::deliver_note`] puts a
//! note from the attention watch into the chat.

use crate::client::{BoxFut, LineHandle, VoiceSink, VoiceTurn};
use crate::exec::AppExecutor;
use crate::notify::title_of;
use crate::sink::{chat_facts, max_row, plain_path, snapshot, RealPort, Snap, STATUS_GAP_MS};
use crate::telegram::{cut_chars, ChatPort};
use crate::voice::{self, Mode, Reply, TagPolicy, WatchLevel, VOICE_ORIGIN};
use crate::voice_show::{Attachment, Shown};
use crate::voice_watch::{Note, NOTE_PREFIX};
use helm_chats::submit::{host_send, HostSend};
use helm_chats::types::{ChatStatus, SubmissionOrigin};
use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_hosttools::OpInvoker;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often the mirror looks at the chat.
const POLL: Duration = Duration::from_millis(500);
/// A turn that has not started by now gets an answer saying so instead of silence.
pub const START_WAIT_MS: i64 = 120_000;
/// How long a delivery retries a chat that is stopping or full before leaving it to the hub.
const BUSY_RETRY: Duration = Duration::from_secs(20);
/// The longest working line.
const MAX_STATUS_CHARS: usize = 300;

/// The working line when nothing is known to be running.
pub const THINKING: &str = "Thinking";
/// The working line when the chat waits on something only Helm's window can answer.
pub const WAITING_STATUS: &str = "Waiting for you to approve something on the PC";
/// What is said of that, once.
pub const WAITING_ANSWER: &str = "I'm waiting for you to approve something on the PC.";
pub const NOT_STARTED: &str = "Helm has not started that yet. It is saved in the voice chat.";
pub const NOTHING_TO_SAY: &str = "Done.";

/// Where the working line and the answers go: the line in the app, a recorder in tests.
pub trait VoiceUpstream: Send + Sync {
    fn status(&self, turn: &str, text: &str);
    /// `false` when the reply was not sent (the line is down); the mirror then offers it again.
    fn reply(&self, reply: &Reply) -> bool;
    /// One thing to show, sent just before the reply it belongs to. `false` when it was not sent.
    fn attachment(&self, _frame: &Value) -> bool {
        true
    }
}

impl VoiceUpstream for LineHandle {
    fn status(&self, turn: &str, text: &str) {
        self.voice_status(turn, text)
    }
    fn reply(&self, reply: &Reply) -> bool {
        self.voice_reply(reply)
    }
    fn attachment(&self, frame: &Value) -> bool {
        self.voice_attachment(frame.clone())
    }
}

/// What the mirror is to answer with, and how: the latest turn, its mode, the conversation, and
/// what the phone's screen last said about itself.
#[derive(Clone, Debug, Default)]
pub struct Live {
    /// The phone's latest turn; the mirror's answers belong to it.
    pub turn: Option<String>,
    pub mode: Mode,
    pub conversation: String,
    /// The phone's screen is in front, and in which mode.
    pub presence_live: bool,
    pub presence_mode: Mode,
}

// ---- the mirror ------------------------------------------------------------------------

/// What an answer says.
#[derive(Clone, Debug, PartialEq)]
pub enum What {
    /// The agent's own words.
    Answer(String),
    /// The turn waits on something only Helm's window can answer.
    Waiting,
    /// No turn started for the message.
    NotStarted,
    /// The turn ended with nothing to say.
    Nothing,
}

/// An answer owed to the phone: its reply id is fixed, so a resend is the same reply.
#[derive(Clone, Debug, PartialEq)]
pub struct Owed {
    pub id: String,
    /// The transcript row it answers, when it is a turn's end.
    row: Option<i64>,
    pub what: What,
}

impl Owed {
    /// An answer that answers no transcript row in particular.
    pub fn unrowed(id: impl Into<String>, what: What) -> Self {
        Owed {
            id: id.into(),
            row: None,
            what,
        }
    }

    /// The text it is made into a reply from.
    pub fn text(&self) -> &str {
        match &self.what {
            What::Answer(t) => t,
            What::Waiting => WAITING_ANSWER,
            What::NotStarted => NOT_STARTED,
            What::Nothing => NOTHING_TO_SAY,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Up {
    Status(String),
    Final(Owed),
}

/// The working line: what is running now, in the few words the dock shows.
pub fn render_status(running: &[String]) -> String {
    if running.is_empty() {
        THINKING.to_owned()
    } else {
        cut_chars(&running.join(", "), MAX_STATUS_CHARS).0
    }
}

/// The mirror's state machine, driven by [`Snap`]s (so it is tested without a runtime).
#[derive(Debug)]
pub struct Mirror {
    pub baseline: i64,
    began: i64,
    started: bool,
    idle_seen: u32,
    last_status: Option<String>,
    last_at: Option<i64>,
    /// The waiting prompts already spoken of.
    waited: HashSet<String>,
    /// The transcript row of the last answer that reached the phone.
    sent_through: i64,
    /// An answer not yet confirmed sent, offered again on every step until [`Self::ack`].
    pending: Option<Owed>,
    /// Whether an answer of this exchange has gone up, so a close says nothing twice.
    answered: bool,
    /// The pending answer is the exchange's close: once it is sent, the mirror is done.
    closing: bool,
    pub done: bool,
}

impl Mirror {
    pub fn new(baseline: i64, now: i64) -> Self {
        Mirror {
            baseline,
            began: now,
            started: false,
            idle_seen: 0,
            last_status: None,
            last_at: None,
            waited: HashSet::new(),
            sent_through: 0,
            pending: None,
            answered: false,
            closing: false,
            done: false,
        }
    }

    /// The transcript row after which the exchange being answered began: the baseline, or the
    /// last answer already sent when the mirror has carried on past one.
    pub fn route_base(&self) -> i64 {
        self.baseline.max(self.sent_through)
    }

    fn owe(&mut self, row: Option<i64>, what: What, closing: bool) -> Vec<Up> {
        let owed = Owed {
            id: uuid::Uuid::new_v4().to_string(),
            row,
            what,
        };
        self.pending = Some(owed.clone());
        self.closing = closing;
        vec![Up::Final(owed)]
    }

    /// Report whether the answer with this reply id reached the phone. One that did is never sent
    /// up twice; one that did not stays offered. When the exchange is closed out, the mirror
    /// finishes with the send that carried the last word.
    pub fn ack(&mut self, id: &str, ok: bool) {
        if !ok || self.pending.as_ref().is_none_or(|p| p.id != id) {
            return;
        }
        if let Some(p) = self.pending.take() {
            if let Some(row) = p.row {
                self.sent_through = self.sent_through.max(row);
            }
            if p.what != What::Waiting {
                self.answered = true;
            }
        }
        if self.closing {
            self.done = true;
        }
    }

    pub fn step(&mut self, s: &Snap, now: i64) -> Vec<Up> {
        if self.done {
            return vec![];
        }
        if let Some(p) = &self.pending {
            // An answer is owed to the phone: offer it again before anything new.
            return vec![Up::Final(p.clone())];
        }
        if s.busy {
            self.started = true;
            self.idle_seen = 0;
            if let Some((id, text)) = &s.turn_done {
                if *id > self.sent_through {
                    // The turn's answer exists. Hand it up now instead of waiting for an idle
                    // sample: a steered-in message opens the next turn at the first boundary,
                    // so the chat may never show idle between the turns.
                    return self.owe(Some(*id), What::Answer(text.clone()), false);
                }
            }
            if let Some((id, _)) = s.question.as_ref().or(s.blocked.as_ref()) {
                if !self.waited.insert(id.clone()) {
                    return vec![];
                }
                self.last_status = None;
                self.last_at = Some(now);
                let mut out = vec![Up::Status(WAITING_STATUS.to_owned())];
                out.extend(self.owe(None, What::Waiting, false));
                return out;
            }
            // The turn's answer has gone up: the chat is only winding down, so no working line.
            if s.turn_done
                .as_ref()
                .is_some_and(|(id, _)| *id <= self.sent_through)
            {
                return vec![];
            }
            let st = render_status(&s.running);
            let due = self.last_at.is_none_or(|t| now - t >= STATUS_GAP_MS);
            if self.last_status.as_ref() != Some(&st) && due {
                self.last_status = Some(st.clone());
                self.last_at = Some(now);
                return vec![Up::Status(st)];
            }
            vec![]
        } else if self.started || s.final_text.is_some() {
            // Settled: seen idle twice in a row, so the last streamed row is flushed.
            self.idle_seen += 1;
            if self.idle_seen < 2 {
                return vec![];
            }
            let due = match (&s.turn_done, &s.final_text) {
                // Its answer went up while the chat was still busy: the exchange just closes.
                (Some((id, _)), _) if *id <= self.sent_through => None,
                (Some((id, text)), _) => Some((Some(*id), What::Answer(text.clone()))),
                (None, Some(text)) => Some((None, What::Answer(text.clone()))),
                (None, None) if self.answered => None,
                (None, None) => Some((None, What::Nothing)),
            };
            match due {
                Some((row, what)) => self.owe(row, what, true),
                None => {
                    self.done = true;
                    vec![]
                }
            }
        } else if now - self.began >= START_WAIT_MS {
            // A turn that never started: say so once and finish. The message itself is saved
            // in the chat, so this is the extra word, not the only path to the answer.
            self.owe(None, What::NotStarted, true)
        } else {
            vec![]
        }
    }
}

/// The reply for an answer owed, in the mode and conversation the phone is in.
pub fn reply_for(live: &Live, owed: &Owed, now: i64) -> Reply {
    Reply::from_text(
        owed.id.clone(),
        live.turn.clone(),
        live.mode,
        owed.text(),
        live.conversation.clone(),
        now,
    )
}

/// Unprompted news, in the mode the phone's screen is in. `conversation` is the voice chat's
/// current one when it is known, else the last the phone was told.
pub fn news_for(
    live: &Live,
    conversation: Option<&str>,
    owed: &Owed,
    about: Option<String>,
    now: i64,
) -> Reply {
    let mut r = Reply::from_text(
        owed.id.clone(),
        None,
        live.presence_mode,
        owed.text(),
        conversation
            .map(str::to_owned)
            .unwrap_or_else(|| live.conversation.clone()),
        now,
    );
    r.about = about;
    r
}

// ---- whose answer ----------------------------------------------------------------------

/// Who an exchange in the voice chat is for.
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    /// The phone started it: its answer is that turn's reply.
    Phone,
    /// Nobody asked: a declared wait finished, the attention watch noted something, or another
    /// chat wrote to this one. What the chat says goes up as news, about that chat if one is named.
    News { about: Option<String> },
    /// Typed in Helm's window, or the app's own note: nothing goes up.
    Silent,
}

/// A user message of the exchange: how it entered the chat, and what it said.
#[derive(Clone, Debug, PartialEq)]
pub struct Starter {
    pub origin: SubmissionOrigin,
    pub text: String,
}

const WAIT_PREFIX: &str = "[Wait finished]";
const OTHER_CHAT_PREFIX: &str = "[Message from another chat: \"";

fn is_news(s: &Starter) -> bool {
    let t = s.text.trim_start();
    match s.origin {
        SubmissionOrigin::OtherChat => true,
        SubmissionOrigin::App => t.starts_with(WAIT_PREFIX) || t.starts_with(NOTE_PREFIX),
        _ => false,
    }
}

/// The title of the chat a piece of news is about, when it names one.
fn about_of(text: &str, title_of: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix(OTHER_CHAT_PREFIX) {
        // `[Message from another chat: "<title>" (<id>)]`: the title may hold quotes itself.
        let head = rest.lines().next().unwrap_or_default();
        return head
            .rsplit_once("\" (")
            .map(|(t, _)| t.to_owned())
            .filter(|t| !t.is_empty());
    }
    if let Some(rest) = text.strip_prefix(WAIT_PREFIX) {
        // `Wait <id> on chat <chat id>: ...`; waits on a file, process or pull request name none.
        let id = rest.split_once(" on chat ")?.1.split(':').next()?.trim();
        return title_of(id).filter(|t| !t.trim().is_empty());
    }
    if text.starts_with(NOTE_PREFIX) {
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with(NOTE_PREFIX))
            .collect();
        if let [only] = lines.as_slice() {
            // `[Attention] <title>: <reason>`; the note never leaves a ": " in a title.
            let body = only[NOTE_PREFIX.len()..].trim_start();
            return body.split_once(": ").map(|(t, _)| t.to_owned());
        }
    }
    None
}

/// Who the exchange is for, from the messages that began it. A message from the phone wins over
/// everything; otherwise the latest piece of news names what the answer is about. `fallback` is
/// for an exchange whose messages are not in the transcript yet.
pub fn route_of(
    starters: &[Starter],
    fallback: &Route,
    title_of: &dyn Fn(&str) -> Option<String>,
) -> Route {
    if starters.iter().any(|s| s.origin == SubmissionOrigin::Voice) {
        return Route::Phone;
    }
    match starters.iter().rev().find(|s| is_news(s)) {
        Some(s) => Route::News {
            about: about_of(&s.text, title_of),
        },
        None if starters.is_empty() => fallback.clone(),
        None => Route::Silent,
    }
}

/// The messages of the chat after transcript row `after`, with how each entered it (blocking).
pub fn starters(db: &Database, chat: &str, after: i64) -> Vec<Starter> {
    db.snapshot(|tx| {
        let mut st = tx.prepare(
            "SELECT COALESCE(sm.origin, 'app'), m.text FROM messages m
             JOIN submissions s ON m.source_key = 'u:' || s.id AND s.chat_id = m.chat_id
             LEFT JOIN submission_meta sm ON sm.submission_id = s.id
             WHERE m.chat_id = ?1 AND m.id > ?2 AND m.role = 'user' ORDER BY m.id",
        )?;
        let rows = st
            .query_map(rusqlite::params![chat, after], |r| {
                Ok(Starter {
                    origin: SubmissionOrigin::parse(&r.get::<_, String>(0)?),
                    text: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok::<_, rusqlite::Error>(rows)
    })
    .unwrap_or_default()
}

/// The last transcript row that ended a turn, or 0: a turn that starts after it begins an
/// exchange nobody on the phone is waiting on (blocking).
pub fn last_answer_row(db: &Database, chat: &str) -> i64 {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM messages WHERE chat_id = ?1 AND role = 'assistant'
             AND active = 1 AND json_valid(details)
             AND json_extract(details, '$.stopReason') = 'end_turn'",
            [chat],
            |r| r.get(0),
        )
    })
    .unwrap_or(0)
}

/// What becomes of an answer owed.
#[derive(Clone, Debug, PartialEq)]
pub enum Delivery {
    /// As the reply to the phone's turn.
    Phone,
    /// As news.
    News(Option<String>),
    /// Not at all.
    Skip,
}

/// An answer that is only `[quiet]`, in any case and spacing: the chat's way of saying the news
/// was not worth telling.
pub fn is_quiet(text: &str) -> bool {
    text.split_whitespace()
        .collect::<String>()
        .eq_ignore_ascii_case("[quiet]")
}

/// Where an answer goes. News says nothing for a turn that gave nothing to say, and nothing for
/// `[quiet]`; a turn blocked on a prompt only Helm's window can answer is still worth telling.
pub fn decide(route: &Route, owed: &Owed) -> Delivery {
    match route {
        Route::Phone => Delivery::Phone,
        Route::Silent => Delivery::Skip,
        Route::News { about } => match &owed.what {
            What::Answer(t) if !is_quiet(t) => Delivery::News(about.clone()),
            What::Waiting => Delivery::News(about.clone()),
            _ => Delivery::Skip,
        },
    }
}

/// What the mirror sends with: the line, the things queued to be shown, and the claim on them.
pub struct Outbox {
    up: Arc<dyn VoiceUpstream>,
    shown: Arc<Shown>,
    chat: String,
    /// The things taken from the queue for the answer being sent, by its reply id. They stay
    /// claimed until the answer is confirmed sent, so a resend carries the same ones.
    claimed: Option<(String, Vec<Attachment>)>,
}

impl Outbox {
    pub fn new(up: Arc<dyn VoiceUpstream>, shown: Arc<Shown>, chat: String) -> Self {
        Outbox {
            up,
            shown,
            chat,
            claimed: None,
        }
    }

    /// The working line, for the phone's own turn only.
    pub fn status(&self, route: &Route, live: &Live, text: &str) {
        if *route == Route::Phone {
            if let Some(turn) = &live.turn {
                self.up.status(turn, text);
            }
        }
    }

    /// Send one answer owed, with what was queued to be shown just before it. `true` when it is
    /// dealt with: sent, or deliberately not sent (the queue is then emptied). `false` leaves it
    /// owed, and the next try sends the same things again.
    pub fn send(
        &mut self,
        route: &Route,
        live: &Live,
        conversation: Option<&str>,
        owed: &Owed,
        now: i64,
    ) -> bool {
        let mut reply = match decide(route, owed) {
            Delivery::Skip => {
                self.shown.clear(&self.chat);
                self.claimed = None;
                return true;
            }
            Delivery::Phone => reply_for(live, owed, now),
            Delivery::News(about) => news_for(live, conversation, owed, about, now),
        };
        if self.claimed.as_ref().is_none_or(|(id, _)| *id != owed.id) {
            self.claimed = Some((owed.id.clone(), self.shown.take(&self.chat)));
        }
        let things = self
            .claimed
            .as_ref()
            .map(|(_, t)| t.as_slice())
            .unwrap_or_default();
        for (i, a) in things.iter().enumerate() {
            if !self.up.attachment(&a.frame(&owed.id, i + 1)) {
                return false;
            }
        }
        reply.attachments = things.len() as u8;
        let sent = self.up.reply(&reply);
        if sent {
            self.claimed = None;
        }
        sent
    }
}

/// Watch a chat until the exchange it is in has ended. `fallback` says who an exchange is for
/// while its messages are not in the transcript yet.
pub async fn mirror(
    rt: ChatRuntime,
    db: Arc<Database>,
    mut out: Outbox,
    baseline: i64,
    live: Arc<Mutex<Live>>,
    tag_path: PathBuf,
    fallback: Route,
) {
    let chat = out.chat.clone();
    let mut m = Mirror::new(baseline, helm_core::now_ms());
    while !m.done {
        tokio::time::sleep(POLL).await;
        let (rt2, db2, chat2) = (rt.clone(), db.clone(), chat.clone());
        let (mark, base, fb) = (m.baseline, m.route_base(), fallback.clone());
        // Nothing is ever approved by voice: a prompt that is not a plain question blocks.
        let Ok((s, route)) = tokio::task::spawn_blocking(move || {
            let s = snapshot(&rt2, &db2, &chat2, mark, false);
            let began = starters(&db2, &chat2, base);
            let route = route_of(&began, &fb, &|id| title_of(&db2, id));
            (s, route)
        })
        .await
        else {
            return;
        };
        for item in m.step(&s, helm_core::now_ms()) {
            let now = helm_core::now_ms();
            let state = live.lock().unwrap_or_else(|p| p.into_inner()).clone();
            match item {
                Up::Status(t) => out.status(&route, &state, &t),
                Up::Final(owed) => {
                    let conversation = voice::load_tag(&tag_path)
                        .filter(|t| t.chat_id == chat)
                        .map(|t| t.conversation);
                    let sent = out.send(&route, &state, conversation.as_deref(), &owed, now);
                    m.ack(&owed.id, sent);
                }
            }
        }
    }
}

// ---- in --------------------------------------------------------------------------------

/// Everything a delivery needs, cloned into the blocking pool.
#[derive(Clone)]
pub struct Ctx {
    pub rt: ChatRuntime,
    pub db: Arc<Database>,
    pub ops: Arc<dyn OpInvoker>,
    pub handle: tokio::runtime::Handle,
    pub tag_path: PathBuf,
    /// The folder a new voice chat starts in.
    pub folder: String,
    pub file: voice::PolicyFile,
    pub policy: TagPolicy,
}

/// What a delivery did.
#[derive(Clone, Debug, PartialEq)]
pub enum Placed {
    /// Stored as a voice-origin submission.
    Sent {
        chat: String,
        baseline: i64,
        conversation: String,
    },
    /// This turn was already stored (a resend) and its turn is over: nothing more to do.
    Already,
}

/// Put one turn into its chat (blocking). `notes` are bracketed lines for the agent that go in
/// front of the message (replies the user did not hear). `Err` leaves the turn unacknowledged at
/// the hub.
pub fn place(c: &Ctx, turn: &VoiceTurn, notes: &[String], now: i64) -> Result<Placed, String> {
    let sid = voice::submission_id(&turn.id);
    // Already stored (a resend): its chat, whether a turn runs there now, and the row the turn
    // starts after (its user row, or the end of the chat while it is on its way).
    let stored: Option<(String, Option<String>, Option<i64>)> =
        c.db.snapshot(|tx| {
            tx.query_row(
                "SELECT s.chat_id, c.status,
                        (SELECT MAX(m.id) FROM messages m
                         WHERE m.chat_id = s.chat_id AND m.source_key = 'u:' || s.id)
                 FROM submissions s LEFT JOIN chats c ON c.id = s.chat_id WHERE s.id = ?1",
                [&sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        })
        .ok()
        .flatten();
    if let Some((chat, status, row)) = stored {
        // While its turn runs, watch it: after a restart between storing it and the hub's
        // acknowledgement, nothing else would tell the phone how that turn goes.
        let busy = status.is_some_and(|s| ChatStatus::parse(&s).is_busy());
        return Ok(if busy {
            let baseline = row.unwrap_or_else(|| max_row(&c.db, &chat));
            let conversation = voice::load_tag(&c.tag_path)
                .filter(|t| t.chat_id == chat)
                .map(|t| t.conversation)
                .unwrap_or_default();
            Placed::Sent {
                chat,
                baseline,
                conversation,
            }
        } else {
            Placed::Already
        });
    }
    let (model, effort) = c.file.new_chat();
    let port = RealPort {
        db: &c.db,
        rt: &c.rt,
        ops: &*c.ops,
        handle: &c.handle,
        folder: &c.folder,
        label: "voice",
        new_chat_model: model.map(str::to_owned),
        new_chat_effort: effort.map(str::to_owned),
    };
    let (mut tag, _created) = voice::active_chat(&c.tag_path, &port, &c.policy, now)?;
    let baseline = max_row(&c.db, &tag.chat_id);
    let (said, _) = cut_chars(&turn.text, voice::MAX_TEXT_CHARS);
    let text = voice::delivered_text(
        !tag.primed,
        Mode::parse(&turn.mode),
        turn.continues.is_some(),
        turn.age_minutes,
        notes,
        &said,
    );
    let started = std::time::Instant::now();
    loop {
        let r = host_send(
            &c.rt,
            HostSend {
                chat_id: &tag.chat_id,
                submission_id: &sid,
                text: &text,
                origin: VOICE_ORIGIN,
            },
        );
        match r {
            Ok(_) => break,
            Err(e)
                if matches!(e.code.as_str(), "busy" | "capacity")
                    && started.elapsed() < BUSY_RETRY =>
            {
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => return Err(e.message),
        }
    }
    if !tag.primed {
        tag.primed = true;
        if voice::save_tag(&c.tag_path, Some(&tag)).is_err() {
            // The next message repeats the primer, which does no harm.
            log::warn!("relay: the voice chat tag could not be updated");
        }
    }
    Ok(Placed::Sent {
        chat: tag.chat_id,
        baseline,
        conversation: tag.conversation,
    })
}

// ---- the sink --------------------------------------------------------------------------

/// The app's voice sink. The executor's wiring gives it the chat runtime and database; the op
/// dispatcher ([`ChatVoice::attach_ops`]) and the line ([`ChatVoice::attach_line`]) arrive at
/// start.
pub struct ChatVoice {
    exec: Arc<AppExecutor>,
    ops: OnceLock<Arc<dyn OpInvoker>>,
    up: OnceLock<Arc<dyn VoiceUpstream>>,
    profile_dir: PathBuf,
    serial: tokio::sync::Mutex<()>,
    live: Arc<Mutex<Live>>,
    watching: Mutex<Option<(String, tokio::task::JoinHandle<()>)>>,
    /// What `app_voice_show` queued for the next answer.
    shown: Arc<Shown>,
    /// The voice chat's id as the tag file last said, and when it was read; the event tap asks on
    /// every busy chat, so the file is read at most every [`TAG_RECHECK`].
    tagged: Mutex<(Option<Instant>, Option<String>)>,
}

/// How long the voice chat's id is trusted before the tag file is read again.
const TAG_RECHECK: Duration = Duration::from_secs(5);

/// What delivering a note to the voice chat came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Noted {
    Sent,
    /// There is no voice chat to tell (none yet, or it has expired).
    Skipped,
    Failed(String),
}

/// Put a note into the voice chat as a message from the app (blocking). `None` when no voice
/// chat holds the tag. A note is not a turn from the phone, so the idle time does not move.
pub fn place_note(
    c: &Ctx,
    submission_id: &str,
    text: &str,
    now: i64,
) -> Result<Option<(String, i64)>, String> {
    let Some(tag) = voice::current(&c.tag_path, &FactsOnly(&c.db), &c.policy, now) else {
        return Ok(None);
    };
    let baseline = max_row(&c.db, &tag.chat_id);
    host_send(
        &c.rt,
        HostSend {
            chat_id: &tag.chat_id,
            submission_id,
            text,
            origin: SubmissionOrigin::App,
        },
    )
    .map_err(|e| e.message)?;
    Ok(Some((tag.chat_id, baseline)))
}

impl ChatVoice {
    pub fn new(exec: Arc<AppExecutor>, profile_dir: PathBuf) -> Self {
        ChatVoice {
            exec,
            ops: OnceLock::new(),
            up: OnceLock::new(),
            profile_dir,
            serial: tokio::sync::Mutex::new(()),
            live: Arc::default(),
            watching: Mutex::new(None),
            shown: Arc::default(),
            tagged: Mutex::new((None, None)),
        }
    }

    /// The queue `app_voice_show` fills.
    pub fn shown(&self) -> Arc<Shown> {
        self.shown.clone()
    }

    fn remember_chat(&self, chat: &str) {
        *self.tagged.lock().unwrap_or_else(|p| p.into_inner()) =
            (Some(Instant::now()), Some(chat.to_owned()));
    }

    /// The chat the tag file names, expired or not: news for it is still mirrored until the next
    /// turn from the phone starts another.
    fn tagged_chat(&self) -> Option<String> {
        let mut t = self.tagged.lock().unwrap_or_else(|p| p.into_inner());
        if t.0.is_none_or(|at| at.elapsed() > TAG_RECHECK) {
            *t = (
                Some(Instant::now()),
                voice::load_tag(&voice::tag_path(&self.profile_dir)).map(|g| g.chat_id),
            );
        }
        t.1.clone()
    }

    /// Whether `chat` is the one the voice screen talks to.
    pub fn is_voice_chat(&self, chat: &str) -> bool {
        self.tagged_chat().as_deref() == Some(chat)
    }

    /// What the attention watch wakes the chat for, as Settings last saved it.
    pub fn watch_level(&self) -> WatchLevel {
        voice::load_file(&voice::policy_path(&self.profile_dir)).watch
    }

    /// The voice chat while its tag holds (not idle, old, full or gone), else `None`.
    pub async fn active_chat(&self, now: i64) -> Option<String> {
        let db = self.exec.wiring()?.services.get::<Database>()?;
        let policy = voice::load_file(&voice::policy_path(&self.profile_dir))
            .check()
            .unwrap_or_default();
        let path = voice::tag_path(&self.profile_dir);
        tokio::task::spawn_blocking(move || {
            voice::current(&path, &FactsOnly(&db), &policy, now).map(|t| t.chat_id)
        })
        .await
        .ok()
        .flatten()
    }

    /// The voice chat became busy. If no turn of it is being mirrored (it was woken: a wait
    /// finished, a note arrived, another chat wrote to it, or the user typed in Helm), mirror this
    /// one, so its answer goes where it belongs.
    pub async fn woken(&self, chat: &str) {
        if !self.is_voice_chat(chat) {
            return;
        }
        let watched = self
            .watching
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|(id, h)| id == chat && !h.is_finished());
        if watched {
            return;
        }
        let Ok(c) = self.ctx() else { return };
        let (db, id) = (c.db.clone(), chat.to_owned());
        let Ok(baseline) = tokio::task::spawn_blocking(move || last_answer_row(&db, &id)).await
        else {
            return;
        };
        self.watch(&c, chat, baseline, Route::Silent);
    }

    /// Put a note from the attention watch into the voice chat, and mirror the turn it starts.
    pub async fn deliver_note(&self, note: &Note, now: i64) -> Noted {
        // Behind the phone's own deliveries, and never holding them up for long: a chat that is
        // busy or full fails the note, which the watch tries again after a while.
        let _turn = self.serial.lock().await;
        let c = match self.ctx() {
            Ok(c) => c,
            Err(e) => return Noted::Failed(e),
        };
        let (c2, text, sid) = (
            c.clone(),
            note.text.clone(),
            voice::submission_id(&note.key),
        );
        match tokio::task::spawn_blocking(move || place_note(&c2, &sid, &text, now)).await {
            Ok(Ok(Some((chat, baseline)))) => {
                self.remember_chat(&chat);
                self.watch(&c, &chat, baseline, Route::News { about: None });
                Noted::Sent
            }
            Ok(Ok(None)) => Noted::Skipped,
            Ok(Err(e)) => Noted::Failed(e),
            Err(_) => Noted::Failed("The delivery did not finish.".into()),
        }
    }

    pub fn attach_ops(&self, ops: Arc<dyn OpInvoker>) {
        let _ = self.ops.set(ops);
    }

    pub fn attach_line(&self, up: Arc<dyn VoiceUpstream>) {
        let _ = self.up.set(up);
    }

    /// What the phone last said about its screen, and the turn being answered.
    pub fn live(&self) -> Live {
        self.live.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn ctx(&self) -> Result<Ctx, String> {
        const STARTING: &str =
            "Helm is still starting; the message will be delivered when Helm is ready.";
        let w = self.exec.wiring().ok_or(STARTING)?;
        let ops = self.ops.get().cloned().ok_or(STARTING)?;
        let rt = w
            .services
            .get::<ChatRuntime>()
            .ok_or("Helm's chats are not available.")?;
        let db = w
            .services
            .get::<Database>()
            .ok_or("Helm's database is not available.")?;
        // Read once per delivery, so a change in Settings applies to the next message.
        let file = voice::load_file(&voice::policy_path(&self.profile_dir));
        Ok(Ctx {
            rt: (*rt).clone(),
            db,
            ops,
            handle: tokio::runtime::Handle::current(),
            tag_path: voice::tag_path(&self.profile_dir),
            folder: plain_path(&voice::folder(&self.profile_dir)),
            policy: file.check().unwrap_or_default(),
            file,
        })
    }

    /// Watch `chat` unless it is already being watched. `fallback` says who an exchange is for
    /// until its messages are in the transcript.
    fn watch(&self, c: &Ctx, chat: &str, baseline: i64, fallback: Route) {
        let Some(up) = self.up.get().cloned() else {
            return;
        };
        let mut w = self.watching.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((id, h)) = w.as_ref() {
            if id == chat && !h.is_finished() {
                return;
            }
            h.abort();
        }
        let task = c.handle.spawn(mirror(
            c.rt.clone(),
            c.db.clone(),
            Outbox::new(up, self.shown.clone(), chat.to_owned()),
            baseline,
            self.live.clone(),
            c.tag_path.clone(),
            fallback,
        ));
        *w = Some((chat.to_owned(), task));
    }
}

impl VoiceSink for ChatVoice {
    fn deliver<'a>(&'a self, turn: VoiceTurn) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async move {
            // One at a time: two turns with no tag must not start two chats.
            let _turn = self.serial.lock().await;
            let c = self.ctx()?;
            let c2 = c.clone();
            let t2 = turn.clone();
            let placed =
                tokio::task::spawn_blocking(move || place(&c2, &t2, &[], helm_core::now_ms()))
                    .await
                    .map_err(|_| "The delivery did not finish.".to_owned())??;
            let Placed::Sent {
                chat,
                baseline,
                conversation,
            } = placed
            else {
                return Ok(());
            };
            {
                let mut l = self.live.lock().unwrap_or_else(|p| p.into_inner());
                l.turn = Some(turn.id);
                l.mode = Mode::parse(&turn.mode);
                l.conversation = conversation;
            }
            self.remember_chat(&chat);
            self.watch(&c, &chat, baseline, Route::Phone);
            Ok(())
        })
    }

    fn stop<'a>(&'a self, turn: String) -> BoxFut<'a, ()> {
        Box::pin(async move {
            // Only the turn being answered: an older one has nothing running to stop.
            if self.live().turn.as_deref() != Some(turn.as_str()) {
                return;
            }
            let Ok(c) = self.ctx() else { return };
            let Some(tag) = voice::load_tag(&c.tag_path) else {
                return;
            };
            if let Err(e) = c
                .ops
                .invoke("chat_stop", json!({ "chatId": tag.chat_id }))
                .await
            {
                log::warn!("relay: stopping a voice turn failed: {}", e.message);
            }
        })
    }

    fn presence(&self, live: bool, mode: &str) {
        let mut l = self.live.lock().unwrap_or_else(|p| p.into_inner());
        l.presence_live = live;
        l.presence_mode = Mode::parse(mode);
    }

    fn fresh<'a>(&'a self) -> BoxFut<'a, ()> {
        Box::pin(async move {
            if voice::save_tag(&voice::tag_path(&self.profile_dir), None).is_err() {
                log::warn!("relay: the voice chat tag could not be removed");
            }
            *self.tagged.lock().unwrap_or_else(|p| p.into_inner()) = (None, None);
        })
    }
}

// ---- settings --------------------------------------------------------------------------

/// The active voice chat, as Settings shows it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceChatView {
    pub id: String,
    pub title: String,
    pub started_at: i64,
    /// Tokens used since the chat was tagged (what the token limit counts).
    pub tokens_used: u64,
}

/// Settings → Hub's Voice block: the chat the phone talks to, and the limits.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceView {
    /// `None` when no chat holds the tag (or it has expired): the next turn starts one.
    pub chat: Option<VoiceChatView>,
    /// Blank: the same model as any other new chat.
    pub model: String,
    /// Blank: the same effort as any other new chat.
    pub effort: String,
    pub idle_minutes: i64,
    pub max_age_hours: i64,
    pub max_tokens: u64,
    pub watch: WatchLevel,
}

pub fn voice_view(db: &Database, profile_dir: &std::path::Path, now: i64) -> VoiceView {
    let file = voice::load_file(&voice::policy_path(profile_dir));
    let policy = file.check().unwrap_or_default();
    let chat = voice::load_tag(&voice::tag_path(profile_dir)).and_then(|tag| {
        let facts = chat_facts(db, &tag.chat_id);
        if voice::expired(&tag, facts, &policy, now).is_some() {
            return None;
        }
        let title: Option<String> = db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT title FROM chats WHERE id = ?1",
                    [&tag.chat_id],
                    |r| r.get(0),
                )
                .optional()
            })
            .ok()
            .flatten()
            .flatten();
        Some(VoiceChatView {
            title: title.unwrap_or_else(|| "Voice".into()),
            started_at: tag.started_at,
            tokens_used: facts
                .map(|f| f.tokens.saturating_sub(tag.tokens_at_start))
                .unwrap_or(0),
            id: tag.chat_id,
        })
    });
    VoiceView {
        chat,
        model: file.model,
        effort: file.effort,
        idle_minutes: file.idle_minutes,
        max_age_hours: file.max_age_hours,
        max_tokens: file.max_tokens,
        watch: file.watch,
    }
}

/// A port over facts alone, for reading the tag without being able to start a chat.
pub struct FactsOnly<'a>(pub &'a Database);

impl ChatPort for FactsOnly<'_> {
    fn facts(&self, chat_id: &str) -> Option<crate::telegram::ChatFacts> {
        chat_facts(self.0, chat_id)
    }
    fn create(&self, _title: &str) -> Result<String, String> {
        Err("No voice chat is started from here.".into())
    }
}
