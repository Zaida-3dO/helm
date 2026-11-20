//! The launcher line.
//!
//! Helm cannot be started by the hub: the design has no inbound port on the PC. A small always-on
//! program on the PC, its launcher, therefore keeps one light outbound WebSocket to `/launcher`
//! (a token with the `relay` scope, the same token the PC's relay line uses) and starts Helm when
//! the hub asks. The hub also keeps what the launcher says about how to wake the PC (see
//! `wake.rs`), so the hub can offer to wake it while it is off or asleep.
//!
//! Each client has one slot; a newer connection from the same client replaces the older one. The
//! slot is separate from the relay line and never touches it. Frames are JSON text, `type` names
//! each:
//!
//! | direction | type | fields |
//! |---|---|---|
//! | launcher → hub | `hello` | `version` (1), `helmRunning` (bool), `wake` (`{"mac": "AA:BB:CC:DD:EE:FF", "broadcast": "192.168.x.255"}` or null) |
//! | hub → launcher | `welcome` | `version`, `machineId`, `machine` |
//! | launcher → hub | `state` | `helmRunning`, `crashLoop` (bool): sent on every change |
//! | hub → launcher | `launch` | `id` (16-32 lowercase hex), `reason` (`telegram`, `voice` or `button`) |
//! | launcher → hub | `launched` | `id`, `ok`, `already` (Helm was running), `error` (a plain sentence, optional) |
//!
//! The launcher pings every 30 s and the hub pings it; a slot with no sound from the launcher for
//! 100 s is dropped. What `hello` says about waking is kept with the machine, and kept when the
//! slot drops, because that is when it is needed: the hardware address must be six hex pairs and
//! the broadcast address a private IPv4 address, anything else is dropped.
//!
//! What the hub does with it (the plan, [`offline_plan`]):
//!
//! - A message for a person whose Helm is offline picks the person's machine: the one they
//!   pinned, the one that took their last message, their own machines, then machines that act for
//!   them; among those, one whose launcher is up, else one the hub can wake.
//! - **Launch** (launcher up, Helm not running): the hub sends `launch` by itself, at most one a
//!   machine every 120 s, and tells the person Helm is starting. A launch that has not led to a
//!   relay line within 120 s is reported once, with a button to try again; the hub does not launch
//!   by itself again for that machine until a person asks or its relay line has come up.
//! - **Asleep** (no launcher, wake known and switched on): the person is offered a Wake button.
//!   Waking is always a person's tap or command, never automatic, and at most one a machine a
//!   minute.

use crate::auth::Client;
use crate::registry::Registry;
use crate::server::{client_of, unauthorized, AppState, PING_EVERY, REVALIDATE_EVERY};
use crate::telegram::now_ms;
use crate::wake::Wake;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub const PROTOCOL_VERSION: u64 = 1;

/// Longest frame a launcher may send.
pub const MAX_FRAME: usize = 16 * 1024;

/// A launcher silent this long is dropped.
pub const SILENCE: Duration = Duration::from_secs(100);

/// How long a launch is waited for, and the shortest time between two launches on one machine.
pub const LAUNCH_WINDOW_MS: i64 = 120_000;

/// The shortest time between two wakes of one machine.
pub const WAKE_WINDOW_MS: i64 = 60_000;

/// Why the hub asked for Helm to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// A Telegram message waited for Helm.
    Telegram,
    /// A voice turn waited for Helm.
    Voice,
    /// A person tapped Start (or sent `/starthelm`).
    Button,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Telegram => "telegram",
            Reason::Voice => "voice",
            Reason::Button => "button",
        }
    }
}

/// What a launcher last said about Helm on its PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LauncherState {
    pub helm_running: bool,
    /// Helm keeps ending soon after it starts; the launcher has stopped restarting it. The hub
    /// still launches when a person asks (the launcher decides what to do with it).
    pub crash_loop: bool,
}

