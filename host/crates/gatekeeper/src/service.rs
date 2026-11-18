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
//!
//! At most two judgements run at once. A card admitted while both are busy waits its turn,
//! but only for as long as still leaves the helper its whole time inside the hold; a card
//! that cannot start in time goes to the user as a normal one. Every judgement that does not
//! come back usable (the helper failed to start, timed out, or answered something unusable)
//! counts toward the failure pause.
//!
//! History, review and change answer act only on the current person's records (and on records
//! that belong to no one).

use crate::route::{self, CardFacts, Decision, Limiter, ModelAnswer, PassReason, Risk, Slot};
use crate::scrub;
use crate::settings::{self, GateSettings};
use crate::store::{self, GateAsk};
use helm_chats::runtime::{ChatRuntime, GATEKEEPER_TIMEOUT, GATE_HOLD};
use helm_chats::seams::{GateOffer, GateOutcome, QuestionGate};
use helm_chats::triage::recent_turns;
use helm_chats::types::{GateBrief, Interaction, InteractionAttention, SubmissionOrigin};
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, EventSink};
use serde_json::{json, Value};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The limits, and the signal a waiting judgement listens for when a running one ends.
#[derive(Default)]
pub(crate) struct Gate {
    limits: Mutex<Limiter>,
    freed: Condvar,
}

