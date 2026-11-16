//! The stop judge: it looks at a chat's clean turn end while the working mode is run to
//! completion, and decides whether the chat should carry on by itself.
//!
//! The chat runtime offers each turn end it may hand over ([`StopJudge::offer`], on the chat's
//! actor, answered at once). Most never get further: the pure table in [`crate::stoproute`]
//! passes check-in chats, local-model chats, side chats, live phone conversations, messages that
//! do not look like a stop and chats with a declared wait. The rest are held, and the judging
//! runs on a thread of its own:
//!
//! 1. a chat that already needs the user (a pending permission, a flag, a card) is simply done;
//! 2. chats it started that are still working get a nudge to wait for them properly;
//! 3. otherwise the short-lived helper reads the end of the conversation and classifies it, and
//!    the host decides ([`crate::stoproute::route`]).
//!
//! The thread ends with [`Host::verdict`], which the actor applies only while the hold is still
//! its own: a verdict that arrives after the user wrote, the deadline passed or the chat closed
//! finds nothing, and then nothing else happens — no nudge, no flag. A nudge is a message from
//! Helm (never the user's own words), sent only after its verdict was applied.
//!
//! Every held turn end leaves one `gate_asks` row (channel `stop`): a nudge is a decision made
//! for the user, a stop is an escalation with its class, a finished chat is a pass. A stop is
//! also flagged, so it reaches Needs attention, the badge and the away alerts.

use crate::route::{self, Slot};
use crate::scrub;
use crate::service::Gate;
use crate::settings::GateSettings;
use crate::stoproute::{self, Answer, Facts, Nudge, Pre, Route, Skip, Usage};
use crate::store::{self, StopRecord};
use crate::workmode::{self, Mode};
use helm_chats::runtime::{ChatRuntime, GATEKEEPER_TIMEOUT};
use helm_chats::seams::{StopJudge, StopKind, StopOffer, StopOutcome, TurnEnd};
use helm_chats::triage::recent_turns;
use helm_chats::types::SubmissionOrigin;
use helm_core::db::Database;
use helm_ipc::{AppEvent, EventSink};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// How much of the last message the helper reads.
const LAST_CHARS: usize = 6_000;

/// Plan usage, as a percentage the host trusts, if there is one.
pub trait UsageProbe: Send + Sync {
    fn used_pct(&self, now: i64) -> Option<f64>;
}

/// The app's reading: the usage meter's own service.
pub struct UsageMeter(pub Arc<helm_usage::UsageService>);

impl UsageProbe for UsageMeter {
    fn used_pct(&self, now: i64) -> Option<f64> {
        helm_governor::bands::used_pct(&self.0.snapshot(now), now)
    }
}

/// What the judge needs from the chat runtime. The app's version goes through [`ChatRuntime`];
/// tests stand in for it.
pub trait Host: Send + Sync {
    /// Ask the short-lived helper.
    fn run_helper(&self, job: &Value) -> Result<Value, String>;
    /// Answer for a held turn end; false when the hold was no longer there.
    fn verdict(&self, chat_id: &str, run_id: &str, outcome: StopOutcome) -> bool;
    /// Send the chat a message from Helm.
    fn send(&self, chat_id: &str, submission_id: &str, text: &str) -> Result<(), String>;
    /// Whether the chat is waiting on the user for something it asked.
    fn waiting_on_user(&self, chat_id: &str) -> bool;
    /// Something that changes what needs the user happened.
    fn changed(&self);
    fn shutting_down(&self) -> bool;
    /// How long a judgement may wait for its turn and still leave the helper its time.
    fn wait_for_slot(&self) -> Duration;
    /// How long the helper may run, when a test shortens the hold.
    fn helper_ms(&self) -> Option<u64>;
}

impl Host for ChatRuntime {
    fn run_helper(&self, job: &Value) -> Result<Value, String> {
        self.spawner().run_gatekeeper(job)
    }

    fn verdict(&self, chat_id: &str, run_id: &str, outcome: StopOutcome) -> bool {
        self.stop_verdict(chat_id, run_id, outcome)
    }

    fn send(&self, chat_id: &str, submission_id: &str, text: &str) -> Result<(), String> {
        helm_chats::submit::host_send(
            self,
            helm_chats::submit::HostSend {
                chat_id,
                submission_id,
                text,
                // Helm's own note, never the user's words: it cannot count as consent.
                origin: SubmissionOrigin::App,
            },
        )
        .map(|_| ())
        .map_err(|e| e.message)
    }

