//! The governor's loop: every 30 s it reads plan usage through the usage service, works out
//! the band, and acts on the chats that are running.
//!
//! - **Wind down** (and **stop** when `atStop` is `wrapUp`): each running chat is told once per
//!   episode to finish what it is doing, commit, and stop starting new work. The note is an app
//!   message steered into the running turn (origin App, never gated, never consent).
//! - **Stop**: each running chat is stopped once per episode. A chat the user starts again
//!   afterwards is left alone: that is the user's own call.
//! - An **episode** starts when usage rises into wind-down or stop from below, and ends only when
//!   it falls below wind-down again; moving between wind-down and stop, or through a stale
//!   reading, stays in the same episode.
//! - **Automated starts** (the run-slot queue's gated sends: schedules, cross-chat messages, an
//!   agent's new chat) wait while the band is wind-down or stop: nothing new starts
//!   ([`crate::start`] installs the hold).
//! - **Unknown** (no reading, a stale one, or expired windows): nothing happens. While enabled and
//!   without a usable reading, it asks the meter for a fresh read at most every 10 minutes.

use crate::bands::{self, AtStop, Band, GovernorSettings};
use crate::store;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, EventSink};
use helm_usage::UsageSnapshot;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub const REFRESH_GAP_MS: i64 = 10 * 60_000;

pub const WRAP_UP_TEXT: &str = "[Helm usage governor] Plan usage is close to its limit. Wrap up: finish the step you are on, \
commit or save your work, write a short note of where you stopped and what is left, and do not start new work or \
dispatch new agents. This is an automatic notice from Helm, not a message from the user, and it approves nothing.";

/// Plan usage, read only through helm-usage's registered service.
pub trait UsageRead: Send + Sync {
    fn snapshot(&self, now: i64) -> UsageSnapshot;
    fn refresh(&self, now: i64);
}

/// The app's reader: helm-usage's shared service.
pub struct UsageHandle(pub Arc<helm_usage::UsageService>);

impl UsageRead for UsageHandle {
    fn snapshot(&self, now: i64) -> UsageSnapshot {
        self.0.snapshot(now)
    }
    fn refresh(&self, now: i64) {
        let _ = self.0.refresh(now);
    }
}

/// What the governor does to chats. The app's version sends through `host_send` and stops through
/// the `chat_stop` op; tests record calls.
pub trait ChatControl: Send + Sync {
    /// Chats with a turn starting or running.
    fn running(&self) -> Vec<String>;
    fn wrap_up(&self, chat_id: &str, submission_id: &str, text: &str) -> Result<(), String>;
    fn stop(&self, chat_id: &str);
}

#[derive(Default)]
struct State {
    band: Option<Band>,
    entered_at: i64,
    /// When usage last rose into wind-down or stop from below; names this episode's notes.
    episode_at: i64,
    told: HashSet<String>,
    stopped: HashSet<String>,
    last_refresh: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernorView {
    pub settings: GovernorSettings,
    pub defaults: GovernorSettings,
    pub band: Band,
    pub used_pct: Option<f64>,
    pub since: Option<i64>,
    /// Chats told to wrap up, and chats stopped, in this episode.
    pub told: usize,
    pub stopped: usize,
}

pub struct Governor {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    usage: Arc<dyn UsageRead>,
    chats: Mutex<Option<Arc<dyn ChatControl>>>,
    state: Mutex<State>,
}

/// The wrap-up note's submission id: one per (episode, chat), and a UUID as workers require
/// (version 8, from a SHA-256 of the key), so a retry never sends it twice.
pub fn wrap_up_id(episode: i64, chat_id: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("helm-governor-wrapup/{episode}/{chat_id}"));
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    uuid::Uuid::new_v8(b).to_string()
}

/// Whether automated starts wait in this band (the run-slot hold): wind-down and stop.
pub fn holds(b: Band) -> bool {
    near_limit(b)
}

