//! The service around the indexer: the thread that runs passes, the folder watchers, the nightly
//! scan, and the checks every change of sources goes through (Settings and the agent tool share
//! them).
//!
//! One std thread at below-normal priority waits for something to do: a wake (a new line, a
//! changed setting), a changed file (debounced for two seconds, because the folders are synced and
//! a burst of changes should make one pass), a fallback poll for sources with no live watch, and
//! the nightly full scan. Nothing here ever blocks a runtime thread.

use crate::answer::Answerer;
use crate::engine::{EmbedErr, Embedder, Indexer, Pacing};
use crate::feeds::Env;
use crate::hub::IndexHub;
use crate::model::{
    parse_nightly, Kind, Scope, Settings, Source, SourceView, StateView, DEFAULT_MODEL,
};
use crate::store;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{AppEvent, EventSink, Services};
use helm_localmodels::LocalService;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The event that tells Settings to read the state again.
pub const EVENT: &str = "memindex";

/// Changed files within this long make one pass.
const DEBOUNCE: Duration = Duration::from_secs(2);
/// Sources with no live watch are looked at this often.
const POLL: Duration = Duration::from_secs(120);
const MAX_SOURCES: usize = 40;

enum Msg {
    /// Something that may need a pass changed (a line came up, a setting was saved).
    Wake,
    Dirty(String),
    Reindex {
        source: Option<String>,
        force: bool,
    },
    Stop,
}

/// Embeds through the local model service, under its graphics-card lock.
pub struct LocalEmbedder {
    pub local: Arc<LocalService>,
    pub db: Arc<Database>,
}

impl Embedder for LocalEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedErr> {
        if self.local.lock_state().is_locked() {
            return Err(EmbedErr::Locked);
        }
        let s = self
            .db
            .snapshot(store::load_settings)
            .map_err(|e| EmbedErr::Failed(e.to_string()))?;
        if s.server_id.is_empty() {
            return Err(EmbedErr::Failed("no embedding server is chosen".into()));
        }
        self.local
            .embed(&s.server_id, &s.model, texts)
            .map_err(|e| {
                if self.local.lock_state().is_locked() {
                    EmbedErr::Locked
                } else {
                    EmbedErr::Failed(e)
                }
            })
    }

    fn busy(&self) -> bool {
        self.local.local_chat_busy()
    }
}

/// An embedder for an install with no local models: nothing is embedded.
struct NoEmbedder;

impl Embedder for NoEmbedder {
    fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>, EmbedErr> {
        Err(EmbedErr::Failed("local models are not available".into()))
    }
    fn busy(&self) -> bool {
        false
    }
}

pub struct Service {
    pub indexer: Arc<Indexer>,
    hub: Arc<dyn IndexHub>,
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    tx: Mutex<Sender<Msg>>,
    rx: Mutex<Option<Receiver<Msg>>>,
    watchers: Mutex<HashMap<String, RecommendedWatcher>>,
    /// Folder sources whose live watch could not start; they are polled.
    unwatched: Mutex<HashSet<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The service, made once the hub line and the other services exist. Does nothing until
/// [`start`] runs the thread.
pub fn register_services(
    services: &mut Services,
    events: &Arc<dyn EventSink>,
    hub: Arc<dyn IndexHub>,
) {
    let (Some(db), Some(profile)) = (services.get::<Database>(), services.get::<Profile>()) else {
        return;
    };
    let embedder: Arc<dyn Embedder> = match services.get::<LocalService>() {
        Some(local) => Arc::new(LocalEmbedder {
            local,
            db: db.clone(),
        }),
        None => Arc::new(NoEmbedder),
    };
    let env = Arc::new(Env::new(
        db.clone(),
        claude_projects(),
        profile.dir.join("sessions"),
    ));
    let (tx, rx) = channel();
    let ev = events.clone();
    let indexer = Arc::new(Indexer::new(
        db.clone(),
        hub.clone(),
        env.clone(),
        embedder.clone(),
        Pacing::default(),
        Arc::new(move || ev.emit(AppEvent::new(EVENT))),
    ));
    hub.set_responder(Arc::new(Answerer { env, embedder }));
    let wake = tx.clone();
    hub.on_change(Arc::new(move || {
        let _ = wake.send(Msg::Wake);
    }));
    services.insert(Arc::new(Service {
        indexer,
        hub,
        db,
        events: events.clone(),
        tx: Mutex::new(tx),
        rx: Mutex::new(Some(rx)),
        watchers: Mutex::new(HashMap::new()),
        unwatched: Mutex::new(HashSet::new()),
    }));
}

/// `<home>/.claude/projects`, when the home folder can be found. Debug builds let a test
/// environment name another one.
fn claude_projects() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("HELM_TEST_CLAUDE_PROJECTS") {
        return Some(PathBuf::from(p));
    }
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)?;
    Some(home.join(".claude").join("projects"))
}

