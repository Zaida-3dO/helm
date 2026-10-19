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
//! - Stop sends `abort`; a clean settle is `stopped`, anything else `interrupted`. Stop makes
//!   the remaining queue `uncertain`; Esc lets it continue. Steers the CLI dropped come back to
//!   the front of the queue. No settle within the stop limit closes the worker.
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
    /// Re-check the queue (after `queue_resume`).
    Kick,
    Stop {
        continue_queue: bool,
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
    DeliverNow {
        submission_id: String,
        reason: &'static str,
    },
    /// Quit: shut the worker down and answer once it has exited.
    Shutdown { reply: oneshot::Sender<()> },
    /// Quit timed out: end the worker tree now.
    Terminate,
    /// Idle over the cap: shut the worker down.
    Retire,
    /// The maintenance tick: mark the chat stalled if it has had no output for `after`.
    StallCheck { after: std::time::Duration },
    /// Apply the staged edit once the worker has closed, then send its submission.
    ApplyEdit,
    /// The send behaviour changed: a different long-command setting relaunches
    /// the worker, at once when idle and at the next idle point when busy.
    SendBehavior(SendBehavior),
    /// Re-read the model catalogue from the live worker's `get_state`; answers whether it did.
    RefreshCatalogue { reply: oneshot::Sender<bool> },
    /// A line for the worker from another crate (`WorkerBroadcast`); held until it is ready.
    WorkerLine(Value),
    /// Start the worker ahead of the first send ([`crate::runtime::ChatRuntime::prewarm`]):
    /// `get_state` only, so Claude Code's start-up runs while the user types.
    Prewarm,
}

pub(crate) const STOP_TIMEOUT_ERROR: &str =
    "The task did not stop within 60 seconds, so Helm closed it.";
pub(crate) const ACK_TIMEOUT_ERROR: &str =
    "Claude Code did not confirm the message in time. It was not sent again; recover it from the queue if it is missing.";
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
    started_at: i64,
    stoppable: bool,
}

struct StopState {
    continue_queue: bool,
    replies: Vec<oneshot::Sender<ChatStatus>>,
}

struct Actor {
    sh: Arc<Shared>,
    chat_id: String,
    live: Arc<Mutex<Live>>,
    link: Option<Arc<dyn WorkerLink>>,
    out: Option<mpsc::Receiver<WorkerOutput>>,
    ready: bool,
    deferred: Vec<Value>,
    next_id: u64,
    pending: HashMap<String, Pending>,
    timers: Vec<(TInstant, Timer)>,
    run_id: Option<String>,
    stopping: Option<StopState>,
    stream: HashMap<String, LiveMsg>,
    dirty: BTreeSet<String>,
    /// Streaming rows whose database copy is behind what the UI was sent.
    db_behind: BTreeSet<String>,
    tools: HashMap<String, LiveToolRec>,
    log: Option<SessionLog>,
    worker_error: Option<String>,
    last_error_row: Option<String>,
    native_session: Option<String>,
    shutdown_replies: Vec<oneshot::Sender<()>>,
    /// Host-tool calls still running, by request id (`chat_cancel` aborts them).
    host_calls: HashMap<String, tokio::task::AbortHandle>,
    retiring: bool,
    finished: bool,
    /// The last worker output (or turn start).
    last_output: Instant,
    /// When `chat_activity.last_activity_at` was last written (writes are at most every 15 s).
    activity_written: Option<Instant>,
    /// An edit waits for the worker to close.
    edit_waiting: bool,
    /// The long-command setting the running worker was launched with.
    launched_long: Option<LongCommands>,
    /// That setting changed while a turn ran: relaunch at the next idle point.
    relaunch_on_idle: bool,
    /// The turn-opening send being timed, until its first output ([`crate::timeline`]).
    timeline: Option<crate::timeline::SendTimeline>,
    /// The last worker start that failed: pre-warm waits [`PREWARM_RETRY`] before trying again.
    spawn_failed_at: Option<Instant>,
}

