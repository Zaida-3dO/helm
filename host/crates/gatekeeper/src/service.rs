//! The gate itself: it holds each question card, judges it through the short-lived helper,
//! and answers the card through the chat runtime — or hands it back to the user.
//!
//! The actor calls [`QuestionGate::offer`] and gets its answer at once; the judging runs on a
//! thread of its own and finishes with [`ChatRuntime::gate_verdict`], which the card's actor
//! applies only while the card is still held. A verdict that arrives after the user answered
//! finds no card and is recorded as answered by the user.
//!
//! What the helper (`helpers/gatekeeper`, run through the runtime's own spawner) answers is
//! data. The host then decides: the route table forces escalation for anything irreversible,
//! high-risk, low-confidence or hard to undo, the brief is scrubbed, and the answer is only
//! ever one of the card's own offered choices.

use crate::route::{self, CardFacts, Decision, Limiter, ModelAnswer, PassReason, Risk, Slot};
use crate::scrub;
use crate::settings::GateSettings;
use crate::store::{self, GateAsk};
use helm_chats::runtime::ChatRuntime;
use helm_chats::seams::{GateOffer, GateOutcome, QuestionGate};
use helm_chats::triage::recent_turns;
use helm_chats::types::{GateBrief, Interaction, InteractionAttention, SubmissionOrigin};
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, EventSink};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub struct GateService {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    rt: ChatRuntime,
    gate: Arc<Mutex<Limiter>>,
}

