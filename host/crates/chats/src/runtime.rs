//! The chat runtime: one shared service holding a map of per-chat actors, plus the
//! seam that starts worker processes.
//!
//! - [`ChatRuntime`] is registered as a service; ops reach it through `ctx.services`.
//! - Each chat with work gets one actor task ([`crate::actor`]) owning its worker, its streaming
//!   state and its timers. Commands reach it over a 64-slot channel: best-effort
//!   sends never wait and are dropped when it is full; a "send soon" (a saved message handed to the
//!   chat) retries for up to [`Limits::send_soon`]; the last slot is kept for interrupts (Stop,
//!   tool stop, terminate), which never wait. Other commands are refused with the draft kept.
//! - [`Spawner`] starts a worker. [`ProcessSpawner`] is the real one (stage, resolve Node, write
//!   `launch.json`, spawn inside a Job Object); tests use an in-process fake.

use crate::actor::{self, Cmd};
use crate::clock::Clock;
use crate::send_behavior;
use crate::types::*;
use helm_core::db::Database;
use helm_ipc::{ApiError, EventSink};
use helm_workers::contrib::{self, LaunchContributors, LaunchCtx};
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
    /// The maintenance tick: stall checks and snooze expiry.
    pub tick: Duration,
    /// One unit of the `stallMinutes` setting (a minute; tests shorten it).
    pub stall_unit: Duration,
    /// How long a "send soon" keeps retrying a full actor channel (60 s).
    pub send_soon: Duration,
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
            tick: Duration::from_secs(30),
            stall_unit: Duration::from_secs(60),
            send_soon: Duration::from_secs(60),
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
    /// Resume `resume_session_id` as a new session (`--fork-session`): a fork's first launch.
    pub fork_session: bool,
    pub send_behavior: SendBehavior,
    /// Host tools for this launch; `None` when the host-tool seam is not attached (tests).
    pub host: Option<HostLaunch>,
}

/// The host-tool and MCP part of one launch.
#[derive(Clone, Debug, PartialEq)]
pub struct HostLaunch {
    /// `launch.json` `hostTools[]`.
    pub tools: Value,
    /// Native MCP servers off: `--strict-mcp-config`.
    pub strict_mcp: bool,
    pub disabled_mcp_servers: Vec<String>,
}

/// The host's end of one worker.
pub trait WorkerLink: Send + Sync {
    fn send(&self, command: &Value) -> bool;
    fn terminate(&self);
}

pub type Spawned = (Arc<dyn WorkerLink>, mpsc::Receiver<WorkerOutput>);

pub trait Spawner: Send + Sync {
    fn spawn(&self, req: &SpawnRequest) -> Result<Spawned, String>;

    /// Run the titles helper on one job and answer the title (blocking; [`crate::titles`]).
    fn run_title(&self, _job: &Value) -> Result<String, String> {
        Err("titles are not available".into())
    }

    /// Run the autocomplete helper on one job and answer the completion ([`crate::autocomplete`]).
    fn run_autocomplete(&self, _job: &Value) -> Result<String, String> {
        Err("autocomplete is not available".into())
    }
}

