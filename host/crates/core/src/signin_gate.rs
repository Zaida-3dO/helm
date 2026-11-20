//! One gate for every Claude process Helm starts, so they do not all try to refresh one sign-in
//! at the same moment.
//!
//! Every Claude Code process on this PC shares one sign-in. When its access token has expired,
//! the first request of each process tries to refresh it, and Claude Code lets only one process
//! refresh at a time: the others fail with "Failed to refresh OAuth token". Helm starts many
//! processes at once (chats carried on after a restart, messages that waited for Helm, titles and
//! other helpers), so after a while away they would all meet the refresh together.
//!
//! Helm never reads the token, so it cannot tell when it expires. Instead it treats the sign-in as
//! unknown whenever a refresh is likely (at start, after the computer wakes, after a sign-in,
//! after a refresh failed, or after half an hour with no answer from Claude) and lets one start
//! through as the probe. The rest wait at their start until the probe gets a real answer, which
//! means the sign-in works and the token is fresh, and then go together.
//!
//! The gate fails open: a probe that has no answer after [`PROBE_LIMIT_MS`] opens it for everyone,
//! and no single start waits longer than [`MAX_WAIT_MS`] whatever the gate says. A background
//! start (the usage meter, a title or other helper) may probe too, since it is a cheap way to find
//! out, but nobody waits on it for long: after [`BACKGROUND_PROBE_LIMIT_MS`] without an answer it
//! stops being the probe and the next start finds out instead, while the background job carries on.
//!
//! [`Gate`] is the plain state machine (every method takes the time); [`SignInGate`] is the shared
//! handle chats, helpers and the sign-in terminal use, with a wake-up for async waiters and a
//! condition variable for blocking ones. [`global`] is the one the app's real Claude processes
//! share.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A probe with no answer for this long opens the gate for everyone.
pub const PROBE_LIMIT_MS: i64 = 90_000;
/// A background probe with no answer for this long stops being the probe: the next start probes.
pub const BACKGROUND_PROBE_LIMIT_MS: i64 = 15_000;
/// The last answer is trusted for this long; after it the next start probes again.
pub const FRESH_MS: i64 = 30 * 60_000;
/// After a refresh failure, everyone waits this long before the next probe.
pub const COOL_MS: i64 = 20_000;
/// No start waits at the gate longer than this.
pub const MAX_WAIT_MS: i64 = 90_000;

/// Where the gate stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing is known: the next start goes as the probe.
    Unknown,
    /// One start (`probe`) is finding out; the rest wait. A `background` probe is a job nobody
    /// is looking at, so it holds the others for a shorter time.
    Probing {
        since: i64,
        probe: u64,
        background: bool,
    },
    /// Claude answered at `last_answer`: everything goes.
    Open { last_answer: i64 },
    /// A refresh just failed: everything waits until `until`, then the next start probes.
    Cooling { until: i64 },
}

/// What the gate says to one start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Start now. `probe` names the start that is finding out for everyone, when this is it.
    Go { probe: Option<u64> },
    /// Wait at the start.
    Wait,
}

/// The state machine. Nothing here touches the outside world.
#[derive(Clone, Debug)]
pub struct Gate {
    state: State,
    probes: u64,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    /// A gate that knows nothing yet (the app has just started).
    pub fn new() -> Self {
        Gate {
            state: State::Unknown,
            probes: 0,
        }
    }

    /// The state with the time rules applied: a background probe past its limit knows nothing
    /// again (the next start probes), any other probe past its limit opens the gate, and a
    /// cooling past its end knows nothing again.
    pub fn current(&self, now: i64) -> State {
        match self.state {
            State::Probing {
                since,
                background: true,
                ..
            } if now - since >= BACKGROUND_PROBE_LIMIT_MS => State::Unknown,
            State::Probing { since, .. } if now - since >= PROBE_LIMIT_MS => {
                State::Open { last_answer: now }
            }
            State::Cooling { until } if now >= until => State::Unknown,
            s => s,
        }
    }

    /// Apply the time rules.
    pub fn tick(&mut self, now: i64) {
        self.state = self.current(now);
    }

