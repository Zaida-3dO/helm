//! The speaker line: a room speaker talks to a person's Helm through the hub.
//!
//! A Home Assistant integration on the speaker's network calls the hub over HTTP (the routes are
//! in `speaker_http.rs`); the hub puts each turn down the person's Helm relay line as a `voice`
//! frame carrying `"lane": "speaker"`, and brings the answer back to the speaker. The phone's
//! voice line (`voice.rs`) and this one never see each other's frames: the lanes have their own
//! upstream variants (`relay.rs`).
//!
//! This module is the hub's whole memory of those conversations; it does no HTTP of its own. It
//! calls the speech-to-text server, and it sends and receives relay frames. State is in memory
//! only: a restart drops sessions and turns. Everything time-dependent takes `now` (epoch
//! milliseconds), so a test drives time by hand; [`SpeakerBridge::tick`] runs under
//! [`SpeakerBridge::run`] every [`TICK`], and after every call that changes state.
//!
//! Per speaker token (one token per speaker):
//!
//! - **Turn queue.** A turn joins the token's queue and only the head of the queue goes down the
//!   line, one at a time, in upload order. A sent turn is sent again every [`RESEND_AFTER_MS`]
//!   until Helm answers with `voice_ack`; a turn still unfinished [`ABANDON_MS`] after its first
//!   send (or after it was made, if it never got sent) is finished off as an error. At most
//!   [`QUEUE_CAP`] live turns per token; past that a turn is refused. A finished turn is kept
//!   for [`DONE_KEEP_MS`] so the same turn id is answered again instead of run twice.
//! - **Finding the PC.** A turn goes down a line only while that PC's last `device` frame said
//!   it takes this person's speaker turns (the candidate gate in `lines.rs`). A person whose PCs
//!   are up but none of which claimed the speaker is answered with [`NEED_UPDATE`] after
//!   [`NO_CLAIM_WAIT_MS`] — a reconnecting PC regains its claims with its next `device` frame,
//!   so a short wait there is normal; a person with no line up at all waits [`NO_LINE_WAIT_MS`]
//!   and is answered as `offline`.
//! - **Sessions.** One session per (token, device), with an id, a start and a last-active time.
//!   A session ends [`idle`](SpeakerConfig::idle) after its last turn (a turn still running
//!   keeps it open), and every line that carried one of its turns is sent
//!   `speaker_session_closed {sessionId, closedAt}` so Helm can close and archive its speaker
//!   chats; `closedAt` is when the session ran out, not when the hub noticed. The next
//!   utterance on that speaker starts a new session with a fresh id.
//! - **Stamping.** Every turn frame carries the fields the speaker's side and Helm read. Who is
//!   speaking, how sure the hub is of them, and what permission an action needs are not decided
//!   here yet, so those fields carry their fixed starting values: assurance `none`, no person,
//!   tier `do`, no deadline, no hand-over. [`turn_frame`] is the one place that writes them, so
//!   the hub can start deciding them later without touching the queue.
//!
//! Turns and speech-to-text calls are rate-limited per token ([`RATE_PER_MIN`] of each in a
//! sliding minute). The phone lane's hub-wide recording budget does not apply here: a speaker
//! clip is short, and it is capped at [`MAX_STT_SECONDS`] before anything reads it.