/// The title-job limits: at most 2 helper runs at once, 8 jobs in all.
pub(crate) struct TitleQueue {
    pub slots: tokio::sync::Semaphore,
    pub jobs: std::sync::atomic::AtomicUsize,
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
    contributors: LaunchContributors,
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
            contributors: LaunchContributors::new(),
        }
    }

    /// The launch contributors every worker launch consults.
    pub fn with_contributors(mut self, contributors: LaunchContributors) -> Self {
        self.contributors = contributors;
        self
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
        let mut env = helm_workers::env::worker_environment(&req.chat_id, &self.data_root);
        let extras = self.contributors.collect(&LaunchCtx {
            chat_id: &req.chat_id,
            cwd: &req.cwd,
        });
        contrib::apply_env(&mut env, &extras);
        let node = resolve_node(&NodeLookup::for_worker(&env)).map_err(|e| e.to_string())?;
        let (runtime, problem) = ClaudeRuntime::load(&self.profile_dir);
        if let Some(p) = problem {
            log::warn!("{p}; using the defaults");
        }
        let test = test_claude(
            self.debug,
            std::env::var("HELM_TEST_CLAUDE").ok().as_deref(),
        );
        let mut claude = claude_launch(
            &runtime,
            test.as_ref(),
            req.model.clone(),
            req.effort.clone(),
            req.resume_session_id.clone(),
        );
        claude.append_system_prompt = contrib::system_prompt(&extras);
        claude.fork_session = req.fork_session;
        let worker_dir = PathBuf::from(&self.data_root)
            .join("workers")
            .join(&req.chat_id);
        let (host_tools, chat_tools_directory) = match &req.host {
            Some(h) => {
                claude.strict_mcp_config = h.strict_mcp;
                claude.disabled_mcp_servers = h.disabled_mcp_servers.clone();
                let dir = worker_dir.join("tools");
                helm_core::privfs::create_private_dir_all(&dir)
                    .map_err(|e| format!("Helm could not make the host-tool folder: {e}"))?;
                (
                    Some(h.tools.clone()),
                    Some(dir.to_string_lossy().into_owned()),
                )
            }
            None => (None, None),
        };
        let launch = Launch {
            chat_id: req.chat_id.clone(),
            harness: "claude",
            cwd: req.cwd.clone(),
            claude,
            send_behavior: LaunchSendBehavior {
                long_commands: serde_json::to_value(req.send_behavior.long_commands)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "off".into()),
                opt_in: send_behavior::opt_in(&req.send_behavior),
            },
            limits: None,
            host_tools,
            chat_tools_directory,
        };
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

    fn run_title(&self, job: &Value) -> Result<String, String> {
        let out = self.run_helper("titles", job, crate::titles::HOST_TIMEOUT)?;
        match (
            out.get("title").and_then(Value::as_str),
            out.get("error").and_then(Value::as_str),
        ) {
            (Some(t), _) => Ok(t.to_owned()),
            (_, Some(e)) => Err(e.to_owned()),
            _ => Err("the helper gave no title".into()),
        }
    }

    fn run_autocomplete(&self, job: &Value) -> Result<String, String> {
        let out = self.run_helper("autocomplete", job, crate::autocomplete::DEADLINE)?;
        match (
            out.get("completion").and_then(Value::as_str),
            out.get("error").and_then(Value::as_str),
        ) {
            (Some(c), _) => Ok(c.to_owned()),
            (_, Some(e)) => Err(e.to_owned()),
            _ => Err("the helper gave no completion".into()),
        }
    }
}

impl ProcessSpawner {
    /// Run `helpers/<name>/src/main.ts` (beside the worker's source: `<repo>/worker/src` →
    /// `<repo>/helpers`) on one job and answer its JSON line.
    fn run_helper(&self, name: &str, job: &Value, timeout: Duration) -> Result<Value, String> {
        let script = self
            .worker_source
            .as_ref()
            .and_then(|w| {
                w.parent()?
                    .parent()
                    .map(|r| r.join(format!("helpers/{name}/src/main.ts")))
            })
            .filter(|p| p.is_file())
            .ok_or_else(|| format!("Helm could not find its {name} helper."))?;
        let env = helm_workers::env::base_environment();
        let node = resolve_node(&NodeLookup::for_worker(&env)).map_err(|e| e.to_string())?;
        let mut job = job.clone();
        let (runtime, _) = ClaudeRuntime::load(&self.profile_dir);
        if let Some(t) = test_claude(
            self.debug,
            std::env::var("HELM_TEST_CLAUDE").ok().as_deref(),
        ) {
            job["command"] = Value::String(t.command);
            job["commandArgs"] = serde_json::json!(t.args);
        } else if !runtime.executable.is_empty() {
            job["command"] = Value::String(runtime.executable);
        }
        crate::titles::run_helper(&node.path, &script, env.into_pairs(), &job, timeout)
    }
}

