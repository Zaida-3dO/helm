//! The AFK service: one leaf lock, one transaction per change,
//! a sequence number bumped on every change and one `afk` event after it.
//!
//! It feeds every signal to [`crate::monitor::decide`] and applies the answer, and it runs the
//! digest delivery loop on the shared 30 s tick.

use crate::digest::{self, Step};
use crate::monitor::{self, Action, Cfg, MonitorState, Signal, Source};
use crate::settings::{self, AfkSettings, AutoEnd};
use crate::store::{self, Span, SpanDigests};
use crate::texts;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, EventSink};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// How a digest reaches its chat. The app's sender is `helm_chats::submit::host_send` with origin
/// App and the Exempt admission class; tests supply their own.
pub trait DigestSender: Send + Sync {
    /// `Ok(true)` when it was queued behind a running turn, `Ok(false)` when handed over now.
    /// `Err((code, message))` otherwise.
    fn send(
        &self,
        chat_id: &str,
        submission_id: &str,
        text: &str,
    ) -> Result<bool, (String, String)>;
}

/// How AFK reaches the running agents and the orchestrator. The app installs the real thing;
/// without it AFK still keeps spans and digests but tells nobody.
pub trait AfkHooks: Send + Sync {
    /// Chats whose agent is working now.
    fn busy_chats(&self) -> Vec<String>;
    /// Hand a chat a note, now if it is mid-turn. Idempotent per `submission_id`.
    fn note(&self, chat_id: &str, submission_id: &str, text: &str) -> Result<(), (String, String)>;
    /// AFK just turned on: run the questions already waiting past the automatic answer.
    fn sweep(&self);
    /// AFK just turned on with plan approval set to approve: sign off the plans waiting on the
    /// user. Returns how many.
    fn sign_off_plans(&self, now: i64) -> usize;
}

/// AFK as it stands now, for the chats it applies to.
#[derive(Clone, Debug)]
pub struct Active {
    pub span_id: String,
    pub started_at: i64,
    pub settings: AfkSettings,
}

/// Which chats are waiting on the user right now (a question or an approval).
pub trait AskSource: Send + Sync {
    fn waiting(&self) -> Vec<String>;
}

pub struct NoAsks;
impl AskSource for NoAsks {
    fn waiting(&self) -> Vec<String> {
        Vec::new()
    }
}

