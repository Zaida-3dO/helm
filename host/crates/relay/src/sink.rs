//! Telegram into chats, and the mirror back out.
//!
//! **In.** [`ChatSink`] takes each message the hub forwards and puts it into the active Telegram
//! chat ([`crate::telegram::active_chat`]), starting `Telegram <bot> <date>` when there is none:
//!
//! - It is stored through the chats crate's host send with origin `telegram`, so it never counts
//!   as consent and never trips the user's wait limit (that limit turns a waiting steer into an
//!   interrupt, and applies only to sends typed in Helm's own window).
//! - It never aborts: on a busy chat it is handed to Claude at once and read at the next tool
//!   boundary, whatever the user's send mode is.
//! - Its submission id is fixed per hub message id, so a message delivered twice is stored once.
//! - It is acknowledged to the hub only once it is stored. A chat that is stopping or full is
//!   retried here for a short while first, because the hub resends an unacknowledged message
//!   only on the next line.
//! - While the chat has a question waiting (an agent's multiple-choice or free-text question),
//!   the reply answers it: a number picks that option, and the phone is told what was answered.
//!   Approvals, plans and trust prompts are never answered from Telegram.
//! - A message the hub sends again (no acknowledgement reached it) is handled once: one already
//!   stored as a prompt is not stored again or taken as an answer, and a reply that answered a
//!   question is remembered by its hub id in the profile, so a resend after a restart is not
//!   taken as a new prompt.
//! - `/stop` stops the tagged chat's turn.
//!
//! **Out.** While a Telegram turn runs, a mirror task watches the chat (its stored status, the
//! live tools and the transcript) and sends ONE status line up the line, only when it changes and
//! at most every [`STATUS_GAP_MS`]; the hub edits a single Telegram message in place. A turn's
//! answer goes up as soon as its end-of-turn row is in the transcript (the hub then deletes the
//! status) — waiting only for the chat to look idle is not enough, since a message the user
//! queued opens the next turn before that happens. An answer the line will not take is offered
//! again, and the chat is told inside its transcript that the user never saw it, so the agent can
//! send it again. A question goes up as a new message with numbered options; an approval, plan or
//! trust prompt goes up once as a note that it waits in Helm's window. The mirror is
//! harness-level: the agent never has to remember to reply.

use crate::client::{BoxFut, LineHandle, TelegramMessage, TelegramSink};
use crate::exec::AppExecutor;
use crate::telegram::{self, ChatFacts, ChatPort, TagPolicy, TELEGRAM_ORIGIN};
use helm_chats::submit::{
    host_send, host_send_with_attachments, HostFile, HostImage, HostSend, FILE_REFUSED,
    IMAGE_REFUSED,
};
use helm_chats::types::{ChatStatus, InteractionAttention, InteractionMethod};
use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_hosttools::OpInvoker;
use rusqlite::OptionalExtension;
use serde_json::json;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The fewest milliseconds between two status updates sent up the line.
pub const STATUS_GAP_MS: i64 = 2_500;
/// How often the mirror looks at the chat.
const POLL: Duration = Duration::from_millis(500);
/// A message that has not started a turn by now gets a final note instead of silence.
pub const START_WAIT_MS: i64 = 120_000;
/// How long a delivery retries a chat that is stopping or full before leaving it to the hub.
const BUSY_RETRY: Duration = Duration::from_secs(20);

/// Where the status and final answer go: the line in the app, a recorder in tests.
pub trait Upstream: Send + Sync {
    fn status(&self, text: &str);
    /// The turn's answer, as a new message. `false` when it was not sent (the line is down);
    /// the mirror then tries it again and tells the chat, so the agent can act on it.
    fn final_answer(&self, text: &str) -> bool;
    /// A note to the phone that leaves the live status alone.
    fn notify(&self, _text: &str) {}
}

impl Upstream for LineHandle {
    fn status(&self, text: &str) {
        self.telegram_status(text)
    }
    fn final_answer(&self, text: &str) -> bool {
        self.telegram_final(text)
    }
    fn notify(&self, text: &str) {
        self.telegram_notify(text);
    }
}