/// Start the indexing thread.
pub fn start(services: &Services) {
    let Some(svc) = services.get::<Service>() else {
        return;
    };
    let Some(rx) = lock(&svc.rx).take() else {
        return;
    };
    let me = svc.clone();
    let spawned = std::thread::Builder::new()
        .name("memindex".into())
        .spawn(move || me.run(rx));
    if let Err(e) = spawned {
        log::warn!("memory indexing could not start its thread: {e}");
    }
}

#[cfg(windows)]
fn below_normal() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: the pseudo handle of the calling thread is always valid.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(not(windows))]
fn below_normal() {}

impl Service {
    fn send(&self, m: Msg) {
        let _ = lock(&self.tx).send(m);
    }

    fn emit(&self) {
        self.events.emit(AppEvent::new(EVENT));
    }

    /// Ask for a pass soon.
    pub fn wake(&self) {
        self.send(Msg::Wake);
    }

    /// Index now: every source, or one. `force` lets held-back removals through.
    pub fn reindex(&self, source: Option<String>, force: bool) {
        self.send(Msg::Reindex { source, force });
    }

    pub fn stop(&self) {
        self.send(Msg::Stop);
    }

    fn run(&self, rx: Receiver<Msg>) {
        below_normal();
        let mut dirty: HashSet<String> = HashSet::new();
        let mut due: Option<Instant> = None;
        let mut next_poll = Instant::now() + POLL;
        let mut nightly_done: Option<chrono::NaiveDate> = None;
        // A first look when the thread starts: the line may already be up.
        let mut first = true;
        loop {
            let now = Instant::now();
            let wait = due
                .unwrap_or(next_poll)
                .min(next_poll)
                .saturating_duration_since(now)
                .min(Duration::from_secs(30));
            let msg = if first {
                first = false;
                Some(Msg::Wake)
            } else {
                match rx.recv_timeout(wait) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            };
            match msg {
                Some(Msg::Stop) => return,
                Some(Msg::Wake) => {
                    self.sync_watchers();
                    self.indexer.pass(None, false);
                    next_poll = Instant::now() + POLL;
                }
                Some(Msg::Dirty(id)) => {
                    dirty.insert(id);
                    due = Some(Instant::now() + DEBOUNCE);
                }
                Some(Msg::Reindex { source, force }) => {
                    self.sync_watchers();
                    let only = source.map(|s| HashSet::from([s]));
                    self.indexer.pass(only.as_ref(), force);
                }
                None => {}
            }
            let now = Instant::now();
            if due.is_some_and(|d| now >= d) {
                due = None;
                let only: HashSet<String> = std::mem::take(&mut dirty);
                self.indexer.pass(Some(&only), false);
            }
            if now >= next_poll {
                next_poll = now + POLL;
                self.poll_unwatched();
            }
            if self.nightly_due(&mut nightly_done) {
                self.indexer.full_rescan();
                self.indexer.pass(None, false);
            }
        }
    }

    /// The sources nothing is watching: every kind but a folder with a live watch.
    fn poll_unwatched(&self) {
        let sources = self.db.snapshot(store::sources).unwrap_or_default();
        let watched: HashSet<String> = lock(&self.watchers).keys().cloned().collect();
        let polled: HashSet<String> = sources
            .iter()
            .filter(|s| s.enabled && !(s.kind == Kind::Folder && watched.contains(&s.id)))
            .map(|s| s.id.clone())
            .collect();
        if !polled.is_empty() {
            self.indexer.pass(Some(&polled), false);
        }
    }

    /// Whether the nightly scan's time has come today and has not run yet.
    fn nightly_due(&self, done: &mut Option<chrono::NaiveDate>) -> bool {
        use chrono::Timelike;
        let s = self.indexer.settings();
        let Some(at) = parse_nightly(&s.nightly).filter(|_| s.enabled) else {
            return false;
        };
        let now = chrono::Local::now();
        let minutes = now.hour() * 60 + now.minute();
        if minutes >= at && *done != Some(now.date_naive()) {
            *done = Some(now.date_naive());
            return true;
        }
        false
    }

