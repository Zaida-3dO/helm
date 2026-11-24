//! The per-chat worker actor.
//!
//! One task per chat. It owns the worker link, the in-flight command table, the live streaming
//! rows, the live tools, the pending interactions and the timers, and it is the only writer of
//! its chat's session log. It selects over host commands, worker output, the 80 ms flush tick and
//! the next timer.
//!
//! Rules it enforces:
//! - `get_state` is answered before anything else is written to the worker; commands wait.
//! - A prompt not answered within the ack limit becomes `uncertain` and is never resent.
//! - Stop sends `abort`; a clean settle is `stopped`, anything else `interrupted`. Messages
//!   Claude had not read when the Stop landed are handed over again at once, as the next turn;
//!   they are never held and never dropped. No settle within the stop limit closes the worker.
//! - Streaming rows are flushed every 80 ms while dirty with one `message` event per flush; tool
//!   start and end are written as they happen.
//! - A model or effort change not acknowledged within 10 s fails the run.
//! - The wait limit arms for the user's accepted steers only and retries while the worker answers
//!   `waiting`.

use crate::runtime::{ActorHandle, Live, Shared, SpawnRequest, WorkerLink};
use crate::session_log::{
    self, ClaudeBinding, ContextEntry, Entry, Header, MessageEntry, RowWrite, SessionLog, ToolEntry,
};
use crate::store;
use crate::types::*;
use helm_workers::WorkerOutput;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant as TInstant;

