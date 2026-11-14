//! The local model service: the servers' settings and keys, the catalogue, and what happens
//! before and after each turn of a local chat.
//!
//! **Nothing here runs on its own.** Helm reaches a server only when asked: a catalogue refresh
//! (the picker or Settings opened), a Test, a local chat's send, an unload of local models, or
//! that chat's queue check — a server that answers one request at a time is asked about its
//! waiting requests only while a request of ours is open against it, and not more than every few
//! seconds. There is no start-up probe and no timer, with one exception: when Helm starts with
//! local models unloaded (the lock below is stored), it unloads them once more, because a restart
//! task may have relaunched a server at boot ([`LocalService::resume`]). A Strata server whose
//! settings allow it is *launched* at one of those same moments, and only when a check finds it
//! not answering — see [`starter`]. Beyond that, the memory check unloads idle models when a model
//! does not fit, and nothing else unloads or stops a server but the user's unload.
//!
//! **The graphics-card lock** ([`crate::lock`]). While it is on, every load path refuses with one
//! sentence: a worker's start ([`LocalModels::server`]), every turn ([`LocalModels::prepare`]),
//! the launch of a Strata that is down, a refresh or Test (they answer from the cache and contact
//! nobody), the queue check and the keep-alive after a turn. Waits already under way (a server
//! starting, memory coming free) give up as soon as the lock lands. Unloading
//! ([`LocalService::unload`]) sets the lock *first*, then pauses the restart task, stops the busy
//! local chats and unloads every configured server, Strata's process included.
//!
//! **Before a turn** ([`LocalService::prepare`]): if the model is already loaded, go. If not, the
//! free graphics memory is read; when the model will not fit, idle models on the *other* servers
//! are unloaded (idle meaning no Helm chat on that model is mid-turn); when only models that are
//! mid-reply stand in the way, no turn starts and the message stays in the draft. Ollama also gets
//! a Helm variant of the model with the chosen context window, loaded with the chosen keep-alive;
//! nothing is changed on Ollama for any other use.

use crate::games::{self, Action, ProcessNames, Watch};
use crate::gpu::Gpu;
use crate::lock::{self as gpulock, LockState};
use crate::net::{self, Conn, Http};
use crate::servers::{self as srv, CallError};
use crate::settings::{self, LocalSettings, Server, ToolsSetup};
use crate::starter::{Stopped, StrataStarter};
use crate::task::RestartTask;
use crate::types::{LocalModelsState, LockKind, LockView, Range, ServerView, TestResult};
use helm_chats::local::{LocalModels, LocalServer, LocalTurn, QueueInfo};
use helm_chats::types::{LocalModelInfo, LocalServerKind};
use helm_core::credentials::CredentialStore;
use helm_core::db::Database;
use helm_ipc::{AppEvent, EventSink};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, TryLockError};
use std::time::{Duration, Instant};

/// The test override: in debug builds only, `HELM_TEST_LOCAL_SERVERS` is `off` (no server is ever
/// contacted) or a loopback address every configured server uses instead.
pub const TEST_ENV: &str = "HELM_TEST_LOCAL_SERVERS";

/// A key a server that wants none is given: Claude Code needs some token, and it must never fall
/// back to the user's own sign-in.
pub const KEYLESS: &str = "local";

const GB: u64 = 1024 * 1024 * 1024;

/// The event that tells the page the lock, or an unload's progress, changed.
pub const LOCK_EVENT: &str = "local_lock";

/// What an unload says when local models are already unloaded.
pub const ALREADY_UNLOADED: &str = "Local models are already unloaded.";

/// How an unload stops the local chats that are busy; the app fills it in once chats can be
/// stopped.
pub trait ChatControl: Send + Sync {
    /// Stop the chat and, once it has stopped, add `note` to its transcript. Returns at once.
    fn stop(&self, chat_id: &str, note: &str);
    /// How long a stop may take before the chat's worker is closed.
    fn stop_limit(&self) -> Duration;
}

/// What the last unload or turn back on did, and how many are still running.
#[derive(Default)]
struct Progress {
    pending: u32,
    report: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Override {
    Off,
    Url(String),
}

/// The override a value of [`TEST_ENV`] stands for. Release builds (`debug` false) ignore it.
pub fn override_from(raw: Option<&str>, debug: bool) -> Option<Override> {
    let raw = raw?.trim();
    if !debug || raw.is_empty() {
        return None;
    }
    if raw.eq_ignore_ascii_case("off") {
        return Some(Override::Off);
    }
    match settings::check_base_url(raw) {
        Ok(u) => Some(Override::Url(u)),
        Err(e) => {
            log::error!(
                "{TEST_ENV} is not a loopback address ({e}); no local server will be contacted"
            );
            Some(Override::Off)
        }
    }
}

/// The override in effect.
pub fn test_override() -> Option<Override> {
    override_from(
        std::env::var(TEST_ENV).ok().as_deref(),
        cfg!(debug_assertions),
    )
}

/// The address to use for `s`: the override, or the saved address checked again.
pub fn base_for(s: &Server, ov: Option<Override>) -> Result<String, String> {
    match ov {
        Some(Override::Off) => Ok(net::OFF.to_owned()),
        Some(Override::Url(u)) => Ok(u),
        None => settings::check_base_url(&s.base_url),
    }
}

/// The address to use for `s` now.
pub fn effective_base(s: &Server) -> Result<String, String> {
    base_for(s, test_override())
}

// ---- what was last seen ---------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Seen {
    servers: BTreeMap<String, SeenServer>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct SeenServer {
    reachable: bool,
    note: Option<String>,
    models: Vec<SeenModel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SeenModel {
    model: String,
    context_tokens: Option<u64>,
    size_bytes: Option<u64>,
    loaded: Option<bool>,
}

/// How long the service waits for memory to come free after an unload, and how often it looks;
/// and how long a started Strata gets to answer its health check, and how often it is asked.
#[derive(Clone, Copy, Debug)]
pub struct Timings {
    pub memory_wait: Duration,
    pub memory_poll: Duration,
    pub start_wait: Duration,
    pub start_poll: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            memory_wait: Duration::from_secs(8),
            memory_poll: Duration::from_millis(500),
            start_wait: Duration::from_secs(90),
            start_poll: Duration::from_secs(2),
        }
    }
}

pub struct LocalService {
    db: Arc<Database>,
    creds: CredentialStore,
    events: Arc<dyn EventSink>,
    http: Http,
    gpu: Arc<dyn Gpu>,
    timings: Timings,
    cache_file: PathBuf,
    seen: Mutex<Option<Seen>>,
    /// Memory each model took when last seen loaded, by base name.
    vram: Mutex<HashMap<String, u64>>,
    /// A model's own context length by server, name and size, so `show` is asked once.
    native: Mutex<HashMap<String, Option<u64>>>,
    /// One model is made ready at a time, so two chats cannot both count on the same free memory.
    loading: Mutex<()>,
    /// None means this service never launches a server: a stand-in can be installed for tests.
    starter: Option<Arc<dyn StrataStarter>>,
    /// One server is started at a time; anyone else who needs it waits on this and then finds it
    /// already answering.
    starting: Mutex<()>,
    /// Whether speech-to-text is set up on this PC (dictation, or the phone's voice line). The
    /// speech reserve is kept only then; without it no reserve is kept.
    speech_in_use: Mutex<Option<SpeechProbe>>,
    /// The graphics-card lock as stored; every load path reads this copy.
    gpu_lock: Arc<Mutex<LockState>>,
    /// Held for reading by a request that loads a model or launches a server, from its lock check
    /// to the end of that request. An unload takes it once for writing after the lock is set, so
    /// no load that passed its check earlier is still on its way when the unloading starts.
    gate: Arc<RwLock<()>>,
    /// One unload or turn back on at a time.
    work: Mutex<()>,
    progress: Mutex<Progress>,
    /// Ends and starts the user's restart task. None: Helm never touches one.
    task: Option<Arc<dyn RestartTask>>,
    /// Stops busy local chats on an unload; attached once chats can be stopped.
    chats: Mutex<Option<Arc<dyn ChatControl>>>,
    /// What the game watch remembers.
    watch: Mutex<Watch>,
}

