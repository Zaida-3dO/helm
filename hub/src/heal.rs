//! Voice trouble that heals itself. While the speech-to-text server is down the hub holds failed
//! voice notes (in [`crate::telegram`] and [`crate::voice`]) instead of giving up on them, tells
//! the person's PC about the outage so a chat can fix it, and puts the held notes through when the
//! health probe sees the server answering again.
//!
//! [`Heal`] watches the probe's state [`HEALTH_PROBE/4`] apart: while the server is down it sends
//! the PC a `service_down` frame at most once an hour; the moment the server answers again it lets
//! every bridge retry what it holds and sends the PC one `service_up` frame, so an alert the PC
//! raised for the outage closes itself when the outage is seen to be over — retried on later
//! healthy ticks until a line carries it, since the PC that was asleep through the outage is
//! usually also the reason no line was up at that moment. A note held while the
//! probe sees nothing wrong (the server answers `/health` but a transcription failed) is retried a
//! minute after its last try, and every held note ends at its hour whether or not an outage was
//! seen.

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
    /// When the outage the last `was_down` saw began: [`SttHealth`] clears its own `down_since`
    /// the moment the probe answers again, before the watch ticks, so the recovery frame needs
    /// the start remembered here. It stays until a line takes the recovery (see
    /// [`Heal::pending_back_ms`]).
    down_since_ms: AtomicI64,
    /// A recovery frame no line would carry yet: when the server was seen answering, retried on
    /// every healthy tick until a line takes it. `0`: nothing pending. A new outage supersedes it.
    pending_back_ms: AtomicI64,
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
            down_since_ms: AtomicI64::new(0),
            pending_back_ms: AtomicI64::new(0),
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

    /// The `service_up` frame for the outage just seen to be over: one per outage, named by its
    /// start, so the PC can close the alert it raised.
    fn up_frame(&self, since_ms: i64, now_ms: i64) -> Value {
        json!({
            "type": "service_up",
            "service": SERVICE,
            "since": clock_text(since_ms),
            "back": clock_text(now_ms),
            "personId": self.person.to_string(),
        })
    }

    /// Send the recovery frame; if no line carries it, hold it (the two atomics stay set) so a
    /// later healthy tick tries again. A PC that was asleep through the outage is the usual case:
    /// it is both the outage and the reason no line was up when the outage ended.
    fn send_up(&self, since_ms: i64, back_ms: i64) {
        if self
            .lines
            .send_frame(self.person, &self.up_frame(since_ms, back_ms))
        {
            eprintln!("stt: told the person's PC that the speech server answers again");
        } else {
            self.down_since_ms.store(since_ms, Ordering::Relaxed);
            self.pending_back_ms.store(back_ms, Ordering::Relaxed);
            eprintln!("stt: the recovery frame found no line; it will be tried again");
        }
    }

    /// One tick of the watch.
    pub fn tick(&self, now_ms: i64) {
        // One read of the outage state: is_down and down_since come from the same atomic, so the
        // probe task cannot slip a note_ok between the check and the start.
        if let Some(broke) = self.health.down_since_ms() {
            if !self.was_down.swap(true, Ordering::Relaxed) {
                // A fresh outage seen by the watch: remember its start for the recovery frame,
                // and drop any recovery held over from the last one (a new outage supersedes it).
                self.down_since_ms.store(broke, Ordering::Relaxed);
                self.pending_back_ms.store(0, Ordering::Relaxed);
            }
            if let Some(f) = self.due_frame(now_ms) {
                if self.lines.send_frame(self.person, &f) {
                    self.last_frame_ms.store(now_ms, Ordering::Relaxed);
                    eprintln!("stt: told the person's PC that the speech server is down");
                }
            }
        } else if self.was_down.swap(false, Ordering::Relaxed) {
            // Back up: the next outage starts its frame count again, and everything held goes.
            let told_down = self.last_frame_ms.load(Ordering::Relaxed) != 0;
            self.last_frame_ms.store(0, Ordering::Relaxed);
            let since = self.down_since_ms.swap(0, Ordering::Relaxed);
            // Only stand an outage down if the PC was told about it: otherwise there is no
            // alert out there to close.
            if since != 0 && told_down {
                self.send_up(since, now_ms);
            }
            for h in &self.healed {
                h.stt_back(now_ms);
            }
        } else {
            // The server looks fine. First carry any recovery no line would have taken; then a
            // note may still be held (a transcription failed between two probes): the ones that
            // have waited a while go through again.
            let back = self.pending_back_ms.load(Ordering::Relaxed);
            let since = self.down_since_ms.load(Ordering::Relaxed);
            if back != 0
                && since != 0
                && self
                    .lines
                    .send_frame(self.person, &self.up_frame(since, back))
            {
                self.pending_back_ms.store(0, Ordering::Relaxed);
                self.down_since_ms.store(0, Ordering::Relaxed);
                eprintln!("stt: told the person's PC that the speech server answers again");
            }
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

    /// Recorded frames, and a switch: the next send fails once (a PC with no line up), the
    /// ones after go through.
    struct Rec(Mutex<Vec<Value>>, std::sync::atomic::AtomicBool);

    impl SendFrame for Rec {
        fn send_frame(&self, _person: i64, frame: &Value) -> bool {
            if self.1.swap(false, Ordering::SeqCst) {
                return false;
            }
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
        let rec = Arc::new(Rec(
            Mutex::new(vec![]),
            std::sync::atomic::AtomicBool::new(false),
        ));
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
        // Back up: every bridge is told once, the PC is told the outage is over, and no more
        // while it stays fine.
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
        let frames = rec.0.lock().unwrap();
        assert_eq!(frames.len(), 3, "two down frames and one recovery");
        let up = frames[2].clone();
        drop(frames);
        assert_eq!(up["type"], "service_up");
        assert_eq!(up["service"], SERVICE);
        assert_eq!(up["personId"], "7");
        // The recovery names the same start as the outage opened with, and the minute it landed.
        assert_eq!(up["since"], rec.0.lock().unwrap()[0]["since"]);
        assert_eq!(up["back"], clock_text(base + FRAME_GAP_MS + 2_000));
        // A fresh outage opens its frame at once, not an hour late.
        health.note_down(
            base + FRAME_GAP_MS + 4_000,
            "its health address answered HTTP 503",
        );
        heal.tick(base + FRAME_GAP_MS + 4_000);
        assert_eq!(rec.0.lock().unwrap().len(), 4);
        assert_eq!(
            rec.0.lock().unwrap()[3]["detail"],
            "its health address answered HTTP 503"
        );
        assert_eq!(rec.0.lock().unwrap()[3]["type"], "service_down");
    }

    #[test]
    fn a_recovery_no_line_would_carry_is_retried_until_a_line_takes_it() {
        let (heal, health, rec, _seen) = hub();
        let base = 1_763_000_000_000;
        health.note_down(base, "its health address does not answer");
        heal.tick(base);
        assert_eq!(
            rec.0.lock().unwrap().len(),
            1,
            "the outage opens with one frame"
        );
        // The PC's line is down at the recovery edge: the frame is held, not lost.
        rec.1.store(true, Ordering::SeqCst);
        health.note_ok(base + 60_000);
        heal.tick(base + 60_000);
        assert!(
            rec.0
                .lock()
                .unwrap()
                .iter()
                .all(|f| f["type"] != "service_up"),
            "the recovery that failed to send is not recorded as sent"
        );
        // A later healthy tick carries it, and still names the edge it was seen at.
        heal.tick(base + 75_000);
        let frames = rec.0.lock().unwrap();
        assert_eq!(frames.len(), 2, "the held recovery went out on the retry");
        assert_eq!(frames[1]["type"], "service_up");
        assert_eq!(frames[1]["back"], clock_text(base + 60_000));
        drop(frames);
        heal.tick(base + 90_000);
        assert_eq!(rec.0.lock().unwrap().len(), 2, "one recovery per outage");
    }

    #[test]
    fn no_recovery_for_an_outage_the_pc_was_never_told_about() {
        let (heal, health, rec, _seen) = hub();
        let base = 1_763_000_000_000;
        // The down frame found no line either: the outage was over before the PC could hear
        // about it, so there is no alert out there to stand down.
        rec.1.store(true, Ordering::SeqCst);
        health.note_down(base, "its health address does not answer");
        heal.tick(base);
        health.note_ok(base + 60_000);
        heal.tick(base + 60_000);
        heal.tick(base + 75_000);
        assert!(
            rec.0.lock().unwrap().is_empty(),
            "nothing was announced, so nothing is stood down"
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