use crate::lines::{Lines, PersonUpstream};
use crate::relay::{Channel, Line, Upstream};
use crate::stt::{retry_transcribing, Transcribe, STT_RETRY_AFTER};
use crate::telegram::now_ms;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// The speaker protocol version the hub answers with in `GET /speaker/status`.
pub const VERSION: u64 = 1;
/// How often the bridge looks for work that depends on time, or on a line coming up.
pub const TICK: Duration = Duration::from_millis(500);
/// A turn sent down the line and not acknowledged this long after is sent again.
pub const RESEND_AFTER_MS: i64 = 10_000;
/// A turn still live this long after its first send, or after it was made if it never got sent,
/// is finished off as an error.
pub const ABANDON_MS: i64 = 120_000;
/// Live turns one token may have at once.
pub const QUEUE_CAP: usize = 4;
/// Turns, and separately speech-to-text calls, one token may start in a sliding minute.
pub const RATE_PER_MIN: usize = 30;
/// How long a turn waits for a line that takes speaker turns when no line is up at all.
pub const NO_LINE_WAIT_MS: i64 = 30_000;
/// How long a turn waits for a line that takes speaker turns when the person's PCs are up but
/// none of them claimed the speaker.
pub const NO_CLAIM_WAIT_MS: i64 = 3_000;
/// A finished turn is remembered this long, so the same turn id is answered, not re-run.
pub const DONE_KEEP_MS: i64 = 600_000;
/// The most a `/speaker/stt` body may carry, and the longest clip it may hold.
pub const MAX_STT_BODY: usize = 1_572_864;
pub const MAX_STT_SECONDS: f64 = 30.0;
/// A clip shorter than this, or a transcript that comes back blank, is heard as nothing.
pub const MIN_AUDIO_SECONDS: f64 = 0.3;
/// The longest turn text kept.
pub const MAX_TEXT_CHARS: usize = 8_000;
/// A session idles out this long, by default, and the range the setting allows.
pub const DEFAULT_IDLE_MINUTES: u64 = 10;
pub const IDLE_MINUTES_RANGE: std::ops::RangeInclusive<u64> = 3..=30;

/// What the speaker is told when the person's Helm is up but has not said that it takes speaker
/// turns: only a Helm new enough to know this lane can answer by voice.
pub const NEED_UPDATE: &str = "Helm needs updating before I can answer by voice.";

/// The outcome of a `/speaker/stt` call.
#[derive(Debug, Clone, PartialEq)]
pub enum SttOutcome {
    Text(String),
    /// Nothing was heard: a very short or silent clip, or a blank transcript.
    Empty,
    /// The speech-to-text server did not answer.
    Failed,
    /// No speech-to-text server is configured on this hub.
    NotConfigured,
    /// The clip was longer than [`MAX_STT_SECONDS`]; refused before anything read it.
    TooLong,
    /// The token made too many calls in the last minute.
    RateLimited,
}

/// How a `/speaker/turn` call is answered, after any wait.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    /// The answer to speak, with the session it belongs to.
    Answer { text: String, session: Value },
    /// Helm has it and is working; the caller asks again later.
    Working,
    /// The person's Helm could not be reached in time.
    Offline,
    /// Something else went wrong; the speaker side says its own fixed sentence.
    Error,
    /// The queue is full, or the token made too many turns in the last minute.
    Rejected,
    /// The turn id belongs to another speaker token's turn.
    Conflict,
    /// No turn of this token has that id.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnState {
    Queued,
    /// Sent down the line, waiting for `voice_ack`.
    Sent,
    /// Helm took it (`voice_ack`) and is working on the answer.
    Working,
    /// The answer is in.
    Done,
    Offline,
    Error,
}

impl TurnState {
    fn live(self) -> bool {
        matches!(
            self,
            TurnState::Queued | TurnState::Sent | TurnState::Working
        )
    }
}

struct TurnRec {
    id: String,
    text: String,
    device: Option<String>,
    room: Option<String>,
    area: Option<String>,
    /// The id of the session this turn belongs to.
    session: String,
    at: i64,
    state: TurnState,
    /// When the state last changed, for the window a finished turn is kept in.
    changed: i64,
    answer: Option<String>,
    /// When the turn was first sent, for the abandon rule.
    first_sent: Option<i64>,
    /// Where the turn last went down (client, connection, when): a resend follows it, and an ack
    /// is only accepted while that connection still stands.
    went: Option<(i64, u64, i64)>,
    /// When this turn became the one waiting for a line.
    head_since: Option<i64>,
}

