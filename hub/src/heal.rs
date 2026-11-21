//! Voice trouble that heals itself. While the speech-to-text server is down the hub holds failed
//! voice notes (in [`crate::telegram`] and [`crate::voice`]) instead of giving up on them, tells
//! the person's PC about the outage so a chat can fix it, and puts the held notes through when the
//! health probe sees the server answering again.
//!
//! [`Heal`] watches the probe's state [`HEALTH_PROBE/4`] apart: while the server is down it sends
//! the PC a `service_down` frame at most once an hour; the moment the server answers again it lets
//! every bridge retry what it holds. A note held while the probe sees nothing wrong (the server
//! answers `/health` but a transcription failed) is retried a minute after its last try, and every
//! held note ends at its hour whether or not an outage was seen.

use crate::lines::Lines;
use crate::relay::Channel;
use crate::stt::{clock_text, SttHealed, SttHealth, HEALTH_PROBE};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

/// The service whose outage the hub reports and works around.
pub const SERVICE: &str = "stt";
/// At most one `service_down` frame per hour, however long the outage runs.
pub const FRAME_GAP_MS: i64 = 3_600_000;
/// How long a failed voice note waits for the server to come back before it is sent without
/// its transcript, or given up on.
pub const HELD_MAX_MS: i64 = 3_600_000;
/// The most recording audio one person has held through outages, and the most notes. A new note
/// past either sends the person's oldest held one on without its text, with a notice.
pub const HELD_AUDIO_MAX: usize = 32 * 1024 * 1024;
pub const HELD_COUNT_MAX: usize = 10;
/// How long a held note waits after a try before the watch tries it again while the server looks
/// fine.
pub const HELD_RETRY_MS: i64 = 60_000;
/// What a person is told when their recording is being held for the server to come back.
pub const VOICE_HELD: &str = "Voice is down; I'm getting it fixed and will transcribe this \
                              when it's back. Text messages still work.";

/// Where the frames go: one of the person's PCs that is up. [`Lines`] answers with its own
/// routing; tests record what arrives.
pub trait SendFrame: Send + Sync {
    fn send_frame(&self, person: i64, frame: &Value) -> bool;
}

impl SendFrame for Lines {
    fn send_frame(&self, person: i64, frame: &Value) -> bool {
        // The same line the person's own messages take, whichever channel the PC holds it on.
        [Channel::Telegram, Channel::Voice, Channel::App]
            .iter()
            .filter_map(|c| self.route_for(person, *c))
            .next()
            .is_some_and(|l| l.send(frame))
    }
}

/// One hub's watch on the speech server's health, driving the frame and the bridges' retries.
pub struct Heal {
    health: Arc<SttHealth>,
    lines: Arc<dyn SendFrame>,
    person: i64,
    healed: Vec<Arc<dyn SttHealed>>,
    /// When the last frame went up; `0`: the current outage has had no frame yet.
    last_frame_ms: AtomicI64,
    /// Whether the last tick saw an outage, so the edge back up is noticed once.
    was_down: AtomicBool,
}

impl Heal {
    pub fn new(health: Arc<SttHealth>, lines: Arc<Lines>, person: i64) -> Self {
        Heal {
            health,
            lines,
            person,
            healed: Vec::new(),
            last_frame_ms: AtomicI64::new(0),
            was_down: AtomicBool::new(false),
        }
    }

    /// Where the frames go, in place of the lines routing (tests record what is sent).
    pub fn with_lines(mut self, lines: Arc<dyn SendFrame>) -> Self {
        self.lines = lines;
        self
    }

    /// Add a bridge that holds notes through outages.
    pub fn with_healed(mut self, bridge: Arc<dyn SttHealed>) -> Self {
        self.healed.push(bridge);
        self
    }

    /// The `service_down` frame due now: `None` while the server is fine or the hour's frame has
    /// already gone. A PC that was down when the frame was due gets it on the next tick.
    pub fn due_frame(&self, now_ms: i64) -> Option<Value> {
        let since = self.health.down_since_ms()?;
        let last = self.last_frame_ms.load(Ordering::Relaxed);
        if last != 0 && now_ms - last < FRAME_GAP_MS {
            return None;
        }
        let mut f = json!({
            "type": "service_down",
            "service": SERVICE,
            "since": clock_text(since),
            "personId": self.person.to_string(),
        });
        if let Some(d) = self.health.down_detail() {
            f["detail"] = json!(d);
        }
        Some(f)
    }

    /// One tick of the watch.
    pub fn tick(&self, now_ms: i64) {
        if self.health.is_down() {
            self.was_down.store(true, Ordering::Relaxed);
            if let Some(f) = self.due_frame(now_ms) {
                if self.lines.send_frame(self.person, &f) {
                    self.last_frame_ms.store(now_ms, Ordering::Relaxed);
                    eprintln!("stt: told the person's PC that the speech server is down");
                }
            }
        } else if self.was_down.swap(false, Ordering::Relaxed) {
            // Back up: the next outage starts its frame count again, and everything held goes.
            self.last_frame_ms.store(0, Ordering::Relaxed);
            for h in &self.healed {
                h.stt_back(now_ms);
            }
        } else {
            // The server looks fine, yet a note may be held (a transcription failed between two
            // probes): the ones that have waited a while go through again.
            for h in &self.healed {
                h.stt_retry_stale(now_ms);
            }
        }
        // A held note ends at its hour whether or not the probe saw an outage.
        for h in &self.healed {
            h.stt_expire(now_ms);
        }
    }