pub(crate) enum Cmd {
    /// Send this saved submission to the worker now.
    Dispatch(String),
    Stop {
        reply: oneshot::Sender<ChatStatus>,
    },
    /// Stop, and do not send again what Claude had not read: it reads "not sent" instead. For a
    /// stop that is not the user's own (a roll over, the usage governor), which must not start
    /// the next turn by itself.
    StopNoResend {
        reply: oneshot::Sender<ChatStatus>,
    },
    /// An `extension_ui_response` (fields without `type`).
    Answer(Map<String, Value>),
    ApplyModel {
        model: Option<String>,
        effort: Option<String>,
    },
    ToolStop {
        tool_call_id: String,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    Compact {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Withdraw {
        submission_id: String,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    /// Quit: shut the worker down and answer once it has exited.
    Shutdown {
        reply: oneshot::Sender<()>,
    },
    /// Quit timed out: end the worker tree now.
    Terminate,
    /// Idle over the cap: shut the worker down.
    Retire,
    /// The chat was archived (its row is already marked): stop a running turn in the background,
    /// kill the worker if the stop is slow, then close it. Nothing waits on the answer.
    Archived,
    /// The chat was restored while a worker was still winding down from its archive.
    Restored,
    /// The queue moved this chat's held start to a new place in line (the row is already
    /// written; this keeps the actor's own place in step with it).
    QueueSeq(u64),
    /// The queue cancelled this chat's held start: the waiting sends go back to their owners,
    /// the hold and its record go (a hand park stays when `keep_park` says so).
    QueueCancel {
        /// The chat stands parked by hand: what waits goes back, the park stays.
        keep_park: bool,
    },
    /// The queue's Start anyway: this chat's waiting send goes ahead even while the Claude plan
    /// windows read full. A per-chat one-shot — the start it opens consumes it, so the next
    /// send is held again like anyone else's.
    QueueStartAnyway,
    /// The maintenance tick: refresh the chat's live state, and mark it stalled if it has had
    /// no output for `after` (longer for a chat on a local model server, which answers one
    /// request at a time and gaps between outputs in between).
    StallCheck {
        after: std::time::Duration,
        local_after: std::time::Duration,
    },
    /// Apply the staged edit once the worker has closed, then send its submission.
    ApplyEdit,
    /// The send behaviour changed: a different long-command setting relaunches
    /// the worker, at once when idle and at the next idle point when busy.
    SendBehavior(SendBehavior),
    /// Re-read the model catalogue from the live worker's `get_state`; answers whether it did.
    RefreshCatalogue {
        reply: oneshot::Sender<bool>,
    },
    /// A line for the worker from another crate (`WorkerBroadcast`); held until it is ready.
    WorkerLine(Value),
    /// A line for the transcript (a context row), written in order with the chat's own rows.
    ContextRow {
        kind: ContextKind,
        text: String,
    },
    /// Start the worker ahead of the first send ([`crate::runtime::ChatRuntime::prewarm`]):
    /// `get_state` only, so Claude Code's start-up runs while the user types.
    Prewarm,
    /// Prepare this local chat's model for its next turn while the user types
    /// ([`crate::runtime::ChatRuntime::warm`]): the same preparation a send makes, but no turn
    /// opens and no worker starts. A send that comes while it runs joins it.
    Warm,
    /// AFK just turned on: run the interactions already waiting past the AFK policy.
    AfkSweep,
    /// The question gate answered a card it was holding. The actor is that card's only owner,
    /// so every race (the user answered first, the card went away) settles here; `reply` gets
    /// whether the verdict was applied.
    GateVerdict {
        interaction_id: String,
        outcome: Box<crate::seams::GateOutcome>,
        reply: std::sync::mpsc::Sender<bool>,
    },
    /// The stop judge answered for the held end of the turn `run_id` names. The actor owns the
    /// hold, so every race (a message was sent, the deadline passed, a later turn began) settles
    /// here; `reply` gets whether the verdict was applied.
    StopVerdict {
        run_id: String,
        outcome: crate::seams::StopOutcome,
        reply: std::sync::mpsc::Sender<bool>,
    },
    /// A local chat's model is being made ready for the turn `token` names: a line to show.
    LocalNote {
        token: u64,
        text: String,
    },
    /// That preparation finished: the name the worker should ask the server for, or why no turn
    /// starts.
    LocalPrepared {
        token: u64,
        result: Result<String, String>,
    },
}

pub(crate) const STOP_TIMEOUT_ERROR: &str =
    "The task did not stop within 60 seconds, so Helm closed it.";
/// Why a turn was stopped when its chat was archived.
pub(crate) const ARCHIVE_STOP_NOTE: &str = "Stopped because the chat was archived.";
pub(crate) const ACK_TIMEOUT_ERROR: &str =
    "Claude Code did not confirm the message in time. It was not sent again; recover it if it is missing.";
pub(crate) const MODEL_ACK_ERROR: &str =
    "Claude Code did not confirm the model or effort change within 10 seconds.";

enum Pending {
    Ready,
    Prompt {
        submission_id: String,
    },
    Abort,
    Model,
    Withdraw {
        submission_id: String,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    DeliverNow {
        submission_id: String,
    },
    ToolStop {
        reply: oneshot::Sender<Result<bool, String>>,
    },
    Compact {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Catalogue {
        reply: oneshot::Sender<bool>,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum Timer {
    Ack(String),
    ModelAck(String),
    StopDeadline,
    WaitLimit(String),
    /// A held question-card's hold ends: the card becomes a normal card. One per held card.
    GateDeadline(String),
    /// A held turn end's check ends: the chat finishes as it would have with no judge.
    StopCheck,
}

/// A turn end the stop judge is holding: the turn's finish is not announced until the judge
/// answers, a message is sent, or the hold runs out.
struct StopHold {
    run_id: String,
}

struct LiveMsg {
    role: MessageRole,
    text: String,
    thinking: String,
    timestamp: i64,
    /// The row as last written to the database, and when (streaming writes are throttled).
    row: Option<MessageRow>,
    written: Option<Instant>,
}

struct LiveToolRec {
    name: String,
    args: String,
    summary: String,
    started_at: i64,
    stoppable: bool,
}

struct StopState {
    replies: Vec<oneshot::Sender<ChatStatus>>,
    /// Unread messages go to Claude again once the stop settles. Any stop that asks for no resend
    /// turns it off for all the stops waiting on this one.
    resend: bool,
}

struct Actor {
    sh: Arc<Shared>,
    chat_id: String,
    live: Arc<Mutex<Live>>,
    link: Option<Arc<dyn WorkerLink>>,
    /// The running worker has been told to shut down (a relaunch, an edit, a retirement or a
    /// quit) and has not exited yet: it is closing and must not be handed new work. A send that
    /// finds it starts a fresh worker instead.
    link_closing: bool,
    out: Option<mpsc::Receiver<WorkerOutput>>,
    ready: bool,
    deferred: Vec<Value>,
    next_id: u64,
    pending: HashMap<String, Pending>,
    timers: Vec<(TInstant, Timer)>,
    run_id: Option<String>,
    /// The submission that opened the current turn. Until Claude has read it, a worker that dies
    /// is not a crash to carry on from: Claude never saw what it would be continuing.
    turn_opener: Option<String>,
    /// The submission that opened the current turn, for the turn's whole life ([`Self::turn_opener`]
    /// is only its life before Claude has read it). A provider switch that stops a quiet turn to
    /// restart it on the new model sends this one again.
    turn_opened_by: Option<String>,
    /// When a note about this chat going silent last reached its parent: one per hour at most.
    silent_noted_at: Option<Instant>,
    /// This child's last turn end was already reported to its parent as unfinished; set again
    /// only after the chat works once more ([`crate::childnote`]).
    unreported_noted: bool,
    /// When those notes reached the parent in the last hour: a child that never reports costs
    /// its parent at most [`UNREPORTED_NOTES_PER_HOUR`] turns an hour, however often it is nudged.
    unreported_at: Vec<Instant>,
    /// Turn ends in a row that closed with a tool call printed as text, on a chat running on a
    /// local model: the first earns one corrective message, the second a flag for the person,
    /// and nothing more until a turn closes normally ([`crate::childnote`]).
    printed_run: u32,
    /// The open turn has produced something: an assistant message began or a tool call started.
    /// A turn that has acted is never opened again by a provider switch (its side effects would
    /// repeat); the change waits for the next idle point instead.
    turn_acted: bool,
    /// How many processes the worker's kill-group held when the worker was ready and when the
    /// turn opened, whichever is larger: what a chat's own helpers (native tool servers, a
    /// language server) keep running all the time. Only a tree grown past it means something the
    /// chat started is still working.
    tree_base: u32,
    /// A provider switch stopped this turn to restart it: the submission to open the next turn
    /// once the stop has settled, on a fresh worker for the new model.
    switch_restart: Option<String>,
    stopping: Option<StopState>,
    /// A Stop ended this run, so an exit before the next run is one Helm asked for, even once
    /// `stopping` has been settled (a worker that ends itself after an interrupt).
    stop_ended: bool,
    stream: HashMap<String, LiveMsg>,
    dirty: BTreeSet<String>,
    /// Streaming rows whose database copy is behind what the UI was sent.
    db_behind: BTreeSet<String>,
    tools: HashMap<String, LiveToolRec>,
    log: Option<SessionLog>,
    worker_error: Option<String>,
    last_error_row: Option<String>,
    native_session: Option<String>,
    /// Claude Code's running usage totals after the last turn this worker reported (at launch:
    /// what the resumed session last reached), which the next turn's share is measured from.
    cost_base: Usage,
    shutdown_replies: Vec<oneshot::Sender<()>>,
    /// Host-tool calls still running, by request id (`chat_cancel` aborts them).
    host_calls: HashMap<String, tokio::task::AbortHandle>,
    retiring: bool,
    /// The chat is archived: a late event from the dying worker changes nothing the user sees
    /// (no completion, no unread mark) and nothing is sent.
    archived: bool,
    /// The held end of the last turn, while the stop judge decides about it.
    stop_hold: Option<StopHold>,
    finished: bool,
    /// The last worker output (or turn start).
    last_output: Instant,
    /// When `chat_activity.last_activity_at` was last written (writes are at most every 15 s).
    activity_written: Option<Instant>,
    /// An edit waits for the worker to close.
    edit_waiting: bool,
    /// Sends that arrived while a quit or an edit was waiting for the worker to close: they are
    /// handed over once it has exited (an edit's new turn takes them), or left saved for the
    /// next start when the app is quitting.
    held_for_exit: Vec<String>,
    /// The long-command setting the running worker was launched with.
    launched_long: Option<LongCommands>,
    /// Whether Helm's web search was offered to the running worker's launch: the tool list is
    /// fixed when a worker starts, so when the user enters or forgets the search key or flips
    /// "Offer in Claude chats", this no longer matches and the worker is restarted (a changed
    /// MCP server set reaches a running worker over its own live config line instead).
    launched_web_tools: Option<bool>,
    /// That setting changed while a turn ran: relaunch at the next idle point.
    relaunch_on_idle: bool,
    /// The relaunch held for the idle point is a change of provider (Claude to a local model or
    /// back): a message that would open a turn must not go to the old worker meanwhile.
    provider_switch_pending: bool,
    /// Running subagents pushed off the live list by a 31st: kept so their end still reaches
    /// their tool row.
    evicted: Vec<Subagent>,
    /// The turn-opening send being timed, until its first output ([`crate::timeline`]).
    timeline: Option<crate::timeline::SendTimeline>,
    /// Steers the worker answered `waiting` for, logged to the same file until each is read.
    steer_waits: crate::timeline::SteerWaits,
    /// The last worker start that failed: pre-warm waits [`PREWARM_RETRY`] before trying again.
    spawn_failed_at: Option<Instant>,
    /// When the running worker was started, for the exit log.
    worker_started: Option<Instant>,
    /// This actor's own channel, held weakly, for work it hands to another thread.
    me: mpsc::WeakSender<Cmd>,
    /// A local chat's turn waiting for its model to be made ready.
    prepare: Option<Prepare>,
    /// A local chat's turn held at its very start because the server already carries the cap
    /// of working chats: the opening submission and those held behind it. The maintenance tick
    /// reopens it when a place frees.
    start_wait: Option<(String, Vec<String>)>,
    /// The queue's Start anyway, waiting to be spent: the next gate look for this chat answers
    /// the Claude plan windows as open, and the start that goes consumes it. Only this chat,
    /// only that one send.
    start_anyway: bool,
    prepare_count: u64,
    /// The submission that is opening its turn now that its preparation is done.
    opening: Option<String>,
    /// The model name the last preparation answered, for the worker started next.
    prepared: Option<String>,
    /// The server and model name the running worker was launched for; `None` for a Claude chat.
    launched_local: Option<(String, String)>,
    /// The prompt carrying a message fork's saved history, until the worker answers it.
    carry_in_flight: Option<String>,
    /// The live state the maintenance tick last published (the `chat_activity.state` mirror);
    /// only this actor writes it, so the cache needs no lock.
    state: Option<crate::types::ChatState>,
    /// A model warm-up ran (or was attempted) since this chat's last turn started; typing does
    /// not start another until one does.
    warmed: bool,
    /// When the last warm-up was attempted, for the gap between attempts.
    last_warm: Option<Instant>,
}

/// A local chat's turn that is waiting for its model: the submission that opens the turn and any
/// later sends held behind it. `sid` is `None` while a warm-up runs with no send yet: the first
/// send to arrive takes it over.
struct Prepare {
    token: u64,
    sid: Option<String>,
    /// The preparation has shown its loading line, so its end should clear it.
    line: bool,
    held: Vec<String>,
    /// The server and model this preparation is making ready. A warm-up whose chat has since been
    /// pointed at another model is out of date and must not answer for the send.
    target: (String, String),
}

/// The server and model name of a chat on a local model, if it is one.
fn local_of(chat: &ChatRecord) -> Option<(String, String)> {
    chat.model
        .as_deref()
        .and_then(crate::local::split)
        .map(|(s, m)| (s.to_owned(), m.to_owned()))
}

/// After a failed worker start, typing does not start another for this long (sends still do).
const PREWARM_RETRY: std::time::Duration = std::time::Duration::from_secs(30);
/// A local server's queue answer is read at most this often while a turn is open against it.
const QUEUE_STALE: std::time::Duration = std::time::Duration::from_secs(10);
/// How long the last good reading of a server's queue still counts after reads start failing: two
/// failed reads' worth. Past that the answer is unknown, not "still busy".
const QUEUE_KEEP: std::time::Duration = std::time::Duration::from_secs(30);

/// A remembered reading of a server's queue, only while it is recent enough to trust.
fn recent_reading(
    info: Option<crate::local::QueueInfo>,
    good_at: Option<Instant>,
) -> Option<crate::local::QueueInfo> {
    good_at.filter(|t| t.elapsed() < QUEUE_KEEP).and(info)
}
/// The most notes an unreported child's turn ends may send its parent in an hour.
const UNREPORTED_NOTES_PER_HOUR: usize = 3;
/// Typing does not start a second model warm-up within this long of the last one.
const WARM_MIN_GAP: std::time::Duration = std::time::Duration::from_secs(300);
/// The soft cap of this PC's chats a local model server carries before new local turns queue at
/// their start rather than piling onto the server (the stored `localChatCap` overrides; 0 is off).
/// A local server answers one request at a time, so this is how many chats can be "working"
/// there at once; the rest wait, visibly, with their place in line.
const DEFAULT_LOCAL_CAP: u64 = 3;
/// The least time between two notes from one child to its parent about the child being silent.
const SILENT_NOTE_GAP: std::time::Duration = std::time::Duration::from_secs(3600);

pub(crate) fn start(sh: Arc<Shared>, chat_id: String) -> ActorHandle {
    let (tx, rx) = mpsc::channel(64);
    let status = sh
        .db
        .snapshot(|t| store::get_chat(t, &chat_id, 0))
        .ok()
        .flatten()
        .map(|c| c.status)
        .unwrap_or(ChatStatus::Idle);
    let thread = sh
        .db
        .snapshot(|t| match store::get_chat(t, &chat_id, 0)? {
            Some(c) => crate::ops::stored_thread(t, &c, 0).map(Some),
            None => Ok(None),
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| ThreadState {
            chat_id: chat_id.clone(),
            status,
            permission_mode: None,
            interactions: vec![],
            live_tools: vec![],
            background_tasks: vec![],
            subagents: vec![],
            subagents_running: 0,
            compacting: None,
            loading: None,
            stop_check: None,
            capabilities: Capabilities::default(),
            send_behavior: crate::send_behavior::HOME_DEFAULTS,
            revision: 0,
        });
    let live = Arc::new(Mutex::new(Live {
        thread,
        has_worker: false,
        last_used: Instant::now(),
        stalled: false,
        skills: Vec::new(),
        context_gen: 0,
        local_model: None,
        server_id: None,
        turn_opened_ms: None,
        local_ahead: None,
        held_at_start: None,
        hold_class: None,
        hold_reason: None,
        hold_top: false,
    }));
    let actor = Actor {
        sh: sh.clone(),
        chat_id,
        live: live.clone(),
        link: None,
        link_closing: false,
        out: None,
        ready: false,
        deferred: Vec::new(),
        next_id: 0,
        pending: HashMap::new(),
        timers: Vec::new(),
        run_id: None,
        turn_opener: None,
        turn_opened_by: None,
        turn_acted: false,
        tree_base: 2,
        silent_noted_at: None,
        unreported_noted: false,
        unreported_at: Vec::new(),
        printed_run: 0,
        switch_restart: None,
        stopping: None,
        stop_ended: false,
        stream: HashMap::new(),
        dirty: BTreeSet::new(),
        db_behind: BTreeSet::new(),
        tools: HashMap::new(),
        log: None,
        worker_error: None,
        last_error_row: None,
        native_session: None,
        cost_base: Usage::default(),
        shutdown_replies: Vec::new(),
        host_calls: HashMap::new(),
        retiring: false,
        archived: false,
        stop_hold: None,
        finished: false,
        last_output: Instant::now(),
        activity_written: None,
        edit_waiting: false,
        held_for_exit: Vec::new(),
        launched_long: None,
        launched_web_tools: None,
        relaunch_on_idle: false,
        provider_switch_pending: false,
        evicted: Vec::new(),
        timeline: None,
        steer_waits: Default::default(),
        spawn_failed_at: None,
        worker_started: None,
        me: tx.downgrade(),
        prepare: None,
        start_wait: None,
        start_anyway: false,
        prepare_count: 0,
        opening: None,
        prepared: None,
        launched_local: None,
        carry_in_flight: None,
        state: None,
        warmed: false,
        last_warm: None,
    };
    sh.handle.spawn(actor.run(rx));
    ActorHandle { tx, live }
}

async fn recv_out(out: &mut Option<mpsc::Receiver<WorkerOutput>>) -> Option<WorkerOutput> {
    match out {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Subagents kept on the live thread (the roster shows at most 30).
const MAX_SUBAGENTS: usize = 30;

/// The most pictures kept from one tool result.
const RESULT_IMAGES_MAX: usize = 6;

/// The most tool pictures one chat keeps; past this the oldest go, so a chat that screenshots
/// every step does not grow the attachments folder without bound.
const TOOL_PICTURES_PER_CHAT: i64 = 200;

/// A settled subagent's report on the live thread: a preview, since the whole thread is sent on
/// every change.
const LIVE_REPORT_BYTES: usize = 2 * 1024;
/// A settled subagent's report on its tool row.
const ROW_REPORT_BYTES: usize = 16 * 1024;

/// `subagents_running` follows every status change in the same lock, so no reader sees the two
/// disagree.
fn recount(t: &mut ThreadState) {
    t.subagents_running = t
        .subagents
        .iter()
        .filter(|s| s.status == SubagentStatus::Running)
        .count() as u32;
}

fn set_report(s: &mut Subagent, report: Option<&str>, max: usize) {
    s.report = report.map(|r| session_log::cut(r, max).to_owned());
    s.report_truncated = report.is_some_and(|r| r.len() > max);
}

/// The scope a reported skill carries: `helm-skill` when it is one of Helm's own (the CLI may
/// show it under the plugin's own prefix, e.g. `helm:run-tests`), `personal` or `project` for a
/// name under those prefixes, empty for every other source.
fn helm_skill_scope(own: &[String], name: &str) -> &'static str {
    let lower = name.to_lowercase();
    let bare = lower.strip_prefix("helm:").unwrap_or(&lower);
    if own.iter().any(|o| o == bare || o == &lower) {
        "helm-skill"
    } else if lower.starts_with("personal:") {
        "personal"
    } else if lower.starts_with("project:") {
        "project"
    } else {
        ""
    }
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

fn now() -> i64 {
    helm_core::now_ms()
}

impl Actor {
    async fn run(mut self, mut rx: mpsc::Receiver<Cmd>) {
        let mut flush = tokio::time::interval(self.sh.cfg.limits.flush);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let next = self.timers.iter().map(|(t, _)| *t).min();
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(c) => self.on_cmd(c),
                    None => break,
                },
                out = recv_out(&mut self.out) => self.on_output(out),
                _ = async { match next { Some(t) => tokio::time::sleep_until(t).await, None => std::future::pending().await } } => self.on_timers(),
                _ = flush.tick(), if !self.dirty.is_empty() || !self.db_behind.is_empty() => self.flush(),
            }
            if self.finished {
                break;
            }
        }
        // However the actor ended, a turn end it was holding still finishes (once).
        self.release_stop_hold(true);
        self.sh.actors.lock().unwrap().remove(&self.chat_id);
        // Its blocking requests (if any) leave the waiting tracker with it.
        crate::attention::publish(&self.sh);
    }

    // ------------------------------------------------------------------ plumbing

    fn write<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<T>,
    ) -> Option<T> {
        #[cfg(test)]
        write_counts::bump(&self.chat_id);
        match self.sh.db.write(f) {
            Ok(v) => Some(v),
            Err(e) => {
                log::error!("chat {}: database write failed: {e}", self.chat_id);
                None
            }
        }
    }

    /// Keep the pictures a tool returned (a screenshot, say) as the chat's own images, so the
    /// tool's row can show them like the thumbnails in a sent message. They are linked to the call,
    /// never to a send, so the composer does not list them as waiting. A picture that is not a
    /// readable still image, or is over the attachments limits, is skipped; at most
    /// [`RESULT_IMAGES_MAX`] are kept per call and [`TOOL_PICTURES_PER_CHAT`] per chat. The bytes
    /// are decoded before the write opens.
    fn keep_result_images(&self, call_id: &str, list: Option<&Value>) -> Vec<MessageImage> {
        use base64::Engine as _;
        let Some(list) = list.and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut decoded: Vec<(String, Vec<u8>)> = Vec::new();
        for img in list.iter().take(RESULT_IMAGES_MAX) {
            let (Some(mime), Some(data)) = (str_of(img, "mimeType"), str_of(img, "data")) else {
                continue;
            };
            match base64::engine::general_purpose::STANDARD.decode(data.as_bytes()) {
                Ok(bytes) => decoded.push((mime, bytes)),
                Err(e) => log::warn!(
                    "chat {}: a tool picture was not valid base64: {e}",
                    self.chat_id
                ),
            }
        }
        if decoded.is_empty() {
            return Vec::new();
        }
        let dir = self.sh.cfg.profile_dir.join("attachments");
        let owner = self.chat_id.clone();
        let link = format!("tool:{call_id}");
        let at = now();
        // The files are written before the transaction, so a large picture never holds the
        // writer lock; only their rows are written inside it.
        let staged: Vec<crate::attachments::StagedImage> = decoded
            .iter()
            .enumerate()
            .filter_map(|(n, (mime, bytes))| {
                let name = format!("Picture {}", n + 1);
                match crate::attachments::stage_image(&dir, &name, mime, bytes) {
                    Ok(s) => Some(s),
                    Err(_) => {
                        log::warn!("chat {owner}: a tool picture could not be kept");
                        None
                    }
                }
            })
            .collect();
        let written = self.write(|t| {
                let mut kept = Vec::new();
                for st in &staged {
                    let stored = crate::attachments::record_image(t, &owner, st, at)?;
                    t.execute(
                        "UPDATE chat_attachments SET submission_id = ?1 WHERE id = ?2",
                        rusqlite::params![link, stored.id],
                    )?;
                    kept.push(MessageImage {
                        id: stored.id,
                        name: st.name.clone(),
                        mime: st.mime.clone(),
                        width: st.width,
                        height: st.height,
                    });
                }
                // The oldest tool pictures past the cap go, rows and files.
                let old: Vec<String> = {
                    let mut st = t.prepare(
                        "SELECT id FROM chat_attachments WHERE chat_id = ?1 AND submission_id LIKE 'tool:%'
                         ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?2",
                    )?;
                    let rows =
                        st.query_map(rusqlite::params![owner, TOOL_PICTURES_PER_CHAT], |r| r.get(0))?;
                    rows.collect::<rusqlite::Result<_>>()?
                };
                for id in &old {
                    t.execute("DELETE FROM chat_attachments WHERE id = ?1", [id])?;
                }
                Ok((kept, old))
            });
        let kept = written.unwrap_or_else(|| {
            // No rows: the staged files would be left with nothing to name them.
            for st in &staged {
                let _ = std::fs::remove_file(dir.join(&st.id));
            }
            Default::default()
        });
        for id in &kept.1 {
            let _ = std::fs::remove_file(dir.join(id));
        }
        kept.0
    }

    fn emit(&self, e: ChatEvent) {
        self.sh.events.emit(e.to_app_event());
    }

    fn emit_chat(&self) {
        let rev = self.sh.clock.next();
        if let Ok(Some(chat)) = self
            .sh
            .db
            .snapshot(|t| store::get_chat(t, &self.chat_id, rev))
        {
            self.live.lock().unwrap().thread.status = chat.status;
            self.emit(ChatEvent::Chat {
                chat: Box::new(chat),
            });
        }
    }

    /// Tell the UI what is in flight for this chat: sent but not yet read by Claude, or left
    /// over and recoverable.
    fn emit_pending(&self) {
        let rev = self.sh.clock.next();
        if let Ok(pending) = self.sh.db.snapshot(|t| store::pending(t, &self.chat_id)) {
            self.emit(ChatEvent::Pending {
                chat_id: self.chat_id.clone(),
                revision: rev,
                pending,
            });
        }
    }

    fn emit_thread(&self) {
        self.emit_thread_with(true);
    }

    /// `publish` recomputes the attention summary too; subagent steps skip it, as they change
    /// nothing it reads and arrive up to twice a second per agent.
    fn emit_thread_with(&self, publish: bool) {
        let rev = self.sh.clock.next();
        let thread = {
            let mut live = self.live.lock().unwrap();
            live.thread.revision = rev;
            recount(&mut live.thread);
            live.thread.live_tools = {
                let mut v: Vec<LiveTool> = self
                    .tools
                    .iter()
                    .map(|(id, t)| LiveTool {
                        tool_call_id: id.clone(),
                        name: t.name.clone(),
                        summary: (!t.summary.is_empty()).then(|| t.summary.clone()),
                        started_at: t.started_at,
                        stoppable: t.stoppable,
                    })
                    .collect();
                v.sort_by_key(|t| t.started_at);
                v
            };
            live.thread.clone()
        };
        self.emit(ChatEvent::Thread {
            chat_id: self.chat_id.clone(),
            thread,
        });
        // Interactions feed the waiting tracker.
        if publish {
            crate::attention::publish(&self.sh);
        }
    }

    fn emit_rows(&self, keys: &[String]) {
        let rows: Vec<MessageRow> = self
            .sh
            .db
            .snapshot(|t| {
                let mut v = Vec::new();
                for k in keys {
                    if let Some(r) = store::message_by_key(t, &self.chat_id, k)? {
                        v.push(r);
                    }
                }
                Ok::<_, rusqlite::Error>(v)
            })
            .unwrap_or_default();
        if rows.is_empty() {
            return;
        }
        let rev = self.sh.clock.next();
        self.emit(ChatEvent::Message {
            chat_id: self.chat_id.clone(),
            revision: rev,
            rows,
        });
    }

    fn set_status(&self, status: ChatStatus, error: Option<&str>) {
        let id = self.chat_id.clone();
        self.write(|t| Self::set_status_in(t, &id, status, error));
        self.after_status(status);
    }

    /// The database half of a status change, so a caller can fold it into a bigger transaction.
    fn set_status_in(
        t: &rusqlite::Transaction<'_>,
        id: &str,
        status: ChatStatus,
        error: Option<&str>,
    ) -> rusqlite::Result<()> {
        store::set_status(t, id, status, error, now())?;
        // Entering an active or stopped state drops the completion.
        if status.is_busy() || status == ChatStatus::Stopped {
            crate::manage::drop_completion(t, id)?;
        }
        Ok(())
    }

    /// The in-memory and event half of a status change, run after the write has committed.
    fn after_status(&self, status: ChatStatus) {
        let busy = status.is_busy();
        let server = if busy {
            self.chat().and_then(|c| local_of(&c)).map(|(s, _)| s)
        } else {
            None
        };
        let waited = {
            let mut live = self.live.lock().unwrap();
            live.thread.status = status;
            if busy {
                if live.turn_opened_ms.is_none() {
                    // The turn opened: when, and which server its request is bound for, so other
                    // chats on the same server can count how many of us would be answered first.
                    live.turn_opened_ms = Some(now());
                    live.server_id = server;
                }
                None
            } else {
                live.turn_opened_ms = None;
                live.server_id = None;
                live.held_at_start = None;
                live.hold_class = None;
                // The waiting flag belongs to the turn; it is gone with it.
                live.local_ahead.take()
            }
        };
        if waited.is_some() {
            let id = self.chat_id.clone();
            self.write(|t| {
                t.execute(
                    "UPDATE chat_activity SET local_ahead = NULL, queue_reason = NULL WHERE chat_id = ?1",
                    [&id],
                )
            });
        }
        if !busy {
            // A turn start that was held is over the moment a turn ends or settles: the hold
            // row goes with it (a hand-parked chat keeps its row, for the next start to obey).
            if self.live.lock().unwrap().hold_reason.take().is_some() {
                let id = self.chat_id.clone();
                self.write(|t| {
                    t.execute(
                        "UPDATE chat_activity SET queue_reason = NULL WHERE chat_id = ?1",
                        [&id],
                    )
                });
            }
            crate::queue::clear_auto_hold(&self.sh.db, &self.chat_id);
        }
        if !status.is_busy() && self.live.lock().unwrap().stalled {
            // A turn that ends is no longer stalled.
            self.set_stalled(false);
        }
        self.emit_chat();
        crate::attention::publish(&self.sh);
        // A status change is exactly what the build queue wants to hear about: a finished
        // tree should settle its entry at once, not on the next minute.
        self.sh.build_queue_poke.notify_one();
    }

    fn status(&self) -> ChatStatus {
        self.live.lock().unwrap().thread.status
    }

    fn touch(&self) {
        self.live.lock().unwrap().last_used = Instant::now();
    }

    fn add_timer(&mut self, after: std::time::Duration, t: Timer) {
        self.timers.push((TInstant::now() + after, t));
    }

    fn clear_timer(&mut self, pred: impl Fn(&Timer) -> bool) {
        self.timers.retain(|(_, t)| !pred(t));
    }

    fn new_id(&mut self) -> String {
        self.next_id += 1;
        format!("h{}", self.next_id)
    }

    /// Write a command now if the worker is ready, else after `get_state` answers.
    fn send(&mut self, cmd: Value) {
        if self.ready {
            if let Some(l) = &self.link {
                if !l.send(&cmd) {
                    log::warn!("chat {}: worker stdin closed", self.chat_id);
                }
            }
        } else {
            self.deferred.push(cmd);
        }
    }

    fn request(&mut self, mut cmd: Value, p: Pending) -> String {
        let id = self.new_id();
        cmd["id"] = json!(id);
        self.pending.insert(id.clone(), p);
        self.send(cmd);
        id
    }

    fn chat(&self) -> Option<ChatRecord> {
        self.sh
            .db
            .snapshot(|t| store::get_chat(t, &self.chat_id, 0))
            .ok()
            .flatten()
    }

    fn session_log(&mut self) -> Option<&mut SessionLog> {
        if self.log.is_none() {
            let chat = self.chat()?;
            let header = Header {
                version: session_log::FORMAT_VERSION.into(),
                chat_id: self.chat_id.clone(),
                harness: "claude".into(),
                cwd: chat.cwd.clone(),
                created_at: chat.created_at,
            };
            match SessionLog::open(&self.sh.cfg.sessions(), &header) {
                Ok((log, _)) => self.log = Some(log),
                Err(e) => {
                    log::error!("chat {}: session log unavailable: {e}", self.chat_id);
                    return None;
                }
            }
        }
        self.log.as_mut()
    }

    /// Append to the session log, then project into the database, then emit the row.
    fn record(&mut self, mut entry: Entry) {
        let key = entry.id().to_owned();
        if let Some(log) = self.session_log() {
            if let Err(e) = log.append(&mut entry) {
                log::error!("chat {}: session log append failed: {e}", self.chat_id);
            }
        }
        let raw = serde_json::to_value(&entry).unwrap_or(Value::Null);
        let rev = self.sh.clock.next();
        let artifacts = self.sh.cfg.artifacts();
        let id = self.chat_id.clone();
        if let Some(obj) = raw.as_object() {
            self.write(|t| session_log::project(t, &artifacts, &id, obj, rev));
        }
        if !matches!(entry, Entry::Usage(_)) {
            self.emit_rows(&[key]);
        }
    }

    /// Ask the AFK policy about one interaction. Returns true when AFK answered it (so it is not
    /// listed as waiting). A deferral is written to the transcript and the card stays.
    fn afk_handle(&mut self, i: &Interaction, at_flip: bool) -> bool {
        let Some(policy) = self.sh.afk.get().cloned() else {
            return false;
        };
        match policy.decide(&self.chat_id, i, at_flip) {
            crate::seams::AfkAction::Leave => false,
            crate::seams::AfkAction::Defer { marker } => {
                self.context_row(ContextKind::Afk, &marker);
                false
            }
            crate::seams::AfkAction::Answer { mut fields, marker } => {
                fields.insert("id".into(), json!(i.id));
                fields.insert("type".into(), json!("extension_ui_response"));
                self.send(Value::Object(fields));
                self.context_row(ContextKind::Afk, &marker);
                true
            }
        }
    }

    /// Offer one new card to the question gate. True when the gate holds it: the card is marked
    /// as being checked, and until its verdict or the hold deadline it reaches neither AFK nor
    /// the waiting tracker. The safety line is checked here as well as in the gate: a card that
    /// grants authority to act (a permission or trust card) or takes a secret input is never
    /// offered — those are safety gates, not questions.
    ///
    /// A card the worker sends with a prefill is a step the user went back to (the card's Back
    /// button asks it again with their earlier answer filled in): they are revising their own
    /// answer, so it is theirs and never offered.
    fn gate_hold(&mut self, i: &mut Interaction) -> bool {
        if i.secret == Some(true) || !matches!(i.attention, Some(InteractionAttention::Question)) {
            return false;
        }
        if i.prefill.is_some() {
            return false;
        }
        let Some(gate) = self.sh.gate.get().cloned() else {
            return false;
        };
        if gate.offer(&self.chat_id, i) != crate::seams::GateOffer::Hold {
            return false;
        }
        i.gate = Some(GateState {
            state: GateStatus::Checking,
            brief: None,
            note: None,
        });
        self.add_timer(
            self.sh.cfg.limits.gate_deadline,
            Timer::GateDeadline(i.id.clone()),
        );
        true
    }

    /// The hold on a card ended without a decision: it is a normal card again, with a quiet
    /// line. AFK then gets its usual say over it, exactly as if the gate had passed it.
    fn gate_fail_card(&mut self, id: &str, note: &str) {
        self.clear_timer(|t| *t == Timer::GateDeadline(id.to_owned()));
        {
            let mut live = self.live.lock().unwrap();
            if let Some(i) = live.thread.interactions.iter_mut().find(|i| i.id == id) {
                if i.gate
                    .as_ref()
                    .is_some_and(|g| g.state == GateStatus::Checking)
                {
                    i.gate = Some(GateState {
                        state: GateStatus::Failed,
                        brief: None,
                        note: Some(note.to_owned()),
                    });
                }
            }
        }
        let card = self
            .live
            .lock()
            .unwrap()
            .thread
            .interactions
            .iter()
            .find(|i| i.id == id)
            .cloned();
        if let Some(i) = card {
            if self.afk_handle(&i, false) {
                self.live
                    .lock()
                    .unwrap()
                    .thread
                    .interactions
                    .retain(|x| x.id != id);
            }
        }
        self.emit_thread();
    }

    /// Apply the question gate's verdict to a card it was holding. Only a card still being
    /// checked accepts one: a card the user answered first, or that went away another way, is
    /// not here, and the answer comes back false. The actor is the card's only owner, so this
    /// is where every race settles.
    fn gate_verdict(&mut self, id: &str, outcome: crate::seams::GateOutcome) -> bool {
        self.clear_timer(|t| *t == Timer::GateDeadline(id.to_owned()));
        let held = self
            .live
            .lock()
            .unwrap()
            .thread
            .interactions
            .iter()
            .any(|i| {
                i.id == id
                    && i.gate
                        .as_ref()
                        .is_some_and(|g| g.state == GateStatus::Checking)
            });
        if !held {
            return false;
        }
        match outcome {
            crate::seams::GateOutcome::Decide { mut fields, marker } => {
                fields.insert("id".into(), json!(id));
                fields.insert("type".into(), json!("extension_ui_response"));
                self.send(Value::Object(fields));
                self.live
                    .lock()
                    .unwrap()
                    .thread
                    .interactions
                    .retain(|i| i.id != id);
                self.context_row(ContextKind::Gatekeeper, &marker);
            }
            crate::seams::GateOutcome::Escalate { brief } => {
                let mut live = self.live.lock().unwrap();
                if let Some(i) = live.thread.interactions.iter_mut().find(|i| i.id == id) {
                    let rec = brief.recommendation.trim().to_owned();
                    i.gate = Some(GateState {
                        state: GateStatus::Escalated,
                        brief: Some(brief),
                        note: None,
                    });
                    // The recommended option arrives pre-filled, so one click answers it.
                    if !rec.is_empty()
                        && i.options
                            .as_ref()
                            .is_some_and(|o| o.iter().any(|x| x.trim() == rec))
                    {
                        i.prefill = Some(rec);
                    }
                }
            }
            crate::seams::GateOutcome::Failed { note } => self.gate_fail_card(id, &note),
        }
        self.emit_thread();
        true
    }

    /// A clean turn end, offered to the stop judge. True when the judge holds it: the turn's
    /// finish (the completion row, its event and the "finished" notice) waits until the judge's
    /// verdict, the next message, or the hold ending. Never offered: a stop the user or the
    /// governor asked for, an interrupted turn, an archived or retiring chat, a quit, a chat with
    /// messages waiting to go out, or one whose background agents still run (it is not over).
    fn stop_hold_offer(&mut self, run: Option<String>) -> bool {
        let Some(judge) = self.sh.stop_judge.get().cloned() else {
            return false;
        };
        let Some(run_id) = run else {
            return false;
        };
        if self.archived
            || self.retiring
            || self.finished
            || self.agents_running()
            || self
                .sh
                .shutting_down
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return false;
        }
        let chat = self.chat_id.clone();
        let waiting: i64 = self
            .sh
            .db
            .snapshot(|t| {
                t.query_row(
                    "SELECT count(*) FROM submissions
                     WHERE chat_id = ?1 AND state IN ('saved', 'dispatching')",
                    [&chat],
                    |r| r.get(0),
                )
            })
            // A chat that cannot be read is not judged: it finishes as it always did.
            .unwrap_or(1);
        if waiting > 0 {
            return false;
        }
        let ended_at = now();
        let end = crate::seams::TurnEnd {
            run_id: run_id.clone(),
            ended_at,
        };
        if judge.offer(&self.chat_id, &end) != crate::seams::StopOffer::Hold {
            return false;
        }
        self.write(|t| {
            t.execute(
                "INSERT INTO chat_stop_holds(chat_id, since) VALUES (?1, ?2)
                 ON CONFLICT(chat_id) DO UPDATE SET since = excluded.since",
                (&chat, ended_at),
            )
        });
        self.stop_hold = Some(StopHold { run_id });
        self.live.lock().unwrap().thread.stop_check = Some(StopCheck {
            started_at: ended_at,
        });
        self.add_timer(self.sh.cfg.limits.stop_deadline, Timer::StopCheck);
        self.emit_thread();
        true
    }

    /// End the hold on the last turn's finish, whichever way it ended: the verdict, the
    /// deadline, a message, an archive, a stop, or the worker leaving. With `complete` the
    /// finish the hold deferred is written and announced now (exactly once); without it (the
    /// chat is carrying on) it is dropped. False when nothing was held.
    fn release_stop_hold(&mut self, complete: bool) -> bool {
        if self.stop_hold.take().is_none() {
            return false;
        }
        self.clear_timer(|t| matches!(t, Timer::StopCheck));
        let chat = self.chat_id.clone();
        self.write(|t| t.execute("DELETE FROM chat_stop_holds WHERE chat_id = ?1", [&chat]));
        self.live.lock().unwrap().thread.stop_check = None;
        self.emit_thread();
        if complete {
            self.complete_turn();
            self.notify_parent_unreported();
            self.recover_printed_tool_call();
        }
        true
    }

    /// Apply the stop judge's verdict to the held turn end of run `run_id`. A verdict for a hold
    /// that is gone (released by a message, the deadline, an archive) or for an earlier turn is
    /// refused, and the judge then sends nothing.
    fn stop_verdict(&mut self, run_id: &str, outcome: crate::seams::StopOutcome) -> bool {
        if self.stop_hold.as_ref().map(|h| h.run_id.as_str()) != Some(run_id) {
            return false;
        }
        use crate::seams::{StopKind, StopOutcome};
        match outcome {
            StopOutcome::Continue => {
                self.release_stop_hold(false);
            }
            StopOutcome::Stop { kind, line } => {
                let kind = match kind {
                    StopKind::Blocker => ContextKind::StopBlocker,
                    StopKind::Limit => ContextKind::StopLimit,
                    StopKind::Failure => ContextKind::StopFailure,
                    StopKind::Asked => ContextKind::StopAsked,
                };
                self.context_row(kind, &line);
                self.release_stop_hold(true);
            }
            StopOutcome::Done { line } => {
                if let Some(line) = line.filter(|l| !l.trim().is_empty()) {
                    self.context_row(ContextKind::Gatekeeper, &line);
                }
                self.release_stop_hold(true);
            }
            StopOutcome::Failed => {
                self.release_stop_hold(true);
            }
        }
        true
    }

    fn context_row(&mut self, kind: ContextKind, text: &str) {
        self.record(Entry::Context(ContextEntry {
            id: format!("c:{}", uuid::Uuid::new_v4()),
            parent_id: None,
            timestamp: now(),
            kind,
            text: text.to_owned(),
        }));
    }

    fn upsert_row(&self, row: &RowWrite) {
        let rev = self.sh.clock.next();
        let artifacts = self.sh.cfg.artifacts();
        let id = self.chat_id.clone();
        self.write(|t| session_log::upsert_row(t, &artifacts, &id, row, rev));
    }

    fn set_submission(&self, sid: &str, state: SubmissionState) {
        let st = store::state_str(state);
        self.write(|t| t.execute("UPDATE submissions SET state = ?1 WHERE id = ?2", (st, sid)));
    }

    fn submission(&self, sid: &str) -> Option<Submission> {
        self.sh
            .db
            .snapshot(|t| store::get_submission(t, sid))
            .ok()
            .flatten()
    }

    // ------------------------------------------------------------------ worker lifecycle

    fn ensure_worker(&mut self) -> bool {
        // A running worker lists Helm's web search (only) when its launch was offered it. When
        // the user enters or forgets the search key, or flips "Offer in Claude chats", that
        // answer no longer matches the running worker's tool list: close it so the launch below
        // lists the tools again, resuming the same session so the conversation carries over.
        // Never mid-turn: while the chat is working or its background agents are running the
        // relaunch waits for this idle point, so the change lands at the chat's next turn. A
        // chat with no worker needs nothing: its next launch reads the current setting.
        if self.link.is_some()
            && !self.link_closing
            && self.launched_web_tools
                != Some(crate::hostcalls::web_tools_offered(
                    &self.sh,
                    self.launched_local.is_some(),
                ))
        {
            if self.status().is_busy() || self.agents_running() {
                self.relaunch_on_idle = true;
            } else if let Some(l) = &self.link {
                self.link_closing = true;
                l.send(&json!({"type": "shutdown"}));
            }
        }
        // A worker that was told to shut down is winding down: its stdin closes and a prompt
        // handed to it would be lost with it. Drop the closing link (the process is already
        // ending itself) and start a fresh one below, so a send that lands in the gap between a
        // relaunch and the old worker's exit is never left uncertain. Not when a quit or an edit
        // is waiting for that exit: dropping the receiver would lose it, and the quit or the edit
        // would wait for ever. The send stays saved until the exit arrives.
        if self.link_closing && (!self.shutdown_replies.is_empty() || self.edit_waiting) {
            return false;
        }
        if self.link_closing {
            self.link = None;
            self.out = None;
            self.link_closing = false;
            self.ready = false;
            self.deferred.clear();
            if !self
                .sh
                .shutting_down
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                // A retirement the send overtook is off: the chat is working again.
                self.retiring = false;
            }
        }
        if self.link.is_some() {
            return true;
        }
        let Some(chat) = self.chat() else {
            return false;
        };
        let binding = ClaudeBinding::load(&self.sh.cfg.sessions(), &self.chat_id)
            .filter(|b| b.cwd == chat.cwd && b.harness_switch_id.is_none());
        let own = binding.as_ref().and_then(|b| b.native_session_id.clone());
        // A whole-chat fork's first launch resumes its source's session as a new one.
        let fork_of = binding
            .as_ref()
            .filter(|_| own.is_none())
            .and_then(|b| b.fork_of.clone());
        self.native_session = own.clone();
        let fork_session = fork_of.is_some();
        let resume = own.or(fork_of);
        // A resumed session's cost starts from its saved total, which earlier turns already counted.
        self.cost_base = resume
            .as_deref()
            .and_then(|s| {
                self.sh
                    .db
                    .snapshot(|t| crate::turn_usage::baseline(t, s))
                    .ok()
                    .flatten()
            })
            .unwrap_or_default();
        let sb = self.live.lock().unwrap().thread.send_behavior;
        self.launched_long = Some(sb.long_commands);
        self.relaunch_on_idle = false;
        self.provider_switch_pending = false;
        // A chat on a local model starts the worker for the name its preparation answered (for
        // Ollama, Helm's variant of the model).
        let local = local_of(&chat).map(|(server_id, model)| crate::runtime::LocalSpawn {
            server_id,
            model: self.prepared.take().unwrap_or(model),
            thinking_tokens: crate::local::thinking_tokens(chat.effort.as_deref()),
            server: None,
        });
        self.launched_local = local
            .as_ref()
            .map(|l| (l.server_id.clone(), l.model.clone()));
        // The launch lists web search on this answer, and the actor remembers it: a send that
        // finds the setting has since changed restarts the worker (above) so the tool list
        // follows the user's settings without making them wait for the next chat.
        let web_tools = crate::hostcalls::web_tools_offered(&self.sh, local.is_some());
        self.launched_web_tools = Some(web_tools);
        let req = SpawnRequest {
            chat_id: self.chat_id.clone(),
            cwd: chat.cwd.clone(),
            model: chat.model.clone(),
            effort: chat.effort.clone(),
            resume_session_id: resume,
            fork_session,
            // The chat's own fast-mode pick, if it has one; the spawner falls back to the
            // runtime file's default.
            fast_mode: chat.fast_mode,
            send_behavior: sb,
            host: crate::hostcalls::launch(
                &self.sh,
                &self.chat_id,
                &chat.cwd,
                local.is_some(),
                web_tools,
            ),
            local,
        };
        if let Some(t) = self.timeline.as_mut() {
            t.spawn = Some(now());
        }
        // The start itself runs on the blocking pool; a failure comes back as `SpawnFailed`.
        let (link, out) = crate::spawning::spawn_off_actor(&self.sh, req);
        self.link = Some(link);
        self.tree_base = 2;
        self.worker_started = Some(Instant::now());
        self.out = Some(out);
        self.ready = false;
        self.worker_error = None;
        {
            let mut live = self.live.lock().unwrap();
            live.has_worker = true;
            live.context_gen += 1;
            live.local_model = self.launched_local.as_ref().map(|(_, m)| m.clone());
        }
        // get_state goes out first, ahead of anything deferred.
        let id = self.new_id();
        self.pending.insert(id.clone(), Pending::Ready);
        if let Some(l) = &self.link {
            l.send(&json!({"type": "get_state", "id": id}));
        }
        true
    }

    /// The worker never started: nothing reached it, so its prompts are refused (not uncertain)
    /// and the run fails with the reason, as a start that failed on the spot always did.
    fn on_spawn_failed(&mut self, error: String) {
        log::error!("chat {}: {error}", self.chat_id);
        self.spawn_failed_at = Some(Instant::now());
        self.link = None;
        self.link_closing = false;
        self.out = None;
        self.ready = false;
        self.deferred.clear();
        self.launched_local = None;
        {
            let mut live = self.live.lock().unwrap();
            live.has_worker = false;
            live.local_model = None;
        }
        for (_, p) in std::mem::take(&mut self.pending) {
            match p {
                Pending::Prompt { submission_id } => {
                    self.finish_carry(&submission_id, false);
                    self.set_submission(&submission_id, SubmissionState::Rejected)
                }
                Pending::Withdraw { reply, .. } | Pending::ToolStop { reply } => {
                    let _ = reply.send(Err(error.clone()));
                }
                Pending::Compact { reply } => {
                    let _ = reply.send(Err(error.clone()));
                }
                Pending::Catalogue { reply } => {
                    let _ = reply.send(false);
                }
                _ => {}
            }
        }
        self.clear_timer(|t| matches!(t, Timer::Ack(_) | Timer::ModelAck(_)));
        self.fail_run(&error);
        if let Some(s) = self.stopping.take() {
            let status = self.status();
            for r in s.replies {
                let _ = r.send(status);
            }
        }
        for r in self.shutdown_replies.drain(..) {
            let _ = r.send(());
        }
        let quitting = self
            .sh
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst);
        if self.retiring || quitting {
            self.finished = true;
        }
        if std::mem::take(&mut self.edit_waiting) {
            self.apply_edit();
        }
        self.emit_thread();
    }

    fn on_ready(&mut self, resp: &Map<String, Value>) {
        if resp.get("success") != Some(&json!(true)) {
            let err = str_of(&Value::Object(resp.clone()), "error")
                .unwrap_or_else(|| "The worker could not start Claude Code.".into());
            self.fail_run(&err);
            if let Some(l) = &self.link {
                l.terminate();
            }
            return;
        }
        if let Some(t) = self.timeline.as_mut() {
            t.ready = Some(now());
            t.launch = resp.get("timing").cloned();
        }
        let data = resp.get("data").cloned().unwrap_or(Value::Null);
        let caps: Capabilities = data
            .get("capabilities")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default();
        let skills: Vec<SkillInfo> = data
            .get("skills")
            .and_then(|s| serde_json::from_value(s.clone()).ok())
            .unwrap_or_default();
        // Mark the skills that came from Helm's own skills folder (the CLI may show them under a
        // plugin prefix), so the `/` menu can say so; the marked list is what gets cached too.
        let own = helm_skills::skill_names(&self.sh.cfg.profile_dir);
        let skills: Vec<SkillInfo> = skills
            .into_iter()
            .map(|mut s| {
                s.scope = helm_skill_scope(&own, &s.name).to_string();
                s
            })
            .collect();
        {
            let mut live = self.live.lock().unwrap();
            live.thread.capabilities = caps;
            live.skills = skills;
        }
        // A chat on a local model lists that server's models, not Claude's: never cache them.
        let models =
            crate::catalogue::from_get_state(&data).filter(|_| self.launched_local.is_none());
        if let Some(models) = models {
            if crate::catalogue::store(&self.sh.cfg.profile_dir, models).is_ok() {
                self.emit(ChatEvent::Catalogue {
                    harness: Harness::Claude,
                });
            }
        }
        self.ready = true;
        self.sample_tree_base();
        // Registered start lines (for example `mcp_config`) go ahead of anything deferred.
        let start = crate::hostcalls::start_lines(&self.sh, &self.chat_id);
        for cmd in start.into_iter().chain(std::mem::take(&mut self.deferred)) {
            if let Some(l) = &self.link {
                l.send(&cmd);
            }
        }
        self.emit_thread();
    }

    /// In-flight submissions (sent, or accepted but not yet read) cannot be known once their worker
    /// is gone, and are never replayed.
    fn mark_in_flight_uncertain(&mut self) {
        let id = self.chat_id.clone();
        self.write(|t| {
            t.execute(
                "UPDATE submissions SET state = 'uncertain' WHERE chat_id = ?1 AND state IN ('dispatching', 'accepted')",
                [&id],
            )
        });
    }

    fn on_exit(&mut self, reason: String) {
        self.stop_ended = false;
        // The timers go with the worker, and a verdict could no longer be followed by a nudge.
        self.release_stop_hold(true);
        crate::hostcalls::sweep_tools_dir(&crate::hostcalls::tools_dir(
            &self.sh.cfg.profile_dir,
            &self.chat_id,
        ));
        self.link = None;
        self.link_closing = false;
        self.out = None;
        self.ready = false;
        self.deferred.clear();
        self.launched_local = None;
        {
            let mut live = self.live.lock().unwrap();
            live.has_worker = false;
            live.local_model = None;
        }
        let pending: Vec<Pending> = self.pending.drain().map(|(_, p)| p).collect();
        for p in pending {
            match p {
                Pending::Withdraw { reply, .. } => {
                    let _ = reply.send(Err("the worker stopped".into()));
                }
                Pending::ToolStop { reply } => {
                    let _ = reply.send(Err("the worker stopped".into()));
                }
                Pending::Compact { reply } => {
                    let _ = reply.send(Err("the worker stopped".into()));
                }
                Pending::Prompt { submission_id } => {
                    self.finish_carry(&submission_id, false);
                    self.set_submission(&submission_id, SubmissionState::Uncertain);
                }
                _ => {}
            }
        }
        self.timers.clear();
        // A switch waiting on this turn's stop is over: the outage watch carries a crashed
        // turn on in its own words, and a half-sent opener must not go out twice.
        self.switch_restart = None;
        // Whatever the worker had not settled is unknowable now, busy or not: a message left
        // "accepted" by a worker that went away must not outlive it.
        self.mark_in_flight_uncertain();
        if self.status().is_busy() {
            // A turn that was still waiting on its local model to be made ready dies with the
            // run: nothing reached the server, so what was typed goes back to the draft, and an
            // answer arriving later must not reopen the turn that just ended.
            if let Some(p) = self.prepare.take() {
                self.give_back(p.sid.into_iter().chain(p.held).collect());
                self.clear_loading();
            }
            if let Some((sid, held)) = self.start_wait.take() {
                self.give_back(std::iter::once(sid).chain(held).collect());
            }
            self.set_held_at_start(false);
            let error = if self
                .sh
                .shutting_down
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                helm_core::recovery::INTERRUPTED_ERROR.to_owned()
            } else if self.stopping.is_some() {
                // Stopped on purpose: never read as an outage to carry on from.
                not_an_outage(self.worker_error.clone().unwrap_or(reason))
            } else {
                self.worker_error.clone().unwrap_or(reason)
            };
            // Written before the status, so no reader sees `interrupted` while they still look in
            // flight (see `mark_in_flight_uncertain`).
            self.mark_in_flight_uncertain();
            if self.archived {
                // The archive stopped it: not a crash, so nothing to resume or explain.
                self.end_run(ChatStatus::Stopped, None, "stopped");
            } else {
                self.end_run(ChatStatus::Interrupted, Some(&error), "interrupted");
            }
            self.emit_pending();
            if let Some(s) = self.stopping.take() {
                // Stopped on purpose, the worker killed: a roll over the chat asked for is
                // dropped, as it is when the stop ends the turn quietly.
                crate::rollover::turn_stopped(&self.chat_id);
                self.settle_dropped(s.resend);
                for r in s.replies {
                    let _ = r.send(ChatStatus::Interrupted);
                }
            }
        }
        // Before the quit sequence hears the worker is gone, so the app cannot exit first.
        self.end_running_subagents("The worker stopped before this agent finished.");
        for r in self.shutdown_replies.drain(..) {
            let _ = r.send(());
        }
        if std::mem::take(&mut self.edit_waiting) {
            self.apply_edit();
        }
        // Sends that waited for this exit: handed over now (after an edit, into the turn it
        // opened), or left saved for the next start when the app is quitting.
        let held = std::mem::take(&mut self.held_for_exit);
        if !self
            .sh
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            for sid in held {
                self.dispatch(&sid);
            }
        }
        if self.retiring
            || self
                .sh
                .shutting_down
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.finished = true;
        }
        self.emit_thread();
    }