    /// Watch every enabled folder source that is not watched yet, and drop the watches of the
    /// ones that are gone or switched off. A watch that cannot start leaves the source polled.
    fn sync_watchers(&self) {
        let sources = self.db.snapshot(store::sources).unwrap_or_default();
        let wanted: HashMap<String, PathBuf> = sources
            .iter()
            .filter(|s| s.enabled && s.kind == Kind::Folder)
            .map(|s| (s.id.clone(), PathBuf::from(&s.path)))
            .collect();
        let mut watchers = lock(&self.watchers);
        watchers.retain(|id, _| wanted.contains_key(id));
        let mut unwatched = lock(&self.unwatched);
        unwatched.retain(|id| wanted.contains_key(id));
        for (id, path) in wanted {
            if watchers.contains_key(&id) || !path.is_dir() {
                continue;
            }
            let tx = lock(&self.tx).clone();
            let sid = id.clone();
            let made = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if res.is_ok() {
                    let _ = tx.send(Msg::Dirty(sid.clone()));
                }
            })
            .and_then(|mut w| w.watch(&path, RecursiveMode::Recursive).map(|()| w));
            match made {
                Ok(w) => {
                    unwatched.remove(&id);
                    watchers.insert(id, w);
                }
                Err(e) => {
                    log::info!(
                        "memory indexing: {} cannot be watched ({e}); polling it",
                        path.display()
                    );
                    unwatched.insert(id);
                }
            }
        }
    }

    // ---- what Settings and the tool see ------------------------------------------------------

    pub fn view(&self) -> StateView {
        let s = self.indexer.settings();
        let sources = self.db.snapshot(store::sources).unwrap_or_default();
        let unwatched = lock(&self.unwatched).clone();
        let hub = match self.hub.status() {
            crate::hub::HubStatus::Ready => "ready",
            crate::hub::HubStatus::Down => "down",
            crate::hub::HubStatus::TooOld => "too_old",
        };
        let note = self.indexer.note();
        let status = if !s.enabled {
            "Memory indexing is off.".to_owned()
        } else if !note.is_empty() {
            note
        } else {
            match hub {
                "too_old" => "The hub needs updating for search.".to_owned(),
                "down" => "Waiting for the hub.".to_owned(),
                _ => "Up to date.".to_owned(),
            }
        };
        StateView {
            enabled: s.enabled,
            server_id: s.server_id,
            model: s.model,
            nightly: s.nightly,
            hub: hub.to_owned(),
            status,
            pending: self.indexer.pending(),
            sources: sources
                .into_iter()
                .map(|src| {
                    let (docs, pushed) = self.indexer.local_counts(&src.id);
                    let st = self.indexer.source_state(&src.id);
                    let mut status = if !src.enabled {
                        "Switched off.".to_owned()
                    } else if st.status.is_empty() {
                        "Waiting for the first scan.".to_owned()
                    } else {
                        st.status
                    };
                    if src.enabled && unwatched.contains(&src.id) {
                        status.push_str(
                            " Checked every 2 minutes: changes cannot be watched live here.",
                        );
                    }
                    SourceView {
                        id: src.id,
                        kind: src.kind,
                        path: src.path,
                        person_id: src.person_id,
                        scope: src.scope,
                        include: src.include,
                        exclude: src.exclude,
                        enabled: src.enabled,
                        docs,
                        pushed,
                        status,
                        last_scan: st.last_scan,
                    }
                })
                .collect(),
        }
    }

    pub fn save_settings(&self, c: SettingsChanges) -> Result<(), String> {
        c.validate()?;
        let mut s: Settings = self.indexer.settings();
        if let Some(v) = c.enabled {
            s.enabled = v;
        }
        if let Some(v) = c.server_id {
            s.server_id = v.trim().to_owned();
        }
        if let Some(v) = c.model {
            s.model = v.trim().to_owned();
        }
        if let Some(v) = c.nightly {
            s.nightly = v.trim().to_owned();
        }
        if s.model.is_empty() {
            s.model = DEFAULT_MODEL.into();
        }
        self.db
            .write(|tx| store::save_settings(tx, &s))
            .map_err(|e| e.to_string())?;
        self.emit();
        self.wake();
        Ok(())
    }

    /// Check and save a source: a new one when `id` is absent. Returns its id.
    pub fn save_source(&self, input: SourceInput) -> Result<String, String> {
        let existing = self
            .db
            .snapshot(store::sources)
            .map_err(|e| e.to_string())?;
        let old = input
            .id
            .as_deref()
            .and_then(|id| existing.iter().find(|s| s.id == id).cloned());
        if input.id.is_some() && old.is_none() {
            return Err("That source is not registered.".into());
        }
        let kind = match (&old, input.kind) {
            (Some(o), _) => o.kind,
            (None, Some(k)) => k,
            (None, None) => return Err("Say what kind of source this is.".into()),
        };
        let single = matches!(kind, Kind::HelmChats | Kind::ClaudeCode);
        if old.is_none() && existing.len() >= MAX_SOURCES {
            return Err(format!("At most {MAX_SOURCES} sources can be registered."));
        }
        if old.is_none() && single && existing.iter().any(|s| s.kind == kind) {
            return Err(format!("{} is already a source.", kind.as_str()));
        }
        let path = match (kind.has_path(), input.path.as_deref().map(str::trim)) {
            (true, Some(p)) if !p.is_empty() => {
                let p = Path::new(p);
                if !p.is_absolute() {
                    return Err("The folder must be a full path.".into());
                }
                if !p.is_dir() {
                    return Err("That folder cannot be found.".into());
                }
                p.to_string_lossy().into_owned()
            }
            (true, None) if old.is_some() => {
                old.as_ref().map(|o| o.path.clone()).unwrap_or_default()
            }
            (true, _) => return Err("Choose a folder.".into()),
            (false, _) => String::new(),
        };
        if existing.iter().any(|s| {
            kind.has_path()
                && s.kind == kind
                && s.path == path
                && Some(&s.id) != old.as_ref().map(|o| &o.id)
        }) {
            return Err("That folder is already a source.".into());
        }
        let person_id = match input.person_id.as_deref().map(str::trim) {
            Some("") | None => old
                .as_ref()
                .map(|o| o.person_id.clone())
                .unwrap_or_default(),
            Some(p) => {
                let known = self
                    .db
                    .snapshot(|tx| helm_profiles::store::get(tx, p))
                    .map_err(|e| e.to_string())?;
                if known.is_none() {
                    return Err("That profile does not exist.".into());
                }
                p.to_owned()
            }
        };
        let globs =
            |v: Option<Vec<String>>, old: Option<Vec<String>>| -> Result<Vec<String>, String> {
                let list: Vec<String> = match v {
                    Some(v) => v
                        .into_iter()
                        .map(|g| g.trim().to_owned())
                        .filter(|g| !g.is_empty())
                        .collect(),
                    None => old.unwrap_or_default(),
                };
                if list.len() > 50 || list.iter().any(|g| g.len() > 200) {
                    return Err("Too many patterns, or one is too long.".into());
                }
                Ok(list)
            };
        let src = Source {
            id: old.as_ref().map_or_else(
                || {
                    if single {
                        kind.as_str().to_owned()
                    } else {
                        uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
                    }
                },
                |o| o.id.clone(),
            ),
            kind,
            path,
            person_id,
            scope: input
                .scope
                .or(old.as_ref().map(|o| o.scope))
                .unwrap_or(Scope::Me),
            include: globs(input.include, old.as_ref().map(|o| o.include.clone()))?,
            exclude: globs(input.exclude, old.as_ref().map(|o| o.exclude.clone()))?,
            enabled: input
                .enabled
                .or(old.as_ref().map(|o| o.enabled))
                .unwrap_or(true),
            created_at: old
                .as_ref()
                .map_or_else(helm_core::now_ms, |o| o.created_at),
        };
        self.db
            .write(|tx| store::save_source(tx, &src))
            .map_err(|e| e.to_string())?;
        self.emit();
        self.reindex(Some(src.id.clone()), false);
        Ok(src.id)
    }

    /// Remove a source and take its docs off the hub.
    pub fn remove_source(&self, id: &str) -> Result<(), String> {
        let known = self
            .db
            .snapshot(store::sources)
            .map_err(|e| e.to_string())?;
        if !known.iter().any(|s| s.id == id) {
            return Err("That source is not registered.".into());
        }
        self.indexer.remove_source(id)?;
        lock(&self.watchers).remove(id);
        self.emit();
        Ok(())
    }
}

