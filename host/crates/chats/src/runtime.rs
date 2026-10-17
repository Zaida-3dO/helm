//! The chat runtime: one shared service holding a map of per-chat actors, plus the
//! seam that starts worker processes.
//!
//! - [`ChatRuntime`] is registered as a service; ops reach it through `ctx.services`.
//! - Each chat with work gets one actor task ([`crate::actor`]) owning its worker, its streaming
//!   state and its timers. Commands reach it over a 64-slot channel; a full channel
//!   refuses the send with the draft kept.
//! - [`Spawner`] starts a worker. [`ProcessSpawner`] is the real one (stage, resolve Node, write
//!   `launch.json`, spawn inside a Job Object); tests use an in-process fake.

use crate::actor::{self, Cmd};
use crate::clock::Clock;
use crate::send_behavior;
use crate::types::*;
use helm_core::db::Database;
use helm_ipc::{ApiError, EventSink};
use helm_workers::launch::{claude_launch, test_claude, ClaudeRuntime, Launch, LaunchSendBehavior};
use helm_workers::node::{resolve_node, NodeLookup};
use helm_workers::{stage, SpawnSpec, WorkerOutput, MAX_RECORD_BYTES};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// Timings and caps. Tests shorten them.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Host backstop for a prompt answer; the worker answers `uncertain` at 30 s itself.
    pub ack: Duration,
    /// After a Stop, a worker that has not settled is closed.
    pub stop: Duration,
    /// A model or effort change not acknowledged in time fails the run.
    pub model_ack: Duration,
    /// The streaming flush tick.
    pub flush: Duration,
    /// Wait-limit retry when the worker answers `waiting`.
    pub wait_retry: Duration,
    pub max_active: usize,
    pub max_idle_workers: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            ack: Duration::from_secs(35),
            stop: Duration::from_secs(60),
            model_ack: Duration::from_secs(10),
            flush: Duration::from_millis(80),
            wait_retry: Duration::from_secs(1),
            max_active: 100,
            max_idle_workers: 20,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// The profile folder (sessions, artifacts, workers live under it).
    pub profile_dir: PathBuf,
    pub limits: Limits,
}

impl RuntimeConfig {
    pub fn sessions(&self) -> PathBuf {
        self.profile_dir.join("sessions")
    }
    pub fn artifacts(&self) -> PathBuf {
        self.profile_dir.join("artifacts")
    }
}

/// What a worker is started with.
#[derive(Clone, Debug)]
pub struct SpawnRequest {
    pub chat_id: String,
    pub cwd: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub resume_session_id: Option<String>,
    pub send_behavior: SendBehavior,
}

/// The host's end of one worker.
pub trait WorkerLink: Send + Sync {
    fn send(&self, command: &Value) -> bool;
    fn terminate(&self);
}

pub type Spawned = (Arc<dyn WorkerLink>, mpsc::Receiver<WorkerOutput>);

pub trait Spawner: Send + Sync {
    fn spawn(&self, req: &SpawnRequest) -> Result<Spawned, String>;
}

struct ProcessLink(helm_workers::WorkerProcess);
impl WorkerLink for ProcessLink {
    fn send(&self, command: &Value) -> bool {
        self.0.send(command)
    }
    fn terminate(&self) {
        self.0.terminate()
    }
}

/// The real spawner: stage `worker/src`, resolve Node on the worker's PATH, write `launch.json`,
/// and start `node <main.ts> <launch.json>` in a Job Object.
pub struct ProcessSpawner {
    profile_dir: PathBuf,
    /// Paths handed to Node: the profile without Windows' verbatim prefix.
    data_root: String,
    debug: bool,
    worker_source: Option<PathBuf>,
    staged: Mutex<Option<PathBuf>>,
}

impl ProcessSpawner {
    pub fn new(
        profile_dir: PathBuf,
        data_root: String,
        debug: bool,
        worker_source: Option<PathBuf>,
    ) -> Self {
        Self {
            profile_dir,
            data_root,
            debug,
            worker_source,
            staged: Mutex::new(None),
        }
    }

    fn staged_main(&self) -> Result<PathBuf, String> {
        let mut staged = self.staged.lock().unwrap();
        if let Some(p) = staged.as_ref().filter(|p| p.is_file()) {
            return Ok(p.clone());
        }
        let source = self
            .worker_source
            .clone()
            .ok_or("Helm could not find its worker files.")?;
        let main = stage::stage_worker(&source, &PathBuf::from(&self.data_root).join("runtime"))
            .map_err(|e| format!("Helm could not prepare its worker: {e}"))?;
        *staged = Some(main.clone());
        Ok(main)
    }
}