    fn waiting_on_user(&self, chat_id: &str) -> bool {
        self.attention().is_ok_and(|s| {
            s.needs_you.iter().any(|r| r.chat_id == chat_id)
                || s.muted.iter().any(|r| r.chat_id == chat_id)
        })
    }

    fn changed(&self) {
        self.publish_attention();
    }

    fn shutting_down(&self) -> bool {
        self.is_shutting_down()
    }

    fn wait_for_slot(&self) -> Duration {
        self.limits()
            .stop_deadline
            .saturating_sub(GATEKEEPER_TIMEOUT)
    }

    fn helper_ms(&self) -> Option<u64> {
        let hold = self.limits().stop_deadline;
        (hold < helm_chats::runtime::STOP_HOLD)
            .then(|| hold.saturating_sub(Duration::from_secs(1)).as_millis() as u64)
    }
}

/// The stop judge.
pub struct StopGate {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    host: Arc<dyn Host>,
    gate: Arc<Gate>,
    usage: Option<Arc<dyn UsageProbe>>,
}

/// A held turn end on its way to the judging thread.
pub(crate) struct Held {
    chat_id: String,
    run_id: String,
    ask_id: String,
    slot: Slot,
    settings: GateSettings,
    started: i64,
}

enum Offered {
    Pass,
    Hold(Box<Held>),
}

/// What the host read of a turn end before deciding to hold it.
struct Seen {
    profile: String,
    settings: GateSettings,
    title: String,
    skip: Option<Skip>,
}

impl StopGate {
    pub fn new(
        db: Arc<Database>,
        events: Arc<dyn EventSink>,
        rt: ChatRuntime,
        usage: Option<Arc<dyn UsageProbe>>,
    ) -> Arc<Self> {
        Self::build(db, events, Arc::new(rt), usage)
    }