/// After a failed worker start, typing does not start another for this long (sends still do).
const PREWARM_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

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
            capabilities: Capabilities::default(),
            send_behavior: crate::send_behavior::HOME_DEFAULTS,
            queue_held: false,
            revision: 0,
        });
    let live = Arc::new(Mutex::new(Live {
        thread,
        has_worker: false,
        last_used: Instant::now(),
        stalled: false,
        skills: Vec::new(),
        context_gen: 0,
    }));
    let actor = Actor {
        sh: sh.clone(),
        chat_id,
        live: live.clone(),
        link: None,
        out: None,
        ready: false,
        deferred: Vec::new(),
        next_id: 0,
        pending: HashMap::new(),
        timers: Vec::new(),
        run_id: None,
        stopping: None,
        stream: HashMap::new(),
        dirty: BTreeSet::new(),
        db_behind: BTreeSet::new(),
        tools: HashMap::new(),
        log: None,
        worker_error: None,
        last_error_row: None,
        native_session: None,
        shutdown_replies: Vec::new(),
        host_calls: HashMap::new(),
        retiring: false,
        finished: false,
        last_output: Instant::now(),
        activity_written: None,
        edit_waiting: false,
        launched_long: None,
        relaunch_on_idle: false,
        timeline: None,
        spawn_failed_at: None,
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
        self.sh.actors.lock().unwrap().remove(&self.chat_id);
        // Its blocking requests (if any) leave the waiting tracker with it.
        crate::attention::publish(&self.sh);
    }

    // ------------------------------------------------------------------ plumbing

    fn write<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<T>,
    ) -> Option<T> {
        match self.sh.db.write(f) {
            Ok(v) => Some(v),
            Err(e) => {
                log::error!("chat {}: database write failed: {e}", self.chat_id);
                None
            }
        }
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
            self.emit(ChatEvent::Chat { chat });
        }
    }

    fn emit_queue(&self) {
        let rev = self.sh.clock.next();
        if let Ok(pending) = self.sh.db.snapshot(|t| store::pending(t, &self.chat_id)) {
            self.emit(ChatEvent::Queue {
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
        self.write(|t| {
            store::set_status(t, &id, status, error, now())?;
            // Entering an active or stopped state drops the completion.
            if status.is_busy() || status == ChatStatus::Stopped {
                crate::manage::drop_completion(t, &id)?;
            }
            Ok(())
        });
        self.live.lock().unwrap().thread.status = status;
        if !status.is_busy() && self.live.lock().unwrap().stalled {
            // A turn that ends is no longer stalled.
            self.set_stalled(false);
        }
        self.emit_chat();
        crate::attention::publish(&self.sh);
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
        self.emit_rows(&[key]);
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
        let sb = self.live.lock().unwrap().thread.send_behavior;
        self.launched_long = Some(sb.long_commands);
        self.relaunch_on_idle = false;
        let req = SpawnRequest {
            chat_id: self.chat_id.clone(),
            cwd: chat.cwd.clone(),
            model: chat.model.clone(),
            effort: chat.effort.clone(),
            resume_session_id: resume,
            fork_session,
            send_behavior: sb,
            host: crate::hostcalls::launch(&self.sh, &self.chat_id, &chat.cwd),
        };
        if let Some(t) = self.timeline.as_mut() {
            t.spawn = Some(now());
        }
        // The start itself runs on the blocking pool; a failure comes back as `SpawnFailed`.
        let (link, out) = crate::spawning::spawn_off_actor(&self.sh, req);
        self.link = Some(link);
        self.out = Some(out);
        self.ready = false;
        self.worker_error = None;
        {
            let mut live = self.live.lock().unwrap();
            live.has_worker = true;
            live.context_gen += 1;
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
        self.out = None;
        self.ready = false;
        self.deferred.clear();
        self.live.lock().unwrap().has_worker = false;
        for (_, p) in std::mem::take(&mut self.pending) {
            match p {
                Pending::Prompt { submission_id } => {
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
        {
            let mut live = self.live.lock().unwrap();
            live.thread.capabilities = caps;
            live.skills = skills;
        }
        if let Some(models) = crate::catalogue::from_get_state(&data) {
            if crate::catalogue::store(&self.sh.cfg.profile_dir, models).is_ok() {
                self.emit(ChatEvent::Catalogue {
                    harness: Harness::Claude,
                });
            }
        }
        self.ready = true;
        // Registered start lines (for example `mcp_config`) go ahead of anything deferred.
        let start = crate::hostcalls::start_lines(&self.sh, &self.chat_id);
        for cmd in start.into_iter().chain(std::mem::take(&mut self.deferred)) {
            if let Some(l) = &self.link {
                l.send(&cmd);
            }
        }
        self.emit_thread();
    }

    fn on_exit(&mut self, reason: String) {
        self.link = None;
        self.out = None;
        self.ready = false;
        self.deferred.clear();
        self.live.lock().unwrap().has_worker = false;
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
                    self.set_submission(&submission_id, SubmissionState::Uncertain);
                }
                _ => {}
            }
        }
        self.timers.clear();
        if self.status().is_busy() {
            let error = if self
                .sh
                .shutting_down
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                helm_core::recovery::INTERRUPTED_ERROR.to_owned()
            } else {
                self.worker_error.clone().unwrap_or(reason)
            };
            // In-flight submissions are now unknowable (never replayed). Written before the status,
            // so no reader sees `interrupted` while they still look in flight.
            let id = self.chat_id.clone();
            self.write(|t| {
                t.execute(
                    "UPDATE submissions SET state = 'uncertain' WHERE chat_id = ?1 AND state IN ('dispatching', 'accepted')",
                    [&id],
                )
            });
            self.end_run(ChatStatus::Interrupted, Some(&error), "interrupted");
            self.emit_queue();
            // A crash holds the queue; a quit does not (startup recovery decides that).
            // Done after the submission update so the extra write does not widen the window in
            // which a reader sees `interrupted` before the in-flight submissions turn uncertain.
            if !self
                .sh
                .shutting_down
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                self.hold_queue();
            }
            if let Some(s) = self.stopping.take() {
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
                "UPDATE submissions SET state = 'rejected' WHERE chat_id = ?1 AND state = 'saved'
                 AND id NOT IN (SELECT submission_id FROM slot_waits)",
                [&id],
            )
        });
        if self.status().is_busy() {
            self.end_run(ChatStatus::Interrupted, Some(error), "failed");
        }
        self.emit_queue();
    }

    // ------------------------------------------------------------------ runs

    fn open_run(&mut self) {
        if self.run_id.is_some() {
            return;
        }
        self.last_output = Instant::now();
        let run = uuid::Uuid::new_v4().to_string();
        let id = self.chat_id.clone();
        let chat = self.chat();
        self.write(|t| {
            t.execute(
                "INSERT INTO runs(id, chat_id, cwd, model, thinking, status, started_at) VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6)",
                rusqlite::params![
                    run,
                    id,
                    chat.as_ref().map(|c| c.cwd.clone()),
                    chat.as_ref().and_then(|c| c.model.clone()),
                    chat.as_ref().and_then(|c| c.effort.clone()),
                    now()
                ],
            )
        });
        self.run_id = Some(run);
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
        }
        self.set_status(status, error);
        self.save_binding();
        self.emit_thread();
    }

    /// Close the idle worker so the next send starts one with the current settings.
    fn relaunch_worker(&mut self) {
        self.relaunch_on_idle = false;
        if let Some(l) = &self.link {
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
                self.emit_queue();
                self.emit_chat();
                self.dispatch(&sid, false);
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
                    "The edit could not be applied; your message is kept in the queue to recover.",
                );
                self.emit_queue();
            }
        }
    }

    /// Worker output arrived: note the activity (written at most every 15 s) and end a stall.
    fn note_output(&mut self) {
        self.last_output = Instant::now();
        if self.live.lock().unwrap().stalled {
            self.set_stalled(false);
        }
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

    fn queue_held(&self) -> bool {
        self.sh
            .db
            .snapshot(|t| {
                t.query_row(
                    "SELECT EXISTS(SELECT 1 FROM chat_queue_holds WHERE chat_id = ?1)",
                    [&self.chat_id],
                    |r| r.get::<_, bool>(0),
                )
            })
            .unwrap_or(false)
    }

    /// Holds the queue (shown with Resume) until the user sends or resumes it.
    fn hold_queue(&mut self) {
        let id = self.chat_id.clone();
        self.write(|t| {
            t.execute(
                "INSERT OR IGNORE INTO chat_queue_holds(chat_id, held_at) VALUES (?1, ?2)",
                rusqlite::params![id, now()],
            )
        });
        self.live.lock().unwrap().thread.queue_held = true;
        self.emit_thread();
    }

    /// Start the next queued follow-up, if the queue may advance.
    fn advance_queue(&mut self) {
        if self.status().is_busy() || self.queue_held() {
            return;
        }
        let id = self.chat_id.clone();
        let next: Option<String> = self
            .sh
            .db
            .snapshot(|t| {
                use rusqlite::OptionalExtension;
                t.query_row(
                    "SELECT s.id FROM queued_submissions q JOIN submissions s ON s.id = q.submission_id
                     WHERE q.chat_id = ?1 AND s.state = 'queued' ORDER BY q.position, s.created_at LIMIT 1",
                    [&id],
                    |r| r.get(0),
                )
                .optional()
            })
            .ok()
            .flatten();
        if let Some(sid) = next {
            self.write(|t| {
                t.execute(
                    "DELETE FROM queued_submissions WHERE submission_id = ?1",
                    [&sid],
                )
            });
            self.dispatch(&sid, true);
        }
    }

    // ------------------------------------------------------------------ commands

    fn on_cmd(&mut self, c: Cmd) {
        self.touch();
        match c {
            Cmd::Dispatch(sid) => self.dispatch(&sid, false),
            Cmd::Kick => self.advance_queue(),
            Cmd::Stop {
                continue_queue,
                reply,
            } => self.stop(continue_queue, reply),
            Cmd::Answer(mut fields) => {
                let rid = fields
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
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
            Cmd::ApplyModel { model, effort } => {
                if self.link.is_none() {
                    return; // the next launch carries it
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
            Cmd::DeliverNow {
                submission_id,
                reason,
            } => self.deliver_now(&submission_id, reason),
            Cmd::Shutdown { reply } => {
                if self.link.is_none() {
                    let _ = reply.send(());
                    self.finished = true;
                    return;
                }
                self.shutdown_replies.push(reply);
                self.flush_rows(true);
                if let Some(l) = &self.link {
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
                    if self.status().is_busy() {
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
                if self.link.is_none() && !self.status().is_busy() && !quitting && !backing_off {
                    self.ensure_worker();
                }
            }
            Cmd::StallCheck { after } => {
                let stalled = self.live.lock().unwrap().stalled;
                if self.status().is_busy() && !stalled && self.last_output.elapsed() >= after {
                    self.set_stalled(true);
                }
            }
            Cmd::Retire => {
                if self.link.is_none() {
                    // No worker to shut down: the actor itself goes (archive, delete).
                    self.finished = true;
                } else if !self.status().is_busy() {
                    self.retiring = true;
                    if let Some(l) = &self.link {
                        l.send(&json!({"type": "shutdown"}));
                    }
                }
            }
        }
    }

    fn dispatch(&mut self, sid: &str, from_queue: bool) {
        let Some(sub) = self.submission(sid) else {
            return;
        };
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
        let busy = self.status().is_busy();
        if !busy {
            self.set_status(ChatStatus::Starting, None);
            self.open_run();
        }
        if from_queue || !busy {
            // A prompt that opens a turn: its user row shows at once, as saved.
            let mut details = json!({"submissionId": sid, "mode": sub.mode});
            let images = crate::message_images::for_submission(&self.sh.db, sid);
            if !images.is_empty() {
                details["images"] = json!(images);
            }
            let row = RowWrite {
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
            };
            self.upsert_row(&row);
            self.emit_rows(std::slice::from_ref(&row.source_key));
        }
        if !busy {
            let mut t = crate::timeline::SendTimeline::new(sid, sub.created_at, now());
            t.warm = self.ready;
            t.warming = self.link.is_some() && !self.ready;
            self.timeline = Some(t);
        }
        if !self.ensure_worker() {
            return;
        }
        let run = self.run_id.clone();
        self.write(|t| {
            t.execute(
                "UPDATE submissions SET state = 'dispatching' WHERE id = ?1",
                [sid],
            )?;
            t.execute(
                "UPDATE submission_meta SET run_id = ?1 WHERE submission_id = ?2",
                rusqlite::params![run, sid],
            )
        });
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
        // A message fork's carried history goes once, with its first prompt.
        let carry: Option<String> = self
            .write(|t| {
                use rusqlite::OptionalExtension;
                let c: Option<String> = t
                    .query_row(
                        "SELECT text FROM fork_carry WHERE chat_id = ?1",
                        [&self.chat_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                t.execute("DELETE FROM fork_carry WHERE chat_id = ?1", [&self.chat_id])?;
                Ok(c)
            })
            .flatten();
        let context = match (carry, context) {
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
        let id = self.request(
            cmd,
            Pending::Prompt {
                submission_id: sid.to_owned(),
            },
        );
        let lim = self.sh.cfg.limits.ack;
        self.add_timer(lim, Timer::Ack(id));
        self.emit_queue();
    }

    fn stop(&mut self, continue_queue: bool, reply: oneshot::Sender<ChatStatus>) {
        let status = self.status();
        if !status.is_busy() {
            let _ = reply.send(status);
            return;
        }
        if let Some(s) = self.stopping.as_mut() {
            s.continue_queue = continue_queue;
            s.replies.push(reply);
            return;
        }
        self.stopping = Some(StopState {
            continue_queue,
            replies: vec![reply],
        });
        self.set_status(ChatStatus::Stopping, None);
        let lim = self.sh.cfg.limits.stop;
        self.add_timer(lim, Timer::StopDeadline);
        if self.link.is_none() {
            // Nothing is running behind the status: settle at once.
            self.settle("stopped", None);
            return;
        }
        self.request(
            json!({"type": "abort", "continueQueue": continue_queue}),
            Pending::Abort,
        );
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

    // ------------------------------------------------------------------ timers

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
                        self.set_submission(&submission_id, SubmissionState::Uncertain);
                        if self.status() == ChatStatus::Starting {
                            // The turn never began: end it rather than wait on a Stop.
                            self.end_run(
                                ChatStatus::Interrupted,
                                Some(ACK_TIMEOUT_ERROR),
                                "failed",
                            );
                        }
                        self.emit_queue();
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
                        self.worker_error = Some(STOP_TIMEOUT_ERROR.into());
                        if let Some(l) = &self.link {
                            l.terminate();
                        } else {
                            self.settle("interrupted", Some(STOP_TIMEOUT_ERROR.into()));
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
            }
        }
    }

    // ------------------------------------------------------------------ worker output

    fn on_output(&mut self, out: Option<WorkerOutput>) {
        match out {
            None => self.on_exit("The worker stopped unexpectedly.".into()),
            Some(WorkerOutput::Exited { code, stderr }) => {
                let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" / ");
                let reason = if tail.is_empty() {
                    format!("The worker stopped (exit code {code:?}).")
                } else {
                    format!("The worker stopped (exit code {code:?}): {tail}")
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

    fn on_record(&mut self, r: Map<String, Value>) {
        let v = Value::Object(r.clone());
        let kind = r.get("type").and_then(Value::as_str).unwrap_or_default();
        if matches!(kind, "message_update" | "tool_execution_start") {
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
                if let Some(m) = self.stream.get_mut(&key) {
                    let first = m.text.is_empty() && m.thinking.is_empty();
                    let text = str_of(&delta, "text").unwrap_or_default();
                    match str_of(&delta, "type").as_deref() {
                        Some("text_delta") => m.text.push_str(&text),
                        Some("thinking_delta") => m.thinking.push_str(&text),
                        _ => {}
                    }
                    self.dirty.insert(key);
                    if first {
                        // The message's first words go out now, not on the next tick.
                        self.flush();
                    }
                }
            }
            "message_end" => self.on_message_end(&v),
            "tool_execution_start" => {
                let id = str_of(&v, "toolCallId").unwrap_or_default();
                let name = str_of(&v, "toolName").unwrap_or_default();
                let args = v.get("args").map(Value::to_string).unwrap_or_default();
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
                {
                    let mut live = self.live.lock().unwrap();
                    live.thread.interactions.retain(|i| i.id != id);
                    if method != "dismiss" {
                        let mut obj = r.clone();
                        obj.remove("type");
                        obj.remove("remaining");
                        obj.insert("chatId".into(), json!(self.chat_id));
                        match serde_json::from_value::<Interaction>(Value::Object(obj)) {
                            Ok(i) => live.thread.interactions.push(i),
                            Err(e) => {
                                log::warn!("chat {}: unreadable interaction: {e}", self.chat_id)
                            }
                        }
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
                    let accepted = self
                        .submission(&sid)
                        .is_some_and(|s| s.state == SubmissionState::Accepted);
                    if accepted {
                        self.set_submission(&sid, SubmissionState::Delivered);
                        self.clear_timer(|t| *t == Timer::WaitLimit(sid.clone()));
                        self.emit_queue();
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
            "subagent_start" | "subagent_step" | "subagent_end" => self.on_subagent(kind, &v),
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
                        list.remove(i);
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
        let usage = msg.get("usage").and_then(|u| {
            Some(Usage {
                input_tokens: u.get("inputTokens")?.as_i64().unwrap_or(0),
                output_tokens: u.get("outputTokens").and_then(Value::as_i64).unwrap_or(0),
                cache_read_tokens: u
                    .get("cacheReadTokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                cache_write_tokens: u
                    .get("cacheWriteTokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                cost_usd: u.get("costUsd").and_then(Value::as_f64).unwrap_or(0.0),
            })
        });
        if role == "user" {
            // The worker's echo of one of our submissions (id = submission id).
            let sid = str_of(&msg, "submissionId").unwrap_or(mid);
            let sub = self.submission(&sid);
            let images = crate::message_images::for_submission(&self.sh.db, &sid);
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
                    mode: sub.map(|s| s.mode),
                    images: (!images.is_empty()).then_some(images),
                    ..Default::default()
                },
            };
            self.record(Entry::Message(entry));
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
        if text.is_empty() {
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
        self.record(Entry::Message(entry));
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
                if let Some(models) = crate::catalogue::from_get_state(&data) {
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
                self.emit_queue();
            }
            Pending::Abort => {
                let cancelled: Vec<String> = data
                    .get("cancelledSubmissionIds")
                    .and_then(|c| serde_json::from_value(c.clone()).ok())
                    .unwrap_or_default();
                if !cancelled.is_empty() {
                    self.requeue_front(&cancelled);
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

    /// Steers the CLI dropped on an interrupt go back to the front of the queue.
    fn requeue_front(&mut self, ids: &[String]) {
        let chat = self.chat_id.clone();
        self.write(|t| {
            let min: i64 = t.query_row(
                "SELECT COALESCE(MIN(position), 0) FROM queued_submissions WHERE chat_id = ?1",
                [&chat],
                |r| r.get(0),
            )?;
            for (i, sid) in ids.iter().enumerate() {
                let pos = min - (ids.len() - i) as i64;
                let n = t.execute(
                    "UPDATE submissions SET state = 'queued', mode = 'followUp' WHERE id = ?1 AND chat_id = ?2",
                    [sid, &chat],
                )?;
                if n > 0 {
                    t.execute(
                        "INSERT INTO queued_submissions(submission_id, chat_id, position) VALUES (?1, ?2, ?3)
                         ON CONFLICT(submission_id) DO UPDATE SET position = excluded.position",
                        rusqlite::params![sid, chat, pos],
                    )?;
                }
            }
            Ok(())
        });
        for sid in ids {
            self.clear_timer(|t| *t == Timer::WaitLimit(sid.clone()));
        }
        self.emit_queue();
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
        let unread_steers: i64 = self
            .sh
            .db
            .snapshot(|t| {
                t.query_row(
                    "SELECT count(*) FROM submissions WHERE chat_id = ?1 AND state = 'accepted' AND mode = 'steer'",
                    [&chat],
                    |r| r.get(0),
                )
            })
            .unwrap_or(0);
        match stopping {
            Some(stop) => {
                let status = if outcome == "stopped" {
                    ChatStatus::Stopped
                } else {
                    ChatStatus::Interrupted
                };
                let err = if status == ChatStatus::Interrupted {
                    error.or_else(|| self.worker_error.clone())
                } else {
                    None
                };
                self.end_run(status, err.as_deref(), outcome);
                if stop.continue_queue {
                    self.advance_queue();
                } else {
                    // Stop pauses: what is still queued becomes uncertain.
                    self.write(|t| {
                        t.execute(
                            "UPDATE submissions SET state = 'uncertain' WHERE chat_id = ?1 AND state IN ('queued', 'accepted')",
                            [&chat],
                        )
                    });
                }
                self.emit_queue();
                let final_status = self.status();
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
                            .or_else(|| Some("The task ended without completing.".to_owned())),
                    ),
                };
                self.end_run(status, err.as_deref(), outcome);
                if status == ChatStatus::Interrupted {
                    // An abnormal end (error or crash, not a user Stop) holds the queue until the
                    // user sends or presses Resume, so a failing turn cannot run through every
                    // queued message.
                    self.hold_queue();
                }
                self.emit_queue();
                if status == ChatStatus::Idle {
                    self.complete_turn();
                    self.advance_queue();
                }
            }
        }
        if !self.status().is_busy() {
            if self.relaunch_on_idle {
                self.relaunch_worker();
            }
            crate::runtime::ChatRuntime {
                sh: self.sh.clone(),
            }
            .retire_idle_over_cap();
        }
    }
}

/// Subagents on the live thread and on their tool row, against the in-process fake worker.
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
        // The row event carries the settled subagent to the UI.
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