/// What other threads can see of an actor without asking it.
pub(crate) struct Live {
    pub thread: ThreadState,
    pub has_worker: bool,
    pub last_used: Instant,
    /// Running with no worker output for the stall limit.
    pub stalled: bool,
    /// The skills the worker's `get_state` reported.
    pub skills: Vec<SkillInfo>,
    /// The panel-context generation: bumped on each worker launch and compaction.
    pub context_gen: u64,
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
    /// The last published attention summary ([`crate::attention::publish`]).
    pub attention: Mutex<Option<AttentionSummary>>,
    pub titles: TitleQueue,
    pub autocomplete: Mutex<crate::autocomplete::AutocompleteGate>,
    /// Host tools and the worker-message registry, attached once the app's services exist.
    pub host: std::sync::OnceLock<crate::hostcalls::HostSeam>,
    /// The run-slot queue's hold and pump wake ([`crate::runslots`]).
    pub run_slots: crate::runslots::Slots,
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
        let rt = Self {
            sh: Arc::new(Shared {
                db,
                clock,
                events,
                spawner,
                cfg,
                handle,
                actors: Mutex::new(HashMap::new()),
                shutting_down: AtomicBool::new(false),
                attention: Mutex::new(None),
                titles: TitleQueue {
                    slots: tokio::sync::Semaphore::new(crate::titles::MAX_CONCURRENT),
                    jobs: std::sync::atomic::AtomicUsize::new(0),
                },
                autocomplete: Mutex::new(Default::default()),
                host: std::sync::OnceLock::new(),
                run_slots: Default::default(),
            }),
        };
        rt.start_tick();
        crate::runslots::start(&rt);
        rt
    }

    /// Attach host tools and the worker-message registry (once, after the app's services are
    /// built). Workers launched afterwards list the host tools.
    pub fn attach_host(&self, seam: crate::hostcalls::HostSeam) {
        if self.sh.host.set(seam).is_err() {
            log::warn!("host tools attached twice; keeping the first");
        }
    }

    /// The maintenance tick (every `limits.tick`): ask busy actors to check for a stall, and
    /// republish attention so expired snoozes show again. It stops when the runtime is dropped.
    fn start_tick(&self) {
        let weak = Arc::downgrade(&self.sh);
        let every = self.sh.cfg.limits.tick;
        self.sh.handle.spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(sh) = weak.upgrade() else { break };
                let minutes = sh
                    .db
                    .snapshot(|tx| Ok::<_, rusqlite::Error>(stall_minutes(tx)))
                    .unwrap_or(DEFAULT_STALL_MINUTES);
                let after = sh.cfg.limits.stall_unit * minutes;
                let busy: Vec<ActorHandle> = sh
                    .actors
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|h| h.live.lock().unwrap().thread.status.is_busy())
                    .cloned()
                    .collect();
                for h in busy {
                    let _ = enqueue(&h.tx, Cmd::StallCheck { after });
                }
                crate::attention::publish(&sh);
                ChatRuntime { sh: sh.clone() }.apply_send_behavior();
            }
        });
    }

    /// Hand every chat the stored send behaviour. An actor whose worker was
    /// launched with another long-command setting relaunches it (idle now, busy at the next idle
    /// point). The maintenance tick calls this, so a change applies within one tick.
    pub fn apply_send_behavior(&self) {
        let sb = self
            .sh
            .db
            .snapshot(|tx| {
                Ok::<_, rusqlite::Error>(send_behavior::resolve(
                    helm_core::prefs::read(tx, "sendBehavior").as_ref(),
                ))
            })
            .unwrap_or(send_behavior::HOME_DEFAULTS);
        let handles: Vec<ActorHandle> = self.sh.actors.lock().unwrap().values().cloned().collect();
        for h in handles {
            let _ = enqueue(&h.tx, Cmd::SendBehavior(sb));
        }
    }

    /// `model_catalogue {refresh: true}`: ask a chat with a live worker to re-read `get_state`.
    /// Answers false when no worker is running (the cached catalogue stands).
    pub async fn refresh_catalogue(&self) -> bool {
        let live: Option<String> = self
            .sh
            .actors
            .lock()
            .unwrap()
            .iter()
            .find(|(_, h)| h.live.lock().unwrap().has_worker)
            .map(|(id, _)| id.clone());
        let Some(chat) = live else { return false };
        self.ask(
            &chat,
            |reply| Cmd::RefreshCatalogue { reply },
            Duration::from_secs(10),
        )
        .await
        .unwrap_or(false)
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

    /// Hand a command to the chat's actor. On a full channel a "send soon" is retried
    /// for up to [`Limits::send_soon`] and logged if it never fits; anything else is a refusal
    /// ("your message is saved"). Use [`Self::command_soon`] to act when a send soon gives up.
    pub(crate) fn command(&self, chat_id: &str, cmd: Cmd) -> Result<(), ApiError> {
        self.command_soon(chat_id, cmd, |_| {})
    }

    /// [`Self::command`], calling `gave_up` with the command if a send soon never fits.
    pub(crate) fn command_soon(
        &self,
        chat_id: &str,
        cmd: Cmd,
        gave_up: impl FnOnce(Cmd) + Send + 'static,
    ) -> Result<(), ApiError> {
        if self.sh.shutting_down.load(Ordering::SeqCst) {
            return Err(ApiError::new(
                "refused",
                "Helm is shutting down; your message is saved.",
            ));
        }
        let tx = self.actor(chat_id).tx;
        match enqueue(&tx, cmd) {
            Ok(()) => Ok(()),
            Err(cmd) if lane(&cmd) == Lane::Soon => {
                let chat = chat_id.to_owned();
                let within = self.sh.cfg.limits.send_soon;
                self.sh.handle.spawn(async move {
                    if let Err(cmd) = send_soon(&tx, cmd, within).await {
                        log::warn!("chat {chat}: its command channel stayed full; giving up");
                        gave_up(cmd);
                    }
                });
                Ok(())
            }
            Err(_) => Err(ApiError::new(
                "busy",
                "Helm is busy with this chat; your message is saved, send it again in a moment.",
            )),
        }
    }

    /// The skills the chat's worker reported; empty before its first launch.
    pub fn skills(&self, chat_id: &str) -> Vec<SkillInfo> {
        let map = self.sh.actors.lock().unwrap();
        map.get(chat_id)
            .map(|h| h.live.lock().unwrap().skills.clone())
            .unwrap_or_default()
    }

    /// The chat's panel-context generation (0 before its first worker launch).
    pub fn context_gen(&self, chat_id: &str) -> crate::seams::ContextGen {
        let map = self.sh.actors.lock().unwrap();
        crate::seams::ContextGen(
            map.get(chat_id)
                .map(|h| h.live.lock().unwrap().context_gen)
                .unwrap_or(0),
        )
    }

    /// Retire the chat's worker if it has an actor (archive, delete). Never starts an actor.
    pub fn retire(&self, chat_id: &str) {
        let h = self.sh.actors.lock().unwrap().get(chat_id).cloned();
        if let Some(h) = h {
            let _ = enqueue(&h.tx, Cmd::Retire);
        }
    }

    /// Recompute the attention summary and emit it if it changed.
    pub fn publish_attention(&self) {
        crate::attention::publish(&self.sh);
    }

    /// The current attention summary (`attention_get`).
    pub fn attention(&self) -> rusqlite::Result<AttentionSummary> {
        crate::attention::current(&self.sh)
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
            let _ = enqueue(&h.tx, Cmd::Retire);
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
            let _ = enqueue(&h.tx, Cmd::Terminate);
        }
    }
}