    pub(crate) fn build(
        db: Arc<Database>,
        events: Arc<dyn EventSink>,
        host: Arc<dyn Host>,
        usage: Option<Arc<dyn UsageProbe>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            events,
            host,
            gate: Arc::new(Gate::with_caps(stoproute::STOP_CAPS)),
            usage,
        })
    }

    /// Everything the table before any model needs, read at once. `None` on any read error:
    /// the turn end finishes as it always did.
    fn see(&self, chat_id: &str) -> Option<Seen> {
        self.db
            .snapshot(|tx| -> rusqlite::Result<Option<Seen>> {
                let run_mode = workmode::effective(tx, chat_id).mode == Mode::Run;
                let profile = store::chat_profile(tx, chat_id);
                let settings = store::load_settings(tx, &profile)?;
                let Some((title, local, side)) = tx
                    .query_row(
                        "SELECT title, provider IS NOT NULL, side_source_id IS NOT NULL
                         FROM chats WHERE id = ?1",
                        [chat_id],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, bool>(1)?,
                                r.get::<_, bool>(2)?,
                            ))
                        },
                    )
                    .optional()?
                else {
                    return Ok(None);
                };
                // A chat in check-in mode is never looked at further: stop reading here.
                if !run_mode {
                    return Ok(Some(Seen {
                        profile,
                        settings,
                        title,
                        skip: Some(Skip::CheckIn),
                    }));
                }
                let last = last_message(tx, chat_id)?;
                let pre = Pre {
                    run_mode,
                    local_chat: local,
                    side_chat: side,
                    live_conversation: live_conversation(tx, chat_id)?,
                    last: &last,
                    open_wait: !helm_waits::store::open(tx, Some(chat_id))?.is_empty(),
                };
                Ok(Some(Seen {
                    profile,
                    settings,
                    title,
                    skip: stoproute::pre_model(&pre),
                }))
            })
            .ok()
            .flatten()
    }

    /// Decide whether to hold a turn end, and open its record if so. Everything but starting the
    /// judging thread.
    fn begin(&self, chat_id: &str, end: &TurnEnd) -> Offered {
        let Some(seen) = self.see(chat_id) else {
            return Offered::Pass;
        };
        if seen.skip.is_some() {
            return Offered::Pass;
        }
        let now = helm_core::now_ms();
        let ask_id = uuid::Uuid::new_v4().to_string();
        let slot = match self.gate.lock().admit(now) {
            Ok(s) => s,
            Err(why) => {
                let reason = match why {
                    route::PassReason::Paused => "paused",
                    _ => "limit",
                };
                let _ = self.db.write(|tx| {
                    store::pass_stop(
                        tx,
                        &ask_id,
                        &seen.profile,
                        chat_id,
                        &seen.title,
                        &end.run_id,
                        reason,
                        now,
                    )
                });
                return Offered::Pass;
            }
        };
        let opened = self.db.write(|tx| {
            store::open_stop_ask(
                tx,
                &ask_id,
                &seen.profile,
                chat_id,
                &seen.title,
                &end.run_id,
                now,
            )
        });
        if !matches!(opened, Ok(true)) {
            // The record could not be opened, or this run already has one.
            self.gate.release(slot);
            return Offered::Pass;
        }
        Offered::Hold(Box::new(Held {
            chat_id: chat_id.to_owned(),
            run_id: end.run_id.clone(),
            ask_id,
            slot,
            settings: seen.settings,
            started: now,
        }))
    }

    /// Judge one held turn end, and answer for it. Runs on a thread of its own.
    pub(crate) fn finish(&self, held: Held) {
        let mut slot = held.slot;
        let decided = self.decide(&held, &mut slot);
        // The helper is done: the next waiting judgement may start while this one settles.
        self.gate.release(slot);
        self.apply(&held, decided);
    }

    /// The judge's say, without applying it.
    fn decide(&self, h: &Held, slot: &mut Slot) -> Decided {
        let now = helm_core::now_ms();
        let failed = Decided::failed;
        if self.host.shutting_down() {
            return failed();
        }
        if self.host.waiting_on_user(&h.chat_id) {
            // What it waits on is already in front of the user.
            return Decided {
                call: Call::Done,
                pass_reason: Some("needs_you"),
                ..failed()
            };
        }
        let Ok(read) = self.read_facts(&h.chat_id, h.settings.bar(), now) else {
            return failed();
        };
        let facts = read.facts();
        if read.children_busy > 0 {
            let call = match stoproute::bound(
                Nudge::Children {
                    count: read.children_busy,
                },
                &facts,
            ) {
                Route::Continue(n) => Call::Continue(n),
                Route::Stop { kind, line } => Call::Stop {
                    kind,
                    line,
                    question: String::new(),
                },
                Route::Done => Call::Done,
            };
            return Decided {
                call,
                nudges: read.nudges,
                by: "rule",
                ..failed()
            };
        }
        // The helper runs only when a slot is free, within the time the hold leaves it.
        if !self.gate.wait_to_run(*slot, self.host.wait_for_slot()) {
            log::info!("the stop judge was busy too long for chat {}", h.chat_id);
            return Decided {
                nudges: read.nudges,
                ..failed()
            };
        }
        *slot = Slot::Running;
        let mut job = json!({
            "kind": "stop",
            "title": read.title,
            "turns": read.turns,
            "last": read.last_tail,
            "facts": {
                "usedPct": read.used_pct,
                "band": read.band,
                "localWorking": read.local_working,
                "nudges": read.nudges,
                "lastNudgeQuestion": read.last_nudge_question,
                "childrenBusy": read.children_busy,
            },
            "policy": h.settings.policy_text(),
            "bar": h.settings.bar(),
            "model": h.settings.model,
        });
        if let Some(ms) = self.host.helper_ms() {
            job["timeoutMs"] = json!(ms);
        }
        let answer = match self.host.run_helper(&job) {
            Ok(v) => {
                let a = stoproute::parse_answer(&v);
                if a.is_none() {
                    let said = v
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("not a verdict");
                    log::warn!(
                        "the stop helper gave no verdict for chat {}: {said}",
                        h.chat_id
                    );
                }
                a
            }
            Err(e) => {
                log::warn!("the stop helper did not answer for chat {}: {e}", h.chat_id);
                None
            }
        };
        let Some(answer) = answer else {
            if !self.host.shutting_down() && self.gate.lock().note_failure(helm_core::now_ms()) {
                log::warn!("the stop judge paused itself after repeated failures");
            }
            return Decided {
                nudges: read.nudges,
                ..failed()
            };
        };
        let call = match stoproute::route(&answer, &facts) {
            Route::Continue(n) => Call::Continue(n),
            Route::Stop { kind, line } => Call::Stop {
                kind,
                line,
                question: answer.question.clone(),
            },
            Route::Done => Call::Done,
        };
        Decided {
            call,
            answer: Some(answer),
            nudges: read.nudges,
            by: "claude",
            ..failed()
        }
    }

    fn read_facts(&self, chat_id: &str, bar: f64, now: i64) -> rusqlite::Result<Read> {
        let (nudges, last_nudge_question) = self.db.write(|tx| store::chain(tx, chat_id, now))?;
        let used = self.usage.as_ref().and_then(|u| u.used_pct(now));
        self.db.snapshot(|tx| {
            let title: String =
                tx.query_row("SELECT title FROM chats WHERE id = ?1", [chat_id], |r| {
                    r.get(0)
                })?;
            let gov = helm_governor::store::load(tx).unwrap_or_default();
            // The governor's own lines while it is on; its defaults while it is off.
            let lines = if gov.enabled {
                gov
            } else {
                helm_governor::GovernorSettings::default()
            };
            let band = helm_governor::bands::band_of(used, &lines);
            let usage = match (used, band) {
                (Some(p), helm_governor::Band::WindDown | helm_governor::Band::Stop) => {
                    Usage::Near(p)
                }
                (Some(p), helm_governor::Band::Unknown) => {
                    let _ = p;
                    Usage::Unknown
                }
                (Some(p), _) => Usage::Below(p),
                (None, _) => Usage::Unknown,
            };
            let last = last_message(tx, chat_id)?;
            Ok(Read {
                title,
                turns: recent_turns(tx, chat_id)?,
                last_tail: stoproute::tail(&last, LAST_CHARS),
                last,
                used_pct: used,
                band: band.as_str(),
                usage,
                local_working: local_working(tx)?,
                children_busy: children_busy(tx, chat_id)?,
                nudges,
                last_nudge_question,
                bar,
            })
        })
    }

    /// Apply the judge's say through the chat runtime, then write the record once.
    fn apply(&self, h: &Held, d: Decided) {
        let now = helm_core::now_ms();
        let chat = h.chat_id.as_str();
        let scrubbed =
            |s: &str, max: usize| -> String { scrub::scrub(s).chars().take(max).collect() };
        let (reason, confidence) = match &d.answer {
            Some(a) => (a.reason.clone(), Some(a.confidence)),
            None => Default::default(),
        };
        let mut rec = Rec::default();
        let applied = match d.call {
            Call::Continue(nudge) => {
                let applied = self.host.verdict(chat, &h.run_id, StopOutcome::Continue);
                rec.verdict = "decided";
                rec.question = scrubbed(&nudge.question(), 400);
                rec.choice = Some(scrubbed(&nudge.default(), 400));
                rec.reason = Some(scrubbed(&reason, 240)).filter(|r| !r.is_empty());
                rec.confidence = confidence;
                if applied {
                    // Count the nudge before it goes out, so a crash never loses the count.
                    let counted = self
                        .db
                        .write(|tx| store::add_nudge(tx, chat, &nudge.question(), now));
                    if let Err(e) = counted {
                        log::warn!("the stop judge could not count a nudge for chat {chat}: {e}");
                    }
                    // The record is written before the message goes out.
                    rec.state = "open";
                    self.write_record(h, d.by, &rec, now);
                    if let Err(e) = self
                        .host
                        .send(chat, &h.ask_id, &stoproute::nudge_text(&nudge))
                    {
                        // The chat is quiet and nobody was told it finished: put it in front of
                        // the user.
                        log::warn!("the stop judge could not nudge chat {chat}: {e}");
                        self.flag(
                            chat,
                            "Helm could not tell this chat to carry on",
                            "Failure: the automatic nudge did not reach the chat.",
                            "normal",
                            now,
                        );
                    }
                    return;
                }
                false
            }
            Call::Stop {
                kind,
                line,
                question: asked,
            } => {
                let line = scrubbed(&line, 300);
                let shown = match d.nudges {
                    0 => line.clone(),
                    1 => format!("{line} \u{b7} 1 choice was made for you on the way"),
                    n => format!("{line} \u{b7} {n} choices were made for you on the way"),
                };
                let applied =
                    self.host
                        .verdict(chat, &h.run_id, StopOutcome::Stop { kind, line: shown });
                rec.verdict = "escalated";
                rec.stop_class = Some(class_word(kind));
                rec.question = scrubbed(&asked, 400);
                rec.reason = Some(line.clone());
                rec.confidence = confidence;
                if applied {
                    let _ = self.db.write(|tx| store::end_chain(tx, chat));
                    self.flag(
                        chat,
                        &Some(scrubbed(&asked, 300))
                            .filter(|q| !q.is_empty())
                            .unwrap_or_else(|| "Helm stopped this chat".into()),
                        &format!("{}: {line}", flag_word(kind)),
                        if kind == StopKind::Blocker {
                            "high"
                        } else {
                            "normal"
                        },
                        now,
                    );
                }
                applied
            }
            Call::Done => {
                let line = (d.nudges > 0).then(|| {
                    format!(
                        "Run finished: {} {} made for you on the way.",
                        d.nudges,
                        if d.nudges == 1 {
                            "choice was"
                        } else {
                            "choices were"
                        }
                    )
                });
                let applied = self
                    .host
                    .verdict(chat, &h.run_id, StopOutcome::Done { line });
                rec.verdict = "passed";
                rec.pass_reason = Some(d.pass_reason.unwrap_or("done"));
                rec.reason = Some(scrubbed(&reason, 240)).filter(|r| !r.is_empty());
                rec.confidence = confidence;
                if applied {
                    let _ = self.db.write(|tx| store::end_chain(tx, chat));
                }
                applied
            }
            Call::Failed => {
                let applied = self.host.verdict(chat, &h.run_id, StopOutcome::Failed);
                rec.verdict = "failed";
                rec.reason = Some("The check did not finish.".into());
                applied
            }
        };
        // A verdict that finds no hold never acted: the user wrote first, the deadline had
        // passed or the chat had closed. The record says so, so History never lists a choice
        // Helm did not make as one it did.
        rec.state = if applied { "open" } else { "withdrawn" };
        self.write_record(h, d.by, &rec, now);
    }

    fn write_record(&self, h: &Held, by: &str, rec: &Rec, now: i64) {
        let model = (by == "claude").then(|| h.settings.model.clone());
        let wrote = self.db.write(|tx| {
            store::finish_stop_ask(
                tx,
                &h.ask_id,
                &StopRecord {
                    verdict: rec.verdict,
                    pass_reason: rec.pass_reason,
                    decided_by: by,
                    model: model.as_deref(),
                    confidence: rec.confidence,
                    question: &rec.question,
                    reason: rec.reason.as_deref(),
                    choice: rec.choice.as_deref(),
                    stop_class: rec.stop_class,
                    took_ms: now.max(h.started) - h.started,
                    state: rec.state,
                },
            )
        });
        if matches!(wrote, Ok(true)) {
            self.events.emit(
                AppEvent::new("gate")
                    .with("askId", h.ask_id.clone())
                    .with("chatId", h.chat_id.clone())
                    .with("what", rec.verdict.to_owned()),
            );
        }
    }

    /// Put a stop in front of the user as the chat's own flag, so it reaches Needs attention,
    /// the badge and the away alerts (a muted chat's goes to the Muted group).
    fn flag(&self, chat_id: &str, question: &str, reason: &str, priority: &str, now: i64) {
        let filed = self
            .db
            .write(|tx| helm_chats::flags::file(tx, chat_id, question, reason, priority, now));
        if matches!(filed, Ok(Some(f)) if f != helm_chats::flags::Filed::Duplicate) {
            self.host.changed();
        }
    }
}

