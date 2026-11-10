//! The relay lines of every connected machine.
//!
//! One [`Line`] per client, made the first time that client connects and kept for the life of the
//! hub: a newer connection from the same client replaces the older one inside its `Line`, while
//! different clients' lines coexist. Calls are routed to a named line; a person's Telegram
//! messages go to that person's most recently active line ([`PersonRoute`]).
//!
//! Household questions (`peer` frames) are answered here too: each line's forwarding task hands
//! them to `peer.rs` and sends the `peer_result` back down the line they came from. The voice
//! frames go to the receiver [`Lines::voice_receiver`] hands out (dropped while there is none).
//! The device and knowledge frames (`device`, `dev`, `dev_push`, `kn_pull`, `kn_put`) are answered
//! here too, by `devices.rs` and `knowledge.rs`; a line coming up or dropping refreshes the
//! household's device list.
//! Everything else a line sends up (the Telegram frames) goes to the one receiver [`Lines::new`]
//! returns.

use crate::auth::{Client, Scope};
use crate::devices;
use crate::knowledge;
use crate::peer::{self, Asker};
use crate::registry::Registry;
use crate::relay::{Line, Route, Upstream};
use crate::server::AppState;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::mpsc;

#[derive(Clone)]
struct Slot {
    person_id: i64,
    machine: String,
    line: Arc<Line>,
    /// Whether the client's token has the `peer` scope, as of its last connection.
    peer: bool,
}

/// What a line sent up that is not a call result, tagged with the person whose machine sent it.
pub type PersonUpstream = (i64, Upstream);

pub struct Lines {
    slots: Mutex<HashMap<i64, Slot>>,
    upstream: mpsc::UnboundedSender<PersonUpstream>,
    /// For the household questions: who the people and their machines are.
    registry: Arc<Mutex<Registry>>,
    /// Where the voice frames go, once the voice bridge asked for them.
    voice: Mutex<Option<mpsc::UnboundedSender<PersonUpstream>>>,
    /// The hub's state, for the knowledge frames (they read and write the store). Set by
    /// [`Lines::bind`] when the router is made.
    state: Mutex<Option<Weak<AppState>>>,
}

impl Lines {
    /// The registry, plus the receiver for everything the lines send up. Each frame is tagged
    /// with its person so the Telegram bridge of that person handles it.
    pub fn new(registry: Arc<Mutex<Registry>>) -> (Self, mpsc::UnboundedReceiver<PersonUpstream>) {
        let (upstream, rx) = mpsc::unbounded_channel();
        (
            Lines {
                slots: Mutex::new(HashMap::new()),
                upstream,
                registry,
                voice: Mutex::new(None),
                state: Mutex::new(None),
            },
            rx,
        )
    }

