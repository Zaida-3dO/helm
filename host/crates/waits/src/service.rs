//! The watcher: two loops over the open waits, and the one delivery each fired wait gets.
//!
//! - The local sweep (every 2 s) looks at chats, processes and files on this PC, applies
//!   timeouts, and cancels the waits of a chat that was stopped or archived.
//! - The network sweep (every 5 s) looks at files on a network share (a `\server\share` path
//!   or a mapped drive) and polls pull requests that are due: 30 s apart at first, doubling to
//!   5 minutes, one `gh` call per distinct pull request. A share that hangs can only hold up
//!   this sweep. A file that cannot be looked at (the share is down) never fires its wait.
//! - A wait is settled by one conditional update, so it fires at most once. Delivery is a
//!   separate, retried step under a fixed submission id, so a message is never sent twice either.
//!
//! No step here involves a model.

use crate::model::{self, Observation, Outcome, Target, Wait};
use crate::probes::Probes;
use crate::store::{self, AddError};
use helm_core::db::Database;
use rusqlite::OptionalExtension;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const LOCAL_SWEEP_SECS: u64 = 2;
pub const NETWORK_SWEEP_SECS: u64 = 5;
const POLL_FIRST_MS: i64 = 30_000;
const POLL_MAX_MS: i64 = 300_000;

/// How a delivery went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    /// The chat can no longer be told (archived or deleted): stop trying.
    Gone,
    /// The chat refused every try: stop trying, but do not call the message delivered.
    GaveUp,
    /// A passing problem: try again at the next sweep.
    Retry,
}

/// Where a fired wait's message goes. `watch` names the chat the wait was about (its id and
/// title) when the wait watched a chat: it rides along so the chat's card can show one plain
/// line about that chat instead of the raw note.
pub trait Deliver: Send + Sync {
    fn deliver(
        &self,
        chat_id: &str,
        submission_id: &str,
        text: &str,
        watch: Option<(&str, &str)>,
    ) -> Delivery;
}

pub struct WaitsService {
    db: Arc<Database>,
    probes: Arc<dyn Probes>,
    sink: OnceLock<Arc<dyn Deliver>>,
    /// Per network wait: when it is next polled, and the gap it will use after that.
    polls: Mutex<HashMap<String, (i64, i64)>>,
    last_prune: AtomicI64,
}

impl WaitsService {
    pub fn new(db: Arc<Database>, probes: Arc<dyn Probes>) -> Self {
        Self {
            db,
            probes,
            sink: OnceLock::new(),
            polls: Mutex::new(HashMap::new()),
            last_prune: AtomicI64::new(0),
        }
    }

    /// Where delivery goes; set once the chat runtime exists. Until then fired waits stay owed.
    pub fn set_sink(&self, sink: Arc<dyn Deliver>) {
        let _ = self.sink.set(sink);
    }

    pub fn probes(&self) -> &Arc<dyn Probes> {
        &self.probes
    }

    pub fn declare(&self, w: &Wait) -> rusqlite::Result<Result<(), AddError>> {
        self.db.write(|tx| store::add(tx, w))
    }

    pub fn list(&self, chat: Option<&str>) -> rusqlite::Result<Vec<Wait>> {
        self.db.snapshot(|tx| store::open(tx, chat))
    }

    pub fn cancel(&self, id: &str, chat: Option<&str>) -> rusqlite::Result<bool> {
        let done = self
            .db
            .write(|tx| store::cancel(tx, id, chat, helm_core::now_ms()))?;
        if done {
            self.polls.lock().unwrap().remove(id);
        }
        Ok(done)
    }

    /// The wait `app_create_chat` declares for the chat that made `child`: its turn ends, it
    /// stops, errors or goes away, for a day at most. The baseline is taken now, like any other
    /// chat wait. Returns the wait's id, or why there is none.
    pub fn watch_child(
        &self,
        parent: &str,
        child: &str,
        child_title: &str,
    ) -> Result<String, String> {
        let now = helm_core::now_ms();
        let busy = self
            .probes
            .chat(child)
            .ok()
            .flatten()
            .is_some_and(|c| c.busy);
        let title: String = child_title.chars().take(120).collect();
        let w = Wait {
            id: uuid::Uuid::new_v4().to_string(),
            chat_id: parent.to_owned(),
            target: Target::ChatIdle {
                chat_id: child.to_owned(),
            },
            note: Some(format!(
                "Declared for you when you created the chat \"{title}\" (watch is on)."
            )),
            created_at: now,
            expires_at: now + model::MAX_TIMEOUT_SECS * 1000,
            state: serde_json::json!({"seenBusy": busy, "watch": true}),
        };
        match self.declare(&w) {
            Ok(Ok(())) => Ok(w.id),
            Ok(Err(AddError::ChatFull)) => Err(format!(
                "You already watch {} chats you created, so none was declared for this chat.",
                model::MAX_WATCHES_PER_CHAT
            )),
            Ok(Err(AddError::AppFull)) => {
                Err("Helm already watches the most waits it will; none was declared.".to_owned())
            }
            Ok(Err(AddError::NoChat)) => Err("The chat no longer exists.".to_owned()),
            Err(e) => Err(format!("The wait could not be saved: {e}")),
        }
    }

