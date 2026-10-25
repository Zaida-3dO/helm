//! The user's own actions on a task, from the workflow panel in the main window: Sign off plan,
//! Skip a stage, Allow more rounds, Park, Reopen, End task and
//! Mark visual / Not visual. The change buttons (Merge, Decline, Drop change, Retry land, Ready for
//! review) are [`crate::land::user_action`] and [`crate::land::ready`].
//!
//! These are `api` ops, never host tools, so no agent can call them: an op answers only the main
//! window. Where an agent could take the same step only with the user's consent (skip a required
//! stage, allow more rounds, reopen), the user's step runs the same action code through the same
//! [`kinds::consent`] helper with [`Proof::user`], so there is one path and one record per step.
//! Approval stays bound to what the user saw: Sign off names the plan id the panel showed and is
//! refused once the plan of record has changed.

use crate::actions::{self, Act};
use crate::engine;
use crate::kinds::{self, Proof};
use crate::store::{self, Event, Run};
use crate::types::*;
use rusqlite::Transaction;
use serde_json::{json, Value};

pub const CHAT_OFF: &str = "Orchestrator is off for this chat. Turn it on to do this.";
pub const MASTER_OFF: &str = "Orchestrator is off in Settings, so the workflow takes no actions.";
pub const MID_LAND: &str =
    "A change of this task is being landed right now; wait until its land finishes or fails.";

/// The task actions, in the order the panel's menu lists them.
pub const TASK_ACTIONS: &[&str] = &[
    "signOff",
    "skip",
    "allowMore",
    "park",
    "reopen",
    "visual",
    "notVisual",
    "end",
];

fn ev(kind: &str) -> Event<'_> {
    Event {
        kind,
        ..Default::default()
    }
}

/// Whether any change of the task is in the middle of a land.
fn mid_land(tx: &Transaction<'_>, run: &Run) -> rusqlite::Result<bool> {
    Ok(store::changes(tx, &run.id)?.iter().any(|c| {
        matches!(
            c.state.as_str(),
            "landing" | "pending_land" | "landing_draft"
        )
    }))
}

/// One task action. `a` holds `action` and its own fields (`planId`, `stage`, `n`).
pub fn task_action(
    tx: &Transaction<'_>,
    kinds: &helm_consent::AllowKinds,
    run_id: &str,
    a: &Value,
    now: i64,
) -> actions::Out {
    let Some(run) = store::run(tx, run_id)? else {
        return Ok(Err("No such task.".into()));
    };
    let action = a.get("action").and_then(Value::as_str).unwrap_or("");
    if !TASK_ACTIONS.contains(&action) {
        return Ok(Err(format!(
            "action must be one of {}.",
            TASK_ACTIONS.join(", ")
        )));
    }
    let s = crate::settings::load(tx)?;
    // End task is the way out of a task whatever the chat's state; everything else needs the
    // workflow on for this chat.
    if action != "end" {
        if !s.enabled {
            return Ok(Err(MASTER_OFF.into()));
        }
        if !crate::mode::in_effect(tx, &run.chat_id, &s)? {
            return Ok(Err(CHAT_OFF.into()));
        }
    }
    let chat = run.chat_id.clone();
    let x = Act {
        chat_id: &chat,
        s: &s,
        kinds,
        now,
    };
    let is_active = run.status == "active";
    match action {
        "signOff" => sign_off(tx, &x, run, a),
        "skip" | "allowMore" | "park" if !is_active => Ok(Err(
            "That task is not the chat's active task; reopen it first.".into(),
        )),
        "skip" => {
            let stage = a.get("stage").cloned().unwrap_or(Value::Null);
            let args = json!({"stage": stage, "reason": "You skipped it from the workflow panel."});
            actions::skip_with(tx, &x, &args, Proof::user())
        }
        "allowMore" => {
            let args = json!({"stage": a.get("stage").cloned().unwrap_or(Value::Null),
                              "n": a.get("n").cloned().unwrap_or(json!(2))});
            actions::allow_more_with(tx, &x, &args, Proof::user())
        }
        "park" => actions::park(tx, &x, &json!({"reason": "You parked it."})),
        "reopen" => reopen(tx, &x, run, &Proof::user()),
        "visual" | "notVisual" => visual(tx, &x, run, action == "visual"),
        _ => end(tx, &x, run),
    }
}