impl Spawner for ProcessSpawner {
    fn spawn(&self, req: &SpawnRequest) -> Result<Spawned, String> {
        let main = self.staged_main()?;
        let env = helm_workers::env::worker_environment(&req.chat_id, &self.data_root);
        let node = resolve_node(&NodeLookup::for_worker(&env)).map_err(|e| e.to_string())?;
        let (runtime, problem) = ClaudeRuntime::load(&self.profile_dir);
        if let Some(p) = problem {
            log::warn!("{p}; using the defaults");
        }
        let test = test_claude(
            self.debug,
            std::env::var("HELM_TEST_CLAUDE").ok().as_deref(),
        );
        let launch = Launch {
            chat_id: req.chat_id.clone(),
            harness: "claude",
            cwd: req.cwd.clone(),
            claude: claude_launch(
                &runtime,
                test.as_ref(),
                req.model.clone(),
                req.effort.clone(),
                req.resume_session_id.clone(),
            ),
            send_behavior: LaunchSendBehavior {
                long_commands: serde_json::to_value(req.send_behavior.long_commands)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "off".into()),
                opt_in: send_behavior::opt_in(&req.send_behavior),
            },
            limits: None,
        };
        let worker_dir = PathBuf::from(&self.data_root)
            .join("workers")
            .join(&req.chat_id);
        let launch_path = launch
            .write(&worker_dir)
            .map_err(|e| format!("Helm could not write the launch file: {e}"))?;
        let (proc_, rx) = helm_workers::spawn(SpawnSpec {
            node: node.path,
            script: main,
            launch: launch_path,
            cwd: PathBuf::from(&req.cwd),
            env: env.into_pairs(),
            max_record_bytes: MAX_RECORD_BYTES,
        })
        .map_err(|e| format!("Helm could not start the worker: {e}"))?;
        Ok((Arc::new(ProcessLink(proc_)), rx))
    }
}

/// What other threads can see of an actor without asking it.
pub(crate) struct Live {
    pub thread: ThreadState,
    pub has_worker: bool,
    pub last_used: Instant,
}

#[derive(Clone)]
pub(crate) struct ActorHandle {
    pub tx: mpsc::Sender<Cmd>,
    pub live: Arc<Mutex<Live>>,
}

pub(crate) struct Shared {
    pub db: Arc<Database>,
    pub clock: Arc<Clock>,
    pub events: Arc<dyn EventSink>,
    pub spawner: Arc<dyn Spawner>,
    pub cfg: RuntimeConfig,
    pub handle: tokio::runtime::Handle,
    pub actors: Mutex<HashMap<String, ActorHandle>>,
    pub shutting_down: AtomicBool,
}

/// The runtime service.
#[derive(Clone)]
pub struct ChatRuntime {
    pub(crate) sh: Arc<Shared>,
}