/// What the mirror sees of the chat at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snap {
    /// A turn is starting, running or stopping.
    pub busy: bool,
    /// Tools running now, by name.
    pub running: Vec<String>,
    /// The last few tools of this turn, oldest first.
    pub recent: Vec<String>,
    /// A question waiting for the user: its request id and the rendered message.
    pub question: Option<(String, String)>,
    /// Something only Helm's window may answer (an approval, a plan, a trust prompt): its
    /// request id and the note for the phone.
    pub blocked: Option<(String, String)>,
    /// The last assistant text since the turn's baseline.
    pub final_text: Option<String>,
    /// The last transcript row since the baseline that closed a turn (the end-of-turn stop
    /// reason): its id and text. The chat stays busy while a message the user queued opens the
    /// next turn, so this — not an idle sample — is how the mirror knows an answer is ready.
    pub turn_done: Option<(i64, String)>,
    /// The highest transcript row id now.
    pub max_id: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Up {
    Status(String),
    Final(String),
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
    asked: HashSet<String>,
    /// The transcript row of the last answer that reached the phone.
    sent_through: i64,
    /// An answer that had not been confirmed sent yet: its transcript row (if it is a turn's
    /// answer) and its text. The mirror offers it again on every step until [`Self::ack`] says
    /// it went up.
    pending: Option<(Option<i64>, String)>,
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
            asked: HashSet::new(),
            sent_through: 0,
            pending: None,
            answered: false,
            closing: false,
            done: false,
        }
    }

    /// Report whether an [`Up::Final`] reached the phone. One that did marks its row delivered
    /// (it is never sent up twice); one that did not stays offered. When the exchange is closed
    /// out, the mirror finishes with the send that carried the last word, not the sample that
    /// found the chat idle.
    pub fn ack(&mut self, text: &str, ok: bool) {
        if self.pending.as_ref().is_none_or(|p| p.1 != text) {
            return;
        }
        if ok {
            if let Some(p) = self.pending.take() {
                if let Some(id) = p.0 {
                    self.sent_through = self.sent_through.max(id);
                }
                self.answered = true;
            }
            if self.closing {
                self.done = true;
            }
        }
    }

    pub fn step(&mut self, s: &Snap, now: i64) -> Vec<Up> {
        let mut out = vec![];
        if self.done {
            return out;
        }
        if let Some(p) = self.pending.clone() {
            // A previous answer is owed to the phone: offer it again before anything new.
            out.push(Up::Final(p.1));
            return out;
        }
        if s.busy {
            self.started = true;
            self.idle_seen = 0;
            if let Some((id, text)) = &s.turn_done {
                if *id > self.sent_through {
                    // The turn's answer exists. Hand it up now instead of waiting for an idle
                    // sample: a message the user queued is read at the turn's first boundary
                    // and opens the next one, so the chat may never show idle between the
                    // turns and an answer held for a settled sample would never go out.
                    let t = telegram::render_final(Some(text.as_str()));
                    self.pending = Some((Some(*id), t.clone()));
                    out.push(Up::Final(t));
                    return out;
                }
            }
            if let Some((id, text)) = s.question.as_ref().or(s.blocked.as_ref()) {
                if self.asked.insert(id.clone()) {
                    // A new message, so the phone notifies; the hub removes the status.
                    out.push(Up::Final(text.clone()));
                    self.last_status = None;
                    self.last_at = Some(now);
                }
                return out;
            }
            let st = telegram::render_status(&s.running, &s.recent);
            let due = self.last_at.is_none_or(|t| now - t >= STATUS_GAP_MS);
            if self.last_status.as_ref() != Some(&st) && due {
                out.push(Up::Status(st.clone()));
                self.last_status = Some(st);
                self.last_at = Some(now);
            }
        } else if self.started || s.final_text.is_some() {
            // Settled: seen idle twice in a row, so the last streamed row is flushed.
            self.idle_seen += 1;
            if self.idle_seen >= 2 {
                // Nothing is ever held behind the turn: an idle chat with two settled
                // samples has no more of this exchange to show.
                let due = match (&s.turn_done, &s.final_text) {
                    // Its answer went up while the chat was still busy: the exchange just
                    // closes, and says nothing a second time.
                    (Some((id, _)), _) if *id <= self.sent_through => None,
                    (Some((_, text)), _) => Some(telegram::render_final(Some(text.as_str()))),
                    (None, Some(text)) => Some(telegram::render_final(Some(text.as_str()))),
                    (None, None) if self.answered => None,
                    (None, None) => Some(telegram::render_final(None)),
                };
                match due {
                    Some(t) => {
                        self.pending = Some((s.turn_done.as_ref().map(|(id, _)| *id), t.clone()));
                        self.closing = true;
                        out.push(Up::Final(t));
                    }
                    None => self.done = true,
                }
            }
        } else if now - self.began >= START_WAIT_MS {
            // A turn that never started: say so once and finish. The message itself is saved
            // in the chat, so this note is the extra word, not the only path to the answer.
            out.push(Up::Final(
                "Helm has not started a turn for that message yet; it is saved in the Telegram chat."
                    .into(),
            ));
            self.done = true;
        }
        out
    }
}

