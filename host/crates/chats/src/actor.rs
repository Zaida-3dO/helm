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
    tools: HashMap<String, LiveToolRec>,
    log: Option<SessionLog>,
    worker_error: Option<String>,
    last_error_row: Option<String>,
    native_session: Option<String>,
    shutdown_replies: Vec<oneshot::Sender<()>>,
    retiring: bool,
    finished: bool,
    /// The last worker output (or turn start).
    last_output: Instant,
    /// When `chat_activity.last_activity_at` was last written (writes are at most every 15 s).
    activity_written: Option<Instant>,
}

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
        tools: HashMap::new(),
        log: None,
        worker_error: None,
        last_error_row: None,
        native_session: None,
        shutdown_replies: Vec::new(),
        retiring: false,
        finished: false,
        last_output: Instant::now(),
        activity_written: None,
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
                _ = flush.tick(), if !self.dirty.is_empty() => self.flush(),
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
        let rev = self.sh.clock.next();
        let thread = {
            let mut live = self.live.lock().unwrap();
            live.thread.revision = rev;
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
        crate::attention::publish(&self.sh);
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
        let binding = ClaudeBinding::load(&self.sh.cfg.sessions(), &self.chat_id);
        let resume = binding
            .filter(|b| b.cwd == chat.cwd && b.harness_switch_id.is_none())
            .and_then(|b| b.native_session_id);
        self.native_session = resume.clone();
        let sb = self.live.lock().unwrap().thread.send_behavior;
        let req = SpawnRequest {
            chat_id: self.chat_id.clone(),
            cwd: chat.cwd.clone(),
            model: chat.model.clone(),
            effort: chat.effort.clone(),
            resume_session_id: resume,
            send_behavior: sb,
        };
        match self.sh.spawner.spawn(&req) {
            Ok((link, out)) => {
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
            Err(e) => {
                log::error!("chat {}: {e}", self.chat_id);
                self.fail_run(&e);
                false
            }
        }
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
        let data = resp.get("data").cloned().unwrap_or(Value::Null);
        let caps: Capabilities = data
            .get("capabilities")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default();
        self.live.lock().unwrap().thread.capabilities = caps;
        if let Some(models) = crate::catalogue::from_get_state(&data) {
            if crate::catalogue::store(&self.sh.cfg.profile_dir, models).is_ok() {
                self.emit(ChatEvent::Catalogue {
                    harness: Harness::Claude,
                });
            }
        }
        self.ready = true;
        for cmd in std::mem::take(&mut self.deferred) {
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
        for r in self.shutdown_replies.drain(..) {
            let _ = r.send(());
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
        self.flush();
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
                if self.link.is_none() || !self.tools.contains_key(&tool_call_id) {
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
                self.flush();
                if let Some(l) = &self.link {
                    l.send(&json!({"type": "shutdown"}));
                }
            }
            Cmd::Terminate => {
                if let Some(l) = &self.link {
                    l.terminate();
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
            let row = RowWrite {
                source_key: format!("u:{sid}"),
                role: "user",
                text: sub.text.clone(),
                thinking: String::new(),
                tool_name: None,
                tool_call_id: None,
                status: "saved",
                timestamp: now(),
                details: json!({"submissionId": sid, "mode": sub.mode}),
                entry_id: None,
                parent_id: None,
            };
            self.upsert_row(&row);
            self.emit_rows(std::slice::from_ref(&row.source_key));
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
                    let text = str_of(&delta, "text").unwrap_or_default();
                    match str_of(&delta, "type").as_deref() {
                        Some("text_delta") => m.text.push_str(&text),
                        Some("thinking_delta") => m.thinking.push_str(&text),
                        _ => {}
                    }
                    self.dirty.insert(key);
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
                    details: json!({"toolArgs": session_log::cut(&args, 4096), "stoppable": stoppable}),
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
            _ => {}
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
                    ..Default::default()
                },
            };
            self.record(Entry::Message(entry));
            return;
        }
        let key = self.msg_key(&mid);
        let live = self.stream.remove(&key);
        self.dirty.remove(&key);
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
        if self.dirty.is_empty() {
            return;
        }
        let keys: Vec<String> = std::mem::take(&mut self.dirty).into_iter().collect();
        for k in &keys {
            if let Some(m) = self.stream.get(k) {
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
        }
        // One `message` event per flush.
        self.emit_rows(&keys);
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
            Pending::Prompt { submission_id } => {
                self.clear_timer(|t| *t == Timer::Ack(id.clone()));
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
            crate::runtime::ChatRuntime {
                sh: self.sh.clone(),
            }
            .retire_idle_over_cap();
        }
    }
}