impl StopJudge for StopGate {
    fn offer(&self, chat_id: &str, end: &TurnEnd) -> StopOffer {
        match self.begin(chat_id, end) {
            Offered::Pass => StopOffer::Pass,
            Offered::Hold(held) => {
                let this = self.clone_handle();
                std::thread::spawn(move || this.finish(*held));
                StopOffer::Hold
            }
        }
    }

    fn turn_line(&self, chat_id: &str) -> Option<String> {
        workmode::turn_line(&self.db, chat_id)
    }
}

impl StopGate {
    fn clone_handle(&self) -> StopGate {
        StopGate {
            db: self.db.clone(),
            events: self.events.clone(),
            host: self.host.clone(),
            gate: self.gate.clone(),
            usage: self.usage.clone(),
        }
    }
}

/// What the judge made of a held turn end.
struct Decided {
    call: Call,
    answer: Option<Answer>,
    pass_reason: Option<&'static str>,
    /// `claude` after a model's answer, `rule` for the host's own table.
    by: &'static str,
    nudges: i64,
}

impl Decided {
    fn failed() -> Self {
        Self {
            call: Call::Failed,
            answer: None,
            pass_reason: None,
            by: "rule",
            nudges: 0,
        }
    }
}

enum Call {
    Continue(Nudge),
    Stop {
        kind: StopKind,
        line: String,
        question: String,
    },
    Done,
    Failed,
}