    /// The run failed before or outside a turn (spawn failure, refused get_state, model timeout).
    fn fail_run(&mut self, error: &str) {
        let id = self.chat_id.clone();
        self.write(|t| {
            t.execute(
                "UPDATE submissions SET state = 'rejected' WHERE chat_id = ?1 AND state = 'saved'",
                [&id],
            )
        });
        if self.status().is_busy() {
            self.end_run(ChatStatus::Interrupted, Some(error), "failed");
        }
        self.emit_pending();
    }

    // ------------------------------------------------------------------ runs

    fn open_run(&mut self) {
        if self.run_id.is_some() {
            return;
        }
        let run = self.new_run_id();
        let id = self.chat_id.clone();
        let chat = self.chat();
        self.write(|t| Self::insert_run(t, &run, &id, chat.as_ref()));
    }

    /// Start a run in memory: the caller writes the row (see `insert_run`).
    fn new_run_id(&mut self) -> String {
        // Whatever starts a turn ends a hold on the one before.
        self.release_stop_hold(true);
        self.last_output = Instant::now();
        self.stop_ended = false;
        let run = uuid::Uuid::new_v4().to_string();
        self.run_id = Some(run.clone());
        run
    }

    fn insert_run(
        t: &rusqlite::Transaction<'_>,
        run: &str,
        chat_id: &str,
        chat: Option<&ChatRecord>,
    ) -> rusqlite::Result<()> {
        t.execute(
            "INSERT INTO runs(id, chat_id, cwd, model, thinking, status, started_at) VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6)",
            rusqlite::params![
                run,
                chat_id,
                chat.map(|c| c.cwd.clone()),
                chat.and_then(|c| c.model.clone()),
                chat.and_then(|c| c.effort.clone()),
                now()
            ],
        )
        .map(|_| ())
    }

    /// Close the run, finish any live rows and tools, clear interactions, set the chat status.
    fn end_run(&mut self, status: ChatStatus, error: Option<&str>, run_status: &str) {
        crate::timeline::finish(
            &mut self.timeline,
            &self.sh.cfg.profile_dir,
            &self.chat_id,
            None,
        );
        self.flush_rows(true);
        let keys: Vec<String> = self.stream.keys().cloned().collect();
        for key in &keys {
            let id = self.chat_id.clone();
            let k = key.clone();
            self.write(|t| {
                t.execute(
                    "UPDATE messages SET status = 'interrupted' WHERE chat_id = ?1 AND source_key = ?2 AND status = 'streaming'",
                    [&id, &k],
                )
            });
        }
        self.stream.clear();
        self.turn_opened_by = None;
        self.turn_acted = false;
        if self.state.is_some() {
            self.publish_state(None);
        }
        let tool_keys: Vec<String> = self.tools.keys().map(|t| format!("t:{t}")).collect();
        for key in &tool_keys {
            let id = self.chat_id.clone();
            let k = key.clone();
            self.write(|t| {
                t.execute(
                    "UPDATE messages SET status = 'interrupted' WHERE chat_id = ?1 AND source_key = ?2 AND status = 'streaming'",
                    [&id, &k],
                )
            });
        }
        self.tools.clear();
        let mut all = keys;
        all.extend(tool_keys);
        self.emit_rows(&all);
        if let Some(run) = self.run_id.take() {
            let rs = run_status.to_owned();
            self.write(|t| {
                t.execute(
                    "UPDATE runs SET status = ?1, ended_at = ?2 WHERE id = ?3",
                    rusqlite::params![rs, now(), run],
                )
            });
        }
        {
            let mut live = self.live.lock().unwrap();
            live.thread.interactions.clear();
            live.thread.compacting = None;
            live.thread.loading = None;
        }
        // Held cards went with them: no hold timer may fire for a card that no longer exists.
        self.clear_timer(|t| matches!(t, Timer::GateDeadline(_)));
        self.set_status(status, error);
        self.save_binding();
        self.emit_thread();
    }

    /// Close the idle worker so the next send starts one with the current settings.
    fn relaunch_worker(&mut self) {
        self.relaunch_on_idle = false;
        self.provider_switch_pending = false;
        if let Some(l) = &self.link {
            self.link_closing = true;
            l.send(&json!({"type": "shutdown"}));
        }
    }

    /// Phase two of an edit, then send the edited message.
    fn apply_edit(&mut self) {
        // The log and Claude's session change under us: reopen both from scratch.
        self.log = None;
        self.native_session = None;
        let rev = self.sh.clock.next();
        match crate::edit::apply(&self.sh.db, &self.sh.cfg.profile_dir, &self.chat_id, rev) {
            Ok(Some(sid)) => {
                self.emit(ChatEvent::Refresh {
                    chat_id: self.chat_id.clone(),
                });
                self.emit_pending();
                self.emit_chat();
                self.dispatch(&sid);
            }
            Ok(None) => {}
            Err(e) => {
                log::error!("chat {}: edit not applied: {}", self.chat_id, e.message);
                let id = self.chat_id.clone();
                self.write(|t| {
                    t.execute(
                        "UPDATE submissions SET state = 'rejected' WHERE id = (SELECT submission_id FROM pending_edits WHERE chat_id = ?1)",
                        [&id],
                    )?;
                    t.execute("DELETE FROM pending_edits WHERE chat_id = ?1", [&id])
                });
                self.context_row(
                    ContextKind::Notice,
                    "The edit could not be applied; your message is kept so you can recover it.",
                );
                self.emit_pending();
            }
        }
    }