struct Slot {
    generation: u64,
    tx: mpsc::UnboundedSender<String>,
    /// `None` until the launcher's `hello`: a launcher that has not said what it sees is not
    /// counted as up.
    state: Option<LauncherState>,
}

/// What the hub has done to one machine lately.
#[derive(Default)]
struct Acts {
    machine: String,
    /// When the last `launch` went (never cleared): the rate limit.
    last_launch: Option<i64>,
    /// When the last `launch` went and the machine's relay line has not come up since.
    awaiting: Option<i64>,
    /// The person to tell, on Telegram, if that launch never arrives.
    notify: Option<i64>,
    notified: bool,
    /// What the launcher said when it could not start Helm.
    error: Option<String>,
    last_wake: Option<i64>,
}

/// What came of asking for a launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOutcome {
    Sent,
    /// Helm is running on that machine already.
    Already,
    /// A launch went to that machine less than [`LAUNCH_WINDOW_MS`] ago.
    RateLimited,
    /// An earlier launch never led to a relay line; only a person's request tries again.
    GaveUp,
    /// No launcher is up on that machine.
    NoLauncher,
}

/// What came of asking for a wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeOutcome {
    Sent,
    /// The hub is not set up to wake PCs.
    Off,
    /// That machine's launcher never said how to wake it.
    NoInfo,
    RateLimited,
    /// The hub tried and could not; a plain sentence with no secret in it.
    Failed(String),
}

/// A launch that did not lead to a relay line in time, for its person to be told once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeout {
    pub client_id: i64,
    pub machine: String,
    /// What the launcher said went wrong, if it said.
    pub error: Option<String>,
}

/// Every machine's launcher slot, and what the hub has done to each machine.
pub struct Launchers {
    slots: Mutex<HashMap<i64, Slot>>,
    acts: Mutex<HashMap<i64, Acts>>,
    generation: AtomicU64,
    registry: Arc<Mutex<Registry>>,
    wake: Mutex<Arc<Wake>>,
}

