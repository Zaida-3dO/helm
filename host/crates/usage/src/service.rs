//! The meter's host service: one place that holds the reading, runs the control reader at
//! the allowed cadence and merges what chats report.
//!
//! - A read starts on launch, every 10 minutes while the window is visible, and on Refresh. The UI
//!   asks (`usage_refresh`); the host enforces the floor: **never more often than once per 60 s**,
//!   and never two at once.
//! - A read runs in a short-lived helper (`helpers/usage`) on a thread of its own; the op answers
//!   at once and the result arrives as the `plan_usage` event.
//! - Whatever the helper cannot read is a readiness state with a reason, never an error toast.

use crate::store::{self, Passive};
use crate::types::*;
use helm_core::db::Database;
use helm_ipc::{AppEvent, EventSink};
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// The floor between two reads.
pub const MIN_GAP_MS: i64 = 60_000;

/// What the control reader answered, as the helper prints it.
#[derive(Clone, Debug, PartialEq)]
pub enum ControlAnswer {
    Reading {
        plan: Option<String>,
        five_hour: Option<(Option<f64>, Option<i64>)>,
        weekly: Option<(Option<f64>, Option<i64>)>,
        per_model: Vec<(String, Option<f64>, Option<i64>)>,
        version: Option<String>,
    },
    /// `no_binary`, `unsupported`, `no_limits`, `timeout` or `error`.
    Failed {
        reason: String,
        message: String,
        version: Option<String>,
    },
}

/// Runs one control read. The app's reader starts the helper; tests supply their own.
pub trait PlanReader: Send + Sync {
    fn read(&self) -> ControlAnswer;
}

fn window(v: &Value) -> Option<(Option<f64>, Option<i64>)> {
    if v.is_null() {
        return None;
    }
    Some((
        v.get("pct").and_then(Value::as_f64),
        v.get("resetsAt").and_then(Value::as_i64),
    ))
}