    /// Worker output arrived: note the activity (written at most every 15 s) and end a stall.
    fn note_output(&mut self) {
        self.last_output = Instant::now();
        if self.live.lock().unwrap().stalled {
            self.set_stalled(false);
        }
        // The model is answering this chat now: its first words end any wait.
        self.set_local_ahead(None);
        let due = self
            .activity_written
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(15));
        if due {
            self.activity_written = Some(Instant::now());
            let id = self.chat_id.clone();
            self.write(|t| {
                t.execute(
                    "INSERT INTO chat_activity(chat_id, last_activity_at) VALUES (?1, ?2)
                     ON CONFLICT(chat_id) DO UPDATE SET last_activity_at = excluded.last_activity_at",
                    rusqlite::params![id, now()],
                )
            });
        }
    }

    fn set_stalled(&self, on: bool) {
        self.live.lock().unwrap().stalled = on;
        let id = self.chat_id.clone();
        self.write(|t| {
            t.execute(
                "INSERT INTO chat_activity(chat_id, last_activity_at, stalled) VALUES (?1, ?2, ?3)
                 ON CONFLICT(chat_id) DO UPDATE SET stalled = excluded.stalled",
                rusqlite::params![id, now(), on],
            )
        });
        self.emit_chat();
        crate::attention::publish(&self.sh);
    }

    /// Store and announce how many chats stand ahead of this one at its model server (the chat
    /// record's `localAhead`; `set_stalled` above is the same shape). The header, the sidebar and
    /// the dock all read it from the record. `None` means not waiting.
    fn set_local_ahead(&self, on: Option<i64>) {
        {
            let mut live = self.live.lock().unwrap();
            if live.local_ahead == on {
                return;
            }
            live.local_ahead = on;
        }
        let id = self.chat_id.clone();
        self.write(|t| {
            t.execute(
                "INSERT INTO chat_activity(chat_id, last_activity_at, local_ahead) VALUES (?1, ?2, ?3)
                 ON CONFLICT(chat_id) DO UPDATE SET local_ahead = excluded.local_ahead",
                rusqlite::params![id, now(), on],
            )
        });
        self.emit_chat();
    }

    /// What the busy chat is doing right now, for the chip, the chat list and the silent check.
    /// A card waiting on the person outranks a tool call: the turn cannot move without them.
    /// Nothing here asks a model or a server — it reads only what the actor already knows.
    fn compute_state(&self, silent_limit: std::time::Duration) -> Option<crate::types::ChatState> {
        use crate::types::ChatState;
        if !self.status().is_busy() {
            return None;
        }
        if !self.live.lock().unwrap().thread.interactions.is_empty() {
            return Some(ChatState::WaitingUser);
        }
        if let Some(rec) = self.tools.values().min_by_key(|r| r.started_at) {
            return Some(ChatState::Tool {
                name: rec.name.clone(),
                since: rec.started_at,
            });
        }
        {
            // A turn start held at the chat's start — by the server's cap, the plan windows or
            // the app's limit — is queued whoever counts the line.
            let live = self.live.lock().unwrap();
            let reason = live.hold_reason;
            if reason.is_some() || live.local_ahead.is_some() {
                // A Claude hold carries the gating window's own words: how full it reads and
                // when it reopens. The reading is the gate's (`capacity`); unknown answers
                // nothing, exactly as in the gate.
                let cap = (reason == Some(crate::queue::CLAUDE_CAPACITY))
                    .then(|| helm_core::usage_windows::capacity(&self.sh.db, now()))
                    .flatten();
                return Some(ChatState::Queued {
                    ahead: live.local_ahead.filter(|a| *a >= 0),
                    reason: reason.map(str::to_owned),
                    pct: cap.map(|c| c.pct),
                    until: cap.and_then(|c| c.until),
                });
            }
        }
        let last_ms = now() - self.last_output.elapsed().as_millis() as i64;
        if self.last_output.elapsed() >= silent_limit {
            return Some(ChatState::Silent { since: last_ms });
        }
        Some(ChatState::Generating { since: last_ms })
    }

    /// Store and announce the live state (the `chat_activity.state` column, read into the chat
    /// record's `state`); writes only when it changed.
    fn publish_state(&mut self, state: Option<crate::types::ChatState>) {
        if self.state == state {
            return;
        }
        self.state = state.clone();
        let json = state.as_ref().and_then(|s| serde_json::to_string(s).ok());
        let id = self.chat_id.clone();
        self.write(|t| {
            t.execute(
                "INSERT INTO chat_activity(chat_id, last_activity_at, state) VALUES (?1, ?2, ?3)
                 ON CONFLICT(chat_id) DO UPDATE SET state = excluded.state",
                rusqlite::params![id, now(), json],
            )
        });
        self.emit_chat();
    }

    /// A child chat went silent: say so in its parent, so an orchestrator learns of it without
    /// polling. The stalled flag on the child covers the person watching the board; this covers
    /// the chat that made the child.
    ///
    /// The note goes only to a parent that is busy: it joins the turn already open, as a message
    /// does. An idle parent is left idle (a timer must not start a model call), and finds the
    /// child's silence in the chat list when it next looks. A child tells its parent once per
    /// [`SILENT_NOTE_GAP`] at most, however often it flips between silent and talking.
    fn notify_parent_silent(&mut self, since: i64) {
        let Some(chat) = self.chat() else { return };
        let Some(parent) = chat.parent_chat_id.clone() else {
            return;
        };
        if self
            .silent_noted_at
            .is_some_and(|t| t.elapsed() < SILENT_NOTE_GAP)
        {
            return;
        }
        let parent_busy = self
            .sh
            .actors
            .lock()
            .unwrap()
            .get(&parent)
            .is_some_and(|h| h.live.lock().unwrap().thread.status.is_busy());
        if !parent_busy {
            return;
        }
        self.silent_noted_at = Some(Instant::now());
        let minutes = ((now() - since) / 60_000).max(1);
        let text = format!(
            "Helm noticed: your subchat \"{}\" has been silent for {minutes} minute{} — its turn \
             is open with no output, no tool call and no queue wait. Check it, or send it a \
             message or stop it if it looks lost.",
            chat.title,
            if minutes == 1 { "" } else { "s" }
        );
        self.send_note_to_parent(parent, text, "silent-child");
    }

    /// A child's turn ended without reporting: tell its parent once, so an orchestrator whose
    /// child stalled by ending early (a tool call printed as text, say) hears of it. Unlike the
    /// silent note, this goes to an idle parent too: a turn ending is an event, not a timer, and
    /// the idle parent is the one that would otherwise wait for ever. Once per turn end, and the
    /// flag resets when the child works again.
    fn notify_parent_unreported(&mut self) {
        if self.unreported_noted {
            return;
        }
        let hour = std::time::Duration::from_secs(3600);
        self.unreported_at.retain(|t| t.elapsed() < hour);
        if self.unreported_at.len() >= UNREPORTED_NOTES_PER_HOUR {
            return;
        }
        let id = self.chat_id.clone();
        let found = self
            .sh
            .db
            .snapshot(|tx| crate::childnote::unreported_turn(tx, &id));
        let Ok(Some((parent, why))) = found else {
            return;
        };
        let Some(chat) = self.chat() else { return };
        self.unreported_noted = true;
        self.unreported_at.push(Instant::now());
        self.send_note_to_parent(
            parent,
            crate::childnote::note_text(&chat.title, why),
            "unreported-child",
        );
    }

    /// Send `text` into the parent as the app, off the actor's task. A refusal is logged only.
    fn send_note_to_parent(&self, parent: String, text: String, what: &'static str) {
        let sh = self.sh.clone();
        self.sh.handle.spawn(async move {
            let rt = crate::runtime::ChatRuntime { sh };
            let sid = uuid::Uuid::new_v4().to_string();
            let sent = crate::submit::host_send(
                &rt,
                crate::submit::HostSend {
                    chat_id: &parent,
                    submission_id: &sid,
                    text: &text,
                    origin: crate::types::SubmissionOrigin::App,
                    page: None,
                },
            );
            if let Err(e) = sent {
                log::debug!("{what} note not sent to {parent}: {}", e.message);
            }
        });
    }

    /// A chat on a local model whose turn wrote a tool call as text: the model wrote markup
    /// nobody runs, and that call changed nothing — even when the turn went on to run real
    /// calls and close with words. Once per run of such turns it gets one corrective message
    /// telling it so; if the next turn ends the same way, the person is flagged to step in or
    /// switch the model, and the chat is left alone. A Claude-model chat gets neither: its
    /// harness runs that markup for real. The parent note (`notify_parent_unreported`) still
    /// fires alongside, on how the turn closed.
    fn recover_printed_tool_call(&mut self) {
        if self.launched_local.is_none() {
            return;
        }
        let id = self.chat_id.clone();
        let found = self
            .sh
            .db
            .snapshot(|tx| crate::childnote::printed_call_text(tx, &id));
        let Ok(Some(markup)) = found else {
            // The turn closed some other way (or the read failed): any run of printed calls is
            // over, and the next one earns the nudge again.
            self.printed_run = 0;
            return;
        };
        self.printed_run += 1;
        match self.printed_run {
            1 => self.send_printed_call_nudge(&markup),
            2 => self.flag_printed_tool_call(),
            _ => {}
        }
    }

    /// The corrective message into the chat itself, off the actor's task and as the app: fresh,
    /// it opens the next turn. It names the tool when the markup names one, since that is the
    /// case of a model calling a tool its chat does not have.
    fn send_printed_call_nudge(&self, markup: &str) {
        let mut text = "Helm noticed: your turn wrote a tool call as text, so that call did not \
                        run. Never write tool-call markup: call the tools in your tool list \
                        directly, and when the tool you want is not in it, call it through \
                        app_mcp_call (app_list_tools shows what is available)."
            .to_owned();
        if let Some(tool) = crate::childnote::named_tool(markup) {
            text.push_str(&format!(" The call you wrote named \"{tool}\"."));
        }
        let sh = self.sh.clone();
        let chat = self.chat_id.clone();
        self.sh.handle.spawn(async move {
            let rt = crate::runtime::ChatRuntime { sh };
            let sid = uuid::Uuid::new_v4().to_string();
            let sent = crate::submit::host_send(
                &rt,
                crate::submit::HostSend {
                    chat_id: &chat,
                    submission_id: &sid,
                    text: &text,
                    origin: crate::types::SubmissionOrigin::App,
                    page: None,
                },
            );
            if let Err(e) = sent {
                log::debug!("printed-call nudge not sent to {chat}: {}", e.message);
            }
        });
    }

    /// The second printed call in a row: the nudge did not help, and only the person can — by
    /// nudging again or switching the chat to a Claude model. Put it in Needs attention.
    fn flag_printed_tool_call(&self) {
        let id = self.chat_id.clone();
        let filed = self.write(|t| {
            crate::flags::file(
                t,
                &id,
                "This chat keeps writing tool calls as text instead of running them",
                "Printed a tool call twice; try again or switch this chat to a Claude model.",
                "normal",
                now(),
            )
        });
        if filed
            .flatten()
            .is_some_and(|f| f != crate::flags::Filed::Duplicate)
        {
            crate::attention::publish(&self.sh);
        }
    }

    /// The maintenance tick's other question about a busy chat: is its turn waiting on a local
    /// model that answers one request at a time? Says so on the record when it is. Only asks the
    /// server while a turn of ours is open against it, and only every [`QUEUE_STALE`].
    fn check_local_wait(&self) {
        // The launched model, not the chat's current pick: a switch queued for the next idle moment
        // must not change which server this wait is measured against.
        let Some(server) = self
            .launched_local
            .as_ref()
            .map(|(server, _)| server.clone())
        else {
            self.set_local_ahead(None);
            return;
        };
        // While the model is still being made ready (loading, starting), the loading line already
        // says what is happening; no request of ours is at the server yet to wait behind others.
        if self.prepare.is_some()
            || !self.status().is_busy()
            || self.last_output.elapsed() < self.sh.cfg.limits.stall_unit
        {
            self.set_local_ahead(None);
            return;
        }
        // A tool call in flight: the silence is the tool's, not the queue's.
        if !self.tools.is_empty() {
            self.set_local_ahead(None);
            return;
        }
        let mine = self.live.lock().unwrap().turn_opened_ms;
        let ahead = self.chats_ahead_on(&server, mine);
        let seen = self.server_queue(&server);
        let waiting = ahead > 0 || seen.is_some_and(|q| q.busy || q.queued.unwrap_or(0) > 0);
        self.set_local_ahead(if waiting { Some(ahead as i64) } else { None });
    }

    /// How many other chats of ours have an open turn on this server whose request got there
    /// before this chat's. Counts Helm's own chats only: the server cannot tell whose request is
    /// whose. An unorderable pair is not counted (under-count beats a wrong number).
    fn chats_ahead_on(&self, server: &str, mine: Option<i64>) -> usize {
        let handles: Vec<crate::runtime::ActorHandle> = self
            .sh
            .actors
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| **id != self.chat_id)
            .map(|(_, h)| h.clone())
            .collect();
        let mut n = 0;
        for h in handles {
            let l = h.live.lock().unwrap();
            if !l.thread.status.is_busy() || l.server_id.as_deref() != Some(server) {
                continue;
            }
            if matches!((mine, l.turn_opened_ms), (Some(m), Some(o)) if o < m) {
                n += 1;
            }
        }
        n
    }

    /// What the server says about requests waiting on it, from the cache; starts a read on the
    /// blocking pool when the cache has aged out. A read that comes back saying nothing keeps
    /// the previous answer. Reachability is not touched either way — this read says nothing about
    /// a server being up.
    fn server_queue(&self, server: &str) -> Option<crate::local::QueueInfo> {
        let seam = self.sh.local.get().cloned()?;
        let mut fetch = false;
        let last = {
            let mut map = self.sh.local_queue.lock().unwrap();
            let e = map
                .entry(server.to_owned())
                .or_insert(crate::runtime::QueueSeen {
                    at: Instant::now() - QUEUE_STALE,
                    info: None,
                    good_at: None,
                    fetching: false,
                });
            if e.at.elapsed() >= QUEUE_STALE && !e.fetching {
                e.fetching = true;
                fetch = true;
            }
            // Only a reading that is still recent counts, so a server that stops answering
            // reads as unknown rather than busy with whatever it last said.
            recent_reading(e.info, e.good_at)
        };
        if fetch {
            let sh = self.sh.clone();
            let id = server.to_owned();
            self.sh.handle.spawn_blocking(move || {
                let info = seam.queue(&id);
                let mut map = sh.local_queue.lock().unwrap();
                let e = map.entry(id).or_insert(crate::runtime::QueueSeen {
                    at: Instant::now(),
                    info: None,
                    good_at: None,
                    fetching: false,
                });
                // A failed or shapeless read says nothing, so it does not erase what the last
                // good read said. The next good answer replaces it; the read keeps being retried
                // every QUEUE_STALE while a chat asks.
                if info.is_some() {
                    e.info = info;
                    e.good_at = Some(Instant::now());
                }
                e.at = Instant::now();
                e.fetching = false;
            });
        }
        last
    }

    /// A clean finish: write the completion, announce it once, refresh the chat and attention.
    fn complete_turn(&mut self) {
        let id = self.chat_id.clone();
        let done = self
            .write(|t| crate::manage::record_completion(t, &id, now()))
            .flatten();
        if let Some(c) = done {
            self.emit(ChatEvent::Completion { completion: c });
        }
        self.emit_chat();
        crate::attention::publish(&self.sh);
    }

    fn save_binding(&mut self) {
        let Some(chat) = self.chat() else { return };
        let leaf = self
            .log
            .as_ref()
            .and_then(|l| l.last_id().map(str::to_owned));
        let b = ClaudeBinding {
            native_session_id: self.native_session.clone(),
            cwd: chat.cwd,
            last_leaf_id: leaf,
            harness_switch_id: None,
            updated_at: now(),
            fork_of: None,
        };
        if b.native_session_id.is_some() {
            if let Err(e) = b.save(&self.sh.cfg.sessions(), &self.chat_id) {
                log::error!("chat {}: binding not saved: {e}", self.chat_id);
            }
        }
    }

    /// After a Stop: everything Claude had not read goes to it again right away — the first
    /// message opens the next turn, the rest are steered in at its first boundary. Nothing is
    /// held and nothing is dropped.
    fn settle_dropped(&mut self, resend: bool) {
        let chat = self.chat_id.clone();
        if self.archived || !resend {
            // An archived chat sends nothing, and neither does a stop that asked for none: what
            // Claude had not read reads "not sent", so the text can be taken back (after a
            // restore, for an archived chat).
            self.write(|t| {
                t.execute(
                    "UPDATE submissions SET state = 'uncertain'
                     WHERE chat_id = ?1 AND state IN ('saved', 'accepted')",
                    [&chat],
                )
            });
            self.emit_pending();
            return;
        }
        let quitting = self
            .sh
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst);
        if quitting || self.retiring {
            return; // still saved: the next start sends them
        }
        let ids: Vec<String> = self
            .sh
            .db
            .snapshot(|t| {
                let mut st = t.prepare(
                    "SELECT id FROM submissions
                     WHERE chat_id = ?1 AND state = 'saved' ORDER BY created_at, id LIMIT 100",
                )?;
                let rows = st.query_map([&chat], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()
            })
            .unwrap_or_default();
        for (i, sid) in ids.iter().enumerate() {
            // The first opens the turn; the rest are read at its first boundary, in order.
            let mode = if i == 0 { "prompt" } else { "steer" };
            self.write(|t| {
                t.execute(
                    "UPDATE submissions SET mode = ?1 WHERE id = ?2",
                    rusqlite::params![mode, sid],
                )
            });
            self.dispatch(sid);
        }
    }

    // ------------------------------------------------------------------ commands

    fn on_cmd(&mut self, c: Cmd) {
        // The runtime's own housekeeping is not use: a tick, the send behaviour every tick hands
        // to every chat, an AFK sweep or a retirement must not make this worker look newer than
        // another's, or the idle cap retires the wrong one.
        if !matches!(
            c,
            Cmd::Retire | Cmd::StallCheck { .. } | Cmd::SendBehavior(_) | Cmd::AfkSweep
        ) {
            self.touch();
        }
        if self.archived {
            match &c {
                // Nothing starts or sends for an archived chat.
                Cmd::Dispatch(sid) => {
                    self.set_submission(sid, SubmissionState::Rejected);
                    self.emit_pending();
                    return;
                }
                Cmd::Prewarm | Cmd::Warm => return,
                _ => {}
            }
        }
        match c {
            Cmd::Dispatch(sid) => self.dispatch(&sid),
            Cmd::ContextRow { kind, text } => self.context_row(kind, &text),
            Cmd::AfkSweep => {
                let waiting: Vec<Interaction> =
                    self.live.lock().unwrap().thread.interactions.clone();
                for i in waiting {
                    // A card the question gate is still holding is not AFK's yet: the gate's
                    // verdict, or the hold ending, decides what happens to it. A card the gate
                    // handed to the user was judged to need them, so AFK's automatic answer
                    // must not take it either: it waits for the user.
                    if i.gate.as_ref().is_some_and(|g| {
                        matches!(g.state, GateStatus::Checking | GateStatus::Escalated)
                    }) {
                        continue;
                    }
                    if self.afk_handle(&i, true) {
                        self.live
                            .lock()
                            .unwrap()
                            .thread
                            .interactions
                            .retain(|x| x.id != i.id);
                    }
                }
                self.emit_thread();
            }
            Cmd::Stop { reply } => self.stop(reply, true),
            Cmd::StopNoResend { reply } => self.stop(reply, false),
            Cmd::Answer(mut fields) => {
                let rid = fields
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                // The user answering ends any hold on that card; the gate's later verdict,
                // if one is on its way, will find nothing and answer false.
                self.clear_timer(|t| *t == Timer::GateDeadline(rid.clone()));
                fields.insert("type".into(), json!("extension_ui_response"));
                self.send(Value::Object(fields));
                self.live
                    .lock()
                    .unwrap()
                    .thread
                    .interactions
                    .retain(|i| i.id != rid);
                self.emit_thread();
            }
            Cmd::GateVerdict {
                interaction_id,
                outcome,
                reply,
            } => {
                let applied = self.gate_verdict(&interaction_id, *outcome);
                let _ = reply.send(applied);
            }
            Cmd::StopVerdict {
                run_id,
                outcome,
                reply,
            } => {
                let applied = self.stop_verdict(&run_id, outcome);
                let _ = reply.send(applied);
            }
            Cmd::ApplyModel { model, effort } => {
                // A send that is waiting on the old model's preparation must not go out on it:
                // take that preparation down first and put the send back through the model the
                // chat has now, once the change below has settled the worker.
                let waiting = if model.is_some() {
                    self.drop_stale_prepare()
                } else {
                    None
                };
                self.apply_model(model, effort);
                if let Some((sid, held)) = waiting {
                    self.restart_prepare(sid, held);
                }
            }
            Cmd::ToolStop {
                tool_call_id,
                reply,
            } => {
                let subagent =
                    self.live.lock().unwrap().thread.subagents.iter().any(|s| {
                        s.tool_call_id == tool_call_id && s.status == SubagentStatus::Running
                    });
                if self.link.is_none() || !(self.tools.contains_key(&tool_call_id) || subagent) {
                    let _ = reply.send(Err("That tool call is not running.".into()));
                    return;
                }
                self.request(
                    json!({"type": "stop_tool", "toolCallId": tool_call_id, "sentAtMs": now()}),
                    Pending::ToolStop { reply },
                );
            }
            Cmd::Compact { reply } => {
                if self.status().is_busy() {
                    let _ = reply.send(Err("Compaction waits until the turn has finished.".into()));
                    return;
                }
                if self.link.is_none() && self.is_local_chat() {
                    // Compacting asks the model, which is only made ready when a turn opens.
                    let _ = reply.send(Err(
                        "Send a message first: Helm gets the local model ready when a turn starts, and compaction needs it."
                            .into(),
                    ));
                    return;
                }
                if !self.ensure_worker() {
                    let _ = reply.send(Err("The worker could not start.".into()));
                    return;
                }
                self.request(json!({"type": "compact"}), Pending::Compact { reply });
            }
            Cmd::Withdraw {
                submission_id,
                reply,
            } => {
                if self.link.is_none() {
                    let _ = reply.send(Ok(false));
                    return;
                }
                self.request(
                    json!({"type": "withdraw", "submissionId": submission_id}),
                    Pending::Withdraw {
                        submission_id,
                        reply,
                    },
                );
            }
            Cmd::Shutdown { reply } => {
                if self.link.is_none() {
                    let _ = reply.send(());
                    self.finished = true;
                    return;
                }
                self.shutdown_replies.push(reply);
                self.flush_rows(true);
                if let Some(l) = &self.link {
                    self.link_closing = true;
                    l.send(&json!({"type": "shutdown"}));
                }
            }
            Cmd::Terminate => {
                if let Some(l) = &self.link {
                    l.terminate();
                }
            }
            Cmd::SendBehavior(sb) => {
                let changed = {
                    let mut live = self.live.lock().unwrap();
                    let changed = live.thread.send_behavior != sb;
                    live.thread.send_behavior = sb;
                    changed
                };
                if changed {
                    self.emit_thread();
                }
                if self.link.is_some() && self.launched_long.is_some_and(|l| l != sb.long_commands)
                {
                    // Running background agents live in the worker too: wait for them.
                    if self.status().is_busy() || self.agents_running() {
                        self.relaunch_on_idle = true;
                    } else {
                        self.relaunch_worker();
                    }
                }
            }
            Cmd::WorkerLine(line) => {
                // No worker: nothing to tell (the next one gets the start lines afresh).
                if let Some(l) = &self.link {
                    if self.ready {
                        l.send(&line);
                    } else {
                        self.deferred.push(line);
                    }
                }
            }
            Cmd::RefreshCatalogue { reply } => {
                if self.link.is_some() && self.ready {
                    self.request(json!({"type": "get_state"}), Pending::Catalogue { reply });
                } else {
                    let _ = reply.send(false);
                }
            }
            Cmd::ApplyEdit => {
                if self.link.is_some() {
                    // Phase two needs the worker closed: shut it down and apply on exit.
                    self.edit_waiting = true;
                    if let Some(l) = &self.link {
                        self.link_closing = true;
                        l.send(&json!({"type": "shutdown"}));
                    }
                } else {
                    self.apply_edit();
                }
            }
            Cmd::Prewarm => {
                let quitting = self
                    .sh
                    .shutting_down
                    .load(std::sync::atomic::Ordering::SeqCst);
                let backing_off = self
                    .spawn_failed_at
                    .is_some_and(|t| t.elapsed() < PREWARM_RETRY);
                // A local chat's worker waits for its first send: the model is made ready then.
                if self.link.is_none()
                    && !self.status().is_busy()
                    && !quitting
                    && !backing_off
                    && !self.is_local_chat()
                {
                    self.ensure_worker();
                }
            }
            Cmd::Warm => self.warm_now(),
            Cmd::StallCheck { after, local_after } => {
                self.check_local_wait();
                // A turn held at the start — by the server's cap, by the plan windows or by
                // hand — gets first look at a freed place. This runs after the queue-position
                // check: a chat held before its turn ever opened has no launched model, so
                // nothing else would keep its place on show. Reopening goes through the same
                // gate that held it: the gate answers again, keeps the place if the wait still
                // stands, and lifts the hold row the moment the reason has gone.
                if let Some((sid, held)) = self.start_wait.take() {
                    self.restart_prepare(sid, held);
                }
                // What the chat is doing right now, then the silent question: a turn that is
                // queued, generating, running a tool or waiting on the person is doing
                // something; only the state with none of those, past its limit, is silent.
                let base = if self.is_local_chat() {
                    local_after
                } else {
                    after
                };
                // Something this chat started (a build) is still in the worker's process tree:
                // its silence is that process working, so the limit stretches.
                let busy_tree = self.link.as_ref().and_then(|l| l.tree_size()).unwrap_or(0);
                let limit = if busy_tree > self.tree_base {
                    base * 3
                } else {
                    base
                };
                let state = self.compute_state(limit);
                let silent_now = matches!(state, Some(crate::types::ChatState::Silent { .. }));
                // A turn held at its start for longer than the silent limit is flagged too, so a
                // line that never moves does not read as "Queued" for ever. It stays Queued: it
                // is waiting, not silent, and a parent is not told it is lost.
                let held_long = self.start_wait.is_some() && self.last_output.elapsed() >= limit;
                let stalled = self.live.lock().unwrap().stalled;
                self.publish_state(state.clone());
                if silent_now && !stalled {
                    self.set_stalled(true);
                    if let Some(crate::types::ChatState::Silent { since }) = state {
                        self.notify_parent_silent(since);
                    }
                } else if held_long && !stalled {
                    self.set_stalled(true);
                } else if !silent_now && !held_long && stalled {
                    self.set_stalled(false);
                }
            }
            Cmd::LocalNote { token, text } => {
                if self.prepare.as_ref().is_some_and(|p| p.token == token) {
                    // Remember that a line is up: its preparation's end is what clears it.
                    if let Some(p) = self.prepare.as_mut() {
                        p.line = true;
                    }
                    self.set_loading(text);
                }
            }
            Cmd::LocalPrepared { token, result } => self.on_prepared(token, result),
            Cmd::Archived => self.on_archived(),
            Cmd::Restored => {
                self.archived = false;
                if self.stopping.is_some() {
                    // The short archive deadline no longer applies: the stop already under way
                    // gets the ordinary stop time instead.
                    self.clear_timer(|t| matches!(t, Timer::StopDeadline));
                    let lim = self.sh.cfg.limits.stop;
                    self.add_timer(lim, Timer::StopDeadline);
                }
                // A worker still closing stays closed; the next send starts a new one.
                if !self
                    .sh
                    .shutting_down
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    self.retiring = false;
                }
            }
            Cmd::QueueSeq(seq) => {
                // The queue re-ordered this chat's held start; the row is written, this keeps
                // the actor's own place in step. It stays where the queue put it until its
                // next gate.
                self.live.lock().unwrap().held_at_start = Some(seq);
            }
            Cmd::QueueCancel { keep_park } => {
                // The queue ended this chat's held start: what waited goes back to its owners
                // (typed text to the draft, as ever), and the hold and its record go.
                let waiting = self.start_wait.take();
                let was_waiting = waiting.is_some();
                if let Some((sid, held)) = waiting {
                    self.give_back(std::iter::once(sid).chain(held).collect());
                }
                self.set_local_ahead(None);
                self.set_held_at_start(false);
                self.set_queue_reason(None);
                // With the wait gone there is nothing for a Start anyway to force.
                self.start_anyway = false;
                self.live.lock().unwrap().hold_class = None;
                if !keep_park {
                    crate::queue::clear_hold(&self.sh.db, &self.chat_id);
                }
                // A held start sat at Starting; with nothing waiting the chat is simply idle.
                // A chat that only carries a hand park while its turn runs has no wait to end:
                // its turn goes on, and only the park goes.
                if was_waiting {
                    self.end_run(ChatStatus::Idle, None, "cancelled");
                }
                self.emit_pending();
            }
            Cmd::QueueStartAnyway => {
                // The user overrode the Claude hold for this chat's waiting send: ask the gate
                // again at once, with the windows answered open. The Go path consumes the
                // one-shot; a start that still waits for another reason keeps it for its start.
                // The flag arms only while the chat really is held by the Claude windows with a
                // send waiting at its start: a hold row left by a worker crash has no send to
                // open, and an armed flag with nothing to spend it would drift onto the chat's
                // next send.
                if self.live.lock().unwrap().hold_reason == Some(crate::queue::CLAUDE_CAPACITY) {
                    if let Some((sid, held)) = self.start_wait.take() {
                        self.start_anyway = true;
                        self.restart_prepare(sid, held);
                    }
                }
            }
            Cmd::Retire => {
                if self.link.is_none() {
                    // No worker to shut down: the actor itself goes (archive, delete).
                    self.finished = true;
                } else if !self.status().is_busy() && self.stop_hold.is_none() {
                    // A chat whose turn end is being checked is not closed under the check.
                    self.retiring = true;
                    if let Some(l) = &self.link {
                        self.link_closing = true;
                        l.send(&json!({"type": "shutdown"}));
                    }
                }
            }
        }
    }

    /// The chat was archived: retire the idle worker, or stop the running turn with a short
    /// deadline (the kill path is the ordinary stop deadline). Nothing sends from here on.
    fn on_archived(&mut self) {
        self.archived = true;
        // An archived chat rolls over no more: a roll over it asked for is dropped.
        crate::rollover::turn_stopped(&self.chat_id);
        // An archived chat announces nothing (the completion is not written for it), but the hold
        // and the busy mark go.
        self.release_stop_hold(true);
        if !self.status().is_busy() {
            self.on_cmd(Cmd::Retire);
            return;
        }
        self.retiring = true;
        let (reply, _) = oneshot::channel();
        self.stop(reply, false);
        if self.stopping.is_some() {
            self.clear_timer(|t| matches!(t, Timer::StopDeadline));
            let lim = self.sh.cfg.limits.archive_stop;
            self.add_timer(lim, Timer::StopDeadline);
        }
    }

    fn dispatch(&mut self, sid: &str) {
        #[cfg(test)]
        let before = write_counts::get(&self.chat_id);
        self.dispatch_inner(sid);
        // The actor is one thread and the only user of `write` for its chat, so the difference is
        // exactly this dispatch's own transactions.
        #[cfg(test)]
        write_counts::set_last_dispatch(&self.chat_id, write_counts::get(&self.chat_id) - before);
    }

    /// A send that found the worker closing for a quit or an edit waits, still saved, for the
    /// exit; see `ensure_worker`.
    fn hold_until_exit(&mut self, sid: &str) {
        if self.link_closing && !self.held_for_exit.iter().any(|h| h == sid) {
            self.held_for_exit.push(sid.to_owned());
        }
    }

    fn dispatch_inner(&mut self, sid: &str) {
        let Some(sub) = self.submission(sid) else {
            return;
        };
        // Only a message still waiting to be handed over is sent. Every sender saves the row
        // before asking for a dispatch, and this is where it stops being saved, so a second ask
        // for the same message (start-up hands over everything unsent, a settle sends what was
        // returned) never writes a second prompt.
        if sub.state != SubmissionState::Saved {
            return;
        }
        // A message ends any hold on the last turn's finish: that is written and announced first,
        // exactly as it would have been, and the judge's late verdict then finds nothing.
        self.release_stop_hold(true);
        let priority: Option<String> = self
            .sh
            .db
            .snapshot(|t| {
                use rusqlite::OptionalExtension;
                t.query_row(
                    "SELECT priority FROM submission_meta WHERE submission_id = ?1",
                    [sid],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
            })
            .ok()
            .flatten()
            .flatten();
        if let Some(w) = self.start_wait.as_mut() {
            // The turn is waiting at the model server's start capacity: this send waits in
            // front of it, and the maintenance tick reopens the pair when a place frees.
            if w.0 != sid && !w.1.iter().any(|h| h == sid) {
                w.1.push(sid.to_owned());
            }
            return;
        }
        // A warm-up started for the model the chat has since left cannot answer for this send:
        // drop it first (its late answers are ignored by token) and prepare the current model.
        let stale = self.prepare.as_ref().is_some_and(|p| {
            p.sid.is_none() && self.chat().and_then(|c| local_of(&c)) != Some(p.target.clone())
        });
        if stale {
            let had_line = self.prepare.as_ref().is_some_and(|p| p.line);
            self.prepare = None;
            if had_line {
                self.clear_loading();
            }
        }
        if let Some(p) = self.prepare.as_mut() {
            // A warm-up for this chat's next turn is running with no send yet: this send takes it
            // over instead of starting a second preparation, and the answer dispatches it as if
            // the preparation had been made for it. Only now does the chat show a turn opening.
            let takeover = p.sid.is_none();
            if takeover {
                p.sid = Some(sid.to_owned());
            } else if p.sid.as_deref() != Some(sid) && !p.held.iter().any(|h| h == sid) {
                // The model of the turn that is opening is still being made ready: this send
                // waits behind it.
                p.held.push(sid.to_owned());
            }
            if takeover {
                self.last_output = Instant::now();
                self.set_status(ChatStatus::Starting, None);
            }
            return;
        }
        let opening = self.opening.as_deref() == Some(sid);
        if opening {
            self.opening = None;
        }
        let busy = !opening && self.status().is_busy();
        if !busy && !opening && self.begin_prepare(sid) {
            return;
        }
        if !busy && self.provider_switch_pending && self.link.is_some() && !self.link_closing {
            // The model changed provider while background agents kept the old worker alive. This
            // message opens a turn, and the old worker would answer it on the wrong provider (for
            // a switch to a local model, under the user's own sign-in): so the agents are stopped
            // now, with a notice, and the message goes to a fresh worker. A switch made while the
            // message only waited on its model's preparation stopped no agents, so says nothing.
            if self.agents_running() {
                self.context_row(
                    ContextKind::Notice,
                    "Background agents were stopped to switch this chat to its new model.",
                );
            }
            self.relaunch_worker();
        }
        if !busy {
            let mut t = crate::timeline::SendTimeline::new(sid, sub.created_at, now());
            t.warm = self.ready;
            t.warming = self.link.is_some() && !self.ready;
            self.timeline = Some(t);
            // Start the worker first, so it boots on the blocking pool while the
            // turn's writes below go to disk. A chat whose record is gone stops here.
            if !self.ensure_worker() {
                self.hold_until_exit(sid);
                return;
            }
        }
        if !self.ensure_worker() {
            self.hold_until_exit(sid);
            return;
        }
        // The turn's opening writes go in one transaction (one fsync, not four): the status, the
        // run, the user row and the submission's 'dispatching' state. The submission is saved as
        // 'dispatching' before the prompt line is written (never resend).
        let opens_run = !busy && self.run_id.is_none();
        let mut new_run: Option<(String, Option<ChatRecord>)> = None;
        if opens_run {
            let run = self.new_run_id();
            new_run = Some((run, self.chat()));
        }
        if !busy {
            self.turn_opener = Some(sid.to_owned());
            self.turn_opened_by = Some(sid.to_owned());
            self.turn_acted = false;
            self.unreported_noted = false;
            self.sample_tree_base();
        }
        let row = if !busy {
            // A prompt that opens a turn: its user row shows at once, as saved.
            let mut details = json!({"submissionId": sid, "mode": sub.mode});
            if sub.origin != SubmissionOrigin::Typed {
                details["origin"] = json!(sub.origin);
            }
            // A page's send carries its card words so the card survives reloads.
            if let Some(t) = &sub.page_title {
                details["pageTitle"] = json!(t);
            }
            if let Some(o) = &sub.page_overview {
                details["pageOverview"] = json!(o);
            }
            let images = crate::message_images::for_submission(&self.sh.db, sid);
            if !images.is_empty() {
                details["images"] = json!(images);
            }
            let refs = crate::attachments::for_row(&self.sh.db, sid);
            if !refs.is_empty() {
                details["refs"] = json!(refs);
            }
            Some(RowWrite {
                source_key: format!("u:{sid}"),
                role: "user",
                text: sub.text.clone(),
                thinking: String::new(),
                tool_name: None,
                tool_call_id: None,
                status: "saved",
                timestamp: now(),
                details,
                entry_id: None,
                parent_id: None,
            })
        } else {
            None
        };
        let run = self.run_id.clone();
        let id = self.chat_id.clone();
        let rev = self.sh.clock.next();
        let artifacts = self.sh.cfg.artifacts();
        let carry_in_flight = self.carry_in_flight.is_some();
        let carry: Option<String> = self
            .write(|t| {
                if !busy {
                    Self::set_status_in(t, &id, ChatStatus::Starting, None)?;
                }
                if let Some((r, chat)) = &new_run {
                    Self::insert_run(t, r, &id, chat.as_ref())?;
                }
                if let Some(row) = &row {
                    session_log::upsert_row(t, &artifacts, &id, row, rev)?;
                }
                t.execute(
                    "UPDATE submissions SET state = 'dispatching' WHERE id = ?1",
                    [sid],
                )?;
                t.execute(
                    "UPDATE submission_meta SET run_id = ?1 WHERE submission_id = ?2",
                    rusqlite::params![run, sid],
                )?;
                // A message fork's carried history goes once, with its first prompt. It stays saved
                // until the worker takes that prompt (see `Pending::Prompt`), and no second prompt
                // carries it while the first is still waiting for its answer.
                use rusqlite::OptionalExtension;
                if carry_in_flight {
                    return Ok(None);
                }
                t.query_row(
                    "SELECT text FROM fork_carry WHERE chat_id = ?1",
                    [&id],
                    |r| r.get(0),
                )
                .optional()
            })
            .flatten();
        if carry.is_some() {
            self.carry_in_flight = Some(sid.to_owned());
        }
        if !busy {
            self.after_status(ChatStatus::Starting);
        }
        if let Some(row) = &row {
            self.emit_rows(std::slice::from_ref(&row.source_key));
        }
        let mut cmd = json!({"type": "prompt", "submissionId": sid, "text": sub.text});
        let context: Option<String> = self
            .sh
            .db
            .snapshot(|t| {
                use rusqlite::OptionalExtension;
                t.query_row(
                    "SELECT text FROM submission_context WHERE submission_id = ?1",
                    [sid],
                    |r| r.get(0),
                )
                .optional()
            })
            .ok()
            .flatten();
        // Attached images go as stream-json image blocks with the prompt.
        let images = self
            .sh
            .db
            .snapshot(|t| {
                crate::attachments::images_for(t, &self.sh.cfg.profile_dir.join("attachments"), sid)
            })
            .unwrap_or_default();
        if !images.is_empty() {
            cmd["images"] = json!(images);
        }
        let context = match (carry, context) {
            (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
            (a, b) => a.or(b),
        };
        // While AFK is on, every turn says so (AFK's own notes already say it).
        let afk_note: bool = self
            .sh
            .db
            .snapshot(|t| {
                t.query_row(
                    "SELECT afk_note <> 0 FROM submission_meta WHERE submission_id = ?1",
                    [sid],
                    |r| r.get(0),
                )
            })
            .unwrap_or(false);
        let afk_text = self
            .sh
            .afk
            .get()
            .filter(|_| !afk_note)
            .and_then(|p| p.turn_text(&self.chat_id));
        let context = match (context, afk_text) {
            (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
            (a, b) => a.or(b),
        };
        // The stop judge's own line (a changed working mode, say) rides beside AFK's.
        let judge_text = self
            .sh
            .stop_judge
            .get()
            .and_then(|j| j.turn_line(&self.chat_id));
        let context = match (context, judge_text) {
            (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
            (a, b) => a.or(b),
        };
        if let Some(c) = context {
            // A separate content block after the text.
            cmd["context"] = json!(c);
        }
        if busy && sub.mode == SubmissionMode::Steer {
            cmd["streamingBehavior"] = json!("steer");
        }
        if busy && priority.as_deref() == Some("now") {
            cmd["priority"] = json!("now");
        }
        // A turn is opening: typing may warm the model again for the one after it.
        self.warmed = false;
        let id = self.request(
            cmd,
            Pending::Prompt {
                submission_id: sid.to_owned(),
            },
        );
        let lim = self.sh.cfg.limits.ack;
        self.add_timer(lim, Timer::Ack(id));
        self.emit_pending();
    }

    // ------------------------------------------------------------------ local models

    fn is_local_chat(&self) -> bool {
        self.chat().is_some_and(|c| local_of(&c).is_some())
    }

    fn set_loading(&mut self, text: String) {
        {
            let mut live = self.live.lock().unwrap();
            let since = live
                .thread
                .loading
                .as_ref()
                .map_or_else(now, |l| l.started_at);
            live.thread.loading = Some(LocalLoading {
                text,
                started_at: since,
            });
        }
        self.emit_thread();
    }

    /// The model's first words have arrived (or the turn ended): the loading line goes.
    fn clear_loading(&mut self) {
        let had = self.live.lock().unwrap().thread.loading.take().is_some();
        if had {
            self.emit_thread();
        }
    }

    /// Put the text of typed submissions that were never sent back in the draft, and their images
    /// and files back in the composer's staged set. Any other submission is only marked rejected.
    fn give_back(&mut self, ids: Vec<String>) {
        let chat = self.chat_id.clone();
        self.write(|t| {
            use rusqlite::OptionalExtension;
            for id in &ids {
                let text: Option<String> = t
                    .query_row(
                        "SELECT text FROM submissions WHERE id = ?1 AND state = 'saved'",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(text) = text {
                    t.execute(
                        "UPDATE submissions SET state = 'rejected' WHERE id = ?1",
                        [id],
                    )?;
                    // Only what the user typed goes back into their draft. A message another
                    // part of the app sent (a wait firing, a schedule, a phone message) stays
                    // "not delivered" for whoever sent it to see; its text is not theirs to edit.
                    let typed: bool = t
                        .query_row(
                            "SELECT COALESCE((SELECT origin FROM submission_meta WHERE submission_id = ?1), 'typed') = 'typed'",
                            [id],
                            |r| r.get(0),
                        )?;
                    if typed {
                        // The images and files go back to the chat's unstaged set with the
                        // text, so the composer can attach them to the next send.
                        crate::attachments::unlink(t, id)?;
                        crate::submit::text_to_draft(t, &chat, &text)?;
                    }
                }
            }
            Ok(())
        });
    }

    fn apply_model(&mut self, model: Option<String>, effort: Option<String>) {
        if model.is_some() {
            // The model changed: anything made ready for the old one does not carry over.
            self.prepared = None;
        }
        if self.link.is_none() {
            return; // the next launch carries it
        }
        if model.is_some()
            && (self.launched_local.is_some()
                || model
                    .as_deref()
                    .is_some_and(|m| crate::local::split(m).is_some()))
        {
            // A local chat's model is part of its launch, and a worker started for one
            // provider cannot simply be retold to use the other: start a new worker,
            // which resumes the same session binding so the conversation carries
            // over — now when nothing runs, otherwise at the next idle point. An
            // effort-only change is not a provider change: it falls through to the
            // live retelling below, where the worker turns the new effort into the
            // local model's thinking budget without a respawn.
            if self.status().is_busy() || self.agents_running() {
                if self.agents_running()
                    || !self.turn_quiet()
                    || self.turn_opened_by.is_none()
                    || self.turn_acted
                {
                    // Something is in flight, or the turn has already replied or run a tool
                    // (starting it again would repeat what it did): the change lands at the
                    // next idle point, as it always did.
                    self.relaunch_on_idle = true;
                    self.provider_switch_pending = true;
                } else {
                    // The turn is open but nothing is running: for a local chat this is the
                    // wait in the model server's queue, which is not work in progress. Stop
                    // the turn now and send its opening message again on the new model,
                    // rather than letting the queued request answer on the old one.
                    self.context_row(
                        ContextKind::Notice,
                        "The model was changed: the turn stops now and starts again on the new model.",
                    );
                    self.switch_restart = self.turn_opened_by.clone();
                    self.provider_switch_pending = true;
                    let (tx, _rx) = oneshot::channel();
                    self.stop(tx, true);
                }
            } else {
                self.relaunch_worker();
            }
            return;
        }
        if let Some(m) = model {
            let id = self.request(json!({"type": "set_model", "model": m}), Pending::Model);
            let lim = self.sh.cfg.limits.model_ack;
            self.add_timer(lim, Timer::ModelAck(id));
        }
        if let Some(e) = effort {
            let id = self.request(
                json!({"type": "set_thinking_level", "level": e}),
                Pending::Model,
            );
            let lim = self.sh.cfg.limits.model_ack;
            self.add_timer(lim, Timer::ModelAck(id));
        }
    }

    /// The chat's model changed: take down the preparation made for the old one. Its late answer
    /// is ignored (it is matched by token), and its loading line goes. A preparation that had a
    /// send behind it hands back that send and the ones held behind it.
    fn drop_stale_prepare(&mut self) -> Option<(String, Vec<String>)> {
        let p = self.prepare.take()?;
        if p.line {
            self.clear_loading();
        }
        p.sid.map(|sid| (sid, p.held))
    }

    /// Put sends that were waiting on a dropped preparation through the model the chat has now:
    /// prepared afresh when it is a local one, otherwise sent as the turn it was always going to
    /// open.
    fn restart_prepare(&mut self, sid: String, held: Vec<String>) {
        if self.begin_prepare(&sid) {
            match self.prepare.as_mut() {
                Some(p) => p.held = held,
                // Back at the start-capacity wait (the server refilled meanwhile): the held
                // sends go in front of it too. Only a refusal leaves them nowhere.
                None => match self.start_wait.as_mut() {
                    Some(w) => w.1.extend(held),
                    None => self.give_back(held),
                },
            }
            return;
        }
        self.opening = Some(sid.clone());
        self.dispatch(&sid);
        for h in held {
            self.dispatch(&h);
        }
    }

    /// Where this chat stands in the queue right now: the class of `sid`'s send, and its place
    /// in line — the one it took when it started to wait, or a fresh one (which puts it behind
    /// every chat already waiting). The pushed-to-top flag is re-read from the queue's own table,
    /// so a change made from outside the actor takes effect on the next look.
    fn my_place(&self, sid: &str) -> (crate::queue::QueueClass, u64) {
        let class = crate::queue::class_for_origin(
            self.submission(sid)
                .map(|s| s.origin)
                .unwrap_or(SubmissionOrigin::Typed)
                .as_str(),
        );
        let top = crate::queue::top_of(&self.sh.db, &self.chat_id);
        let mut live = self.live.lock().unwrap();
        live.hold_top = top;
        let seq = live.held_at_start.unwrap_or_else(crate::queue::next_seq);
        (class, seq)
    }

    /// The other chats of ours busy on local model `server`: how many are working, and the ones
    /// held at their start with their standing.
    fn server_line(&self, server: &str) -> (usize, Vec<crate::queue::Waiter>) {
        let handles: Vec<ActorHandle> = self
            .sh
            .actors
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| **id != self.chat_id)
            .map(|(_, h)| h.clone())
            .collect();
        let mut working = 0usize;
        let mut waiters = Vec::new();
        for h in handles {
            let l = h.live.lock().unwrap();
            if !l.thread.status.is_busy() || l.server_id.as_deref() != Some(server) {
                continue;
            }
            match l.held_at_start {
                None => working += 1,
                Some(seq) => waiters.push(crate::queue::Waiter {
                    class: l.hold_class.unwrap_or(crate::queue::QueueClass::Normal),
                    top: l.hold_top,
                    seq,
                }),
            }
        }
        (working, waiters)
    }

    /// The other chats of ours waiting for the Claude plan windows to open again.
    fn plan_line(&self) -> Vec<crate::queue::Waiter> {
        let handles: Vec<ActorHandle> = self
            .sh
            .actors
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| **id != self.chat_id)
            .map(|(_, h)| h.clone())
            .collect();
        let mut waiters = Vec::new();
        for h in handles {
            let l = h.live.lock().unwrap();
            match (l.held_at_start, l.hold_reason) {
                (Some(seq), Some(r)) if r != crate::queue::LOCAL_CAP => {
                    waiters.push(crate::queue::Waiter {
                        class: l.hold_class.unwrap_or(crate::queue::QueueClass::Normal),
                        top: l.hold_top,
                        seq,
                    })
                }
                _ => {}
            }
        }
        waiters
    }

    /// How many of our chats have a turn open right now, not counting this one.
    fn working_chats(&self) -> usize {
        self.sh
            .actors
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| **id != self.chat_id)
            .filter(|(_, h)| {
                // A chat held at its start is waiting, not working.
                let l = h.live.lock().unwrap();
                l.thread.status.is_busy() && l.held_at_start.is_none()
            })
            .count()
    }

    /// The queue's whole answer to "may this turn open now", for the saved send `sid`. Only
    /// asked where no turn of this chat is open: a chat mid-turn is never preempted.
    ///
    /// A local chat's place is the line on its server: the server carries at most its own cap
    /// (set in the server's settings) or else `localChatCap` (0 switches the cap off), and only
    /// chats that are working
    /// count against it — a chat held at its start is not working, so waiters never block each
    /// other; they go pushed-to-top first, then normal before background, then in the order
    /// they began to wait. The cap is deliberately soft at the margin: a chat that has passed
    /// this check but has not opened its turn yet counts as neither working nor waiting, so
    /// briefly one chat more than the cap can be preparing; it settles as soon as the turn
    /// opens. A chat on Claude instead asks the plan windows (`claudeHoldPct`) and the app's
    /// own working-chat limit. In every case an interactive start is never held, and a
    /// hand-parked chat waits whatever the rest answers.
    fn turn_gate(&self, sid: &str) -> (crate::queue::QueueClass, crate::queue::Verdict) {
        use crate::queue;
        let Some(chat) = self.chat() else {
            return (queue::QueueClass::Normal, queue::Verdict::Go);
        };
        let local = local_of(&chat);
        let (class, seq) = self.my_place(sid);
        let mine = queue::Mine {
            class,
            top: self.live.lock().unwrap().hold_top,
            seq,
        };
        let manual = queue::is_manual(&self.sh.db, &self.chat_id);
        let (working, waiters, cap) = match &local {
            Some((server, _)) => {
                // The server's own cap (its settings) beats the default preference.
                let cap = queue::server_cap(&self.sh.db, server).unwrap_or_else(|| {
                    self.sh
                        .db
                        .snapshot(|tx| {
                            Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, "localChatCap"))
                        })
                        .ok()
                        .flatten()
                        .and_then(|v| v.as_u64())
                        .unwrap_or(DEFAULT_LOCAL_CAP)
                }) as usize;
                // Cap off still orders the waiters; only the working-chat count is lifted.
                let cap = if cap == 0 { usize::MAX } else { cap };
                let (w, waits) = self.server_line(server);
                (w, waits, cap)
            }
            None => (0, self.plan_line(), usize::MAX),
        };
        // The plan windows answer only for chats that run on Claude; local chats cost the plan
        // nothing. A Start anyway lets exactly its own chat's one start through while they read
        // full (the Go path spends it); it never reaches an interactive start, which the
        // windows answer in words instead of queueing.
        let claude_full = local.is_none()
            && !(self.start_anyway && class != queue::QueueClass::Interactive)
            && helm_core::usage_windows::claude_full(&self.sh.db, now()).is_some();
        let at_limit = self.working_chats() >= self.sh.cfg.limits.max_active;
        (
            class,
            queue::decide(&mine, &waiters, working, cap, claude_full, at_limit, manual),
        )
    }

    /// Park this turn start at the chat's start: the send stays saved, the chat shows as
    /// starting and queued, and the queue's own table keeps the reason, class, server and line
    /// place until it opens, goes back or is released.
    fn arm_hold(
        &mut self,
        sid: &str,
        class: crate::queue::QueueClass,
        reason: crate::queue::Reason,
        server: Option<&str>,
        ahead: Option<usize>,
    ) {
        if self.start_wait.is_none() {
            self.last_output = Instant::now();
        }
        let seq = self.set_held_at_start(true);
        self.set_queue_reason(Some(reason));
        self.live.lock().unwrap().hold_class = Some(class);
        let hold_server = if reason == crate::queue::LOCAL_CAP {
            server
        } else {
            None
        };
        crate::queue::write_hold(
            &self.sh.db,
            &self.chat_id,
            reason,
            class,
            hold_server,
            seq,
            now(),
        );
        self.set_local_ahead(ahead.map(|a| a as i64));
        self.set_status(ChatStatus::Starting, None);
        self.start_wait = Some((sid.to_owned(), Vec::new()));
    }

    /// Record and announce why this chat's turn start waits (the `chat_activity.queue_reason`
    /// column, read into the chat record's `queueReason`); writes only when it changed.
    fn set_queue_reason(&self, on: Option<&'static str>) {
        {
            let mut live = self.live.lock().unwrap();
            if live.hold_reason == on {
                return;
            }
            live.hold_reason = on;
        }
        let id = self.chat_id.clone();
        self.write(|t| {
            t.execute(
                "INSERT INTO chat_activity(chat_id, last_activity_at, queue_reason) VALUES (?1, ?2, ?3)
                 ON CONFLICT(chat_id) DO UPDATE SET queue_reason = excluded.queue_reason",
                rusqlite::params![id, now(), on],
            )
        });
        self.emit_chat();
    }

    /// The plain sentence a full plan window answers an interactive start with, in the
    /// transcript and (through the relay's own reply line) back to the speaker or Telegram.
    fn claude_full_notice(&self) -> String {
        match crate::queue::claude_full(&self.sh.db, now()) {
            Some(cap) => crate::queue::claude_full_line(&cap, now()),
            None => "Claude's plan allowance is used up right now — nothing was started, so \
                     please send it again in a little while."
                .to_owned(),
        }
    }

    /// Note that this chat's turn is held at its start (keeping its place if it already was) or
    /// is not. The place is a sequence number taken when the wait began.
    fn set_held_at_start(&self, on: bool) -> u64 {
        let mut live = self.live.lock().unwrap();
        live.held_at_start = match (on, live.held_at_start) {
            (true, None) => Some(crate::queue::next_seq()),
            (true, kept) => kept,
            (false, _) => None,
        };
        live.held_at_start.unwrap_or(0)
    }

    /// Every turn start passes the queue's gate here ([`Self::turn_gate`]): one that may go on
    /// a local model gets its model made ready before the turn opens (the memory check, the
    /// load, Ollama's variant), started on the blocking pool, answering true; the turn goes
    /// ahead, or is refused, when it reports back. One that may not yet go waits at the chat's
    /// start — it queues here, where a place is known to free, rather than piling requests
    /// onto a server or a full plan window — and a turn that may open straight away on Claude
    /// answers false, so the caller sends it.
    fn begin_prepare(&mut self, sid: &str) -> bool {
        let Some(chat) = self.chat() else {
            return false;
        };
        let local = local_of(&chat);
        // The queue first: nothing refuses a start — what cannot open now waits at the chat's
        // start, and an interactive start with Claude's windows full is answered in words
        // instead of started. A Claude chat's turn opens straight after this answer.
        let (class, verdict) = self.turn_gate(sid);
        let server = local.as_ref().map(|(s, _)| s.as_str());
        match verdict {
            crate::queue::Verdict::Go => {
                crate::queue::clear_auto_hold(&self.sh.db, &self.chat_id);
                self.set_queue_reason(None);
                // A Start anyway has done its one job, whatever else decided this start.
                self.start_anyway = false;
                self.live.lock().unwrap().hold_class = None;
                // A chat that waited is a working chat from here on, Claude or local, so the
                // cap counts it.
                self.set_held_at_start(false);
            }
            crate::queue::Verdict::Refuse => {
                let line = if crate::queue::is_manual(&self.sh.db, &self.chat_id) {
                    "This chat is parked in Helm's queue, so nothing was started. Release it in \
                     the queue on the PC, then send it again."
                        .to_owned()
                } else {
                    self.claude_full_notice()
                };
                self.refuse_local(&[sid.to_owned()], &line);
                return true;
            }
            crate::queue::Verdict::Behind(a) => {
                self.arm_hold(sid, class, crate::queue::LOCAL_CAP, server, Some(a));
                return true;
            }
            crate::queue::Verdict::Hold(reason, a) => {
                let ahead = (reason != crate::queue::MANUAL).then_some(a);
                self.arm_hold(sid, class, reason, server, ahead);
                return true;
            }
        }
        let Some((server_id, model)) = local else {
            return false;
        };
        let Some(seam) = self.sh.local.get().cloned() else {
            self.refuse_local(&[sid.to_owned()], crate::local::NOT_AVAILABLE);
            return true;
        };
        self.set_held_at_start(false);
        // The chat shows as starting while its model is made ready; Stop works as usual. The wait
        // is not silence: the stall flag counts from here, not from the chat's last output.
        self.last_output = Instant::now();
        self.set_status(ChatStatus::Starting, None);
        self.prepare_start(seam, server_id, model, Some(sid.to_owned()));
        true
    }

    /// The first keystroke of a draft in an idle local chat: make its model ready now, so the
    /// turn the message opens rides on this preparation instead of waiting for it. Nothing else
    /// happens: no turn opens, no worker starts, no status changes. A warm-up runs at most once
    /// between turns and never more often than [`WARM_MIN_GAP`] apart; a failed one stays quiet
    /// — the send path reports in its own words when the message goes.
    fn warm_now(&mut self) {
        if !self.is_local_chat() || self.prepare.is_some() || self.warmed || self.status().is_busy()
        {
            return;
        }
        // A worker still booting warms itself; a local chat with no worker is the usual case.
        if self.link.is_some() && !self.ready {
            return;
        }
        if self.last_warm.is_some_and(|t| t.elapsed() < WARM_MIN_GAP) {
            return;
        }
        let Some(chat) = self.chat() else { return };
        let Some((server_id, model)) = local_of(&chat) else {
            return;
        };
        let Some(seam) = self.sh.local.get().cloned() else {
            return; // a send would be refused the same way; stay quiet for now
        };
        self.last_warm = Some(Instant::now());
        self.warmed = true;
        self.prepare_start(seam, server_id, model, None);
    }

    /// The preparation itself: [`Prepare`] for `sid` (`None` = a warm-up no send has claimed
    /// yet), the live-state reads, and the seam call on the blocking pool.
    fn prepare_start(
        &mut self,
        seam: crate::local::SharedLocal,
        server_id: String,
        model: String,
        sid: Option<String>,
    ) {
        self.prepare_count += 1;
        let token = self.prepare_count;
        self.prepare = Some(Prepare {
            token,
            sid,
            line: false,
            held: Vec::new(),
            target: (server_id.clone(), model.clone()),
        });
        // Read one at a time: the guard of this chat's own live state is among those taken below.
        let launched = self.live.lock().unwrap().local_model.clone();
        let in_use: Vec<String> = self
            .sh
            .actors
            .lock()
            .unwrap()
            .values()
            .filter_map(|h| h.live.lock().unwrap().local_model.clone())
            .collect();
        let turn = crate::local::LocalTurn {
            chat_id: self.chat_id.clone(),
            server_id,
            model,
            launched,
            in_use,
        };
        let weak = self.me.clone();
        self.sh.handle.spawn(async move {
            let notes = weak.clone();
            let result = tokio::task::spawn_blocking(move || {
                let note = |text: &str| {
                    if let Some(tx) = notes.upgrade() {
                        let _ = tx.try_send(Cmd::LocalNote {
                            token,
                            text: text.to_owned(),
                        });
                    }
                };
                seam.prepare(&turn, &note)
            })
            .await
            .unwrap_or_else(|e| Err(format!("Helm could not get the local model ready: {e}")));
            if let Some(tx) = weak.upgrade() {
                let _ = tx.send(Cmd::LocalPrepared { token, result }).await;
            }
        });
    }

    fn on_prepared(&mut self, token: u64, result: Result<String, String>) {
        let Some(p) = self.prepare.take_if(|p| p.token == token) else {
            return; // stopped or replaced meanwhile
        };
        // The chat may have been pointed at another model while this one was made ready. A
        // preparation for the old model cannot answer for the send: prepare the current one with
        // the same send waiting, or (the chat no longer on a local model) drop the answer.
        let current = self.chat().and_then(|c| local_of(&c));
        let mut keep_name = true;
        if current.as_ref() != Some(&p.target) {
            match (current, self.sh.local.get().cloned()) {
                (Some((server, model)), Some(seam)) if result.is_ok() => {
                    let (line, held) = (p.line, p.held);
                    self.prepare_start(seam, server, model, p.sid);
                    if let Some(n) = self.prepare.as_mut() {
                        n.line = line;
                        n.held = held;
                    }
                    return;
                }
                // Not a local chat any more (or no server seam): the name is no use to a launch.
                _ => keep_name = false,
            }
        }
        match (result, p.sid) {
            (Ok(name), Some(sid)) => {
                self.prepared = keep_name.then_some(name);
                self.opening = Some(sid.clone());
                self.dispatch(&sid);
                for held in p.held {
                    self.dispatch(&held);
                }
            }
            (Ok(name), None) => {
                // A warm-up: no turn opens. The next worker launched for this chat asks the
                // server for this model by name, and the loading line it showed, if any, goes.
                self.prepared = keep_name.then_some(name);
                if p.line {
                    self.clear_loading();
                }
            }
            (Err(why), Some(sid)) => {
                let ids = std::iter::once(sid).chain(p.held).collect::<Vec<_>>();
                self.refuse_local(&ids, &why);
            }
            (Err(_), None) => {
                // A warm-up's failure stays quiet — but quiet is not a stuck line: what it
                // showed goes, and the send reports afresh in its own words.
                if p.line {
                    self.clear_loading();
                }
            }
        }
    }

    /// No turn starts: say why in the transcript, put the typed text back in the draft and leave
    /// the chat idle.
    fn refuse_local(&mut self, ids: &[String], why: &str) {
        self.give_back(ids.to_vec());
        self.context_row(ContextKind::Notice, why);
        self.end_run(ChatStatus::Idle, None, "refused");
        self.emit_pending();
    }

    fn stop(&mut self, reply: oneshot::Sender<ChatStatus>, resend: bool) {
        let status = self.status();
        if !status.is_busy() {
            // A Stop on a chat whose turn end is being checked ends the check: it finishes now.
            self.release_stop_hold(true);
            let _ = reply.send(status);
            return;
        }
        if let Some(p) = self.prepare.take() {
            // Stopped while the model was being made ready: nothing was sent, so what was typed
            // goes back to the draft. (A warm-up no send had claimed is never stopped here: the
            // chat is idle, and the stop returns before this.)
            self.give_back(p.sid.into_iter().chain(p.held).collect());
            self.clear_loading();
        }
        if let Some((sid, held)) = self.start_wait.take() {
            // Waiting at the start — the server's cap, the plan windows, the app's limit or a
            // hand-park: nothing reached the server or the plan either.
            self.give_back(std::iter::once(sid).chain(held).collect());
            self.set_local_ahead(None);
            self.set_held_at_start(false);
            self.set_queue_reason(None);
            crate::queue::clear_auto_hold(&self.sh.db, &self.chat_id);
        }
        if let Some(s) = self.stopping.as_mut() {
            s.replies.push(reply);
            s.resend &= resend;
            return;
        }
        self.stopping = Some(StopState {
            replies: vec![reply],
            resend,
        });
        self.set_status(ChatStatus::Stopping, None);
        let lim = self.sh.cfg.limits.stop;
        self.add_timer(lim, Timer::StopDeadline);
        if self.link.is_none() {
            // Nothing is running behind the status: settle at once.
            self.settle("stopped", None);
            return;
        }
        self.request(json!({"type": "abort"}), Pending::Abort);
    }

    /// The worker answered (or can no longer answer) the prompt that carried a message fork's
    /// saved history. Taken: the history is dropped. Otherwise it stays saved for the next prompt.
    fn finish_carry(&mut self, sid: &str, taken: bool) {
        if self.carry_in_flight.as_deref() != Some(sid) {
            return;
        }
        self.carry_in_flight = None;
        if taken {
            let id = self.chat_id.clone();
            self.write(|t| t.execute("DELETE FROM fork_carry WHERE chat_id = ?1", [&id]));
        }
    }

    fn deliver_now(&mut self, sid: &str, reason: &'static str) {
        let caps = self.live.lock().unwrap().thread.capabilities;
        if self.link.is_none() || !caps.deliver_now {
            return;
        }
        let limit_s = self
            .live
            .lock()
            .unwrap()
            .thread
            .send_behavior
            .wait_limit_seconds
            .unwrap_or(10);
        let min_run_ms = u64::from(limit_s.min(10)) * 1000;
        self.request(
            json!({"type": "deliver_now", "submissionId": sid, "reason": reason, "minRunMs": min_run_ms}),
            Pending::DeliverNow {
                submission_id: sid.to_owned(),
            },
        );
    }

    /// A `waiting` answer to `deliver_now`: the first one for a steer, and one every 30 s after,
    /// goes to the send-timeline log with what holds it back.
    fn log_steer_wait(&mut self, sid: &str, data: &Value) {
        let at = now();
        let Some(answer) = self.steer_waits.waiting(sid, at) else {
            return;
        };
        let Some(saved) = self.submission(sid).map(|s| s.created_at) else {
            return;
        };
        let line = crate::timeline::steer_wait_line(sid, saved, at, answer, data);
        crate::timeline::log_line(&self.sh.cfg.profile_dir, &self.chat_id, saved, &line);
    }

    /// Claude read a steer: log how long after it was saved, and how often it was held.
    fn log_steer_read(&mut self, sub: &Submission) {
        let answers = self.steer_waits.read(&sub.id);
        if sub.mode != SubmissionMode::Steer {
            return;
        }
        let line = crate::timeline::steer_read_line(&sub.id, sub.created_at, now(), answers);
        crate::timeline::log_line(
            &self.sh.cfg.profile_dir,
            &self.chat_id,
            sub.created_at,
            &line,
        );
    }

    // ------------------------------------------------------------------ timers

    /// A line already waiting on the worker's output channel, taken without waiting; `None`
    /// when the channel is empty or gone.
    fn next_queued_worker_line(&mut self) -> Option<WorkerOutput> {
        let rx = self.out.as_mut()?;
        match rx.try_recv() {
            Ok(out) => Some(out),
            Err(mpsc::error::TryRecvError::Empty)
            | Err(mpsc::error::TryRecvError::Disconnected) => None,
        }
    }

    fn on_timers(&mut self) {
        let now_i = TInstant::now();
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.timers)
            .into_iter()
            .partition(|(t, _)| *t <= now_i);
        self.timers = rest;
        for (_, t) in due {
            match t {
                Timer::Ack(id) => {
                    if let Some(Pending::Prompt { submission_id }) = self.pending.remove(&id) {
                        // No answer at all: the outcome is unknown, so never resend.
                        self.finish_carry(&submission_id, false);
                        self.set_submission(&submission_id, SubmissionState::Uncertain);
                        if self.status() == ChatStatus::Starting {
                            // The turn never began: end it rather than wait on a Stop.
                            self.end_run(
                                ChatStatus::Interrupted,
                                Some(ACK_TIMEOUT_ERROR),
                                "failed",
                            );
                        }
                        self.emit_pending();
                    }
                }
                Timer::ModelAck(id) => {
                    if self.pending.remove(&id).is_some() {
                        log::error!("chat {}: model/effort ack timed out", self.chat_id);
                        self.worker_error = Some(MODEL_ACK_ERROR.into());
                        if self.status().is_busy() {
                            self.end_run(ChatStatus::Interrupted, Some(MODEL_ACK_ERROR), "failed");
                        }
                        if let Some(l) = &self.link {
                            l.terminate();
                        }
                    }
                }
                Timer::StopDeadline => {
                    if self.stopping.is_some() {
                        // A line the worker already sent outranks the deadline: the kill cures a
                        // silent worker, not one that is on its way out. On an oversubscribed
                        // machine the deadline's expiry and the worker's own settle line can land
                        // in the same scheduling slice, where killing is a coin flip; take the
                        // line and re-arm the deadline instead, so a worker that keeps talking
                        // without settling is still killed.
                        if let Some(out) = self.next_queued_worker_line() {
                            self.on_output(Some(out));
                            if self.stopping.is_some() && !self.finished {
                                let lim = if self.archived {
                                    self.sh.cfg.limits.archive_stop
                                } else {
                                    self.sh.cfg.limits.stop
                                };
                                self.add_timer(lim, Timer::StopDeadline);
                            }
                            continue;
                        }
                        let why = if self.archived {
                            ARCHIVE_STOP_NOTE
                        } else {
                            STOP_TIMEOUT_ERROR
                        };
                        self.worker_error = Some(why.into());
                        if let Some(l) = &self.link {
                            l.terminate();
                        } else {
                            self.settle("interrupted", Some(why.into()));
                        }
                    }
                }
                Timer::WaitLimit(sid) => {
                    let still = self
                        .submission(&sid)
                        .is_some_and(|s| s.state == SubmissionState::Accepted);
                    if still && self.status().is_busy() {
                        self.deliver_now(&sid, "wait_limit");
                    }
                }
                Timer::StopCheck => {
                    // A verdict that never arrived: the chat finishes as it would with no judge.
                    self.release_stop_hold(true);
                }
                Timer::GateDeadline(id) => {
                    // A verdict that never arrived. Nothing is ever dropped: the card becomes
                    // a normal card again, shown and counted, and AFK gets its usual say.
                    self.gate_fail_card(&id, "Helm couldn't check this one.");
                }
            }
        }
    }

    // ------------------------------------------------------------------ worker output

    fn on_output(&mut self, out: Option<WorkerOutput>) {
        match out {
            None => self.on_exit("The worker stopped unexpectedly.".into()),
            Some(WorkerOutput::Exited {
                code,
                stderr,
                by_host,
                job_active,
                signal,
            }) => {
                let facts = self.exit_facts(code, signal, by_host, job_active, stderr);
                let shown = crate::exits::exit_reason(&facts);
                let rec = self.sh.exits.record(&facts, &shown);
                // What the worker said is in `shown` now, with the incident it belongs to.
                self.worker_error = None;
                let reason = match crate::exits::incident_note(&rec) {
                    Some(note) => format!("{shown} {note}"),
                    None => shown,
                };
                self.on_exit(reason)
            }
            Some(WorkerOutput::SpawnFailed(e)) => self.on_spawn_failed(e),
            Some(WorkerOutput::Fault(f)) => {
                self.worker_error = Some(format!(
                    "The worker sent a broken record ({f}), so Helm stopped it."
                ));
            }
            Some(WorkerOutput::Record(r)) => {
                self.note_output();
                self.on_record(r)
            }
        }
    }

    /// What is known about the worker that just exited, read before [`Self::on_exit`] clears it.
    fn exit_facts(
        &self,
        code: Option<i32>,
        signal: Option<i32>,
        by_host: bool,
        job_active: Option<u32>,
        stderr: String,
    ) -> crate::exits::ExitFacts {
        use crate::exits::HostReason;
        let quitting = self
            .sh
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst);
        let host_reason = if quitting {
            Some(HostReason::Quit)
        } else if self.archived {
            Some(HostReason::Archive)
        } else if self.stopping.is_some() || self.stop_ended {
            Some(HostReason::Stop)
        } else if by_host {
            Some(HostReason::Ended)
        } else if self.link_closing
            || self.retiring
            || self.edit_waiting
            || !self.shutdown_replies.is_empty()
        {
            Some(HostReason::Close)
        } else {
            None
        };
        // The call that started last is the one most likely to say what ended the worker (a
        // command of the agent's own, say).
        let last_tool = self.tools.values().max_by_key(|t| t.started_at).map(|t| {
            let command = serde_json::from_str::<Value>(&t.args)
                .ok()
                .and_then(|a| a.get("command").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| t.args.clone());
            (t.name.clone(), command)
        });
        // Claude had read the prompt that opened the turn (a turn it opened by itself has none).
        let prompt_read = self.turn_opener.as_deref().is_none_or(|sid| {
            self.submission(sid).is_some_and(|s| {
                matches!(
                    s.state,
                    SubmissionState::Delivered | SubmissionState::Complete
                )
            })
        });
        let chat = self.chat();
        crate::exits::ExitFacts {
            chat_id: self.chat_id.clone(),
            at_ms: now(),
            code,
            signal,
            host_reason,
            worker_error: self.worker_error.clone(),
            job_active,
            stderr,
            last_tool,
            uptime_ms: self.worker_started.map(|t| t.elapsed().as_millis() as i64),
            model: self
                .launched_local
                .as_ref()
                .map(|(_, m)| m.clone())
                .or_else(|| chat.as_ref().and_then(|c| c.model.clone())),
            provider: self.launched_local.as_ref().map(|(s, _)| s.clone()),
            turn_open: self.status().is_busy(),
            prompt_read,
            run_id: self.run_id.clone(),
            launch_path: Some(
                self.sh
                    .cfg
                    .profile_dir
                    .join("workers")
                    .join(&self.chat_id)
                    .join("launch.json"),
            ),
            cwd: chat.map(|c| c.cwd),
        }
    }

    fn on_record(&mut self, r: Map<String, Value>) {
        let v = Value::Object(r.clone());
        let kind = r.get("type").and_then(Value::as_str).unwrap_or_default();
        if matches!(kind, "message_update" | "tool_execution_start") {
            self.clear_loading();
            crate::timeline::finish(
                &mut self.timeline,
                &self.sh.cfg.profile_dir,
                &self.chat_id,
                Some(now()),
            );
        }
        match kind {
            "response" => self.on_response(r),
            "agent_start" => {
                if self.status() != ChatStatus::Stopping {
                    self.open_run();
                    if self.status() != ChatStatus::Running {
                        self.set_status(ChatStatus::Running, None);
                    }
                }
            }
            "message_start" => {
                let msg = v.get("message").cloned().unwrap_or_default();
                let role = str_of(&msg, "role").unwrap_or_default();
                if role == "assistant" {
                    self.turn_acted = true;
                    let key = self.msg_key(&str_of(&msg, "id").unwrap_or_default());
                    self.stream.insert(
                        key.clone(),
                        LiveMsg {
                            role: MessageRole::Assistant,
                            text: String::new(),
                            thinking: String::new(),
                            timestamp: now(),
                            row: None,
                            written: None,
                        },
                    );
                    self.dirty.insert(key);
                    self.flush();
                }
            }
            "message_update" => {
                let key = self.msg_key(&str_of(&v, "messageId").unwrap_or_default());
                let delta = v.get("delta").cloned().unwrap_or_default();
                let text = str_of(&delta, "text").unwrap_or_default();
                if let Some(m) = self.stream.get_mut(&key) {
                    let was_empty = m.text.is_empty() && m.thinking.is_empty();
                    match str_of(&delta, "type").as_deref() {
                        Some("text_delta") => m.text.push_str(&text),
                        Some("thinking_delta") => m.thinking.push_str(&text),
                        _ => {}
                    }
                    // Only the delta that gives the row its first words (not tool-input chunks).
                    let first = was_empty && !(m.text.is_empty() && m.thinking.is_empty());
                    self.dirty.insert(key);
                    if first {
                        // The message's first words go out now, not on the next tick.
                        self.flush();
                    }
                }
            }
            "message_end" => self.on_message_end(&v),
            "usage" => self.on_turn_usage(&v),
            "tool_execution_start" => {
                self.turn_acted = true;
                let id = str_of(&v, "toolCallId").unwrap_or_default();
                let name = str_of(&v, "toolName").unwrap_or_default();
                let args = v.get("args").map(Value::to_string).unwrap_or_default();
                // A few plain words on what the call is doing, read off its own arguments.
                let summary =
                    crate::toolsummary::tool_summary(&name, v.get("args").unwrap_or(&Value::Null));
                let stoppable = v.get("stoppable").and_then(Value::as_bool).unwrap_or(false);
                let started = now();
                let row = RowWrite {
                    source_key: format!("t:{id}"),
                    role: "toolResult",
                    text: String::new(),
                    thinking: String::new(),
                    tool_name: Some(name.clone()),
                    tool_call_id: Some(id.clone()),
                    status: "streaming",
                    timestamp: started,
                    details: {
                        let mut d = json!({"toolArgs": session_log::cut(&args, 4096), "stoppable": stoppable});
                        // Which server the tool came from.
                        if let Some(src) = v.get("toolSource").filter(|s| s.is_object()) {
                            d["toolSource"] = src.clone();
                        }
                        // Stored with the row so every later view of this call can show it too.
                        if !summary.is_empty() {
                            d["toolSummary"] = json!(&summary);
                        }
                        d
                    },
                    entry_id: None,
                    parent_id: None,
                };
                self.upsert_row(&row);
                self.tools.insert(
                    id,
                    LiveToolRec {
                        name,
                        args,
                        summary,
                        started_at: started,
                        stoppable,
                    },
                );
                self.emit_rows(&[row.source_key]);
                self.emit_thread();
            }
            "tool_execution_update" => {
                let id = str_of(&v, "toolCallId").unwrap_or_default();
                if let Some(s) = v.get("stoppable").and_then(Value::as_bool) {
                    if let Some(t) = self.tools.get_mut(&id) {
                        t.stoppable = s;
                        self.emit_thread();
                    }
                }
            }
            "tool_execution_end" => {
                let id = str_of(&v, "toolCallId").unwrap_or_default();
                let rec = self.tools.remove(&id);
                let result = v
                    .get("result")
                    .and_then(|r| r.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let is_error = v.get("isError").and_then(Value::as_bool).unwrap_or(false);
                let images =
                    self.keep_result_images(&id, v.get("result").and_then(|r| r.get("images")));
                let ts = now();
                let entry = ToolEntry {
                    id: format!("t:{id}"),
                    parent_id: None,
                    timestamp: ts,
                    tool_call_id: id.clone(),
                    tool_name: rec
                        .as_ref()
                        .map(|r| r.name.clone())
                        .or_else(|| str_of(&v, "toolName"))
                        .unwrap_or_default(),
                    args: rec.as_ref().map(|r| r.args.clone()).unwrap_or_default(),
                    result,
                    is_error,
                    started_at: rec.as_ref().map(|r| r.started_at).unwrap_or(ts),
                    ended_at: ts,
                    images,
                };
                self.record(Entry::Tool(entry));
                self.emit_thread();
            }
            "auto_retry_start" => {
                let attempt = v.get("attempt").and_then(Value::as_u64).unwrap_or(1) as u32;
                let state = if v.get("waiting").and_then(Value::as_bool) == Some(true) {
                    RetryState::Waiting
                } else {
                    RetryState::Retrying
                };
                self.mark_retry(state, attempt);
                let id = self.chat_id.clone();
                self.write(|t| {
                    t.execute(
                        "INSERT INTO chat_retry_markers(chat_id, attempt, reason) VALUES (?1, ?2, ?3)
                         ON CONFLICT(chat_id) DO UPDATE SET attempt = excluded.attempt, reason = excluded.reason",
                        rusqlite::params![id, attempt, v.get("errorMessage").and_then(Value::as_str)],
                    )
                });
            }
            "auto_retry_end" => {
                let attempt = v.get("attempt").and_then(Value::as_u64).unwrap_or(1) as u32;
                let state = match str_of(&v, "state").as_deref() {
                    Some("recovered") => RetryState::Recovered,
                    Some("retried") => RetryState::Retried,
                    Some("cancelled") => RetryState::Cancelled,
                    _ => RetryState::Failed,
                };
                self.mark_retry(state, attempt);
                let id = self.chat_id.clone();
                self.write(|t| {
                    t.execute("DELETE FROM chat_retry_markers WHERE chat_id = ?1", [&id])
                });
            }
            "agent_settled" => {
                let outcome = str_of(&v, "outcome").unwrap_or_else(|| "complete".into());
                if let Some(s) = str_of(&v, "sessionId") {
                    self.native_session = Some(s);
                }
                self.settle(&outcome, str_of(&v, "error"));
            }
            "extension_ui_request" => {
                let method = str_of(&v, "method").unwrap_or_default();
                let id = str_of(&v, "id").unwrap_or_default();
                // A re-request or dismissal ends any hold on that card id.
                self.clear_timer(|t| *t == Timer::GateDeadline(id.clone()));
                let mut parsed = None;
                {
                    let mut live = self.live.lock().unwrap();
                    live.thread.interactions.retain(|i| i.id != id);
                    if method != "dismiss" {
                        let mut obj = r.clone();
                        obj.remove("type");
                        obj.remove("remaining");
                        obj.insert("chatId".into(), json!(self.chat_id));
                        match serde_json::from_value::<Interaction>(Value::Object(obj)) {
                            Ok(i) => parsed = Some(i),
                            Err(e) => {
                                log::warn!("chat {}: unreadable interaction: {e}", self.chat_id)
                            }
                        }
                    }
                }
                if let Some(mut i) = parsed {
                    // The question gate goes first: a card it holds waits for its verdict (or
                    // the hold deadline), marked as being checked. Everything it passes follows
                    // the old path: while AFK is on it may answer it at once; otherwise (or when
                    // it defers) the card waits for the user.
                    if self.gate_hold(&mut i) || !self.afk_handle(&i, false) {
                        self.live.lock().unwrap().thread.interactions.push(i);
                    }
                }
                self.emit_thread();
            }
            "permission_mode" => {
                self.live.lock().unwrap().thread.permission_mode = str_of(&v, "mode");
                self.emit_thread();
            }
            "native_session" => {
                self.native_session = str_of(&v, "sessionId");
                self.save_binding();
            }
            "submission_delivered" => {
                if let Some(sid) = str_of(&v, "submissionId") {
                    let sub = self
                        .submission(&sid)
                        .filter(|s| s.state == SubmissionState::Accepted);
                    if let Some(sub) = sub {
                        self.set_submission(&sid, SubmissionState::Delivered);
                        self.clear_timer(|t| *t == Timer::WaitLimit(sid.clone()));
                        self.emit_pending();
                        self.log_steer_read(&sub);
                    }
                }
            }
            "submission_cancelled" => {
                // Claude dropped a message it never read, outside a Stop or a withdrawal: it is
                // not coming, so it must neither keep the chat running nor read as delivered.
                if let Some(sid) = str_of(&v, "submissionId") {
                    let accepted = self
                        .submission(&sid)
                        .is_some_and(|s| s.state == SubmissionState::Accepted);
                    if accepted && self.stopping.is_none() {
                        self.set_submission(&sid, SubmissionState::Uncertain);
                        self.clear_timer(|t| *t == Timer::WaitLimit(sid.clone()));
                        self.emit_pending();
                    }
                }
            }
            "background_tasks" => {
                let tasks: Vec<BackgroundTask> = v
                    .get("tasks")
                    .and_then(|t| serde_json::from_value(t.clone()).ok())
                    .unwrap_or_default();
                self.live.lock().unwrap().thread.background_tasks = tasks;
                self.emit_thread();
            }
            "subagent_start" | "subagent_step" | "subagent_end" => {
                self.on_subagent(kind, &v);
                if kind == "subagent_end" && !self.agents_running() {
                    self.on_quiet();
                }
            }
            "compaction_start" => {
                self.live.lock().unwrap().thread.compacting = Some(Compacting {
                    started_at: now(),
                    retrying: false,
                });
                self.emit_thread();
            }
            "compaction_end" => {
                {
                    let mut live = self.live.lock().unwrap();
                    live.thread.compacting = None;
                    live.context_gen += 1;
                }
                self.emit_thread();
                self.context_row(ContextKind::Compaction, "Context compacted");
            }
            "notice" => {
                if let Some(text) = str_of(&v, "text") {
                    self.context_row(ContextKind::Notice, &text);
                }
            }
            "long_command" => {
                log::info!("chat {}: long command {}", self.chat_id, v);
            }
            "shell_changes" => self.on_shell_changes(&v),
            "worker_error" => {
                self.worker_error = str_of(&v, "error");
            }
            "chat_request" => {
                crate::hostcalls::on_request(&self.sh, &self.chat_id, &mut self.host_calls, &v)
            }
            "chat_cancel" => crate::hostcalls::on_cancel(&mut self.host_calls, &v),
            _ => crate::hostcalls::other_line(&self.sh, &self.chat_id, &v),
        }
    }

    /// `shell_changes`: the files one shell command changed, as the worker found them. Kept as an
    /// entry of its own (no row) that the turn's change card reads; a bad line is dropped.
    fn on_shell_changes(&mut self, v: &Value) {
        let Some(call) = str_of(v, "toolCallId").filter(|s| !s.is_empty() && s.len() <= 200) else {
            return;
        };
        let paths: Vec<String> = v
            .get("paths")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|p| !p.is_empty() && p.len() <= 1024 && !p.contains('\0'))
                    .take(session_log::MAX_SHELL_PATHS)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if paths.is_empty() {
            return;
        }
        self.record(Entry::ShellChanges(session_log::ShellChangesEntry {
            id: format!("s:{call}"),
            parent_id: None,
            timestamp: now(),
            tool_call_id: call,
            paths,
        }));
    }

    // ------------------------------------------------------------------ subagents

    /// `subagent_start` / `subagent_step` / `subagent_end` (see "Native subagents" in
    /// `worker/PROTOCOL-NOTES.md`). The live list is kept on the thread and never written per step;
    /// the end is written to the Agent call's tool row.
    fn on_subagent(&mut self, kind: &str, v: &Value) {
        let Some(id) = str_of(v, "toolCallId").filter(|s| !s.is_empty()) else {
            return;
        };
        let int = |k: &str| v.get(k).and_then(Value::as_i64);
        // Any subagent activity keeps this worker the most recently used, so the idle cap
        // retires other workers first.
        self.touch();
        match kind {
            "subagent_start" => {
                let s = Subagent {
                    tool_call_id: id.clone(),
                    agent_type: str_of(v, "agentType").unwrap_or_else(|| "general-purpose".into()),
                    name: str_of(v, "name").unwrap_or_default(),
                    description: str_of(v, "description").unwrap_or_default(),
                    assignment: str_of(v, "assignment").unwrap_or_default(),
                    status: SubagentStatus::Running,
                    step_tool: None,
                    step: None,
                    started_at: now(),
                    ended_at: None,
                    duration_ms: None,
                    tokens: None,
                    tool_uses: None,
                    error: None,
                    report: None,
                    report_truncated: false,
                };
                self.evicted.retain(|x| x.tool_call_id != id);
                {
                    let mut live = self.live.lock().unwrap();
                    let list = &mut live.thread.subagents;
                    list.retain(|x| x.tool_call_id != id);
                    list.push(s);
                    while list.len() > MAX_SUBAGENTS {
                        // Drop the oldest settled agent; with none settled, the oldest.
                        let i = list
                            .iter()
                            .position(|x| x.status != SubagentStatus::Running)
                            .unwrap_or(0);
                        let gone = list.remove(i);
                        if gone.status == SubagentStatus::Running {
                            self.evicted.push(gone);
                        }
                    }
                    recount(&mut live.thread);
                }
                self.emit_thread();
            }
            "subagent_step" => {
                let changed = {
                    let mut live = self.live.lock().unwrap();
                    match live
                        .thread
                        .subagents
                        .iter_mut()
                        .find(|x| x.tool_call_id == id && x.status == SubagentStatus::Running)
                    {
                        Some(s) => {
                            s.step_tool = str_of(v, "tool");
                            s.step = str_of(v, "text").filter(|t| !t.is_empty());
                            s.tokens = int("tokens").or(s.tokens);
                            s.tool_uses = int("toolUses").or(s.tool_uses);
                            true
                        }
                        None => false,
                    }
                };
                if changed {
                    self.emit_thread_with(false);
                }
            }
            _ => {
                let status = match str_of(v, "status").as_deref() {
                    Some("completed") => SubagentStatus::Completed,
                    Some("stopped") => SubagentStatus::Stopped,
                    _ => SubagentStatus::Failed,
                };
                let report = str_of(v, "report").filter(|r| !r.is_empty());
                let ended = {
                    let mut live = self.live.lock().unwrap();
                    let ended = live
                        .thread
                        .subagents
                        .iter_mut()
                        .find(|x| x.tool_call_id == id && x.status == SubagentStatus::Running)
                        .map(|s| {
                            let t = now();
                            s.status = status;
                            s.ended_at = Some(t);
                            s.duration_ms = int("durationMs").or(Some(t - s.started_at));
                            s.tokens = int("tokens").or(s.tokens);
                            s.tool_uses = int("toolUses").or(s.tool_uses);
                            s.error = str_of(v, "error").filter(|e| !e.is_empty());
                            let full = s.clone();
                            set_report(s, report.as_deref(), LIVE_REPORT_BYTES);
                            full
                        });
                    recount(&mut live.thread);
                    ended
                };
                // One pushed off the list while it ran still gets its end onto its tool row.
                let ended = ended.or_else(|| {
                    let i = self.evicted.iter().position(|x| x.tool_call_id == id)?;
                    let mut s = self.evicted.remove(i);
                    let t = now();
                    s.status = status;
                    s.ended_at = Some(t);
                    s.duration_ms = int("durationMs").or(Some(t - s.started_at));
                    s.tokens = int("tokens").or(s.tokens);
                    s.tool_uses = int("toolUses").or(s.tool_uses);
                    s.error = str_of(v, "error").filter(|e| !e.is_empty());
                    Some(s)
                });
                if let Some(mut full) = ended {
                    set_report(&mut full, report.as_deref(), ROW_REPORT_BYTES);
                    self.persist_subagent(&full);
                    self.emit_thread();
                }
            }
        }
    }

    /// Write a settled subagent onto its Agent call's tool row (`details.subagent`).
    fn persist_subagent(&self, s: &Subagent) {
        let key = format!("t:{}", s.tool_call_id);
        let patch = json!({ "subagent": s }).to_string();
        let rev = self.sh.clock.next();
        let id = self.chat_id.clone();
        let k = key.clone();
        let n = self.write(|t| {
            t.execute(
                "UPDATE messages SET details = json_patch(COALESCE(details, '{}'), ?1), revision = ?2
                 WHERE chat_id = ?3 AND source_key = ?4",
                rusqlite::params![patch, rev, id, k],
            )
        });
        if n.unwrap_or(0) > 0 {
            self.emit_rows(&[key]);
        }
    }

    /// Background agents of this chat still running (they keep the worker busy).
    fn agents_running(&self) -> bool {
        self.live.lock().unwrap().thread.subagents_running > 0
    }

    /// Nothing is in flight: no tool call running and no message being generated. A turn in
    /// this state is waiting — at the model server's queue, or between requests — not working,
    /// so a provider change need not wait for it.
    fn turn_quiet(&self) -> bool {
        self.tools.is_empty() && self.stream.is_empty()
    }

    /// Remember how many processes the worker's group holds now, as what the chat's own helpers
    /// account for ([`Self::tree_base`]). Never below the worker and the CLI themselves.
    fn sample_tree_base(&mut self) {
        let now = self.link.as_ref().and_then(|l| l.tree_size()).unwrap_or(0);
        self.tree_base = self.tree_base.max(now).max(2);
    }

    /// The turn has settled or the last background agent ended: a relaunch held for this point
    /// runs once nothing is left running, and the idle-worker cap is applied.
    fn on_quiet(&mut self) {
        self.on_quiet_after(false);
    }

    /// [`Self::on_quiet`]; `stopped` says the turn that ended was stopped on purpose, which
    /// cancels a roll over waiting for the turn's end rather than starting one.
    fn on_quiet_after(&mut self, stopped: bool) {
        if self.status().is_busy() {
            return;
        }
        if self.retiring && !self.finished {
            // An archived chat's turn is over: close its worker (on_exit ends the actor).
            if let Some(l) = &self.link {
                // Closing, so a send arriving before the exit starts a fresh worker rather than
                // reaching this one.
                self.link_closing = true;
                l.send(&json!({"type": "shutdown"}));
            } else {
                self.finished = true;
            }
            return;
        }
        if self.relaunch_on_idle && !self.agents_running() {
            self.relaunch_worker();
        }
        // A chat that has grown past the roll-over setting is asked for a hand-off right here,
        // while it is quiet (helm-chats' rollover). Cheap reads; it never interrupts a turn.
        if stopped {
            crate::rollover::turn_stopped(&self.chat_id);
        } else {
            crate::rollover::on_turn_end(&self.sh, &self.chat_id);
        }
        crate::runtime::ChatRuntime {
            sh: self.sh.clone(),
        }
        .retire_idle_over_cap();
    }

    /// The worker is gone: no agent of it can still be running.
    fn end_running_subagents(&mut self, reason: &str) {
        let ended: Vec<Subagent> = {
            let mut live = self.live.lock().unwrap();
            let t = now();
            let ended = live
                .thread
                .subagents
                .iter_mut()
                .filter(|s| s.status == SubagentStatus::Running)
                .map(|s| {
                    s.status = SubagentStatus::Stopped;
                    s.ended_at = Some(t);
                    s.duration_ms = Some(t - s.started_at);
                    s.error = Some(reason.to_owned());
                    s.clone()
                })
                .collect();
            recount(&mut live.thread);
            ended
        };
        let mut ended = ended;
        for mut s in std::mem::take(&mut self.evicted) {
            s.status = SubagentStatus::Stopped;
            s.ended_at = Some(now());
            s.duration_ms = Some(now() - s.started_at);
            s.error = Some(reason.to_owned());
            ended.push(s);
        }
        for s in &ended {
            self.persist_subagent(s);
        }
    }

    fn msg_key(&self, message_id: &str) -> String {
        match &self.run_id {
            Some(run) => format!("m:{run}:{message_id}"),
            None => format!("m:{message_id}"),
        }
    }

    fn mark_retry(&mut self, state: RetryState, attempt: u32) {
        let Some(key) = self.last_error_row.clone() else {
            return;
        };
        let retry = json!({"retry": {"state": state, "attempt": attempt}});
        let id = self.chat_id.clone();
        let rev = self.sh.clock.next();
        self.write(|t| {
            t.execute(
                "UPDATE messages SET details = json_patch(COALESCE(details, '{}'), ?1), revision = ?2 WHERE chat_id = ?3 AND source_key = ?4",
                rusqlite::params![retry.to_string(), rev, id, key],
            )
        });
        self.emit_rows(&[key]);
    }

    /// The turn's result: record what it used as a `usage` entry (see [`crate::turn_usage`]).
    fn on_turn_usage(&mut self, v: &Value) {
        let main = crate::turn_usage::parse(v).unwrap_or_default();
        let local = self.launched_local.is_some();
        let turn = match v.get("running").and_then(crate::turn_usage::parse) {
            Some(running) => {
                // The share is worked out on the raw running totals, so a switch between Claude
                // and a local model does not read as a fresh start. Whatever cost Claude Code
                // works out for a model it does not know is not real: a local turn's cost is
                // zeroed below, once the share is known, and its tokens are counted apart from
                // the plan's.
                let turn = crate::turn_usage::share(&self.cost_base, &running, &main);
                self.cost_base = running;
                turn
            }
            // No running totals: the main conversation's tokens, and no cost.
            None => Usage {
                cost_usd: 0.0,
                ..main
            },
        };
        let turn = if local {
            Usage {
                cost_usd: 0.0,
                ..turn
            }
        } else {
            turn
        };
        if crate::turn_usage::is_empty(&turn) {
            return;
        }
        self.record(Entry::Usage(crate::session_log::UsageEntry {
            id: format!("s:{}", uuid::Uuid::new_v4()),
            parent_id: None,
            timestamp: now(),
            native_session: self.native_session.clone(),
            usage: turn,
            running: self.cost_base.clone(),
            provider: Some(if local { "local" } else { "claude" }.to_owned()),
        }));
    }

    fn on_message_end(&mut self, v: &Value) {
        let msg = v.get("message").cloned().unwrap_or_default();
        let role = str_of(&msg, "role").unwrap_or_default();
        let mid = str_of(&msg, "id").unwrap_or_default();
        let content = msg
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut text = String::new();
        let mut thinking = String::new();
        let mut tools = Vec::new();
        for b in &content {
            match b.get("type").and_then(Value::as_str) {
                Some("text") => text.push_str(b.get("text").and_then(Value::as_str).unwrap_or("")),
                Some("thinking") => {
                    thinking.push_str(b.get("thinking").and_then(Value::as_str).unwrap_or(""))
                }
                Some("toolCall") => {
                    let args = b.get("arguments").map(Value::to_string).unwrap_or_default();
                    let cut = session_log::cut(&args, 4096);
                    tools.push(ToolCallSummary {
                        id: str_of(b, "id").unwrap_or_default(),
                        name: str_of(b, "name").unwrap_or_default(),
                        truncated: cut.len() < args.len(),
                        args: cut.to_owned(),
                    });
                }
                _ => {}
            }
        }
        let status = match str_of(&msg, "status").as_deref() {
            Some("error") => MessageStatus::Error,
            Some("interrupted") => MessageStatus::Interrupted,
            _ => MessageStatus::Complete,
        };
        // The request's own tokens, as the API reports them; the turn's cost is recorded apart
        // ([`Self::on_turn_usage`]).
        let usage = msg.get("usage").and_then(crate::turn_usage::parse);
        if role == "user" {
            // The worker's echo of one of our submissions (id = submission id).
            let sid = str_of(&msg, "submissionId").unwrap_or(mid);
            let sub = self.submission(&sid);
            let images = crate::message_images::for_submission(&self.sh.db, &sid);
            let refs = crate::attachments::for_row(&self.sh.db, &sid);
            let entry = MessageEntry {
                id: format!("u:{sid}"),
                parent_id: None,
                timestamp: self
                    .sh
                    .db
                    .snapshot(|t| store::message_by_key(t, &self.chat_id, &format!("u:{sid}")))
                    .ok()
                    .flatten()
                    .map(|m| m.timestamp)
                    .unwrap_or_else(now),
                role: MessageRole::User,
                text: sub.as_ref().map(|s| s.text.clone()).unwrap_or(text),
                thinking: String::new(),
                status: MessageStatus::Complete,
                details: MessageDetails {
                    submission_id: Some(sid.clone()),
                    mode: sub.as_ref().map(|s| s.mode),
                    origin: sub
                        .as_ref()
                        .map(|s| s.origin)
                        .filter(|o| *o != SubmissionOrigin::Typed),
                    page_title: sub.as_ref().and_then(|s| s.page_title.clone()),
                    page_overview: sub.as_ref().and_then(|s| s.page_overview.clone()),
                    images: (!images.is_empty()).then_some(images),
                    refs: (!refs.is_empty()).then_some(refs),
                    ..Default::default()
                },
            };
            self.record(Entry::message(entry));
            return;
        }
        let key = self.msg_key(&mid);
        let live = self.stream.remove(&key);
        self.dirty.remove(&key);
        self.db_behind.remove(&key);
        if text.is_empty() {
            if let Some(l) = &live {
                text = l.text.clone();
            }
        }
        if thinking.is_empty() {
            if let Some(l) = &live {
                thinking = l.thinking.clone();
            }
        }
        let error_message = str_of(&msg, "errorMessage");
        if status == MessageStatus::Error {
            // An error row never carries its error as reply text: the error stays in the
            // details, so neither the transcript nor the phone mirrors read it as Claude's
            // answer. Claude Code reports a failed request as a message whose words are the
            // error; words that differ from it (a reply the error cut short) stay.
            let same = |e: &str| e.split_whitespace().eq(text.split_whitespace());
            if error_message.as_deref().is_some_and(same) {
                text.clear();
            }
        } else if text.is_empty() {
            if let Some(e) = &error_message {
                text = e.clone();
            }
        }
        let entry = MessageEntry {
            id: key.clone(),
            parent_id: None,
            timestamp: live.as_ref().map(|l| l.timestamp).unwrap_or_else(now),
            role: MessageRole::Assistant,
            text,
            thinking,
            status,
            details: MessageDetails {
                usage,
                stop_reason: str_of(&msg, "stopReason"),
                error_message,
                tools: (!tools.is_empty()).then_some(tools),
                ..Default::default()
            },
        };
        if status == MessageStatus::Error {
            self.last_error_row = Some(key);
        }
        self.record(Entry::message(entry));
    }

    fn flush(&mut self) {
        self.flush_rows(false);
    }

    /// Send the UI every dirty streaming row (one `message` event). The database copy
    /// is written at most every [`crate::runtime::Limits::stream_db`] (not an fsync
    /// per 80 ms tick; `synchronous=FULL` stays), when the row starts, and with `force` (the run
    /// ends, quit); in between the row goes out from memory. The session log is written at
    /// `message_end` as before.
    fn flush_rows(&mut self, force: bool) {
        let every = self.sh.cfg.limits.stream_db;
        let due: Vec<String> = self
            .db_behind
            .iter()
            .filter(|k| {
                force
                    || self
                        .stream
                        .get(*k)
                        .and_then(|m| m.written)
                        .is_none_or(|t| t.elapsed() >= every)
            })
            .cloned()
            .collect();
        self.dirty.extend(due);
        if self.dirty.is_empty() {
            return;
        }
        let keys: Vec<String> = std::mem::take(&mut self.dirty).into_iter().collect();
        let mut rows = Vec::with_capacity(keys.len());
        for k in &keys {
            let Some(m) = self.stream.get(k) else {
                continue;
            };
            // A row big enough to be cut into an artifact always takes the database path.
            let small = m.text.len() + m.thinking.len() <= session_log::ROW_TEXT_BYTES;
            let fresh = m.written.is_some_and(|t| t.elapsed() < every);
            if let Some(cached) = m.row.as_ref().filter(|_| !force && small && fresh) {
                let mut row = cached.clone();
                row.text = m.text.clone();
                row.thinking = m.thinking.clone();
                row.revision = self.sh.clock.next();
                rows.push(row);
                self.db_behind.insert(k.clone());
                continue;
            }
            {
                let row = RowWrite {
                    source_key: k.clone(),
                    role: match m.role {
                        MessageRole::User => "user",
                        _ => "assistant",
                    },
                    text: m.text.clone(),
                    thinking: m.thinking.clone(),
                    tool_name: None,
                    tool_call_id: None,
                    status: "streaming",
                    timestamp: m.timestamp,
                    details: json!({}),
                    entry_id: None,
                    parent_id: None,
                };
                self.upsert_row(&row);
            }
            let stored = self
                .sh
                .db
                .snapshot(|t| store::message_by_key(t, &self.chat_id, k))
                .ok()
                .flatten();
            if let Some(m) = self.stream.get_mut(k) {
                m.row = stored.clone();
                m.written = Some(Instant::now());
            }
            self.db_behind.remove(k);
            rows.extend(stored);
        }
        if rows.is_empty() {
            return;
        }
        let rev = self.sh.clock.next();
        self.emit(ChatEvent::Message {
            chat_id: self.chat_id.clone(),
            revision: rev,
            rows,
        });
    }

    fn on_response(&mut self, r: Map<String, Value>) {
        let id = r
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(p) = self.pending.remove(&id) else {
            return;
        };
        let ok = r.get("success") == Some(&json!(true));
        let data = r.get("data").cloned().unwrap_or(Value::Null);
        match p {
            Pending::Ready => self.on_ready(&r),
            Pending::Catalogue { reply } => {
                let mut stored = false;
                let models = crate::catalogue::from_get_state(&data)
                    .filter(|_| self.launched_local.is_none());
                if let Some(models) = models {
                    if crate::catalogue::store(&self.sh.cfg.profile_dir, models).is_ok() {
                        stored = true;
                        self.emit(ChatEvent::Catalogue {
                            harness: Harness::Claude,
                        });
                    }
                }
                let _ = reply.send(stored && ok);
            }
            Pending::Prompt { submission_id } => {
                self.clear_timer(|t| *t == Timer::Ack(id.clone()));
                if let Some(t) = self.timeline.as_mut() {
                    if t.submission_id == submission_id {
                        t.acked = Some(now());
                        t.prompt = r.get("timing").cloned();
                    }
                }
                let state = if ok {
                    SubmissionState::Accepted
                } else if r.get("uncertain") == Some(&json!(true)) {
                    SubmissionState::Uncertain
                } else {
                    SubmissionState::Rejected
                };
                self.finish_carry(&submission_id, state == SubmissionState::Accepted);
                // Delivery may already have been reported for a turn-opening prompt.
                let cur = self.submission(&submission_id).map(|s| s.state);
                if cur != Some(SubmissionState::Delivered) {
                    self.set_submission(&submission_id, state);
                }
                if state == SubmissionState::Accepted {
                    self.arm_wait_limit(&submission_id);
                } else if self.status() == ChatStatus::Starting && !ok {
                    let err = str_of(&Value::Object(r.clone()), "error")
                        .unwrap_or_else(|| "Claude Code did not take the message.".into());
                    let key = format!("u:{submission_id}");
                    let cid = self.chat_id.clone();
                    self.write(|t| {
                        t.execute(
                            "UPDATE messages SET status = 'error' WHERE chat_id = ?1 AND source_key = ?2",
                            [&cid, &key],
                        )
                    });
                    self.emit_rows(&[key]);
                    self.end_run(ChatStatus::Interrupted, Some(&err), "failed");
                }
                self.emit_pending();
            }
            Pending::Abort => {
                let cancelled: Vec<String> = data
                    .get("cancelledSubmissionIds")
                    .and_then(|c| serde_json::from_value(c.clone()).ok())
                    .unwrap_or_default();
                if !cancelled.is_empty() {
                    self.return_unread(&cancelled);
                }
                if data.get("state").and_then(Value::as_str) == Some("idle") {
                    self.settle("stopped", None);
                }
            }
            Pending::Model => {}
            Pending::Withdraw {
                submission_id,
                reply,
            } => {
                let withdrawn = ok && data.get("withdrawn").and_then(Value::as_bool) == Some(true);
                if withdrawn {
                    self.clear_timer(|t| *t == Timer::WaitLimit(submission_id.clone()));
                }
                let _ = reply.send(if ok {
                    Ok(withdrawn)
                } else {
                    Err(str_of(&Value::Object(r.clone()), "error").unwrap_or_default())
                });
            }
            Pending::DeliverNow { submission_id } => {
                let result = data
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if ok && result == "waiting" {
                    self.log_steer_wait(&submission_id, &data);
                    // Nothing movable yet: try again in a second (never an interrupt).
                    let lim = self.sh.cfg.limits.wait_retry;
                    self.add_timer(lim, Timer::WaitLimit(submission_id));
                }
            }
            Pending::ToolStop { reply } => {
                let stopped = ok && data.get("stopped").and_then(Value::as_bool) == Some(true);
                let _ = reply.send(if ok {
                    Ok(stopped)
                } else {
                    Err(str_of(&Value::Object(r.clone()), "error").unwrap_or_default())
                });
            }
            Pending::Compact { reply } => {
                let _ = reply.send(if ok {
                    Ok(())
                } else {
                    Err(str_of(&Value::Object(r.clone()), "error").unwrap_or_default())
                });
            }
        }
    }

    fn arm_wait_limit(&mut self, sid: &str) {
        let Some(sub) = self.submission(sid) else {
            return;
        };
        if sub.mode != SubmissionMode::Steer || !sub.origin.is_users_own() {
            return;
        }
        let Some(secs) = self
            .live
            .lock()
            .unwrap()
            .thread
            .send_behavior
            .wait_limit_seconds
        else {
            return;
        };
        self.add_timer(
            std::time::Duration::from_secs(u64::from(secs)),
            Timer::WaitLimit(sid.to_owned()),
        );
    }

    /// Steers the CLI gave back unread on an interrupt: they were never read, so they go back to
    /// the sendable state. When the stop settles they are handed to Claude again at once.
    fn return_unread(&mut self, ids: &[String]) {
        let chat = self.chat_id.clone();
        self.write(|t| {
            for sid in ids {
                t.execute(
                    "UPDATE submissions SET state = 'saved' WHERE id = ?1 AND chat_id = ?2
                     AND state IN ('accepted', 'dispatching')",
                    [sid, &chat],
                )?;
            }
            Ok(())
        });
        for sid in ids {
            self.clear_timer(|t| *t == Timer::WaitLimit(sid.clone()));
        }
        self.emit_pending();
    }

    /// A sign-in style failure while a Claude plan window reads used up is the limit, not a
    /// sign-in problem: the chat's error says so (and is not retried).
    fn limit_notice_for(&self, error: String) -> String {
        if self.launched_local.is_some() {
            return error;
        }
        let cap = helm_core::usage_windows::capacity(&self.sh.db, now());
        crate::outage::plan_limit_error(&chrono::Local, &error, cap).unwrap_or(error)
    }

    /// The turn is over (`agent_settled`, or a forced end).
    fn settle(&mut self, outcome: &str, error: Option<String>) {
        self.clear_timer(|t| matches!(t, Timer::StopDeadline | Timer::WaitLimit(_)));
        let stopping = self.stopping.take();
        let chat = self.chat_id.clone();
        // Delivered and turn-opening submissions of this turn are complete.
        self.write(|t| {
            t.execute(
                "UPDATE submissions SET state = 'complete' WHERE chat_id = ?1
                 AND (state = 'delivered' OR (state = 'accepted' AND mode != 'steer'))",
                [&chat],
            )
        });
        // Only this run's steers: one left over from an earlier worker or run is not coming, and
        // counting it would keep the chat running after every later turn.
        let run = self.run_id.clone();
        let unread_steers: i64 = self
            .sh
            .db
            .snapshot(|t| {
                t.query_row(
                    "SELECT count(*) FROM submissions s
                     JOIN submission_meta m ON m.submission_id = s.id
                     WHERE s.chat_id = ?1 AND s.state = 'accepted' AND s.mode = 'steer'
                       AND m.run_id = ?2",
                    rusqlite::params![chat, run],
                    |r| r.get(0),
                )
            })
            .unwrap_or(0);
        match stopping {
            Some(stop) => {
                let status = if outcome == "stopped" || self.archived {
                    ChatStatus::Stopped
                } else {
                    ChatStatus::Interrupted
                };
                let err = if status == ChatStatus::Interrupted {
                    error
                        .or_else(|| self.worker_error.clone())
                        .map(not_an_outage)
                } else {
                    None
                };
                self.end_run(status, err.as_deref(), outcome);
                self.stop_ended = true;
                // The stop's answer is how the turn ended; what Claude had not read is sent
                // again right after, which may open the next turn at once.
                let final_status = self.status();
                // A provider switch stopped a quiet turn to restart it: its opening message
                // goes out first, on the new worker the dispatch below starts. If the turn
                // actually finished before the stop landed, its answer stands and nothing is
                // sent again — the worker relaunches at this quiet point instead.
                if let Some(sid) = self.switch_restart.take() {
                    if outcome == "stopped" || outcome == "interrupted" {
                        self.write(|t| {
                            t.execute(
                                "UPDATE submissions SET state = 'saved', mode = 'prompt' WHERE id = ?1",
                                [&sid],
                            )
                        });
                        self.dispatch(&sid);
                    } else {
                        self.relaunch_on_idle = true;
                    }
                }
                self.settle_dropped(stop.resend);
                self.emit_pending();
                for r in stop.replies {
                    let _ = r.send(final_status);
                }
            }
            None => {
                if outcome == "complete" && unread_steers > 0 {
                    // An unread steer opens the CLI's next turn by itself; keep the chat running.
                    self.flush();
                    return;
                }
                let (status, err) = match outcome {
                    "complete" => (ChatStatus::Idle, None),
                    "stopped" => (ChatStatus::Stopped, None),
                    _ => (
                        ChatStatus::Interrupted,
                        error
                            .or_else(|| self.worker_error.clone())
                            .or_else(|| Some("The task ended without completing.".to_owned()))
                            .map(|e| self.limit_notice_for(e)),
                    ),
                };
                self.end_run(status, err.as_deref(), outcome);
                self.emit_pending();
                if status == ChatStatus::Idle {
                    let held = outcome == "complete" && self.stop_hold_offer(run);
                    if !held {
                        self.complete_turn();
                        if outcome == "complete" {
                            self.notify_parent_unreported();
                            self.recover_printed_tool_call();
                        }
                    }
                }
            }
        }
        if matches!(outcome, "complete" | "stopped") {
            if let (Some((server, model)), Some(local)) =
                (self.launched_local.as_ref(), self.sh.local.get())
            {
                local.finished(server, model);
            }
        }
        if !self.status().is_busy() {
            self.on_quiet_after(outcome == "stopped");
        }
    }
}

