//! Telegram into chats, and the mirror back out.
//!
//! **In.** [`ChatSink`] takes each message the hub forwards and puts it into the active Telegram
//! chat ([`crate::telegram::active_chat`]), starting `Telegram <bot> <date>` when there is none:
//!
//! - It is stored through the chats crate's host send with origin `telegram`, so it never counts
//!   as consent and never trips the user's wait limit (that limit turns a waiting steer into an
//!   interrupt, and applies only to sends typed in Helm's own window).
//! - It follows the user's send behaviour, without ever aborting: "queue" queues it behind the
//!   running turn; "asap" and "interrupt" steer it in at the next tool boundary.
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
//! at most every [`STATUS_GAP_MS`]; the hub edits a single Telegram message in place. When the
//! turn ends, the final answer goes up as a new message (the hub then deletes the status). A
//! question goes up as a new message with numbered options; an approval, plan or trust prompt
//! goes up once as a note that it waits in Helm's window. The mirror is harness-level: the agent
//! never has to remember to reply.

use crate::client::{BoxFut, LineHandle, TelegramMessage, TelegramSink};
use crate::exec::AppExecutor;
use crate::telegram::{self, ChatFacts, ChatPort, TagPolicy, TELEGRAM_ORIGIN};
use helm_chats::submit::{host_send, HostSend, HostSendMode};
use helm_chats::types::{ChatStatus, InteractionAttention, InteractionMethod, SendMode};
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
    fn final_answer(&self, text: &str);
    /// A note to the phone that leaves the live status alone.
    fn notify(&self, _text: &str) {}
}

impl Upstream for LineHandle {
    fn status(&self, text: &str) {
        self.telegram_status(text)
    }
    fn final_answer(&self, text: &str) {
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
    /// The highest transcript row id now.
    pub max_id: i64,
    /// Another Telegram message is queued behind this turn.
    pub queued: bool,
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
            done: false,
        }
    }

    pub fn step(&mut self, s: &Snap, now: i64) -> Vec<Up> {
        let mut out = vec![];
        if self.done {
            return out;
        }
        if s.busy {
            self.started = true;
            self.idle_seen = 0;
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
                out.push(Up::Final(telegram::render_final(s.final_text.as_deref())));
                if s.queued {
                    // The next Telegram message runs as its own turn: mirror that one too.
                    self.baseline = s.max_id;
                    self.began = now;
                    self.started = false;
                    self.idle_seen = 0;
                    self.last_status = None;
                } else {
                    self.done = true;
                }
            }
        } else if !s.queued && now - self.began >= START_WAIT_MS {
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
    TelegramView {
        chat,
        max_age_hours: file.max_age_hours,
        max_tokens: file.max_tokens,
        away_alerts: file.away_alerts,
        approvals: file.approvals,
    }
}

