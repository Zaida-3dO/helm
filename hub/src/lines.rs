//! The relay lines of every connected machine.
//!
//! One [`Line`] per client, made the first time that client connects and kept for the life of the
//! hub: a newer connection from the same client replaces the older one inside its `Line`, while
//! different clients' lines coexist. Calls are routed to a named line; a person's Telegram
//! messages go to that person's most recently active line ([`PersonRoute`]).
//!
//! Household questions (`peer` frames) are answered here too: each line's forwarding task hands
//! them to `peer.rs` and sends the `peer_result` back down the line they came from. Everything
//! else a line sends up (the Telegram frames) goes to the one receiver [`Lines::new`] returns.

use crate::auth::{Client, Scope};
use crate::peer::{self, Asker};
use crate::registry::Registry;
use crate::relay::{Line, Route, Upstream};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Clone)]
struct Slot {
    person_id: i64,
    machine: String,
    line: Arc<Line>,
}

/// What a line sent up that is not a call result, tagged with the person whose machine sent it.
pub type PersonUpstream = (i64, Upstream);

pub struct Lines {
    slots: Mutex<HashMap<i64, Slot>>,
    upstream: mpsc::UnboundedSender<PersonUpstream>,
    /// For the household questions: who the people and their machines are.
    registry: Arc<Mutex<Registry>>,
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
            },
            rx,
        )
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
            return slot.line.clone();
        }
        let (line, mut rx) = Line::in_namespace(client.id as u64);
        let line = Arc::new(line);
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
    }
}