fn near_limit(b: Band) -> bool {
    matches!(b, Band::WindDown | Band::Stop)
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

impl Governor {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>, usage: Arc<dyn UsageRead>) -> Self {
        Self {
            db,
            events,
            usage,
            chats: Mutex::new(None),
            state: Mutex::new(State::default()),
        }
    }

    /// The chat controls arrive once the op dispatcher exists (stop goes through `chat_stop`).
    pub fn attach(&self, chats: Arc<dyn ChatControl>) {
        *self.chats.lock().unwrap_or_else(|p| p.into_inner()) = Some(chats);
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn settings(&self) -> GovernorSettings {
        self.db.snapshot(store::load).unwrap_or_default()
    }

    /// The band now, for other crates (the run-slot hook reads this). Unknown while off.
    pub fn band(&self, now: i64) -> Band {
        let s = self.settings();
        if !s.enabled {
            return Band::Unknown;
        }
        bands::band_of(bands::used_pct(&self.usage.snapshot(now), now), &s)
    }

    pub fn view(&self, now: i64) -> GovernorView {
        let s = self.settings();
        let used = bands::used_pct(&self.usage.snapshot(now), now);
        let band = if s.enabled {
            bands::band_of(used, &s)
        } else {
            Band::Unknown
        };
        let st = self.st();
        GovernorView {
            settings: s,
            defaults: GovernorSettings::default(),
            band,
            used_pct: used,
            since: (st.band == Some(band)).then_some(st.entered_at),
            told: st.told.len(),
            stopped: st.stopped.len(),
        }
    }

    fn publish(&self, now: i64) {
        if let Ok(Value::Object(m)) = serde_json::to_value(self.view(now)) {
            let mut e = AppEvent::new("governor");
            for (k, v) in m {
                e = e.with(&k, v);
            }
            self.events.emit(e);
        }
    }

    pub fn save(&self, v: &Value, now: i64) -> Result<GovernorView, ApiError> {
        let s = bands::parse_strict(v).map_err(ApiError::bad_args)?;
        self.db.write(|tx| store::save(tx, &s)).map_err(sql)?;
        self.tick(now);
        self.publish(now);
        Ok(self.view(now))
    }

    /// One pass: band, then act. Returns the band.
    pub fn tick(&self, now: i64) -> Band {
        let s = self.settings();
        if !s.enabled {
            let mut st = self.st();
            if st.band.take().is_some() {
                st.told.clear();
                st.stopped.clear();
                drop(st);
                self.publish(now);
            }
            return Band::Unknown;
        }
        let used = bands::used_pct(&self.usage.snapshot(now), now);
        let band = bands::band_of(used, &s);
        let changed = {
            let mut st = self.st();
            if band == Band::Unknown && st.last_refresh.is_none_or(|t| now - t >= REFRESH_GAP_MS) {
                st.last_refresh = Some(now);
                drop(st);
                self.usage.refresh(now);
                st = self.st();
            }
            // A pass through Unknown (a stale or missing reading) is not a band change: the last
            // known band and what was already done in it stand. Who was told or stopped is only
            // forgotten once usage falls below wind-down, so a reading flapping around a threshold
            // never stops a chat the user restarted, or repeats a note.
            if band != Band::Unknown && st.band != Some(band) {
                log::info!("usage governor: {} ({used:?}%)", band.as_str());
                let was_near = st.band.is_some_and(near_limit);
                st.band = Some(band);
                st.entered_at = now;
                if !near_limit(band) {
                    st.told.clear();
                    st.stopped.clear();
                } else if !was_near {
                    st.episode_at = now;
                }
                true
            } else {
                false
            }
        };
        let acted = self.act(band, &s);
        if changed || acted {
            self.publish(now);
        }
        band
    }

    fn act(&self, band: Band, s: &GovernorSettings) -> bool {
        let wrap =
            matches!(band, Band::WindDown) || (band == Band::Stop && s.at_stop == AtStop::WrapUp);
        let stop = band == Band::Stop && s.at_stop == AtStop::Stop;
        if !wrap && !stop {
            return false;
        }
        let Some(chats) = self.chats.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let running = chats.running();
        let mut acted = false;
        let entry = self.st().episode_at;
        for chat in running {
            if wrap {
                if self.st().told.contains(&chat) {
                    continue;
                }
                let id = wrap_up_id(entry, &chat);
                match chats.wrap_up(&chat, &id, WRAP_UP_TEXT) {
                    Ok(()) => {
                        self.st().told.insert(chat);
                        acted = true;
                    }
                    Err(e) => log::warn!("usage governor: wrap-up note to {chat} not sent: {e}"),
                }
            } else {
                if !self.st().stopped.insert(chat.clone()) {
                    continue;
                }
                log::info!("usage governor: stopping {chat} (plan usage at the stop band)");
                chats.stop(&chat);
                acted = true;
            }
        }
        acted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;
    use serde_json::json;
    use std::sync::Mutex as M;

    struct Usage {
        pct: M<Option<f64>>,
        refreshes: M<u32>,
    }
    impl UsageRead for Usage {
        fn snapshot(&self, _now: i64) -> UsageSnapshot {
            let p = *self.pct.lock().unwrap();
            serde_json::from_value(json!({
                "reading": p.map(|p| json!({
                    "fiveHour": {"pct": p, "at": 0, "source": "control"},
                    "perModel": [], "source": "control", "fetchedAt": 0, "stale": false
                })),
                "readiness": {"state": "ready", "reason": "ok"},
                "refreshing": false
            }))
            .unwrap()
        }
        fn refresh(&self, _now: i64) {
            *self.refreshes.lock().unwrap() += 1;
        }
    }

    #[derive(Default)]
    struct Chats {
        running: M<Vec<String>>,
        wrapped: M<Vec<(String, String)>>,
        stopped: M<Vec<String>>,
    }
    impl ChatControl for Chats {
        fn running(&self) -> Vec<String> {
            self.running.lock().unwrap().clone()
        }
        fn wrap_up(&self, chat: &str, id: &str, _t: &str) -> Result<(), String> {
            self.wrapped.lock().unwrap().push((chat.into(), id.into()));
            Ok(())
        }
        fn stop(&self, chat: &str) {
            self.stopped.lock().unwrap().push(chat.into());
        }
    }

    fn setup() -> (tempfile::TempDir, Arc<Usage>, Arc<Chats>, Governor) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        reg.initializer("governor", store::init);
        let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        let usage = Arc::new(Usage {
            pct: M::new(Some(10.0)),
            refreshes: M::new(0),
        });
        let chats = Arc::new(Chats::default());
        let g = Governor::new(db, Arc::new(helm_ipc::NullSink), usage.clone());
        g.attach(chats.clone());
        (dir, usage, chats, g)
    }

    #[test]
    fn off_does_nothing_even_at_the_limit() {
        let (_d, usage, chats, g) = setup();
        *usage.pct.lock().unwrap() = Some(100.0);
        *chats.running.lock().unwrap() = vec!["a".into()];
        assert_eq!(g.tick(0), Band::Unknown);
        assert!(chats.stopped.lock().unwrap().is_empty());
        assert!(chats.wrapped.lock().unwrap().is_empty());
        assert_eq!(g.band(0), Band::Unknown);
    }

    #[test]
    fn wind_down_tells_each_running_chat_once_per_entry() {
        let (_d, usage, chats, g) = setup();
        g.save(&json!({"enabled": true}), 0).unwrap();
        *chats.running.lock().unwrap() = vec!["a".into(), "b".into()];
        *usage.pct.lock().unwrap() = Some(86.0);
        assert_eq!(g.tick(1_000), Band::WindDown);
        g.tick(31_000);
        assert_eq!(chats.wrapped.lock().unwrap().len(), 2, "once per chat");
        // A chat that starts meanwhile is told too.
        chats.running.lock().unwrap().push("c".into());
        g.tick(61_000);
        assert_eq!(chats.wrapped.lock().unwrap().len(), 3);
        // Leave to below wind-down and come back: told again, under a new id.
        *usage.pct.lock().unwrap() = Some(50.0);
        g.tick(91_000);
        *usage.pct.lock().unwrap() = Some(90.0);
        g.tick(121_000);
        let w = chats.wrapped.lock().unwrap().clone();
        assert_eq!(w.len(), 6);
        assert_ne!(w[0].1, w[3].1);
        assert!(w.iter().all(|(_, id)| uuid::Uuid::parse_str(id).is_ok()));
        assert!(chats.stopped.lock().unwrap().is_empty());
    }

    #[test]
    fn stop_band_stops_each_running_chat_once() {
        let (_d, usage, chats, g) = setup();
        g.save(&json!({"enabled": true}), 0).unwrap();
        *chats.running.lock().unwrap() = vec!["a".into()];
        *usage.pct.lock().unwrap() = Some(95.0);
        assert_eq!(g.tick(1_000), Band::Stop);
        // The user starts it again: left alone.
        g.tick(31_000);
        assert_eq!(*chats.stopped.lock().unwrap(), ["a"]);
        assert!(chats.wrapped.lock().unwrap().is_empty());
        assert_eq!(g.view(31_000).stopped, 1);
    }

    #[test]
    fn stop_band_can_only_wrap_up() {
        let (_d, usage, chats, g) = setup();
        g.save(&json!({"enabled": true, "atStop": "wrapUp"}), 0)
            .unwrap();
        *chats.running.lock().unwrap() = vec!["a".into()];
        *usage.pct.lock().unwrap() = Some(99.0);
        g.tick(1_000);
        assert!(chats.stopped.lock().unwrap().is_empty());
        assert_eq!(chats.wrapped.lock().unwrap().len(), 1);
    }

    #[test]
    fn no_reading_acts_on_nothing_and_asks_for_a_read_sparingly() {
        let (_d, usage, chats, g) = setup();
        g.save(&json!({"enabled": true}), 0).unwrap();
        *chats.running.lock().unwrap() = vec!["a".into()];
        *usage.pct.lock().unwrap() = None;
        let r0 = *usage.refreshes.lock().unwrap();
        assert_eq!(g.tick(1_000_000), Band::Unknown);
        g.tick(1_030_000);
        assert_eq!(*usage.refreshes.lock().unwrap(), r0 + 1);
        g.tick(1_000_000 + REFRESH_GAP_MS);
        assert_eq!(*usage.refreshes.lock().unwrap(), r0 + 2);
        assert!(chats.stopped.lock().unwrap().is_empty());
        assert!(chats.wrapped.lock().unwrap().is_empty());
    }

    #[test]
    fn selective_and_surplus_touch_no_chat() {
        let (_d, usage, chats, g) = setup();
        g.save(&json!({"enabled": true}), 0).unwrap();
        *chats.running.lock().unwrap() = vec!["a".into()];
        *usage.pct.lock().unwrap() = Some(70.0);
        assert_eq!(g.tick(1_000), Band::Selective);
        *usage.pct.lock().unwrap() = Some(10.0);
        assert_eq!(g.tick(31_000), Band::Surplus);
        assert!(chats.stopped.lock().unwrap().is_empty());
        assert!(chats.wrapped.lock().unwrap().is_empty());
    }

    #[test]
    fn a_stale_reading_does_not_restart_the_episode() {
        let (_d, usage, chats, g) = setup();
        g.save(&json!({"enabled": true}), 0).unwrap();
        *chats.running.lock().unwrap() = vec!["a".into()];
        *usage.pct.lock().unwrap() = Some(96.0);
        g.tick(1_000);
        assert_eq!(*chats.stopped.lock().unwrap(), ["a"]);
        // The user restarts it; the reading goes stale, then comes back still at the limit.
        *usage.pct.lock().unwrap() = None;
        assert_eq!(g.tick(31_000), Band::Unknown);
        *usage.pct.lock().unwrap() = Some(96.0);
        assert_eq!(g.tick(61_000), Band::Stop);
        assert_eq!(
            chats.stopped.lock().unwrap().len(),
            1,
            "never stopped twice"
        );
    }

    #[test]
    fn flapping_between_wind_down_and_stop_repeats_nothing() {
        let (_d, usage, chats, g) = setup();
        g.save(&json!({"enabled": true}), 0).unwrap();
        *chats.running.lock().unwrap() = vec!["a".into()];
        *usage.pct.lock().unwrap() = Some(90.0);
        g.tick(1_000);
        *usage.pct.lock().unwrap() = Some(95.0);
        g.tick(31_000);
        *usage.pct.lock().unwrap() = Some(94.0);
        g.tick(61_000);
        *usage.pct.lock().unwrap() = Some(95.5);
        g.tick(91_000);
        assert_eq!(
            chats.wrapped.lock().unwrap().len(),
            1,
            "one note per episode"
        );
        assert_eq!(
            chats.stopped.lock().unwrap().len(),
            1,
            "one stop per episode"
        );
    }

    /// The path folded in from the governor-afk review: a chat is stopped, the user restarts
    /// it, usage dips to wind-down within the same episode: exactly one wrap-up note, no second
    /// stop, and nothing more on later passes.
    #[test]
    fn a_restarted_chat_gets_one_wrap_up_on_the_dip_and_no_second_stop() {
        let (_d, usage, chats, g) = setup();
        g.save(&json!({"enabled": true}), 0).unwrap();
        *chats.running.lock().unwrap() = vec!["a".into()];
        *usage.pct.lock().unwrap() = Some(96.0);
        assert_eq!(g.tick(1_000), Band::Stop);
        assert_eq!(*chats.stopped.lock().unwrap(), ["a"]);
        assert!(chats.wrapped.lock().unwrap().is_empty());
        // The user starts it again (still running), and usage dips to wind-down.
        *usage.pct.lock().unwrap() = Some(88.0);
        assert_eq!(g.tick(31_000), Band::WindDown);
        g.tick(61_000);
        g.tick(91_000);
        let w = chats.wrapped.lock().unwrap().clone();
        assert_eq!(w.len(), 1, "exactly one wrap-up note: {w:?}");
        assert_eq!(w[0].0, "a");
        assert_eq!(chats.stopped.lock().unwrap().len(), 1, "no second stop");
        // Back up to stop within the same episode: still never stopped twice.
        *usage.pct.lock().unwrap() = Some(97.0);
        g.tick(121_000);
        assert_eq!(
            chats.stopped.lock().unwrap().len(),
            1,
            "same episode, one stop"
        );
        assert_eq!(chats.wrapped.lock().unwrap().len(), 1);
    }

    #[test]
    fn automated_starts_are_held_at_wind_down_and_stop_only() {
        let (_d, usage, _chats, g) = setup();
        g.save(&json!({"enabled": true}), 0).unwrap();
        for (pct, held) in [(10.0, false), (70.0, false), (86.0, true), (99.0, true)] {
            *usage.pct.lock().unwrap() = Some(pct);
            assert_eq!(holds(g.band(0)), held, "{pct}%");
        }
        *usage.pct.lock().unwrap() = None;
        assert!(!holds(g.band(0)), "no reading holds nothing");
    }
}