impl ChatPort for RealPort<'_> {
    fn facts(&self, chat_id: &str) -> Option<ChatFacts> {
        chat_facts(self.db, chat_id)
    }

    fn create(&self, title: &str) -> Result<String, String> {
        std::fs::create_dir_all(self.folder)
            .map_err(|_| "The Telegram chats' folder could not be made.".to_owned())?;
        let chat = self
            .handle
            .block_on(
                self.ops
                    .invoke("chat_create", json!({ "cwd": self.folder })),
            )
            .map_err(|e| e.message)?;
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

/// How a Telegram message lands on a busy chat: the user's send mode, never an abort.
pub fn host_mode(mode: SendMode) -> HostSendMode {
    match mode {
        SendMode::Queue => HostSendMode::Queue,
        SendMode::Asap | SendMode::Interrupt => HostSendMode::Steer,
    }
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
    // starts after (its user row, or the end of the chat while it still waits in the queue).
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
    };
    let (chat, _created) =
        telegram::active_chat(&c.tag_path, &port, &c.policy, msg.bot.as_deref(), now)?;
    let baseline = max_row(&c.db, &chat);
    if let Some(q) = answerable(&c.rt, &chat) {
        let value = match q.method {
            InteractionMethod::Select => {
                telegram::question_answer(q.options.as_deref().unwrap_or(&[]), &msg.text)
            }
            _ => msg.text.trim().to_owned(),
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
    let mode =
        c.db.snapshot(|t| {
            Ok::<_, rusqlite::Error>(helm_chats::send_behavior::resolve(
                helm_core::prefs::read(t, "sendBehavior").as_ref(),
            ))
        })
        .map(|sb| sb.mode)
        .unwrap_or(SendMode::Asap);
    let sent = telegram::load_sent(&c.sent_path);
    let text = telegram::delivered_text(&msg.text, msg.age_minutes, &sent);
    let started = std::time::Instant::now();
    loop {
        let r = host_send(
            &c.rt,
            HostSend {
                chat_id: &chat,
                submission_id: &sid,
                text: &text,
                mode: host_mode(mode),
                origin: TELEGRAM_ORIGIN,
                // The user's own message, typed on the phone: it never waits for a run slot.
                gated: false,
            },
        );
        match r {
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
            let mut st = tx.prepare(
                "SELECT tool_name FROM messages WHERE chat_id = ?1 AND id > ?2
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
            let max_id: i64 = tx.query_row(
                "SELECT COALESCE(MAX(id), 0) FROM messages WHERE chat_id = ?1",
                [chat],
                |r| r.get(0),
            )?;
            let queued: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queued_submissions q
                   JOIN submission_meta m ON m.submission_id = q.submission_id
                   WHERE q.chat_id = ?1 AND m.origin = ?2)",
                rusqlite::params![chat, TELEGRAM_ORIGIN.as_str()],
                |r| r.get(0),
            )?;
            Ok::<_, rusqlite::Error>((status, recent, final_text, max_id, queued))
        })
        .unwrap_or_default();
    let (status, recent, final_text, max_id, queued) = row;
    let busy = status
        .map(|s| {
            matches!(
                ChatStatus::parse(&s),
                ChatStatus::Starting | ChatStatus::Running | ChatStatus::Stopping
            )
        })
        .unwrap_or(false);
    let live = rt.live_thread(chat);
    let running = live
        .as_ref()
        .map(|t| t.live_tools.iter().map(|l| l.name.clone()).collect())
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
        max_id,
        queued,
    }
}

/// Watch a chat until its Telegram turn (and any queued behind it) has ended.
pub async fn mirror(
    rt: ChatRuntime,
    db: Arc<Database>,
    up: Arc<dyn Upstream>,
    chat: String,
    baseline: i64,
    policy: PathBuf,
) {
    let mut m = Mirror::new(baseline, helm_core::now_ms());
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
                Up::Final(t) => up.final_answer(&t),
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
            .ok_or("Helm is still starting; the message stays queued.")?;
        let ops = self
            .ops
            .get()
            .cloned()
            .ok_or("Helm is still starting; the message stays queued.")?;
        let rt = w
            .services
            .get::<ChatRuntime>()
            .ok_or("Helm's chats are not available.")?;
        let db = w
            .services
            .get::<Database>()
            .ok_or("Helm's database is not available.")?;
        Ok(Ctx {
            rt: (*rt).clone(),
            db,
            ops,
            handle: tokio::runtime::Handle::current(),
            tag_path: telegram::tag_path(&self.profile_dir),
            folder: plain_path(&self.profile_dir.join("telegram")),
            // Read per delivery, so a change in Settings applies to the next message.
            policy: telegram::load_policy(&telegram::policy_path(&self.profile_dir)),
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
                .invoke(
                    "chat_stop",
                    json!({ "chatId": tag.chat_id, "continueQueue": false }),
                )
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
                        mode: HostSendMode::Steer,
                        origin: helm_chats::types::SubmissionOrigin::App,
                        gated: false,
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
