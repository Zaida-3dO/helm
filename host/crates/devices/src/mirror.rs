//! The watcher side: this PC's copy of the chats other devices run.
//!
//! A [`Mirror`] hears the hub line. For every device of the household that is up it asks `hello`
//! (who uses it, its projects and models), subscribes to its list of chats, and, for the chats
//! opened here, to their rows. Everything heard is kept in tables of its own (`devices`,
//! `mirror_projects`, `mirror_chats`, `mirror_rows`, `mirror_cursor`) and never in `chats`, so the
//! rest of the app cannot mistake a copy for a chat that runs here.
//!
//! - **Catch-up is by revision.** A row is applied only when its revision is newer than the one
//!   held; the list resumes from a cursor; a chat resumes from the newest row revision held. A
//!   gap in a subscription's `seq` subscribes again from what is held, and `refresh` drops the
//!   chat's rows and starts over.
//! - **News is applied in order.** Everything that changes the tables goes through one task, so a
//!   subscription's result is always applied before the news that came behind it (the line says
//!   where the result fell, see [`crate::link::Rpc::call_then`]).
//! - **The window hears the same events** a chat that runs here would raise (`chat`, `thread`,
//!   `message`, `pending`, `refresh`, `chat_removed`), for the chats opened here; the list as a
//!   whole raises `devices_chats_changed`, and the set of devices or their state `devices_changed`.
//!
//! The ops the window uses for these chats are answered in [`crate::route`].

use crate::identity;
use crate::link::{DeviceLink, Rpc};
use crate::wire::Frame;
use crate::wire::{PeerDevice, PersonInfo, Push, WireError};
use helm_core::db::Database;
use helm_ipc::{AppEvent, EventSink};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

/// The newest rows kept for one chat; older ones are read from the device that runs it.
pub const MAX_ROWS: i64 = 3000;
/// Rows of a chat not opened for this long are dropped; its entry in the list stays.
pub const UNOPENED_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// The most chats opened at once on one device (the device holds no more for one watcher).
pub const MAX_OPEN_PER_DEVICE: usize = 16;
/// Rows in one transcript page.
pub const PAGE_ROWS: i64 = 100;
/// How long the list of chats may go unannounced after a change.
const LIST_EVENT_DELAY: Duration = Duration::from_millis(300);
/// How often a failed sync of a device is tried again, and how long it waits.
const SYNC_RETRIES: u32 = 3;
const SYNC_RETRY_DELAY: Duration = Duration::from_secs(5);

/// The error code of a request whose line went away after it was sent.
pub const UNCONFIRMED: &str = "unconfirmed";

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS devices (
             uid           TEXT PRIMARY KEY,
             name          TEXT NOT NULL,
             person_id     TEXT,
             person        TEXT,
             online        INTEGER NOT NULL DEFAULT 0,
             last_seen_at  TEXT,
             run           INTEGER NOT NULL DEFAULT 0,
             is_self       INTEGER NOT NULL DEFAULT 0,
             hello         TEXT,
             hello_at      INTEGER
         );
         CREATE TABLE IF NOT EXISTS mirror_projects (
             device_uid  TEXT NOT NULL,
             project_id  TEXT NOT NULL,
             name        TEXT NOT NULL,
             colour      TEXT NOT NULL DEFAULT '',
             kind        TEXT NOT NULL DEFAULT 'folder',
             path        TEXT NOT NULL DEFAULT '',
             PRIMARY KEY (device_uid, project_id)
         );
         CREATE TABLE IF NOT EXISTS mirror_chats (
             chat_id         TEXT PRIMARY KEY,
             device_uid      TEXT NOT NULL,
             record          TEXT NOT NULL,
             person_id       TEXT,
             shared_with     TEXT NOT NULL DEFAULT '[]',
             revision        INTEGER NOT NULL DEFAULT 0,
             watched_at      INTEGER,
             draft           TEXT NOT NULL DEFAULT '',
             draft_revision  INTEGER NOT NULL DEFAULT 0,
             thread          TEXT,
             pending         TEXT,
             older           INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS mirror_chats_device ON mirror_chats(device_uid, revision);
         CREATE TABLE IF NOT EXISTS mirror_rows (
             chat_id   TEXT NOT NULL,
             row_id    INTEGER NOT NULL,
             revision  INTEGER NOT NULL,
             ts        INTEGER NOT NULL DEFAULT 0,
             row       TEXT NOT NULL,
             PRIMARY KEY (chat_id, row_id)
         );
         CREATE INDEX IF NOT EXISTS mirror_rows_order ON mirror_rows(chat_id, ts, row_id);
         CREATE TABLE IF NOT EXISTS mirror_cursor (
             device_uid    TEXT PRIMARY KEY,
             chats_cursor  INTEGER NOT NULL DEFAULT 0
         );",
    )
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// How this PC turns hub people into profiles (the profiles crate supplies it): creates the ones
/// that are missing, links the ones that match, never deletes. `true` when anything changed.
pub struct PeopleSync(pub Arc<dyn Fn(&Database, &[PersonInfo]) -> bool + Send + Sync>);

impl PeopleSync {
    pub fn new(f: impl Fn(&Database, &[PersonInfo]) -> bool + Send + Sync + 'static) -> PeopleSync {
        PeopleSync(Arc::new(f))
    }
}

/// Where the app finds the running mirror once the hub line exists.
#[derive(Default)]
pub struct MirrorSlot(OnceLock<Mirror>);

impl MirrorSlot {
    pub fn attach(&self, m: Mirror) {
        let _ = self.0.set(m);
    }

    pub fn get(&self) -> Option<Mirror> {
        self.0.get().cloned()
    }
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    List(String),
    Chat(String),
}

enum Phase {
    /// The request is out; news that comes before its result belongs to an older subscription.
    Waiting,
    /// The result has come down the line; its news is kept until the result is applied.
    Arrived(Vec<Push>),
    /// The result is applied; the next news should carry this `seq`.
    Live { next: u64 },
}

struct Feed {
    device: String,
    phase: Phase,
}

#[derive(Default)]
struct State {
    devices: HashMap<String, PeerDevice>,
    /// Mirror chat id → the device that runs it.
    owners: HashMap<String, String>,
    feeds: HashMap<Key, Feed>,
    /// Chats opened here, oldest first, with their device.
    open: Vec<(String, String)>,
    syncing: HashSet<String>,
    /// Bumped whenever the line drops: work begun under an older one is thrown away.
    epoch: u64,
    list_dirty: HashSet<String>,
}

enum Work {
    Devices(Vec<PeerDevice>),
    Push(Key, Push),
    List {
        device: String,
        epoch: u64,
        since: i64,
        result: Value,
        done: oneshot::Sender<()>,
    },
    Chat {
        device: String,
        chat_id: String,
        epoch: u64,
        since: i64,
        result: Value,
        done: oneshot::Sender<()>,
    },
}