/// Tells the service whether speech-to-text is set up, so the speech reserve is worth keeping.
pub type SpeechProbe = Arc<dyn Fn() -> bool + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read(g: &RwLock<()>) -> RwLockReadGuard<'_, ()> {
    g.read().unwrap_or_else(|e| e.into_inner())
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

impl LocalService {
    pub fn new(
        db: Arc<Database>,
        creds: CredentialStore,
        events: Arc<dyn EventSink>,
        gpu: Arc<dyn Gpu>,
        cache_file: PathBuf,
    ) -> Self {
        let stored = db.snapshot(gpulock::load).unwrap_or_else(|e| {
            log::warn!("the local model lock could not be read: {e}");
            LockState::On
        });
        Self {
            db,
            creds,
            events,
            http: Http::default(),
            gpu,
            timings: Timings::default(),
            cache_file,
            seen: Mutex::new(None),
            vram: Mutex::new(HashMap::new()),
            native: Mutex::new(HashMap::new()),
            loading: Mutex::new(()),
            starter: None,
            starting: Mutex::new(()),
            speech_in_use: Mutex::new(None),
            gpu_lock: Arc::new(Mutex::new(stored)),
            gate: Arc::new(RwLock::new(())),
            work: Mutex::new(()),
            progress: Mutex::new(Progress::default()),
            task: None,
            chats: Mutex::new(None),
            watch: Mutex::new(Watch::default()),
        }
    }

    /// Say how to tell whether speech-to-text is set up. Until this is called, and whenever it
    /// answers no, the speech reserve is 0: a PC that never dictates gives up no memory for it.
    pub fn set_speech_probe(&self, probe: SpeechProbe) {
        *lock(&self.speech_in_use) = Some(probe);
    }

    /// The memory kept free for speech now: the saved reserve once speech-to-text is set up,
    /// else nothing.
    fn speech_reserve(&self, st: &LocalSettings) -> u64 {
        let probe = lock(&self.speech_in_use).clone();
        if probe.is_some_and(|p| p()) {
            u64::from(st.speech_reserve_gb) * GB
        } else {
            0
        }
    }

    pub fn with_timings(mut self, t: Timings) -> Self {
        self.timings = t;
        self
    }

    /// Lets this service launch a Strata server that its settings allow and a check found down.
    /// Without this, a down server simply stays down.
    pub fn with_starter(mut self, starter: Arc<dyn StrataStarter>) -> Self {
        self.starter = Some(starter);
        self
    }

    /// Lets an unload end the user's restart task, and a turn back on start it.
    pub fn with_task(mut self, task: Arc<dyn RestartTask>) -> Self {
        self.task = Some(task);
        self
    }

    /// Lets an unload stop the local chats that are busy.
    pub fn attach_chats(&self, chats: Arc<dyn ChatControl>) {
        *lock(&self.chats) = Some(chats);
    }

    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    pub fn settings(&self) -> LocalSettings {
        self.db.snapshot(settings::load).unwrap_or_default()
    }

    fn emit(&self) {
        self.events.emit(AppEvent::new("local_models"));
    }

    // ---- keys -------------------------------------------------------------------------------

    fn key_name(id: &str) -> String {
        format!("local-{id}")
    }

    fn key_identity(id: &str) -> String {
        format!("local-model:{id}")
    }

    fn key_for(&self, id: &str) -> Option<String> {
        let bytes = self
            .creds
            .read(&Self::key_name(id), &Self::key_identity(id))
            .ok()?;
        String::from_utf8(bytes).ok().filter(|k| !k.is_empty())
    }

    pub fn has_key(&self, id: &str) -> bool {
        self.key_for(id).is_some()
    }

