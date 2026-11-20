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
//! - `/mode` shows or sets the working mode. Like `/stop` it is answered between the phone and
//!   Helm and never reaches a chat. Only a sender the hub paired with someone on this PC gets
//!   that far, so the command is always one person's own words, never an agent's text.
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
//!
//! A failed request is never the answer: error rows are left out, and a turn that stopped on an
//! error gets a plain sentence instead ([`Stop`]). When Helm carries that turn on by itself (a
//! busy sign-in, a lost connection), the exchange stays open with a line saying so, and the
//! continued turn's answer is the one that goes up.

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
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
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
/// `person` is the hub person the exchange is for (`""`: nobody named, the hub's own default).
pub trait Upstream: Send + Sync {
    fn status(&self, person: &str, text: &str);
    /// The turn's answer, as a new message. `false` when it was not sent (the line is down);
    /// the mirror then tries it again and tells the chat, so the agent can act on it.
    fn final_answer(&self, person: &str, text: &str) -> bool;
    /// A note to the phone that leaves the live status alone.
    fn notify(&self, _person: &str, _text: &str) {}
}

impl Upstream for LineHandle {
    fn status(&self, person: &str, text: &str) {
        self.telegram_status(person, text)
    }
    fn final_answer(&self, person: &str, text: &str) -> bool {
        self.telegram_final(person, text)
    }
    fn notify(&self, person: &str, text: &str) {
        self.telegram_notify(person, text);
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
    /// The chat stopped on an error: what the mirror says instead of an answer.
    pub stopped: Option<Stop>,
}

/// How a chat's turn stopped, when it stopped on an error.
#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    /// Helm carries the turn on by itself: Claude's sign-in was busy (`sign_in`), or the
    /// connection or the service failed. `headline` is the error's first line.
    Retrying { sign_in: bool, headline: String },
    /// Helm stopped retrying because the sign-in keeps failing: the person has to sign in.
    SignInNeeded,
    /// Any other error, by its first line.
    Error(String),
}

impl Stop {
    /// Read from the chat's row: its status and error, whether it runs on a local model, whether
    /// the user is writing in it, and whether Helm stopped retrying it (on its sign-in, the
    /// connection or the service) since it was last carried on. `None` unless it stopped on an
    /// error.
    pub fn of(
        status: &str,
        error: Option<&str>,
        local: bool,
        typing: bool,
        gave_up: bool,
    ) -> Option<Stop> {
        use helm_chats::outage::{classify, Cause};
        if status != "interrupted" {
            return None;
        }
        let error = error.map(str::trim).filter(|e| !e.is_empty())?;
        let headline = error
            .split("\n\n")
            .next()
            .unwrap_or(error)
            .trim()
            .to_owned();
        let cause = if local { None } else { classify(error) };
        Some(match cause {
            Some(Cause::TokenRefresh) if gave_up => Stop::SignInNeeded,
            // Helm stopped retrying: the error is the exchange's last word.
            Some(_) if gave_up => Stop::Error(headline),
            // A chat the user is writing in is theirs to carry on: Helm does not.
            Some(c) if !typing => Stop::Retrying {
                sign_in: c == Cause::TokenRefresh,
                headline,
            },
            _ => Stop::Error(headline),
        })
    }

    /// The working line while Helm retries, if it does.
    pub fn status(&self) -> Option<&'static str> {
        match self {
            Stop::Retrying { sign_in: true, .. } => Some(telegram::SIGN_IN_RETRYING),
            Stop::Retrying { .. } => Some(telegram::OUTAGE_RETRYING),
            _ => None,
        }
    }

    /// The exchange's last word: what to do about a sign-in, or the error's headline.
    pub fn last_word(&self) -> String {
        match self {
            Stop::Retrying { sign_in: true, .. } | Stop::SignInNeeded => {
                telegram::SIGN_IN_NEEDED.to_owned()
            }
            Stop::Retrying { headline, .. } | Stop::Error(headline) => {
                telegram::render_stopped(headline)
            }
        }
    }
}

/// How long an exchange has waited on Helm's own retry of its stopped turn. Both mirrors keep
/// one, so neither waits past [`telegram::RETRY_HOLD_MS`].
#[derive(Debug, Default)]
pub struct RetryHold {
    since: Option<i64>,
}