impl GateService {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>, rt: ChatRuntime) -> Arc<Self> {
        Arc::new(Self {
            db,
            events,
            rt,
            gate: Arc::new(Mutex::new(Limiter::default())),
        })
    }

    fn admit(&self, now: i64) -> Result<Slot, PassReason> {
        self.gate
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .admit(now)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Limiter> {
        self.gate.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn paused_until(&self, now: i64) -> Option<i64> {
        self.lock().paused_until(now)
    }

    /// History and the review list: `all`, `decided`, `asked`, `failed` or `review`.
    pub fn list(
        &self,
        filter: &str,
        query: &str,
        limit: i64,
        now: i64,
    ) -> Result<Vec<GateAsk>, ApiError> {
        self.db
            .snapshot(|tx| store::list(tx, filter, query, limit, now))
            .map_err(|e| ApiError::internal(e.to_string()))
    }

    pub fn get(&self, id: &str) -> Result<Option<GateAsk>, ApiError> {
        self.db
            .snapshot(|tx| store::ask(tx, id))
            .map_err(|e| ApiError::internal(e.to_string()))
    }

    /// Mark one row: "fine" or "should have asked me", with an optional note of at most 300
    /// characters. The first mark wins; re-marking answers with the row as it stands.
    pub fn review(&self, id: &str, review: &str, note: Option<&str>) -> Result<GateAsk, ApiError> {
        let note = note.map(str::trim).filter(|s| !s.is_empty());
        if let Some(n) = note {
            if n.chars().count() > 300 {
                return Err(ApiError::bad_args("The note is at most 300 characters."));
            }
        }
        if !matches!(review, "fine" | "should_have_asked" | "didnt_need_me") {
            return Err(ApiError::bad_args(format!(
                "\"{review}\" is not a mark the gatekeeper keeps."
            )));
        }
        let row = self
            .db
            .write(|tx| store::review(tx, id, review, note))
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::new("not_found", "No gatekeeper record has that id."))?;
        self.emit(&row, "review");
        Ok(row)
    }

    /// The user changed the answer of a decided row: the chat is told to undo what depended
    /// on the old one (an archived chat is brought back first, so the message lands), and the
    /// record leaves the review list. Helm never reverts anything itself.
    pub fn change_answer(&self, id: &str, new_answer: &str) -> Result<GateAsk, ApiError> {
        let new_answer = new_answer.trim();
        if new_answer.is_empty() {
            return Err(ApiError::bad_args("The new answer cannot be empty."));
        }
        if new_answer.chars().count() > 1_000 {
            return Err(ApiError::bad_args(
                "The new answer is at most 1000 characters.",
            ));
        }
        let row = self
            .get(id)?
            .ok_or_else(|| ApiError::new("not_found", "No gatekeeper record has that id."))?;
        if row.verdict != "decided" {
            return Err(ApiError::new(
                "refused",
                "Only a decision Helm made for the user can be changed here.",
            ));
        }
        if !matches!(row.state.as_str(), "open" | "kept") {
            return Err(ApiError::new("refused", "That record is already closed."));
        }
        let chose = row.choice.clone().unwrap_or_default();
        let text = correction(&chose, &row.question, new_answer);
        let now = helm_core::now_ms();
        let sub_id = uuid::Uuid::new_v4().to_string();
        if let Err(r) = self
            .db
            .write(|tx| helm_chats::archive::restore_in_tx(tx, &row.chat_id, now))
            .map_err(|e| ApiError::internal(e.to_string()))?
        {
            return Err(r.into());
        }
        helm_chats::submit::host_send(
            &self.rt,
            helm_chats::submit::HostSend {
                chat_id: &row.chat_id,
                submission_id: &sub_id,
                text: &text,
                origin: SubmissionOrigin::Answer,
            },
        )?;
        let updated = self
            .db
            .write(|tx| {
                if !store::veto(tx, id, now)? {
                    return Ok(None);
                }
                tx.execute(
                    "UPDATE gate_asks SET answer = ?1 WHERE id = ?2 AND answer IS NULL",
                    [new_answer, id],
                )?;
                store::ask(tx, id)
            })
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| {
                ApiError::new("refused", "That record was already closed by another path.")
            })?;
        self.emit(&updated, "changed");
        Ok(updated)
    }

    fn emit(&self, row: &GateAsk, what: &str) {
        self.events.emit(
            AppEvent::new("gate")
                .with("askId", row.id.clone())
                .with("chatId", row.chat_id.clone())
                .with("what", what.to_owned()),
        );
    }

    /// One audit row for a card the gate let through, with the reason.
    fn pass(&self, chat_id: &str, profile: &str, title: &str, i: &Interaction, why: PassReason) {
        let id = uuid::Uuid::new_v4().to_string();
        let reason = match why {
            PassReason::Safety => "safety",
            PassReason::Limit => "limit",
            PassReason::Paused => "paused",
        };
        let now = helm_core::now_ms();
        let _ = self.db.write(|tx| {
            store::open_ask(
                tx,
                &id,
                profile,
                chat_id,
                title,
                &i.id,
                &scrub::question_of(i),
                &serde_json::to_string(i).unwrap_or_default(),
                now,
            )?;
            store::finish_ask(
                tx,
                &id,
                "passed",
                Some(reason),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                0,
                "open",
                None,
                now,
            )
        });
    }
}

/// The words the correction carries. The chat makes the change, which goes through the
/// normal review and landing; Helm never reverts anything itself.
pub fn correction(chose: &str, question: &str, wanted: &str) -> String {
    format!(
        "Correction from the user: Helm chose \u{ab}{chose}\u{ab} on \u{ab}{question}\u{ab}. \
         The user wants \u{ab}{wanted}\u{ab}. Undo what depended on \u{ab}{chose}\u{ab} where \
         you can, through a new reviewed change if it has landed (never by rewriting history), \
         and say what can't be undone."
    )
}