/// One talk on one speaker: its clock, and the lines that carried it.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub started: i64,
    pub last_active: i64,
    /// The clients whose line carried a turn of this session.
    pub carried: HashSet<i64>,
}

impl Session {
    fn stamp(&self) -> Value {
        json!({
            "id": self.id,
            "startedAt": self.started,
            "lastActiveAt": self.last_active,
            "assurance": "none",
            "person": Value::Null,
            "elevated": false,
            "elevatedUntil": Value::Null,
        })
    }
}

/// Everything the hub remembers about one speaker token.
#[derive(Default)]
struct TokenState {
    person: i64,
    turns: HashMap<String, TurnRec>,
    /// Turn ids in upload order.
    order: VecDeque<String>,
    /// Keyed by device; the empty string when the call names none.
    sessions: HashMap<String, Session>,
    /// Sliding-minute windows for the two rate limits.
    turn_times: VecDeque<i64>,
    stt_times: VecDeque<i64>,
}

/// What `GET /speaker/status` answers.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusOut {
    pub helm: &'static str,
    pub stt: bool,
    pub session: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerConfig {
    /// How long a session may go without a turn before it closes.
    pub idle: Duration,
}

impl Default for SpeakerConfig {
    fn default() -> Self {
        SpeakerConfig {
            idle: Duration::from_secs(DEFAULT_IDLE_MINUTES * 60),
        }
    }
}

impl SpeakerConfig {
    /// Reads `HUB_SPEAKER` (`on`, the default, or `off`) and `HUB_SPEAKER_IDLE_MINUTES`
    /// (3-30, default 10). `None`: the speaker line is off.
    pub fn from_env() -> Result<Option<SpeakerConfig>, String> {
        Self::from_vars(&|k: &str| std::env::var(k).ok())
    }

    pub fn from_vars(
        get: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<SpeakerConfig>, String> {
        let var = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        match var("HUB_SPEAKER").as_deref() {
            None | Some("on") => {}
            Some("off") => return Ok(None),
            Some(_) => return Err("HUB_SPEAKER must be on or off".into()),
        }
        let idle = match var("HUB_SPEAKER_IDLE_MINUTES") {
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| IDLE_MINUTES_RANGE.contains(n))
                .ok_or(format!(
                    "HUB_SPEAKER_IDLE_MINUTES must be {}-{}",
                    IDLE_MINUTES_RANGE.start(),
                    IDLE_MINUTES_RANGE.end()
                ))?,
            None => DEFAULT_IDLE_MINUTES,
        };
        Ok(Some(SpeakerConfig {
            idle: Duration::from_secs(idle * 60),
        }))
    }
}

/// What a new turn's call carries.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerTurn {
    pub id: String,
    pub text: String,
    pub device: Option<String>,
    pub room: Option<String>,
    pub area: Option<String>,
    /// Recorded in the registry only; routing never reads it.
    pub presence: Option<Value>,
}

/// The turn frame sent down the line: the phone's `voice` frame plus the speaker's own fields.
/// The identity fields are the hub's to decide later; for now they always start at their fixed
/// values, and this is the one place that writes them.
fn turn_frame(t: &TurnRec, person: i64) -> Value {
    json!({
        "type": "voice",
        "id": t.id,
        "personId": person.to_string(),
        "mode": "conversation",
        "text": t.text,
        "source": "speech",
        "sentAt": t.at,
        "ageMinutes": Value::Null,
        "lane": "speaker",
        "device": t.device,
        "room": t.room,
        "area": t.area,
        "session": t.session,
        "assurance": "none",
        "person": Value::Null,
        "tier": "do",
        "until": Value::Null,
    })
}

/// The client a line belongs to: a line's generations carry its client's id in their high half
/// (`Line::in_namespace`), and only an up line has a generation at all.
fn line_client(line: &Line) -> Option<i64> {
    line.generation().map(|g| (g >> 32) as i64)
}