impl RetryHold {
    /// Whether the exchange (`begun`: its message is in the transcript) still waits on Helm's
    /// own retry of its stopped turn at `now`.
    pub fn holds(&mut self, s: &Snap, begun: bool, now: i64) -> bool {
        if !begun || !matches!(s.stopped, Some(Stop::Retrying { .. })) {
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now - since < telegram::RETRY_HOLD_MS
    }

    /// The chat is working again: a later stop is timed afresh.
    pub fn reset(&mut self) {
        self.since = None;
    }
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
    /// How long the exchange has waited on Helm's own retry of its stopped turn.
    retry: RetryHold,
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
            retry: RetryHold::default(),
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
        // The exchange's message is in the transcript: a stop seen now is this exchange's.
        let begun = self.started || s.max_id > self.baseline;
        if s.busy {
            self.started = true;
            self.idle_seen = 0;
            self.retry.reset();
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
        } else if self.retry.holds(s, begun, now) {
            // Helm carries the stopped turn on by itself: the exchange stays open for the
            // continued turn's answer, with a line saying so instead of the error.
            self.started = true;
            self.idle_seen = 0;
            let st = s
                .stopped
                .as_ref()
                .and_then(Stop::status)
                .unwrap_or_default();
            let due = self.last_at.is_none_or(|t| now - t >= STATUS_GAP_MS);
            if self.last_status.as_deref() != Some(st) && due {
                out.push(Up::Status(st.to_owned()));
                self.last_status = Some(st.to_owned());
                self.last_at = Some(now);
            }
        } else if self.started || s.final_text.is_some() || (begun && s.stopped.is_some()) {
            // Settled: seen idle twice in a row, so the last streamed row is flushed.
            self.idle_seen += 1;
            if self.idle_seen >= 2 {
                // Nothing is ever held behind the turn: an idle chat with two settled
                // samples has no more of this exchange to show.
                let unsent = s
                    .turn_done
                    .as_ref()
                    .is_some_and(|(id, _)| *id > self.sent_through);
                let due = match (&s.stopped, &s.turn_done, &s.final_text) {
                    // A turn that stopped on an error says so in words, never with the error
                    // row or a reply the error cut short.
                    (Some(stop), _, _) if !unsent => Some(stop.last_word()),
                    // Its answer went up while the chat was still busy: the exchange just
                    // closes, and says nothing a second time.
                    (_, Some((id, _)), _) if *id <= self.sent_through => None,
                    (_, Some((_, text)), _) => Some(telegram::render_final(Some(text.as_str()))),
                    (_, None, Some(text)) => Some(telegram::render_final(Some(text.as_str()))),
                    (_, None, None) if self.answered => None,
                    (_, None, None) => Some(telegram::render_final(None)),
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
pub(crate) struct RealPort<'a> {
    pub(crate) db: &'a Database,
    pub(crate) rt: &'a ChatRuntime,
    pub(crate) ops: &'a dyn OpInvoker,
    pub(crate) handle: &'a tokio::runtime::Handle,
    pub(crate) folder: &'a str,
    /// What the chats this port starts are for ("Telegram", "voice"), for the log and errors.
    pub(crate) label: &'a str,
    /// The model and effort saved for new chats of this kind; `None` passes neither to the op, so
    /// the chat opens on the new-chat default as before.
    pub(crate) new_chat_model: Option<String>,
    pub(crate) new_chat_effort: Option<String>,
    /// Set by `create` when the saved model or effort could not be used and the chat opened on
    /// the app default: what the chat and the phone are told, in plain words.
    pub(crate) ignored: Mutex<Option<String>>,
    /// The profile a new chat is kept to, once created. `None` leaves it unassigned.
    pub(crate) profile_id: Option<String>,
}

/// Whether the chat is archived and the tokens it has used; `None` when it no longer exists.
pub(crate) fn chat_facts(db: &Database, chat_id: &str) -> Option<ChatFacts> {
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
    /// Waiting prompts answered from the hub's Remote page ([`crate::remote`]).
    pub remote_answers: bool,
    /// The model a new Telegram chat opens with; `None`: the same as any other new chat.
    pub model: Option<String>,
    /// The effort a new Telegram chat opens with; `None`: the same as any other new chat.
    pub effort: Option<String>,
    /// Whether this PC takes this person's Telegram (the switch, or its default: on for the
    /// home profile, off for the others).
    pub take_telegram: bool,
}

pub fn telegram_view(
    db: &Database,
    profile_dir: &std::path::Path,
    profile_id: &str,
    now: i64,
) -> TelegramView {
    let paths = crate::people::telegram_paths(profile_dir, profile_id);
    let file = telegram::load_file(&paths.policy);
    let policy = file.check().unwrap_or_default();
    let chat = telegram::load_tag(&paths.tag).and_then(|tag| {
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
    let is_home = db
        .snapshot(|tx| helm_profiles::store::home(tx))
        .ok()
        .flatten()
        .is_some_and(|h| h.id == profile_id);
    TelegramView {
        chat,
        max_age_hours: file.max_age_hours,
        max_tokens: file.max_tokens,
        away_alerts: file.away_alerts,
        approvals: file.approvals,
        remote_answers: file.remote_answers,
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        take_telegram: telegram::takes_telegram(&file, is_home),
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

    /// Keep the word for the chat and the phone that a saved setting was not used. The first
    /// reason stands: a model left out and then a refusal of the effort say one thing.
    fn say_ignored(&self, note: String) {
        self.ignored
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert(note);
    }
}

impl ChatPort for RealPort<'_> {
    fn facts(&self, chat_id: &str) -> Option<ChatFacts> {
        chat_facts(self.db, chat_id)
    }

    fn create(&self, title: &str) -> Result<String, String> {
        std::fs::create_dir_all(self.folder)
            .map_err(|_| format!("The {} chats' folder could not be made.", self.label))?;
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
                log::warn!(
                    "relay: the saved {} model is on a local server that is not available; opening on the app default",
                    self.label
                );
                self.say_ignored(format!(
                    "The model saved for new {} chats is on a local server that is not available, so this chat opened on the app default.",
                    self.label
                ));
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
                log::warn!(
                    "relay: refused the saved {} chat settings ({}); opening on the app default",
                    self.label,
                    e.message
                );
                self.say_ignored(format!(
                    "The model or effort saved for new {} chats was refused ({}), so this chat opened on the app default.",
                    self.label,
                    crate::telegram::cut_chars(&e.message, 160).0
                ));
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
        // A channel chat belongs to the profile that runs the channel: the chat's owner decides
        // who may see it, who the away alerts route to, and which profile its next message lands
        // in. A refused assignment is not worth losing the chat over: it stays unassigned and
        // the next delivery finds it by its tag as usual.
        if let Some(pid) = &self.profile_id {
            if let Err(e) = self.handle.block_on(self.ops.invoke(
                "profiles_assign_chats",
                json!({ "chatIds": [&id], "profileId": pid }),
            )) {
                log::warn!(
                    "relay: the new {} chat could not be kept to its profile: {}",
                    self.label,
                    e.message
                );
            }
        }
        Ok(id)
    }
}

/// What a delivery did.
#[derive(Clone, Debug, PartialEq)]
pub enum Placed {
    /// Stored as a Telegram-origin submission. `ignored` says, for a chat this message just
    /// opened, that the saved model or effort could not be used (the chat has been told; the
    /// phone is told by the caller).
    Sent {
        chat: String,
        baseline: i64,
        ignored: Option<String>,
    },
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

pub(crate) fn is_answerable(i: &helm_chats::types::Interaction) -> bool {
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

pub(crate) fn max_row(db: &Database, chat: &str) -> i64 {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM messages WHERE chat_id = ?1",
            [chat],
            |r| r.get(0),
        )
    })
    .unwrap_or(0)
}

/// Everything a delivery needs, cloned into the blocking pool. All of it is for one profile:
/// its channel files, its folder, and the hub person its frames carry (`""` when the profile
/// has no person yet).
#[derive(Clone)]
pub struct Ctx {
    pub rt: ChatRuntime,
    pub db: Arc<Database>,
    pub ops: Arc<dyn OpInvoker>,
    pub handle: tokio::runtime::Handle,
    /// The local profile this exchange belongs to.
    pub profile_id: String,
    /// The hub person of that profile, or `""` when it is not linked yet.
    pub person: String,
    pub tag_path: PathBuf,
    /// The folder new Telegram chats start in.
    pub folder: String,
    pub policy: TagPolicy,
    /// This profile's policy file, for a re-read while a mirror runs.
    pub policy_path: PathBuf,
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
            Placed::Sent {
                chat,
                baseline,
                ignored: None,
            }
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
        label: "Telegram",
        new_chat_model: c.new_chat_model.clone(),
        new_chat_effort: c.new_chat_effort.clone(),
        ignored: Mutex::new(None),
        profile_id: Some(c.profile_id.clone()),
    };
    let (chat, _created) =
        telegram::active_chat(&c.tag_path, &port, &c.policy, msg.bot.as_deref(), now)?;
    let ignored = port
        .ignored
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
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
    let mut text = telegram::delivered_text(&msg.text, msg.age_minutes, &sent);
    // The chat opened without the model or effort saved for it: say so in the chat, where the
    // agent and the user can see it.
    if let Some(why) = &ignored {
        text.push_str(&format!("\n\n[Helm] {why}"));
    }
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
                return Ok(Placed::Sent {
                    chat,
                    baseline,
                    ignored,
                });
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
            // The chat's status, and what the mirror needs to word a stop: its error, whether it
            // runs on a local model, whether the user is writing in it, and whether Helm stopped
            // retrying it since it was last carried on (a give-up line, on the sign-in or on any
            // other outage, after the last continuation, in this exchange).
            type ChatRow = (String, Option<String>, Option<String>, Option<String>, bool);
            let chat_row: Option<ChatRow> = tx
                .query_row(
                    "SELECT c.status, c.error, c.provider, c.draft,
                            EXISTS(SELECT 1 FROM messages g
                                   WHERE g.chat_id = c.id AND g.id > ?2 AND g.role = 'context'
                                     AND g.text IN (?3, ?4)
                                     AND g.id > COALESCE((SELECT MAX(o.id) FROM messages o
                                                          WHERE o.chat_id = c.id
                                                            AND o.source_key LIKE 'c:outage:%'), 0))
                     FROM chats c WHERE c.id = ?1",
                    rusqlite::params![
                        chat,
                        baseline,
                        helm_chats::outage::SIGN_IN_AGAIN_TEXT,
                        helm_chats::outage::GAVE_UP_TEXT
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            let stopped = chat_row.as_ref().and_then(|(status, error, provider, draft, gave_up)| {
                Stop::of(
                    status,
                    error.as_deref(),
                    provider.as_deref().is_some_and(|p| !p.is_empty()),
                    draft.as_deref().is_some_and(|d| !d.trim().is_empty()),
                    *gave_up,
                )
            });
            let status = chat_row.map(|r| r.0);
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
            // A failed request is never the answer: error rows are left out.
            let final_text: Option<String> = tx
                .query_row(
                    "SELECT text FROM messages WHERE chat_id = ?1 AND id > ?2 AND role = 'assistant'
                     AND active = 1 AND text != '' AND status != 'error' ORDER BY id DESC LIMIT 1",
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
                     AND active = 1 AND text != '' AND status != 'error' AND json_valid(details)
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
            Ok::<_, rusqlite::Error>((status, recent, final_text, turn_done, max_id, stopped))
        })
        .unwrap_or_default();
    let (status, recent, final_text, turn_done, max_id, stopped) = row;
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
        stopped,
    }
}

/// How long a final answer may go on being refused by the line before the chat is asked to send
/// it again. The mirror offers the answer every half second, so a line that is back within this
/// delivers it itself, and asking sooner would give the phone the answer twice.
pub const UNDELIVERED_GRACE_MS: i64 = 90_000;

/// Decides when the chat is told that its answer did not reach the phone: once, and only after
/// the line has refused the answer for [`UNDELIVERED_GRACE_MS`] without a break.
#[derive(Debug, Default)]
pub struct Undelivered {
    /// When the current run of refusals began.
    since: Option<i64>,
    told: bool,
}

impl Undelivered {
    /// One offer of an answer to the line: `ok` says it was taken. True when the chat is to be
    /// told now.
    pub fn offered(&mut self, ok: bool, now: i64) -> bool {
        if ok {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        if self.told || now - since < UNDELIVERED_GRACE_MS {
            return false;
        }
        self.told = true;
        true
    }
}

/// Watch a chat until its Telegram turn has ended.
pub async fn mirror(
    rt: ChatRuntime,
    db: Arc<Database>,
    up: Arc<dyn Upstream>,
    person: String,
    chat: String,
    baseline: i64,
    policy: PathBuf,
) {
    let mut m = Mirror::new(baseline, helm_core::now_ms());
    // When the chat is told its answer could not reach the phone: once per exchange (the message
    // has a fixed id, so it is stored once however often failures come after it), and only when
    // the line stays down, see [`Undelivered`].
    let mut late = Undelivered::default();
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
                Up::Status(t) => up.status(&person, &t),
                Up::Final(t) => {
                    let ok = up.final_answer(&person, &t);
                    m.ack(&t, ok);
                    if !late.offered(ok, helm_core::now_ms()) {
                        continue;
                    }
                    // The line has stayed down, so the user cannot see an answer it would not
                    // take. Say so inside the chat, where the agent can act on it: it can send the
                    // answer again, and until it does the user simply has no reply.
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

/// Why a request could not be answered.
#[derive(Debug, PartialEq)]
pub(crate) enum AnswerError {
    /// It no longer waits (answered in Helm, withdrawn, or not the kind that may be answered).
    Gone,
    /// Helm refused the answer.
    Failed(String),
}

/// Answer a request that still waits, the way the card in Helm does, and leave a line in the chat
/// saying where the answer came from. `accepts` says which waiting requests this way may answer;
/// one that is gone, or not of that kind, answers nothing. Telegram's buttons and the Remote page
/// both answer through here.
pub(crate) async fn answer_request(
    c: &Ctx,
    chat_id: &str,
    request_id: &str,
    args: serde_json::Value,
    accepts: fn(&helm_chats::types::Interaction) -> bool,
    row: (helm_chats::types::ContextKind, String),
) -> Result<(), AnswerError> {
    let still = c.rt.live_thread(chat_id).is_some_and(|t| {
        t.interactions
            .iter()
            .any(|i| i.id == request_id && accepts(i))
    });
    if !still {
        return Err(AnswerError::Gone);
    }
    c.ops
        .invoke("interaction_answer", args)
        .await
        .map_err(|e| AnswerError::Failed(e.message))?;
    c.rt.add_context_row(chat_id, row.0, &row.1);
    Ok(())
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
    /// Hub ids that answered a question, per profile id, so a resend does not answer twice
    /// (or turn into a prompt). Built from each profile's file on the first message for it.
    answered: Mutex<std::collections::HashMap<String, Arc<Mutex<VecDeque<String>>>>>,
    /// The mirror task watching each chat, by chat id.
    watching: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// Approvals asked on Telegram and not yet answered ([`crate::approve`]).
    pub asks: crate::approve::Asks,
}

/// A path the worker can be started in: without Windows' verbatim prefix.
pub(crate) fn plain_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().into_owned();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_owned(),
        _ => s,
    }
}

impl ChatSink {
    pub fn new(exec: Arc<AppExecutor>, profile_dir: PathBuf) -> Self {
        ChatSink {
            exec,
            ops: OnceLock::new(),
            up: OnceLock::new(),
            profile_dir,
            serial: tokio::sync::Mutex::new(()),
            answered: Mutex::new(HashMap::new()),
            asks: crate::approve::Asks::default(),
            watching: Mutex::new(HashMap::new()),
        }
    }

    pub fn attach_ops(&self, ops: Arc<dyn OpInvoker>) {
        let _ = self.ops.set(ops);
    }

    pub fn attach_line(&self, up: Arc<dyn Upstream>) {
        let _ = self.up.set(up);
    }

    /// The delivery context for the profile that runs the channel of hub person `person`
    /// (`None`: an older hub's frame, the home profile).
    pub(crate) fn ctx(&self, person: Option<&str>) -> Result<Ctx, String> {
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
        crate::people::migrate(&self.profile_dir, &db);
        let profile = crate::people::profile_for(&db, person)?;
        let paths = crate::people::telegram_paths(&self.profile_dir, &profile.id);
        // Read once per delivery, so a change in Settings applies to the next message.
        let file = telegram::load_file(&paths.policy);
        let (model, effort) = file.new_chat();
        let answered = self
            .answered
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(profile.id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(telegram::load_answered(&paths.answered))))
            .clone();
        Ok(Ctx {
            rt: (*rt).clone(),
            db,
            ops,
            handle: tokio::runtime::Handle::current(),
            person: profile.hub_person_id.unwrap_or_default(),
            profile_id: profile.id,
            tag_path: paths.tag,
            folder: plain_path(&paths.folder),
            policy: file.check().unwrap_or_default(),
            policy_path: paths.policy,
            new_chat_model: model.map(str::to_owned),
            new_chat_effort: effort.map(str::to_owned),
            answered,
            answered_path: paths.answered,
            sent_path: paths.sent,
        })
    }

    /// A tap on a staged settings change: the same one decision the card in Helm makes, so the
    /// first answer wins. The chat must still belong to the profile that was asked; whatever the
    /// outcome the tap is spent and the person is told what happened.
    async fn answer_consent(
        &self,
        c: &Ctx,
        ask: &crate::approve::Ask,
        index: usize,
        up: &Arc<dyn Upstream>,
    ) {
        let label = ask.options[index].clone();
        let unused = |why: &str| format!("Your \"{label}\" was not used: {why}");
        let Some(w) = self.exec.wiring() else {
            up.notify(&c.person, &unused("Helm is still starting."));
            return;
        };
        let (services, events) = (w.services.clone(), w.events.clone());
        let (db, chat, profile) = (c.db.clone(), ask.chat_id.clone(), c.profile_id.clone());
        let owned = tokio::task::spawn_blocking(move || {
            db.snapshot(|tx| helm_profiles::store::profile_of_chat(tx, &chat))
                .ok()
                .flatten()
                .is_some_and(|p| p.id == profile)
        })
        .await
        .unwrap_or(false);
        if !owned {
            up.notify(&c.person, &unused("that chat is not yours."));
            return;
        }
        let ask2 = ask.clone();
        let done = tokio::task::spawn_blocking(move || {
            let d = crate::approve::decide_consent(&services, &events, &ask2, index)?;
            let allowed = d.request.state != helm_consent::RequestState::Denied;
            if let Some(applying) = d.applying {
                let _ = applying.join();
            }
            let kinds = services.get::<helm_consent::AllowKinds>();
            let db = services.need::<Database>()?;
            let outcome = kinds
                .and_then(|k| {
                    helm_consent::store::get(&db, &k, &d.request.id)
                        .ok()
                        .flatten()
                })
                .and_then(|r| r.outcome);
            Ok::<_, helm_ipc::ApiError>((allowed, outcome))
        })
        .await;
        match done {
            Ok(Ok((allowed, outcome))) => {
                c.rt.add_context_row(
                    &ask.chat_id,
                    helm_chats::types::ContextKind::Telegram,
                    &format!("Answered from Telegram: {label}"),
                );
                let said = match (allowed, outcome) {
                    (true, Some(o)) => format!("Approved. {o}"),
                    (true, None) => "Approved.".to_owned(),
                    (false, _) => "Denied: nothing changed.".to_owned(),
                };
                up.notify(&c.person, &said);
            }
            Ok(Err(e)) if e.code == "conflict" => {
                // Somebody answered first: say where, from the shared record.
                let (db, id) = (c.db.clone(), ask.request_id.clone());
                let services = self.exec.wiring().map(|w| w.services.clone());
                let note = tokio::task::spawn_blocking(move || {
                    let kinds = services?.get::<helm_consent::AllowKinds>()?;
                    let r = helm_consent::store::get(&db, &kinds, &id).ok().flatten();
                    Some(crate::approve::settled_note(
                        r.as_ref(),
                        helm_core::now_ms(),
                    ))
                })
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "It was answered already.".into());
                up.notify(&c.person, &unused(&note));
            }
            Ok(Err(e)) => up.notify(
                &c.person,
                &unused(&format!("Helm could not answer it ({}).", e.message)),
            ),
            Err(_) => up.notify(&c.person, &unused("Helm could not answer it.")),
        }
    }

    /// Answer a `/mode` command between the phone and Helm: it reads and changes the working
    /// mode through the same ops the window's chip uses, and reaches no chat.
    async fn answer_mode(&self, c: &Ctx, up: &Arc<dyn Upstream>, cmd: ModeCommand) {
        // The working mode governs every chat on this PC, so only the person whose profile this
        // machine is home to may use it from a phone. A household person whose Telegram this PC
        // takes, or anyone else, gets a plain line and nothing changes.
        let is_home =
            c.db.snapshot(|tx| helm_profiles::store::home(tx))
                .ok()
                .flatten()
                .is_some_and(|h| h.id == c.profile_id);
        if !is_home {
            up.notify(
                &c.person,
                "The working mode belongs to the PC that runs Helm: only its owner can change it from a phone.",
            );
            return;
        }
        let (op, args) = match cmd {
            ModeCommand::Show => ("workmode_get", json!({})),
            ModeCommand::Set(m) => ("workmode_set", json!({ "scope": "app", "mode": m })),
            ModeCommand::Usage => {
                up.notify(
                    &c.person,
                    "Working mode: \"/mode\" shows it, \"/mode run\" and \"/mode check\" change it.",
                );
                return;
            }
        };
        match c.ops.invoke(op, args).await {
            Ok(view) => match mode_answer(cmd, &view) {
                Some(text) => up.notify(&c.person, &text),
                None => up.notify(
                    &c.person,
                    "Helm answered the working mode in words it could not read.",
                ),
            },
            Err(e) => {
                let what = if op == "workmode_get" {
                    "read"
                } else {
                    "change"
                };
                up.notify(
                    &c.person,
                    &format!("Helm could not {what} the working mode ({}).", e.message),
                );
            }
        }
    }

    /// Watch `chat` for its profile unless it is already being watched. One task per chat; a
    /// new exchange in a watched chat reuses the task, a chat whose task ended gets a new one.
    fn watch(&self, c: &Ctx, chat: &str, baseline: i64) {
        let Some(up) = self.up.get().cloned() else {
            return;
        };
        let mut w = self.watching.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(h) = w.get(chat) {
            if !h.is_finished() {
                return;
            }
        }
        w.retain(|_, h| !h.is_finished());
        let task = c.handle.spawn(mirror(
            c.rt.clone(),
            c.db.clone(),
            up,
            c.person.clone(),
            chat.to_owned(),
            baseline,
            c.policy_path.clone(),
        ));
        w.insert(chat.to_owned(), task);
    }
}

/// What a `/mode` message is asking for. Only the two user words reach the op; anything else
/// after the command gets the usage line instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeCommand {
    /// Show the mode, the board overrides and the counts.
    Show,
    /// Set the app's mode to the op's word (`run` or `check_in`).
    Set(&'static str),
    /// Something the command does not understand.
    Usage,
}

/// The command behind a Telegram text: the first word exactly `/mode` (a bot handle after `@`
/// allowed, and the word itself read without minding case, as Telegram's own commands are).
/// `None` for anything else — `/modem`, `mode run` and ordinary sentences all stay messages.
pub fn mode_command(text: &str) -> Option<ModeCommand> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut words = trimmed.split_whitespace();
    let head = words.next()?;
    // The word after the slash, before any `@handle`: exactly `mode`, in any case. This keeps
    // `/modem` and `/modeX` out.
    if !head[1..].split('@').next()?.eq_ignore_ascii_case("mode") {
        return None;
    }
    Some(match words.next() {
        None => ModeCommand::Show,
        Some(w) => match w.to_ascii_lowercase().as_str() {
            "run" | "complete" => ModeCommand::Set("run"),
            "check" | "checkin" | "check-in" | "check_in" | "ask" => ModeCommand::Set("check_in"),
            _ => ModeCommand::Usage,
        },
    })
}

/// The user's words for one of the op's mode names.
fn mode_label(word: &str) -> Option<&'static str> {
    match word {
        "run" => Some("Run to completion"),
        "check_in" => Some("Check in with me"),
        _ => None,
    }
}

/// The phone's reply, from the view the ops answered with: the mode and counts for a show, the
/// new mode for a set. `None` when the view does not read as one (which never happens with the
/// real op).
fn mode_answer(cmd: ModeCommand, view: &serde_json::Value) -> Option<String> {
    let mode = mode_label(view.get("app")?.as_str()?)?;
    match cmd {
        ModeCommand::Show => {
            let boards = view
                .get("overrides")
                .and_then(Value::as_array)
                .map_or(0, |a| a.len());
            let counts = view.get("counts")?;
            let decided = counts.get("decided").and_then(Value::as_u64)?;
            let needs = counts.get("needsYou").and_then(Value::as_u64)?;
            Some(format!(
                "Working mode: {mode}.\nBoards with a mode of their own: {boards}.\n\
                 Decided for you: {decided} \u{b7} need you: {needs}"
            ))
        }
        ModeCommand::Set(_) => Some(format!("Working mode is now {mode}.")),
        // Usage is answered to the phone before any op runs, so it never comes here.
        ModeCommand::Usage => None,
    }
}

impl TelegramSink for ChatSink {
    fn deliver<'a>(&'a self, msg: TelegramMessage) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async move {
            // One at a time: two messages with no tag must not start two chats.
            let _turn = self.serial.lock().await;
            let c = self.ctx(msg.person.as_deref())?;
            // A working-mode command is answered between the phone and Helm: it opens no chat,
            // sets no tag and starts no watch. One with a picture or file attached is left as a
            // message, so nothing the person sent is silently dropped.
            if msg.images.is_empty() && msg.files.is_empty() {
                if let Some(cmd) = mode_command(&msg.text) {
                    if let Some(up) = self.up.get() {
                        self.answer_mode(&c, up, cmd).await;
                    }
                    return Ok(());
                }
            }
            let c2 = c.clone();
            let placed = tokio::task::spawn_blocking(move || place(&c2, &msg, helm_core::now_ms()))
                .await
                .map_err(|_| "The delivery did not finish.".to_owned())??;
            let (chat, baseline) = match placed {
                Placed::Already => return Ok(()),
                Placed::Sent {
                    chat,
                    baseline,
                    ignored,
                } => {
                    // The phone is told too: the first message of a chat that opened without its
                    // saved model would otherwise be answered by a model nobody chose.
                    if let (Some(why), Some(up)) = (ignored, self.up.get()) {
                        up.notify(&c.person, &why);
                    }
                    (chat, baseline)
                }
                Placed::Answered {
                    chat,
                    baseline,
                    value,
                } => {
                    if let Some(up) = self.up.get() {
                        up.final_answer(&c.person, &telegram::render_answered(&value));
                    }
                    (chat, baseline)
                }
            };
            self.watch(&c, &chat, baseline);
            Ok(())
        })
    }

    fn stop<'a>(&'a self, person: Option<String>) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let Ok(c) = self.ctx(person.as_deref()) else {
                return;
            };
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

    fn answer<'a>(&'a self, person: Option<String>, id: String, index: usize) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let Some(up) = self.up.get().cloned() else {
                return;
            };
            let who = person.clone().unwrap_or_default();
            // The tap must come from the person the question was asked of.
            let ask = match self.asks.tap(&id, index, &who, helm_core::now_ms()) {
                crate::approve::Tap::Taken(ask) => ask,
                crate::approve::Tap::Gone => {
                    up.notify(
                        &who,
                        "That request is no longer waiting, so your tap was not used.",
                    );
                    return;
                }
                crate::approve::Tap::Foreign => {
                    up.notify(
                        &who,
                        "That request was not asked of you, so your tap was not used.",
                    );
                    return;
                }
                crate::approve::Tap::Expired => {
                    up.notify(
                        &who,
                        "That request has expired, so your tap was not used. Open Helm to answer it.",
                    );
                    return;
                }
            };
            let label = ask.options[index].clone();
            let nothing =
                format!("Your \"{label}\" was not used: that request is no longer waiting.");
            let Ok(c) = self.ctx(person.as_deref()) else {
                up.notify(&who, &nothing);
                return;
            };
            if !telegram::load_approvals(&c.policy_path) {
                up.notify(
                    &c.person,
                    &format!(
                        "Your \"{label}\" was not used: approving from Telegram is off in Helm."
                    ),
                );
                return;
            }
            if ask.consent {
                self.answer_consent(&c, &ask, index, &up).await;
                return;
            }
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
                    Err(e) => up.notify(
                        &c.person,
                        &format!("Helm could not pass it on: {}", e.message),
                    ),
                }
                return;
            }
            let row = (
                helm_chats::types::ContextKind::Telegram,
                format!("Answered from Telegram: {label}"),
            );
            match answer_request(
                &c,
                &ask.chat_id,
                &ask.request_id,
                crate::approve::answer_args(&ask, index),
                crate::approve::askable,
                row,
            )
            .await
            {
                Ok(()) => {}
                Err(AnswerError::Gone) => up.notify(&c.person, &nothing),
                Err(AnswerError::Failed(why)) => up.notify(
                    &c.person,
                    &format!("Your \"{label}\" was not used: Helm could not answer it ({why})."),
                ),
            }
        })
    }
}

#[cfg(test)]
#[path = "sink_tests.rs"]
mod tests;