#[derive(Default)]
struct Inner {
    mon: MonitorState,
    seq: u64,
    /// When each currently waiting chat was first seen waiting (30 s resolution).
    ask_seen: HashMap<String, i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AfkDefaults {
    pub settings: AfkSettings,
    pub digest_prompt: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AfkView {
    pub settings: AfkSettings,
    pub open: Option<Span>,
    pub seq: u64,
    pub last: Option<SpanDigests>,
    pub failures: Option<SpanDigests>,
    /// Any span ever recorded.
    pub used: bool,
    pub defaults: AfkDefaults,
}

pub struct AfkService {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    sender: Arc<dyn DigestSender>,
    asks: Arc<dyn AskSource>,
    hooks: Mutex<Option<Arc<dyn AfkHooks>>>,
    inner: Mutex<Inner>,
    delivering: Mutex<()>,
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn cfg_of(s: &AfkSettings) -> Cfg {
    Cfg {
        enabled: s.enabled,
        screen_lock: s.auto.screen_lock,
        grace_ms: s.auto.lock_grace_seconds as i64 * 1000,
        sleep: s.auto.sleep,
        unanswered_ms: s.auto.unanswered_minutes.map(|m| m as i64 * 60_000),
        idle_ms: s.auto.idle_minutes.map(|m| m as i64 * 60_000),
        end_manual: s.auto.end == AutoEnd::Manual,
    }
}

impl AfkService {
    pub fn new(
        db: Arc<Database>,
        events: Arc<dyn EventSink>,
        sender: Arc<dyn DigestSender>,
        asks: Arc<dyn AskSource>,
    ) -> Self {
        let mut inner = Inner::default();
        // A span a restart left open carries on.
        if let Ok(Some(s)) = db.snapshot(store::open_span) {
            inner.mon.opened(s.source);
        }
        Self {
            db,
            events,
            sender,
            asks,
            hooks: Mutex::new(None),
            inner: Mutex::new(inner),
            delivering: Mutex::new(()),
        }
    }

    /// Install how AFK reaches the agents (once the app's services exist).
    pub fn set_hooks(&self, h: Arc<dyn AfkHooks>) {
        *self.hooks.lock().unwrap_or_else(|p| p.into_inner()) = Some(h);
    }

    fn hooks(&self) -> Option<Arc<dyn AfkHooks>> {
        self.hooks.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// AFK for every chat, if it is on: enabled, in all-chats mode, with a span open. One-chat
    /// mode is not built, so it never counts here.
    pub fn active(&self) -> Option<Active> {
        self.db
            .snapshot(|tx| {
                let s = store::load_settings(tx)?;
                if !s.enabled || s.applies_to != settings::AppliesTo::All {
                    return Ok::<_, rusqlite::Error>(None);
                }
                Ok(store::open_span(tx)?.map(|sp| Active {
                    span_id: sp.id,
                    started_at: sp.started_at,
                    settings: s,
                }))
            })
            .ok()
            .flatten()
    }

    /// Whether plans that wait on the user's sign-off should be signed off for them now.
    pub fn approves_plans(&self) -> bool {
        self.active()
            .is_some_and(|a| a.settings.plan_approval == settings::PlanApproval::Approve)
    }

    /// Keep what AFK did with one ask, for the digest. Never fails the caller.
    pub fn record(&self, span_id: &str, chat_id: &str, kind: &str, title: &str, now: i64) {
        let title: String = title.chars().take(500).collect();
        if let Err(e) = self
            .db
            .write(|tx| store::record_event(tx, span_id, chat_id, kind, &title, now))
        {
            log::warn!("AFK event not kept: {e}");
        }
    }

    fn open_now(&self) -> Option<Span> {
        self.db.snapshot(store::open_span).ok().flatten()
    }

    /// After a change: tell the agents when a span opened or closed. Never under the leaf lock.
    fn announce(&self, before: Option<Span>, now: i64) {
        let after = self.open_now();
        if before.as_ref().map(|s| &s.id) == after.as_ref().map(|s| &s.id) {
            return;
        }
        if let Some(b) = before {
            self.announce_off(&b.id);
        }
        if let Some(a) = after {
            self.announce_on(&a, now);
        }
    }

    fn announce_on(&self, span: &Span, now: i64) {
        let (Some(hooks), Some(active)) = (self.hooks(), self.active()) else {
            return;
        };
        let approve = active.settings.plan_approval == settings::PlanApproval::Approve;
        let text = texts::on_note(span.started_at, approve);
        for chat in hooks.busy_chats() {
            let id = format!("afk-note:{}:on:{chat}", span.id);
            if let Err((code, msg)) = hooks.note(&chat, &id, &text) {
                log::info!("AFK note to {chat} not delivered ({code}): {msg}");
            }
        }
        hooks.sweep();
        if approve {
            hooks.sign_off_plans(now);
        }
    }

    /// The chats that get a digest hear the user is back from it; the other working chats get a
    /// short note.
    fn announce_off(&self, span_id: &str) {
        let Some(hooks) = self.hooks() else { return };
        let digested: Vec<String> = self
            .db
            .snapshot(|tx| {
                let mut st = tx.prepare("SELECT chat_id FROM afk_digests WHERE span_id = ?1")?;
                let rows = st.query_map([span_id], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
            })
            .unwrap_or_default();
        let text = texts::off_note();
        for chat in hooks.busy_chats() {
            if digested.contains(&chat) {
                continue;
            }
            let id = format!("afk-note:{span_id}:off:{chat}");
            if let Err((code, msg)) = hooks.note(&chat, &id, &text) {
                log::info!("AFK note to {chat} not delivered ({code}): {msg}");
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn settings(&self) -> AfkSettings {
        self.db.snapshot(store::load_settings).unwrap_or_default()
    }

    pub fn view(&self) -> Result<AfkView, ApiError> {
        let seq = self.lock().seq;
        self.db
            .snapshot(|tx| {
                Ok::<_, rusqlite::Error>(AfkView {
                    settings: store::load_settings(tx)?,
                    open: store::open_span(tx)?,
                    seq,
                    last: store::last_closed(tx)?,
                    failures: store::failures(tx)?,
                    used: store::any_span(tx)?,
                    defaults: AfkDefaults {
                        settings: AfkSettings::default(),
                        digest_prompt: digest::DEFAULT_TEMPLATE,
                    },
                })
            })
            .map_err(sql)
    }

    fn publish(&self) {
        if let Ok(v) = self.view() {
            if let Ok(Value::Object(m)) = serde_json::to_value(&v) {
                let mut e = AppEvent::new("afk");
                for (k, v) in m {
                    e = e.with(&k, v);
                }
                self.events.emit(e);
            }
        }
    }

    /// Open a span (if none is open and AFK is on). Under the leaf lock.
    fn open(&self, inner: &mut Inner, src: Source, now: i64) -> Result<bool, ApiError> {
        let opened = self
            .db
            .write(|tx| {
                if !store::load_settings(tx)?.enabled || store::open_span(tx)?.is_some() {
                    return Ok::<_, rusqlite::Error>(false);
                }
                store::insert_span(tx, &uuid::Uuid::new_v4().to_string(), src, now)?;
                Ok(true)
            })
            .map_err(sql)?;
        if opened {
            inner.mon.opened(src);
            inner.seq += 1;
            log::info!("AFK on ({})", src.as_str());
        }
        Ok(opened)
    }

    /// Close the open span, whatever opened it. Under the leaf lock.
    fn close(&self, inner: &mut Inner, now: i64) -> Result<bool, ApiError> {
        let closed = self
            .db
            .write(|tx| store::close_span(tx, now))
            .map_err(sql)?;
        let Some((span, n)) = closed else {
            inner.mon.open = None;
            return Ok(false);
        };
        inner.mon.closed(span.ended_at.unwrap_or(now));
        inner.seq += 1;
        log::info!("AFK off (span {}, {n} digests)", span.id);
        Ok(true)
    }

    fn apply(&self, inner: &mut Inner, action: Action, now: i64) -> Result<bool, ApiError> {
        match action {
            Action::On(src) => self.open(inner, src, now),
            Action::OffIf(src) => {
                if inner.mon.open == Some(src) {
                    self.close(inner, now)
                } else {
                    Ok(false)
                }
            }
        }
    }

    /// Feed one signal to the decision function and apply its answer.
    pub fn signal(&self, sig: Signal, now: i64) {
        let cfg = cfg_of(&self.settings());
        let before = self.open_now();
        let changed = {
            let mut inner = self.lock();
            match monitor::decide(&mut inner.mon, &cfg, sig) {
                Some(a) => self.apply(&mut inner, a, now).unwrap_or_else(|e| {
                    log::warn!("AFK change failed: {}", e.message);
                    false
                }),
                None => false,
            }
        };
        if changed {
            self.publish();
            self.announce(before, now);
            self.deliver(now);
        }
    }

    /// The 30 s tick: auto-AFK, then digest delivery.
    pub fn tick(&self, now: i64, idle_secs: Option<u64>) {
        let waiting = self.asks.waiting();
        let oldest = {
            let mut inner = self.lock();
            inner.ask_seen.retain(|c, _| waiting.contains(c));
            for c in &waiting {
                inner.ask_seen.entry(c.clone()).or_insert(now);
            }
            inner.ask_seen.values().copied().min()
        };
        self.signal(
            Signal::Tick {
                now,
                idle_secs,
                oldest_ask: oldest,
            },
            now,
        );
        self.deliver(now);
    }

    /// The user's switch (afk_set): on opens a manual span; off ends any open span.
    pub fn set(&self, on: bool, now: i64) -> Result<AfkView, ApiError> {
        let before = self.open_now();
        let changed = {
            let mut inner = self.lock();
            if on {
                if !self.settings().enabled {
                    return Err(ApiError::new(
                        "refused",
                        "Turn AFK on in Settings → AFK first.",
                    ));
                }
                self.open(&mut inner, Source::Manual, now)?
            } else {
                self.close(&mut inner, now)?
            }
        };
        if changed {
            self.publish();
            self.announce(before, now);
            self.deliver(now);
        }
        self.view()
    }

    /// A strict save. Turning AFK off, or changing whom it applies to, ends the open span with its
    /// digests. Any save forgets a recorded lock: the user is evidently at the machine.
    pub fn save_settings(&self, v: &Value, now: i64) -> Result<AfkView, ApiError> {
        let new = settings::parse_strict(v).map_err(ApiError::bad_args)?;
        let before = self.open_now();
        {
            let mut inner = self.lock();
            let old = self.settings();
            self.db
                .write(|tx| store::save_settings(tx, &new))
                .map_err(sql)?;
            inner.mon.lock_at = None;
            if (old.enabled && !new.enabled) || old.applies_to != new.applies_to {
                self.close(&mut inner, now)?;
            }
            inner.seq += 1;
        }
        self.publish();
        self.announce(before, now);
        self.deliver(now);
        self.view()
    }

    /// The UI's presence report: window focus and in-window input.
    pub fn presence(&self, focused: Option<bool>, input: bool, now: i64) {
        if let Some(f) = focused {
            self.signal(Signal::Focused(f), now);
        }
        if input {
            self.signal(Signal::WindowInput { at: now }, now);
        }
    }

    pub fn retry(&self, span_id: &str, now: i64) -> Result<AfkView, ApiError> {
        self.db
            .write(|tx| store::retry_failed(tx, span_id))
            .map_err(sql)?;
        self.publish();
        self.deliver(now);
        self.view()
    }

    pub fn dismiss(&self, span_id: &str) -> Result<AfkView, ApiError> {
        self.db
            .write(|tx| store::dismiss_failed(tx, span_id))
            .map_err(sql)?;
        self.publish();
        self.view()
    }

    /// One pass of the delivery loop over every live row. Never two passes at once.
    pub fn deliver(&self, now: i64) {
        let Ok(_guard) = self.delivering.try_lock() else {
            return;
        };
        let rows = match self.db.snapshot(store::live_digests) {
            Ok(r) => r,
            Err(e) => return log::warn!("AFK digests unreadable: {e}"),
        };
        if rows.is_empty() {
            return;
        }
        let template = self.settings().digest_prompt;
        let mut changed = false;
        for row in rows {
            changed |= self.deliver_one(&row, template.as_deref(), now);
        }
        if changed {
            self.publish();
        }
    }

    fn deliver_one(&self, row: &store::DigestRow, template: Option<&str>, now: i64) -> bool {
        let (sub_state, available, span, deferred) = match self.db.snapshot(|tx| {
            let s = match &row.submission_id {
                Some(id) => store::submission_state(tx, id)?,
                None => None,
            };
            let span = store::span(tx, &row.span_id)?;
            let deferred = match &span {
                Some(sp) => digest::deferred_lines(
                    &store::chat_events(tx, &sp.id, &row.chat_id)?,
                    &store::undecided_allows(tx, &row.chat_id, sp.started_at),
                ),
                None => vec![],
            };
            Ok::<_, rusqlite::Error>((s, store::chat_available(tx, &row.chat_id)?, span, deferred))
        }) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("AFK digest read failed: {e}");
                return false;
            }
        };
        let mark = |state: &str, err: Option<&str>| {
            self.db
                .write(|tx| store::set_digest_state(tx, &row.span_id, &row.chat_id, state, err))
                .map_err(|e| log::warn!("AFK digest update failed: {e}"))
                .is_ok()
        };
        match digest::step(
            &row.state,
            row.attempts,
            row.next_attempt_at,
            sub_state.as_deref(),
            now,
        ) {
            Step::Wait => false,
            Step::Mark(state, err) => mark(state, err),
            Step::Send { attempt } => {
                let Some(span) = span else {
                    return mark("dismissed", None);
                };
                if !available {
                    return mark(
                        "failed",
                        Some("The chat is unavailable (archived or deleted)."),
                    );
                }
                let id = digest::submission_id(&row.span_id, &row.chat_id, attempt);
                if self
                    .db
                    .write(|tx| store::begin_attempt(tx, &row.span_id, &row.chat_id, &id))
                    .is_err()
                {
                    return false;
                }
                let text = digest::compose(
                    template,
                    &span.id,
                    span.started_at,
                    span.ended_at.unwrap_or(now),
                    &deferred,
                );
                match self.sender.send(&row.chat_id, &id, &text) {
                    Ok(true) => mark("queued", None),
                    Ok(false) => true,
                    Err((code, msg)) if code == "not_found" || code == "refused" => {
                        log::info!("AFK digest for {} not deliverable: {msg}", row.chat_id);
                        mark(
                            "failed",
                            Some("The chat is unavailable (archived or deleted)."),
                        )
                    }
                    Err((_, msg)) => {
                        let at = now + digest::backoff_ms(attempt);
                        let err: String = msg.chars().take(240).collect();
                        self.db
                            .write(|tx| {
                                store::retry_later(tx, &row.span_id, &row.chat_id, &err, at)
                            })
                            .is_ok()
                    }
                }
            }
        }
    }

    /// The open span's source, for tests and the tray-free signal thread.
    pub fn open_source(&self) -> Option<Source> {
        self.lock().mon.open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;
    use serde_json::json;
    use std::sync::Mutex as M;

    #[derive(Default)]
    struct Sent {
        calls: M<Vec<(String, String, String)>>,
        fail: M<Option<(String, String)>>,
        queue: M<bool>,
    }
    impl DigestSender for Sent {
        fn send(&self, chat: &str, id: &str, text: &str) -> Result<bool, (String, String)> {
            self.calls
                .lock()
                .unwrap()
                .push((chat.into(), id.into(), text.into()));
            if let Some(e) = self.fail.lock().unwrap().clone() {
                return Err(e);
            }
            Ok(*self.queue.lock().unwrap())
        }
    }

    struct Asks(M<Vec<String>>);
    impl AskSource for Asks {
        fn waiting(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    fn setup() -> (
        tempfile::TempDir,
        Arc<Database>,
        Arc<Sent>,
        Arc<Asks>,
        AfkService,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        reg.initializer("afk", store::init);
        let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects (id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats (id, project_id, title, created_at, updated_at) VALUES
                   ('c1', 'p', 'One', 0, 2), ('c2', 'p', 'Two', 0, 1), ('c3', 'p', 'Idle', 0, 3);",
            )
        })
        .unwrap();
        let sent = Arc::new(Sent::default());
        let asks = Arc::new(Asks(M::new(vec![])));
        let svc = AfkService::new(
            db.clone(),
            Arc::new(helm_ipc::NullSink),
            sent.clone(),
            asks.clone(),
        );
        (dir, db, sent, asks, svc)
    }

    fn run(db: &Database, chat: &str, start: i64, end: Option<i64>) {
        db.write(|tx| {
            tx.execute(
                "INSERT INTO runs (id, chat_id, status, started_at, ended_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    chat,
                    if end.is_some() { "ended" } else { "running" },
                    start,
                    end
                ],
            )
        })
        .unwrap();
    }

    fn sub_state(db: &Database, id: &str, chat: &str, state: &str) {
        db.write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO submissions (id, chat_id, text, state, created_at) VALUES (?1, ?2, 'x', ?3, 0)",
                rusqlite::params![id, chat, state],
            )
        })
        .unwrap();
    }

    fn digest_states(db: &Database) -> Vec<(String, String, u32)> {
        db.snapshot(|tx| {
            let mut st =
                tx.prepare("SELECT chat_id, state, attempts FROM afk_digests ORDER BY chat_id")?;
            let r = st.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u32))
            })?;
            r.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
    }

    const MIN: i64 = 60_000;

    fn v_span(db: &Database) -> String {
        db.snapshot(|tx| tx.query_row("SELECT id FROM afk_spans", [], |r| r.get::<_, String>(0)))
            .unwrap()
    }

    #[test]
    fn switch_refused_until_enabled() {
        let (_d, _db, _s, _a, svc) = setup();
        assert_eq!(svc.set(true, 0).unwrap_err().code, "refused");
        svc.save_settings(&json!({"enabled": true}), 0).unwrap();
        assert!(svc.set(true, 0).unwrap().open.is_some());
    }

    #[test]
    fn closing_a_span_sends_digests_to_chats_that_worked() {
        let (_d, db, sent, _a, svc) = setup();
        svc.save_settings(&json!({"enabled": true}), 0).unwrap();
        let start = 1_000 * MIN;
        svc.set(true, start).unwrap();
        run(&db, "c1", start + MIN, Some(start + 2 * MIN));
        run(&db, "c2", start - 10 * MIN, None); // crash-left, still marked running
        run(&db, "c3", start - 10 * MIN, Some(start - 5 * MIN)); // before the span
        let v = svc.set(false, start + 10 * MIN).unwrap();
        assert!(v.open.is_none());
        let calls = sent.calls.lock().unwrap().clone();
        let chats: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(chats, ["c1", "c2"], "newest first, c3 did not work");
        assert!(calls[0].2.starts_with("[Helm · Back from AFK · span "));
        assert_eq!(calls[0].1, digest::submission_id(&v_span(&db), "c1", 1));
        // Delivered now: still pending until the submission says it arrived.
        sub_state(&db, &calls[0].1, "c1", "delivered");
        sub_state(&db, &calls[1].1, "c2", "withdrawn");
        svc.deliver(start + 11 * MIN);
        assert_eq!(
            digest_states(&db),
            [
                ("c1".into(), "sent".into(), 1),
                ("c2".into(), "dismissed".into(), 1)
            ]
        );
    }

    #[test]
    fn accidental_spans_get_no_digest() {
        let (_d, db, sent, _a, svc) = setup();
        svc.save_settings(&json!({"enabled": true}), 0).unwrap();
        svc.set(true, 0).unwrap();
        run(&db, "c1", 1, Some(2));
        svc.set(false, MIN - 1).unwrap();
        assert!(sent.calls.lock().unwrap().is_empty());
        // Exactly a minute is not accidental for a manual span.
        svc.set(true, 10 * MIN).unwrap();
        run(&db, "c1", 10 * MIN + 1, Some(10 * MIN + 2));
        svc.set(false, 11 * MIN).unwrap();
        assert_eq!(sent.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn failed_sends_back_off_and_give_up_after_five() {
        let (_d, db, sent, _a, svc) = setup();
        svc.save_settings(&json!({"enabled": true}), 0).unwrap();
        svc.set(true, 0).unwrap();
        run(&db, "c1", 1, Some(2));
        *sent.fail.lock().unwrap() = Some(("capacity".into(), "Too many chats.".into()));
        let mut t = 10 * MIN;
        svc.set(false, t).unwrap();
        assert_eq!(sent.calls.lock().unwrap().len(), 1);
        // Not due again until 5 s later.
        svc.deliver(t + 4_999);
        assert_eq!(sent.calls.lock().unwrap().len(), 1);
        for wait in [5_000, 15_000, 60_000, 300_000] {
            t += wait;
            svc.deliver(t);
        }
        assert_eq!(sent.calls.lock().unwrap().len(), 5);
        svc.deliver(t + 1_000_000);
        assert_eq!(sent.calls.lock().unwrap().len(), 5);
        assert_eq!(digest_states(&db), [("c1".into(), "failed".into(), 5)]);
        let f = svc.view().unwrap().failures.expect("failures surface");
        assert_eq!(
            f.digests[0].error.as_deref(),
            Some("Gave up after 5 tries.")
        );
        // Retry gives fresh attempts; Dismiss clears the surface.
        *sent.fail.lock().unwrap() = None;
        svc.retry(&f.span.id, t + 2_000_000).unwrap();
        assert_eq!(sent.calls.lock().unwrap().len(), 6);
        assert!(svc.view().unwrap().failures.is_none());
    }

    #[test]
    fn unavailable_chat_fails_without_retrying() {
        let (_d, db, sent, _a, svc) = setup();
        svc.save_settings(&json!({"enabled": true}), 0).unwrap();
        svc.set(true, 0).unwrap();
        run(&db, "c1", 1, Some(2));
        *sent.fail.lock().unwrap() = Some(("not_found".into(), "gone".into()));
        svc.set(false, 10 * MIN).unwrap();
        assert_eq!(digest_states(&db), [("c1".into(), "failed".into(), 1)]);
    }

    #[test]
    fn queued_digest_turns_sent_when_its_submission_runs() {
        let (_d, db, sent, _a, svc) = setup();
        svc.save_settings(&json!({"enabled": true}), 0).unwrap();
        svc.set(true, 0).unwrap();
        run(&db, "c1", 1, None);
        *sent.queue.lock().unwrap() = true;
        svc.set(false, 10 * MIN).unwrap();
        assert_eq!(digest_states(&db)[0].1, "queued");
        let id = sent.calls.lock().unwrap()[0].1.clone();
        sub_state(&db, &id, "c1", "queued");
        svc.deliver(11 * MIN);
        assert_eq!(digest_states(&db)[0].1, "queued");
        sub_state(&db, &id, "c1", "complete");
        svc.deliver(12 * MIN);
        assert_eq!(digest_states(&db)[0].1, "sent");
        assert_eq!(sent.calls.lock().unwrap().len(), 1, "never sent twice");
    }

    #[test]
    fn turning_afk_off_in_settings_ends_the_span() {
        let (_d, _db, _s, _a, svc) = setup();
        svc.save_settings(&json!({"enabled": true}), 0).unwrap();
        svc.set(true, 0).unwrap();
        let v = svc.save_settings(&json!({"enabled": false}), MIN).unwrap();
        assert!(v.open.is_none());
        assert_eq!(svc.open_source(), None);
    }

    #[test]
    fn signals_drive_spans_end_to_end() {
        let (_d, _db, _s, asks, svc) = setup();
        svc.save_settings(
            &json!({"enabled": true, "auto": {"unansweredMinutes": 5, "idleMinutes": null}}),
            0,
        )
        .unwrap();
        svc.signal(Signal::Locked { at: 10 }, 10);
        assert_eq!(svc.open_source(), Some(Source::ScreenLock));
        svc.signal(Signal::Unlocked, 20);
        assert_eq!(svc.open_source(), None);
        // A question waiting five minutes (tracked from first sight on a tick).
        *asks.0.lock().unwrap() = vec!["c1".into()];
        svc.tick(100 * MIN, Some(0));
        assert_eq!(svc.open_source(), None);
        svc.tick(105 * MIN, Some(0));
        assert_eq!(svc.open_source(), Some(Source::Unanswered));
    }

    #[test]
    fn an_open_span_survives_a_restart() {
        let (_d, db, sent, asks, svc) = setup();
        svc.save_settings(&json!({"enabled": true}), 0).unwrap();
        svc.signal(Signal::WillSleep { sent_at: 5 }, 5);
        drop(svc);
        let again = AfkService::new(db, Arc::new(helm_ipc::NullSink), sent, asks);
        assert_eq!(again.open_source(), Some(Source::Sleep));
    }

    #[derive(Default)]
    struct Hooks {
        busy: M<Vec<String>>,
        notes: M<Vec<(String, String, String)>>,
        sweeps: M<u32>,
        signed: M<u32>,
    }
    impl AfkHooks for Hooks {
        fn busy_chats(&self) -> Vec<String> {
            self.busy.lock().unwrap().clone()
        }
        fn note(&self, chat: &str, id: &str, text: &str) -> Result<(), (String, String)> {
            self.notes
                .lock()
                .unwrap()
                .push((chat.into(), id.into(), text.into()));
            Ok(())
        }
        fn sweep(&self) {
            *self.sweeps.lock().unwrap() += 1;
        }
        fn sign_off_plans(&self, _now: i64) -> usize {
            *self.signed.lock().unwrap() += 1;
            0
        }
    }

    #[test]
    fn agents_are_told_when_afk_starts_and_ends_and_the_digest_lists_what_waited() {
        let (_d, db, sent, _a, svc) = setup();
        let hooks = Arc::new(Hooks::default());
        *hooks.busy.lock().unwrap() = vec!["c1".into(), "c2".into()];
        svc.set_hooks(hooks.clone());
        svc.save_settings(&json!({"enabled": true, "planApproval": "approve"}), 0)
            .unwrap();
        assert!(!svc.approves_plans(), "not before a span is open");
        let start = 1_000 * MIN;
        svc.set(true, start).unwrap();
        assert!(svc.approves_plans());
        {
            let notes = hooks.notes.lock().unwrap();
            assert_eq!(notes.len(), 2, "both working chats hear it start");
            assert!(
                notes[0].2.contains("AFK is on") && notes[0].2.contains("approved automatically")
            );
            assert!(notes[0].1.starts_with("afk-note:") && notes[0].1.ends_with(":on:c1"));
        }
        assert_eq!(
            (*hooks.sweeps.lock().unwrap(), *hooks.signed.lock().unwrap()),
            (1, 1)
        );

        // What AFK did while away: it counts even though the span is otherwise accidental.
        let span = v_span(&db);
        svc.record(&span, "c1", "question", "Which database?", start + 10_000);
        svc.record(
            &span,
            "c1",
            "approval_deferred",
            "Allow Bash? rm -rf x",
            start + 20_000,
        );
        svc.record(
            &span,
            "c3",
            "plan_deferred",
            "approval of the plan",
            start + 30_000,
        );
        db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE consent_requests (chat_id TEXT, summary TEXT, state TEXT, created_at INTEGER);
                 INSERT INTO consent_requests VALUES ('c1', 'Allow the repo server', 'pending', 1000000000);
                 INSERT INTO consent_requests VALUES ('c1', 'Old one', 'denied', 1000000000);",
            )
        })
        .unwrap();
        svc.set(false, start + 40_000).unwrap();
        let calls = sent.calls.lock().unwrap().clone();
        let mut chats: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
        chats.sort();
        assert_eq!(
            chats,
            ["c1", "c3"],
            "neither ran, but AFK handled something for both"
        );
        let c1 = &calls.iter().find(|c| c.0 == "c1").unwrap().2;
        assert!(c1.contains("Answered a question automatically: \"Which database?\""));
        assert!(c1.contains("Waiting for you: Allow Bash? rm -rf x"));
        assert!(c1.contains("Allow request waiting for you: Allow the repo server"));
        assert!(!c1.contains("Old one"));
        // c1 and c3 get the digest; c2 is working but gets the short note.
        let off: Vec<_> = hooks
            .notes
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.1.contains(":off:"))
            .map(|n| n.0.clone())
            .collect();
        assert_eq!(off, ["c2"]);
    }

    #[test]
    fn asks_are_answered_or_left_by_kind() {
        use crate::policy::decide_with;
        use helm_chats::seams::AfkAction;
        let active = |plan: &str, answer: Option<&str>| Active {
            span_id: "s".into(),
            started_at: 0,
            settings: settings::parse_strict(&json!({
                "enabled": true, "planApproval": plan, "answer": answer
            }))
            .unwrap(),
        };
        let ask = |v: Value| -> helm_chats::types::Interaction {
            let mut o = json!({"chatId": "c", "id": "r", "method": "select"});
            o.as_object_mut()
                .unwrap()
                .extend(v.as_object().unwrap().clone());
            serde_json::from_value(o).unwrap()
        };
        let kept = M::new(Vec::<(String, String)>::new());
        let keep = |k: &str, w: &str| kept.lock().unwrap().push((k.into(), w.into()));
        let plan = json!({"attention": "plan", "options": ["Approve plan", "Keep planning"]});
        let cases = [
            // (ask, plan setting, at flip, expect answered, kept kind)
            (
                json!({"attention": "question", "title": "Which?"}),
                "defer",
                false,
                true,
                "question",
            ),
            (
                json!({"attention": "question", "secret": true}),
                "defer",
                false,
                false,
                "",
            ),
            (plan.clone(), "approve", false, true, "plan_approved"),
            (plan.clone(), "approve", true, false, "plan_deferred"),
            (plan.clone(), "defer", false, false, "plan_deferred"),
            (
                json!({"attention": "plan", "options": ["Keep planning"]}),
                "approve",
                false,
                false,
                "plan_deferred",
            ),
            (
                json!({"attention": "approval", "title": "Allow Bash?", "context": [{"label": "Command", "text": "npm test"}]}),
                "approve",
                false,
                false,
                "approval_deferred",
            ),
            (
                json!({"attention": "trust", "title": "Trust this folder?"}),
                "approve",
                false,
                false,
                "approval_deferred",
            ),
            (
                json!({"title": "Pick a file", "method": "input"}),
                "approve",
                false,
                false,
                "",
            ),
        ];
        for (n, (v, plan_setting, flip, answered, kind)) in cases.into_iter().enumerate() {
            kept.lock().unwrap().clear();
            let act = decide_with(
                &active(plan_setting, Some("Go with the default.")),
                &ask(v),
                flip,
                &keep,
            );
            let got = kept.lock().unwrap().clone();
            match (&act, answered) {
                (AfkAction::Answer { fields, marker }, true) => {
                    assert!(marker.starts_with("AFK "), "case {n}");
                    if kind == "question" {
                        let text = fields["value"].as_str().unwrap();
                        assert!(
                            text.starts_with("Go with the default.")
                                && text.ends_with(texts::SAFETY)
                        );
                    } else {
                        assert_eq!(fields["confirmed"], json!(true), "case {n}");
                    }
                }
                (AfkAction::Defer { .. }, false) => assert!(!kind.is_empty(), "case {n}"),
                (AfkAction::Leave, false) => assert!(kind.is_empty(), "case {n}"),
                (other, _) => panic!("case {n}: {other:?}"),
            }
            assert_eq!(
                got.first().map(|k| k.0.as_str()).unwrap_or(""),
                kind,
                "case {n}"
            );
        }
    }
}