/// Subagents on the live thread and on their tool row, against the in-process fake worker.
/// The error kept for a turn that was stopped on purpose: what the worker said, unless that reads
/// as a lost connection or a service outage, which would have Helm carry the chat on by itself.
fn not_an_outage(error: String) -> String {
    if crate::outage::classify(&error).is_some() {
        "Stopped before the turn finished.".to_owned()
    } else {
        error
    }
}

#[cfg(test)]
mod subagent_tests {
    use crate::runtime_tests::{harness, FakeWorker, H};
    use serde_json::{json, Value};

    async fn running(h: &mut H) -> (String, FakeWorker) {
        let chat = h.create().await;
        h.send(&chat, "dispatch", "default").await;
        let mut w = h.worker().await;
        w.ready().await;
        let p = w.expect("prompt").await;
        w.respond(
            &p,
            json!({"submissionId": p["submissionId"], "state": "accepted"}),
        )
        .await;
        w.emit(json!({"type": "agent_start"})).await;
        h.until("running", |h| h.status(&chat) == "running").await;
        (chat, w)
    }

    /// Starts an Agent call `id` whose call returns at once (a background agent).
    async fn launch(w: &FakeWorker, id: &str) {
        w.emit(json!({"type": "tool_execution_start", "toolCallId": id, "toolName": "Agent", "args": {"description": "Echo alpha"}, "stoppable": false}))
            .await;
        w.emit(json!({"type": "subagent_start", "toolCallId": id, "agentType": "reviewer", "name": "Echo alpha", "description": "Echo alpha", "assignment": "Run echo alpha.", "startedAt": 1}))
            .await;
        w.emit(json!({"type": "tool_execution_end", "toolCallId": id, "toolName": "Agent", "result": {"text": "Async agent launched successfully."}, "isError": false}))
            .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_running_subagent_pushed_off_the_roster_still_settles_onto_its_row() {
        let mut h = harness();
        let (chat, w) = running(&mut h).await;
        launch(&w, "sa_0").await;
        // The rest only need their roster entry: no tool rows to write.
        for i in 1..31 {
            w.emit(json!({"type": "subagent_start", "toolCallId": format!("sa_{i}"), "name": format!("agent {i}")}))
                .await;
        }
        patiently(&h, "31 launched", |h| {
            h.rt.live_thread(&chat)
                .is_some_and(|t| t.subagents.iter().any(|s| s.tool_call_id == "sa_30"))
        })
        .await;
        let t = h.rt.live_thread(&chat).unwrap();
        assert_eq!(t.subagents.len(), 30);
        assert!(
            t.subagents.iter().all(|s| s.tool_call_id != "sa_0"),
            "the oldest was pushed off"
        );
        w.emit(json!({"type": "subagent_end", "toolCallId": "sa_0", "status": "completed", "report": "done early", "durationMs": 10}))
            .await;
        patiently(&h, "the evicted agent settled on its row", |h| {
            subagent_col(h, "sa_0", ".status") == ["completed"]
        })
        .await;
        assert_eq!(subagent_col(&h, "sa_0", ".report"), ["done early"]);
        assert_eq!(subagent_col(&h, "sa_0", ".name"), ["Echo alpha"]);
    }

    /// `H::until` allows two seconds; this test sends 30 events and is run under heavy load too.
    async fn patiently(h: &H, what: &str, f: impl Fn(&H) -> bool) {
        for _ in 0..1000 {
            if f(h) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    fn subagent_col(h: &H, id: &str, path: &str) -> Vec<String> {
        h.col(&format!(
            "SELECT json_extract(details, '$.subagent{path}') FROM messages WHERE source_key = 't:{id}'"
        ))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_subagent_lives_on_the_thread_and_settles_onto_its_tool_row() {
        let mut h = harness();
        let (chat, w) = running(&mut h).await;
        launch(&w, "sa_1").await;
        h.until("tool row ended", |h| {
            h.col("SELECT status FROM messages WHERE source_key = 't:sa_1'") == ["complete"]
        })
        .await;
        let t = h.rt.live_thread(&chat).unwrap();
        assert_eq!(t.subagents.len(), 1);
        assert_eq!(t.subagents[0].agent_type, "reviewer");
        assert_eq!(t.subagents[0].assignment, "Run echo alpha.");
        assert_eq!(t.subagents_running, 1);

        w.emit(json!({"type": "subagent_step", "toolCallId": "sa_1", "tool": "Bash", "text": "echo alpha", "tokens": 1200, "toolUses": 1}))
            .await;
        h.until("step on the thread", |h| {
            h.rt.live_thread(&chat).is_some_and(|t| {
                t.subagents[0].step.as_deref() == Some("echo alpha")
                    && t.subagents[0].step_tool.as_deref() == Some("Bash")
                    && t.subagents[0].tokens == Some(1200)
            })
        })
        .await;
        assert_eq!(
            subagent_col(&h, "sa_1", ""),
            ["NULL"],
            "a step is never written to the database"
        );

        // The parent's turn ends while its agent works on: idle, with a running subagent.
        w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
            .await;
        h.until("idle", |h| h.status(&chat) == "idle").await;
        assert_eq!(h.rt.live_thread(&chat).unwrap().subagents_running, 1);

        let report = "r".repeat(3000);
        w.emit(json!({"type": "subagent_end", "toolCallId": "sa_1", "status": "completed", "report": report, "durationMs": 4200, "tokens": 1500, "toolUses": 2}))
            .await;
        h.until("settled on the row", |h| {
            subagent_col(h, "sa_1", ".status") == ["completed"]
        })
        .await;
        assert_eq!(subagent_col(&h, "sa_1", ".report"), [report.as_str()]);
        assert_eq!(subagent_col(&h, "sa_1", ".durationMs"), ["4200"]);
        assert_eq!(subagent_col(&h, "sa_1", ".tokens"), ["1500"]);
        // The tool row keeps its own details beside the subagent's.
        assert_eq!(
            h.col("SELECT json_extract(details, '$.toolArgs') FROM messages WHERE source_key = 't:sa_1'"),
            [r#"{"description":"Echo alpha"}"#]
        );
        let t = h.rt.live_thread(&chat).unwrap();
        assert_eq!(t.subagents_running, 0);
        let s = &t.subagents[0];
        assert_eq!(s.status, crate::types::SubagentStatus::Completed);
        assert_eq!(
            s.report.as_deref().map(str::len),
            Some(2048),
            "the live thread carries a preview"
        );
        assert!(s.report_truncated);
        // The row event carries the settled subagent to the UI. `persist_subagent` emits it after
        // the write it reads back, so the database can show the row before the event is out: wait
        // for the event itself rather than reading the sink the moment the row lands.
        h.until("a message event carries details.subagent", |h| {
            h.events("message").iter().any(|e| {
                e["rows"].as_array().is_some_and(|rows| {
                    rows.iter()
                        .any(|r| r["details"]["subagent"]["status"] == "completed")
                })
            })
        })
        .await;
        let rows: Vec<Value> = h
            .events("message")
            .iter()
            .flat_map(|e| e["rows"].as_array().cloned().unwrap_or_default())
            .filter(|r| r["details"]["subagent"]["status"] == "completed")
            .collect();
        assert!(!rows.is_empty(), "a message event carries details.subagent");
        assert_eq!(rows[0]["details"]["subagent"]["reportTruncated"], false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tool_stop_reaches_a_running_subagent_and_a_worker_exit_stops_the_rest() {
        let mut h = harness();
        let (chat, mut w) = running(&mut h).await;
        launch(&w, "sa_2").await;
        h.until("subagent on the thread", |h| {
            h.rt.live_thread(&chat)
                .is_some_and(|t| t.subagents_running == 1)
        })
        .await;
        // Its Agent call has returned, yet Stop still goes to the worker for the agent.
        let (r, _) = tokio::join!(
            h.call("tool_stop", json!({"chatId": chat, "toolCallId": "sa_2"})),
            async {
                let c = w.expect("stop_tool").await;
                assert_eq!(c["toolCallId"], "sa_2");
                w.respond(&c, json!({"stopped": true})).await;
            }
        );
        assert_eq!(r.unwrap()["stopped"], true);
        // An id that is neither a live tool nor a running agent is refused without asking.
        let refused = h
            .call("tool_stop", json!({"chatId": chat, "toolCallId": "nope"}))
            .await;
        assert!(refused.is_err());

        w.exit().await;
        h.until("stopped on the row", |h| {
            subagent_col(h, "sa_2", ".status") == ["stopped"]
        })
        .await;
        assert_eq!(
            subagent_col(&h, "sa_2", ".error"),
            ["The worker stopped before this agent finished."]
        );
        assert_eq!(h.rt.live_thread(&chat).unwrap().subagents_running, 0);
    }
}

/// Test-only count of the actor's write transactions per chat (the opening-writes test).
#[cfg(test)]
pub(crate) mod write_counts {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    static COUNTS: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

    pub(crate) fn bump(chat: &str) {
        *COUNTS.lock().unwrap().entry(chat.to_owned()).or_default() += 1;
    }

    pub(crate) fn get(chat: &str) -> usize {
        COUNTS.lock().unwrap().get(chat).copied().unwrap_or(0)
    }

    static LAST_DISPATCH: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

    pub(crate) fn set_last_dispatch(chat: &str, n: usize) {
        LAST_DISPATCH.lock().unwrap().insert(chat.to_owned(), n);
    }

    /// Write transactions the chat's most recent `dispatch` made.
    pub(crate) fn last_dispatch(chat: &str) -> usize {
        LAST_DISPATCH
            .lock()
            .unwrap()
            .get(chat)
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod queue_reading_tests {
    use super::*;

    /// A failed read keeps the last good answer only briefly: a hung server must read as unknown,
    /// not as busy for ever (which would hold the "may be stuck" flag off).
    #[test]
    fn an_old_server_reading_is_forgotten() {
        let busy = Some(crate::local::QueueInfo {
            busy: true,
            queued: None,
        });
        let at = |ago: u64| Instant::now().checked_sub(std::time::Duration::from_secs(ago));
        for (info, good_at, want) in [
            (busy, at(1), busy),
            (busy, at(QUEUE_KEEP.as_secs() + 5), None),
            (busy, None, None),
            (None, at(1), None),
        ] {
            assert_eq!(recent_reading(info, good_at), want);
        }
    }
}