/// What the record will say.
#[derive(Default)]
struct Rec {
    verdict: &'static str,
    pass_reason: Option<&'static str>,
    question: String,
    reason: Option<String>,
    choice: Option<String>,
    confidence: Option<f64>,
    stop_class: Option<&'static str>,
    state: &'static str,
}

/// What the host read for the judging itself.
struct Read {
    title: String,
    turns: Vec<Value>,
    last: String,
    last_tail: String,
    used_pct: Option<f64>,
    band: &'static str,
    usage: Usage,
    local_working: i64,
    children_busy: i64,
    nudges: i64,
    last_nudge_question: String,
    bar: f64,
}

impl Read {
    fn facts(&self) -> Facts<'_> {
        Facts {
            bar: self.bar,
            usage: self.usage,
            local_working: self.local_working,
            nudges: self.nudges,
            last_nudge_question: &self.last_nudge_question,
            last: &self.last,
        }
    }
}

fn class_word(kind: StopKind) -> &'static str {
    match kind {
        StopKind::Blocker => "blocker",
        StopKind::Limit => "limit",
        StopKind::Failure => "failure",
        StopKind::Asked => "asked",
    }
}

fn flag_word(kind: StopKind) -> &'static str {
    match kind {
        StopKind::Blocker => "Blocker",
        StopKind::Limit => "Limit",
        StopKind::Failure => "Failure",
        StopKind::Asked => "Stopped to ask",
    }
}