    /// Settle `w` if the observation says so, else keep what the watcher learned — which
    /// includes a chat target moved by a roll over, so the follow survives a restart.
    fn step(&self, mut w: Wait, obs: &Observation, now: i64) {
        let before = w.state.clone();
        let target_before = w.target.clone();
        match model::judge(&mut w, obs, now) {
            Some(o) if self.child_watch_is_redundant(&w, &o) => {
                // The parent already has one message about this turn: the wait ends quietly.
                let _ = self.db.write(|tx| store::cancel(tx, &w.id, None, now));
                self.polls.lock().unwrap().remove(&w.id);
            }
            Some(o) => {
                match self.db.write(|tx| store::fire(tx, &w.id, &o, now)) {
                    Ok(true) => {
                        log::info!("wait {} ({}) fired: {}", w.id, w.target.kind(), o.code)
                    }
                    Ok(false) => {}
                    Err(e) => log::warn!("wait {} could not be settled: {e}", w.id),
                }
                self.polls.lock().unwrap().remove(&w.id);
            }
            None if w.state != before || w.target != target_before => {
                let state: Value = w.state.clone();
                let target = w.target.clone();
                if let Err(e) = self
                    .db
                    .write(|tx| store::save_memory(tx, &w.id, &state, &target))
                {
                    log::warn!("wait {} memory not saved: {e}", w.id);
                }
            }
            None => {}
        }
    }

    /// A watch declared for a created chat that fires because the child's turn ended (or was
    /// found already idle), when the parent already has a message about that turn (the child
    /// reported, or it ended unfinished and the parent was told so): firing too would wake the
    /// parent twice.
    fn child_watch_is_redundant(&self, w: &Wait, o: &Outcome) -> bool {
        let Target::ChatIdle { chat_id: child } = &w.target else {
            return false;
        };
        if !matches!(o.code, "finished" | "idle") || !store::is_watch(&w.state) {
            return false;
        }
        self.db
            .snapshot(|tx| helm_chats::childnote::watch_redundant(tx, &w.chat_id, child))
            .unwrap_or(false)
    }

    /// Cancel the waits of a chat that was stopped or archived. Deleting the chat removes them.
    fn cancel_orphans(&self, waits: &[Wait], now: i64) -> Vec<String> {
        let mut gone = Vec::new();
        let mut seen: HashMap<&str, bool> = HashMap::new();
        for w in waits {
            let dead = *seen.entry(w.chat_id.as_str()).or_insert_with(|| {
                // A chat that could not be looked at is not a chat that is gone.
                match self.probes.chat(&w.chat_id) {
                    Ok(c) => c.is_none_or(|c| c.archived || c.stopped),
                    Err(e) => {
                        log::warn!("wait {}: chat not read: {e}", w.id);
                        false
                    }
                }
            });
            if dead {
                let _ = self.db.write(|tx| store::cancel(tx, &w.id, None, now));
                gone.push(w.id.clone());
            }
        }
        gone
    }

    /// A file on a network share: watched by the network sweep.
    fn on_share(&self, t: &Target) -> bool {
        matches!(t, Target::File { path } if self.probes.on_share(path))
    }

    /// "Could not look" is not an observation: only the timeout can fire on it.
    fn file_obs(&self, path: &str) -> Observation {
        match self.probes.file(path) {
            Some(f) => Observation::File(f),
            None => Observation::Skipped,
        }
    }

    /// One pass over the waits that need no network.
    pub fn sweep_local(&self, now: i64) {
        let Ok(waits) = self.list(None) else { return };
        let cancelled = self.cancel_orphans(&waits, now);
        for w in waits {
            if w.target.is_network() || self.on_share(&w.target) || cancelled.contains(&w.id) {
                continue;
            }
            let obs = match &w.target {
                Target::ChatIdle { chat_id } => match self.probes.chat(chat_id) {
                    Ok(c) => Observation::Chat(c),
                    Err(_) => Observation::Skipped,
                },
                Target::Process { pid } => Observation::Process(self.probes.process(*pid)),
                Target::File { path } => self.file_obs(path),
                _ => Observation::Skipped,
            };
            self.step(w, &obs, now);
        }
        self.deliver_pending();
        if now - self.last_prune.load(Ordering::Relaxed) >= 3_600_000 {
            self.last_prune.store(now, Ordering::Relaxed);
            let _ = self.db.write(|tx| store::prune(tx, now));
        }
    }