/// The real chat port: facts from the database, creation through the `chat_create` op.
struct RealPort<'a> {
    db: &'a Database,
    rt: &'a ChatRuntime,
    ops: &'a dyn OpInvoker,
    handle: &'a tokio::runtime::Handle,
    folder: &'a str,
    /// The model and effort saved for new Telegram chats; `None` passes neither to the op, so
    /// the chat opens on the new-chat default as before.
    new_chat_model: Option<String>,
    new_chat_effort: Option<String>,
}

/// Whether the chat is archived and the tokens it has used; `None` when it no longer exists.
fn chat_facts(db: &Database, chat_id: &str) -> Option<ChatFacts> {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT c.archived,
                    (SELECT COALESCE(SUM(
                         COALESCE(json_extract(m.details, '$.usage.inputTokens'), 0)
                       + COALESCE(json_extract(m.details, '$.usage.outputTokens'), 0)), 0)
                     FROM messages m WHERE m.chat_id = c.id AND m.details IS NOT NULL
                       AND json_valid(m.details))
             FROM chats c WHERE c.id = ?1",
            [chat_id],
            |r| {
                Ok(ChatFacts {
                    archived: r.get(0)?,
                    tokens: r.get::<_, i64>(1)?.max(0) as u64,
                })
            },
        )
        .optional()
    })
    .ok()
    .flatten()
}

/// The active Telegram chat, as Settings shows it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramChatView {
    pub id: String,
    pub title: String,
    pub started_at: i64,
    /// Tokens used since the chat was tagged (what the token limit counts).
    pub tokens_used: u64,
}

/// Settings → Hub's Telegram section: the chat new messages go to, and the limits.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramView {
    /// `None` when no chat holds the tag (or it has expired): the next message starts one.
    pub chat: Option<TelegramChatView>,
    pub max_age_hours: i64,
    pub max_tokens: u64,
    /// Telegram alerts while the user is away ([`crate::notify`]).
    pub away_alerts: bool,
    /// Approval prompts answered with Telegram buttons ([`crate::approve`]).
    pub approvals: bool,
    /// The model a new Telegram chat opens with; `None`: the same as any other new chat.
    pub model: Option<String>,
    /// The effort a new Telegram chat opens with; `None`: the same as any other new chat.
    pub effort: Option<String>,
}

pub fn telegram_view(db: &Database, profile_dir: &std::path::Path, now: i64) -> TelegramView {
    let file = telegram::load_file(&telegram::policy_path(profile_dir));
    let policy = file.check().unwrap_or_default();
    let chat = telegram::load_tag(&telegram::tag_path(profile_dir)).and_then(|tag| {
        let facts = chat_facts(db, &tag.chat_id);
        if telegram::expired(&tag, facts, &policy, now).is_some() {
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
        Some(TelegramChatView {
            title: title.unwrap_or_else(|| "Telegram".into()),
            started_at: tag.started_at,
            tokens_used: facts
                .map(|f| f.tokens.saturating_sub(tag.tokens_at_start))
                .unwrap_or(0),
            id: tag.chat_id,
        })
    });
    let (model, effort) = file.new_chat();
    TelegramView {
        chat,
        max_age_hours: file.max_age_hours,
        max_tokens: file.max_tokens,
        away_alerts: file.away_alerts,
        approvals: file.approvals,
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
    }
}

impl RealPort<'_> {
    /// Whether a saved model can be used for a new chat now: a Claude model always, a local one
    /// when its server is set up and switched on.
    fn model_usable(&self, model: &str) -> bool {
        let Some((server, _)) = helm_chats::local::split(model) else {
            return !model.starts_with(helm_chats::local::PREFIX);
        };
        self.rt.local_models().is_none_or(|l| l.available(server))
    }
}