impl Launchers {
    pub fn new(registry: Arc<Mutex<Registry>>) -> Self {
        Launchers {
            slots: Mutex::new(HashMap::new()),
            acts: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            registry,
            wake: Mutex::new(Arc::new(Wake::off())),
        }
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Slot>> {
        self.slots.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn acts(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Acts>> {
        self.acts.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// How this hub wakes a PC (off until set).
    pub fn set_wake(&self, wake: Wake) {
        *self.wake.lock().unwrap_or_else(|p| p.into_inner()) = Arc::new(wake);
    }

    fn wake_mode(&self) -> Arc<Wake> {
        self.wake.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Whether the hub can wake a PC at all.
    pub fn wake_enabled(&self) -> bool {
        self.wake_mode().enabled()
    }

    /// A launcher connected: it replaces an older slot of the same client, and is sent `welcome`.
    /// Returns the connection's generation.
    pub fn attach(&self, client: &Client, tx: mpsc::UnboundedSender<String>) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = tx.send(
            json!({
                "type": "welcome",
                "version": PROTOCOL_VERSION,
                "machineId": client.id,
                "machine": client.machine,
            })
            .to_string(),
        );
        self.acts().entry(client.id).or_default().machine = client.machine.clone();
        // The older slot's sender is dropped here, which ends its socket loop.
        self.slots().insert(
            client.id,
            Slot {
                generation,
                tx,
                state: None,
            },
        );
        generation
    }

    /// The connection of `generation` ended. A newer connection is left alone.
    pub fn detach(&self, client_id: i64, generation: u64) {
        let mut slots = self.slots();
        if slots
            .get(&client_id)
            .is_some_and(|s| s.generation == generation)
        {
            slots.remove(&client_id);
        }
    }

    /// What the machine's launcher last said, while its slot is up.
    pub fn state(&self, client_id: i64) -> Option<LauncherState> {
        self.slots()
            .get(&client_id)
            .filter(|s| !s.tx.is_closed())
            .and_then(|s| s.state)
    }

    /// One frame the launcher of `generation` sent. False for a frame that is malformed, of an
    /// unknown type, or from a connection that has been replaced.
    pub fn receive(&self, client_id: i64, generation: u64, text: &str, now: i64) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return false;
        };
        let Some(kind) = v.get("type").and_then(Value::as_str) else {
            return false;
        };
        let flag = |key: &str| v.get(key).and_then(Value::as_bool);
        match kind {
            "hello" => {
                let Some(running) = flag("helmRunning") else {
                    return false;
                };
                if !self.set_state(client_id, generation, |s| {
                    s.map_or(
                        LauncherState {
                            helm_running: running,
                            crash_loop: false,
                        },
                        |s| LauncherState {
                            helm_running: running,
                            ..s
                        },
                    )
                }) {
                    return false;
                }
                // What it says about waking is kept, checked; a null leaves what was kept alone.
                if let Some(mac) = v.pointer("/wake/mac").and_then(Value::as_str) {
                    let broadcast = v.pointer("/wake/broadcast").and_then(Value::as_str);
                    let stored = self
                        .registry
                        .lock()
                        .map(|r| r.store_wake(client_id, mac, broadcast))
                        .unwrap_or(Ok(false));
                    if !matches!(stored, Ok(true)) {
                        eprintln!("launcher: dropped a wake report that did not check out");
                    }
                }
                true
            }
            "state" => {
                let Some(running) = flag("helmRunning") else {
                    return false;
                };
                let crash = flag("crashLoop").unwrap_or(false);
                self.set_state(client_id, generation, |_| LauncherState {
                    helm_running: running,
                    crash_loop: crash,
                })
            }
            "launched" => {
                if !self.is_current(client_id, generation) {
                    return false;
                }
                let ok = flag("ok").unwrap_or(false);
                let already = flag("already").unwrap_or(false);
                if already {
                    self.set_state(client_id, generation, |s| LauncherState {
                        helm_running: true,
                        crash_loop: s.is_some_and(|s| s.crash_loop),
                    });
                }
                if !ok && !already {
                    // It said it could not: stop waiting, so the person hears at the next look.
                    let error = v
                        .get("error")
                        .and_then(Value::as_str)
                        .map(|e| crate::telegram::cut(&plain(e), 200))
                        .filter(|e| !e.is_empty());
                    let mut acts = self.acts();
                    let a = acts.entry(client_id).or_default();
                    if a.awaiting.is_some() {
                        a.awaiting = Some(now - LAUNCH_WINDOW_MS);
                    }
                    a.error = error;
                }
                true
            }
            _ => false,
        }
    }

    fn is_current(&self, client_id: i64, generation: u64) -> bool {
        self.slots()
            .get(&client_id)
            .is_some_and(|s| s.generation == generation)
    }

    fn set_state(
        &self,
        client_id: i64,
        generation: u64,
        f: impl FnOnce(Option<LauncherState>) -> LauncherState,
    ) -> bool {
        let mut slots = self.slots();
        match slots
            .get_mut(&client_id)
            .filter(|s| s.generation == generation)
        {
            Some(slot) => {
                slot.state = Some(f(slot.state));
                true
            }
            None => false,
        }
    }

    /// What the plan needs to know about one machine. Takes no registry lock: `wake_ready` is
    /// the caller's.
    pub fn machine(&self, client_id: i64, name: &str, wake_ready: bool) -> Machine {
        Machine {
            client_id,
            name: name.to_owned(),
            launcher: self.state(client_id),
            awaiting: self.acts().get(&client_id).and_then(|a| a.awaiting),
            wake_ready: wake_ready && self.wake_enabled(),
        }
    }

    /// Ask the machine's launcher to start Helm. `notify` is the person to tell on Telegram if it
    /// never comes up. `manual` is a person's own request: it may try again after an earlier
    /// launch gave up; an automatic one may not.
    pub fn try_launch(
        &self,
        client_id: i64,
        reason: Reason,
        notify: Option<i64>,
        now: i64,
        manual: bool,
    ) -> LaunchOutcome {
        let mut acts = self.acts();
        let a = acts.entry(client_id).or_default();
        let slots = self.slots();
        let Some(slot) = slots.get(&client_id).filter(|s| !s.tx.is_closed()) else {
            return LaunchOutcome::NoLauncher;
        };
        let Some(state) = slot.state else {
            return LaunchOutcome::NoLauncher;
        };
        if state.helm_running {
            return LaunchOutcome::Already;
        }
        if rate_limited(a.last_launch, now, LAUNCH_WINDOW_MS) {
            return LaunchOutcome::RateLimited;
        }
        if !manual && a.awaiting.is_some() {
            return LaunchOutcome::GaveUp;
        }
        let frame = json!({
            "type": "launch",
            "id": uuid::Uuid::new_v4().simple().to_string(),
            "reason": reason.as_str(),
        });
        if slot.tx.send(frame.to_string()).is_err() {
            return LaunchOutcome::NoLauncher;
        }
        a.last_launch = Some(now);
        a.awaiting = Some(now);
        a.notify = notify;
        a.notified = false;
        a.error = None;
        LaunchOutcome::Sent
    }

    /// Wake the machine. The hub never does this by itself: it is a person's tap or command.
    pub async fn wake(&self, client_id: i64, now: i64) -> WakeOutcome {
        let wake = self.wake_mode();
        if !wake.enabled() {
            return WakeOutcome::Off;
        }
        let info = self.registry.lock().ok().and_then(|r| r.wake_of(client_id));
        let Some(info) = info else {
            return WakeOutcome::NoInfo;
        };
        {
            let mut acts = self.acts();
            let a = acts.entry(client_id).or_default();
            if rate_limited(a.last_wake, now, WAKE_WINDOW_MS) {
                return WakeOutcome::RateLimited;
            }
            a.last_wake = Some(now);
        }
        match wake.send(&info).await {
            Ok(()) => WakeOutcome::Sent,
            Err(why) => {
                eprintln!("launcher: waking a machine failed: {why}");
                WakeOutcome::Failed(why)
            }
        }
    }

    /// The machine's relay line came up or dropped. Up ends the wait for a launch.
    pub fn relay_changed(&self, client_id: i64, up: bool) {
        if up {
            if let Some(a) = self.acts().get_mut(&client_id) {
                a.awaiting = None;
                a.notify = None;
                a.notified = false;
                a.error = None;
            }
        }
    }

    /// The launches `person` asked for (by Telegram) that never led to a relay line, each once.
    pub fn timeouts(&self, person: i64, now: i64) -> Vec<Timeout> {
        let mut out = Vec::new();
        for (client_id, a) in self.acts().iter_mut() {
            let late = a.awaiting.is_some_and(|t| now - t >= LAUNCH_WINDOW_MS);
            if late && !a.notified && a.notify == Some(person) {
                a.notified = true;
                out.push(Timeout {
                    client_id: *client_id,
                    machine: a.machine.clone(),
                    error: a.error.clone(),
                });
            }
        }
        out.sort_by_key(|t| t.client_id);
        out
    }
}

/// `text` with control characters turned into spaces, so a launcher's words are one plain line.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Whether something done at `last` is too recent to do again at `now`.
pub fn rate_limited(last: Option<i64>, now: i64, window_ms: i64) -> bool {
    last.is_some_and(|t| now - t < window_ms)
}

// -------------------------------------------------------------------------------------------
// The plan
// -------------------------------------------------------------------------------------------

/// One machine a person's message could be given to, as far as the plan needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub client_id: i64,
    pub name: String,
    /// `Some` while the machine's launcher line is up and has said what it sees.
    pub launcher: Option<LauncherState>,
    /// When the hub last asked for a launch and the machine's relay line has not come up since.
    pub awaiting: Option<i64>,
    /// The hub can wake it: waking is on and its launcher once said how.
    pub wake_ready: bool,
}

/// What a message for an offline Helm gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// A launch for that machine is under way.
    Starting { client_id: i64, machine: String },
    /// The launcher is up and Helm is not running: the hub starts it.
    Launch { client_id: i64, machine: String },
    /// Helm is running there but is not connected to the hub yet.
    Connecting { client_id: i64, machine: String },
    /// No launcher, but the PC can be woken: a person's button.
    Asleep { client_id: i64, machine: String },
    /// Nothing the hub can do; the person is given steps.
    Unknown,
}

