//! The relay lines of every connected machine.
//!
//! One [`Line`] per client, made the first time that client connects and kept for the life of the
//! hub: a newer connection from the same client replaces the older one inside its `Line`, while
//! different clients' lines coexist. A person's messages are routed by [`Lines::route_for`]:
//! the PC that person pinned, else the one that took their last message, else their own PCs,
//! else any PC that acts for them ([`PersonRoute`] wraps it for the Telegram bridge). A frame
//! naming the person it acts for is checked against what the sending machine may do before it
//! is tagged with that person.
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
use crate::relay::{Channel, Line, Route, Upstream};
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

/// What a line sent up that is not a call result: the person it acts for, the machine that sent
/// it, and the frame. The sender travels with it so an answer can go back to that machine — a
/// question one PC asked is not another PC's to answer.
pub type PersonUpstream = (i64, i64, Upstream);

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
    /// For each person and channel, the client that acknowledged that person's last message
    /// there. Second only to a stored pin when [`Lines::route_for`] picks; a client that is no
    /// longer a candidate (line down, no longer acts for them, channel off) simply never wins.
    sticky: Mutex<HashMap<(i64, Channel), i64>>,
    /// The Telegram bot, while it runs, so a profile's `tg_link` / `tg_unlink` / `tg_status` can
    /// be answered on the line that asked (see `telegram.rs`).
    telegram: Mutex<Option<Weak<crate::telegram::Telegram>>>,
}