impl ChatPort for RealPort<'_> {
    fn facts(&self, chat_id: &str) -> Option<ChatFacts> {
        chat_facts(self.db, chat_id)
    }

    fn create(&self, title: &str) -> Result<String, String> {
        std::fs::create_dir_all(self.folder)
            .map_err(|_| "The Telegram chats' folder could not be made.".to_owned())?;
        // `chat_create` already takes a model and an effort; without them it uses the new-chat
        // default exactly as before. No board is named, so the chat goes to the default board
        // (no board has the Telegram folder as its own).
        let mut args = json!({ "cwd": self.folder });
        // A saved local model whose server is set up but switched off would be accepted here and
        // then refuse every turn, leaving the phone with no reason: it is left out, and the chat
        // opens on the app default (the effort, if any, is kept).
        let model = self.new_chat_model.as_deref().filter(|m| {
            let usable = self.model_usable(m);
            if !usable {
                log::warn!("relay: the saved Telegram model is on a local server that is not available; opening on the app default");
            }
            usable
        });
        if let Some(m) = model {
            args["model"] = json!(m);
        }
        if let Some(e) = &self.new_chat_effort {
            args["effort"] = json!(e);
        }
        let asked = self.new_chat_model.is_some() || self.new_chat_effort.is_some();
        let chat = match self.handle.block_on(self.ops.invoke("chat_create", args)) {
            Ok(chat) => chat,
            Err(e) if !asked => return Err(e.message),
            Err(e) => {
                // A refused saved model or effort — the model's local server was removed from
                // this machine, say — must not leave the channel dark: open the chat without
                // them so it answers on the app default instead.
                log::warn!("relay: refused the saved Telegram chat settings ({0}); opening on the app default: {0}", e.message);
                self.handle
                    .block_on(
                        self.ops
                            .invoke("chat_create", json!({ "cwd": self.folder })),
                    )
                    .map_err(|e| e.message)?
            }
        };
        let id = chat["id"]
            .as_str()
            .ok_or("chat_create answered no id")?
            .to_owned();
        helm_chats::titles::rename(self.rt, &id, title).map_err(|e| e.message)?;
        Ok(id)
    }
}

/// What a delivery did.
#[derive(Clone, Debug, PartialEq)]
pub enum Placed {
    /// Stored as a Telegram-origin submission.
    Sent { chat: String, baseline: i64 },
    /// Answered the chat's waiting question with `value`.
    Answered {
        chat: String,
        baseline: i64,
        value: String,
    },
    /// This hub id was already stored or already answered a question (a resend): nothing more
    /// to do.
    Already,
}

fn is_answerable(i: &helm_chats::types::Interaction) -> bool {
    i.attention == Some(InteractionAttention::Question)
        && matches!(
            i.method,
            InteractionMethod::Select | InteractionMethod::Input
        )
}

/// The question a chat is waiting on that Telegram may answer: a multiple-choice or free-text
/// question. Approvals, plans and trust prompts are never offered.
pub fn answerable(rt: &ChatRuntime, chat: &str) -> Option<helm_chats::types::Interaction> {
    rt.live_thread(chat)?
        .interactions
        .into_iter()
        .find(is_answerable)
}

/// The first waiting prompt a Telegram reply may not answer (an approval, a plan, a trust
/// prompt). With approving from Telegram on, the ones it asks with buttons are left to it.
fn blocking(
    rt: &ChatRuntime,
    chat: &str,
    approvals: bool,
) -> Option<helm_chats::types::Interaction> {
    rt.live_thread(chat)?.interactions.into_iter().find(|i| {
        i.attention.is_some() && !is_answerable(i) && !(approvals && crate::approve::askable(i))
    })
}

fn max_row(db: &Database, chat: &str) -> i64 {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM messages WHERE chat_id = ?1",
            [chat],
            |r| r.get(0),
        )
    })
    .unwrap_or(0)
}

/// Everything a delivery needs, cloned into the blocking pool.
#[derive(Clone)]
pub struct Ctx {
    pub rt: ChatRuntime,
    pub db: Arc<Database>,
    pub ops: Arc<dyn OpInvoker>,
    pub handle: tokio::runtime::Handle,
    pub tag_path: PathBuf,
    /// The folder new Telegram chats start in.
    pub folder: String,
    pub policy: TagPolicy,
    /// The model and effort saved for a new Telegram chat; `None` each: the new-chat default.
    pub new_chat_model: Option<String>,
    pub new_chat_effort: Option<String>,
    /// Hub ids that answered a question, so a resend does not answer twice (or turn into a
    /// prompt), and the file they are kept in.
    pub answered: Arc<Mutex<VecDeque<String>>>,
    pub answered_path: PathBuf,
    /// Alerts other chats sent the user since the last message ([`telegram::SentNote`]).
    pub sent_path: PathBuf,
}