impl QuestionGate for GateService {
    fn offer(&self, chat_id: &str, i: &Interaction) -> GateOffer {
        let now = helm_core::now_ms();
        // What the card is, who the chat belongs to, and whether the chat runs on a local
        // model (its words never go to Claude).
        let (profile, settings, chat_title, local_chat) = self
            .db
            .snapshot(|tx| {
                let profile = store::chat_profile(tx, chat_id);
                let settings = store::load_settings(tx, &profile)?;
                let (title, local) = tx
                    .query_row(
                        "SELECT title, provider IS NOT NULL FROM chats WHERE id = ?1",
                        [chat_id],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
                    )
                    .unwrap_or_else(|_| (String::new(), false));
                Ok::<_, rusqlite::Error>((profile, settings, title, local))
            })
            .unwrap_or_else(|_| (String::new(), GateSettings::default(), String::new(), false));
        if !settings.enabled {
            return GateOffer::Pass;
        }
        // The safety line, checked here as well as in the actor: only a plain question card
        // is ever judged.
        if i.attention != Some(InteractionAttention::Question) || i.secret == Some(true) {
            self.pass(chat_id, &profile, &chat_title, i, PassReason::Safety);
            return GateOffer::Pass;
        }
        let slot = match self.admit(now) {
            Ok(s) => s,
            Err(reason) => {
                self.pass(chat_id, &profile, &chat_title, i, reason);
                return GateOffer::Pass;
            }
        };

        // Open the record and build the job in one write, so the audit always covers a card
        // the gate took.
        let question = scrub::question_of(i);
        let card_json = serde_json::to_string(i).unwrap_or_default();
        let ask_id = uuid::Uuid::new_v4().to_string();
        let job = self
            .db
            .write(|tx| -> rusqlite::Result<Option<Value>> {
                if !store::open_ask(
                    tx,
                    &ask_id,
                    &profile,
                    chat_id,
                    &chat_title,
                    &i.id,
                    &question,
                    &card_json,
                    now,
                )? {
                    return Ok(None);
                }
                Ok(Some(json!({
                    "title": chat_title,
                    "turns": recent_turns(tx, chat_id)?,
                    "card": {
                        "title": i.title, "header": i.header, "message": i.message,
                        "options": i.options, "optionDescriptions": i.option_descriptions,
                        "allowOther": i.allow_other.unwrap_or(true),
                    },
                    "policy": settings.policy_text(),
                    "bar": settings.bar(),
                    "model": settings.model,
                    "examples": store::examples(tx, &profile)?,
                })))
            })
            .unwrap_or_else(|e| {
                log::warn!("gatekeeper could not open its record for {chat_id}: {e}");
                None
            });
        let Some(job) = job else {
            // The record could not be opened (or the request replayed): let the card through.
            self.lock().release(slot);
            return GateOffer::Pass;
        };

        // Judge away from the actor: the helper blocks, and the verdict call blocks on the
        // card's actor, which is never this thread.
        let db = self.db.clone();
        let events = self.events.clone();
        let rt = self.rt.clone();
        let gate = self.gate.clone();
        let chat_id = chat_id.to_owned();
        let card = i.clone();
        std::thread::spawn(move || {
            judge(
                db, events, rt, gate, chat_id, card, slot, ask_id, settings, local_chat, job, now,
            );
        });
        GateOffer::Hold
    }
}

/// Judge one held card. Runs on a thread of its own.
#[allow(clippy::too_many_arguments)]
fn judge(
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    rt: ChatRuntime,
    gate: Arc<Mutex<Limiter>>,
    chat_id: String,
    card: Interaction,
    slot: Slot,
    ask_id: String,
    settings: GateSettings,
    local_chat: bool,
    job: Value,
    started: i64,
) {
    let mut held = slot;
    let answer = if local_chat {
        // Its words never go to Claude, and this build has no local judge: hand it over as
        // it stands, which the route table also says whatever a judge might have answered.
        None
    } else {
        {
            let mut g = gate.lock().unwrap_or_else(|p| p.into_inner());
            g.start_running(slot);
        }
        held = Slot::Running;
        let spawner = rt.spawner();
        match spawner.run_gatekeeper(&job) {
            Ok(v) => parse_answer(&v),
            Err(e) => {
                log::warn!("the gatekeeper helper did not answer for chat {chat_id}: {e}");
                let now = helm_core::now_ms();
                let paused = gate
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .note_failure(now);
                if paused {
                    log::warn!("the question gate paused itself after repeated failures");
                }
                None
            }
        }
    };
    let facts = CardFacts {
        question: scrub::question_of(&card),
        options: card.options.clone().unwrap_or_default(),
        allow_other: card.allow_other.unwrap_or(true),
    };
    let decision = match &answer {
        Some(a) => route::route(local_chat, settings.bar(), &facts, a),
        None if local_chat => Decision::Escalate,
        None => Decision::Failed,
    };
    // The model's own write-up, scrubbed, or a plain one from the card.
    let brief = answer
        .as_ref()
        .and_then(|a| a.brief.as_ref())
        .and_then(scrub::clean_brief)
        .unwrap_or_else(|| scrub::fallback_brief(&card));
    let applied = settle(
        &db, &events, &rt, &card, &ask_id, &settings, decision, brief, local_chat, started,
    );
    gate.lock().unwrap_or_else(|p| p.into_inner()).release(held);
    if !applied {
        log::debug!("the user answered chat {chat_id}'s card before the gatekeeper did");
    }
}