/// Sign off plan: the plan the panel showed (`planId`) must still be the plan of record,
/// and approved by plan review when that stage is needed.
fn sign_off(tx: &Transaction<'_>, x: &Act<'_>, mut run: Run, a: &Value) -> actions::Out {
    if x.s.plan_sign_off != SignOff::Me {
        return Ok(Err(
            "Plan sign-off is off in Settings, so the plan needs no sign-off.".into(),
        ));
    }
    if run.status != "active" {
        return Ok(Err(
            "That task is not the chat's active task; reopen it first.".into(),
        ));
    }
    let Some(plan) = run.plan_id.clone() else {
        return Ok(Err("The task has no plan of record to sign off.".into()));
    };
    if a.get("planId").and_then(Value::as_str) != Some(plan.as_str()) {
        return Ok(Err(
            "The plan changed since it was shown; read the new plan, then sign it off.".into(),
        ));
    }
    let review = x.s.stages.plan_review.use_;
    let review_needed = review == StageUse::Required
        || (review == StageUse::Auto && !engine::skipped(&run, "planReview"));
    if review_needed && !run.plan_approved {
        return Ok(Err(
            "The plan review has not approved this plan yet; sign off once it has.".into(),
        ));
    }
    if run.plan_signed_off {
        return Ok(Ok(json!({"signedOff": true, "changed": false})));
    }
    run.plan_signed_off = true;
    store::save_run(tx, &run, None, x.now)?;
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            verdict: Some("signOff"),
            plan_id: Some(&plan),
            body: Some("You signed off the plan."),
            ..ev("consent")
        },
        x.now,
    )?;
    Ok(Ok(json!({"signedOff": true, "changed": true})))
}

/// The service the AFK crate installs: whether AFK is on with plan approval set to approve. While
/// it says yes, a plan that waits on the user's sign-off is signed off automatically.
#[derive(Clone)]
pub struct AfkPlanSignOff(pub std::sync::Arc<dyn Fn() -> bool + Send + Sync>);

/// Sign off, for the user who is away, the plan of record of `chat_id`'s active task. It follows
/// the same rules as the user's own sign-off (the workflow on for the chat, sign-off set to "me",
/// the plan reviewed where a review is needed) and records `afk_plan_signed_off` with how to take
/// it back. Returns whether it signed anything off.
pub fn afk_sign_off(tx: &Transaction<'_>, chat_id: &str, now: i64) -> rusqlite::Result<bool> {
    let s = crate::settings::load(tx)?;
    if !s.enabled || s.plan_sign_off != SignOff::Me || !crate::mode::in_effect(tx, chat_id, &s)? {
        return Ok(false);
    }
    let Some(mut run) = store::active_run(tx, chat_id)? else {
        return Ok(false);
    };
    let Some(plan) = run.plan_id.clone() else {
        return Ok(false);
    };
    let review = s.stages.plan_review.use_;
    let review_needed = review == StageUse::Required
        || (review == StageUse::Auto && !engine::skipped(&run, "planReview"));
    if run.plan_signed_off || (review_needed && !run.plan_approved) {
        return Ok(false);
    }
    run.plan_signed_off = true;
    store::save_run(tx, &run, None, now)?;
    store::event(
        tx,
        chat_id,
        Some(&run),
        &Event {
            verdict: Some("afkSignOff"),
            plan_id: Some(&plan),
            body: Some(
                "The plan was signed off automatically while you were away, because AFK plan approval is set to approve. To take it back, park the task and reopen it, or end the task.",
            ),
            ..ev("afk_plan_signed_off")
        },
        now,
    )?;
    Ok(true)
}

/// Every active task whose plan waits on the user, signed off through [`afk_sign_off`] (AFK just
/// turned on). Returns the chats it signed off.
pub fn afk_sign_off_waiting(
    db: &helm_core::db::Database,
    events: &dyn helm_ipc::EventSink,
    now: i64,
) -> Vec<String> {
    let chats: Vec<String> = db
        .snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT chat_id FROM workflow_runs
                 WHERE status = 'active' AND plan_id IS NOT NULL AND plan_signed_off = 0",
            )?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap_or_default();
    let mut done = vec![];
    for chat in chats {
        if db.write(|tx| afk_sign_off(tx, &chat, now)).unwrap_or(false) {
            done.push(chat);
        }
    }
    store::emit_touched(events);
    done
}