/// Put one message into its chat (blocking). `Err` leaves it unacknowledged at the hub.
pub fn place(c: &Ctx, msg: &TelegramMessage, now: i64) -> Result<Placed, String> {
    let sid = telegram::submission_id(&msg.id);
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
            Placed::Sent { chat, baseline }
        } else {
            Placed::Already
        });
    }
    if c.answered
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&msg.id)
    {
        return Ok(Placed::Already);
    }
    let port = RealPort {
        db: &c.db,
        rt: &c.rt,
        ops: &*c.ops,
        handle: &c.handle,
        folder: &c.folder,
        new_chat_model: c.new_chat_model.clone(),
        new_chat_effort: c.new_chat_effort.clone(),
    };
    let (chat, _created) =
        telegram::active_chat(&c.tag_path, &port, &c.policy, msg.bot.as_deref(), now)?;
    let baseline = max_row(&c.db, &chat);
    // A picture or file is never an answer to a question: the message goes in as an ordinary one.
    // A voice note is: the hub attaches the recording, but what answers is its transcript.
    let spoken = spoken_answer(msg);
    let question = answerable(&c.rt, &chat)
        .filter(|_| spoken.is_some() || (msg.images.is_empty() && msg.files.is_empty()));
    if let Some(q) = question {
        let said = spoken.as_deref().unwrap_or(&msg.text);
        let value = match q.method {
            InteractionMethod::Select => {
                telegram::question_answer(q.options.as_deref().unwrap_or(&[]), said)
            }
            _ => said.trim().to_owned(),
        };
        c.handle
            .block_on(c.ops.invoke(
                "interaction_answer",
                json!({ "chatId": chat, "requestId": q.id, "value": value }),
            ))
            .map_err(|e| e.message)?;
        let mut a = c.answered.lock().unwrap_or_else(|p| p.into_inner());
        if telegram::remember_answered(&c.answered_path, &mut a, &msg.id).is_err() {
            // Answered all the same; only a resend after a restart could repeat it.
            log::warn!("relay: the answered Telegram message could not be recorded");
        }
        return Ok(Placed::Answered {
            chat,
            baseline,
            value,
        });
    }
    let sent = telegram::load_sent(&c.sent_path);
    let text = telegram::delivered_text(&msg.text, msg.age_minutes, &sent);
    // The pictures go in as the composer's do, checked by the same rules; other files are saved
    // where the agent can read them and attached by reference. One the chat will not take does not
    // hold the message back: the text goes without it, saying so.
    let (pictures, mut unreadable) = decode(&msg.images);
    let (files, mut unsaved) = decode(&msg.files);
    let mut with_pictures = !pictures.is_empty();
    let mut with_files = !files.is_empty();
    let started = std::time::Instant::now();
    loop {
        let mut shown = text.clone();
        if let Some(why) = unreadable.as_deref() {
            shown.push_str(&format!(
                "\n\n(A picture came with this message but could not be attached. {why})"
            ));
        }
        if let Some(why) = unsaved.as_deref() {
            shown.push_str(&format!(
                "\n\n(A file came with this message but could not be attached. {why})"
            ));
        }
        let attach: Vec<HostImage> = if with_pictures {
            pictures
                .iter()
                .map(|(name, mime, bytes)| HostImage { name, mime, bytes })
                .collect()
        } else {
            vec![]
        };
        let saved: Vec<HostFile> = if with_files {
            files
                .iter()
                .map(|(name, _, bytes)| HostFile { name, bytes })
                .collect()
        } else {
            vec![]
        };
        let r = host_send_with_attachments(
            &c.rt,
            HostSend {
                chat_id: &chat,
                submission_id: &sid,
                text: &shown,
                origin: TELEGRAM_ORIGIN,
            },
            &attach,
            &saved,
        );
        match r {
            Err(e) if e.code == IMAGE_REFUSED && with_pictures => {
                with_pictures = false;
                unreadable = Some(e.message);
            }
            Err(e) if e.code == FILE_REFUSED && with_files => {
                with_files = false;
                unsaved = Some(e.message);
            }
            Ok(_) => {
                if !sent.is_empty() && telegram::clear_sent(&c.sent_path).is_err() {
                    log::warn!("relay: the notes of alerts sent could not be cleared");
                }
                return Ok(Placed::Sent { chat, baseline });
            }
            Err(e)
                if matches!(e.code.as_str(), "busy" | "capacity")
                    && started.elapsed() < BUSY_RETRY =>
            {
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => return Err(e.message),
        }
    }
}

/// A picture or file ready for the chat: its name, type and bytes.
type Decoded = (String, String, Vec<u8>);