/// The speaker line of a whole hub: one [`TokenState`] per speaker token.
pub struct SpeakerBridge {
    lines: Arc<Lines>,
    cfg: SpeakerConfig,
    stt: Option<Arc<dyn Transcribe>>,
    /// One speaker clip is transcribed at a time, on its own gate: the phone's gate must never
    /// sit in front of a spoken command.
    stt_gate: tokio::sync::Semaphore,
    /// By the registry id of the speaker token.
    states: Mutex<HashMap<i64, TokenState>>,
}

impl SpeakerBridge {
    pub fn new(lines: Arc<Lines>, cfg: SpeakerConfig) -> Self {
        SpeakerBridge {
            lines,
            cfg,
            stt: None,
            stt_gate: tokio::sync::Semaphore::new(1),
            states: Mutex::new(HashMap::new()),
        }
    }

    /// Spoken turns are transcribed by `stt`.
    pub fn with_stt(mut self, stt: Arc<dyn Transcribe>) -> Self {
        self.stt = Some(stt);
        self
    }

    pub fn has_stt(&self) -> bool {
        self.stt.is_some()
    }

    pub fn config(&self) -> &SpeakerConfig {
        &self.cfg
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<i64, TokenState>> {
        self.states.lock().unwrap_or_else(|s| s.into_inner())
    }

    /// Whether the sliding-minute window allows one more call of this kind; records it when it
    /// does.
    fn allow(window: &mut VecDeque<i64>, now: i64) -> bool {
        while window.front().is_some_and(|t| now - t >= 60_000) {
            window.pop_front();
        }
        if window.len() >= RATE_PER_MIN {
            return false;
        }
        window.push_back(now);
        true
    }

    /// `GET /speaker/status` for this token: whether a line takes its turns, whether speech can
    /// be heard at all, and the newest session it has (whatever device that was on). `person` is
    /// the person of the token that asked, so the answer is right before this speaker's first
    /// turn as well as after it.
    pub fn status(&self, token: i64, person: i64) -> StatusOut {
        let states = self.locked();
        let st = states.get(&token);
        let person = st.map(|s| s.person).filter(|p| *p != 0).unwrap_or(person);
        let helm = if self.lines.route_for(person, Channel::Speaker).is_some() {
            "online"
        } else {
            "offline"
        };
        let session = st
            .and_then(|s| s.sessions.values().max_by_key(|s| s.last_active))
            .map(|s| s.stamp());
        StatusOut {
            helm,
            stt: self.stt.is_some(),
            session,
        }
    }

    /// Transcribe a speaker clip. A short clip or a blank result becomes `empty` before anything
    /// else sees it; the server is asked one clip at a time, at interactive priority (the header
    /// is set on the transport, so a queued bulk job waits behind a spoken word).
    pub async fn stt_call(&self, token: i64, person: i64, audio: Vec<u8>) -> SttOutcome {
        {
            let mut states = self.locked();
            let st = states.entry(token).or_default();
            st.person = person;
            if !Self::allow(&mut st.stt_times, now_ms()) {
                return SttOutcome::RateLimited;
            }
        }
        if audio.is_empty() {
            return SttOutcome::Empty;
        }
        match crate::voice::audio_seconds(&audio) {
            Some(seconds) if seconds > MAX_STT_SECONDS => return SttOutcome::TooLong,
            Some(seconds) if seconds < MIN_AUDIO_SECONDS => return SttOutcome::Empty,
            _ => {}
        }
        let Some(stt) = self.stt.clone() else {
            return SttOutcome::NotConfigured;
        };
        let _permit = match self.stt_gate.acquire().await {
            Ok(p) => p,
            Err(_) => return SttOutcome::Failed,
        };
        let result = retry_transcribing(stt.endpoint(), STT_RETRY_AFTER, || {
            stt.transcribe(audio.clone(), "speaker.wav", "audio/wav")
        })
        .await;
        match result {
            Ok(text) => {
                let text = text.trim().to_owned();
                if text.is_empty() {
                    SttOutcome::Empty
                } else {
                    SttOutcome::Text(text.chars().take(MAX_TEXT_CHARS).collect())
                }
            }
            Err(e) => {
                // Never the audio or a transcript: only that the server failed, as on the phone's line.
                eprintln!("speaker: transcription failed: {}", e.why);
                SttOutcome::Failed
            }
        }
    }

    /// `POST /speaker/turn`: the turn joins the token's queue. Whatever wait the caller asked
    /// for is kept by `speaker_http`, which polls [`SpeakerBridge::outcome`].
    pub fn begin_turn(&self, token: i64, person: i64, t: SpeakerTurn, now: i64) -> TurnOutcome {
        if !crate::voice::valid_id(&t.id) {
            return TurnOutcome::Rejected;
        }
        let text = t.text.trim().to_owned();
        if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
            return TurnOutcome::Rejected;
        }
        let device = t.device.clone().unwrap_or_default();
        // A turn this token already has is answered from its record, never re-run.
        let known = self.outcome(token, &t.id);
        if !matches!(known, TurnOutcome::Unknown) {
            return known;
        }
        {
            let mut states = self.locked();
            // While a turn id lives at all it belongs to one token.
            if states
                .iter()
                .any(|(id, s)| *id != token && s.turns.contains_key(&t.id))
            {
                return TurnOutcome::Conflict;
            }
            let st = states.entry(token).or_default();
            st.person = person;
            if let Some(old) = st.turns.get(&t.id) {
                // The same id again from the same token is answered from its record, never re-run.
                return match old.state {
                    TurnState::Done => TurnOutcome::Answer {
                        text: old.answer.clone().unwrap_or_default(),
                        session: Value::Null,
                    },
                    TurnState::Offline => TurnOutcome::Offline,
                    TurnState::Error => TurnOutcome::Error,
                    _ => TurnOutcome::Working,
                };
            }
            if !Self::allow(&mut st.turn_times, now) {
                return TurnOutcome::Rejected;
            }
            if st.turns.values().filter(|r| r.state.live()).count() >= QUEUE_CAP {
                return TurnOutcome::Rejected;
            }
            // The session: open one when this speaker has none, and mark it talking.
            let session = st
                .sessions
                .entry(device.clone())
                .or_insert_with(|| Session {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    started: now,
                    last_active: now,
                    carried: HashSet::new(),
                });
            session.last_active = now;
            let session = session.id.clone();
            st.turns.insert(
                t.id.clone(),
                TurnRec {
                    id: t.id.clone(),
                    text,
                    device: t.device.clone(),
                    room: t.room.clone(),
                    area: t.area.clone(),
                    session,
                    at: now,
                    state: TurnState::Queued,
                    changed: now,
                    answer: None,
                    first_sent: None,
                    went: None,
                    head_since: None,
                },
            );
            st.order.push_back(t.id.clone());
        }
        // The speaker registry notes what this call named, when it named a speaker at all.
        if let Some(d) = t.device.as_deref().filter(|d| !d.is_empty()) {
            let registry = self.lines.registry();
            let registry = registry.lock().unwrap_or_else(|r| r.into_inner());
            let presence = t.presence.as_ref().map(|p| p.to_string());
            if let Err(e) = registry.upsert_speaker(
                token,
                d,
                t.room.as_deref(),
                t.area.as_deref(),
                presence.as_deref(),
                now,
            ) {
                eprintln!("speaker: the registry write failed: {e}");
            }
        }
        self.tick(now);
        self.outcome(token, &t.id)
    }