    /// One pass over the waits that need the network: files on a share are looked at every
    /// sweep; pull requests are polled when due, and the rest only time out.
    pub fn sweep_network(&self, now: i64) {
        let Ok(waits) = self.list(None) else { return };
        let waits: Vec<Wait> = waits
            .into_iter()
            .filter(|w| w.target.is_network() || self.on_share(&w.target))
            .collect();
        let cancelled = self.cancel_orphans(&waits, now);
        let mut fetched: HashMap<String, Result<model::PrObs, String>> = HashMap::new();
        for w in waits {
            if cancelled.contains(&w.id) {
                continue;
            }
            let url = match &w.target {
                Target::File { path } => {
                    let obs = self.file_obs(path);
                    self.step(w, &obs, now);
                    continue;
                }
                Target::PrChecks { url } | Target::PrReview { url } => url.clone(),
                _ => continue,
            };
            let due = {
                let mut polls = self.polls.lock().unwrap();
                let (at, gap) = polls.get(&w.id).copied().unwrap_or((0, POLL_FIRST_MS));
                if now >= at {
                    polls.insert(w.id.clone(), (now + gap, (gap * 2).min(POLL_MAX_MS)));
                    true
                } else {
                    false
                }
            };
            let obs = if due {
                let r = fetched
                    .entry(url.clone())
                    .or_insert_with(|| self.probes.pr(&url))
                    .clone();
                if let Err(e) = &r {
                    log::info!("wait {}: pull request not read: {e}", w.id);
                }
                Observation::Pr(r)
            } else {
                Observation::Skipped
            };
            self.step(w, &obs, now);
        }
        self.deliver_pending();
    }

    /// Send each fired wait's message once.
    pub fn deliver_pending(&self) {
        let Some(sink) = self.sink.get() else { return };
        let owed = self.db.snapshot(store::undelivered).unwrap_or_default();
        for (w, o) in owed {
            let text = model::framed(&w, &o);
            let sid = delivery_id(&w.id);
            let watch = watched_chat(&self.db, &w);
            match sink.deliver(
                &w.chat_id,
                &sid,
                &text,
                watch
                    .as_ref()
                    .map(|(id, title)| (id.as_str(), title.as_str())),
            ) {
                Delivery::Sent | Delivery::Gone => {
                    let _ = self.db.write(|tx| store::mark_delivered(tx, &w.id));
                }
                Delivery::GaveUp => {
                    let _ = self.db.write(|tx| store::mark_given_up(tx, &w.id));
                }
                Delivery::Retry => {}
            }
        }
    }
}