    /// Whether a start must wait now, without changing anything. `waiting_since` is when this
    /// start began to wait (it never waits past [`MAX_WAIT_MS`]); `ahead` counts the starts
    /// waiting here that go before it, so when a probe may go, it is the first in line.
    pub fn must_wait(&self, now: i64, waiting_since: Option<i64>, ahead: usize) -> bool {
        if waiting_since.is_some_and(|s| now - s >= MAX_WAIT_MS) {
            return false;
        }
        match self.current(now) {
            State::Open { last_answer } if now - last_answer < FRESH_MS => false,
            State::Open { .. } | State::Unknown => ahead > 0,
            State::Probing { .. } | State::Cooling { .. } => true,
        }
    }

    /// A start asks to go. One that finds nothing known becomes the probe; `background` says the
    /// start is a job nobody is waiting on (see [`BACKGROUND_PROBE_LIMIT_MS`]).
    pub fn acquire(&mut self, now: i64, waiting_since: Option<i64>, background: bool) -> Admit {
        self.tick(now);
        match self.state {
            State::Open { last_answer } if now - last_answer < FRESH_MS => {
                Admit::Go { probe: None }
            }
            State::Open { .. } | State::Unknown => {
                self.probes += 1;
                self.state = State::Probing {
                    since: now,
                    probe: self.probes,
                    background,
                };
                Admit::Go {
                    probe: Some(self.probes),
                }
            }
            State::Probing { .. } | State::Cooling { .. } => {
                if waiting_since.is_some_and(|s| now - s >= MAX_WAIT_MS) {
                    Admit::Go { probe: None }
                } else {
                    Admit::Wait
                }
            }
        }
    }

    /// Claude answered a request: the sign-in works.
    pub fn answered(&mut self, now: i64) {
        self.state = State::Open { last_answer: now };
    }

    /// A process could not refresh the sign-in (any process, probe or not).
    pub fn refresh_failed(&mut self, now: i64) {
        self.state = State::Cooling {
            until: now + COOL_MS,
        };
    }

    /// The probe ended with neither an answer nor a refresh failure: the next start probes.
    /// A start that is no longer the probe changes nothing.
    pub fn released(&mut self, probe: u64) {
        if matches!(self.state, State::Probing { probe: p, .. } if p == probe) {
            self.state = State::Unknown;
        }
    }

    /// The sign-in may have changed (the app started, the computer woke, someone signed in).
    pub fn reset(&mut self) {
        self.state = State::Unknown;
    }
}

/// The shared gate.
pub struct SignInGate {
    gate: Mutex<Gate>,
    /// Blocking waiters (helpers) sleep on this.
    cv: Condvar,
    /// Bumped on every change, so async waiters wake.
    changed: tokio::sync::watch::Sender<u64>,
    /// Bumped when a sign-in finishes, so whoever watches can carry on the chats that waited
    /// for one.
    sign_ins: AtomicU64,
}

impl std::fmt::Debug for SignInGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInGate")
            .field("state", &self.lock().state)
            .finish()
    }
}

impl Default for SignInGate {
    fn default() -> Self {
        Self::new()
    }
}