pub(crate) struct Inner {
    pub(crate) db: Arc<Database>,
    pub(crate) rpc: Arc<Rpc>,
    link: Arc<dyn DeviceLink>,
    events: Arc<dyn EventSink>,
    handle: Handle,
    people: Option<Arc<PeopleSync>>,
    pub(crate) self_uid: String,
    st: Mutex<State>,
    work: mpsc::UnboundedSender<Work>,
}

/// This PC's copy of the other devices' chats, and the traffic that keeps it current.
#[derive(Clone)]
pub struct Mirror {
    pub(crate) inner: Arc<Inner>,
}

impl Mirror {
    /// Start watching: hear the link, load what an earlier run kept, and start the task that
    /// applies news.
    pub fn new(
        db: Arc<Database>,
        link: Arc<dyn DeviceLink>,
        events: Arc<dyn EventSink>,
        handle: Handle,
        people: Option<Arc<PeopleSync>>,
    ) -> Mirror {
        let self_uid = db
            .write(identity::ensure)
            .map(|(uid, _)| uid)
            .unwrap_or_default();
        let (work, rx) = mpsc::unbounded_channel();
        let rpc = Rpc::new(link.clone());
        let mut st = State::default();
        let _ = db.snapshot(|tx| {
            for d in load_devices(tx)? {
                st.devices.insert(d.uid.clone(), d);
            }
            let mut q = tx.prepare("SELECT chat_id, device_uid FROM mirror_chats")?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for (chat, device) in rows.flatten() {
                st.owners.insert(chat, device);
            }
            Ok::<_, rusqlite::Error>(())
        });
        let inner = Arc::new(Inner {
            db,
            rpc,
            link: link.clone(),
            events,
            handle: handle.clone(),
            people,
            self_uid,
            st: Mutex::new(st),
            work,
        });
        if let Err(e) = inner.db.write(|tx| prune_unopened(tx, helm_core::now_ms())) {
            log::warn!("devices: mirror rows were not pruned: {e}");
        }
        let weak = Arc::downgrade(&inner);
        link.subscribe(Box::new(move |f| {
            let Some(i) = weak.upgrade() else { return };
            match f {
                Frame::Devices { list } => {
                    let _ = i.work.send(Work::Devices(list));
                }
                Frame::DevPush(p) => i.on_push(p),
                _ => {}
            }
        }));
        handle.spawn(apply_loop(Arc::downgrade(&inner), rx));
        Mirror { inner }
    }

    /// The line to the hub came up. The `devices` frame that follows starts the syncs.
    pub fn line_up(&self) {
        self.inner.emit(AppEvent::new("devices_changed"));
    }

    /// The line to the hub went down: every request still waiting fails, and every subscription
    /// is forgotten (they are made again when the line is back and a device shows as up).
    pub fn line_down(&self) {
        {
            let mut st = lock(&self.inner.st);
            st.epoch += 1;
            st.feeds.clear();
            st.syncing.clear();
        }
        self.inner.rpc.fail_all(WireError::new(
            UNCONFIRMED,
            "The hub line dropped before the answer came.",
        ));
        self.inner.emit(AppEvent::new("devices_changed"));
    }

    pub fn line_is_up(&self) -> bool {
        self.inner.link.up()
    }

    /// The device that runs a mirrored chat.
    pub fn owner(&self, chat_id: &str) -> Option<String> {
        lock(&self.inner.st).owners.get(chat_id).cloned()
    }

    /// What the hub last said about a device.
    pub fn device(&self, uid: &str) -> Option<PeerDevice> {
        lock(&self.inner.st).devices.get(uid).cloned()
    }

    pub fn device_name(&self, uid: &str) -> String {
        self.device(uid)
            .map(|d| d.name)
            .unwrap_or_else(|| uid.to_owned())
    }

    /// Whether anything is subscribed to a chat right now.
    pub fn is_open(&self, chat_id: &str) -> bool {
        lock(&self.inner.st)
            .feeds
            .contains_key(&Key::Chat(chat_id.to_owned()))
    }

    /// Open a chat: remember it as open, and subscribe to its rows when the device is up. Waits
    /// for the first rows. Reading the copy works whatever this answers.
    pub async fn open_chat(&self, chat_id: &str) -> Result<(), WireError> {
        let Some(device) = self.owner(chat_id) else {
            return Err(WireError::not_found());
        };
        self.inner.open(&device, chat_id, true).await
    }

    /// As [`Mirror::open_chat`] without waiting: the rows arrive as events.
    pub fn open_chat_later(&self, chat_id: &str) {
        let Some(device) = self.owner(chat_id) else {
            return;
        };
        let i = self.inner.clone();
        let chat = chat_id.to_owned();
        self.inner.handle.spawn(async move {
            let _ = i.open(&device, &chat, true).await;
        });
    }

    /// A chat this device just made on `device`: open it so its first rows arrive.
    pub async fn open_new_chat(&self, device: &str, chat_id: &str) -> Result<(), WireError> {
        self.inner.open(device, chat_id, true).await
    }