/// The helper's line, checked: a decision needs a choice, a confidence and a risk. Anything
/// shaped wrong counts as a failure to judge. The helper says `"escalate"`; `"ask"` stays
/// accepted as a synonym.
fn parse_answer(v: &Value) -> Option<ModelAnswer> {
    let o = v.as_object()?;
    let decide = match o.get("verdict")?.as_str()? {
        "decide" => true,
        "escalate" | "ask" => false,
        _ => return None,
    };
    let choice = o.get("choice").and_then(Value::as_str).map(str::to_owned);
    if decide && choice.is_none() {
        return None;
    }
    Some(ModelAnswer {
        decide,
        choice,
        reason: o
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(240)
            .collect(),
        confidence: o
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0),
        risk: o
            .get("risk")
            .and_then(Value::as_str)
            .and_then(Risk::parse)
            .unwrap_or(Risk::Medium),
        reversible: o
            .get("reversible")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        brief: o.get("brief").cloned(),
    })
}

/// Apply one decision: answer the card through the runtime, then write the audit row once.
/// Returns whether the verdict reached the card.
/// The audit facts of one decision: the choice, the reason, the confidence and the risk word.
type Audit = (String, Option<String>, Option<f64>, Option<&'static str>);

#[allow(clippy::too_many_arguments)]
fn settle(
    db: &Arc<Database>,
    events: &Arc<dyn EventSink>,
    rt: &ChatRuntime,
    card: &Interaction,
    ask_id: &str,
    settings: &GateSettings,
    decision: Decision,
    brief: GateBrief,
    local_chat: bool,
    started: i64,
) -> bool {
    let now = helm_core::now_ms();
    let took = now.max(started) - started;
    let (verdict, outcome, audit): (&str, GateOutcome, Option<Audit>) = match &decision {
        Decision::Decide {
            choice,
            reason,
            confidence,
            risk,
        } => {
            let mut fields = serde_json::Map::new();
            fields.insert(
                "value".into(),
                json!(format!(
                    "{choice} (Helm chose this default for the user; \
                     they may change it later, so keep it easy to undo)"
                )),
            );
            (
                "decided",
                GateOutcome::Decide {
                    fields,
                    marker: format!(
                        "Helm answered for you: \u{ab}{choice}\u{ab}, because {reason}."
                    ),
                },
                Some((
                    choice.clone(),
                    Some(reason.clone()),
                    Some(*confidence),
                    Some(risk.as_str()),
                )),
            )
        }
        Decision::Escalate => (
            "escalated",
            GateOutcome::Escalate {
                brief: brief.clone(),
            },
            Some((
                String::new(),
                Some("This one needs the user.".to_owned()),
                None,
                None,
            )),
        ),
        Decision::Failed | Decision::Pass(_) => (
            "failed",
            GateOutcome::Failed {
                note: "Helm couldn't check this one.".into(),
            },
            Some((
                String::new(),
                Some("The check did not finish.".to_owned()),
                None,
                None,
            )),
        ),
    };
    let applied = rt.gate_verdict(&card.chat_id, &card.id, outcome);
    // A verdict that finds no card means the user (or a dismissal) settled the card first;
    // the audit says the chat answered it. A failure keeps the row open: there is nothing
    // the user could have answered differently, the card simply went back to being normal.
    let (state, answered_via) = if !applied && !matches!(&decision, Decision::Failed) {
        ("answered", Some("chat"))
    } else {
        ("open", None)
    };
    let wrote = db
        .write(|tx| {
            store::finish_ask(
                tx,
                ask_id,
                verdict,
                None,
                match &decision {
                    Decision::Decide { .. } | Decision::Escalate => {
                        Some(if local_chat { "rule" } else { "claude" })
                    }
                    _ => Some("rule"),
                },
                if local_chat {
                    None
                } else {
                    Some(&settings.model)
                },
                audit.as_ref().and_then(|a| a.2),
                audit.as_ref().and_then(|a| a.3),
                audit.as_ref().and_then(|a| a.1.as_deref()),
                audit
                    .as_ref()
                    .map(|a| a.0.as_str())
                    .filter(|c| !c.is_empty()),
                matches!(&decision, Decision::Escalate).then_some(&brief),
                took,
                state,
                answered_via,
                now,
            )
        })
        .unwrap_or(false);
    if wrote {
        if let Ok(Some(row)) = db.snapshot(|tx| store::ask(tx, ask_id)) {
            events.emit(
                AppEvent::new("gate")
                    .with("askId", row.id)
                    .with("chatId", row.chat_id)
                    .with("what", verdict.to_owned()),
            );
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_chats::runtime::{Limits, RuntimeConfig, SpawnRequest, Spawned};
    use helm_core::db::{Database, MigrationRegistry};

    struct NoWorker;
    impl helm_chats::runtime::Spawner for NoWorker {
        fn spawn(&self, _req: &SpawnRequest) -> Result<Spawned, String> {
            Err("no worker in this test".into())
        }
    }

    fn card(chat: &str, req: &str, attention: InteractionAttention, secret: bool) -> Interaction {
        Interaction {
            chat_id: chat.into(),
            id: req.into(),
            method: helm_chats::types::InteractionMethod::Select,
            attention: Some(attention),
            title: Some("Pay the invoice?".into()),
            header: None,
            message: None,
            context: None,
            options: Some(vec!["Pay".into(), "Wait".into()]),
            option_descriptions: None,
            allow_other: Some(true),
            secret: Some(secret),
            step: None,
            steps: None,
            answer: None,
            prefill: None,
            gate: None,
        }
    }

    fn env() -> (tempfile::TempDir, Arc<Database>) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        reg.initializer("gatekeeper", crate::store::init);
        let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
        (dir, db)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_gate_answer_is_never_the_users_typed_words() {
        // The correction for a changed decision reaches the chat as an Answer-origin
        // submission — the consent words rule counts only `Typed`, so this can never pass
        // for the user's own consent.
        let (_dir, db) = env();
        let chat = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        db.write(|tx| {
            crate::store::open_ask(
                tx,
                "a1",
                "",
                &chat,
                "Pay the invoice?",
                "req-1",
                "Pay the invoice?",
                "{}",
                100,
            )?;
            crate::store::finish_ask(
                tx,
                "a1",
                "decided",
                None,
                Some("claude"),
                Some("sonnet"),
                Some(0.9),
                Some("low"),
                Some("routine"),
                Some("Pay"),
                None,
                900,
                "open",
                None,
                1_100,
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        let events: Arc<dyn EventSink> = Arc::new(helm_ipc::NullSink);
        let rt = ChatRuntime::new(
            db.clone(),
            Arc::new(helm_chats::clock::Clock::new(0)),
            events.clone(),
            Arc::new(NoWorker),
            RuntimeConfig {
                profile_dir: _dir.path().join("prof"),
                limits: Limits::default(),
            },
            tokio::runtime::Handle::current(),
        );
        let svc = GateService::new(db.clone(), events, rt);
        let updated = svc.change_answer("a1", "Use the slower plan").unwrap();
        assert_eq!(updated.state, "vetoed");
        let rows: Vec<(String, String)> = db
            .snapshot(|tx| {
                let mut st = tx.prepare(
                    "SELECT s.text, COALESCE(m.origin, '') FROM submissions s \
                     LEFT JOIN submission_meta m ON m.submission_id = s.id WHERE s.chat_id = ?1",
                )?;
                let out = st
                    .query_map([&chat], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(out)
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "answer", "never the user's own words");
        assert!(rows[0].0.contains("Correction from the user:"));
        assert!(rows[0].0.contains("\u{ab}Pay\u{ab}"));
        assert!(rows[0].0.contains("\u{ab}Use the slower plan\u{ab}"));
        assert!(rows[0].0.contains("never by rewriting history"));
        // The decided row left the review list and remembers the new answer.
        let row = svc.get("a1").unwrap().unwrap();
        assert_eq!(row.answer.as_deref(), Some("Use the slower plan"));
        // A second change of the same record is refused: the first closed it.
        assert!(svc.change_answer("a1", "again").is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_safety_line_is_checked_twice() {
        // Even if a card reached the gate that the actor should never have offered, the gate
        // passes it and says so in its own record.
        let (_dir, db) = env();
        let chat = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        let events: Arc<dyn EventSink> = Arc::new(helm_ipc::NullSink);
        let rt = ChatRuntime::new(
            db.clone(),
            Arc::new(helm_chats::clock::Clock::new(0)),
            events.clone(),
            Arc::new(NoWorker),
            RuntimeConfig {
                profile_dir: _dir.path().join("prof"),
                limits: Limits::default(),
            },
            tokio::runtime::Handle::current(),
        );
        let svc = GateService::new(db, events, rt);
        for (n, attention) in [
            ("req-a", InteractionAttention::Approval),
            ("req-b", InteractionAttention::Trust),
            ("req-c", InteractionAttention::Plan),
        ] {
            assert_eq!(
                svc.offer(&chat, &card(&chat, n, attention, false)),
                GateOffer::Pass,
                "cards that grant authority are never judged"
            );
        }
        assert_eq!(
            svc.offer(
                &chat,
                &card(&chat, "req-d", InteractionAttention::Question, true)
            ),
            GateOffer::Pass,
            "a secret input is never judged"
        );
        // A replay of the same request leaves the first record and stacks nothing.
        assert_eq!(
            svc.offer(
                &chat,
                &card(&chat, "req-a", InteractionAttention::Trust, false)
            ),
            GateOffer::Pass
        );
        let rows = svc.list("all", "", 50, helm_core::now_ms()).unwrap();
        assert_eq!(rows.len(), 4, "each refused card left its record");
        assert!(rows
            .iter()
            .all(|r| r.verdict == "passed" && r.pass_reason.as_deref() == Some("safety")));
    }

    #[test]
    fn the_answer_line_is_checked() {
        let ok = parse_answer(&json!({
            "verdict": "decide", "choice": "Yes", "reason": "routine",
            "confidence": 0.9, "risk": "low", "reversible": true
        }))
        .unwrap();
        assert!(ok.decide);
        assert_eq!(ok.choice.as_deref(), Some("Yes"));
        // The helper says "escalate"; "ask" is accepted as a synonym.
        for verdict in ["escalate", "ask"] {
            let a = parse_answer(&json!({"verdict": verdict, "reason": "needs you"})).unwrap();
            assert!(!a.decide, "{verdict} means ask the user");
            assert_eq!(a.risk, Risk::Medium, "missing risk errs to medium");
            assert!(!a.reversible, "missing reversible errs to unsafe");
        }
        assert!(
            parse_answer(&json!({"verdict": "decide"})).is_none(),
            "a decide with no choice is not an answer"
        );
        assert!(parse_answer(&json!({"verdict": "maybe"})).is_none());
        assert!(parse_answer(&json!("junk")).is_none());
        assert!(
            parse_answer(&json!({"confidence": 0.9})).is_none(),
            "no verdict at all"
        );
    }
}