impl Lines {
    /// Let the lines reach the Telegram bot, for the link questions a profile asks. Called by
    /// [`crate::telegram::Telegram::attach`].
    pub fn bind_telegram(&self, bot: &Arc<crate::telegram::Telegram>) {
        *self.telegram.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::downgrade(bot));
    }

    /// The bot, while it runs.
    pub fn telegram(&self) -> Option<Arc<crate::telegram::Telegram>> {
        self.telegram
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(Weak::upgrade)
    }

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
                telegram: Mutex::new(None),
                state: Mutex::new(None),
                sticky: Mutex::new(HashMap::new()),
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
        let machine = client.machine.clone();
        let asker = Asker {
            person_id,
            person: client.person.clone(),
            allowed: client.has(Scope::Peer),
        };
        let (lines, own_line) = (self.clone(), line.clone());
        let dev_calls = Arc::new(tokio::sync::Semaphore::new(devices::MAX_DEV_CALLS));
        tokio::spawn(async move {
            while let Some((claimed, u)) = rx.recv().await {
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
                    Upstream::Device {
                        uid,
                        name,
                        run,
                        people,
                    } => {
                        devices::on_device(&lines, client_id, &uid, &name, run, &people);
                    }
                    Upstream::PersonEnsure { id, name } => {
                        if !lines.is_peer(client_id) {
                            own_line.send(&devices::person_result_error(
                                &id,
                                "This machine's token cannot use the household.",
                            ));
                            continue;
                        }
                        let frame = match lines.registry.lock() {
                            Ok(mut r) => match r.ensure_person_created(&name) {
                                Ok((p, created)) => {
                                    if created {
                                        eprintln!("household: {} made person {}", machine, p.name);
                                    }
                                    devices::person_result_ok(&id, &p, created)
                                }
                                Err(e) => devices::person_result_error(&id, &e.to_string()),
                            },
                            Err(_) => {
                                devices::person_result_error(&id, "the registry is unavailable")
                            }
                        };
                        own_line.send(&frame);
                    }
                    Upstream::RoutePin { person_id, channel } => {
                        lines.on_route_pin(client_id, &person_id, &channel);
                    }
                    // A profile asking about its own Telegram link. Answered on the line that
                    // asked, since the answer is a link for that person to tap on their phone.
                    up @ Upstream::TgAdmin { .. } => match lines.telegram() {
                        Some(bot) => {
                            let frame = bot.answer_admin(&lines, client_id, &up);
                            own_line.send(&frame);
                        }
                        None => {
                            // Still answered with the id it was asked with and the matching
                            // result type, or the profile waits out its own timeout instead of
                            // saying why.
                            own_line.send(&crate::telegram::no_bot_answer(&up));
                        }
                    },
                    Upstream::Dev {
                        id,
                        to,
                        op,
                        args,
                        size,
                        persons,
                    } => {
                        let Ok(permit) = dev_calls.clone().try_acquire_owned() else {
                            own_line.send(&devices::busy(&id));
                            continue;
                        };
                        let (lines, line) = (lines.clone(), own_line.clone());
                        tokio::spawn(async move {
                            let reply = devices::route(
                                &lines, client_id, id, &to, &op, args, size, &persons,
                            )
                            .await;
                            line.send(&reply);
                            drop(permit);
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
                    // A Telegram or voice frame that named a person is checked before it can be
                    // tagged with them; one naming someone this machine cannot act for is
                    // dropped and logged, so a wrong or dishonest person id never reaches the
                    // person's bridges. A frame that named no one, or one that is not a
                    // person-bearing kind, is tagged with the machine's own person, as always.
                    other if other.takes_person() => {
                        let tag = match &claimed {
                            Some(p) => match p.trim().parse::<i64>() {
                                Ok(pid) if lines.may_act_as(client_id, pid) => pid,
                                _ => {
                                    eprintln!(
                                        "relay: dropped a frame from {machine} naming person {p:?}"
                                    );
                                    continue;
                                }
                            },
                            None => person_id,
                        };
                        if let Some(ch) = sticky_channel(&other) {
                            lines.note_sticky(tag, ch, client_id);
                        }
                        if other.is_voice() {
                            // The voice bridge's frames are dropped until it has asked for them.
                            if let Some(voice) = lines.voice_sink() {
                                let _ = voice.send((tag, client_id, other));
                            }
                        } else {
                            let _ = tx.send((tag, client_id, other));
                        }
                    }
                    other => {
                        let _ = tx.send((person_id, client_id, other));
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

    /// The connected line that a frame for `person_id` on `channel` went down, found by the
    /// connection it went down. Generations are unique per client, so this finds the same line
    /// again even after the route has moved to another machine — which is what keeps a message
    /// with the machine it went to, and lets an answer go back where the question was asked.
    /// A line that is not a candidate for that person and channel is never found.
    pub fn up_on_candidate(
        &self,
        person_id: i64,
        channel: Channel,
        generation: u64,
    ) -> Option<Arc<Line>> {
        self.candidates_for(person_id, channel)
            .into_iter()
            .map(|(_, _, line)| line)
            .find(|line| line.generation() == Some(generation))
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

    /// Whether the machine `client_id` may act for `person_id` on this hub: as its own person,
    /// by an allowance an admin gave, or by a people announcement it last made.
    pub fn may_act_as(&self, client_id: i64, person_id: i64) -> bool {
        self.registry
            .lock()
            .is_ok_and(|r| r.may_act_for(client_id, person_id))
    }

    /// Remember that `client_id` placed `person_id`'s last message on `channel`, so the next
    /// one goes to the same machine while it stays a candidate.
    fn note_sticky(&self, person_id: i64, channel: Channel, client_id: i64) {
        self.sticky
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((person_id, channel), client_id);
    }

    /// Make `client_id` the machine that receives `person_id`'s `channel`, until they clear it.
    pub fn pin(&self, person_id: i64, channel: Channel, client_id: i64) -> bool {
        match self.registry.lock() {
            Ok(mut r) => r.pin_route(person_id, channel.label(), client_id).is_ok(),
            Err(_) => false,
        }
    }

    /// Drop the person's pin for `channel`, returning to automatic routing.
    pub fn clear_pin(&self, person_id: i64, channel: Channel) -> bool {
        match self.registry.lock() {
            Ok(mut r) => r.clear_route(person_id, channel.label()).is_ok(),
            Err(_) => false,
        }
    }

    /// A `route_pin`: the sending PC asks to take a person's channel. Only a PC that acts for
    /// that person may pin itself to it.
    fn on_route_pin(&self, client_id: i64, person_id: &str, channel: &str) {
        let Some(channel) = Channel::parse(channel) else {
            eprintln!("relay: a route_pin named an unknown channel {channel:?}");
            return;
        };
        let Ok(person) = person_id.trim().parse::<i64>() else {
            eprintln!("relay: a route_pin named a person id that is not a number: {person_id:?}");
            return;
        };
        if !self.may_act_as(client_id, person) {
            eprintln!("relay: a route_pin was refused: this machine does not act for that person");
            return;
        }
        if self.pin(person, channel, client_id) {
            eprintln!("relay: a route_pin now sends person {person} to machine {client_id}");
        }
    }

    /// The line a person's `channel` messages go down, or `None` when nothing qualifies.
    ///
    /// Candidates are lines that are up, whose client acts for the person (its own person, an
    /// admin allowance or a people announcement), and — for `telegram` and `voice` — whose last
    /// `device` frame says it takes that channel for that person (a line that made no claim
    /// takes them only for its own person). The pick, in order: the PC the person pinned, if it
    /// is a candidate; the PC that placed the person's last message there, if it is one; the
    /// person's own PCs, most recently active first; then any candidate, most recently active
    /// first.
    pub fn route_for(&self, person_id: i64, channel: Channel) -> Option<Arc<Line>> {
        let Ok(reg) = self.registry.lock() else {
            return None;
        };
        let pinned = reg.pinned_client(person_id, channel.label()).ok().flatten();
        drop(reg);
        let candidates = self.candidates_for(person_id, channel);
        let wanted = |id: i64| {
            candidates
                .iter()
                .find(|(client_id, _, _)| *client_id == id)
                .map(|(_, _, line)| line.clone())
        };
        if let Some(line) = pinned.and_then(wanted) {
            return Some(line);
        }
        let sticky = self
            .sticky
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(person_id, channel))
            .copied();
        if let Some(line) = sticky.and_then(wanted) {
            return Some(line);
        }
        if let Some((_, _, line)) = candidates
            .iter()
            .filter(|(_, home, _)| *home == person_id)
            .max_by_key(|(_, _, line)| line.activity())
        {
            return Some(line.clone());
        }
        candidates
            .iter()
            .max_by_key(|(_, _, line)| line.activity())
            .map(|(_, _, line)| line.clone())
    }

    /// Every line that is a candidate for `person_id` on `channel`: client id, the machine's
    /// own person (its home person), and the line. See [`Lines::route_for`] for the rules.
    pub fn candidates_for(&self, person_id: i64, channel: Channel) -> Vec<(i64, i64, Arc<Line>)> {
        self.candidate_views(person_id, channel)
            .into_iter()
            .map(|(id, home, _, line)| (id, home, line))
            .collect()
    }

    /// The machine names of every candidate line for `person_id` on `channel`.
    pub fn candidate_names(&self, person_id: i64, channel: Channel) -> Vec<String> {
        self.candidate_views(person_id, channel)
            .into_iter()
            .map(|(_, _, machine, _)| machine)
            .collect()
    }

    /// Every candidate line for `person_id` on `channel` as `(client id, machine name, line)`,
    /// for a person choosing one of them (the bot's `/pc`).
    pub fn candidate_lines(
        &self,
        person_id: i64,
        channel: Channel,
    ) -> Vec<(i64, String, Arc<Line>)> {
        self.candidate_views(person_id, channel)
            .into_iter()
            .map(|(client_id, _, machine, line)| (client_id, machine, line))
            .collect()
    }

    fn candidate_views(
        &self,
        person_id: i64,
        channel: Channel,
    ) -> Vec<(i64, i64, String, Arc<Line>)> {
        let slots: Vec<(i64, i64, String, Arc<Line>)> = self
            .lock()
            .iter()
            .map(|(client_id, s)| (*client_id, s.person_id, s.machine.clone(), s.line.clone()))
            .collect();
        let Ok(reg) = self.registry.lock() else {
            return Vec::new();
        };
        slots
            .into_iter()
            .filter(|(client_id, home, _, line)| {
                // A line always acts for its own person (the slot knows it from the authenticated
                // attach); for anyone else it needs an allowance: an admin one in the registry,
                // or one the machine itself announced.
                line.is_up()
                    && (*home == person_id || reg.may_act_for(*client_id, person_id))
                    && match channel {
                        Channel::App => true,
                        telegram_or_voice => match line.claim_for(person_id) {
                            Some((telegram, voice)) => match telegram_or_voice {
                                Channel::Telegram => telegram,
                                Channel::Voice => voice,
                                Channel::App => true,
                            },
                            None => *home == person_id,
                        },
                    }
            })
            .collect()
    }

    /// The candidate line for `person_id` on `channel` whose machine is named `machine`
    /// (case-insensitive), with its client id and name. Two people's machines can carry the same
    /// name, so the one whose own person is `person_id` wins; a machine that is down, or is not a
    /// candidate for this person and channel, is never chosen.
    pub fn find_by_name(
        &self,
        person_id: i64,
        channel: Channel,
        machine: &str,
    ) -> Option<(i64, String, Arc<Line>)> {
        let found: Vec<(i64, i64, String, Arc<Line>)> = self
            .candidate_views(person_id, channel)
            .into_iter()
            .filter(|(_, _, name, _)| name.eq_ignore_ascii_case(machine))
            .collect();
        found
            .iter()
            .find(|(_, home, _, _)| *home == person_id)
            .or_else(|| found.first())
            .cloned()
            .map(|(client_id, _, name, line)| (client_id, name, line))
    }

    /// The candidate line for `person_id` on `channel` whose machine is named `machine`; `None`
    /// when that machine is down or is not a candidate — never another machine's line. See
    /// [`Self::find_by_name`] for how two machines of the same name are told apart.
    pub fn named_candidate(
        &self,
        person_id: i64,
        machine: &str,
        channel: Channel,
    ) -> Option<Arc<Line>> {
        self.find_by_name(person_id, channel, machine)
            .map(|(_, _, line)| line)
    }
}

/// The channel an acknowledgement anchors a conversation to.
fn sticky_channel(up: &Upstream) -> Option<Channel> {
    match up {
        Upstream::TelegramAck(_) => Some(Channel::Telegram),
        Upstream::VoiceAck(_) => Some(Channel::Voice),
        _ => None,
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

    /// This person's Telegram line, as [`Lines::route_for`] picks it.
    fn route(&self) -> Option<Arc<Line>> {
        self.lines.route_for(self.person_id, Channel::Telegram)
    }
}

impl Route for PersonRoute {
    fn is_up(&self) -> bool {
        self.route().is_some()
    }

    fn generation(&self) -> Option<u64> {
        self.route()?.generation()
    }

    fn send(&self, frame: &Value) -> bool {
        self.route().is_some_and(|l| l.send(frame))
    }

    // A message stays with the machine it went to while that machine's line is up, even when the
    // route for the person now points elsewhere — which is how a pinned or sticky non-home PC, or
    // one picked only because it is up, still gets its answers.
    fn holds(&self, generation: u64) -> bool {
        self.lines
            .up_on_candidate(self.person_id, Channel::Telegram, generation)
            .is_some()
    }

    fn send_on(&self, generation: u64, frame: &Value) -> bool {
        self.lines
            .up_on_candidate(self.person_id, Channel::Telegram, generation)
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

    /// A registry holding client rows that match the `client` ids below, so the hub's
    /// may-act-for checks see the same machines the lines do.
    fn registry() -> Arc<Mutex<Registry>> {
        let mut r = Registry::open_in_memory().unwrap();
        for (person, machine) in [("p10", "desk"), ("p10", "laptop"), ("p20", "pc")] {
            r.add_client(person, machine, &Scopes::default()).unwrap();
        }
        Arc::new(Mutex::new(r))
    }

    #[tokio::test]
    async fn lines_coexist_replace_per_client_and_route_by_person_and_activity() {
        let registry = registry();
        let (p10, p20) = {
            let r = registry.lock().unwrap();
            (
                r.find_person("p10").unwrap().id,
                r.find_person("p20").unwrap().id,
            )
        };
        let (lines, mut up) = Lines::new(registry);
        let lines = Arc::new(lines);
        let (a, b, c) = (
            client(1, p10, "desk"),
            client(2, p10, "laptop"),
            client(3, p20, "pc"),
        );
        let (la, lb, lc) = (lines.line_for(&a), lines.line_for(&b), lines.line_for(&c));
        assert!(Arc::ptr_eq(&la, &lines.line_for(&a)), "one line per client");

        let (tx_a, mut rx_a) = mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = mpsc::unbounded_channel();
        let (tx_c, _rx_c) = mpsc::unbounded_channel();
        let (ga, gb, gc) = (la.attach(tx_a), lb.attach(tx_b), lc.attach(tx_c));
        assert!(ga != gb && gb != gc, "generations differ between clients");

        // Another person's machine carries the same name as this person's own. A named choice
        // takes the person's own, and never lands on the other person's of that name.
        let d = client(4, p20, "DESK");
        let ld = lines.line_for(&d);
        let (tx_d, _rx_d) = mpsc::unbounded_channel();
        ld.attach(tx_d);
        assert!(Arc::ptr_eq(
            &lines.named_candidate(p10, "desk", Channel::App).unwrap(),
            &la
        ));
        assert!(Arc::ptr_eq(
            &lines.named_candidate(p20, "desk", Channel::App).unwrap(),
            &ld
        ));
        assert!(lines
            .named_candidate(p10, "laptop", Channel::Voice)
            .is_some());
        assert!(lines.named_candidate(p10, "desk", Channel::App).is_some());

        // A second machine does not knock the first off; the person's latest wins Telegram.
        assert!(la.is_up() && lb.is_up());
        let route = PersonRoute::new(lines.clone(), p10);
        assert_eq!(route.generation(), Some(gb));
        assert!(route.send(&json!({"type": "telegram_stop"})));
        assert!(rx_a.try_recv().unwrap().contains("welcome"));
        assert!(rx_a.try_recv().is_err(), "the other machine got nothing");
        assert!(rx_b.try_recv().unwrap().contains("welcome"));
        assert!(rx_b.try_recv().unwrap().contains("telegram_stop"));
        // A frame may also go to one machine's connection, while that one is up; never to
        // another person's.
        assert!(route.holds(ga) && route.holds(gb) && !route.holds(gc));
        assert!(route.send_on(ga, &json!({"type": "telegram_stop"})));
        assert!(rx_a.try_recv().unwrap().contains("telegram_stop"));
        assert!(rx_b.try_recv().is_err());
        assert!(!route.send_on(gc, &json!({"type": "telegram_stop"})));

        // Activity on the first moves the route to it; frames are tagged with the person.
        assert!(la.receive(ga, r#"{"type":"telegram_final","text":"hi"}"#));
        assert_eq!(route.generation(), Some(ga));
        assert_eq!(
            up.recv().await.unwrap(),
            (p10, 1, Upstream::TelegramFinal("hi".into()))
        );

        // A named machine is returned even when down; another person's is never reachable.
        lb.detach(gb);
        assert!(lines.pick(p10, Some("LAPTOP")).is_some());
        assert!(lines.pick(p10, Some("pc")).is_none());
        assert_eq!(lines.candidate_names(p10, Channel::App).len(), 1);
        assert!(lines.pick(30, None).is_none());
        assert!(!PersonRoute::new(lines.clone(), 30).is_up());

        // Voice frames skip the Telegram receiver and arrive tagged with their person.
        let ack = r#"{"type":"voice_ack","id":"turn-0001"}"#;
        let mut voice = lines.voice_receiver();
        assert!(la.receive(ga, ack));
        assert_eq!(
            voice.recv().await.unwrap(),
            (p10, 1, Upstream::VoiceAck("turn-0001".into()))
        );
        assert!(up.try_recv().is_err(), "Telegram never saw a voice frame");
    }

    /// The hub half of the whole feature, on the wire: announced people (with replace
    /// semantics that spare admin rows), device calls acting for a verified person, upstream
    /// frames tagged only when allowed, `route_for` picking pinned > sticky > home > any, and
    /// `person_ensure` making a person once.
    #[tokio::test]
    async fn a_pc_acts_for_its_profiles_and_each_persons_channels_route_to_one_pc() {
        let registry = Arc::new(Mutex::new(Registry::open_in_memory().unwrap()));
        let (bea, kim, ivy) = {
            let mut r = registry.lock().unwrap();
            (
                r.ensure_person("Bea").unwrap().id,
                r.ensure_person("Kim").unwrap().id,
                r.ensure_person("Ivy").unwrap().id,
            )
        };
        let scopes = Scopes::parse_list("relay,peer").unwrap();
        let (desk, laptop) = {
            let mut r = registry.lock().unwrap();
            let d = r.add_client("Bea", "desk", &scopes).unwrap().client.id;
            let l = r.add_client("Kim", "laptop", &scopes).unwrap().client.id;
            (d, l)
        };
        let cl = |id: i64, person_id: i64, person: &str, machine: &str| Client {
            id,
            name: machine.into(),
            person_id,
            person: person.into(),
            machine: machine.into(),
            scopes: scopes.clone(),
        };
        let (lines, mut up) = Lines::new(registry.clone());
        let lines = Arc::new(lines);
        let dline = lines.line_for(&cl(desk, bea, "Bea", "desk"));
        let lline = lines.line_for(&cl(laptop, kim, "Kim", "laptop"));
        let (tx_d, mut rx_d) = mpsc::unbounded_channel();
        let (tx_l, mut rx_l) = mpsc::unbounded_channel();
        let gd = dline.attach(tx_d);
        let gl = lline.attach(tx_l);

        async fn wait_frames(rx: &mut mpsc::UnboundedReceiver<String>, what: &str) -> Vec<Value> {
            loop {
                let mut got = Vec::new();
                while let Ok(f) = rx.try_recv() {
                    let v: Value = serde_json::from_str(&f).unwrap();
                    if v["type"] == what {
                        got.push(v);
                    }
                }
                if !got.is_empty() {
                    return got;
                }
                tokio::task::yield_now().await;
            }
        }
        async fn wait_until(cond: impl Fn() -> bool) {
            while !cond() {
                tokio::task::yield_now().await;
            }
        }

        // Laptop announces it acts for Bea (Telegram yes, voice no); Desk announces nobody.
        assert!(lline.receive(
            gl,
            &json!({"type": "device", "uid": "uid-laptop", "name": "Laptop", "run": true,
                    "people": [{"personId": bea.to_string(), "telegram": true, "voice": false}]})
            .to_string()
        ));
        assert!(dline.receive(
            gd,
            r#"{"type":"device","uid":"uid-desk","name":"Desk","run":true}"#
        ));
        wait_until(|| lline.claim_for(bea) == Some((true, false))).await;
        wait_until(|| registry.lock().is_ok_and(|r| r.may_act_for(laptop, bea))).await;
        assert_eq!(dline.claim_for(bea), None, "Desk announced no people");
        // A PC that takes only the Telegram channel is not a candidate for voice at all.
        assert!(lines
            .candidate_names(bea, Channel::Telegram)
            .iter()
            .any(|name| name == "laptop"));
        assert!(!lines
            .candidate_names(bea, Channel::Voice)
            .iter()
            .any(|name| name == "laptop"));
        // The broadcast list shows each device's people, home person first.
        let laptop_row = 'find: loop {
            for list in wait_frames(&mut rx_l, "devices").await {
                let row = list["list"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|e| e["uid"] == "uid-laptop")
                    .cloned();
                if let Some(row) = row {
                    if row["people"].as_array().unwrap().len() == 2 {
                        break 'find row;
                    }
                }
            }
        };
        assert_eq!(
            laptop_row["people"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["person"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>(),
            ["Kim", "Bea"],
            "the home person first, then the announced one"
        );

        // An admin allowance survives the next announcement; device rows are replaced whole.
        registry
            .lock()
            .unwrap()
            .set_acts_for(laptop, "Ivy", true)
            .unwrap();
        assert!(lline.receive(
            gl,
            &json!({"type": "device", "uid": "uid-laptop", "name": "Laptop", "run": true,
                    "people": [{"personId": bea.to_string(), "telegram": true, "voice": true}]})
            .to_string()
        ));
        wait_until(|| lline.claim_for(bea) == Some((true, true))).await;
        {
            let r = registry.lock().unwrap();
            assert!(r.may_act_for(laptop, bea) && r.may_act_for(laptop, ivy));
            let mut listed = r.acts_for(laptop).unwrap();
            listed.sort();
            assert_eq!(
                listed,
                vec![
                    ("Bea".into(), "device".into()),
                    ("Ivy".into(), "admin".into())
                ]
            );
        }

        // A device call acting for Bea arrives on Desk stamped with the verified people; the
        // call runs until Desk answers, so it is driven from a task while the test answers.
        let lines_c = lines.clone();
        let call_task = tokio::spawn(async move {
            devices::route(
                &lines_c,
                laptop,
                json!("c1"),
                "uid-desk",
                "hello",
                json!({}),
                2,
                &[bea.to_string()],
            )
            .await
        });
        let call = wait_frames(&mut rx_d, "dev_call").await.remove(0);
        assert_eq!(
            (
                call["from"]["personId"].as_str(),
                call["from"]["person"].as_str(),
                call["from"]["machinePersonId"].as_str(),
                call["from"]["people"].as_array().unwrap().len(),
            ),
            (
                Some(bea.to_string().as_str()),
                Some("Bea"),
                Some(kim.to_string().as_str()),
                1
            )
        );
        assert!(dline.receive(
            gd,
            &json!({"type": "dev_result", "id": call["id"], "ok": true, "result": {"seen": true}})
                .to_string()
        ));
        let out = call_task.await.unwrap();
        assert_eq!(out["ok"], json!(true), "reply was {out}");
        // A person the sender does not act for refuses the whole call, nothing forwarded.
        let out = devices::route(
            &lines,
            laptop,
            json!("c2"),
            "uid-desk",
            "hello",
            json!({}),
            2,
            &["404".into()],
        )
        .await;
        assert_eq!(
            (out["ok"].as_bool(), out["error"]["code"].as_str()),
            (Some(false), Some("not_allowed"))
        );
        assert!(
            rx_d.try_recv().is_err(),
            "nothing was forwarded for the refused call"
        );
        // With no `as` the sender acts as its own person, as before.
        let lines_c = lines.clone();
        let call_task = tokio::spawn(async move {
            devices::route(
                &lines_c,
                laptop,
                json!("c3"),
                "uid-desk",
                "hello",
                json!({}),
                2,
                &[],
            )
            .await
        });
        let call = wait_frames(&mut rx_d, "dev_call").await.remove(0);
        assert_eq!(
            call["from"]["personId"].as_str(),
            Some(kim.to_string().as_str())
        );
        assert!(dline.receive(
            gd,
            &json!({"type": "dev_result", "id": call["id"], "ok": true}).to_string()
        ));
        let out = call_task.await.unwrap();
        assert_eq!(out["ok"], json!(true), "reply was {out}");

        // Upstream Telegram frames are tagged with a named person only when allowed.
        assert!(lline.receive(
            gl,
            &json!({"type": "telegram_final", "text": "as Bea", "personId": bea.to_string()})
                .to_string()
        ));
        assert_eq!(
            up.recv().await.unwrap(),
            (bea, laptop, Upstream::TelegramFinal("as Bea".into()))
        );
        assert!(lline.receive(
            gl,
            r#"{"type":"telegram_final","text":"as nobody","personId":"404"}"#
        ));
        assert!(
            up.try_recv().is_err(),
            "a frame naming a stranger was dropped"
        );
        assert!(lline.receive(gl, r#"{"type":"telegram_final","text":"plain"}"#));
        assert_eq!(
            up.recv().await.unwrap(),
            (kim, laptop, Upstream::TelegramFinal("plain".into()))
        );

        // Routing: home first when nothing else applies � Desk is Bea's own PC even though
        // Laptop attached later and acts for her too.
        let (tx_d2, _rx_d2) = mpsc::unbounded_channel();
        dline.attach(tx_d2);
        let pick = |person: i64, channel: Channel, want: &str| {
            let line = lines.route_for(person, channel);
            let got = match (&line, want) {
                (Some(l), "desk") => Arc::ptr_eq(l, &dline),
                (Some(l), "laptop") => Arc::ptr_eq(l, &lline),
                (None, "none") => true,
                _ => false,
            };
            assert!(got, "route_for({person}, {})", channel.label());
        };
        pick(bea, Channel::Telegram, "desk");
        pick(bea, Channel::Voice, "desk");
        pick(kim, Channel::Telegram, "laptop");
        pick(kim, Channel::App, "laptop");
        pick(404, Channel::Telegram, "none");

        // Sticky beats home: an acknowledgement on Laptop keeps the conversation there.
        assert!(lline.receive(
            gl,
            &json!({"type": "telegram_ack", "id": "m1", "personId": bea.to_string()}).to_string()
        ));
        assert_eq!(
            up.recv().await.unwrap(),
            (bea, laptop, Upstream::TelegramAck("m1".into()))
        );
        pick(bea, Channel::Telegram, "laptop");
        // A pin beats sticky; clearing it falls back to sticky.
        assert!(lines.pin(bea, Channel::Telegram, desk));
        pick(bea, Channel::Telegram, "desk");
        assert!(lines.clear_pin(bea, Channel::Telegram));
        pick(bea, Channel::Telegram, "laptop");
        // A dropped line falls through, and its channel claims died with it.
        lline.detach(gl);
        pick(bea, Channel::Telegram, "desk");
        pick(kim, Channel::Telegram, "none");

        // A profile with no person makes one, once; the same name in any case finds it again.
        let (tx_l2, mut rx_l2) = mpsc::unbounded_channel();
        let gl = lline.attach(tx_l2);
        assert!(lline.receive(
            gl,
            &json!({"type": "person_ensure", "id": "e1", "name": "wren"}).to_string()
        ));
        let made = wait_frames(&mut rx_l2, "person_result").await.remove(0);
        assert_eq!(
            (
                made["ok"].as_bool(),
                made["created"].as_bool(),
                made["person"].as_str()
            ),
            (Some(true), Some(true), Some("wren"))
        );
        assert!(lline.receive(
            gl,
            &json!({"type": "person_ensure", "id": "e2", "name": "WREN"}).to_string()
        ));
        let again = wait_frames(&mut rx_l2, "person_result").await.remove(0);
        assert_eq!(
            (
                again["ok"].as_bool(),
                again["created"].as_bool(),
                again["personId"].as_str()
            ),
            (Some(true), Some(false), made["personId"].as_str())
        );

        // A `route_pin` from a PC that acts for the person pins that PC; the re-announced
        // claims bring it back as a candidate and the pin decides.
        assert!(lline.receive(
            gl,
            &json!({"type": "route_pin", "personId": bea.to_string(), "channel": "telegram"})
                .to_string()
        ));
        assert!(lline.receive(
            gl,
            &json!({"type": "device", "uid": "uid-laptop", "name": "Laptop", "run": true,
                    "people": [{"personId": bea.to_string(), "telegram": true, "voice": true}]})
            .to_string()
        ));
        wait_until(|| lline.claim_for(bea) == Some((true, true))).await;
        wait_until(|| {
            registry.lock().is_ok_and(|r| {
                r.pinned_client(bea, "telegram")
                    .is_ok_and(|p| p == Some(laptop))
            })
        })
        .await;
        pick(bea, Channel::Telegram, "laptop");
        // A route to another person's PC delivers there: the frames really go down that line, and
        // a message stays with the line it went down even after the route moves again.
        let route = PersonRoute::new(lines.clone(), bea);
        assert!(route.send(&json!({"type": "telegram", "id": "m9", "text": "hi"})));
        assert_eq!(wait_frames(&mut rx_l2, "telegram").await[0]["id"], "m9");
        assert!(route.holds(gl));
        assert!(route.send_on(gl, &json!({"type": "telegram_answer", "id": "a1"})));
        assert_eq!(
            wait_frames(&mut rx_l2, "telegram_answer").await[0]["id"],
            "a1",
            "the answer went back to the laptop that carries Bea's Telegram"
        );
        lines.pin(bea, Channel::Telegram, desk);
        pick(bea, Channel::Telegram, "desk");
        assert!(
            route.holds(gl),
            "a line that still carries her Telegram holds"
        );
        assert!(route.send_on(gl, &json!({"type": "telegram", "id": "m10"})));
        assert_eq!(wait_frames(&mut rx_l2, "telegram").await[0]["id"], "m10");
        // No PC acts for a stranger, however it is named.
        assert!(lines.route_for(404, Channel::Telegram).is_none());
    }
}