impl Gate {
    /// A gate with limits of its own.
    pub(crate) fn with_caps(caps: route::Caps) -> Self {
        Self {
            limits: Mutex::new(Limiter::with_caps(caps)),
            freed: Condvar::new(),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Limiter> {
        self.limits.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Give a slot back and wake the judgements waiting for one.
    pub(crate) fn release(&self, slot: Slot) {
        self.lock().release(slot);
        self.freed.notify_all();
    }

    /// Wait, at most `within`, until the job in `slot` may run. False when it never could:
    /// the slot is still the caller's to release.
    pub(crate) fn wait_to_run(&self, slot: Slot, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let mut l = self.lock();
        loop {
            if l.start_running(slot) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            l = self
                .freed
                .wait_timeout(l, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

pub struct GateService {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    rt: ChatRuntime,
    gate: Arc<Gate>,
    /// The test switch ([`settings::test_switch`]): in debug builds, forces the gate on or off
    /// for everyone. `None` follows each person's own setting.
    forced: Option<bool>,
}

impl GateService {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>, rt: ChatRuntime) -> Arc<Self> {
        let forced = settings::test_switch(
            cfg!(debug_assertions),
            std::env::var(settings::TEST_GATE_ENV).ok().as_deref(),
            std::env::var_os("HELM_TEST_CLAUDE").is_some(),
        );
        Self::build(db, events, rt, forced)
    }

    fn build(
        db: Arc<Database>,
        events: Arc<dyn EventSink>,
        rt: ChatRuntime,
        forced: Option<bool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            events,
            rt,
            gate: Arc::new(Gate::default()),
            forced,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Limiter> {
        self.gate.lock()
    }

    pub fn paused_until(&self, now: i64) -> Option<i64> {
        self.lock().paused_until(now)
    }

    /// History and the review list: `all`, `decided`, `asked`, `failed` or `review`, for the
    /// current person.
    pub fn list(
        &self,
        filter: &str,
        query: &str,
        limit: i64,
        now: i64,
    ) -> Result<Vec<GateAsk>, ApiError> {
        if !matches!(filter, "all" | "decided" | "asked" | "failed" | "review") {
            return Err(ApiError::bad_args(format!(
                "\"{filter}\" is not a list filter the gatekeeper keeps."
            )));
        }
        self.db
            .snapshot(|tx| {
                let profile = store::active_profile(tx);
                store::list(tx, &profile, filter, query, limit, now)
            })
            .map_err(|e| ApiError::internal(e.to_string()))
    }

    pub fn get(&self, id: &str) -> Result<Option<GateAsk>, ApiError> {
        self.db
            .snapshot(|tx| store::ask(tx, id))
            .map_err(|e| ApiError::internal(e.to_string()))
    }

    /// One record, whoever's it is: the list shows the current person's rows first, but any
    /// row can be marked or changed by id.
    fn own(&self, id: &str) -> Result<GateAsk, ApiError> {
        self.db
            .snapshot(|tx| store::ask(tx, id))
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::new("not_found", "No gatekeeper record has that id."))
    }

    /// Mark one row: a decision "fine" or "should have asked me", or an ask "didn't need me",
    /// with an optional note of at most 300 characters. The first mark wins; re-marking answers
    /// with the row as it stands.
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
        let row = self.own(id)?;
        if !store::mark_fits(&row.verdict, review) {
            return Err(ApiError::bad_args(
                "\"Fine\" and \"should have asked me\" mark a decision Helm made; \"didn't need me\" marks a question it asked you.",
            ));
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
    ///
    /// The record is closed first, as one compare-and-set, and only the call that closed it
    /// sends the correction: a double click or a second window can never send it twice. If the
    /// message cannot be sent, the record is put back as it was.
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
        let row = self.own(id)?;
        if row.verdict != "decided" {
            return Err(ApiError::new(
                "refused",
                "Only a decision Helm made for the user can be changed here.",
            ));
        }
        let now = helm_core::now_ms();
        let closed = self
            .db
            .write(|tx| store::veto(tx, id, now))
            .map_err(|e| ApiError::internal(e.to_string()))?;
        if !closed {
            return Err(ApiError::new("refused", "That record is already closed."));
        }
        let chose = row.choice.clone().unwrap_or_default();
        let text = correction(&chose, &row.question, new_answer);
        let sub_id = uuid::Uuid::new_v4().to_string();
        let sent = self
            .db
            .write(|tx| helm_chats::archive::restore_in_tx(tx, &row.chat_id, now))
            .map_err(|e| ApiError::internal(e.to_string()))
            .and_then(|r| r.map_err(ApiError::from))
            .and_then(|_restored| {
                helm_chats::submit::host_send(
                    &self.rt,
                    helm_chats::submit::HostSend {
                        chat_id: &row.chat_id,
                        submission_id: &sub_id,
                        text: &text,
                        origin: SubmissionOrigin::Answer,
                    },
                )
                .map(|_| ())
            });
        if let Err(e) = sent {
            // Nothing reached the chat: the decision stands and can be changed again.
            let _ = self.db.write(|tx| store::unveto(tx, id, &row.state));
            return Err(e);
        }
        let updated = self
            .db
            .write(|tx| {
                tx.execute(
                    "UPDATE gate_asks SET answer = ?1 WHERE id = ?2 AND answer IS NULL",
                    [new_answer, id],
                )?;
                store::ask(tx, id)
            })
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::new("not_found", "No gatekeeper record has that id."))?;
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
        "Correction from the user: Helm chose \u{ab}{chose}\u{bb} on \u{ab}{question}\u{bb}. \
         The user wants \u{ab}{wanted}\u{bb}. Undo what depended on \u{ab}{chose}\u{bb} where \
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
        if !self.forced.unwrap_or(settings.enabled) {
            return GateOffer::Pass;
        }
        // The safety line, checked here as well as in the actor: only a plain question card
        // is ever judged. The actor never offers any other kind, so in this version this is a
        // second line only, and the cards the actor keeps back leave no record here.
        if i.attention != Some(InteractionAttention::Question) || i.secret == Some(true) {
            self.pass(chat_id, &profile, &chat_title, i, PassReason::Safety);
            return GateOffer::Pass;
        }
        let slot = match self.lock().admit(now) {
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
        // A hold shortened for a test gives the helper all of it but a second; the normal hold
        // leaves the helper its own default.
        let hold = self.rt.limits().gate_deadline;
        let helper_ms = (hold < GATE_HOLD)
            .then(|| hold.saturating_sub(Duration::from_secs(1)).as_millis() as u64);
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
                let mut job = json!({
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
                });
                if let Some(ms) = helper_ms {
                    job["timeoutMs"] = json!(ms);
                }
                Ok(Some(job))
            })
            .unwrap_or_else(|e| {
                log::warn!("gatekeeper could not open its record for {chat_id}: {e}");
                None
            });
        let Some(job) = job else {
            // The record could not be opened (or the request replayed): let the card through.
            self.gate.release(slot);
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
    gate: Arc<Gate>,
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
    // A waiting job may wait for a running slot only as long as still leaves the helper its
    // whole time inside the hold.
    let wait = rt.limits().gate_deadline.saturating_sub(GATEKEEPER_TIMEOUT);
    let answer = if local_chat {
        // Its words never go to Claude, and this build has no local judge: hand it over as
        // it stands, which the route table also says whatever a judge might have answered.
        None
    } else if rt.is_shutting_down() {
        // Helm is quitting: no helper starts now.
        None
    } else if !gate.wait_to_run(slot, wait) {
        log::info!("the question gate was busy too long for chat {chat_id}'s card");
        None
    } else {
        held = Slot::Running;
        let answer = match rt.spawner().run_gatekeeper(&job) {
            Ok(v) => {
                let a = parse_answer(&v);
                if a.is_none() {
                    let said = v
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("not a verdict");
                    log::warn!("the gatekeeper helper gave no verdict for chat {chat_id}: {said}");
                }
                a
            }
            Err(e) => {
                log::warn!("the gatekeeper helper did not answer for chat {chat_id}: {e}");
                None
            }
        };
        // Whatever kept the judgement from coming back usable counts toward the pause: a
        // helper that cannot start, one that reports its own error or time-out, and an
        // answer that is not a verdict.
        if answer.is_none()
            && !rt.is_shutting_down()
            && gate.lock().note_failure(helm_core::now_ms())
        {
            log::warn!("the question gate paused itself after repeated failures");
        }
        answer
    };
    let facts = CardFacts {
        question: scrub::question_of(&card),
        header: card.header.clone().unwrap_or_default(),
        options: card.options.clone().unwrap_or_default(),
        descriptions: card.option_descriptions.clone().unwrap_or_default(),
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
    // The helper is done: the next waiting judgement may start while this one settles.
    gate.release(held);
    let applied = settle(
        &db, &events, &rt, &card, &ask_id, &settings, decision, brief, local_chat, started,
    );
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
                    marker: format!("\u{ab}{choice}\u{bb}, because {reason}."),
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
    // A verdict that finds no card never acted: the user answered first, the hold had already
    // run out, the chat closed or the app was quitting. The row says the card was closed before
    // Helm answered, so History never lists a choice Helm did not make as one it did.
    let state = if applied { "open" } else { "withdrawn" };
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
                None,
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

    /// The gate on a runtime with no live chats, so no verdict ever finds its card. The test
    /// switch is left out: each person's own setting decides.
    fn service(
        dir: &tempfile::TempDir,
        db: &Arc<Database>,
        spawner: Arc<dyn helm_chats::runtime::Spawner>,
    ) -> Arc<GateService> {
        let events: Arc<dyn EventSink> = Arc::new(helm_ipc::NullSink);
        let rt = ChatRuntime::new(
            db.clone(),
            Arc::new(helm_chats::clock::Clock::new(0)),
            events.clone(),
            spawner,
            RuntimeConfig {
                profile_dir: dir.path().join("prof"),
                limits: Limits::default(),
            },
            tokio::runtime::Handle::current(),
        );
        GateService::build(db.clone(), events, rt, None)
    }

    /// A spawner whose gatekeeper helper answers from a script, one answer per job.
    struct Scripted {
        answers: Mutex<std::collections::VecDeque<Result<Value, String>>>,
        jobs: Mutex<Vec<Value>>,
    }
    impl helm_chats::runtime::Spawner for Scripted {
        fn spawn(&self, _req: &SpawnRequest) -> Result<Spawned, String> {
            Err("no worker in this test".into())
        }
        fn run_gatekeeper(&self, job: &Value) -> Result<Value, String> {
            self.jobs.lock().unwrap().push(job.clone());
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("nothing scripted".into()))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_gate_judges_through_its_helper_and_pauses_after_failures() {
        let (dir, db) = env();
        let chat = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        let spawner = Arc::new(Scripted {
            answers: Mutex::new(Default::default()),
            jobs: Mutex::new(Vec::new()),
        });
        let svc = service(&dir, &db, spawner.clone());
        let plain = |req: &str| {
            let mut i = card(&chat, req, InteractionAttention::Question, false);
            i.title = Some("Which folder should the report go in?".into());
            i.options = Some(vec!["reports".into(), "archive".into()]);
            i
        };
        let decide = json!({"verdict": "decide", "choice": "reports", "reason": "it is where the others are",
                            "confidence": 0.9, "risk": "low", "reversible": true});
        // Each row: the helper's answer, then the record it leaves. No chat is live, so no
        // verdict reaches a card: every record says it was closed before Helm answered.
        let rows: Vec<(&str, Result<Value, String>, &str)> = vec![
            ("q1", Ok(decide.clone()), "decided"),
            (
                "q2",
                Ok(json!({"verdict": "escalate", "reason": "needs you"})),
                "escalated",
            ),
            // Three ways a judgement fails: the helper's own error line, a helper that
            // never started, and an answer that is not a verdict. Three trip the pause.
            ("q3", Ok(json!({"error": "timed out"})), "failed"),
            ("q4", Err("the helper did not start".into()), "failed"),
            ("q5", Ok(json!({"verdict": "maybe"})), "failed"),
        ];
        for (req, answer, verdict) in rows {
            assert_eq!(svc.paused_until(helm_core::now_ms()), None, "{req}");
            spawner.answers.lock().unwrap().push_back(answer);
            assert_eq!(svc.offer(&chat, &plain(req)), GateOffer::Hold, "{req}");
            let mut row = None;
            for _ in 0..3_000 {
                row = svc
                    .list("all", "", 50, helm_core::now_ms())
                    .unwrap()
                    .into_iter()
                    .find(|r| r.request_ref == req && r.verdict != "checking");
                if row.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let row = row.unwrap_or_else(|| panic!("{req} never settled"));
            assert_eq!(
                (row.verdict.as_str(), row.state.as_str()),
                (verdict, "withdrawn"),
                "{req}"
            );
            // A verdict that never acted is not listed as one Helm made.
            for filter in ["decided", "asked", "failed", "review"] {
                assert!(
                    svc.list(filter, "", 50, helm_core::now_ms())
                        .unwrap()
                        .iter()
                        .all(|r| r.request_ref != req),
                    "{req} under {filter}"
                );
            }
        }
        assert!(
            svc.paused_until(helm_core::now_ms()).is_some(),
            "three failed judgements pause the gate"
        );
        // While paused, a card passes straight to the user and its record says why.
        assert_eq!(svc.offer(&chat, &plain("q6")), GateOffer::Pass);
        let q6 = svc
            .list("all", "", 50, helm_core::now_ms())
            .unwrap()
            .into_iter()
            .find(|r| r.request_ref == "q6")
            .unwrap();
        assert_eq!(
            (q6.verdict.as_str(), q6.pass_reason.as_deref()),
            ("passed", Some("paused"))
        );
        assert_eq!(
            svc.list("nope", "", 50, helm_core::now_ms())
                .unwrap_err()
                .code,
            "bad_args",
            "an unknown filter never lists everything"
        );
        let jobs = spawner.jobs.lock().unwrap().clone();
        assert_eq!(jobs.len(), 5, "the paused card never reached the helper");
        assert_eq!(jobs[0]["card"]["options"], json!(["reports", "archive"]));
        assert!(
            jobs[0].get("timeoutMs").is_none(),
            "the normal hold leaves the helper its own time"
        );

        // At most two judgements run at once: a waiting job never runs past the limit, and
        // a running job that ends wakes it to take the freed slot.
        let gate = Gate::default();
        let t0 = 1_000_000_000;
        let a = gate.lock().admit(t0).unwrap();
        let _b = gate.lock().admit(t0).unwrap();
        let c = gate.lock().admit(t0).unwrap();
        assert_eq!(c, Slot::Waiting);
        assert!(
            !gate.wait_to_run(c, Duration::from_millis(50)),
            "no slot freed"
        );
        // The waiter blocks on the signal with a long limit; only the release can end its
        // wait in time. (Read the counts through one guard: the lock is not re-entrant.)
        let (ran, waited) = std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                let t = Instant::now();
                (gate.wait_to_run(c, Duration::from_secs(30)), t.elapsed())
            });
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                !waiter.is_finished(),
                "the waiter waits while both slots run"
            );
            gate.release(a);
            waiter.join().unwrap()
        });
        assert!(ran, "a running job ended: the waiting one takes its place");
        assert!(
            waited < Duration::from_secs(10),
            "woken by the release, not by its own time limit"
        );
        let l = gate.lock();
        assert_eq!((l.running, l.waiting), (2, 0));
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
                helm_core::now_ms(),
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
        let svc = service(&_dir, &db, Arc::new(NoWorker));
        // Two changes at once (a double click, or two windows): exactly one closes the record
        // and sends the correction; the other is refused before anything goes out.
        let results: Vec<Result<GateAsk, ApiError>> = std::thread::scope(|s| {
            let a = s.spawn(|| svc.change_answer("a1", "Use the slower plan"));
            let b = s.spawn(|| svc.change_answer("a1", "Use the slower plan"));
            vec![a.join().unwrap(), b.join().unwrap()]
        });
        let won: Vec<&GateAsk> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert_eq!(won.len(), 1, "one change wins");
        assert_eq!(won[0].state, "vetoed");
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
        assert_eq!(rows.len(), 1, "the correction went out once");
        assert_eq!(rows[0].1, "answer", "never the user's own words");
        assert!(rows[0].0.contains("Correction from the user:"));
        assert!(rows[0].0.contains("\u{ab}Pay\u{bb}"));
        assert!(rows[0].0.contains("\u{ab}Use the slower plan\u{bb}"));
        assert!(rows[0].0.contains("never by rewriting history"));
        // The nudge a stop sends is a note from Helm as well: never the user's own words.
        let nudge_id = uuid::Uuid::new_v4().to_string();
        let _ = crate::stops::Host::send(
            &svc.rt,
            &chat,
            &nudge_id,
            &crate::stoproute::nudge_text(&crate::stoproute::Nudge::UseLocal),
        );
        let origin: String = db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT origin FROM submission_meta WHERE submission_id = ?1",
                    [&nudge_id],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(origin, "app", "a nudge is never typed");
        // The decided row left the review list and remembers the new answer.
        let row = svc.get("a1").unwrap().unwrap();
        assert_eq!(row.answer.as_deref(), Some("Use the slower plan"));
        // A second change of the same record is refused: the first closed it.
        assert!(svc.change_answer("a1", "again").is_err());

        // Another person's record is not in the list (it shows the current person's rows), but
        // it can be marked and changed by id like any other.
        db.write(|tx| {
            let asked = helm_core::now_ms();
            crate::store::open_ask(tx, "b1", "p1", &chat, "t", "req-2", "Which?", "{}", asked)?;
            crate::store::finish_ask(
                tx,
                "b1",
                "decided",
                None,
                Some("claude"),
                Some("sonnet"),
                Some(0.9),
                Some("low"),
                Some("routine"),
                Some("Left"),
                None,
                900,
                "open",
                None,
                1_100,
            )?;
            helm_core::prefs::set_raw(tx, "activeProfile", &json!("p2"))
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        let now = helm_core::now_ms();
        let listed = svc.list("all", "", 50, now).unwrap();
        assert!(
            listed.iter().any(|r| r.id == "a1"),
            "a record of no one shows"
        );
        assert!(
            listed.iter().all(|r| r.id != "b1"),
            "another person's is left out of the list"
        );
        assert_eq!(svc.review("b1", "fine", None).unwrap().review, "fine");
        assert!(svc.change_answer("b1", "Right").is_ok());
        // The same person sees and acts on it.
        db.write(|tx| {
            helm_core::prefs::set_raw(tx, "activeProfile", &json!("p1"))
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })
        .unwrap();
        assert!(svc
            .list("all", "", 50, now)
            .unwrap()
            .iter()
            .any(|r| r.id == "b1"));
        // A mark must fit the verdict: "didn't need me" is for a question Helm asked.
        assert_eq!(
            svc.review("b1", "didnt_need_me", None).unwrap_err().code,
            "bad_args"
        );
        assert_eq!(svc.review("b1", "fine", None).unwrap().review, "fine");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_safety_line_is_checked_twice() {
        // Even if a card reached the gate that the actor should never have offered, the gate
        // passes it and says so in its own record.
        let (_dir, db) = env();
        let chat = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        let svc = service(&_dir, &db, Arc::new(NoWorker));
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