impl SignInGate {
    pub fn new() -> Self {
        SignInGate {
            gate: Mutex::new(Gate::new()),
            cv: Condvar::new(),
            changed: tokio::sync::watch::channel(0).0,
            sign_ins: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Run `f` on the gate and wake every waiter when the state moved.
    fn apply<T>(&self, f: impl FnOnce(&mut Gate) -> T) -> T {
        let mut g = self.lock();
        let before = g.state;
        let out = f(&mut g);
        if g.state != before {
            self.wake();
        }
        out
    }

    fn wake(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
        self.cv.notify_all();
    }

    /// The state as of `now`.
    pub fn state(&self, now: i64) -> State {
        self.apply(|g| {
            g.tick(now);
            g.state
        })
    }

    /// Whether the gate is open and fresh: everything goes without waiting.
    pub fn is_open(&self, now: i64) -> bool {
        matches!(self.state(now), State::Open { last_answer } if now - last_answer < FRESH_MS)
    }

    /// See [`Gate::must_wait`].
    pub fn must_wait(&self, now: i64, waiting_since: Option<i64>, ahead: usize) -> bool {
        self.apply(|g| {
            g.tick(now);
            g.must_wait(now, waiting_since, ahead)
        })
    }

    /// Ask to start now: a ticket to go, or `None` to wait. Report through the ticket.
    pub fn admit(self: &Arc<Self>, now: i64, waiting_since: Option<i64>) -> Option<Ticket> {
        self.admit_as(now, waiting_since, false)
    }

    /// [`Self::admit`] for a background job, whose probe holds the others only briefly.
    pub fn admit_background(self: &Arc<Self>, now: i64) -> Option<Ticket> {
        self.admit_as(now, None, true)
    }

    fn admit_as(
        self: &Arc<Self>,
        now: i64,
        waiting_since: Option<i64>,
        background: bool,
    ) -> Option<Ticket> {
        match self.apply(|g| g.acquire(now, waiting_since, background)) {
            Admit::Go { probe } => Some(Ticket {
                gate: self.clone(),
                probe,
            }),
            Admit::Wait => None,
        }
    }

    /// Wait here (blocking) until the gate lets this background job go, at most `cap` (and never
    /// past [`MAX_WAIT_MS`]), or until `stop` answers true. Always answers a ticket: past the cap
    /// the job goes anyway. A job that goes as the probe holds the others only for
    /// [`BACKGROUND_PROBE_LIMIT_MS`].
    pub fn admit_blocking(self: &Arc<Self>, cap: Duration, stop: &dyn Fn() -> bool) -> Ticket {
        let began = Instant::now();
        let since = crate::now_ms();
        let mut g = self.lock();
        loop {
            let now = crate::now_ms();
            let before = g.state;
            let admit = g.acquire(now, Some(since), true);
            if g.state != before {
                self.wake();
            }
            let probe = match admit {
                Admit::Go { probe } => Some(probe),
                Admit::Wait if began.elapsed() >= cap || stop() => Some(None),
                Admit::Wait => None,
            };
            if let Some(probe) = probe {
                return Ticket {
                    gate: self.clone(),
                    probe,
                };
            }
            let left = cap.saturating_sub(began.elapsed());
            g = self
                .cv
                .wait_timeout(g, left.min(Duration::from_secs(1)))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Claude answered a request.
    pub fn answered(&self, now: i64) {
        self.apply(|g| g.answered(now));
    }

    /// A Claude process could not refresh the sign-in.
    pub fn refresh_failed(&self, now: i64) {
        self.apply(|g| g.refresh_failed(now));
    }

    fn release(&self, probe: u64) {
        self.apply(|g| g.released(probe));
    }

    /// The sign-in may have changed: the next start probes.
    pub fn reset(&self) {
        self.apply(Gate::reset);
    }

    /// A sign-in finished: the next start probes, and [`Self::sign_ins`] moves on so the chats
    /// that gave up waiting for one can be carried on.
    pub fn signed_in(&self) {
        self.sign_ins.fetch_add(1, Ordering::SeqCst);
        self.apply(Gate::reset);
        // The counter moved even when the state did not.
        self.wake();
    }

    /// How many sign-ins have finished since the app started.
    pub fn sign_ins(&self) -> u64 {
        self.sign_ins.load(Ordering::SeqCst)
    }

    /// A number that moves on every change.
    pub fn generation(&self) -> u64 {
        *self.changed.borrow()
    }

    /// Wait until something changes after generation `seen`.
    pub async fn changed_after(&self, seen: u64) {
        let mut rx = self.changed.subscribe();
        let _ = rx.wait_for(|g| *g != seen).await;
    }
}

/// Leave to start, held while the start's first request is under way. Report what happened
/// through it; a probe's ticket dropped without either report frees the gate for the next probe,
/// so a start that ends any other way (stopped, crashed, refused) never holds the others up.
pub struct Ticket {
    gate: Arc<SignInGate>,
    probe: Option<u64>,
}

impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket")
            .field("probe", &self.probe)
            .finish()
    }
}

impl Ticket {
    /// This start is finding out for everyone.
    pub fn is_probe(&self) -> bool {
        self.probe.is_some()
    }

    /// Claude answered this start's request.
    pub fn answered(mut self, now: i64) {
        self.probe = None;
        self.gate.answered(now);
    }

    /// This start could not refresh the sign-in.
    pub fn refresh_failed(mut self, now: i64) {
        self.probe = None;
        self.gate.refresh_failed(now);
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if let Some(p) = self.probe.take() {
            self.gate.release(p);
        }
    }
}

/// The gate every real Claude process of this app shares. It starts knowing nothing.
pub fn global() -> &'static Arc<SignInGate> {
    static G: OnceLock<Arc<SignInGate>> = OnceLock::new();
    G.get_or_init(|| Arc::new(SignInGate::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000;
    const MIN: i64 = 60 * S;

    /// Every transition of the gate, as one table: each row starts from a state, does one
    /// thing at a time, and says what the gate answers and where it ends up.
    #[test]
    fn the_gate_is_one_table_of_transitions() {
        #[derive(Clone, Copy, Debug)]
        enum Do {
            Acquire(i64, Option<i64>),
            AcquireBackground(i64),
            Answered(i64),
            Failed(i64),
            Released(u64),
            Tick(i64),
            Reset,
        }
        let probing = |since| State::Probing {
            since,
            probe: 1,
            background: false,
        };
        let background = |since| State::Probing {
            since,
            probe: 1,
            background: true,
        };
        let open = |last_answer| State::Open { last_answer };
        let go = |p: Option<u64>| Some(Admit::Go { probe: p });
        let rows: Vec<(&str, State, Do, Option<Admit>, State)> = vec![
            // Nothing known: the first start is the probe, the next waits.
            (
                "unknown: probe",
                State::Unknown,
                Do::Acquire(0, None),
                go(Some(1)),
                probing(0),
            ),
            (
                "probing: wait",
                probing(0),
                Do::Acquire(5 * S, None),
                Some(Admit::Wait),
                probing(0),
            ),
            // An answer opens it; a fresh answer lets everyone through without a probe.
            (
                "probe answered",
                probing(0),
                Do::Answered(3 * S),
                None,
                open(3 * S),
            ),
            (
                "open: go",
                open(0),
                Do::Acquire(29 * MIN, None),
                go(None),
                open(0),
            ),
            // Half an hour after the last answer, the next start probes again.
            (
                "stale: probe",
                open(0),
                Do::Acquire(30 * MIN, None),
                go(Some(1)),
                probing(30 * MIN),
            ),
            // A refresh failure (any chat's) cools everyone for 20 s, then nothing is known.
            (
                "probe failed",
                probing(0),
                Do::Failed(4 * S),
                None,
                State::Cooling { until: 24 * S },
            ),
            (
                "open, failed",
                open(0),
                Do::Failed(10 * S),
                None,
                State::Cooling { until: 30 * S },
            ),
            (
                "cooling: wait",
                State::Cooling { until: 20 * S },
                Do::Acquire(19 * S, None),
                Some(Admit::Wait),
                State::Cooling { until: 20 * S },
            ),
            (
                "cooled: probe",
                State::Cooling { until: 20 * S },
                Do::Acquire(20 * S, None),
                go(Some(1)),
                probing(20 * S),
            ),
            (
                "cooled by tick",
                State::Cooling { until: 20 * S },
                Do::Tick(21 * S),
                None,
                State::Unknown,
            ),
            // A probe that ends with neither frees the gate; a stale probe's end changes nothing.
            (
                "probe released",
                probing(0),
                Do::Released(1),
                None,
                State::Unknown,
            ),
            (
                "old probe released",
                probing(0),
                Do::Released(7),
                None,
                probing(0),
            ),
            (
                "released while open",
                open(0),
                Do::Released(1),
                None,
                open(0),
            ),
            // A probe with no answer for 90 s opens the gate for everyone.
            (
                "probe overdue",
                probing(0),
                Do::Tick(90 * S),
                None,
                open(90 * S),
            ),
            (
                "probe not yet overdue",
                probing(0),
                Do::Tick(89 * S),
                None,
                probing(0),
            ),
            (
                "overdue then go",
                probing(0),
                Do::Acquire(95 * S, None),
                go(None),
                open(95 * S),
            ),
            // A background job may probe, but after 15 s with no answer the next start probes
            // instead; an answer it brings later still opens the gate.
            (
                "background: probe",
                State::Unknown,
                Do::AcquireBackground(0),
                go(Some(1)),
                background(0),
            ),
            (
                "background probe: wait",
                background(0),
                Do::Acquire(14 * S, None),
                Some(Admit::Wait),
                background(0),
            ),
            (
                "background probe overdue: next start probes",
                background(0),
                Do::Acquire(15 * S, None),
                go(Some(1)),
                probing(15 * S),
            ),
            (
                "overdue background probe answers late",
                background(0),
                Do::Answered(40 * S),
                None,
                open(40 * S),
            ),
            (
                "background behind a probe: wait",
                probing(0),
                Do::AcquireBackground(20 * S),
                Some(Admit::Wait),
                probing(0),
            ),
            // No single start waits past 90 s, whatever the gate says.
            (
                "waited too long",
                probing(60 * S),
                Do::Acquire(100 * S, Some(10 * S)),
                go(None),
                probing(60 * S),
            ),
            (
                "waited a while",
                probing(60 * S),
                Do::Acquire(100 * S, Some(20 * S)),
                Some(Admit::Wait),
                probing(60 * S),
            ),
            // Start, wake and a sign-in know nothing again.
            ("reset", open(0), Do::Reset, None, State::Unknown),
        ];
        for (name, from, act, want, end) in rows {
            let mut g = Gate {
                state: from,
                probes: 0,
            };
            let got = match act {
                Do::Acquire(now, since) => Some(g.acquire(now, since, false)),
                Do::AcquireBackground(now) => Some(g.acquire(now, None, true)),
                Do::Answered(now) => {
                    g.answered(now);
                    None
                }
                Do::Failed(now) => {
                    g.refresh_failed(now);
                    None
                }
                Do::Released(p) => {
                    g.released(p);
                    None
                }
                Do::Tick(now) => {
                    g.tick(now);
                    None
                }
                Do::Reset => {
                    g.reset();
                    None
                }
            };
            assert_eq!(got, want, "{name}");
            assert_eq!(g.state, end, "{name}");
        }
        // Must a start wait? The first in line may probe; the others wait behind it.
        let g = Gate::new();
        assert!(!g.must_wait(0, None, 0), "first in line probes");
        assert!(g.must_wait(0, None, 1), "the rest wait for the probe");
        assert!(
            !g.must_wait(MAX_WAIT_MS, Some(0), 1),
            "never past the longest wait"
        );
        let g = Gate {
            state: open(0),
            probes: 0,
        };
        assert!(!g.must_wait(MIN, None, 3), "open: nobody waits");
        assert!(
            g.must_wait(FRESH_MS, None, 1),
            "stale: the first in line probes"
        );
        let g = Gate {
            state: background(0),
            probes: 0,
        };
        assert!(g.must_wait(14 * S, None, 0), "a background probe holds");
        assert!(
            !g.must_wait(BACKGROUND_PROBE_LIMIT_MS, None, 0),
            "but not past its limit"
        );
    }

    /// The shared handle: a dropped probe ticket frees the gate, a reported one does not, async
    /// waiters see each change, and a blocking waiter goes once the probe is answered.
    #[test]
    fn tickets_report_and_free_the_gate_on_every_path() {
        let gate = Arc::new(SignInGate::new());
        let now = crate::now_ms();
        let first = gate.admit(now, None).expect("the probe goes");
        assert!(first.is_probe());
        assert!(gate.admit(now, None).is_none(), "the rest wait");
        let seen = gate.generation();
        drop(first);
        assert_eq!(gate.state(now), State::Unknown, "a dropped probe frees it");
        assert_ne!(gate.generation(), seen, "and waiters are woken");
        let probe = gate.admit(now, None).unwrap();
        // A blocking helper waits for the probe's answer, then goes as an ordinary start.
        let g2 = gate.clone();
        let waiter = std::thread::spawn(move || {
            g2.admit_blocking(Duration::from_secs(30), &|| false)
                .is_probe()
        });
        std::thread::sleep(Duration::from_millis(50));
        probe.answered(crate::now_ms());
        assert!(!waiter.join().unwrap(), "it went behind the answer");
        assert!(gate.is_open(crate::now_ms()));
        // A refresh failure cools the gate; a blocking start past its cap still goes.
        let t = gate.admit(crate::now_ms(), None).unwrap();
        t.refresh_failed(crate::now_ms());
        assert!(matches!(gate.state(crate::now_ms()), State::Cooling { .. }));
        let late = gate.admit_blocking(Duration::from_millis(30), &|| false);
        assert!(!late.is_probe());
        // A sign-in moves the counter and knows nothing again.
        let before = gate.sign_ins();
        gate.signed_in();
        assert_eq!(gate.sign_ins(), before + 1);
        assert_eq!(gate.state(crate::now_ms()), State::Unknown);
    }
}