    /// Change the stored record of a chat (a title, an archive flag) after the device that runs
    /// it accepted the change; the device's own news, with a newer revision, replaces it.
    pub fn patch_record(&self, chat_id: &str, patch: impl FnOnce(&mut Map<String, Value>)) {
        let done = self.inner.db.write(|tx| {
            let rec: Option<String> = tx
                .query_row(
                    "SELECT record FROM mirror_chats WHERE chat_id = ?1",
                    [chat_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(Ok(Value::Object(mut m))) = rec.map(|r| serde_json::from_str::<Value>(&r))
            else {
                return Ok::<_, rusqlite::Error>(false);
            };
            patch(&mut m);
            tx.execute(
                "UPDATE mirror_chats SET record = ?2 WHERE chat_id = ?1",
                params![chat_id, Value::Object(m).to_string()],
            )?;
            Ok(true)
        });
        if matches!(done, Ok(true)) {
            self.inner.emit_chat(chat_id);
        }
    }

    pub(crate) fn emit_chat(&self, chat_id: &str) {
        self.inner.emit_chat(chat_id);
    }
}

fn load_devices(conn: &Connection) -> rusqlite::Result<Vec<PeerDevice>> {
    let mut q = conn.prepare(
        "SELECT uid, name, person_id, person, online, last_seen_at, run, is_self FROM devices",
    )?;
    let rows = q.query_map([], |r| {
        Ok(PeerDevice {
            uid: r.get(0)?,
            name: r.get(1)?,
            person_id: r.get(2)?,
            person: r.get(3)?,
            online: r.get::<_, i64>(4)? != 0,
            last_seen_at: r.get(5)?,
            run: r.get::<_, i64>(6)? != 0,
            is_self: r.get::<_, i64>(7)? != 0,
        })
    })?;
    rows.collect()
}

/// Drop the rows of chats not opened for 30 days; the entries of the list stay.
pub fn prune_unopened(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<()> {
    let cut = now - UNOPENED_MS;
    tx.execute(
        "DELETE FROM mirror_rows WHERE chat_id IN (
             SELECT chat_id FROM mirror_chats WHERE COALESCE(watched_at, 0) < ?1)",
        [cut],
    )?;
    tx.execute(
        "UPDATE mirror_chats SET older = 1, thread = NULL, pending = NULL
         WHERE COALESCE(watched_at, 0) < ?1 AND watched_at IS NOT NULL",
        [cut],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The task that applies news
// ---------------------------------------------------------------------------------------------

async fn apply_loop(inner: Weak<Inner>, mut rx: mpsc::UnboundedReceiver<Work>) {
    while let Some(w) = rx.recv().await {
        let Some(i) = inner.upgrade() else { return };
        match w {
            Work::Devices(list) => i.apply_devices(list),
            Work::Push(key, p) => {
                if i.apply_push(&key, &p) {
                    i.restart(&key);
                }
            }
            Work::List {
                device,
                epoch,
                since,
                result,
                done,
            } => {
                i.apply_list(&device, epoch, since, &result);
                let _ = done.send(());
            }
            Work::Chat {
                device,
                chat_id,
                epoch,
                since,
                result,
                done,
            } => {
                i.apply_chat(&device, &chat_id, epoch, since, &result);
                let _ = done.send(());
            }
        }
    }
}

impl Inner {
    pub(crate) fn emit(&self, ev: AppEvent) {
        self.events.emit(ev);
    }

    fn epoch(&self) -> u64 {
        lock(&self.st).epoch
    }

    /// Whether the device is up and this PC's line is too.
    pub(crate) fn device_up(&self, uid: &str) -> bool {
        self.link.up() && lock(&self.st).devices.get(uid).is_some_and(|d| d.online)
    }

    fn watched(&self, chat_id: &str) -> bool {
        lock(&self.st)
            .feeds
            .contains_key(&Key::Chat(chat_id.to_owned()))
    }

    pub(crate) fn emit_chat(&self, chat_id: &str) {
        if let Ok(Some(chat)) = self.db.snapshot(|tx| chat_value(tx, chat_id)) {
            self.emit(AppEvent::new("chat").with("chat", chat));
        }
    }

    /// The list of a device's chats changed. Said at most once in a little while.
    fn list_changed(self: &Arc<Self>, device: &str) {
        if !lock(&self.st).list_dirty.insert(device.to_owned()) {
            return;
        }
        let i = self.clone();
        let device = device.to_owned();
        self.handle.spawn(async move {
            tokio::time::sleep(LIST_EVENT_DELAY).await;
            lock(&i.st).list_dirty.remove(&device);
            i.emit(AppEvent::new("devices_chats_changed").with("deviceUid", device));
        });
    }

    // -------------------------------------------------------------------------- the hub's list

    fn apply_devices(self: &Arc<Self>, list: Vec<PeerDevice>) {
        let changed = {
            let mut st = lock(&self.st);
            let mut next: HashMap<String, PeerDevice> = HashMap::new();
            for d in &list {
                next.insert(d.uid.clone(), d.clone());
            }
            let changed = next != st.devices;
            st.devices = next;
            changed
        };
        let stored = self.db.write(|tx| {
            for d in &list {
                tx.execute(
                    "INSERT INTO devices(uid, name, person_id, person, online, last_seen_at, run, is_self)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(uid) DO UPDATE SET name = excluded.name,
                         person_id = excluded.person_id, person = excluded.person,
                         online = excluded.online, last_seen_at = excluded.last_seen_at,
                         run = excluded.run, is_self = excluded.is_self",
                    params![
                        d.uid,
                        d.name,
                        d.person_id,
                        d.person,
                        d.online,
                        d.last_seen_at,
                        d.run,
                        d.is_self
                    ],
                )?;
            }
            let known: Vec<String> = {
                let mut q = tx.prepare("SELECT uid FROM devices")?;
                let rows = q.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for uid in known {
                if !list.iter().any(|d| d.uid == uid) {
                    tx.execute("DELETE FROM devices WHERE uid = ?1", [&uid])?;
                }
            }
            Ok::<_, rusqlite::Error>(())
        });
        if let Err(e) = stored {
            log::warn!("devices: the device list was not stored: {e}");
        }
        // Everyone the hub names is a person with a profile here.
        let mut seen = HashSet::new();
        let people: Vec<PersonInfo> = list
            .iter()
            .filter_map(|d| Some((d.person_id.clone()?, d.person.clone()?)))
            .filter(|(id, _)| seen.insert(id.clone()))
            .map(|(person_id, name)| PersonInfo {
                person_id,
                name,
                colour: String::new(),
                about: String::new(),
            })
            .collect();
        self.sync_people(&people);
        for d in &list {
            if d.uid == self.self_uid || d.is_self {
                continue;
            }
            if !d.online {
                self.forget_feeds(&d.uid);
            } else {
                self.sync_soon(&d.uid, 0);
            }
        }
        if changed {
            self.emit(AppEvent::new("devices_changed"));
        }
    }

    fn sync_people(&self, people: &[PersonInfo]) {
        if people.is_empty() {
            return;
        }
        if let Some(p) = &self.people {
            if (p.0)(&self.db, people) {
                self.emit(AppEvent::new("profiles_changed"));
            }
        }
    }

    /// A device went offline: its subscriptions are gone with it. Chats opened stay opened, and
    /// are subscribed again when it is back.
    fn forget_feeds(&self, device: &str) {
        let mut st = lock(&self.st);
        st.feeds.retain(|_, f| f.device != device);
        st.syncing.remove(device);
    }

    /// Start syncing a device that shows as up and is not subscribed yet.
    fn sync_soon(self: &Arc<Self>, device: &str, attempt: u32) {
        {
            let mut st = lock(&self.st);
            if st.feeds.contains_key(&Key::List(device.to_owned()))
                || !st.syncing.insert(device.to_owned())
            {
                return;
            }
        }
        let i = self.clone();
        let device = device.to_owned();
        self.handle.spawn(async move {
            let epoch = i.epoch();
            let r = i.sync_device(&device).await;
            {
                let mut st = lock(&i.st);
                if st.epoch == epoch {
                    st.syncing.remove(&device);
                }
            }
            if let Err(e) = r {
                log::info!("devices: syncing {device} failed: {e}");
                if attempt < SYNC_RETRIES && i.epoch() == epoch && i.device_up(&device) {
                    tokio::time::sleep(SYNC_RETRY_DELAY).await;
                    if i.epoch() == epoch {
                        i.sync_soon(&device, attempt + 1);
                    }
                }
            }
        });
    }

    async fn sync_device(self: &Arc<Self>, device: &str) -> Result<(), WireError> {
        let hello = self.rpc.call(device, "hello", json!({})).await?;
        self.apply_hello(device, &hello);
        self.sub_list(device).await?;
        let reopen: Vec<String> = lock(&self.st)
            .open
            .iter()
            .filter(|(_, d)| d == device)
            .map(|(c, _)| c.clone())
            .collect();
        for chat in reopen {
            if let Err(e) = self.sub_chat(device, &chat, false, false).await {
                log::info!("devices: reopening {chat} failed: {e}");
            }
        }
        Ok(())
    }

    fn apply_hello(&self, device: &str, hello: &Value) {
        let people: Vec<PersonInfo> = hello
            .get("people")
            .and_then(|p| serde_json::from_value(p.clone()).ok())
            .unwrap_or_default();
        let projects = hello
            .get("projects")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let stored = self.db.write(|tx| {
            tx.execute(
                "UPDATE devices SET hello = ?2, hello_at = ?3 WHERE uid = ?1",
                params![device, hello.to_string(), helm_core::now_ms()],
            )?;
            tx.execute("DELETE FROM mirror_projects WHERE device_uid = ?1", [device])?;
            for p in &projects {
                let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default();
                if s("id").is_empty() {
                    continue;
                }
                tx.execute(
                    "INSERT OR REPLACE INTO mirror_projects(device_uid, project_id, name, colour, kind, path)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![device, s("id"), s("name"), s("colour"), s("kind"), s("path")],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        });
        if let Err(e) = stored {
            log::warn!("devices: a hello was not stored: {e}");
        }
        self.sync_people(&people);
        self.emit(AppEvent::new("devices_changed"));
    }

    // -------------------------------------------------------------------------- subscriptions

    /// Mark the place in the stream where a subscription's result fell.
    fn arrived(&self, key: &Key) {
        if let Some(f) = lock(&self.st).feeds.get_mut(key) {
            if matches!(f.phase, Phase::Waiting) {
                f.phase = Phase::Arrived(Vec::new());
            }
        }
    }

    fn start_feed(&self, key: &Key, device: &str) -> u64 {
        let mut st = lock(&self.st);
        st.feeds.insert(
            key.clone(),
            Feed {
                device: device.to_owned(),
                phase: Phase::Waiting,
            },
        );
        st.epoch
    }

    fn end_feed(&self, key: &Key, epoch: u64) {
        let mut st = lock(&self.st);
        if st.epoch == epoch {
            st.feeds.remove(key);
        }
    }

    async fn sub_list(self: &Arc<Self>, device: &str) -> Result<(), WireError> {
        let key = Key::List(device.to_owned());
        let epoch = self.start_feed(&key, device);
        let since = self
            .db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT chats_cursor FROM mirror_cursor WHERE device_uid = ?1",
                    [device],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
            })
            .ok()
            .flatten()
            .unwrap_or(0);
        let me = self.clone();
        let k = key.clone();
        let r = self
            .rpc
            .call_then(device, "chats_sub", json!({ "since": since }), move || {
                me.arrived(&k)
            })
            .await;
        match r {
            Ok(result) => {
                let (done, wait) = oneshot::channel();
                let _ = self.work.send(Work::List {
                    device: device.to_owned(),
                    epoch,
                    since,
                    result,
                    done,
                });
                let _ = wait.await;
                Ok(())
            }
            Err(e) => {
                self.end_feed(&key, epoch);
                Err(e)
            }
        }
    }

    /// Subscribe to a chat's rows from the newest revision held (from the start when
    /// `from_start`). A chat already subscribed is left alone unless `again`.
    async fn sub_chat(
        self: &Arc<Self>,
        device: &str,
        chat_id: &str,
        from_start: bool,
        again: bool,
    ) -> Result<(), WireError> {
        let key = Key::Chat(chat_id.to_owned());
        if !again && lock(&self.st).feeds.contains_key(&key) {
            return Ok(());
        }
        let epoch = self.start_feed(&key, device);
        let since = if from_start {
            0
        } else {
            self.db
                .snapshot(|tx| {
                    tx.query_row(
                        "SELECT COALESCE(MAX(revision), 0) FROM mirror_rows WHERE chat_id = ?1",
                        [chat_id],
                        |r| r.get::<_, i64>(0),
                    )
                })
                .unwrap_or(0)
        };
        let me = self.clone();
        let k = key.clone();
        let r = self
            .rpc
            .call_then(
                device,
                "chat_sub",
                json!({ "chatId": chat_id, "since": since }),
                move || me.arrived(&k),
            )
            .await;
        match r {
            Ok(result) => {
                let (done, wait) = oneshot::channel();
                let _ = self.work.send(Work::Chat {
                    device: device.to_owned(),
                    chat_id: chat_id.to_owned(),
                    epoch,
                    since,
                    result,
                    done,
                });
                let _ = wait.await;
                Ok(())
            }
            Err(e) => {
                self.end_feed(&key, epoch);
                Err(e)
            }
        }
    }

    /// Subscribe again from what is held (or from the start), in the background. The feed must
    /// already be there, waiting.
    fn resubscribe(self: &Arc<Self>, key: &Key, from_start: bool) {
        let device = match lock(&self.st).feeds.get(key) {
            Some(f) => f.device.clone(),
            None => return,
        };
        let i = self.clone();
        let key = key.clone();
        self.handle.spawn(async move {
            let r = match &key {
                Key::List(d) => i.sub_list(d).await,
                Key::Chat(c) => i.sub_chat(&device, c, from_start, true).await,
            };
            if let Err(e) = r {
                log::info!("devices: subscribing again failed: {e}");
            }
        });
    }

    /// A chat is opened here. Remember it (the oldest beyond the limit is let go), and subscribe
    /// to its rows when the device is up.
    pub(crate) async fn open(
        self: &Arc<Self>,
        device: &str,
        chat_id: &str,
        wait: bool,
    ) -> Result<(), WireError> {
        let evicted = {
            let mut st = lock(&self.st);
            st.open.retain(|(c, _)| c != chat_id);
            st.open.push((chat_id.to_owned(), device.to_owned()));
            let mut out = Vec::new();
            while st.open.iter().filter(|(_, d)| d == device).count() > MAX_OPEN_PER_DEVICE {
                if let Some(at) = st.open.iter().position(|(_, d)| d == device) {
                    let (c, _) = st.open.remove(at);
                    st.feeds.remove(&Key::Chat(c.clone()));
                    out.push(c);
                }
            }
            out
        };
        for c in evicted {
            let i = self.clone();
            let device = device.to_owned();
            self.handle.spawn(async move {
                let _ = i
                    .rpc
                    .call(&device, "chat_unsub", json!({ "chatId": c }))
                    .await;
            });
        }
        if !self.device_up(device) {
            return Ok(());
        }
        if wait {
            self.sub_chat(device, chat_id, false, false).await
        } else {
            let i = self.clone();
            let (device, chat_id) = (device.to_owned(), chat_id.to_owned());
            self.handle.spawn(async move {
                let _ = i.sub_chat(&device, &chat_id, false, false).await;
            });
            Ok(())
        }
    }

    // -------------------------------------------------------------------------- news

    fn on_push(self: &Arc<Self>, p: Push) {
        let Some(from) = p.from.clone() else { return };
        let key = if p.sub == "chats" {
            Key::List(from.clone())
        } else if let Some(c) = p.sub.strip_prefix("chat:") {
            Key::Chat(c.to_owned())
        } else {
            return;
        };
        enum Act {
            Nothing,
            Apply,
            Gap,
        }
        let act = {
            let mut st = lock(&self.st);
            match st.feeds.get_mut(&key).filter(|f| f.device == from) {
                None => Act::Nothing,
                Some(f) => {
                    let act = match &mut f.phase {
                        // News from before the result belongs to a subscription that was
                        // replaced.
                        Phase::Waiting => Act::Nothing,
                        Phase::Arrived(buf) => {
                            buf.push(p.clone());
                            Act::Nothing
                        }
                        Phase::Live { next } => {
                            if p.seq == *next || p.seq == 1 {
                                *next = p.seq + 1;
                                Act::Apply
                            } else if p.seq < *next {
                                Act::Nothing
                            } else {
                                Act::Gap
                            }
                        }
                    };
                    if matches!(act, Act::Gap) {
                        f.phase = Phase::Waiting;
                    }
                    act
                }
            }
        };
        match act {
            Act::Nothing => {}
            Act::Apply => {
                let _ = self.work.send(Work::Push(key, p));
            }
            Act::Gap => self.resubscribe(&key, false),
        }
    }

    /// Apply the news that waited for a result, then go live. News that says to start over (or
    /// that skips ahead) sends the subscription back to waiting for a new result.
    fn go_live(self: &Arc<Self>, key: &Key) {
        let mut expected = 1u64;
        loop {
            let buf = {
                let mut st = lock(&self.st);
                let Some(feed) = st.feeds.get_mut(key) else {
                    return;
                };
                let buf = match &mut feed.phase {
                    Phase::Arrived(b) => std::mem::take(b),
                    _ => return,
                };
                if buf.is_empty() {
                    feed.phase = Phase::Live { next: expected };
                    return;
                }
                buf
            };
            for p in buf {
                if p.seq == expected {
                    expected += 1;
                    if self.apply_push(key, &p) {
                        self.restart(key);
                        return;
                    }
                } else if p.seq > expected {
                    self.restart(key);
                    return;
                }
            }
        }
    }

    fn restart(self: &Arc<Self>, key: &Key) {
        if let Some(f) = lock(&self.st).feeds.get_mut(key) {
            f.phase = Phase::Waiting;
        }
        self.resubscribe(key, true);
    }

    /// Apply one piece of news. `true` when the chat must be read again from the start.
    fn apply_push(self: &Arc<Self>, key: &Key, p: &Push) -> bool {
        let kind = p
            .event
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match key {
            Key::List(device) => {
                match kind {
                    "chat" => {
                        if let Some(chat) = p.event.get("chat") {
                            self.apply_chats(device, std::slice::from_ref(chat), None);
                        }
                    }
                    "chat_removed" => {
                        if let Some(id) = p.event.get("chatId").and_then(Value::as_str) {
                            self.remove_chat(device, id);
                        }
                    }
                    _ => {}
                }
                false
            }
            Key::Chat(chat_id) => self.apply_chat_news(chat_id, kind, &p.event),
        }
    }

    fn remove_chat(self: &Arc<Self>, device: &str, chat_id: &str) {
        let owned = lock(&self.st)
            .owners
            .get(chat_id)
            .is_some_and(|d| d == device);
        if !owned {
            return;
        }
        let gone = self.db.write(|tx| {
            tx.execute("DELETE FROM mirror_rows WHERE chat_id = ?1", [chat_id])?;
            tx.execute("DELETE FROM mirror_chats WHERE chat_id = ?1", [chat_id])?;
            Ok::<_, rusqlite::Error>(())
        });
        if gone.is_ok() {
            {
                let mut st = lock(&self.st);
                st.owners.remove(chat_id);
                st.open.retain(|(c, _)| c != chat_id);
                st.feeds.remove(&Key::Chat(chat_id.to_owned()));
            }
            self.emit(AppEvent::new("chat_removed").with("chatId", chat_id));
            self.list_changed(device);
        }
    }

    /// Store chats from a device's list. `full`: the complete list (a first subscription), so a
    /// chat of the device that is not in it is gone.
    fn apply_chats(self: &Arc<Self>, device: &str, chats: &[Value], full: Option<i64>) {
        let self_uid = self.self_uid.clone();
        let stored = self.db.write(|tx| {
            let mut changed = Vec::new();
            let mut ids = HashSet::new();
            for c in chats {
                if let Some(id) = upsert_chat(tx, device, c, &self_uid)? {
                    ids.insert(id.0.clone());
                    if id.1 {
                        changed.push(id.0);
                    }
                }
            }
            if let Some(cursor) = full {
                let have: Vec<String> = {
                    let mut q = tx.prepare("SELECT chat_id FROM mirror_chats WHERE device_uid = ?1")?;
                    let rows = q.query_map([device], |r| r.get(0))?;
                    rows.collect::<rusqlite::Result<_>>()?
                };
                for id in have {
                    if !ids.contains(&id) {
                        tx.execute("DELETE FROM mirror_rows WHERE chat_id = ?1", [&id])?;
                        tx.execute("DELETE FROM mirror_chats WHERE chat_id = ?1", [&id])?;
                    }
                }
                tx.execute(
                    "INSERT INTO mirror_cursor(device_uid, chats_cursor) VALUES (?1, ?2)
                     ON CONFLICT(device_uid) DO UPDATE SET chats_cursor = MAX(chats_cursor, excluded.chats_cursor)",
                    params![device, cursor],
                )?;
            } else {
                // The cursor follows the news so a later catch-up starts near it.
                let newest = chats
                    .iter()
                    .filter_map(|c| c.get("revision").and_then(Value::as_i64))
                    .max()
                    .unwrap_or(0);
                tx.execute(
                    "UPDATE mirror_cursor SET chats_cursor = MAX(chats_cursor, ?2) WHERE device_uid = ?1",
                    params![device, newest],
                )?;
            }
            Ok::<_, rusqlite::Error>((ids, changed))
        });
        match stored {
            Ok((ids, changed)) => {
                {
                    let mut st = lock(&self.st);
                    if full.is_some() {
                        st.owners
                            .retain(|c, d| d.as_str() != device || ids.contains(c));
                    }
                    for id in &ids {
                        st.owners.insert(id.clone(), device.to_owned());
                    }
                }
                for id in changed {
                    if self.watched(&id) {
                        self.emit_chat(&id);
                    }
                }
                self.list_changed(device);
            }
            Err(e) => log::warn!("devices: chats of {device} were not stored: {e}"),
        }
    }

    fn apply_list(self: &Arc<Self>, device: &str, epoch: u64, since: i64, result: &Value) {
        let key = Key::List(device.to_owned());
        {
            let st = lock(&self.st);
            let live = st.epoch == epoch
                && st
                    .feeds
                    .get(&key)
                    .is_some_and(|f| matches!(f.phase, Phase::Arrived(_)));
            if !live {
                return;
            }
        }
        let chats: Vec<Value> = result
            .get("chats")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let cursor = result.get("cursor").and_then(Value::as_i64).unwrap_or(0);
        // A list from the start is the whole truth; a catch-up only adds and changes.
        let whole = since == 0;
        self.apply_chats(device, &chats, whole.then_some(cursor));
        if !whole {
            let _ = self.db.write(|tx| {
                tx.execute(
                    "INSERT INTO mirror_cursor(device_uid, chats_cursor) VALUES (?1, ?2)
                     ON CONFLICT(device_uid) DO UPDATE SET chats_cursor = MAX(chats_cursor, excluded.chats_cursor)",
                    params![device, cursor],
                )
            });
        }
        self.go_live(&key);
    }

    fn apply_chat(
        self: &Arc<Self>,
        device: &str,
        chat_id: &str,
        epoch: u64,
        since: i64,
        result: &Value,
    ) {
        let key = Key::Chat(chat_id.to_owned());
        {
            let st = lock(&self.st);
            let live = st.epoch == epoch
                && st
                    .feeds
                    .get(&key)
                    .is_some_and(|f| matches!(f.phase, Phase::Arrived(_)));
            if !live {
                return;
            }
        }
        let rows: Vec<Value> = result
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let more = result.get("more").and_then(Value::as_bool).unwrap_or(false);
        let thread = result.get("thread").cloned().unwrap_or(Value::Null);
        let pending = result.get("pending").cloned().unwrap_or(Value::Null);
        let self_uid = self.self_uid.clone();
        let stored = self.db.write(|tx| {
            if let Some(chat) = result.get("chat") {
                upsert_chat(tx, device, chat, &self_uid)?;
            }
            let known: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM mirror_chats WHERE chat_id = ?1)",
                [chat_id],
                |r| r.get(0),
            )?;
            if !known {
                return Ok((false, false, Vec::new()));
            }
            tx.execute(
                "UPDATE mirror_chats SET thread = ?2, pending = ?3, watched_at = ?4 WHERE chat_id = ?1",
                params![chat_id, thread.to_string(), pending.to_string(), helm_core::now_ms()],
            )?;
            // More news than one answer holds, on top of rows already held: there would be a
            // hole in the middle, so start again from nothing.
            if more && since > 0 {
                tx.execute("DELETE FROM mirror_rows WHERE chat_id = ?1", [chat_id])?;
                return Ok::<_, rusqlite::Error>((true, true, Vec::new()));
            }
            let applied = upsert_rows(tx, chat_id, &rows)?;
            if since == 0 {
                tx.execute(
                    "UPDATE mirror_chats SET older = ?2 WHERE chat_id = ?1",
                    params![chat_id, more],
                )?;
            }
            cap_rows(tx, chat_id)?;
            Ok((true, false, applied))
        });
        let (known, again, applied) = match stored {
            Ok(v) => v,
            Err(e) => {
                log::warn!("devices: chat {chat_id} was not stored: {e}");
                self.end_feed(&key, epoch);
                return;
            }
        };
        if !known {
            self.end_feed(&key, epoch);
            return;
        }
        lock(&self.st)
            .owners
            .insert(chat_id.to_owned(), device.to_owned());
        self.emit_chat(chat_id);
        self.emit(
            AppEvent::new("thread")
                .with("chatId", chat_id)
                .with("thread", thread),
        );
        let revision = result
            .get("chat")
            .and_then(|c| c.get("revision"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        self.emit(
            AppEvent::new("pending")
                .with("chatId", chat_id)
                .with("revision", revision)
                .with("pending", pending),
        );
        if again {
            self.restart(&key);
            return;
        }
        if since == 0 || !applied.is_empty() {
            self.emit(AppEvent::new("refresh").with("chatId", chat_id));
        }
        self.go_live(&key);
    }

    /// News for an opened chat. `true` when it must be read again from the start.
    fn apply_chat_news(self: &Arc<Self>, chat_id: &str, kind: &str, ev: &Value) -> bool {
        match kind {
            "message" => {
                let rows: Vec<Value> = ev
                    .get("rows")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let applied = self.db.write(|tx| {
                    let a = upsert_rows(tx, chat_id, &rows)?;
                    cap_rows(tx, chat_id)?;
                    Ok::<_, rusqlite::Error>(a)
                });
                if let Ok(applied) = applied {
                    if !applied.is_empty() {
                        let revision = ev.get("revision").and_then(Value::as_i64).unwrap_or(0);
                        self.emit(
                            AppEvent::new("message")
                                .with("chatId", chat_id)
                                .with("revision", revision)
                                .with("rows", applied),
                        );
                    }
                }
                false
            }
            "thread" => {
                let thread = ev.get("thread").cloned().unwrap_or(Value::Null);
                let _ = self.db.write(|tx| {
                    tx.execute(
                        "UPDATE mirror_chats SET thread = ?2 WHERE chat_id = ?1",
                        params![chat_id, thread.to_string()],
                    )
                });
                self.emit(
                    AppEvent::new("thread")
                        .with("chatId", chat_id)
                        .with("thread", thread),
                );
                false
            }
            "pending" => {
                let pending = ev.get("pending").cloned().unwrap_or(Value::Null);
                let _ = self.db.write(|tx| {
                    tx.execute(
                        "UPDATE mirror_chats SET pending = ?2 WHERE chat_id = ?1",
                        params![chat_id, pending.to_string()],
                    )
                });
                self.emit(
                    AppEvent::new("pending")
                        .with("chatId", chat_id)
                        .with(
                            "revision",
                            ev.get("revision").and_then(Value::as_i64).unwrap_or(0),
                        )
                        .with("pending", pending),
                );
                false
            }
            "refresh" => {
                let _ = self.db.write(|tx| {
                    tx.execute("DELETE FROM mirror_rows WHERE chat_id = ?1", [chat_id])
                });
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rows and records
// ---------------------------------------------------------------------------------------------

/// Store a chat's record when it is newer than the one held. `Some((id, changed))` for a chat
/// kept; `None` for one that is not a mirror's business (this device's own).
fn upsert_chat(
    tx: &Transaction<'_>,
    device: &str,
    chat: &Value,
    self_uid: &str,
) -> rusqlite::Result<Option<(String, bool)>> {
    let Some(id) = chat.get("id").and_then(Value::as_str) else {
        return Ok(None);
    };
    if device == self_uid {
        return Ok(None);
    }
    // A chat that runs here is never mirrored, whatever another device says.
    let mine: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1)",
            [id],
            |r| r.get(0),
        )
        .unwrap_or(false);
    if mine {
        return Ok(None);
    }
    let revision = chat.get("revision").and_then(Value::as_i64).unwrap_or(0);
    let person = chat.get("personId").and_then(Value::as_str);
    let shared = chat.get("sharedWith").cloned().unwrap_or_else(|| json!([]));
    let n = tx.execute(
        "INSERT INTO mirror_chats(chat_id, device_uid, record, person_id, shared_with, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(chat_id) DO UPDATE SET device_uid = excluded.device_uid,
             record = excluded.record, person_id = excluded.person_id,
             shared_with = excluded.shared_with, revision = excluded.revision
         WHERE excluded.revision > mirror_chats.revision",
        params![
            id,
            device,
            chat.to_string(),
            person,
            shared.to_string(),
            revision
        ],
    )?;
    Ok(Some((id.to_owned(), n == 1)))
}

/// Store the rows newer than those held. Returns the ones stored.
fn upsert_rows(
    tx: &Transaction<'_>,
    chat_id: &str,
    rows: &[Value],
) -> rusqlite::Result<Vec<Value>> {
    let mut applied = Vec::new();
    for r in rows {
        let Some(id) = r.get("id").and_then(Value::as_i64) else {
            continue;
        };
        let revision = r.get("revision").and_then(Value::as_i64).unwrap_or(0);
        let ts = r.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
        let n = tx.execute(
            "INSERT INTO mirror_rows(chat_id, row_id, revision, ts, row) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(chat_id, row_id) DO UPDATE SET revision = excluded.revision,
                 ts = excluded.ts, row = excluded.row
             WHERE excluded.revision > mirror_rows.revision",
            params![chat_id, id, revision, ts, r.to_string()],
        )?;
        if n == 1 {
            applied.push(r.clone());
        }
    }
    Ok(applied)
}

/// Keep the newest [`MAX_ROWS`] rows of a chat; older ones are read from the device on demand.
fn cap_rows(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<()> {
    let cut: Option<(i64, i64)> = tx
        .query_row(
            "SELECT ts, row_id FROM mirror_rows WHERE chat_id = ?1
             ORDER BY ts DESC, row_id DESC LIMIT 1 OFFSET ?2",
            params![chat_id, MAX_ROWS - 1],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((ts, id)) = cut {
        tx.execute(
            "DELETE FROM mirror_rows WHERE chat_id = ?1 AND (ts, row_id) < (?2, ?3)",
            params![chat_id, ts, id],
        )?;
        tx.execute(
            "UPDATE mirror_chats SET older = 1 WHERE chat_id = ?1",
            [chat_id],
        )?;
    }
    Ok(())
}

fn profile_for_person(conn: &Connection, person: &str) -> Option<String> {
    conn.query_row(
        "SELECT id FROM profiles WHERE hub_person_id = ?1",
        [person],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// A mirrored chat as the window's chat record: the device's record with the person turned into
/// this PC's profile, the draft kept here, and where it runs. `None` for an unknown chat.
pub fn chat_value(conn: &Connection, chat_id: &str) -> rusqlite::Result<Option<Value>> {
    let row: Option<(String, String, String, i64)> = conn
        .query_row(
            "SELECT device_uid, record, draft, draft_revision FROM mirror_chats WHERE chat_id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((device, record, draft, draft_revision)) = row else {
        return Ok(None);
    };
    let Ok(Value::Object(mut rec)) = serde_json::from_str::<Value>(&record) else {
        return Ok(None);
    };
    let profile = rec
        .get("personId")
        .and_then(Value::as_str)
        .and_then(|p| profile_for_person(conn, p))
        .unwrap_or_default();
    let people = rec.get("sharedWith").cloned().unwrap_or_else(|| json!([]));
    let shared: Vec<String> = people
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|p| profile_for_person(conn, p))
        .collect();
    rec.insert("profileId".into(), json!(profile));
    rec.insert("sharedWithPeople".into(), people);
    rec.insert("sharedWith".into(), json!(shared));
    rec.insert("hostDeviceId".into(), json!(device));
    rec.insert("draft".into(), json!(draft));
    rec.insert("draftRevision".into(), json!(draft_revision));
    Ok(Some(Value::Object(rec)))
}

// ---------------------------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------------------------

fn parse_cursor(conn: &Connection, chat: &str, c: &str) -> rusqlite::Result<Option<(i64, i64)>> {
    let Ok(id) = c.trim().parse::<i64>() else {
        return Ok(None);
    };
    conn.query_row(
        "SELECT ts, row_id FROM mirror_rows WHERE chat_id = ?1 AND row_id = ?2",
        params![chat, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

fn select_page(
    conn: &Connection,
    chat: &str,
    newer: bool,
    from: Option<(i64, i64)>,
    inclusive: bool,
    limit: i64,
) -> rusqlite::Result<Vec<Value>> {
    let (cmp, order) = match (newer, inclusive) {
        (false, false) => ("<", "DESC"),
        (false, true) => ("<=", "DESC"),
        (true, false) => (">", "ASC"),
        (true, true) => (">=", "ASC"),
    };
    let where_from = if from.is_some() {
        format!("AND (ts, row_id) {cmp} (?2, ?3)")
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT row FROM mirror_rows WHERE chat_id = ?1 {where_from}
         ORDER BY ts {order}, row_id {order} LIMIT ?4"
    );
    let (t, i) = from.unwrap_or((0, 0));
    let mut q = conn.prepare(&sql)?;
    let rows = q.query_map(params![chat, t, i, limit], |r| r.get::<_, String>(0))?;
    let mut v: Vec<Value> = rows
        .flatten()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    if !newer {
        v.reverse();
    }
    Ok(v)
}

fn exists_beyond(
    conn: &Connection,
    chat: &str,
    m: Option<&Value>,
    newer: bool,
) -> rusqlite::Result<bool> {
    let Some(m) = m else { return Ok(false) };
    let cmp = if newer { ">" } else { "<" };
    conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM mirror_rows WHERE chat_id = ?1 AND (ts, row_id) {cmp} (?2, ?3))"),
        params![
            chat,
            m.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
            m.get("id").and_then(Value::as_i64).unwrap_or(0)
        ],
        |r| r.get(0),
    )
}

/// What the copy here can say about a page of a chat.
pub struct LocalPage {
    /// The page in the transcript page shape.
    pub page: Value,
    /// The cursor named a row this copy does not hold (older than it, or never kept).
    pub cursor_unknown: bool,
    /// The copy lacks rows older than the ones it holds (the device has them).
    pub older_on_device: bool,
}

/// A transcript page from the rows held. `None` for an unknown chat.
pub fn local_page(
    conn: &Connection,
    chat: &str,
    before: Option<&str>,
    after: Option<&str>,
    around: Option<i64>,
    inclusive: bool,
) -> rusqlite::Result<Option<LocalPage>> {
    let meta: Option<(i64, bool, Option<String>)> = conn
        .query_row(
            "SELECT revision, older, pending FROM mirror_chats WHERE chat_id = ?1",
            [chat],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0, r.get(2)?)),
        )
        .optional()?;
    let Some((revision, older, pending)) = meta else {
        return Ok(None);
    };
    let mut cursor_unknown = false;
    let messages = if let Some(target) = around {
        let at: Option<(i64, i64)> = conn
            .query_row(
                "SELECT ts, row_id FROM mirror_rows WHERE chat_id = ?1 AND row_id = ?2",
                params![chat, target],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match at {
            Some(at) => {
                let mut o = select_page(conn, chat, false, Some(at), false, PAGE_ROWS / 2)?;
                o.extend(select_page(
                    conn,
                    chat,
                    true,
                    Some(at),
                    true,
                    PAGE_ROWS / 2,
                )?);
                o
            }
            None => select_page(conn, chat, false, None, false, PAGE_ROWS)?,
        }
    } else if let Some(c) = before {
        match parse_cursor(conn, chat, c)? {
            Some(from) => select_page(conn, chat, false, Some(from), inclusive, PAGE_ROWS)?,
            None => {
                cursor_unknown = true;
                Vec::new()
            }
        }
    } else if let Some(c) = after {
        match parse_cursor(conn, chat, c)? {
            Some(from) => select_page(conn, chat, true, Some(from), inclusive, PAGE_ROWS)?,
            None => {
                cursor_unknown = true;
                Vec::new()
            }
        }
    } else {
        select_page(conn, chat, false, None, false, PAGE_ROWS)?
    };
    let has_older = exists_beyond(conn, chat, messages.first(), false)?;
    let has_newer = exists_beyond(conn, chat, messages.last(), true)?;
    let cursor = |m: Option<&Value>, on: bool| {
        m.filter(|_| on)
            .and_then(|m| m.get("id"))
            .and_then(Value::as_i64)
            .map(|i| i.to_string())
    };
    let pending: Value = pending
        .and_then(|p| serde_json::from_str(&p).ok())
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]));
    let page = json!({
        "chatId": chat,
        "olderCursor": cursor(messages.first(), has_older),
        "newerCursor": cursor(messages.last(), has_newer),
        "hasOlder": has_older,
        "hasNewer": has_newer,
        "messages": messages,
        "runs": [],
        "pending": pending,
        "revision": revision,
    });
    Ok(Some(LocalPage {
        page,
        cursor_unknown,
        older_on_device: older,
    }))
}

// ---------------------------------------------------------------------------------------------
// What the window lists
// ---------------------------------------------------------------------------------------------

/// How a device reads to the person: `live`, `readonly` (it lets no one run its chats),
/// `offline`, or `reconnecting` (this PC's own line is down, so what is shown is the copy here).
pub fn device_state(is_self: bool, line_up: bool, online: bool, run: bool) -> &'static str {
    if is_self {
        "live"
    } else if !line_up {
        "reconnecting"
    } else if !online {
        "offline"
    } else if !run {
        "readonly"
    } else {
        "live"
    }
}

/// `devices_list`: this device first, then the others by name.
pub fn devices_view(conn: &Connection, line_up: bool) -> rusqlite::Result<Value> {
    let self_uid = identity::self_uid(conn).unwrap_or_default();
    let self_name = identity::self_name(conn).unwrap_or_default();
    let run_here = helm_core::prefs::read(conn, "deviceRunFromOthers")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let mut list = load_devices(conn).unwrap_or_default();
    list.retain(|d| d.uid != self_uid && !d.is_self);
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let mut out = vec![json!({
        "uid": self_uid,
        "name": self_name,
        "person": Value::Null,
        "online": true,
        "lastSeenAt": Value::Null,
        "run": run_here,
        "isSelf": true,
        "state": "live",
    })];
    for d in list {
        out.push(json!({
            "uid": d.uid,
            "name": d.name,
            "person": d.person,
            "personId": d.person_id,
            "online": d.online,
            "lastSeenAt": d.last_seen_at,
            "run": d.run,
            "isSelf": false,
            "state": device_state(false, line_up, d.online, d.run),
        }));
    }
    Ok(json!({ "self": self_uid, "devices": out }))
}

/// The projects another device last said it has.
pub fn mirror_projects(conn: &Connection, device: &str) -> rusqlite::Result<Vec<Value>> {
    let mut q = conn.prepare(
        "SELECT project_id, name, colour, kind FROM mirror_projects WHERE device_uid = ?1 ORDER BY name",
    )?;
    let rows = q.query_map([device], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "name": r.get::<_, String>(1)?,
            "colour": r.get::<_, String>(2)?,
            "kind": r.get::<_, String>(3)?,
        }))
    })?;
    rows.collect()
}

/// The chats of another device that a profile shows (its own and those shared with it), the
/// most recently active first. A profile with no hub person sees none of them; no profile at all
/// sees every one.
pub fn mirror_chat_list(
    conn: &Connection,
    device: &str,
    profile_id: Option<&str>,
    archived: bool,
) -> rusqlite::Result<Vec<Value>> {
    let person: Option<Option<String>> = match profile_id {
        Some(p) => Some(
            conn.query_row(
                "SELECT hub_person_id FROM profiles WHERE id = ?1",
                [p],
                |r| r.get(0),
            )
            .optional()?
            .flatten(),
        ),
        None => None,
    };
    let ids: Vec<String> = {
        let mut q = conn.prepare("SELECT chat_id FROM mirror_chats WHERE device_uid = ?1")?;
        let rows = q.query_map([device], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::new();
    for id in ids {
        let Some(c) = chat_value(conn, &id)? else {
            continue;
        };
        if c["archived"].as_bool().unwrap_or(false) != archived {
            continue;
        }
        if let Some(p) = &person {
            let Some(p) = p else { continue };
            let mine = c["personId"].as_str() == Some(p.as_str());
            let shared = c["sharedWithPeople"]
                .as_array()
                .is_some_and(|l| l.iter().any(|s| s.as_str() == Some(p.as_str())));
            if !mine && !shared {
                continue;
            }
        }
        out.push(c);
    }
    out.sort_by_key(|c| std::cmp::Reverse(c["lastActivityAt"].as_i64().unwrap_or(0)));
    Ok(out)
}