/// The chat a wait watched, with its title: `Some` only for a chat wait whose chat still has a
/// row. The title comes from the chats table beside the waits', as the probes read them.
fn watched_chat(db: &Database, w: &Wait) -> Option<(String, String)> {
    let Target::ChatIdle { chat_id } = &w.target else {
        return None;
    };
    let id = chat_id.clone();
    let id_for_read = id.clone();
    let title = db
        .snapshot(move |tx| -> rusqlite::Result<Option<String>> {
            let title = tx
                .query_row(
                    "SELECT title FROM chats WHERE id = ?1",
                    [&id_for_read],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten();
            Ok(title)
        })
        .ok()
        .flatten()?;
    Some((id, title))
}

/// The submission id for a fired wait's message: the same wait always maps to the same id, so
/// a retry can never deliver the message twice. Chat workers take only UUIDs as message ids,
/// so it is a name-based UUID (version 5, from a SHA-256 of the wait id), like the ids the
/// scheduler and the other automated senders derive for themselves.
pub fn delivery_id(wait_id: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("helm-wait/{wait_id}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(b).to_string()
}

/// How many times a refused message is sent again before the wait gives up on it.
const MAX_RESENDS: u32 = 3;

/// The id for the `n`-th re-send of a message the chat refused (the same name-based form as
/// [`delivery_id`], so a retry still can never deliver twice).
fn resend_id(first: &str, n: u32) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("helm-wait-resend/{first}/{n}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(b).to_string()
}

/// Delivery through the chat runtime: an app message that starts a turn if the chat is
/// idle and is read by the running turn at its next tool boundary if it is busy.
///
/// The wait counts as delivered only once the chat has accepted the message: a chat that refuses
/// it (a local server that is down, a worker that will not start) leaves the message "not
/// delivered", and the wait sends it again under a new id, up to a few times, rather than
/// being used up with the agent never woken.
pub struct RuntimeSink(pub helm_chats::ChatRuntime);

impl Deliver for RuntimeSink {
    fn deliver(
        &self,
        chat_id: &str,
        submission_id: &str,
        text: &str,
        watch: Option<(&str, &str)>,
    ) -> Delivery {
        use helm_chats::types::SubmissionState as S;
        let state_of = |id: &str| {
            self.0
                .db()
                .snapshot(|t| helm_chats::store::get_submission(t, id))
                .ok()
                .flatten()
                .map(|s| s.state)
        };
        for n in 0..=MAX_RESENDS {
            let id = if n == 0 {
                submission_id.to_owned()
            } else {
                resend_id(submission_id, n)
            };
            match state_of(&id) {
                // Not read yet, or maybe read: either way the message is with the chat, and
                // sending it again could wake the agent twice.
                Some(S::Accepted | S::Delivered | S::Complete | S::Uncertain) => {
                    return Delivery::Sent;
                }
                Some(S::Withdrawn | S::Superseded) => return Delivery::Gone,
                // Refused before it reached Claude: the next id gets its own try.
                Some(S::Rejected) => {
                    log::info!("wait message for {chat_id} was refused; sending it again");
                    continue;
                }
                // Handed over (or about to be) and not answered yet: ask again at the next sweep.
                Some(S::Saved | S::Dispatching) | None => {
                    return match self.hand_over(chat_id, &id, text, watch) {
                        Delivery::Sent => Delivery::Retry,
                        other => other,
                    };
                }
            }
        }
        log::warn!("wait message for {chat_id} was refused every time; giving up");
        Delivery::GaveUp
    }
}

impl RuntimeSink {
    /// Save the message in the chat (or hand a saved one over again). `Sent` means the chat has
    /// it; whether it accepts it is seen at a later sweep.
    fn hand_over(
        &self,
        chat_id: &str,
        submission_id: &str,
        text: &str,
        watch: Option<(&str, &str)>,
    ) -> Delivery {
        use helm_chats::submit::{host_send, HostSend};
        let sent = host_send(
            &self.0,
            HostSend {
                chat_id,
                submission_id,
                text,
                origin: helm_chats::types::SubmissionOrigin::App,
                page: None,
                // Helm's own wait note: its kind is the wait finishing, and when the wait was
                // about a chat, the card gets that chat's name with it.
                kind: Some(helm_chats::types::MessageKind::WaitFinished),
                wait: watch,
            },
        );
        match sent {
            // Only an accepted hand-over counts: the wait stays owed until the chat has the
            // message, so a send that died before handing it over is tried again.
            Ok(s) if s.delivered => Delivery::Sent,
            Ok(_) => Delivery::Retry,
            Err(e) if e.code == "not_found" || e.code == "refused" => {
                log::info!("wait message for {chat_id} not deliverable: {}", e.message);
                Delivery::Gone
            }
            Err(e) => {
                log::warn!("wait message for {chat_id}: {}; retrying", e.message);
                Delivery::Retry
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChatObs;
    use crate::testkit::{Fake, Sink};
    use helm_core::db::MigrationRegistry;

    fn idle() -> ChatObs {
        ChatObs {
            archived: false,
            busy: false,
            stopped: false,
            error: None,
            rolls_to: None,
            helpers: false,
            finished_turn: false,
            unreported: false,
        }
    }

    /// The same shape check the chat worker applies to every message id: eight groups of hex
    /// digits in 8-4-4-4-12 order. A submission id that is not this shape is dropped there.
    fn worker_shape(s: &str) -> bool {
        let hex = |c: char| c.is_ascii_hexdigit();
        let g = s.split('-').collect::<Vec<_>>();
        g.len() == 5
            && [8, 4, 4, 4, 12]
                .iter()
                .zip(&g)
                .all(|(n, part)| part.len() == *n && part.chars().all(hex))
    }

    #[test]
    fn delivery_ids_are_stable_worker_shape_uuids() {
        let a = delivery_id("a1");
        assert_eq!(a, delivery_id("a1"), "a retry stays the same submission");
        assert_ne!(a, delivery_id("a2"));
        assert!(worker_shape(&a), "{a} must pass the worker's id check");
        assert_eq!(uuid::Uuid::parse_str(&a).unwrap().get_version_num(), 5);
    }

    fn wait(id: &str, chat: &str, target: Target, now: i64, secs: i64) -> Wait {
        Wait {
            id: id.into(),
            chat_id: chat.into(),
            target,
            note: None,
            created_at: now,
            expires_at: now + secs * 1000,
            state: serde_json::json!({}),
        }
    }

    /// The wait a chat gets for a chat it creates: a chat_idle on the child for a day, with the
    /// child's busy baseline, and a plain reason when the parent has no room for one.
    #[test]
    fn watch_child_declares_a_day_long_chat_wait_and_says_why_when_it_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        helm_chats::register_migrations(&mut reg);
        let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        let kids: Vec<String> = (0..=model::MAX_WATCHES_PER_CHAT)
            .map(|i| format!("k{i}"))
            .collect();
        db.write(|tx| {
            tx.execute("INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)", [])?;
            for id in kids.iter().map(String::as_str).chain(["par"]) {
                tx.execute(
                    "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 1, 1)",
                    [id],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let probes = Arc::new(Fake::default());
        for id in &kids {
            probes.chats.lock().unwrap().insert(id.clone(), idle());
        }
        probes.chats.lock().unwrap().insert(
            "k0".into(),
            ChatObs {
                busy: true,
                ..idle()
            },
        );
        let svc = WaitsService::new(db, probes);
        let before = helm_core::now_ms();
        let id = svc.watch_child("par", "k0", "Doc builder").unwrap();
        let open = svc.list(Some("par")).unwrap();
        assert_eq!(open.len(), 1);
        let w = &open[0];
        assert_eq!(w.id, id);
        assert_eq!(
            w.target,
            Target::ChatIdle {
                chat_id: "k0".into()
            }
        );
        assert_eq!(
            w.state["seenBusy"], true,
            "a child already working is the baseline"
        );
        assert!(w.expires_at - before >= model::MAX_TIMEOUT_SECS * 1000);
        assert!(w.note.as_deref().unwrap().contains("Doc builder"));
        // The watches have a room of their own: a parent's ordinary waits fill theirs and a watch
        // still fits, and the watches' room is bounded in turn.
        for n in 0..model::MAX_PER_CHAT {
            let w = wait(
                &format!("o{n}"),
                "par",
                Target::Process {
                    pid: 900 + n as u32,
                },
                before,
                600,
            );
            assert_eq!(svc.declare(&w).unwrap(), Ok(()));
        }
        for k in &kids[1..model::MAX_WATCHES_PER_CHAT] {
            svc.watch_child("par", k, "x").unwrap();
        }
        let last = &kids[model::MAX_WATCHES_PER_CHAT];
        let full = svc.watch_child("par", last, "x").unwrap_err();
        assert!(full.contains("watch"), "{full}");
        let gone = svc.watch_child("nobody", last, "x").unwrap_err();
        assert!(gone.contains("no longer exists"), "{gone}");
    }

    /// A wait on a session that a roll over archived moves to the session that carries the
    /// chat on (across a second roll over), survives that way in the table, and its message
    /// names both the session it started on and the one the chat ended in. It fires once.
    #[test]
    fn a_chat_idle_wait_follows_a_roll_over_and_names_both_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        helm_chats::register_migrations(&mut reg);
        let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        db.write(|tx| {
            tx.execute("INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)", [])?;
            for id in ["watcher", "s1", "s2", "s3"] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 1, 1)",
                    [id],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let probes = Arc::new(Fake::default());
        let put = |p: &Fake, id: &str, o: ChatObs| {
            p.chats.lock().unwrap().insert(id.to_owned(), o);
        };
        put(
            &probes,
            "s1",
            ChatObs {
                busy: true,
                ..idle()
            },
        );
        put(&probes, "watcher", idle());
        let sink = Arc::new(Sink::default());
        let svc = WaitsService::new(db, probes.clone());
        svc.set_sink(sink.clone());
        let t0 = 1_000_000;
        svc.declare(&wait(
            "f1",
            "watcher",
            Target::ChatIdle {
                chat_id: "s1".into(),
            },
            t0,
            600,
        ))
        .unwrap()
        .unwrap();
        svc.sweep_local(t0 + 2_000); // seen working
                                     // s1 is archived by a roll over carrying on in s2: the wait moves, nothing fires,
                                     // and the move is what a restart would read back.
        put(
            &probes,
            "s1",
            ChatObs {
                archived: true,
                rolls_to: Some("s2".into()),
                ..idle()
            },
        );
        put(
            &probes,
            "s2",
            ChatObs {
                busy: true,
                ..idle()
            },
        );
        svc.sweep_local(t0 + 4_000);
        let open = svc.list(Some("watcher")).unwrap();
        assert_eq!(open.len(), 1, "the move does not fire the wait");
        assert_eq!(
            open[0].target,
            Target::ChatIdle {
                chat_id: "s2".into()
            },
            "the moved target is saved"
        );
        // A second roll over, then the newest session's turn ends.
        put(
            &probes,
            "s2",
            ChatObs {
                archived: true,
                rolls_to: Some("s3".into()),
                ..idle()
            },
        );
        put(
            &probes,
            "s3",
            ChatObs {
                busy: true,
                ..idle()
            },
        );
        svc.sweep_local(t0 + 6_000);
        probes.chats.lock().unwrap().get_mut("s3").unwrap().busy = false;
        svc.sweep_local(t0 + 8_000);
        let sent = sink.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "one message, exactly once");
        assert_eq!(sent[0].0, "watcher");
        assert!(sent[0].2.contains("chat s3"), "{}", sent[0].2);
        assert!(
            sent[0].2.contains("Followed the roll over from s1 to s3"),
            "{}",
            sent[0].2
        );
        svc.sweep_local(t0 + 10_000);
        assert_eq!(
            sink.sent.lock().unwrap().len(),
            1,
            "a fired wait never fires again"
        );
    }

    #[test]
    fn waits_are_capped_fire_once_survive_a_restart_and_follow_their_chat() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        helm_chats::register_migrations(&mut reg);
        let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        let ids: Vec<String> = (0..9).map(|i| format!("c{i}")).collect();
        db.write(|tx| {
            tx.execute("INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)", [])?;
            for id in &ids {
                tx.execute(
                    "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 1, 1)",
                    [id],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let probes = Arc::new(Fake::default());
        for id in &ids {
            probes.chats.lock().unwrap().insert(id.clone(), idle());
        }
        let sink = Arc::new(Sink::default());
        let make = |db: &Arc<Database>| {
            let s = WaitsService::new(db.clone(), probes.clone());
            s.set_sink(sink.clone());
            s
        };
        let svc = make(&db);
        let t0 = 1_000_000;
        let proc = |pid| Target::Process { pid };

        // Caps: eight per chat, sixty-four in the app.
        for n in 0..crate::model::MAX_PER_CHAT {
            assert_eq!(
                svc.declare(&wait(&format!("a{n}"), "c0", proc(100 + n as u32), t0, 600))
                    .unwrap(),
                Ok(())
            );
        }
        assert_eq!(
            svc.declare(&wait("a9", "c0", proc(1), t0, 600)).unwrap(),
            Err(AddError::ChatFull)
        );
        for c in 1..8 {
            for n in 0..8 {
                svc.declare(&wait(
                    &format!("b{c}{n}"),
                    &format!("c{c}"),
                    proc(200 + n),
                    t0,
                    600,
                ))
                .unwrap()
                .unwrap();
            }
        }
        assert_eq!(
            svc.declare(&wait("z", "c8", proc(1), t0, 600)).unwrap(),
            Err(AddError::AppFull)
        );
        // Cancel is scoped to the owner, and a cancelled wait never fires.
        assert!(!svc.cancel("a0", Some("c1")).unwrap());
        assert!(svc.cancel("a0", Some("c0")).unwrap());
        assert!(!svc.cancel("a0", Some("c0")).unwrap());
        for c in 1..8 {
            for n in 0..8 {
                svc.cancel(&format!("b{c}{n}"), None).unwrap();
            }
        }
        probes.gone.lock().unwrap().push(100);
        svc.sweep_local(t0 + 2_000);
        assert!(sink.sent.lock().unwrap().is_empty(), "a0 was cancelled");

        // A process exits: one message, once, even across sweeps and a fresh service.
        probes.gone.lock().unwrap().push(101);
        sink.fail_first.store(1, Ordering::SeqCst);
        svc.sweep_local(t0 + 4_000);
        assert!(
            sink.sent.lock().unwrap().is_empty(),
            "delivery failed, owed"
        );
        let svc = make(&db); // a restart: open waits and owed messages are still there
        svc.sweep_local(t0 + 6_000);
        svc.sweep_local(t0 + 8_000);
        {
            let sent = sink.sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].0, "c0");
            assert_eq!(sent[0].1, delivery_id("a1"));
            assert!(worker_shape(&sent[0].1), "the worker takes the id");
            assert!(sent[0].2.contains("process 101: exited"));
        }
        assert_eq!(
            svc.list(Some("c0")).unwrap().len(),
            6,
            "a2..a7 and nothing fired twice"
        );

        // The waiting chat is stopped: its waits are cancelled, not delivered.
        probes.chats.lock().unwrap().get_mut("c0").unwrap().stopped = true;
        probes.gone.lock().unwrap().push(102);
        svc.sweep_local(t0 + 10_000);
        assert!(svc.list(Some("c0")).unwrap().is_empty());
        assert_eq!(sink.sent.lock().unwrap().len(), 1);

        // Another chat's turn ends (after being seen working).
        svc.declare(&wait(
            "w1",
            "c1",
            Target::ChatIdle {
                chat_id: "c2".into(),
            },
            t0,
            600,
        ))
        .unwrap()
        .unwrap();
        db.write(|tx| {
            tx.execute("UPDATE chats SET title = 'Docs desk' WHERE id = 'c2'", [])?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        probes.chats.lock().unwrap().get_mut("c2").unwrap().busy = true;
        svc.sweep_local(t0 + 12_000);
        probes.chats.lock().unwrap().get_mut("c2").unwrap().busy = false;
        svc.sweep_local(t0 + 14_000);
        let fired = sink.sent.lock().unwrap().clone();
        let w1 = fired
            .iter()
            .find(|m| m.1 == delivery_id("w1"))
            .expect("w1 delivered");
        assert!(w1.2.contains("finished"));
        assert_eq!(
            w1.3.as_ref()
                .map(|(id, title)| (id.as_str(), title.as_str())),
            Some(("c2", "Docs desk")),
            "the watched chat's name rides with the note, so the transcript can link it"
        );
        assert!(
            fired
                .iter()
                .any(|m| m.2.contains("process 101: exited") && m.3.is_none()),
            "a process wait has no chat to name"
        );

        // A timeout, and a deleted waiting chat taking its waits with it.
        svc.declare(&wait("t1", "c3", proc(300), t0, 60))
            .unwrap()
            .unwrap();
        svc.declare(&wait("d1", "c4", proc(301), t0, 600))
            .unwrap()
            .unwrap();
        db.write(|tx| tx.execute("DELETE FROM chats WHERE id = 'c4'", []))
            .unwrap();
        svc.sweep_local(t0 + 61_000);
        let sent = sink.sent.lock().unwrap().clone();
        assert!(sent
            .iter()
            .any(|m| m.1 == delivery_id("t1") && m.2.contains("timeout")));
        assert!(svc.list(None).unwrap().iter().all(|w| w.id != "d1"));

        // Pull requests are polled when due, then backed off; a failed poll never fires.
        svc.declare(&wait(
            "p1",
            "c5",
            Target::PrChecks {
                url: "https://github.com/o/r/pull/1".into(),
            },
            t0,
            3_600,
        ))
        .unwrap()
        .unwrap();
        svc.sweep_network(t0 + 70_000);
        svc.sweep_network(t0 + 75_000);
        assert_eq!(
            probes.polls.load(Ordering::SeqCst),
            1,
            "not due again for 30 s"
        );
        svc.sweep_network(t0 + 101_000);
        assert_eq!(probes.polls.load(Ordering::SeqCst), 2);
        assert!(svc.list(Some("c5")).unwrap().len() == 1);

        // A file on a share is looked at by the network sweep only, a file on this PC by the
        // local sweep only. A share that cannot be looked at never fires; a confirmed removal does.
        let (on_share, on_disk) = ("\\\\host\\share\\x.txt", "C:\\work\\y.txt");
        probes.shares.lock().unwrap().insert(on_share.into());
        for p in [on_share, on_disk] {
            probes.files.lock().unwrap().insert(
                p.into(),
                crate::model::FileObs {
                    exists: true,
                    len: 1,
                    modified_ms: 1,
                },
            );
        }
        for (id, path) in [("f1", on_share), ("f2", on_disk)] {
            let target = Target::File { path: path.into() };
            svc.declare(&wait(id, "c5", target, t0, 3_600))
                .unwrap()
                .unwrap();
        }
        let reads = || probes.file_reads.load(Ordering::SeqCst);
        let before = reads();
        svc.sweep_network(t0 + 110_000);
        assert_eq!(
            reads() - before,
            1,
            "the network sweep reads the share file only"
        );
        svc.sweep_local(t0 + 112_000);
        assert_eq!(
            reads() - before,
            2,
            "the local sweep reads the other file only"
        );
        probes.unreachable.store(true, Ordering::SeqCst);
        svc.sweep_network(t0 + 115_000);
        svc.sweep_local(t0 + 116_000);
        probes.unreachable.store(false, Ordering::SeqCst);
        let open: Vec<String> = svc.list(None).unwrap().into_iter().map(|w| w.id).collect();
        assert!(open.contains(&"f1".to_string()) && open.contains(&"f2".to_string()));
        probes.files.lock().unwrap().remove(on_share);
        svc.sweep_local(t0 + 118_000);
        assert!(
            svc.list(None).unwrap().iter().any(|w| w.id == "f1"),
            "a local sweep leaves a share file alone"
        );
        svc.sweep_network(t0 + 120_000);
        assert!(svc.list(None).unwrap().iter().all(|w| w.id != "f1"));
        assert!(sink
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.1 == delivery_id("f1") && m.2.contains("removed")));
        svc.cancel("f2", None).unwrap();

        // A chat that cannot be read is unknown, not gone: nothing is cancelled or fired.
        probes.fail_chat.store(true, Ordering::SeqCst);
        svc.declare(&wait(
            "e1",
            "c6",
            Target::ChatIdle {
                chat_id: "c7".into(),
            },
            t0,
            3_600,
        ))
        .unwrap()
        .unwrap();
        svc.sweep_local(t0 + 200_000);
        svc.sweep_network(t0 + 200_000);
        probes.fail_chat.store(false, Ordering::SeqCst);
        let open: Vec<String> = svc.list(None).unwrap().into_iter().map(|w| w.id).collect();
        assert!(open.contains(&"e1".to_string()) && open.contains(&"p1".to_string()));

        // A running chat reads as busy while its turn lasts; messages never hold it open.
        let real = crate::probes::RealProbes { db: db.clone() };
        assert!(!real.chat("c7").unwrap().unwrap().busy);
        // A turn end the stop judge is still checking reads as busy while the hold is recent, and
        // not once it is old (a crash could leave one behind) or gone.
        let now = helm_core::now_ms();
        let hold = |since: Option<i64>| {
            db.write(|tx| {
                tx.execute("DELETE FROM chat_stop_holds", [])?;
                if let Some(s) = since {
                    tx.execute(
                        "INSERT INTO chat_stop_holds(chat_id, since) VALUES ('c7', ?1)",
                        [s],
                    )?;
                }
                Ok::<_, rusqlite::Error>(())
            })
            .unwrap();
            real.chat("c7").unwrap().unwrap().busy
        };
        assert!(hold(Some(now)));
        assert!(!hold(Some(
            now - helm_chats::runtime::STOP_HOLD_BUSY_MS - 1_000
        )));
        assert!(!hold(None));
        assert!(real.chat("nope").unwrap().is_none());

        // A message sent to it that has not opened its turn yet reads as busy while it is
        // recent, and not once its turn has started; so does a chat whose roll over has started
        // a successor but not yet archived it, until the successor is gone.
        let busy_after = |sql: String| {
            db.write(|tx| tx.execute_batch(&sql)).unwrap();
            real.chat("c7").unwrap().unwrap().busy
        };
        assert!(busy_after(format!(
            "INSERT INTO submissions(id, chat_id, text, state, created_at)
             VALUES ('sent', 'c7', 'hi', 'saved', {now})"
        )));
        assert!(!busy_after(
            "UPDATE submissions SET state = 'complete' WHERE id = 'sent'".into()
        ));
        assert!(busy_after(
            "INSERT INTO chats(id, project_id, status, created_at, updated_at)
             VALUES ('c7next', 'p', 'idle', 1, 1);
             INSERT INTO chat_succession(chat_id, predecessor_id) VALUES ('c7next', 'c7');"
                .into()
        ));
        assert!(!busy_after("DELETE FROM chats WHERE id = 'c7next'".into()));

        // The same read answers the questions the idle rules ask: nothing of its own is
        // working yet and no turn has closed, so c7 starts plain.
        let plain = real.chat("c7").unwrap().unwrap();
        assert!(!plain.helpers && !plain.finished_turn && !plain.unreported);
        // A child that started working gives it helpers; a closed turn makes its idle
        // answer immediate.
        db.write(|tx| {
            tx.execute(
                "INSERT INTO chats(id, project_id, status, parent_chat_id, created_at, updated_at)
                 VALUES ('c7kid', 'p', 'running', 'c7', 1, 1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO runs(id, chat_id, status, started_at, ended_at)
                 VALUES ('r7', 'c7', 'complete', 1, 2)",
                [],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let helped = real.chat("c7").unwrap().unwrap();
        assert!(helped.helpers, "a working child counts");
        assert!(helped.finished_turn, "a closed turn counts");
        // With the child quiet, an open wait the chat declared itself counts as a helper.
        db.write(|tx| {
            tx.execute("UPDATE chats SET status = 'idle' WHERE id = 'c7kid'", [])?;
            tx.execute(
                "INSERT INTO agent_waits(id, chat_id, kind, target, state, created_at, expires_at)
                 VALUES ('aw1', 'c7', 'process', '{}', 'open', 1, 2)",
                [],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        assert!(real.chat("c7").unwrap().unwrap().helpers);
        // The child's own turn never closed and reported: it reads as unreported (its
        // parent is live), which is what the finish message flags.
        assert!(real.chat("c7kid").unwrap().unwrap().unreported);
        db.write(|tx| {
            tx.execute(
                "UPDATE agent_waits SET state = 'fired' WHERE id = 'aw1'",
                [],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        assert!(
            !real.chat("c7").unwrap().unwrap().helpers,
            "a settled wait is not a helper"
        );
    }

    /// A chat idle while its own helpers work is not finished: the wait waits for them, and
    /// when they leave the finish says the ending was unreported. A chat already idle with a
    /// closed turn is answered at once, with `idle` and its state.
    #[test]
    fn waits_wait_for_a_chats_own_helpers_and_answer_at_once_when_it_is_already_idle() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        helm_chats::register_migrations(&mut reg);
        let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        db.write(|tx| {
            tx.execute("INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)", [])?;
            for id in ["watcher", "crew", "quiet"] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 1, 1)",
                    [id],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let probes = Arc::new(Fake::default());
        probes.chats.lock().unwrap().insert(
            "watcher".into(),
            ChatObs {
                helpers: true, // watcher itself is busy behind its helpers
                ..idle()
            },
        );
        probes.chats.lock().unwrap().insert(
            "crew".into(),
            ChatObs {
                helpers: true,
                finished_turn: true,
                unreported: true,
                ..idle()
            },
        );
        probes.chats.lock().unwrap().insert(
            "quiet".into(),
            ChatObs {
                finished_turn: true,
                ..idle()
            },
        );
        let sink = Arc::new(Sink::default());
        let svc = WaitsService::new(db, probes.clone());
        svc.set_sink(sink.clone());
        let t0 = 1_000_000;
        for (id, target) in [("h1", "crew"), ("h2", "quiet")] {
            svc.declare(&wait(
                id,
                "watcher",
                Target::ChatIdle {
                    chat_id: target.into(),
                },
                t0,
                600,
            ))
            .unwrap()
            .unwrap();
        }
        // First sweep: the crew is behind its helpers, so it waits; the already-idle chat
        // answers at once.
        svc.sweep_local(t0 + 2_000);
        assert!(svc
            .list(Some("watcher"))
            .unwrap()
            .iter()
            .any(|w| w.id == "h1"));
        let sent = sink.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "only the already-idle chat answered");
        assert_eq!(sent[0].1, delivery_id("h2"));
        assert!(sent[0].2.contains("chat quiet: idle"), "{}", sent[0].2);
        assert!(
            sent[0].2.contains("its last turn is closed"),
            "{}",
            sent[0].2
        );
        // The crew's helpers finish: now its idle counts, with the unreported flag.
        probes
            .chats
            .lock()
            .unwrap()
            .get_mut("crew")
            .unwrap()
            .helpers = false;
        svc.sweep_local(t0 + 4_000);
        let sent = sink.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].1, delivery_id("h1"));
        assert!(sent[1].2.contains("finished"), "{}", sent[1].2);
        assert!(sent[1].2.contains("idleWithoutWait: true"), "{}", sent[1].2);
        assert!(sent[1].2.contains("steer"), "{}", sent[1].2);
    }
}