/// The pictures or files of a message, and why one could not be read at all.
/// The transcript of a message that is only a voice note the hub could understand: its files are
/// recordings (no pictures), and its text is `[Voice note] <transcript>` with something heard.
/// `None` for anything else, a recording that was not transcribed included.
fn spoken_answer(msg: &crate::client::TelegramMessage) -> Option<String> {
    if !msg.images.is_empty()
        || msg.files.is_empty()
        || !msg.files.iter().all(|f| f.media_type.starts_with("audio/"))
    {
        return None;
    }
    let heard = msg.text.trim().strip_prefix("[Voice note]")?.trim();
    (!heard.is_empty() && !heard.starts_with('(')).then(|| heard.to_owned())
}

fn decode(images: &[crate::client::TelegramFile]) -> (Vec<Decoded>, Option<String>) {
    use base64::Engine as _;
    let mut out = vec![];
    let mut why = None;
    for i in images {
        match base64::engine::general_purpose::STANDARD.decode(i.data.as_bytes()) {
            Ok(bytes) => out.push((i.name.clone(), i.media_type.clone(), bytes)),
            Err(_) => why = Some("It arrived damaged.".to_owned()),
        }
    }
    (out, why)
}

/// The chat as the mirror sees it (blocking).
pub fn snapshot(
    rt: &ChatRuntime,
    db: &Database,
    chat: &str,
    baseline: i64,
    approvals: bool,
) -> Snap {
    let row = db
        .snapshot(|tx| {
            let status: Option<String> = tx
                .query_row("SELECT status FROM chats WHERE id = ?1", [chat], |r| {
                    r.get(0)
                })
                .optional()?;
            // The few words stored with the call when it has them, otherwise the bare name.
            let mut st = tx.prepare(
                "SELECT CASE WHEN json_valid(details) THEN \
                 COALESCE(NULLIF(json_extract(details, '$.toolSummary'), ''), tool_name) \
                 ELSE tool_name END \
                 FROM messages WHERE chat_id = ?1 AND id > ?2 \
                 AND tool_name IS NOT NULL ORDER BY id DESC LIMIT 3",
            )?;
            let mut recent: Vec<String> = st
                .query_map(rusqlite::params![chat, baseline], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            recent.reverse();
            let final_text: Option<String> = tx
                .query_row(
                    "SELECT text FROM messages WHERE chat_id = ?1 AND id > ?2 AND role = 'assistant'
                     AND active = 1 AND text != '' ORDER BY id DESC LIMIT 1",
                    rusqlite::params![chat, baseline],
                    |r| r.get(0),
                )
                .optional()?;
            // The last row that ended a turn: its stop reason says the exchange's answer is
            // complete, which is knowable while the chat is still busy (a queued message opens
            // the next turn before an idle sample can come).
            let turn_done: Option<(i64, String)> = tx
                .query_row(
                    "SELECT id, text FROM messages WHERE chat_id = ?1 AND id > ?2 AND role = 'assistant'
                     AND active = 1 AND text != '' AND json_valid(details)
                     AND json_extract(details, '$.stopReason') = 'end_turn' ORDER BY id DESC LIMIT 1",
                    rusqlite::params![chat, baseline],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let max_id: i64 = tx.query_row(
                "SELECT COALESCE(MAX(id), 0) FROM messages WHERE chat_id = ?1",
                [chat],
                |r| r.get(0),
            )?;
            Ok::<_, rusqlite::Error>((status, recent, final_text, turn_done, max_id))
        })
        .unwrap_or_default();
    let (status, recent, final_text, turn_done, max_id) = row;
    let busy = status
        .map(|s| {
            matches!(
                ChatStatus::parse(&s),
                ChatStatus::Starting | ChatStatus::Running | ChatStatus::Stopping
            )
        })
        .unwrap_or(false);
    let live = rt.live_thread(chat);
    // The same few words the dock shows, so the phone and the app describe a call alike.
    let running = live
        .as_ref()
        .map(|t| {
            t.live_tools
                .iter()
                .map(|l| {
                    l.summary
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| l.name.clone())
                })
                .collect()
        })
        .unwrap_or_default();
    let question = answerable(rt, chat).map(|q| {
        let text = match q.method {
            InteractionMethod::Select => telegram::render_question(
                q.title.as_deref(),
                q.message.as_deref(),
                q.options.as_deref().unwrap_or(&[]),
            ),
            _ => telegram::render_question(q.title.as_deref(), q.message.as_deref(), &[]),
        };
        (q.id, text)
    });
    let blocked = blocking(rt, chat, approvals).map(|b| {
        (
            b.id,
            telegram::render_blocked(b.title.as_deref(), b.message.as_deref()),
        )
    });
    Snap {
        busy,
        running,
        recent,
        question,
        blocked,
        final_text,
        turn_done,
        max_id,
    }
}

/// Watch a chat until its Telegram turn has ended.
pub async fn mirror(
    rt: ChatRuntime,
    db: Arc<Database>,
    up: Arc<dyn Upstream>,
    chat: String,
    baseline: i64,
    policy: PathBuf,
) {
    let mut m = Mirror::new(baseline, helm_core::now_ms());
    // Whether the chat has been told its answer could not reach the phone. Once per exchange:
    // the message has a fixed id, so it is stored once however often failures come after it.
    let mut told = false;
    while !m.done {
        tokio::time::sleep(POLL).await;
        let (rt2, db2, chat2, base) = (rt.clone(), db.clone(), chat.clone(), m.baseline);
        let policy = policy.clone();
        let Ok(s) = tokio::task::spawn_blocking(move || {
            let approvals = telegram::load_approvals(&policy);
            snapshot(&rt2, &db2, &chat2, base, approvals)
        })
        .await
        else {
            return;
        };
        for out in m.step(&s, helm_core::now_ms()) {
            match out {
                Up::Status(t) => up.status(&t),
                Up::Final(t) => {
                    let ok = up.final_answer(&t);
                    m.ack(&t, ok);
                    if ok || told {
                        continue;
                    }
                    // The user cannot see an answer the line would not take. Say so inside the
                    // chat, where the agent can act on it: it can send the answer again, and
                    // until it does the user simply has no reply.
                    told = true;
                    let note = "[Helm] Your last reply did not reach the user's phone: the Telegram line was down. Only the final message of each turn reaches them. While the user has not answered, send your reply again as your final message.";
                    let sid = telegram::submission_id(&format!("undelivered/{chat}/{baseline}"));
                    let _ = host_send(
                        &rt,
                        HostSend {
                            chat_id: &chat,
                            submission_id: &sid,
                            text: note,
                            origin: helm_chats::types::SubmissionOrigin::App,
                        },
                    );
                }
            }
        }
    }
}

/// The app's Telegram sink. The executor's wiring gives it the chat runtime and database; the
/// op dispatcher ([`ChatSink::attach_ops`]) and the line ([`ChatSink::attach_line`]) arrive at
/// start.
pub struct ChatSink {
    exec: Arc<AppExecutor>,
    ops: OnceLock<Arc<dyn OpInvoker>>,
    up: OnceLock<Arc<dyn Upstream>>,
    profile_dir: PathBuf,
    serial: tokio::sync::Mutex<()>,
    answered: Arc<Mutex<VecDeque<String>>>,
    watching: Mutex<Option<(String, tokio::task::JoinHandle<()>)>>,
    /// Approvals asked on Telegram and not yet answered ([`crate::approve`]).
    pub asks: crate::approve::Asks,
}

/// A path the worker can be started in: without Windows' verbatim prefix.
fn plain_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().into_owned();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_owned(),
        _ => s,
    }
}