    /// Where a turn stands now, for the caller's wait and for `GET /speaker/turn/<id>`.
    pub fn outcome(&self, token: i64, id: &str) -> TurnOutcome {
        let states = self.locked();
        let Some(st) = states.get(&token) else {
            return TurnOutcome::Unknown;
        };
        let Some(t) = st.turns.get(id) else {
            return TurnOutcome::Unknown;
        };
        match t.state {
            TurnState::Queued | TurnState::Sent | TurnState::Working => TurnOutcome::Working,
            TurnState::Done => TurnOutcome::Answer {
                text: t.answer.clone().unwrap_or_default(),
                session: st
                    .sessions
                    .values()
                    .find(|s| s.id == t.session)
                    .map(|s| s.stamp())
                    .unwrap_or(Value::Null),
            },
            TurnState::Offline => TurnOutcome::Offline,
            TurnState::Error => TurnOutcome::Error,
        }
    }

    /// Helm's speaker-tagged frame: an ack, a working line (logged only), or the answer. A
    /// reply only counts when it is tagged with the person the turn was routed to.
    pub fn on_upstream(&self, person: i64, u: Upstream, now: i64) {
        let (id, text, done) = match &u {
            Upstream::SpeakerAck(id) => (id.clone(), None, false),
            Upstream::SpeakerStatus { turn, .. } => {
                eprintln!("speaker: working line for {turn}");
                return;
            }
            Upstream::SpeakerFinal(r) => match &r.turn {
                // Unprompted speech on the speaker lane has no route to a person yet: dropped.
                None => return,
                Some(turn) => (turn.clone(), Some(r.text.clone()), true),
            },
            _ => return,
        };
        let mut states = self.locked();
        // The turn's own token, whichever of its lines answered.
        let st = match states.values_mut().find(|s| s.turns.contains_key(&id)) {
            Some(st) if st.person == person => st,
            _ => return,
        };
        let t = match st.turns.get_mut(&id) {
            Some(t) => t,
            None => return,
        };
        if !t.state.live() {
            return;
        }
        if done {
            t.state = TurnState::Done;
            t.answer = Some(text.unwrap_or_default());
        } else if t.state == TurnState::Sent {
            t.state = TurnState::Working;
        }
        t.changed = now;
    }