/// What Settings may change of the switches; absent keeps.
#[derive(Debug, Default, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename = "MemindexSettingsChanges")]
pub struct SettingsChanges {
    pub enabled: Option<bool>,
    pub server_id: Option<String>,
    pub model: Option<String>,
    pub nightly: Option<String>,
}

impl SettingsChanges {
    /// Whether the names and times in the change are well formed. Checked before a grant is
    /// spent on a change that would be refused anyway.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(v) = &self.model {
            let v = v.trim();
            let ok = !v.is_empty()
                && v.len() <= 100
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-_:/".contains(c));
            if !ok {
                return Err("The model name may hold letters, digits and . - _ : / only.".into());
            }
        }
        if let Some(v) = &self.nightly {
            if parse_nightly(v.trim()).is_none() {
                return Err("The nightly scan time is HH:MM, like 03:30.".into());
            }
        }
        Ok(())
    }
}

/// A source to add (no `id`) or change (`id`); absent keeps.
#[derive(Debug, Default, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename = "MemindexSourceInput")]
pub struct SourceInput {
    pub id: Option<String>,
    pub kind: Option<Kind>,
    pub path: Option<String>,
    pub person_id: Option<String>,
    pub scope: Option<Scope>,
    pub include: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
    pub enabled: Option<bool>,
}