/// The chat's last assistant message ("" when it has none).
fn last_message(tx: &rusqlite::Connection, chat_id: &str) -> rusqlite::Result<String> {
    Ok(tx
        .query_row(
            "SELECT text FROM messages
             WHERE chat_id = ?1 AND role = 'assistant' AND active = 1 AND text <> ''
             ORDER BY timestamp DESC, id DESC LIMIT 1",
            [chat_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

/// Whether the user's latest message in the chat came from the phone (Telegram or voice): a
/// live conversation, where the question is the reply.
fn live_conversation(tx: &rusqlite::Connection, chat_id: &str) -> rusqlite::Result<bool> {
    let origin: Option<String> = tx
        .query_row(
            "SELECT s.origin FROM messages m
             JOIN submission_meta s ON m.source_key = 'u:' || s.submission_id
             WHERE m.chat_id = ?1 AND m.role = 'user' AND m.active = 1
               AND s.origin IN ('typed', 'answer', 'remote', 'telegram', 'voice', 'action', 'notification')
             ORDER BY m.timestamp DESC, m.id DESC LIMIT 1",
            [chat_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(matches!(origin.as_deref(), Some("telegram" | "voice")))
}

/// Local-model chats working, or with a message waiting to start. Read from the database only:
/// no server is asked.
fn local_working(tx: &rusqlite::Connection) -> rusqlite::Result<i64> {
    tx.query_row(
        "SELECT COUNT(*) FROM chats c
         WHERE c.provider IS NOT NULL AND c.archived = 0
           AND (c.status IN ('starting', 'running', 'stopping')
                OR EXISTS(SELECT 1 FROM submissions s
                          WHERE s.chat_id = c.id AND s.state IN ('saved', 'dispatching', 'accepted')))",
        [],
        |r| r.get(0),
    )
}

/// Chats this one started that are working now.
fn children_busy(tx: &rusqlite::Connection, chat_id: &str) -> rusqlite::Result<i64> {
    tx.query_row(
        "SELECT COUNT(*) FROM chats
         WHERE created_by_chat = ?1 AND archived = 0 AND status IN ('starting', 'running', 'stopping')",
        params![chat_id],
        |r| r.get(0),
    )
}

#[cfg(test)]
#[path = "stops_tests.rs"]
mod tests;
