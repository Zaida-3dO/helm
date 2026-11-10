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
//! and the answer that follows is the one for it. A turn the mirror was not started for (news the
//! chat is woken with) is not routed here yet; [`Live`] is where that will hang.

use crate::client::{BoxFut, LineHandle, VoiceSink, VoiceTurn};
use crate::exec::AppExecutor;
use crate::sink::{chat_facts, max_row, plain_path, snapshot, RealPort, Snap, STATUS_GAP_MS};
use crate::telegram::{cut_chars, ChatPort};
use crate::voice::{self, Mode, Reply, TagPolicy, WatchLevel, VOICE_ORIGIN};
use helm_chats::submit::{host_send, HostSend};
use helm_chats::types::ChatStatus;
use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_hosttools::OpInvoker;
use rusqlite::OptionalExtension;
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

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
}

impl VoiceUpstream for LineHandle {
    fn status(&self, turn: &str, text: &str) {
        self.voice_status(turn, text)
    }
    fn reply(&self, reply: &Reply) -> bool {
        self.voice_reply(reply)
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

/// Watch a chat until the turn it was started for has ended.
pub async fn mirror(
    rt: ChatRuntime,
    db: Arc<Database>,
    up: Arc<dyn VoiceUpstream>,
    chat: String,
    baseline: i64,
    live: Arc<Mutex<Live>>,
) {
    let mut m = Mirror::new(baseline, helm_core::now_ms());
    while !m.done {
        tokio::time::sleep(POLL).await;
        let (rt2, db2, chat2, base) = (rt.clone(), db.clone(), chat.clone(), m.baseline);
        // Nothing is ever approved by voice: a prompt that is not a plain question blocks.
        let Ok(s) =
            tokio::task::spawn_blocking(move || snapshot(&rt2, &db2, &chat2, base, false)).await
        else {
            return;
        };
        for out in m.step(&s, helm_core::now_ms()) {
            let now = helm_core::now_ms();
            let state = live.lock().unwrap_or_else(|p| p.into_inner()).clone();
            match out {
                Up::Status(t) => {
                    if let Some(turn) = &state.turn {
                        up.status(turn, &t);
                    }
                }
                Up::Final(owed) => {
                    let ok = up.reply(&reply_for(&state, &owed, now));
                    m.ack(&owed.id, ok);
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

    /// Watch `chat` unless it is already being watched.
    fn watch(&self, c: &Ctx, chat: &str, baseline: i64) {
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
            up,
            chat.to_owned(),
            baseline,
            self.live.clone(),
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
            self.watch(&c, &chat, baseline);
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