/// Parse the helper's one JSON line.
pub fn parse_answer(v: &Value) -> ControlAnswer {
    let version = v.get("version").and_then(Value::as_str).map(str::to_owned);
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        let r = &v["reading"];
        return ControlAnswer::Reading {
            plan: r.get("plan").and_then(Value::as_str).map(str::to_owned),
            five_hour: window(&r["fiveHour"]),
            weekly: window(&r["weekly"]),
            per_model: r["perModel"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|m| {
                            Some((
                                m.get("model")?.as_str()?.to_owned(),
                                m.get("pct").and_then(Value::as_f64),
                                m.get("resetsAt").and_then(Value::as_i64),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            version,
        };
    }
    ControlAnswer::Failed {
        reason: v
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_owned(),
        message: v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("The usage helper gave no answer.")
            .to_owned(),
        version,
    }
}

#[derive(Default)]
struct State {
    stored: StoredUsage,
    running: bool,
    last_attempt: Option<i64>,
    /// The last control read's failure, cleared by the next success.
    failure: Option<(String, String)>,
    version: Option<String>,
}

pub struct UsageService {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    reader: Arc<dyn PlanReader>,
    state: Mutex<State>,
}

impl UsageService {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>, reader: Arc<dyn PlanReader>) -> Self {
        let stored = store::load(&db);
        Self {
            db,
            events,
            reader,
            state: Mutex::new(State {
                stored,
                ..State::default()
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn snapshot(&self, now: i64) -> UsageSnapshot {
        let st = self.lock();
        snapshot_of(&st, now)
    }

    fn emit(&self, snap: &UsageSnapshot) {
        if let Ok(Value::Object(m)) = serde_json::to_value(snap) {
            let mut e = AppEvent::new("plan_usage");
            for (k, v) in m {
                e = e.with(&k, v);
            }
            self.events.emit(e);
        }
    }

    /// Windows a chat's worker reported from `rate_limit_event` lines.
    pub fn ingest_passive(&self, windows: &[Passive], now: i64) {
        let snap = {
            let mut st = self.lock();
            if !store::apply_passive(&mut st.stored, windows, now) {
                return;
            }
            store::save(&self.db, &st.stored, now);
            snapshot_of(&st, now)
        };
        self.emit(&snap);
    }

    fn finish_control(&self, answer: ControlAnswer, now: i64) {
        let snap = {
            let mut st = self.lock();
            st.running = false;
            match answer {
                ControlAnswer::Reading {
                    plan,
                    five_hour,
                    weekly,
                    per_model,
                    version,
                } => {
                    st.failure = None;
                    if version.is_some() {
                        st.version = version;
                    }
                    store::apply_control(&mut st.stored, plan, five_hour, weekly, per_model, now);
                    store::save(&self.db, &st.stored, now);
                }
                ControlAnswer::Failed {
                    reason,
                    message,
                    version,
                } => {
                    if version.is_some() {
                        st.version = version;
                    }
                    st.failure = Some((reason, message));
                }
            }
            snapshot_of(&st, now)
        };
        self.emit(&snap);
    }

    /// Start a read when the floor allows and none is running. Always answers with the current
    /// snapshot; the result of a started read arrives as the `plan_usage` event.
    pub fn refresh(self: &Arc<Self>, now: i64) -> UsageSnapshot {
        let start = {
            let mut st = self.lock();
            let due = st.last_attempt.is_none_or(|t| now - t >= MIN_GAP_MS);
            if st.running || !due {
                false
            } else {
                st.running = true;
                st.last_attempt = Some(now);
                true
            }
        };
        if start {
            let me = self.clone();
            let spawned = std::thread::Builder::new()
                .name("plan-usage".into())
                .spawn(move || {
                    let answer = me.reader.read();
                    me.finish_control(answer, helm_core::now_ms());
                });
            if spawned.is_err() {
                self.finish_control(
                    ControlAnswer::Failed {
                        reason: "error".into(),
                        message: "Helm could not start the usage reader.".into(),
                        version: None,
                    },
                    now,
                );
            }
        }
        self.snapshot(now)
    }
}

fn snapshot_of(st: &State, now: i64) -> UsageSnapshot {
    let reading = store::reading(&st.stored, now);
    let readiness = readiness(
        &st.failure,
        st.version.as_deref(),
        reading.is_some(),
        st.running,
        st.last_attempt.is_some(),
    );
    UsageSnapshot {
        reading,
        readiness,
        refreshing: st.running,
        next_refresh_at: st.last_attempt.map(|t| t + MIN_GAP_MS).filter(|t| *t > now),
    }
}

const WHERE_TO_ACT: &str = "Open Settings → Usage";
const ACT_TARGET: &str = "settings:usage";

/// The readiness rule: with no reading, say why and where to act.
fn readiness(
    failure: &Option<(String, String)>,
    version: Option<&str>,
    have_reading: bool,
    running: bool,
    tried: bool,
) -> UsageReadiness {
    let mk = |state, reason: String, act: bool| UsageReadiness {
        state,
        reason,
        action_label: act.then(|| WHERE_TO_ACT.to_owned()),
        action_target: act.then(|| ACT_TARGET.to_owned()),
        binary_version: version.map(str::to_owned),
    };
    let with_version = |s: &str| match version {
        Some(v) => format!("{s} (Claude Code {v})"),
        None => s.to_owned(),
    };
    match failure {
        Some((reason, message)) => {
            let text = match reason.as_str() {
                "unsupported" if have_reading => {
                    with_version("This Claude Code version did not report plan usage; showing the last reading from a chat")
                }
                "unsupported" => with_version(
                    "This Claude Code version did not report plan usage. Update Claude Code, or start a chat: its rate-limit messages fill the meter",
                ),
                "no_binary" => "Helm could not find Claude Code, so it cannot read plan usage. Install it, or set its path in Settings".to_owned(),
                "no_limits" => with_version(
                    "Claude Code reports no plan limits for this sign-in (an API key or another provider has none). The meter is off",
                ),
                "timeout" => "Claude Code did not answer the usage request in time. Try Refresh".to_owned(),
                _ => format!("Plan usage could not be read: {message}"),
            };
            let state = if have_reading {
                ReadinessState::Ready
            } else {
                ReadinessState::Inactive
            };
            mk(state, text, true)
        }
        None if have_reading => mk(
            ReadinessState::Ready,
            "Plan usage is up to date.".into(),
            false,
        ),
        None if running || !tried => mk(
            ReadinessState::Inactive,
            "Reading plan usage from Claude Code…".into(),
            false,
        ),
        None => mk(
            ReadinessState::Inactive,
            "No plan usage has been read yet. Try Refresh".into(),
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_ipc::RecordingSink;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scripted {
        answers: Mutex<Vec<ControlAnswer>>,
        calls: AtomicUsize,
    }
    impl PlanReader for Scripted {
        fn read(&self) -> ControlAnswer {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answers.lock().unwrap().remove(0)
        }
    }

    fn reading(pct: f64) -> ControlAnswer {
        ControlAnswer::Reading {
            plan: Some("max".into()),
            five_hour: Some((Some(pct), Some(5_000_000))),
            weekly: Some((Some(50.0), Some(9_000_000))),
            per_model: vec![],
            version: Some("2.1.289".into()),
        }
    }

    fn service(
        answers: Vec<ControlAnswer>,
    ) -> (
        Arc<UsageService>,
        Arc<Scripted>,
        Arc<RecordingSink>,
        tempfile::TempDir,
    ) {
        let mut reg = helm_core::db::MigrationRegistry::new();
        reg.initializer("plan-usage", store::init);
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        let reader = Arc::new(Scripted {
            answers: Mutex::new(answers),
            calls: AtomicUsize::new(0),
        });
        let sink = Arc::new(RecordingSink::default());
        let events: Arc<dyn EventSink> = sink.clone();
        (
            Arc::new(UsageService::new(db, events, reader.clone())),
            reader,
            sink,
            dir,
        )
    }

    fn wait_until(svc: &UsageService, f: impl Fn(&UsageSnapshot) -> bool) -> UsageSnapshot {
        for _ in 0..200 {
            let s = svc.snapshot(helm_core::now_ms());
            if f(&s) {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out");
    }

    #[test]
    fn a_refresh_reads_once_then_the_floor_holds_for_sixty_seconds() {
        let (svc, reader, sink, _d) = service(vec![reading(7.0), reading(9.0)]);
        let t0 = 1_000_000;
        let first = svc.refresh(t0);
        assert!(
            first.refreshing,
            "the op answers at once; the read finishes on its own thread"
        );
        let done = wait_until(&svc, |s| s.reading.is_some());
        assert_eq!(
            done.reading
                .as_ref()
                .unwrap()
                .five_hour
                .as_ref()
                .unwrap()
                .pct,
            Some(7.0)
        );
        assert_eq!(done.readiness.state, ReadinessState::Ready);
        assert_eq!(sink.kinds(), ["plan_usage"]);
        // Inside the floor: no second read, whatever asks.
        let again = svc.refresh(t0 + MIN_GAP_MS - 1);
        assert!(!again.refreshing);
        assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
        assert_eq!(again.next_refresh_at, Some(t0 + MIN_GAP_MS));
        // At the floor it reads again.
        svc.refresh(t0 + MIN_GAP_MS);
        wait_until(&svc, |s| {
            s.reading
                .as_ref()
                .is_some_and(|r| r.five_hour.as_ref().unwrap().pct == Some(9.0))
        });
        assert_eq!(reader.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn an_unsupported_binary_is_a_readiness_state_with_the_version_and_a_place_to_act() {
        let (svc, _r, _s, _d) = service(vec![ControlAnswer::Failed {
            reason: "unsupported".into(),
            message: "Unknown request".into(),
            version: Some("1.0.0".into()),
        }]);
        svc.refresh(10);
        let s = wait_until(&svc, |s| !s.refreshing);
        assert!(s.reading.is_none());
        assert_eq!(s.readiness.state, ReadinessState::Inactive);
        assert!(s.readiness.reason.contains("did not report plan usage"));
        assert!(s.readiness.reason.contains("1.0.0"));
        assert_eq!(s.readiness.action_target.as_deref(), Some("settings:usage"));
        // A chat's rate-limit line then supplies a reading, and the reason says so.
        svc.ingest_passive(
            &[Passive {
                kind: "five_hour".into(),
                pct: Some(33.0),
                resets_at: Some(77),
            }],
            helm_core::now_ms(),
        );
        let s = svc.snapshot(helm_core::now_ms());
        assert_eq!(s.readiness.state, ReadinessState::Ready);
        assert!(s
            .readiness
            .reason
            .contains("showing the last reading from a chat"));
        assert_eq!(s.reading.unwrap().source, UsageSource::RateLimitEvent);
    }

    #[test]
    fn nothing_read_yet_says_so_and_a_success_clears_an_old_failure() {
        let (svc, _r, _s, _d) = service(vec![
            ControlAnswer::Failed {
                reason: "timeout".into(),
                message: "slow".into(),
                version: None,
            },
            reading(1.0),
        ]);
        let before = svc.snapshot(0);
        assert!(before.readiness.reason.contains("Reading plan usage"));
        svc.refresh(0);
        let s = wait_until(&svc, |s| !s.refreshing);
        assert!(s.readiness.reason.contains("did not answer"));
        svc.refresh(MIN_GAP_MS);
        let s = wait_until(&svc, |s| s.reading.is_some());
        assert_eq!(s.readiness.reason, "Plan usage is up to date.");
    }

    #[test]
    fn the_helper_line_parses_both_ways() {
        let ok = parse_answer(&json!({"ok": true, "version": "2.1.289",
            "reading": {"plan": "max", "fiveHour": {"pct": 5, "resetsAt": 10}, "weekly": null,
                        "perModel": [{"model": "Fable", "pct": 0, "resetsAt": 20}]}}));
        assert_eq!(
            ok,
            ControlAnswer::Reading {
                plan: Some("max".into()),
                five_hour: Some((Some(5.0), Some(10))),
                weekly: None,
                per_model: vec![("Fable".into(), Some(0.0), Some(20))],
                version: Some("2.1.289".into()),
            }
        );
        let bad = parse_answer(&json!({"ok": false, "reason": "no_limits", "message": "m"}));
        assert!(matches!(bad, ControlAnswer::Failed { ref reason, .. } if reason == "no_limits"));
        assert!(matches!(
            parse_answer(&json!("junk")),
            ControlAnswer::Failed { .. }
        ));
    }
}