    pub fn set_key(&self, id: &str, key: &str) -> Result<(), String> {
        if !self.settings().servers.iter().any(|s| s.id == id) {
            return Err("That server is not set up.".into());
        }
        let key = key.trim();
        if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_control) {
            return Err("The key must be one line of text, up to 4096 characters.".into());
        }
        self.creds
            .write(&Self::key_name(id), &Self::key_identity(id), key.as_bytes())
            .map_err(|e| format!("The key could not be stored: {e}"))?;
        self.emit();
        Ok(())
    }

    pub fn forget_key(&self, id: &str) -> Result<(), String> {
        self.creds
            .forget(&Self::key_name(id))
            .map_err(|e| format!("The key could not be removed: {e}"))?;
        self.emit();
        Ok(())
    }

    // ---- settings view and saves --------------------------------------------------------------

    pub fn view(&self) -> LocalModelsState {
        let s = self.settings();
        let seen = self.seen();
        let range = |lo, hi, default| Range {
            min: lo,
            max: hi,
            default,
        };
        LocalModelsState {
            servers: s
                .servers
                .iter()
                .map(|x| {
                    let last = seen.servers.get(&x.id);
                    ServerView {
                        id: x.id.clone(),
                        name: x.name.clone(),
                        kind: x.kind,
                        base_url: x.base_url.clone(),
                        enabled: x.enabled,
                        model: x.model.clone(),
                        auto_start: x.auto_start,
                        folder: x.folder.clone(),
                        preset: x.preset.clone(),
                        has_key: self.has_key(&x.id),
                        reachable: last.map(|l| l.reachable),
                        note: last.and_then(|l| l.note.clone()),
                    }
                })
                .collect(),
            ollama_context: s.ollama_context,
            ollama_context_range: range(
                settings::MIN_CONTEXT,
                settings::MAX_CONTEXT,
                settings::DEFAULT_CONTEXT,
            ),
            keep_alive_minutes: s.keep_alive_minutes,
            keep_alive_range: range(
                settings::MIN_KEEP_ALIVE,
                settings::MAX_KEEP_ALIVE,
                settings::DEFAULT_KEEP_ALIVE,
            ),
            strata_need_gb: s.strata_need_gb,
            strata_need_range: range(
                settings::MIN_STRATA_GB,
                settings::MAX_STRATA_GB,
                settings::DEFAULT_STRATA_GB,
            ),
            tools: s.tools,
            compare_with: s.compare_with,
            games: s.games.clone(),
            game_lift_minutes: s.game_lift_minutes,
            game_lift_range: range(
                settings::MIN_GAME_LIFT,
                settings::MAX_GAME_LIFT,
                settings::DEFAULT_GAME_LIFT,
            ),
            restart_task: s.restart_task.clone(),
            pause_file: s.pause_file.clone(),
        }
    }

    /// Apply a save. Servers removed here lose their key; the catalogue forgets them.
    pub fn save(&self, changes: &settings::Changes) -> Result<LocalModelsState, String> {
        let before = self.settings();
        let after = self
            .db
            .write(|tx| {
                let cur = settings::load(tx)?;
                match settings::apply_changes(&cur, changes) {
                    Ok(next) => settings::store(tx, &next).map(|_| Ok(next)),
                    Err(e) => Ok(Err(e)),
                }
            })
            .map_err(|e| format!("The settings could not be saved: {e}"))??;
        for gone in before
            .servers
            .iter()
            .filter(|b| !after.servers.iter().any(|a| a.id == b.id))
        {
            let _ = self.creds.forget(&Self::key_name(&gone.id));
        }
        self.forget_unlisted(&after);
        self.emit();
        Ok(self.view())
    }

    fn forget_unlisted(&self, s: &LocalSettings) {
        let mut guard = lock(&self.seen);
        if let Some(seen) = guard.as_mut() {
            seen.servers
                .retain(|id, _| s.servers.iter().any(|x| &x.id == id));
            self.write_cache(seen);
        }
    }

    // ---- the cache of what was last seen ------------------------------------------------------

    fn seen(&self) -> Seen {
        let mut g = lock(&self.seen);
        g.get_or_insert_with(|| {
            helm_core::privfs::read_capped(&self.cache_file, 1024 * 1024)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default()
        })
        .clone()
    }

    fn write_cache(&self, seen: &Seen) {
        let Some(dir) = self.cache_file.parent() else {
            return;
        };
        let write = || -> std::io::Result<()> {
            helm_core::privfs::create_private_dir_all(dir)?;
            let bytes = serde_json::to_vec(seen).map_err(std::io::Error::other)?;
            helm_core::privfs::write_atomic(&self.cache_file, &bytes)
        };
        if let Err(e) = write() {
            log::warn!("the local model list could not be cached: {e}");
        }
    }

    fn remember(&self, results: Vec<(String, SeenServer)>) {
        let mut g = lock(&self.seen);
        let seen = g.get_or_insert_with(Seen::default);
        for (id, r) in results {
            seen.servers.insert(id, r);
        }
        self.write_cache(seen);
    }

    // ---- connections ------------------------------------------------------------------------

    fn conn(&self, s: &Server) -> Result<Conn, String> {
        Ok(Conn {
            base: effective_base(s)?,
            key: self.key_for(&s.id),
        })
    }

    /// The server's connection, or the sentence to show when it can't be reached.
    fn conn_or_say(&self, s: &Server) -> Result<Conn, String> {
        self.conn(s)
            .map_err(|e| format!("{} can't be used: {e}", s.name))
    }

    // ---- starting a server that is down -------------------------------------------------------

    /// Whether this service may launch `s`: a Strata whose settings allow it, a starter installed,
    /// no lock on the card, and no test override — with one in effect the run points at fakes and
    /// the real starter must never spawn.
    fn can_start(&self, s: &Server) -> bool {
        s.kind == LocalServerKind::Strata
            && s.auto_start
            && self.starter.is_some()
            && !self.locked()
            && test_override().is_none()
    }

    /// Ask a Strata if it is up, starting it first when it is set up for that and is not there.
    /// `note` is what a waiting turn's progress line gets: a start is slow, and silence is not
    /// the answer for the chat that is watching.
    fn strata_probe(
        &self,
        s: &Server,
        conn: &Conn,
        note: &dyn Fn(&str),
    ) -> Result<Vec<String>, String> {
        match srv::strata_loaded(&self.http, conn) {
            Ok(l) => Ok(l),
            Err(CallError::Unreachable(_)) if self.can_start(s) => {
                self.start_up(s, note).and_then(|()| {
                    srv::strata_loaded(&self.http, conn).map_err(|e| e.say(&s.name, &s.base_url))
                })
            }
            Err(e) => Err(e.say(&s.name, &s.base_url)),
        }
    }

    /// Launch the install and wait for the server to answer. One start at a time: anyone else
    /// waiting here finds the server answering (or fails the same way) rather than launching a
    /// second one. Any HTTP answer means it is up — a key refusal included, which the caller then
    /// reports as its own problem.
    fn start_up(&self, s: &Server, note: &dyn Fn(&str)) -> Result<(), String> {
        let _single = lock(&self.starting);
        let conn = self.conn(s)?;
        match srv::strata_loaded(&self.http, &conn) {
            Ok(_) => return Ok(()),
            Err(CallError::Unreachable(_)) => {}
            Err(_) => return Ok(()), // answering, just not about models: the caller's problem
        }
        let starter = self
            .starter
            .as_ref()
            .ok_or("Helm cannot start servers: none is set up.")?;
        note(&format!("Starting {}…", s.name));
        let mut child = {
            let _loading = read(&self.gate);
            self.refuse_if_locked()?;
            starter
                .start(s)
                .map_err(|e| format!("{} could not be started: {e}", s.name))?
        };
        let deadline = Instant::now() + self.timings.start_wait;
        loop {
            std::thread::sleep(self.timings.start_poll);
            // Unloaded while this start waited: stop waiting. The unload stops what was launched.
            self.refuse_if_locked()?;
            match srv::strata_loaded(&self.http, &conn) {
                Ok(_) => return Ok(()),
                Err(CallError::Unreachable(_)) => {}
                Err(_) => return Ok(()), // up; its key or shape problem surfaces in the caller
            }
            // A launcher that died at once (a bad preset, an install that is not there) should not
            // make the user wait the whole wait to hear about it.
            if let Some(ch) = child.as_mut() {
                if let Ok(Some(status)) = ch.try_wait() {
                    if !status.success() {
                        let how = match status.code() {
                            Some(c) => format!("exited with code {c}"),
                            None => "did not finish normally".to_owned(),
                        };
                        return Err(format!(
                            "{} could not be started: the launcher {how}.",
                            s.name
                        ));
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{} was started but did not answer within {} seconds.",
                    s.name,
                    self.timings.start_wait.as_secs()
                ));
            }
        }
    }

    // ---- the catalogue ----------------------------------------------------------------------

    /// Ask every enabled server what it has, remember the answers and say so. Servers are asked
    /// side by side with short timeouts; one that does not answer shows as unreachable and keeps
    /// the models last seen on it.
    pub fn refresh(&self) -> Vec<LocalModelInfo> {
        let st = self.settings();
        if self.locked() {
            // Nobody is asked while local models are unloaded: the cached list, marked down.
            return self.rows(&st, &self.seen());
        }
        let before = self.seen();
        let results: Vec<(String, SeenServer)> = std::thread::scope(|sc| {
            let handles: Vec<_> = st
                .servers
                .iter()
                .filter(|s| s.enabled)
                .map(|s| {
                    let prev = before.servers.get(&s.id).cloned();
                    (s.id.clone(), sc.spawn(|| self.fetch(&st, s, prev)))
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|(id, h)| h.join().ok().map(|r| (id, r)))
                .collect()
        });
        self.remember(results);
        self.emit();
        self.rows(&st, &self.seen())
    }

    fn fetch(&self, st: &LocalSettings, s: &Server, prev: Option<SeenServer>) -> SeenServer {
        let down = |why: String| SeenServer {
            reachable: false,
            note: Some(why),
            // The models last seen stay listed, greyed, so the picker still shows them.
            models: prev
                .as_ref()
                .map(|p| {
                    p.models
                        .iter()
                        .map(|m| SeenModel {
                            loaded: None,
                            ..m.clone()
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        let conn = match self.conn_or_say(s) {
            Ok(c) => c,
            Err(e) => return down(e),
        };
        match s.kind {
            LocalServerKind::Ollama => {
                let tags = match srv::ollama_tags(&self.http, &conn) {
                    Ok(t) => t,
                    Err(e) => return down(e.say(&s.name, &s.base_url)),
                };
                let resident = srv::ollama_ps(&self.http, &conn).ok();
                if let Some(ps) = &resident {
                    self.note_memory(ps);
                }
                let base: Vec<&srv::Installed> =
                    tags.iter().filter(|t| !srv::is_variant(&t.name)).collect();
                let natives = self.natives(s, &conn, &base);
                let models = base
                    .iter()
                    .zip(natives)
                    .map(|(t, native)| SeenModel {
                        model: t.name.clone(),
                        // The window Helm will ask for: its setting, held to what the model supports.
                        context_tokens: Some(match native {
                            Some(n) => n.min(u64::from(st.ollama_context)),
                            None => u64::from(st.ollama_context),
                        }),
                        size_bytes: (t.size > 0).then_some(t.size),
                        loaded: resident.as_ref().map(|ps| {
                            ps.iter().any(|r| {
                                srv::norm(srv::variant_base(&r.name)) == srv::norm(&t.name)
                            })
                        }),
                    })
                    .collect();
                SeenServer {
                    reachable: true,
                    note: None,
                    models,
                }
            }
            LocalServerKind::Strata => {
                let loaded = match self.strata_probe(s, &conn, &|_| {}) {
                    Ok(l) => l,
                    Err(e) => return down(e),
                };
                let listed = srv::strata_models(&self.http, &conn).or_else(|| {
                    s.model.clone().map(|m| {
                        vec![srv::Listed {
                            id: m,
                            context: None,
                        }]
                    })
                });
                let Some(listed) = listed else {
                    return SeenServer {
                        reachable: true,
                        note: Some(format!(
                            "{} did not list its models. Give it a model name in Settings > Local models.",
                            s.name
                        )),
                        models: Vec::new(),
                    };
                };
                // A model that does not name its own window takes the server's, which `/health`
                // reports as `max_context`. Without it the worker would run the chat with no
                // known ceiling and the server would hit the wall instead of compacting in time.
                let window = if listed.iter().any(|m| m.context.is_none()) {
                    srv::strata_context(&self.http, &conn)
                } else {
                    None
                };
                let models = listed
                    .into_iter()
                    .map(|m| SeenModel {
                        loaded: Some(loaded.iter().any(|l| l == "*" || l == &m.id)),
                        model: m.id,
                        context_tokens: m.context.or(window),
                        size_bytes: None,
                    })
                    .collect();
                SeenServer {
                    reachable: true,
                    note: None,
                    models,
                }
            }
        }
    }

    /// Each installed model's own context length, asked a few at a time and remembered.
    fn natives(&self, s: &Server, conn: &Conn, models: &[&srv::Installed]) -> Vec<Option<u64>> {
        let key = |m: &srv::Installed| format!("{}\u{0}{}\u{0}{}", s.id, m.name, m.size);
        let mut out: Vec<Option<Option<u64>>> = models
            .iter()
            .map(|m| lock(&self.native).get(&key(m)).copied())
            .collect();
        let missing: Vec<usize> = (0..models.len()).filter(|i| out[*i].is_none()).collect();
        for chunk in missing.chunks(8) {
            let found: Vec<(usize, Option<u64>)> = std::thread::scope(|sc| {
                chunk
                    .iter()
                    .map(|&i| {
                        (
                            i,
                            sc.spawn(move || {
                                srv::ollama_context(&self.http, conn, &models[i].name)
                            }),
                        )
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|(i, h)| (i, h.join().ok().flatten()))
                    .collect()
            });
            for (i, v) in found {
                lock(&self.native).insert(key(models[i]), v);
                out[i] = Some(v);
            }
        }
        out.into_iter().map(|o| o.flatten()).collect()
    }

    fn note_memory(&self, ps: &[srv::Resident]) {
        let mut v = lock(&self.vram);
        for r in ps.iter().filter(|r| r.size_vram > 0) {
            v.insert(
                srv::norm(srv::variant_base(&r.name)).to_owned(),
                r.size_vram,
            );
        }
    }

    /// The picker's rows. While local models are unloaded every row shows as down and unloaded.
    fn rows(&self, st: &LocalSettings, seen: &Seen) -> Vec<LocalModelInfo> {
        let locked = self.locked();
        st.servers
            .iter()
            .filter(|s| s.enabled)
            .flat_map(|s| {
                let last = seen.servers.get(&s.id);
                let reachable = !locked && last.is_some_and(|l| l.reachable);
                last.into_iter()
                    .flat_map(move |l| l.models.iter())
                    .map(move |m| LocalModelInfo {
                        id: helm_chats::local::id(&s.id, &m.model),
                        name: m.model.clone(),
                        server: s.name.clone(),
                        kind: s.kind,
                        context_tokens: m.context_tokens,
                        size_bytes: m.size_bytes,
                        loaded: if locked { Some(false) } else { m.loaded },
                        reachable,
                        // A Strata that did not answer but starts when used: the picker can offer it.
                        startable: !reachable && self.can_start(s),
                    })
            })
            .collect()
    }

    /// Sentences worth showing under the model list: servers that answered but have something to
    /// say (for example they did not list their models). While local models are unloaded, the
    /// lock's sentence comes first.
    pub fn notes(&self) -> Vec<String> {
        let st = self.settings();
        let seen = self.seen();
        self.refusal()
            .into_iter()
            .chain(st.servers.iter().filter(|s| s.enabled).filter_map(|s| {
                seen.servers
                    .get(&s.id)
                    .filter(|l| l.reachable)
                    .and_then(|l| l.note.clone())
            }))
            .collect()
    }

    // ---- Test -------------------------------------------------------------------------------

    /// Is the server there? One request, short timeout; reports a sentence either way.
    pub fn test(&self, server_id: &str) -> Result<TestResult, String> {
        let st = self.settings();
        let s = st
            .servers
            .iter()
            .find(|s| s.id == server_id)
            .ok_or("That server is not set up.")?;
        if let Some(message) = self.refusal() {
            return Ok(TestResult {
                ok: false,
                message,
                models: None,
            });
        }
        let result = match self.conn_or_say(s) {
            Err(e) => Err(e),
            Ok(conn) => match s.kind {
                LocalServerKind::Ollama => srv::ollama_tags(&self.http, &conn)
                    .map(|t| {
                        let n = t.iter().filter(|m| !srv::is_variant(&m.name)).count() as u32;
                        (
                            format!(
                                "{} answered: {n} model{} installed.",
                                s.name,
                                if n == 1 { "" } else { "s" }
                            ),
                            Some(n),
                        )
                    })
                    .map_err(|e| e.say(&s.name, &s.base_url)),
                LocalServerKind::Strata => self.strata_probe(s, &conn, &|_| {}).map(|l| {
                    let msg = match l.iter().find(|x| x.as_str() != "*") {
                        Some(m) => format!("{} answered: {m} is loaded.", s.name),
                        None if l.is_empty() => {
                            format!("{} answered: nothing is loaded.", s.name)
                        }
                        None => format!("{} answered: a model is loaded.", s.name),
                    };
                    (msg, None)
                }),
            },
        };
        let (ok, message, models) = match result {
            Ok((m, n)) => (true, m, n),
            Err(m) => (false, m, None),
        };
        self.remember(vec![(
            s.id.clone(),
            SeenServer {
                reachable: ok,
                note: (!ok).then(|| message.clone()),
                models: self
                    .seen()
                    .servers
                    .get(&s.id)
                    .map(|x| x.models.clone())
                    .unwrap_or_default(),
            },
        )]);
        self.emit();
        Ok(TestResult {
            ok,
            message,
            models,
        })
    }

    // ---- before and after a turn --------------------------------------------------------------

    fn busy_on(&self, server_id: &str, except_chat: &str) -> Vec<(String, String)> {
        self.db
            .snapshot(|tx| {
                let mut st = tx.prepare(
                    "SELECT c.title, c.model FROM chats c
                     WHERE c.provider = ?1 AND c.archived = 0 AND c.id <> ?2
                       AND c.status IN ('starting', 'running', 'stopping')",
                )?;
                let rows = st.query_map([server_id, except_chat], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default()
    }

    /// Unload the idle models on every enabled server except `except` (all of them when it is
    /// `None`). A model a chat is answering in (not counting `chat_id`) is left alone and named
    /// instead. Returns how many unloads went through, and the first model left alone.
    fn unload_idle(
        &self,
        st: &LocalSettings,
        except: Option<&str>,
        chat_id: &str,
    ) -> (u32, Option<(String, String)>) {
        let mut asked = 0;
        let mut blocker: Option<(String, String)> = None;
        for other in st
            .servers
            .iter()
            .filter(|o| o.enabled && Some(o.id.as_str()) != except)
        {
            let Ok(conn) = self.conn(other) else { continue };
            let loaded: Vec<String> = match other.kind {
                LocalServerKind::Ollama => match srv::ollama_ps(&self.http, &conn) {
                    Ok(ps) => {
                        self.note_memory(&ps);
                        ps.into_iter().map(|r| r.name).collect()
                    }
                    Err(_) => continue,
                },
                LocalServerKind::Strata => match srv::strata_loaded(&self.http, &conn) {
                    Ok(l) => l,
                    Err(_) => continue,
                },
            };
            let busy = self.busy_on(&other.id, chat_id);
            for name in loaded {
                let working = busy.iter().find(|(_, m)| match other.kind {
                    LocalServerKind::Ollama => srv::norm(m) == srv::norm(srv::variant_base(&name)),
                    // Strata holds one model at a time: any chat on it is using it.
                    LocalServerKind::Strata => true,
                });
                if let Some((title, _)) = working {
                    blocker.get_or_insert((other.name.clone(), title.clone()));
                    continue;
                }
                let done = match other.kind {
                    LocalServerKind::Ollama => srv::ollama_unload(&self.http, &conn, &name),
                    LocalServerKind::Strata => srv::strata_unload(&self.http, &conn),
                };
                match done {
                    Ok(()) => asked += 1,
                    Err(e) => log::warn!(
                        "could not unload {name} on {}: {}",
                        other.name,
                        e.say(&other.name, &other.base_url)
                    ),
                }
            }
        }
        (asked, blocker)
    }

    /// Free graphics memory for `need` bytes, by unloading idle models on the other servers. Only
    /// a model that does not fit makes anything unload; when something must go anyway, the wait
    /// for memory aims to leave the speech reserve free as well, so speech-to-text still fits
    /// beside the new model. The reserve on its own never unloads a model. `Err` when models that
    /// are mid-reply are what stands in the way, or when local models were unloaded meanwhile.
    fn make_room(
        &self,
        st: &LocalSettings,
        server: &Server,
        display: &str,
        need: u64,
        chat_id: &str,
    ) -> Result<(), String> {
        let Some(free) = self.gpu.free_bytes() else {
            return Ok(()); // no figure: the server decides, as it would without Helm
        };
        if need <= free {
            return Ok(());
        }
        let (unloaded, blocker) = self.unload_idle(st, Some(server.id.as_str()), chat_id);
        // A server hands its memory back a moment after an unload. The reserve is only waited
        // for when something was unloaded to make it: otherwise waiting could not change it.
        let goal = if unloaded > 0 {
            need + self.speech_reserve(st)
        } else {
            need
        };
        let deadline = Instant::now() + self.timings.memory_wait;
        loop {
            self.refuse_if_locked()?;
            if self.gpu.free_bytes().is_none_or(|f| goal <= f) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(self.timings.memory_poll);
        }
        match blocker {
            Some((other, title)) if self.gpu.free_bytes().is_some_and(|f| f < need) => {
                Err(format!(
                    "{display} can't load while {other} is answering in {title}. Send again when it finishes."
                ))
            }
            // Nothing Helm can free, or the model fits without the reserve: let the server try,
            // as it would without Helm.
            _ => Ok(()),
        }
    }

    fn prepare_ollama(
        &self,
        st: &LocalSettings,
        server: &Server,
        conn: &Conn,
        turn: &LocalTurn,
        progress: &dyn Fn(&str),
    ) -> Result<String, String> {
        let who = |e: CallError| e.say(&server.name, &server.base_url);
        let tags = srv::ollama_tags(&self.http, conn).map_err(who)?;
        let base = tags
            .iter()
            .find(|t| srv::norm(&t.name) == srv::norm(&turn.model))
            .ok_or_else(|| {
                format!(
                    "{} is not installed on {} any more. Pick another model, or pull it again.",
                    turn.model, server.name
                )
            })?;
        // The worker keeps asking for the variant it was launched with; a new worker takes the
        // variant for the context size set now. A launched variant of another model (the chat's
        // model was just changed and its worker is still closing) is not the one to prepare.
        let target = match &turn.launched {
            Some(l) if srv::norm(srv::variant_base(l)) == srv::norm(&turn.model) => l.clone(),
            _ => srv::variant_name(&turn.model, st.ollama_context),
        };
        let ctx = srv::variant_context(&target).unwrap_or(st.ollama_context);
        let made = !tags
            .iter()
            .any(|t| srv::norm(&t.name) == srv::norm(&target));
        if made {
            srv::ollama_create(&self.http, conn, &target, &base.name, ctx).map_err(who)?;
        }
        // The ledger of variants Helm made, so a later step can list the stale ones and hide or
        // delete them. A ledger fault never fails the turn.
        if let Err(e) = self.db.write(|tx| {
            crate::variants::note_variant(tx, &server.id, &target, &base.name, ctx, made)
        }) {
            log::warn!("could not record the variant {target} in the ledger: {e}");
        }
        let resident = srv::ollama_ps(&self.http, conn).map_err(who)?;
        self.note_memory(&resident);
        let loaded = resident
            .iter()
            .any(|r| srv::norm(&r.name) == srv::norm(&target));
        let _one_at_a_time;
        if !loaded {
            _one_at_a_time = lock(&self.loading);
            progress(&format!("Loading {}…", turn.model));
            let need = lock(&self.vram)
                .get(srv::norm(&turn.model))
                .copied()
                .unwrap_or(base.size / 10 * 12);
            self.make_room(st, server, &turn.model, need, &turn.chat_id)?;
        }
        // Loads the variant (or only restarts its keep-alive timer when it is already there).
        {
            let _loading = read(&self.gate);
            self.refuse_if_locked()?;
            srv::ollama_load(&self.http, conn, &target, st.keep_alive_minutes).map_err(who)?;
        }
        // Helm's older variants of this model go, unless a running worker still uses one.
        for old in tags.iter().filter(|t| {
            srv::is_variant(&t.name)
                && srv::norm(srv::variant_base(&t.name)) == srv::norm(&turn.model)
                && srv::norm(&t.name) != srv::norm(&target)
                && !turn
                    .in_use
                    .iter()
                    .any(|u| srv::norm(u) == srv::norm(&t.name))
        }) {
            match srv::ollama_delete(&self.http, conn, &old.name) {
                Ok(()) => {
                    if let Err(e) = self
                        .db
                        .write(|tx| crate::variants::forget_variant(tx, &server.id, &old.name))
                    {
                        log::warn!("could not remove {} from the variant ledger: {e}", old.name);
                    }
                }
                Err(e) => log::warn!(
                    "could not remove the older variant {}: {}",
                    old.name,
                    who(e)
                ),
            }
        }
        Ok(target)
    }

    fn prepare_strata(
        &self,
        st: &LocalSettings,
        server: &Server,
        conn: &Conn,
        turn: &LocalTurn,
        progress: &dyn Fn(&str),
    ) -> Result<String, String> {
        let loaded = self.strata_probe(server, conn, progress)?;
        if !loaded.iter().any(|l| l == "*" || l == &turn.model) {
            let _one_at_a_time = lock(&self.loading);
            // The model loads when the first request reaches the server; this line stays up until
            // its first words arrive.
            progress(&format!("Loading {}…", turn.model));
            let need = u64::from(st.strata_need_gb) * GB;
            self.make_room(st, server, &turn.model, need, &turn.chat_id)?;
        }
        // The model loads when the worker's first request reaches the server: one last look.
        self.refuse_if_locked()?;
        Ok(turn.model.clone())
    }
}

// ---- the graphics-card lock -----------------------------------------------------------------

impl LocalService {
    /// The lock now. Other parts of Helm read this before they offer a local model.
    pub fn lock_state(&self) -> LockState {
        lock(&self.gpu_lock).clone()
    }

    fn locked(&self) -> bool {
        lock(&self.gpu_lock).is_locked()
    }

    /// The sentence a load gets while local models are unloaded.
    fn refusal(&self) -> Option<String> {
        let state = self.lock_state();
        if !state.is_locked() {
            return None;
        }
        state.refusal(self.settings().game_lift_minutes)
    }

    fn refuse_if_locked(&self) -> Result<(), String> {
        match self.refusal() {
            Some(text) => Err(text),
            None => Ok(()),
        }
    }

    /// The lock as the top bar and Settings show it.
    pub fn lock_view(&self) -> LockView {
        let st = self.settings();
        let state = self.lock_state();
        let p = lock(&self.progress);
        LockView {
            state: match state {
                LockState::On => LockKind::On,
                LockState::Manual { .. } => LockKind::Manual,
                LockState::Game { .. } => LockKind::Game,
            },
            since: state.since(),
            game: state.game().map(str::to_owned),
            working: p.pending > 0,
            report: p.report.clone(),
            any_local: st.servers.iter().any(|s| s.enabled),
            lift_minutes: st.game_lift_minutes,
            restart_task: st.restart_task.clone(),
        }
    }

    fn emit_lock(&self) {
        self.events.emit(AppEvent::new(LOCK_EVENT));
        // The picker's rows change with the lock.
        self.emit();
    }

    fn begin_work(&self) {
        let mut p = lock(&self.progress);
        p.pending += 1;
        p.report.clear();
    }

    fn end_work(&self, report: Vec<String>) {
        {
            let mut p = lock(&self.progress);
            p.pending = p.pending.saturating_sub(1);
            if !report.is_empty() {
                p.report = report;
            }
        }
        self.emit_lock();
    }

    /// The task runner, when one may be used: never with the test override in effect.
    fn task_runner(&self) -> Option<Arc<dyn RestartTask>> {
        self.task.clone().filter(|_| test_override().is_none())
    }

    /// The Strata stopper, under the same rule.
    fn stopper(&self) -> Option<Arc<dyn StrataStarter>> {
        self.starter.clone().filter(|_| test_override().is_none())
    }

    /// Unload local models by hand: the lock is stored at once, so from here on every load is
    /// refused, and the rest runs off this thread. While a game holds the lock, this turns it
    /// into a manual one (the unloading is already done), so models stay off after the game
    /// closes. Refused when the lock is already manual.
    pub fn unload(self: &Arc<Self>) -> Result<LockView, String> {
        self.lock_if_on(
            LockState::Manual {
                since: helm_core::now_ms(),
            },
            true,
        )?;
        Ok(self.lock_view())
    }

    /// Store `next` if nothing is locked yet, then do the unloading on a thread of its own. With
    /// `upgrade`, a game lock is made manual instead (keeping its time) and nothing more is done.
    fn lock_if_on(self: &Arc<Self>, next: LockState, upgrade: bool) -> Result<(), String> {
        {
            let mut cur = lock(&self.gpu_lock);
            let held = cur.clone();
            if let (true, LockState::Game { since, .. }) = (upgrade, &held) {
                let manual = LockState::Manual { since: *since };
                self.db
                    .write(|tx| gpulock::store(tx, &manual))
                    .map_err(|e| format!("Local models could not be kept off: {e}"))?;
                *cur = manual;
                drop(cur);
                self.emit_lock();
                return Ok(());
            }
            if held.is_locked() {
                return Err(ALREADY_UNLOADED.into());
            }
            self.db
                .write(|tx| gpulock::store(tx, &next))
                .map_err(|e| format!("Local models could not be unloaded: {e}"))?;
            *cur = next;
        }
        self.begin_work();
        self.emit_lock();
        let me = self.clone();
        std::thread::spawn(move || me.enforce(true));
        Ok(())
    }

    /// The unloading itself: pause the restart task, stop the busy local chats (when
    /// `stop_chats`), unload every configured server. One sentence per step goes in the report.
    fn enforce(&self, stop_chats: bool) {
        let _one = lock(&self.work);
        // Ends the work this was started for, even if a step below panics.
        let mut work = EndWork {
            svc: self,
            report: Vec::new(),
        };
        if !self.locked() {
            // Turned back on before this got its turn: nothing to do.
            return;
        }
        // Wait out any load that passed its check before the lock landed.
        drop(self.gate.write().unwrap_or_else(|e| e.into_inner()));
        let st = self.settings();
        self.pause_restarts(&st, &mut work.report);
        if stop_chats {
            self.stop_local_chats(&st, &mut work.report);
        }
        self.unload_servers(&st, &mut work.report);
    }

    fn pause_restarts(&self, st: &LocalSettings, report: &mut Vec<String>) {
        if let Some(path) = &st.pause_file {
            report.push(match std::fs::write(path, gpulock::PAUSE_TEXT) {
                Ok(()) => "Wrote the pause file.".to_owned(),
                Err(e) => format!("The pause file could not be written: {e}."),
            });
        }
        if let Some(name) = &st.restart_task {
            report.push(match self.task_runner() {
                None => format!(
                    "The restart task \"{name}\" was left running: Helm can't pause tasks here."
                ),
                Some(t) => match t.end(name) {
                    Ok(()) => format!("Paused the restart task \"{name}\"."),
                    Err(e) => format!("The restart task \"{name}\" could not be paused: {e}."),
                },
            });
        }
    }

    /// The chats on a configured server that are starting, answering or stopping.
    fn busy_local(&self, st: &LocalSettings) -> Vec<String> {
        self.db
            .snapshot(|tx| {
                let mut q = tx.prepare(
                    "SELECT id, provider FROM chats
                     WHERE archived = 0 AND status IN ('starting', 'running', 'stopping')",
                )?;
                let rows =
                    q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, provider)| st.servers.iter().any(|s| &s.id == provider))
            .map(|(id, _)| id)
            .collect()
    }

    fn stop_local_chats(&self, st: &LocalSettings, report: &mut Vec<String>) {
        let busy = self.busy_local(st);
        if busy.is_empty() {
            return;
        }
        let Some(ctl) = lock(&self.chats).clone() else {
            report.push("Busy local-model chats could not be stopped from here.".into());
            return;
        };
        for id in &busy {
            ctl.stop(id, gpulock::STOPPED_TEXT);
        }
        let until = Instant::now() + ctl.stop_limit();
        while Instant::now() < until && !self.busy_local(st).is_empty() {
            std::thread::sleep(Duration::from_millis(100));
        }
        report.push(format!(
            "Stopped {} local-model chat{}.",
            busy.len(),
            plural(busy.len())
        ));
    }

    /// Unload every configured server, switched on or not, and stop Strata's process.
    fn unload_servers(&self, st: &LocalSettings, report: &mut Vec<String>) {
        for s in &st.servers {
            let conn = match self.conn(s) {
                Ok(c) => c,
                Err(e) => {
                    report.push(format!("{} was left alone: {e}", s.name));
                    continue;
                }
            };
            match s.kind {
                LocalServerKind::Ollama => report.push(match srv::ollama_ps(&self.http, &conn) {
                    Err(CallError::Unreachable(_)) => format!("{} was not running.", s.name),
                    Err(e) => e.say(&s.name, &s.base_url),
                    Ok(ps) => {
                        // Every resident model goes, Helm's or not.
                        let mut n = 0;
                        for r in &ps {
                            match srv::ollama_unload(&self.http, &conn, &r.name) {
                                Ok(()) => n += 1,
                                Err(e) => log::warn!(
                                    "could not unload {} on {}: {}",
                                    r.name,
                                    s.name,
                                    e.say(&s.name, &s.base_url)
                                ),
                            }
                        }
                        match (n, ps.len()) {
                            (0, 0) => format!("{} had nothing loaded.", s.name),
                            (n, all) if n == all => {
                                format!("{} unloaded {n} model{}.", s.name, plural(n))
                            }
                            (n, all) => format!(
                                "{} unloaded {n} of its {all} models; the rest did not answer.",
                                s.name
                            ),
                        }
                    }
                }),
                LocalServerKind::Strata => {
                    let unloaded = srv::strata_unload(&self.http, &conn);
                    let stopped = self.stopper().map(|st| st.stop(s));
                    report.push(strata_sentence(s, unloaded, stopped));
                }
            }
        }
    }

    /// Turn local models back on: wait for any unload still running, clear the lock, remove the
    /// pause file and start the restart task again. A turn back on during a game counts as an
    /// override for that game until it closes. Done here when nothing else is running, else on a
    /// thread of its own (the view then says `working`).
    pub fn reload(self: &Arc<Self>) -> LockView {
        let was = self.lock_state();
        if !was.is_locked() {
            return self.lock_view();
        }
        lock(&self.watch).override_running(&was);
        self.begin_work();
        let free = match self.work.try_lock() {
            Ok(g) => Some(g),
            Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        match free {
            Some(_one) => self.lift(&was),
            None => {
                let me = self.clone();
                std::thread::spawn(move || {
                    let _one = lock(&me.work);
                    me.lift(&was);
                });
            }
        }
        self.lock_view()
    }

    /// Clear the lock if it is still `expected`, then let the restarter run again. The caller
    /// holds the work lock and has counted this in the progress.
    fn lift(&self, expected: &LockState) {
        {
            let mut cur = lock(&self.gpu_lock);
            if *cur != *expected {
                drop(cur);
                self.end_work(Vec::new());
                return;
            }
            if let Err(e) = self.db.write(|tx| gpulock::store(tx, &LockState::On)) {
                drop(cur);
                self.end_work(vec![format!(
                    "Local models could not be turned back on: {e}"
                )]);
                return;
            }
            *cur = LockState::On;
        }
        let st = self.settings();
        let mut report = vec!["Local models are back on.".to_owned()];
        if let Some(path) = &st.pause_file {
            match std::fs::remove_file(path) {
                Ok(()) => report.push("Removed the pause file.".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => report.push(format!("The pause file could not be removed: {e}.")),
            }
        }
        if let Some(name) = &st.restart_task {
            report.push(match self.task_runner() {
                None => format!(
                    "The restart task \"{name}\" was not started: Helm can't run tasks here."
                ),
                Some(t) => match t.run(name) {
                    Ok(()) => format!("Started the restart task \"{name}\" again."),
                    Err(e) => format!("The restart task \"{name}\" could not be started: {e}."),
                },
            });
        }
        self.end_work(report);
    }

    /// At Helm's start. With a lock stored, the unloading runs once more (without stopping chats,
    /// which have not started): a restart task may have relaunched a server at boot. A game lock
    /// whose list has since been emptied is lifted instead. Contacts no server without a stored
    /// lock, and none under the test override.
    pub fn resume(&self) {
        let state = self.lock_state();
        if !state.is_locked() {
            return;
        }
        if matches!(state, LockState::Game { .. }) {
            if self.settings().games.is_empty() {
                self.begin_work();
                let _one = lock(&self.work);
                self.lift(&state);
                return;
            }
            // The game is given the usual delay from now, not from before Helm started.
            lock(&self.watch).last_seen = Some(helm_core::now_ms());
        }
        if test_override().is_some() {
            return;
        }
        self.begin_work();
        self.enforce(false);
    }

    /// One read of the process list: set a game lock when a listed game runs, lift it once the
    /// games have been gone long enough. Reads nothing while there is nothing to watch.
    pub fn game_tick(self: &Arc<Self>, procs: &dyn ProcessNames, now: i64) {
        let st = self.settings();
        let state = self.lock_state();
        let game_locked = matches!(state, LockState::Game { .. });
        if !game_locked && (st.games.is_empty() || st.servers.is_empty()) {
            return;
        }
        let seen = if st.games.is_empty() {
            Vec::new()
        } else {
            procs.names()
        };
        let action = games::next(
            &state,
            &mut lock(&self.watch),
            &seen,
            now,
            &st.games,
            st.game_lift_minutes,
        );
        match action {
            Action::Lock(game) => {
                // A lock that landed in the meantime wins; nothing to do then.
                let _ = self.lock_if_on(LockState::Game { since: now, game }, false);
            }
            Action::Lift => {
                self.begin_work();
                let _one = lock(&self.work);
                self.lift(&state);
            }
            Action::Nothing => {}
        }
    }

    /// Start the game watch on a thread of its own, after [`LocalService::resume`].
    pub fn start_watch(self: &Arc<Self>, procs: Arc<dyn ProcessNames>) {
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("local-models-watch".into())
            .spawn(move || {
                me.resume();
                loop {
                    me.game_tick(&*procs, helm_core::now_ms());
                    std::thread::sleep(games::POLL);
                }
            });
        if let Err(e) = spawned {
            log::warn!("the game watch could not start: {e}");
        }
    }
}

/// Counts one piece of work as finished, with its report, when dropped.
struct EndWork<'a> {
    svc: &'a LocalService,
    report: Vec<String>,
}

impl Drop for EndWork<'_> {
    fn drop(&mut self) {
        self.svc.end_work(std::mem::take(&mut self.report));
    }
}

/// What happened to a Strata server on an unload, in one sentence.
fn strata_sentence(
    s: &Server,
    unloaded: Result<(), CallError>,
    stopped: Option<Result<Stopped, String>>,
) -> String {
    let name = &s.name;
    match (unloaded, stopped) {
        (Ok(()), Some(Ok(Stopped::Done))) => {
            format!("{name}'s model was unloaded and its server stopped.")
        }
        (Ok(()), Some(Ok(Stopped::NoInstall))) => format!(
            "{name}'s model was unloaded; its server keeps running (Helm could not find its install to stop it)."
        ),
        (Ok(()), Some(Err(e))) => {
            format!("{name}'s model was unloaded, but its server could not be stopped: {e}.")
        }
        (Ok(()), None) => format!("{name}'s model was unloaded."),
        (Err(CallError::Unreachable(_)), Some(Ok(Stopped::Done))) => {
            format!("{name} was not answering; its install was told to stop anyway.")
        }
        (Err(CallError::Unreachable(_)), _) => format!("{name} was not running."),
        (Err(e), Some(Ok(Stopped::Done))) => format!(
            "{} Its server was stopped.",
            e.say(name, &s.base_url)
        ),
        (Err(e), _) => e.say(name, &s.base_url),
    }
}

impl LocalModels for LocalService {
    fn knows(&self, server_id: &str) -> bool {
        self.settings().servers.iter().any(|s| s.id == server_id)
    }

    fn available(&self, server_id: &str) -> bool {
        self.settings()
            .servers
            .iter()
            .any(|s| s.id == server_id && s.enabled)
    }

    fn server(&self, server_id: &str, model: &str) -> Result<LocalServer, String> {
        self.refuse_if_locked()?;
        let st = self.settings();
        let s = st
            .servers
            .iter()
            .find(|s| s.id == server_id)
            .ok_or("That local model server is not set up any more. Pick another model or add it in Settings > Local models.")?;
        if !s.enabled {
            return Err(format!(
                "{} is switched off in Settings > Local models.",
                s.name
            ));
        }
        let conn = self.conn_or_say(s)?;
        if conn.base == net::OFF {
            return Err(format!(
                "{} can't be used: local model servers are switched off for this run.",
                s.name
            ));
        }
        let context = match s.kind {
            LocalServerKind::Ollama => Some(u64::from(
                srv::variant_context(model).unwrap_or(st.ollama_context),
            )),
            LocalServerKind::Strata => self
                .seen()
                .servers
                .get(&s.id)
                .and_then(|l| l.models.iter().find(|m| m.model == model))
                .and_then(|m| m.context_tokens),
        };
        Ok(LocalServer {
            base_url: conn.base,
            key: conn.key.unwrap_or_else(|| KEYLESS.to_owned()),
            context_tokens: context,
            // Room for the answer without crowding the window out.
            max_output_tokens: context.map(|c| (c / 4).clamp(1024, 32_000)),
            lean: st.tools == ToolsSetup::Lean,
        })
    }

    fn prepare(&self, turn: &LocalTurn, progress: &dyn Fn(&str)) -> Result<String, String> {
        self.refuse_if_locked()?;
        let st = self.settings();
        let server = st
            .servers
            .iter()
            .find(|s| s.id == turn.server_id)
            .ok_or("That local model server is not set up any more. Pick another model or add it in Settings > Local models.")?;
        if !server.enabled {
            return Err(format!(
                "{} is switched off in Settings > Local models.",
                server.name
            ));
        }
        let conn = self.conn_or_say(server)?;
        let out = match server.kind {
            LocalServerKind::Ollama => self.prepare_ollama(&st, server, &conn, turn, progress),
            LocalServerKind::Strata => self.prepare_strata(&st, server, &conn, turn, progress),
        };
        // What the check learned about the server counts as having asked it.
        if out.is_ok() {
            let mut g = lock(&self.seen);
            if let Some(l) = g.as_mut().and_then(|s| s.servers.get_mut(&server.id)) {
                l.reachable = true;
                l.note = None;
            }
        }
        out
    }

    fn finished(&self, server_id: &str, model: &str) {
        if self.locked() {
            return;
        }
        let st = self.settings();
        let Some(s) = st
            .servers
            .iter()
            .find(|s| s.id == server_id && s.kind == LocalServerKind::Ollama)
            .cloned()
        else {
            return;
        };
        let Ok(conn) = self.conn(&s) else { return };
        let me = self.handle();
        let (model, minutes) = (model.to_owned(), st.keep_alive_minutes);
        // Claude Code's own requests leave Ollama's default timer behind; put Helm's back. Off this
        // thread, and only for a model that is still loaded (this never loads one).
        std::thread::spawn(move || {
            let Ok(ps) = srv::ollama_ps(&me.http, &conn) else {
                return;
            };
            if ps.iter().any(|r| srv::norm(&r.name) == srv::norm(&model)) {
                let _loading = read(&me.gate);
                if !lock(&me.gpu_lock).is_locked() {
                    let _ = srv::ollama_load(&me.http, &conn, &model, minutes);
                }
            }
        });
    }

    fn catalogue(&self, refresh: bool) -> Vec<LocalModelInfo> {
        if refresh {
            self.refresh()
        } else {
            self.rows(&self.settings(), &self.seen())
        }
    }

    fn queue(&self, server_id: &str) -> Option<QueueInfo> {
        // Only Strata answers one request at a time and reports its slots; Ollama serves several
        // at once. A failed or empty answer says nothing, and says nothing about reachability:
        // this read never marks a server reachable or unreachable either way.
        let st = self.settings();
        let s = st
            .servers
            .iter()
            .find(|s| s.id == server_id && s.kind == LocalServerKind::Strata)?;
        if !s.enabled || self.locked() {
            return None;
        }
        let conn = self.conn(s).ok()?;
        if conn.base == net::OFF {
            return None;
        }
        srv::strata_queue(&self.http, &conn)
    }
}

/// A thread-safe handle on the pieces `finished` uses off the calling thread.
struct Handle {
    http: Http,
    gpu_lock: Arc<Mutex<LockState>>,
    gate: Arc<RwLock<()>>,
}

impl LocalService {
    fn handle(&self) -> Handle {
        Handle {
            http: Http::default(),
            gpu_lock: self.gpu_lock.clone(),
            gate: self.gate.clone(),
        }
    }
}