impl ChatSink {
    pub fn new(exec: Arc<AppExecutor>, profile_dir: PathBuf) -> Self {
        let answered = telegram::load_answered(&telegram::answered_path(&profile_dir));
        ChatSink {
            exec,
            ops: OnceLock::new(),
            up: OnceLock::new(),
            profile_dir,
            serial: tokio::sync::Mutex::new(()),
            answered: Arc::new(Mutex::new(answered)),
            asks: crate::approve::Asks::default(),
            watching: Mutex::new(None),
        }
    }

    pub fn attach_ops(&self, ops: Arc<dyn OpInvoker>) {
        let _ = self.ops.set(ops);
    }

    pub fn attach_line(&self, up: Arc<dyn Upstream>) {
        let _ = self.up.set(up);
    }

    fn ctx(&self) -> Result<Ctx, String> {
        let w = self
            .exec
            .wiring()
            .ok_or("Helm is still starting; the message will be delivered when Helm is ready.")?;
        let ops =
            self.ops.get().cloned().ok_or(
                "Helm is still starting; the message will be delivered when Helm is ready.",
            )?;
        let rt = w
            .services
            .get::<ChatRuntime>()
            .ok_or("Helm's chats are not available.")?;
        let db = w
            .services
            .get::<Database>()
            .ok_or("Helm's database is not available.")?;
        // Read once per delivery, so a change in Settings applies to the next message.
        let file = telegram::load_file(&telegram::policy_path(&self.profile_dir));
        let (model, effort) = file.new_chat();
        Ok(Ctx {
            rt: (*rt).clone(),
            db,
            ops,
            handle: tokio::runtime::Handle::current(),
            tag_path: telegram::tag_path(&self.profile_dir),
            folder: plain_path(&self.profile_dir.join("telegram")),
            policy: file.check().unwrap_or_default(),
            new_chat_model: model.map(str::to_owned),
            new_chat_effort: effort.map(str::to_owned),
            answered: self.answered.clone(),
            answered_path: telegram::answered_path(&self.profile_dir),
            sent_path: telegram::sent_path(&self.profile_dir),
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
            telegram::policy_path(&self.profile_dir),
        ));
        *w = Some((chat.to_owned(), task));
    }
}