    /// Everything that depends on time, or on a line coming up, for every token.
    pub fn tick(&self, now: i64) {
        let closed = {
            let mut states = self.locked();
            let mut closed: Vec<(String, i64, HashSet<i64>)> = Vec::new();
            for st in states.values_mut() {
                // Finished turns are forgotten once their idempotency window has passed.
                let gone: Vec<String> = st
                    .turns
                    .iter()
                    .filter(|(_, t)| !t.state.live() && now - t.changed >= DONE_KEEP_MS)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in gone {
                    st.turns.remove(&id);
                    st.order.retain(|o| *o != id);
                }
                // Sessions: an idle one closes, and the lines that carried it are told.
                let idle_ms = self.cfg.idle.as_millis() as i64;
                let idle_now: Vec<String> = st
                    .sessions
                    .iter()
                    .filter(|(_, s)| now - s.last_active >= idle_ms)
                    .filter(|(_, s)| {
                        // A turn still running keeps its session open.
                        !st.turns
                            .values()
                            .any(|t| t.session == s.id && t.state.live())
                    })
                    .map(|(device, _)| device.clone())
                    .collect();
                for device in idle_now {
                    if let Some(s) = st.sessions.remove(device.as_str()) {
                        closed.push((s.id, s.last_active + idle_ms, s.carried));
                    }
                }
                // The queue: the first live turn is the only one that moves; a turn already in
                // flight holds the rest.
                let head = match st
                    .order
                    .iter()
                    .find(|id| st.turns.get(*id).is_some_and(|t| t.state.live()))
                {
                    Some(id) => id.clone(),
                    None => continue,
                };
                let person = st.person;
                if let Some(t) = st.turns.get_mut(&head) {
                    if t.state == TurnState::Queued {
                        if t.head_since.is_none() {
                            t.head_since = Some(now);
                        }
                        let waited = now - t.head_since.unwrap_or(now);
                        let line = self
                            .lines
                            .route_for(person, Channel::Speaker)
                            .and_then(|line| line.generation().map(|g| (line, g)));
                        match line {
                            Some((line, gen)) => {
                                let client = line_client(&line).unwrap_or(0);
                                t.state = TurnState::Sent;
                                t.changed = now;
                                t.first_sent = Some(now);
                                t.went = Some((client, gen, now));
                                if let Some(s) =
                                    st.sessions.values_mut().find(|s| s.id == t.session)
                                {
                                    s.carried.insert(client);
                                }
                                line.send(&turn_frame(t, person));
                            }
                            None => {
                                // The person's PCs are up but none claimed the speaker: a short
                                // wait, then the one answer only a Helm new enough to know this
                                // lane can give. With no line at all there is nothing to ask.
                                let claimed = self.lines.best_up(person).is_some();
                                let limit = if claimed {
                                    NO_CLAIM_WAIT_MS
                                } else {
                                    NO_LINE_WAIT_MS
                                };
                                if waited >= limit {
                                    if claimed {
                                        t.state = TurnState::Done;
                                        t.answer = Some(NEED_UPDATE.to_owned());
                                    } else {
                                        t.state = TurnState::Offline;
                                    }
                                    t.changed = now;
                                }
                            }
                        }
                    }
                }
                // A turn waiting for its ack goes again on its schedule; one past its span ends.
                let resend = st
                    .turns
                    .iter()
                    .find(|(_, t)| {
                        t.state == TurnState::Sent
                            && t.went.is_some_and(|(_, _, at)| now - at >= RESEND_AFTER_MS)
                    })
                    .map(|(id, t)| {
                        let (client, gen, _) = t.went.unwrap();
                        (id.clone(), client, gen)
                    });
                if let Some((id, client, gen)) = resend {
                    let line = self
                        .lines
                        .get(client)
                        .filter(|line| line.generation() == Some(gen));
                    if let Some(line) = line {
                        if let Some(t) = st.turns.get(&id) {
                            line.send(&turn_frame(t, person));
                        }
                        if let Some(t) = st.turns.get_mut(&id) {
                            t.went = Some((client, gen, now));
                        }
                    } else if let Some(t) = st.turns.get_mut(&id) {
                        // The line went down with the turn unacknowledged: it waits in the queue
                        // for whichever line is up next. The abandon clock keeps running.
                        t.state = TurnState::Queued;
                        t.went = None;
                        t.changed = now;
                    }
                }
                for t in st.turns.values_mut() {
                    let overdue = t.state.live()
                        && match t.first_sent {
                            Some(at) => now - at >= ABANDON_MS,
                            None => now - t.at >= ABANDON_MS,
                        };
                    if overdue {
                        t.state = TurnState::Error;
                        t.changed = now;
                    }
                }
            }
            closed
        };
        // The close frames go after the states lock is gone: the bridge lock comes first, the
        // line and registry locks only ever after it, never the other way round.
        for (session_id, closed_at, carried) in closed {
            for client in carried {
                if let Some(line) = self.lines.get(client) {
                    line.send(&json!({
                        "type": "speaker_session_closed",
                        "sessionId": session_id,
                        "closedAt": closed_at,
                    }));
                }
            }
        }
    }