impl ChatRuntime {
    pub fn new(
        db: Arc<Database>,
        clock: Arc<Clock>,
        events: Arc<dyn EventSink>,
        spawner: Arc<dyn Spawner>,
        cfg: RuntimeConfig,
        handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            sh: Arc::new(Shared {
                db,
                clock,
                events,
                spawner,
                cfg,
                handle,
                actors: Mutex::new(HashMap::new()),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    pub fn db(&self) -> &Arc<Database> {
        &self.sh.db
    }
    pub fn clock(&self) -> &Arc<Clock> {
        &self.sh.clock
    }
    pub fn limits(&self) -> &Limits {
        &self.sh.cfg.limits
    }
    pub fn handle(&self) -> &tokio::runtime::Handle {
        &self.sh.handle
    }

    /// The chat's actor, started when missing.
    pub(crate) fn actor(&self, chat_id: &str) -> ActorHandle {
        let mut map = self.sh.actors.lock().unwrap();
        if let Some(h) = map.get(chat_id) {
            if !h.tx.is_closed() {
                return h.clone();
            }
        }
        let h = actor::start(self.sh.clone(), chat_id.to_owned());
        map.insert(chat_id.to_owned(), h.clone());
        h
    }

    /// Hand a command to the chat's actor. A full channel is a refusal ("your message is saved").
    pub(crate) fn command(&self, chat_id: &str, cmd: Cmd) -> Result<(), ApiError> {
        if self.sh.shutting_down.load(Ordering::SeqCst) {
            return Err(ApiError::new(
                "refused",
                "Helm is shutting down; your message is saved.",
            ));
        }
        self.actor(chat_id).tx.try_send(cmd).map_err(|_| {
            ApiError::new(
                "busy",
                "Helm is busy with this chat; your message is saved, send it again in a moment.",
            )
        })
    }

    /// The live thread state, if the chat has an actor.
    pub fn live_thread(&self, chat_id: &str) -> Option<ThreadState> {
        let map = self.sh.actors.lock().unwrap();
        map.get(chat_id)
            .map(|h| h.live.lock().unwrap().thread.clone())
    }

    /// Ask the actor something and wait for the answer (from a blocking op or async code).
    pub(crate) async fn ask<T>(
        &self,
        chat_id: &str,
        make: impl FnOnce(oneshot::Sender<T>) -> Cmd,
        timeout: Duration,
    ) -> Result<T, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.command(chat_id, make(tx))?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => Err(ApiError::internal("the chat stopped before answering")),
            Err(_) => Err(ApiError::new("timeout", "The chat did not answer in time.")),
        }
    }

    /// Retire least-recently-used idle workers beyond the cap.
    pub(crate) fn retire_idle_over_cap(&self) {
        let cap = self.sh.cfg.limits.max_idle_workers;
        let mut idle: Vec<(Instant, ActorHandle)> = {
            let map = self.sh.actors.lock().unwrap();
            map.values()
                .filter_map(|h| {
                    let live = h.live.lock().unwrap();
                    (live.has_worker && !live.thread.status.is_busy())
                        .then(|| (live.last_used, h.clone()))
                })
                .collect()
        };
        if idle.len() <= cap {
            return;
        }
        idle.sort_by_key(|(t, _)| *t);
        let excess = idle.len() - cap;
        for (_, h) in idle.into_iter().take(excess) {
            let _ = h.tx.try_send(Cmd::Retire);
        }
    }

    /// Startup: send each chat owed a restart continuation its one fixed prompt.
    pub fn start_continuations(&self) {
        let ids: Vec<String> = self
            .sh
            .db
            .snapshot(|tx| {
                let mut st = tx.prepare(
                    "SELECT r.chat_id FROM restart_continuations r JOIN chats c ON c.id = r.chat_id
                     WHERE c.archived = 0 ORDER BY r.created_at",
                )?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
            })
            .unwrap_or_default();
        for chat_id in ids {
            match crate::submit::continuation(self, &chat_id) {
                Ok(()) => log::info!("chat {chat_id}: restart continuation sent"),
                Err(e) => log::error!("chat {chat_id}: restart continuation not sent: {e}"),
            }
        }
    }

    /// Chats with a turn in progress, for the quit confirmation.
    pub fn running_titles(&self) -> Vec<String> {
        self.sh
            .db
            .snapshot(|tx| {
                let mut st = tx.prepare(
                    "SELECT c.title, COALESCE(p.path, '') FROM chats c JOIN projects p ON p.id = c.project_id
                     WHERE c.status IN ('starting', 'running', 'stopping') ORDER BY c.updated_at DESC",
                )?;
                let rows = st.query_map([], |r| {
                    Ok(format!("{} ({})", r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
            })
            .unwrap_or_default()
    }

    /// Quit: record continuations for busy chats the user did not stop, send
    /// every worker `shutdown`, and wait for them to exit. The caller bounds the wait (8 s); the
    /// Job Objects end anything left when the process exits.
    pub async fn drain(&self) {
        self.sh.shutting_down.store(true, Ordering::SeqCst);
        let now = helm_core::now_ms();
        let _ = self.sh.db.write(|tx| {
            let ids: Vec<String> = {
                let mut st = tx.prepare(
                    "SELECT id FROM chats WHERE status IN ('starting', 'running') AND archived = 0",
                )?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for id in ids {
                crate::schema::record_continuation(tx, &id, now)?;
            }
            Ok::<_, rusqlite::Error>(())
        });
        let handles: Vec<ActorHandle> = self.sh.actors.lock().unwrap().values().cloned().collect();
        let mut waits = Vec::new();
        for h in handles {
            let (tx, rx) = oneshot::channel();
            if h.tx.send(Cmd::Shutdown { reply: tx }).await.is_ok() {
                waits.push(rx);
            }
        }
        for w in waits {
            let _ = w.await;
        }
    }

    /// Terminate every worker now (after the drain timed out).
    pub fn terminate_all(&self) {
        let handles: Vec<ActorHandle> = self.sh.actors.lock().unwrap().values().cloned().collect();
        for h in handles {
            let _ = h.tx.try_send(Cmd::Terminate);
        }
    }
}

/// The quit sequence's view of the runtime.
pub struct RuntimeDrain(pub ChatRuntime);

impl helm_core::lifecycle::WorkerDrain for RuntimeDrain {
    fn running_work(&self) -> Vec<String> {
        self.0.running_titles()
    }
    fn drain(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let rt = self.0.clone();
            struct Guard(ChatRuntime);
            impl Drop for Guard {
                fn drop(&mut self) {
                    // Runs when the quit sequence's 8 s bound cancels the drain, too.
                    self.0.terminate_all();
                }
            }
            let _g = Guard(rt.clone());
            rt.drain().await;
        })
    }
}