    /// Watch from inside the runtime, a quarter of a probe cycle apart.
    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                self.tick(crate::telegram::now_ms());
                tokio::time::sleep(HEALTH_PROBE / 4).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use std::sync::Mutex;

    struct Rec(Mutex<Vec<Value>>);

    impl SendFrame for Rec {
        fn send_frame(&self, _person: i64, frame: &Value) -> bool {
            self.0.lock().unwrap().push(frame.clone());
            true
        }
    }

    #[derive(Default)]
    struct Seen {
        backs: AtomicI64,
        expires: AtomicI64,
        retries: AtomicI64,
    }

    impl SttHealed for Seen {
        fn stt_back(&self, _at: i64) {
            self.backs.fetch_add(1, Ordering::SeqCst);
        }
        fn stt_expire(&self, _at: i64) {
            self.expires.fetch_add(1, Ordering::SeqCst);
        }
        fn stt_retry_stale(&self, _at: i64) {
            self.retries.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn hub() -> (Heal, Arc<SttHealth>, Arc<Rec>, Arc<Seen>) {
        let registry = Arc::new(Mutex::new(
            Registry::from_legacy_spec("line:line-token-0123456789abcdefghij:relay", "owner")
                .unwrap(),
        ));
        let (lines, _up) = Lines::new(registry);
        let health = Arc::new(SttHealth::default());
        let rec = Arc::new(Rec(Mutex::new(vec![])));
        let seen = Arc::new(Seen::default());
        let heal = Heal::new(health.clone(), Arc::new(lines), 7)
            .with_lines(rec.clone())
            .with_healed(seen.clone());
        (heal, health, rec, seen)
    }

    #[test]
    fn one_frame_an_hour_while_down_and_bridges_go_when_it_is_back() {
        let (heal, health, rec, seen) = hub();
        let base = 1_763_000_000_000;
        // Fine: no frame, and no outage to come back from; what is held (nothing) is looked at.
        heal.tick(base);
        assert!(rec.0.lock().unwrap().is_empty());
        assert_eq!(seen.backs.load(Ordering::SeqCst), 0);
        assert_eq!(seen.retries.load(Ordering::SeqCst), 1);
        health.note_down(base, "its health address does not answer");
        heal.tick(base);
        let frames = rec.0.lock().unwrap().len();
        assert_eq!(frames, 1, "the outage opens with one frame");
        let f = rec.0.lock().unwrap()[0].clone();
        assert_eq!(f["type"], "service_down");
        assert_eq!(f["service"], SERVICE);
        assert_eq!(f["personId"], "7");
        assert_eq!(f["detail"], "its health address does not answer");
        assert!(
            f["since"]
                .as_str()
                .is_some_and(|s| s.len() == 5 && s.chars().all(|c| c.is_ascii_digit() || c == ':')),
            "{f}"
        );
        // Still down: expire runs each tick, but no second frame inside the hour, and nothing is
        // retried while the server is known to be down.
        heal.tick(base + 1_800_000);
        assert_eq!(rec.0.lock().unwrap().len(), 1);
        assert_eq!(seen.expires.load(Ordering::SeqCst), 3);
        assert_eq!(seen.retries.load(Ordering::SeqCst), 1);
        // Past the hour: one more frame.
        heal.tick(base + FRAME_GAP_MS + 1_000);
        assert_eq!(rec.0.lock().unwrap().len(), 2);
        // Back up: every bridge is told once, and no more while it stays fine.
        health.note_ok(base + FRAME_GAP_MS + 2_000);
        heal.tick(base + FRAME_GAP_MS + 2_000);
        assert_eq!(seen.backs.load(Ordering::SeqCst), 1);
        heal.tick(base + FRAME_GAP_MS + 3_000);
        assert_eq!(seen.backs.load(Ordering::SeqCst), 1);
        assert_eq!(
            seen.retries.load(Ordering::SeqCst),
            2,
            "fine again: held notes are looked at, not only on the edge"
        );
        // A fresh outage opens its frame at once, not an hour late.
        health.note_down(
            base + FRAME_GAP_MS + 4_000,
            "its health address answered HTTP 503",
        );
        heal.tick(base + FRAME_GAP_MS + 4_000);
        assert_eq!(rec.0.lock().unwrap().len(), 3);
        assert_eq!(
            rec.0.lock().unwrap()[2]["detail"],
            "its health address answered HTTP 503"
        );
    }

    #[test]
    fn no_frame_while_the_server_has_never_been_probed() {
        let (heal, _health, rec, _seen) = hub();
        for k in 0..6 {
            heal.tick(1_763_000_000_000 + k * 15_000);
        }
        assert!(rec.0.lock().unwrap().is_empty());
    }
}