/// How a command uses the actor's 64-slot channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    /// May take the last slot, which every other lane leaves free; never waits.
    Interrupt,
    /// A saved message handed to the chat: retried while the channel is full.
    Soon,
    /// Everything else: never waits; dropped (best effort) or refused when full.
    Other,
}

pub(crate) fn lane(cmd: &Cmd) -> Lane {
    match cmd {
        Cmd::Stop { .. } | Cmd::ToolStop { .. } | Cmd::Terminate => Lane::Interrupt,
        Cmd::Dispatch(_) | Cmd::Kick | Cmd::ApplyEdit => Lane::Soon,
        _ => Lane::Other,
    }
}

/// Put `cmd` on the channel without waiting, keeping the last slot for interrupts. Hands the
/// command back when it does not fit.
pub(crate) fn enqueue(tx: &mpsc::Sender<Cmd>, cmd: Cmd) -> Result<(), Cmd> {
    let Ok(permit) = tx.try_reserve() else {
        return Err(cmd);
    };
    if lane(&cmd) != Lane::Interrupt && tx.capacity() == 0 {
        // That was the reserved slot: give it back.
        drop(permit);
        return Err(cmd);
    }
    permit.send(cmd);
    Ok(())
}

/// Retry [`enqueue`] until it fits or `within` has passed (then the command comes back).
pub(crate) async fn send_soon(
    tx: &mpsc::Sender<Cmd>,
    mut cmd: Cmd,
    within: Duration,
) -> Result<(), Cmd> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match enqueue(tx, cmd) {
            Ok(()) => return Ok(()),
            Err(c) if tx.is_closed() || tokio::time::Instant::now() >= deadline => return Err(c),
            Err(c) => cmd = c,
        }
        tokio::time::sleep(SEND_SOON_POLL).await;
    }
}

const SEND_SOON_POLL: Duration = Duration::from_millis(25);

pub const DEFAULT_STALL_MINUTES: u32 = 5;

/// The `stallMinutes` setting: 1 to 1440, else the default 5.
pub fn stall_minutes(tx: &rusqlite::Transaction<'_>) -> u32 {
    helm_core::prefs::read(tx, "stallMinutes")
        .and_then(|v| v.as_u64())
        .filter(|n| (1..=1440).contains(n))
        .map(|n| n as u32)
        .unwrap_or(DEFAULT_STALL_MINUTES)
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