    /// Let the lines reach the hub's state (the knowledge store). Called once by the router.
    pub fn bind(&self, state: &Arc<AppState>) {
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::downgrade(state));
    }

    /// The hub's state, while the router is alive.
    pub fn app_state(&self) -> Option<Arc<AppState>> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(Weak::upgrade)
    }

    /// The client registry the lines answer from.
    pub fn registry(&self) -> &Arc<Mutex<Registry>> {
        &self.registry
    }

    /// Whether the client's token carries the `peer` scope (false for a client never seen).
    pub fn is_peer(&self, client_id: i64) -> bool {
        self.lock().get(&client_id).is_some_and(|s| s.peer)
    }

    /// Every connected line whose token has the `peer` scope, with its client id.
    pub fn peer_lines(&self) -> Vec<(i64, Arc<Line>)> {
        self.lock()
            .iter()
            .filter(|(_, s)| s.peer && s.line.is_up())
            .map(|(id, s)| (*id, s.line.clone()))
            .collect()
    }

    /// Send `frame` down every connected `peer` line.
    pub fn broadcast_peers(&self, frame: &Value) {
        for (_, line) in self.peer_lines() {
            line.send(frame);
        }
    }

    /// The receiver for the voice frames the lines send up, tagged with their person. A second
    /// call replaces the first receiver's feed.
    pub fn voice_receiver(&self) -> mpsc::UnboundedReceiver<PersonUpstream> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.voice.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
        rx
    }

    fn voice_sink(&self) -> Option<mpsc::UnboundedSender<PersonUpstream>> {
        self.voice.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Slot>> {
        self.slots.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The line of `client`, made on first use. Must run inside the tokio runtime (a task
    /// forwards the line's upstream frames).
    pub fn line_for(self: &Arc<Self>, client: &Client) -> Arc<Line> {
        let mut slots = self.lock();
        if let Some(slot) = slots.get_mut(&client.id) {
            // A renamed machine keeps its line.
            slot.machine.clone_from(&client.machine);
            slot.peer = client.has(Scope::Peer);
            return slot.line.clone();
        }
        let (line, mut rx) = Line::in_namespace(client.id as u64);
        let line = Arc::new(line);
        let weak = Arc::downgrade(self);
        let client_id = client.id;
        line.on_change(move |_up| devices::on_line_change(&weak, client_id));
        let (tx, person_id) = (self.upstream.clone(), client.person_id);
        let asker = Asker {
            person_id,
            person: client.person.clone(),
            allowed: client.has(Scope::Peer),
        };
        let (lines, own_line) = (self.clone(), line.clone());
        tokio::spawn(async move {
            while let Some(u) = rx.recv().await {
                match u {
                    // Each question is answered on a task of its own: a slow machine must not
                    // hold up the Telegram frames behind it.
                    Upstream::Peer { id, op, args } => {
                        let (lines, line, asker) = (lines.clone(), own_line.clone(), asker.clone());
                        tokio::spawn(async move {
                            let answer =
                                peer::answer(&lines.registry, &lines, &asker, &op, &args).await;
                            line.send(&peer::result_frame(&id, answer));
                        });
                    }
                    // The household's devices and knowledge. These are handled in order, on this
                    // task, except a device call, which waits for another machine.
                    Upstream::Device { uid, name, run } => {
                        devices::on_device(&lines, client_id, &uid, &name, run);
                    }
                    Upstream::Dev {
                        id,
                        to,
                        op,
                        args,
                        size,
                    } => {
                        let (lines, line) = (lines.clone(), own_line.clone());
                        tokio::spawn(async move {
                            let reply =
                                devices::route(&lines, client_id, id, &to, &op, args, size).await;
                            line.send(&reply);
                        });
                    }
                    Upstream::DevPush {
                        to,
                        sub,
                        seq,
                        event,
                    } => devices::push(&lines, client_id, &to, sub, seq, event),
                    Upstream::KnPull { id, since, limit } => {
                        own_line.send(&knowledge::pull_frame(&lines, client_id, &id, since, limit));
                    }
                    Upstream::KnPut {
                        id,
                        layer,
                        path,
                        base_seq,
                        sha256,
                        deleted,
                        body,
                    } => {
                        let put = knowledge::PutFrame {
                            layer: &layer,
                            path: &path,
                            base_seq,
                            sha256: &sha256,
                            deleted,
                            body: body.as_deref(),
                        };
                        let (reply, changed) = knowledge::put_frame(&lines, client_id, &id, &put);
                        own_line.send(&reply);
                        if let Some(seq) = changed {
                            lines.broadcast_peers(&serde_json::json!({
                                "type": "kn_changed",
                                "seq": seq,
                            }));
                        }
                    }
                    // The voice bridge's frames are dropped until it has asked for them.
                    other if other.is_voice() => {
                        if let Some(voice) = lines.voice_sink() {
                            let _ = voice.send((person_id, other));
                        }
                    }
                    other => {
                        let _ = tx.send((person_id, other));
                    }
                }
            }
        });
        slots.insert(
            client.id,
            Slot {
                person_id,
                machine: client.machine.clone(),
                line: line.clone(),
                peer: client.has(Scope::Peer),
            },
        );
        line
    }

    /// The line of a client, if it ever connected.
    pub fn get(&self, client_id: i64) -> Option<Arc<Line>> {
        self.lock().get(&client_id).map(|s| s.line.clone())
    }

    /// Every connected line of a person with its machine's name.
    pub fn up_lines_of(&self, person_id: i64) -> Vec<(String, Arc<Line>)> {
        self.lock()
            .values()
            .filter(|s| s.person_id == person_id && s.line.is_up())
            .map(|s| (s.machine.clone(), s.line.clone()))
            .collect()
    }

    /// The most recently active connected line of a person.
    pub fn best_up(&self, person_id: i64) -> Option<Arc<Line>> {
        self.lock()
            .values()
            .filter(|s| s.person_id == person_id && s.line.is_up())
            .max_by_key(|s| s.line.activity())
            .map(|s| s.line.clone())
    }

    /// The line a caller's call goes down: the person's machine called `machine`, or with no
    /// name the person's most recently active connected line. A named machine is returned even
    /// when its line is down, so the call answers "offline" itself. Never another person's line.
    pub fn pick(&self, person_id: i64, machine: Option<&str>) -> Option<Arc<Line>> {
        match machine {
            None => self.best_up(person_id),
            Some(name) => self
                .lock()
                .values()
                .find(|s| s.person_id == person_id && s.machine.eq_ignore_ascii_case(name))
                .map(|s| s.line.clone()),
        }
    }
}

/// All of one person's machines as a single Telegram destination.
pub struct PersonRoute {
    lines: Arc<Lines>,
    person_id: i64,
}

impl PersonRoute {
    pub fn new(lines: Arc<Lines>, person_id: i64) -> Self {
        PersonRoute { lines, person_id }
    }
}

impl Route for PersonRoute {
    fn is_up(&self) -> bool {
        self.lines.best_up(self.person_id).is_some()
    }

    fn generation(&self) -> Option<u64> {
        self.lines.best_up(self.person_id)?.generation()
    }

    fn send(&self, frame: &Value) -> bool {
        self.lines
            .best_up(self.person_id)
            .is_some_and(|l| l.send(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Scopes;
    use serde_json::json;

    fn client(id: i64, person_id: i64, machine: &str) -> Client {
        Client {
            id,
            name: machine.into(),
            person_id,
            person: format!("p{person_id}"),
            machine: machine.into(),
            scopes: Scopes::default(),
        }
    }

    fn registry() -> Arc<Mutex<Registry>> {
        Arc::new(Mutex::new(Registry::open_in_memory().unwrap()))
    }

    #[tokio::test]
    async fn lines_coexist_replace_per_client_and_route_by_person_and_activity() {
        let (lines, mut up) = Lines::new(registry());
        let lines = Arc::new(lines);
        let (a, b, c) = (
            client(1, 10, "desk"),
            client(2, 10, "laptop"),
            client(3, 20, "pc"),
        );
        let (la, lb, lc) = (lines.line_for(&a), lines.line_for(&b), lines.line_for(&c));
        assert!(Arc::ptr_eq(&la, &lines.line_for(&a)), "one line per client");

        let (tx_a, mut rx_a) = mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = mpsc::unbounded_channel();
        let (tx_c, _rx_c) = mpsc::unbounded_channel();
        let (ga, gb, gc) = (la.attach(tx_a), lb.attach(tx_b), lc.attach(tx_c));
        assert!(ga != gb && gb != gc, "generations differ between clients");

        // A second machine does not knock the first off; the person's latest wins Telegram.
        assert!(la.is_up() && lb.is_up());
        let route = PersonRoute::new(lines.clone(), 10);
        assert_eq!(route.generation(), Some(gb));
        assert!(route.send(&json!({"type": "telegram_stop"})));
        assert!(rx_a.try_recv().unwrap().contains("welcome"));
        assert!(rx_a.try_recv().is_err(), "the other machine got nothing");
        assert!(rx_b.try_recv().unwrap().contains("welcome"));
        assert!(rx_b.try_recv().unwrap().contains("telegram_stop"));

        // Activity on the first moves the route to it; frames are tagged with the person.
        assert!(la.receive(ga, r#"{"type":"telegram_final","text":"hi"}"#));
        assert_eq!(route.generation(), Some(ga));
        assert_eq!(
            up.recv().await.unwrap(),
            (10, Upstream::TelegramFinal("hi".into()))
        );

        // A named machine is returned even when down; another person's is never reachable.
        lb.detach(gb);
        assert!(lines.pick(10, Some("LAPTOP")).is_some());
        assert!(lines.pick(10, Some("pc")).is_none());
        assert_eq!(lines.up_lines_of(10).len(), 1);
        assert!(lines.pick(30, None).is_none());
        assert!(!PersonRoute::new(lines.clone(), 30).is_up());

        // Voice frames skip the Telegram receiver and arrive tagged with their person.
        let ack = r#"{"type":"voice_ack","id":"turn-0001"}"#;
        let mut voice = lines.voice_receiver();
        assert!(la.receive(ga, ack));
        assert_eq!(
            voice.recv().await.unwrap(),
            (10, Upstream::VoiceAck("turn-0001".into()))
        );
        assert!(up.try_recv().is_err(), "Telegram never saw a voice frame");
    }
}