impl Plan {
    pub fn machine(&self) -> Option<&str> {
        match self {
            Plan::Starting { machine, .. }
            | Plan::Launch { machine, .. }
            | Plan::Connecting { machine, .. }
            | Plan::Asleep { machine, .. } => Some(machine),
            Plan::Unknown => None,
        }
    }

    /// The `pc` field the phone is given while Helm is offline: `state` is one of `starting`,
    /// `connecting`, `asleep`, `closed` (the launcher is up and nothing is under way) or
    /// `unknown`; `canStart` and `canWake` say which button the phone may offer.
    pub fn pc_json(&self) -> Value {
        let (state, can_start, can_wake) = match self {
            Plan::Starting { .. } => ("starting", false, false),
            Plan::Connecting { .. } => ("connecting", false, false),
            Plan::Launch { .. } => ("closed", true, false),
            Plan::Asleep { .. } => ("asleep", false, true),
            Plan::Unknown => ("unknown", false, false),
        };
        json!({
            "state": state,
            "machine": self.machine(),
            "canStart": can_start,
            "canWake": can_wake,
        })
    }
}

/// What to do for a person whose Helm is offline, given their machines best first (the order is
/// the caller's: the pinned one, the one that took their last message, their own, then those
/// that act for them). A machine whose launcher is up wins; then one with a launch under way;
/// then one the hub can wake.
pub fn offline_plan(machines: &[Machine], now: i64) -> Plan {
    let in_flight = |m: &Machine| m.awaiting.is_some_and(|t| now - t < LAUNCH_WINDOW_MS);
    let picked = machines
        .iter()
        .find(|m| m.launcher.is_some())
        .or_else(|| machines.iter().find(|m| in_flight(m)))
        .or_else(|| machines.iter().find(|m| m.wake_ready));
    let Some(m) = picked else {
        return Plan::Unknown;
    };
    let (client_id, machine) = (m.client_id, m.name.clone());
    match m.launcher {
        Some(l) if l.helm_running => Plan::Connecting { client_id, machine },
        Some(_) if in_flight(m) => Plan::Starting { client_id, machine },
        Some(_) => Plan::Launch { client_id, machine },
        None if in_flight(m) => Plan::Starting { client_id, machine },
        None => Plan::Asleep { client_id, machine },
    }
}