impl TelegramSink for ChatSink {
    fn deliver<'a>(&'a self, msg: TelegramMessage) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async move {
            // One at a time: two messages with no tag must not start two chats.
            let _turn = self.serial.lock().await;
            let c = self.ctx()?;
            let c2 = c.clone();
            let placed = tokio::task::spawn_blocking(move || place(&c2, &msg, helm_core::now_ms()))
                .await
                .map_err(|_| "The delivery did not finish.".to_owned())??;
            let (chat, baseline) = match placed {
                Placed::Already => return Ok(()),
                Placed::Sent { chat, baseline } => (chat, baseline),
                Placed::Answered {
                    chat,
                    baseline,
                    value,
                } => {
                    if let Some(up) = self.up.get() {
                        up.final_answer(&telegram::render_answered(&value));
                    }
                    (chat, baseline)
                }
            };
            self.watch(&c, &chat, baseline);
            Ok(())
        })
    }

    fn stop<'a>(&'a self) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let Ok(c) = self.ctx() else { return };
            let Some(tag) = telegram::load_tag(&c.tag_path) else {
                return;
            };
            if let Err(e) = c
                .ops
                .invoke("chat_stop", json!({ "chatId": tag.chat_id }))
                .await
            {
                log::warn!("relay: /stop from Telegram failed: {}", e.message);
            }
        })
    }

    fn answer<'a>(&'a self, id: String, index: usize) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let Some(up) = self.up.get().cloned() else {
                return;
            };
            let Some(ask) = self.asks.take(&id, index) else {
                up.notify("That request is no longer waiting, so your tap was not used.");
                return;
            };
            let label = ask.options[index].clone();
            let nothing =
                format!("Your \"{label}\" was not used: that request is no longer waiting.");
            if !telegram::load_approvals(&telegram::policy_path(&self.profile_dir)) {
                up.notify(&format!(
                    "Your \"{label}\" was not used: approving from Telegram is off in Helm."
                ));
                return;
            }
            let Ok(c) = self.ctx() else {
                up.notify(&nothing);
                return;
            };
            if let Some(question) = &ask.question {
                // A chat's own question: the tapped option goes back to it as a note from Helm.
                let note = crate::approve::answer_note(&id, question, &label);
                let sid = telegram::submission_id(&format!("ask/{id}"));
                let sent = host_send(
                    &c.rt,
                    HostSend {
                        chat_id: &ask.chat_id,
                        submission_id: &sid,
                        text: &note,
                        origin: helm_chats::types::SubmissionOrigin::App,
                    },
                );
                match sent {
                    Ok(_) => c.rt.add_context_row(
                        &ask.chat_id,
                        helm_chats::types::ContextKind::Telegram,
                        &format!("Answered from Telegram: {label}"),
                    ),
                    Err(e) => up.notify(&format!("Helm could not pass it on: {}", e.message)),
                }
                return;
            }
            let still = c.rt.live_thread(&ask.chat_id).is_some_and(|t| {
                t.interactions
                    .iter()
                    .any(|i| i.id == ask.request_id && crate::approve::askable(i))
            });
            if !still {
                up.notify(&nothing);
                return;
            }
            match c
                .ops
                .invoke(
                    "interaction_answer",
                    crate::approve::answer_args(&ask, index),
                )
                .await
            {
                Ok(_) => c.rt.add_context_row(
                    &ask.chat_id,
                    helm_chats::types::ContextKind::Telegram,
                    &format!("Answered from Telegram: {label}"),
                ),
                Err(e) => up.notify(&format!(
                    "Your \"{label}\" was not used: Helm could not answer it ({}).",
                    e.message
                )),
            }
        })
    }
}

#[cfg(test)]
#[path = "sink_tests.rs"]
mod tests;