/// Reopen: a parked or handed-off task becomes active again with the next attempt,
/// which voids every Allow grant and ask of the earlier one. Its handed-off, failed, declined and
/// awaiting changes go back to open (their approval binding stays; a new hand-off is needed).
/// Refused while a change is mid-land or the chat has another active task.
pub fn reopen(tx: &Transaction<'_>, x: &Act<'_>, mut run: Run, proof: &Proof) -> actions::Out {
    if !matches!(run.status.as_str(), "parked" | "handed_off") {
        return Ok(Err(format!(
            "Only a parked or handed-off task can be reopened; this one is {}.",
            run.status
        )));
    }
    if let Some(other) = store::active_run(tx, x.chat_id)? {
        return Ok(Err(format!(
            "Task \"{}\" is active in this chat; park or end it first.",
            crate::mode::tooltip_reason(&other.title)
        )));
    }
    if mid_land(tx, &run)? {
        return Ok(Err(MID_LAND.into()));
    }
    let t = json!({"runId": run.id, "attempt": run.attempt});
    if let Err(e) = kinds::consent(
        tx,
        x.kinds,
        x.chat_id,
        "reopen",
        &t,
        proof,
        "Reopening a task is the user's step.",
        x.now,
    )? {
        return Ok(Err(e));
    }
    run.status = "active".into();
    run.park_reason = None;
    run.attempt += 1;
    store::save_run(tx, &run, None, x.now)?;
    for mut c in store::changes(tx, &run.id)? {
        if matches!(
            c.state.as_str(),
            "handed_off" | "land_failed" | "declined" | "awaiting_merge"
        ) {
            c.error = None;
            if let Err(e) = engine::move_change(tx, &run, &mut c, "open", false, x.now)? {
                return Ok(Err(e));
            }
        }
    }
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            round: Some(run.attempt),
            ..ev("reopened")
        },
        x.now,
    )?;
    Ok(Ok(
        json!({"runId": run.id, "status": "active", "attempt": run.attempt}),
    ))
}

/// End task: the task ends with its record kept, and every change it still holds is
/// released (dropped; nothing changes on GitHub). Refused while a change is mid-land.
fn end(tx: &Transaction<'_>, x: &Act<'_>, mut run: Run) -> actions::Out {
    if !matches!(run.status.as_str(), "active" | "parked" | "handed_off") {
        return Ok(Ok(
            json!({"runId": run.id, "status": run.status, "changed": false}),
        ));
    }
    if mid_land(tx, &run)? {
        return Ok(Err(MID_LAND.into()));
    }
    for mut c in store::changes(tx, &run.id)? {
        if engine::is_terminal(&c.state) {
            continue;
        }
        let (repo, branch) = (c.repo.clone(), c.branch.clone());
        if let Err(e) = engine::move_change(tx, &run, &mut c, "dropped", false, x.now)? {
            return Ok(Err(e));
        }
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                repo: Some(&repo),
                branch: Some(&branch),
                body: Some("You ended the task."),
                ..ev("change_dropped")
            },
            x.now,
        )?;
    }
    run.status = "ended".into();
    store::save_run(tx, &run, None, x.now)?;
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            body: Some("You ended the task."),
            ..ev("ended")
        },
        x.now,
    )?;
    Ok(Ok(
        json!({"runId": run.id, "status": "ended", "changed": true}),
    ))
}

/// Mark visual / Not visual. Only the user can clear the flag (the agent's `update` can
/// only set it).
fn visual(tx: &Transaction<'_>, x: &Act<'_>, mut run: Run, on: bool) -> actions::Out {
    if !matches!(run.status.as_str(), "active" | "parked") {
        return Ok(Err(
            "Only an active or parked task can be marked visual or not.".into(),
        ));
    }
    if run.visual == on {
        return Ok(Ok(json!({"visual": on, "changed": false})));
    }
    run.visual = on;
    store::save_run(tx, &run, None, x.now)?;
    let kind = if on { "visual_set" } else { "visual_cleared" };
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            body: Some("By you, from the workflow panel."),
            ..ev(kind)
        },
        x.now,
    )?;
    Ok(Ok(json!({"visual": on, "changed": true})))
}