// -------------------------------------------------------------------------------------------
// The socket
// -------------------------------------------------------------------------------------------

/// `GET /launcher`: a machine's launcher line. Upgraded only for a token with the `relay` scope.
pub async fn connect(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(client) = client_of(&state, &headers) else {
        return unauthorized();
    };
    if !client.has(crate::auth::Scope::Relay) {
        return (
            StatusCode::FORBIDDEN,
            Json(
                json!({ "error": "only a token with the relay scope may open the launcher line" }),
            ),
        )
            .into_response();
    }
    ws.max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| run(socket, state, client))
}

async fn run(mut socket: WebSocket, state: Arc<AppState>, client: Client) {
    let launchers = state.lines.launchers().clone();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let generation = launchers.attach(&client, tx);
    eprintln!("launcher: line up ({} / {})", client.person, client.machine);
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut revalidate = tokio::time::interval(REVALIDATE_EVERY);
    revalidate.tick().await;
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(frame) => {
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                // A newer slot of this client replaced this one.
                None => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(t))) => {
                    last_heard = Instant::now();
                    if !launchers.receive(client.id, generation, t.as_str(), now_ms()) {
                        eprintln!("launcher: dropped a frame it could not use");
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => last_heard = Instant::now(),
            },
            _ = ping.tick() => {
                if last_heard.elapsed() > SILENCE {
                    eprintln!("launcher: line silent, dropping it");
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            _ = revalidate.tick() => {
                let active = state
                    .registry
                    .lock()
                    .map(|r| r.is_active(client.id))
                    .unwrap_or(false);
                if !active {
                    eprintln!("launcher: {} / {} was revoked, dropping its line", client.person, client.machine);
                    break;
                }
            }
        }
    }
    launchers.detach(client.id, generation);
    eprintln!(
        "launcher: line down ({} / {})",
        client.person, client.machine
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Scopes;

    const T0: i64 = 1_800_000_000_000;

    fn machine(
        id: i64,
        launcher: Option<(bool, bool)>,
        awaiting: Option<i64>,
        wake: bool,
    ) -> Machine {
        Machine {
            client_id: id,
            name: format!("pc{id}"),
            launcher: launcher.map(|(helm_running, crash_loop)| LauncherState {
                helm_running,
                crash_loop,
            }),
            awaiting,
            wake_ready: wake,
        }
    }

    #[test]
    fn the_plan_picks_a_machine_and_says_what_to_do() {
        let ago = |ms: i64| Some(T0 - ms);
        let rows: Vec<(&str, Vec<Machine>, Plan)> = vec![
            ("no machines", vec![], Plan::Unknown),
            (
                "machines with nothing to go on",
                vec![machine(1, None, None, false)],
                Plan::Unknown,
            ),
            (
                "launcher up, Helm closed",
                vec![machine(1, Some((false, false)), None, false)],
                Plan::Launch {
                    client_id: 1,
                    machine: "pc1".into(),
                },
            ),
            (
                "launcher up, Helm closed, and a launch went 10 s ago",
                vec![machine(1, Some((false, false)), ago(10_000), false)],
                Plan::Starting {
                    client_id: 1,
                    machine: "pc1".into(),
                },
            ),
            (
                "the launch is 119 s old: still starting",
                vec![machine(1, Some((false, false)), ago(119_000), false)],
                Plan::Starting {
                    client_id: 1,
                    machine: "pc1".into(),
                },
            ),
            (
                "the launch is 120 s old and nothing came: the hub offers to start again",
                vec![machine(1, Some((false, false)), ago(120_000), false)],
                Plan::Launch {
                    client_id: 1,
                    machine: "pc1".into(),
                },
            ),
            (
                "launcher up and Helm running, not connected",
                vec![machine(1, Some((true, false)), None, false)],
                Plan::Connecting {
                    client_id: 1,
                    machine: "pc1".into(),
                },
            ),
            (
                "running wins over a launch in flight",
                vec![machine(1, Some((true, false)), ago(5_000), false)],
                Plan::Connecting {
                    client_id: 1,
                    machine: "pc1".into(),
                },
            ),
            (
                "no launcher, wake known",
                vec![machine(1, None, None, true)],
                Plan::Asleep {
                    client_id: 1,
                    machine: "pc1".into(),
                },
            ),
            (
                "the launcher dropped after a launch: still starting",
                vec![machine(1, None, ago(30_000), true)],
                Plan::Starting {
                    client_id: 1,
                    machine: "pc1".into(),
                },
            ),
            (
                "a launch long ago and a wake known: asleep",
                vec![machine(1, None, ago(300_000), true)],
                Plan::Asleep {
                    client_id: 1,
                    machine: "pc1".into(),
                },
            ),
            (
                "a launcher beats a better-ranked machine that only has wake",
                vec![
                    machine(1, None, None, true),
                    machine(2, Some((false, false)), None, false),
                ],
                Plan::Launch {
                    client_id: 2,
                    machine: "pc2".into(),
                },
            ),
            (
                "the first of two launchers is the one picked",
                vec![
                    machine(3, Some((false, true)), None, false),
                    machine(2, Some((false, false)), None, false),
                ],
                Plan::Launch {
                    client_id: 3,
                    machine: "pc3".into(),
                },
            ),
            (
                "a launch in flight beats a wake",
                vec![
                    machine(1, None, None, true),
                    machine(2, None, ago(1_000), false),
                ],
                Plan::Starting {
                    client_id: 2,
                    machine: "pc2".into(),
                },
            ),
        ];
        for (what, machines, want) in rows {
            assert_eq!(offline_plan(&machines, T0), want, "{what}");
        }

        // The rate limits: a launch a machine every 120 s, a wake every 60 s.
        let limits = [
            (None, 0, false),
            (Some(T0), 0, true),
            (Some(T0), 119_999, true),
            (Some(T0), 120_000, false),
        ];
        for (last, later, limited) in limits {
            assert_eq!(
                rate_limited(last, T0 + later, LAUNCH_WINDOW_MS),
                limited,
                "launch {last:?} +{later}"
            );
        }
        assert!(rate_limited(Some(T0), T0 + 59_999, WAKE_WINDOW_MS));
        assert!(!rate_limited(Some(T0), T0 + 60_000, WAKE_WINDOW_MS));

        // What the phone is told.
        let pc = |plan: Plan| plan.pc_json();
        assert_eq!(
            pc(Plan::Launch {
                client_id: 1,
                machine: "desk".into()
            }),
            json!({"state": "closed", "machine": "desk", "canStart": true, "canWake": false})
        );
        assert_eq!(
            pc(Plan::Asleep {
                client_id: 1,
                machine: "desk".into()
            }),
            json!({"state": "asleep", "machine": "desk", "canStart": false, "canWake": true})
        );
        assert_eq!(
            pc(Plan::Unknown),
            json!({"state": "unknown", "machine": null, "canStart": false, "canWake": false})
        );
    }

    #[tokio::test]
    async fn a_launcher_reports_is_asked_to_start_helm_and_is_waited_for() {
        let scopes = Scopes::parse_list("relay").unwrap();
        let mut reg = Registry::open_in_memory().unwrap();
        let client_id = reg.add_client("Owner", "desk", &scopes).unwrap().client.id;
        let registry = Arc::new(Mutex::new(reg));
        let launchers = Launchers::new(registry.clone());
        let client = Client {
            id: client_id,
            name: "desk".into(),
            person_id: 1,
            person: "Owner".into(),
            machine: "desk".into(),
            scopes,
        };
        let drain = |rx: &mut mpsc::UnboundedReceiver<String>| -> Vec<Value> {
            let mut out = Vec::new();
            while let Ok(f) = rx.try_recv() {
                out.push(serde_json::from_str(&f).unwrap());
            }
            out
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        let gen = launchers.attach(&client, tx);
        let welcome = drain(&mut rx);
        assert_eq!(
            welcome,
            vec![
                json!({"type": "welcome", "version": 1, "machineId": client_id, "machine": "desk"})
            ]
        );
        // Not up until it has said what it sees.
        assert_eq!(
            launchers.try_launch(client_id, Reason::Telegram, Some(1), T0, false),
            LaunchOutcome::NoLauncher
        );
        let hello = json!({"type": "hello", "version": 1, "helmRunning": false,
            "wake": {"mac": "aa:bb:cc:dd:ee:ff", "broadcast": "8.8.8.8"}});
        assert!(launchers.receive(client_id, gen, &hello.to_string(), T0));
        assert_eq!(
            launchers.state(client_id),
            Some(LauncherState {
                helm_running: false,
                crash_loop: false
            })
        );
        // The hardware address is kept, the outside broadcast address is not.
        let kept = registry.lock().unwrap().wake_of(client_id).unwrap();
        assert_eq!(kept.mac_text(), "AA:BB:CC:DD:EE:FF");
        assert_eq!(kept.broadcast, None);
        // A malformed frame, an unknown one and one from a replaced connection are refused.
        assert!(!launchers.receive(client_id, gen, "{", T0));
        assert!(!launchers.receive(client_id, gen, r#"{"type":"nope"}"#, T0));
        assert!(!launchers.receive(client_id, gen + 1, &hello.to_string(), T0));

        // A launch goes once; a second within 120 s is held back.
        assert_eq!(
            launchers.try_launch(client_id, Reason::Telegram, Some(7), T0, false),
            LaunchOutcome::Sent
        );
        let sent = drain(&mut rx);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["type"], "launch");
        assert_eq!(sent[0]["reason"], "telegram");
        let id = sent[0]["id"].as_str().unwrap();
        assert!((16..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            launchers.try_launch(client_id, Reason::Telegram, Some(7), T0 + 1, false),
            LaunchOutcome::RateLimited
        );
        assert!(drain(&mut rx).is_empty());

        // Nothing to say before the wait is over; then it is said once, to the person asked.
        assert!(launchers.timeouts(7, T0 + 119_000).is_empty());
        assert!(launchers.timeouts(8, T0 + 121_000).is_empty());
        let late = launchers.timeouts(7, T0 + 121_000);
        assert_eq!(
            late,
            vec![Timeout {
                client_id,
                machine: "desk".into(),
                error: None
            }]
        );
        assert!(launchers.timeouts(7, T0 + 122_000).is_empty());
        // The hub does not start it again by itself, but a person's request does.
        assert_eq!(
            launchers.try_launch(client_id, Reason::Telegram, Some(7), T0 + 125_000, false),
            LaunchOutcome::GaveUp
        );
        assert_eq!(
            launchers.try_launch(client_id, Reason::Button, Some(7), T0 + 125_000, true),
            LaunchOutcome::Sent
        );
        // Once the relay line has come up the wait is over and an automatic launch is allowed
        // again, after the rate limit.
        launchers.relay_changed(client_id, true);
        assert!(launchers.timeouts(7, T0 + 500_000).is_empty());
        assert_eq!(
            launchers.try_launch(client_id, Reason::Voice, None, T0 + 130_000, false),
            LaunchOutcome::RateLimited
        );
        assert_eq!(
            launchers.try_launch(client_id, Reason::Voice, None, T0 + 250_000, false),
            LaunchOutcome::Sent
        );

        // A launcher that says it could not start Helm ends the wait at once and is quoted.
        drain(&mut rx);
        let failed =
            json!({"type": "launched", "id": id, "ok": false, "error": "Helm's folder is missing"});
        assert!(launchers.receive(client_id, gen, &failed.to_string(), T0 + 251_000));
        assert_eq!(
            launchers.machine(client_id, "desk", true).awaiting,
            Some(T0 + 251_000 - LAUNCH_WINDOW_MS)
        );
        // Helm running is what a `state` frame says, and what `already` means.
        let running = json!({"type": "state", "helmRunning": true, "crashLoop": false});
        assert!(launchers.receive(client_id, gen, &running.to_string(), T0));
        assert_eq!(
            launchers.try_launch(client_id, Reason::Button, None, T0 + 900_000, true),
            LaunchOutcome::Already
        );
        // The slot's own connection ending takes it down; a newer one is left alone.
        launchers.detach(client_id, gen + 1);
        assert!(launchers.state(client_id).is_some());
        launchers.detach(client_id, gen);
        assert!(launchers.state(client_id).is_none());
        // The hardware address outlives the line.
        assert!(registry.lock().unwrap().wake_of(client_id).is_some());
    }
}
