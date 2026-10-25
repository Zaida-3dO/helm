//! The watcher: two loops over the open waits, and the one delivery each fired wait gets.
//!
//! - The local sweep (every 2 s) looks at chats, processes and files, applies timeouts, and
//!   cancels the waits of a chat that was stopped or archived.
//! - The network sweep (every 5 s) polls pull requests that are due: 30 s apart at first,
//!   doubling to 5 minutes, one `gh` call per distinct pull request.
//! - A wait is settled by one conditional update, so it fires at most once. Delivery is a
//!   separate, retried step under a fixed submission id, so a message is never sent twice either.
//!
//! No step here involves a model.

use crate::model::{self, Observation, Target, Wait};
use crate::probes::Probes;
use crate::store::{self, AddError};
use helm_core::db::Database;
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
    /// A passing problem: try again at the next sweep.
    Retry,
}

/// Where a fired wait's message goes.
pub trait Deliver: Send + Sync {
    fn deliver(&self, chat_id: &str, submission_id: &str, text: &str) -> Delivery;
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

    /// Settle `w` if the observation says so, else keep what the watcher learned.
    fn step(&self, mut w: Wait, obs: &Observation, now: i64) {
        let before = w.state.clone();
        match model::judge(&mut w, obs, now) {
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
            None if w.state != before => {
                let state: Value = w.state.clone();
                if let Err(e) = self.db.write(|tx| store::save_memory(tx, &w.id, &state)) {
                    log::warn!("wait {} memory not saved: {e}", w.id);
                }
            }
            None => {}
        }
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

    /// One pass over the waits that need no network.
    pub fn sweep_local(&self, now: i64) {
        let Ok(waits) = self.list(None) else { return };
        let cancelled = self.cancel_orphans(&waits, now);
        for w in waits {
            if w.target.is_network() || cancelled.contains(&w.id) {
                continue;
            }
            let obs = match &w.target {
                Target::ChatIdle { chat_id } => match self.probes.chat(chat_id) {
                    Ok(c) => Observation::Chat(c),
                    Err(_) => Observation::Skipped,
                },
                Target::Process { pid } => Observation::Process(self.probes.process(*pid)),
                Target::File { path } => Observation::File(self.probes.file(path)),
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

    /// One pass over the pull-request waits: the due ones are polled, the rest only time out.
    pub fn sweep_network(&self, now: i64) {
        let Ok(waits) = self.list(None) else { return };
        let waits: Vec<Wait> = waits
            .into_iter()
            .filter(|w| w.target.is_network())
            .collect();
        let cancelled = self.cancel_orphans(&waits, now);
        let mut fetched: HashMap<String, Result<model::PrObs, String>> = HashMap::new();
        for w in waits {
            if cancelled.contains(&w.id) {
                continue;
            }
            let url = match &w.target {
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
            let sid = format!("wait-{}", w.id);
            match sink.deliver(&w.chat_id, &sid, &text) {
                Delivery::Sent | Delivery::Gone => {
                    let _ = self.db.write(|tx| store::mark_delivered(tx, &w.id));
                }
                Delivery::Retry => {}
            }
        }
    }
}

/// Delivery through the chat runtime: a gated app message that starts a turn if the chat is
/// idle and queues behind the running turn if it is busy.
pub struct RuntimeSink(pub helm_chats::ChatRuntime);

impl Deliver for RuntimeSink {
    fn deliver(&self, chat_id: &str, submission_id: &str, text: &str) -> Delivery {
        use helm_chats::submit::{host_send, HostSend, HostSendMode};
        let sent = host_send(
            &self.0,
            HostSend {
                chat_id,
                submission_id,
                text,
                mode: HostSendMode::Queue,
                origin: helm_chats::types::SubmissionOrigin::App,
                gated: true,
            },
        );
        match sent {
            Ok(_) => Delivery::Sent,
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
            queued: false,
        }
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
            assert_eq!(sent[0].1, "wait-a1");
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
        probes.chats.lock().unwrap().get_mut("c2").unwrap().busy = true;
        svc.sweep_local(t0 + 12_000);
        probes.chats.lock().unwrap().get_mut("c2").unwrap().busy = false;
        svc.sweep_local(t0 + 14_000);
        assert!(sink
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.1 == "wait-w1" && m.2.contains("finished")));

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
            .any(|m| m.1 == "wait-t1" && m.2.contains("timeout")));
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

        // A message waiting for a run slot counts as queued, so the chat is not yet idle.
        let real = crate::probes::RealProbes { db: db.clone() };
        assert!(!real.chat("c7").unwrap().unwrap().queued);
        db.write(|tx| {
            tx.execute(
                "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES ('s1', 'c7', 'hi', 'saved', 1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO slot_waits(submission_id, chat_id, since) VALUES ('s1', 'c7', 1)",
                [],
            )
        })
        .unwrap();
        assert!(real.chat("c7").unwrap().unwrap().queued);
        assert!(real.chat("nope").unwrap().is_none());
    }
}
