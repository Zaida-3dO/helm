//! The local model service: the servers' settings and keys, the catalogue, and what happens
//! before and after each turn of a local chat.
//!
//! **Nothing here runs on its own.** Helm reaches a server only when asked: a catalogue refresh
//! (the picker or Settings opened), a Test, or a local chat's send. There is no start-up probe and
//! no timer. A Strata server whose settings allow it is *launched* at one of those same moments,
//! and only when a check finds it not answering — see [`starter`]. Nothing here ever stops,
//! restarts or unloads a running server beyond what the memory check does to idle models.
//!
//! **Before a turn** ([`LocalService::prepare`]): if the model is already loaded, go. If not, the
//! free graphics memory is read; when the model will not fit, idle models on the *other* servers
//! are unloaded (idle meaning no Helm chat on that model is mid-turn); when only models that are
//! mid-reply stand in the way, no turn starts and the message stays in the draft. Ollama also gets
//! a Helm variant of the model with the chosen context window, loaded with the chosen keep-alive;
//! nothing is changed on Ollama for any other use.

use crate::gpu::Gpu;
use crate::net::{self, Conn, Http};
use crate::servers::{self as srv, CallError};
use crate::settings::{self, LocalSettings, Server, ToolsSetup};
use crate::starter::StrataStarter;
use crate::types::{LocalModelsState, Range, ServerView, TestResult};
use helm_chats::local::{LocalModels, LocalServer, LocalTurn};
use helm_chats::types::{LocalModelInfo, LocalServerKind};
use helm_core::credentials::CredentialStore;
use helm_core::db::Database;
use helm_ipc::{AppEvent, EventSink};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The test override: in debug builds only, `HELM_TEST_LOCAL_SERVERS` is `off` (no server is ever
/// contacted) or a loopback address every configured server uses instead.
pub const TEST_ENV: &str = "HELM_TEST_LOCAL_SERVERS";

/// A key a server that wants none is given: Claude Code needs some token, and it must never fall
/// back to the user's own sign-in.
pub const KEYLESS: &str = "local";

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
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl LocalService {
    pub fn new(
        db: Arc<Database>,
        creds: CredentialStore,
        events: Arc<dyn EventSink>,
        gpu: Arc<dyn Gpu>,
        cache_file: PathBuf,
    ) -> Self {
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
    /// and no test override — with one in effect the run points at fakes and the real starter must
    /// never spawn.
    fn can_start(&self, s: &Server) -> bool {
        s.kind == LocalServerKind::Strata
            && s.auto_start
            && self.starter.is_some()
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
        let mut child = starter
            .start(s)
            .map_err(|e| format!("{} could not be started: {e}", s.name))?;
        let deadline = Instant::now() + self.timings.start_wait;
        loop {
            std::thread::sleep(self.timings.start_poll);
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
                let models = listed
                    .into_iter()
                    .map(|m| SeenModel {
                        loaded: Some(loaded.iter().any(|l| l == "*" || l == &m.id)),
                        model: m.id,
                        context_tokens: m.context,
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

    fn rows(&self, st: &LocalSettings, seen: &Seen) -> Vec<LocalModelInfo> {
        st.servers
            .iter()
            .filter(|s| s.enabled)
            .flat_map(|s| {
                let last = seen.servers.get(&s.id);
                let reachable = last.is_some_and(|l| l.reachable);
                last.into_iter()
                    .flat_map(move |l| l.models.iter())
                    .map(move |m| LocalModelInfo {
                        id: helm_chats::local::id(&s.id, &m.model),
                        name: m.model.clone(),
                        server: s.name.clone(),
                        kind: s.kind,
                        context_tokens: m.context_tokens,
                        size_bytes: m.size_bytes,
                        loaded: m.loaded,
                        reachable,
                        // A Strata that did not answer but starts when used: the picker can offer it.
                        startable: !reachable && self.can_start(s),
                    })
            })
            .collect()
    }

    /// Sentences worth showing under the model list: servers that answered but have something to
    /// say (for example they did not list their models).
    pub fn notes(&self) -> Vec<String> {
        let st = self.settings();
        let seen = self.seen();
        st.servers
            .iter()
            .filter(|s| s.enabled)
            .filter_map(|s| {
                seen.servers
                    .get(&s.id)
                    .filter(|l| l.reachable)
                    .and_then(|l| l.note.clone())
            })
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

    /// Free graphics memory for `need` bytes, by unloading idle models on the other servers.
    /// `Err` only when models that are mid-reply are what stands in the way.
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
        let mut blocker: Option<(String, String)> = None;
        for other in st.servers.iter().filter(|o| o.enabled && o.id != server.id) {
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
                if let Err(e) = done {
                    log::warn!(
                        "could not unload {name} on {}: {}",
                        other.name,
                        e.say(&other.name, &other.base_url)
                    );
                }
            }
        }
        // A server hands its memory back a moment after an unload.
        let deadline = Instant::now() + self.timings.memory_wait;
        loop {
            if self.gpu.free_bytes().is_none_or(|f| need <= f) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(self.timings.memory_poll);
        }
        match blocker {
            Some((other, title)) => Err(format!(
                "{display} can't load while {other} is answering in {title}. Send again when it finishes."
            )),
            // Nothing Helm can free: let the server try, as it would without Helm.
            None => Ok(()),
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
        if !tags
            .iter()
            .any(|t| srv::norm(&t.name) == srv::norm(&target))
        {
            srv::ollama_create(&self.http, conn, &target, &base.name, ctx).map_err(who)?;
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
        srv::ollama_load(&self.http, conn, &target, st.keep_alive_minutes).map_err(who)?;
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
            if let Err(e) = srv::ollama_delete(&self.http, conn, &old.name) {
                log::warn!(
                    "could not remove the older variant {}: {}",
                    old.name,
                    who(e)
                );
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
            let need = u64::from(st.strata_need_gb) * 1024 * 1024 * 1024;
            self.make_room(st, server, &turn.model, need, &turn.chat_id)?;
        }
        Ok(turn.model.clone())
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
                let _ = srv::ollama_load(&me.http, &conn, &model, minutes);
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
}

/// A thread-safe handle on the pieces `finished` uses off the calling thread.
struct Handle {
    http: Http,
}

impl LocalService {
    fn handle(&self) -> Handle {
        Handle {
            http: Http::default(),
        }
    }
}