    /// Tick for as long as the hub runs, and handle what the lines send up.
    pub async fn run(self: Arc<Self>, mut upstream: UnboundedReceiver<PersonUpstream>) {
        let ticker = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(TICK);
            loop {
                iv.tick().await;
                ticker.tick(now_ms());
            }
        });
        while let Some((person, _client, u)) = upstream.recv().await {
            if u.is_speaker() {
                self.on_upstream(person, u, now_ms());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sliding-minute window, and the settings: one table each.
    /// One set of settings and what it means: the idle time, `off`, or a refused start.
    type Case<'a> = (&'a [(&'a str, &'a str)], Result<Option<Duration>, bool>);

    #[test]
    fn rate_window_and_config() {
        // Thirty in a minute, no more; the window slides, so the oldest drops out at a minute.
        let mut w = VecDeque::new();
        for i in 0..RATE_PER_MIN {
            assert!(SpeakerBridge::allow(&mut w, 1_000 + i as i64), "call {i}");
        }
        assert!(!SpeakerBridge::allow(&mut w, 1_000 + 40), "past thirty");
        assert!(
            SpeakerBridge::allow(&mut w, 1_000 + 61_000),
            "the first has aged out"
        );
        // A window kept across an hour-long gap empties completely.
        let mut w = VecDeque::new();
        for _ in 0..5 {
            assert!(SpeakerBridge::allow(&mut w, 1_000));
        }
        assert!(SpeakerBridge::allow(&mut w, 3_600_000) && w.len() == 1);

        let get = |vars: &[(&str, &str)]| {
            SpeakerConfig::from_vars(&|k| {
                vars.iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| (*v).to_owned())
            })
        };
        // (settings, what they mean: the idle, or `off`, or a refused start)
        let table: &[Case] = &[
            (&[], Ok(Some(Duration::from_secs(600)))),
            (&[("HUB_SPEAKER", "on")], Ok(Some(Duration::from_secs(600)))),
            (&[("HUB_SPEAKER", "off")], Ok(None)),
            (
                &[("HUB_SPEAKER_IDLE_MINUTES", "3")],
                Ok(Some(Duration::from_secs(180))),
            ),
            (
                &[("HUB_SPEAKER_IDLE_MINUTES", "30")],
                Ok(Some(Duration::from_secs(1800))),
            ),
            (
                &[("HUB_SPEAKER_IDLE_MINUTES", " 10 ")],
                Ok(Some(Duration::from_secs(600))),
            ),
            // Out of range, not a number, or not on/off: the hub says so and refuses to start.
            (&[("HUB_SPEAKER_IDLE_MINUTES", "2")], Err(true)),
            (&[("HUB_SPEAKER_IDLE_MINUTES", "31")], Err(true)),
            (&[("HUB_SPEAKER_IDLE_MINUTES", "soon")], Err(true)),
            (&[("HUB_SPEAKER", "maybe")], Err(true)),
        ];
        for (vars, want) in table {
            let got = get(vars);
            match want {
                Ok(idle) => assert_eq!(got.map(|c| c.map(|c| c.idle)), Ok(*idle), "{vars:?}"),
                Err(_) => assert!(got.is_err(), "{vars:?}"),
            }
        }
    }

    /// A turn frame always carries the starting identity values, in the one place that writes
    /// them, whatever the turn looks like.
    #[test]
    fn a_turn_frame_starts_at_its_fixed_values() {
        let t = TurnRec {
            id: "t-1".into(),
            text: "Hello".into(),
            device: Some("desk".into()),
            room: None,
            area: Some("Study".into()),
            session: "s-1".into(),
            at: 10,
            state: TurnState::Queued,
            changed: 10,
            answer: None,
            first_sent: None,
            went: None,
            head_since: None,
        };
        let f = turn_frame(&t, 7);
        assert_eq!(f["type"], json!("voice"));
        assert_eq!(f["lane"], json!("speaker"));
        assert_eq!(f["personId"], json!("7"));
        assert_eq!(f["person"], Value::Null);
        assert_eq!(f["assurance"], json!("none"));
        assert_eq!(f["tier"], json!("do"));
        assert_eq!(f["until"], Value::Null);
        assert_eq!(f["session"], json!("s-1"));
        assert_eq!(f["room"], Value::Null);
        assert_eq!(f["area"], json!("Study"));
    }
}
