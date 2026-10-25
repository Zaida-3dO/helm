//! The orchestrator's `app_workflow` actions, except `hand_off` and `finish`
//! ([`crate::handoff`]). Each runs in one transaction; a refusal writes nothing unless it says so
//! (a `gate_denied` row, or a consent ask).

use crate::engine::{self, CapStage};
use crate::kinds::{self, Proof};
use crate::store::{self, Change, Event, Run};
use crate::types::*;
use helm_consent::AllowKinds;
use rusqlite::Transaction;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const MAX_PLAN: usize = 256 * 1024;
pub const MAX_TITLE: usize = 200;
pub const MAX_REASON: usize = 1000;

pub type Out = rusqlite::Result<Result<Value, String>>;

pub struct Act<'a> {
    pub chat_id: &'a str,
    pub s: &'a WorkflowSettings,
    pub kinds: &'a AllowKinds,
    pub now: i64,
}

pub fn text<'a>(a: &'a Value, k: &str, max: usize) -> Result<Option<&'a str>, String> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= max => Ok(Some(s.as_str())),
        Some(Value::String(_)) => Err(format!("{k} is longer than {max} characters.")),
        Some(_) => Err(format!("{k} must be text.")),
    }
}

pub fn need<'a>(a: &'a Value, k: &str, max: usize) -> Result<&'a str, String> {
    text(a, k, max)?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{k} is required."))
}

/// `repo` and `branch` from the arguments, checked and normalised.
pub fn target(a: &Value) -> Result<(String, String), String> {
    let repo = crate::settings::normalize_repo(need(a, "repo", 255)?)
        .ok_or("repo must be host/owner/name.")?;
    let branch = need(a, "branch", 255)?;
    if !crate::settings::is_branch_name(branch) {
        return Err("branch is not a valid branch name.".into());
    }
    Ok((repo, branch.to_owned()))
}

pub fn active(tx: &Transaction<'_>, chat: &str) -> rusqlite::Result<Result<Run, String>> {
    Ok(store::active_run(tx, chat)?
        .ok_or_else(|| "This chat has no active task; start one with app_workflow start.".into()))
}

fn ev<'a>(kind: &'a str) -> Event<'a> {
    Event {
        kind,
        ..Default::default()
    }
}

/// `start {title, itemId?, new?}`: the active task, or a new one.
pub fn start(tx: &Transaction<'_>, x: &Act<'_>, a: &Value) -> Out {
    if let Some(r) = store::active_run(tx, x.chat_id)? {
        return Ok(Ok(
            json!({"runId": r.id, "title": r.title, "started": false}),
        ));
    }
    let title = match need(a, "title", MAX_TITLE) {
        Ok(t) => t.trim().to_owned(),
        Err(e) => return Ok(Err(e)),
    };
    let item = match text(a, "itemId", 64) {
        Ok(i) => i,
        Err(e) => return Ok(Err(e)),
    };
    let new = a.get("new").and_then(Value::as_bool).unwrap_or(false);
    if !new {
        for r in store::open_runs(tx, x.chat_id)? {
            if r.status == "parked"
                && store::changes(tx, &r.id)?
                    .iter()
                    .any(|c| !matches!(c.state.as_str(), "landed" | "flipped" | "dropped"))
            {
                return Ok(Err(format!(
                    "Task \"{}\" is parked and still holds changes. Reopen it, release its changes with app_workflow drop, or start a separate task with start {{new: true}}.",
                    r.title
                )));
            }
        }
    }
    // A board item another chat is already working on is refused, unless the user agrees.
    let second_start = match item {
        Some(i) => match crate::claims::check_item(tx, x, i, a)? {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        },
        None => false,
    };
    let id = uuid::Uuid::new_v4().to_string();
    store::insert_run(tx, &id, x.chat_id, &title, item, x.now)?;
    let run = store::run(tx, &id)?.expect("just inserted");
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            body: Some(&title),
            ..ev("run_started")
        },
        x.now,
    )?;
    if second_start {
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                verdict: Some("takeOver"),
                body: Some("A second task for this board item, with the user's agreement."),
                ..ev("consent")
            },
            x.now,
        )?;
    }
    Ok(Ok(json!({"runId": id, "title": title, "started": true})))
}

pub fn plan_id(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// `plan {text, title?}`: adopt the task's plan of record, starting a task if none is active.
/// `plan_text` is the text already read (inline, or from `path` by the caller).
pub fn plan(tx: &Transaction<'_>, x: &Act<'_>, a: &Value, plan_text: &str) -> Out {
    if a.get("repo").is_some() || a.get("branch").is_some() || a.get("queued").is_some() {
        return Ok(Err(
            "Lane plans and queued follow-ups are not in this build; adopt the task's plan.".into(),
        ));
    }
    if plan_text.trim().is_empty() || plan_text.len() > MAX_PLAN {
        return Ok(Err("The plan must be 1 byte to 256 KiB of text.".into()));
    }
    if store::active_run(tx, x.chat_id)?.is_none() {
        let title = match text(a, "title", MAX_TITLE) {
            Ok(Some(t)) if !t.trim().is_empty() => t.trim().to_owned(),
            Ok(_) => plan_text
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("Task")
                .trim_start_matches('#')
                .trim()
                .chars()
                .take(MAX_TITLE)
                .collect(),
            Err(e) => return Ok(Err(e)),
        };
        let mut sa =
            json!({"title": title, "new": a.get("new").cloned().unwrap_or(Value::Bool(false))});
        for k in ["itemId", "userQuote", "grant"] {
            if let Some(v) = a.get(k) {
                sa[k] = v.clone();
            }
        }
        if let Err(e) = start(tx, x, &sa)? {
            return Ok(Err(e));
        }
    }
    let mut run = active(tx, x.chat_id)?.expect("started above");
    let id = plan_id(plan_text);
    if run.plan_id.as_deref() == Some(id.as_str()) {
        return Ok(Ok(json!({"runId": run.id, "planId": id, "changed": false})));
    }
    run.plan_id = Some(id.clone());
    run.plan_approved = false;
    run.plan_signed_off = false;
    store::save_run(tx, &run, Some(plan_text), x.now)?;
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            plan_id: Some(&id),
            ..ev("plan_recorded")
        },
        x.now,
    )?;
    Ok(Ok(json!({"runId": run.id, "planId": id, "changed": true,
        "next": "Have a fresh plan reviewer review exactly this plan and report with app_workflow_report."})))
}

fn change_view(
    tx: &Transaction<'_>,
    s: &WorkflowSettings,
    run: &Run,
    c: &Change,
    all: &[Change],
) -> rusqlite::Result<RunChangeView> {
    let cap = engine::cap(tx, s, run, CapStage::Review, Some((&c.repo, &c.branch)))?;
    let smoke_cap = engine::cap(tx, s, run, CapStage::Smoketest, Some((&c.repo, &c.branch)))?;
    let missing = if c.state == "open" {
        engine::missing(s, run, c, all)
    } else {
        vec![]
    };
    Ok(RunChangeView {
        repo: c.repo.clone(),
        branch: c.branch.clone(),
        pr: c.pr.clone(),
        state: c.state.clone(),
        failed_mode: c.failed_mode.clone(),
        tip: c.tip.clone(),
        approved: engine::approval_holds(c),
        review_rounds: c.review_rounds,
        review_cap: cap,
        smoketest_rounds: c.smoke_rounds,
        smoketest_cap: smoke_cap,
        error: c.error.clone(),
        missing,
    })
}

/// A change that waits on the user: Awaiting your Merge, Land failed, or a draft for review.
fn waits_on_user(c: &Change) -> bool {
    matches!(
        c.state.as_str(),
        "awaiting_merge" | "land_failed" | "handed_off"
    )
}

/// One task with its plan and changes, plus the other open tasks listed (≤ 8): the `status`
/// shape, shared by the orchestrator's `status` and the UI's `workflow_run_get`. The other tasks
/// that have a change waiting on the user are listed first, each with those changes.
fn run_view(
    tx: &Transaction<'_>,
    s: &WorkflowSettings,
    run: Option<&Run>,
    open: &[Run],
) -> rusqlite::Result<RunView> {
    let mut others: Vec<OtherTaskView> = Vec::new();
    for r in open.iter().filter(|r| Some(&r.id) != run.map(|x| &x.id)) {
        let all = store::changes(tx, &r.id)?;
        let waiting = all
            .iter()
            .filter(|c| waits_on_user(c))
            .map(|c| change_view(tx, s, r, c, &all))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        others.push(OtherTaskView {
            run_id: r.id.clone(),
            title: r.title.clone(),
            status: r.status.clone(),
            park_reason: r.park_reason.clone(),
            waiting,
        });
    }
    // Stable: tasks that wait on the user come first, so the cut at 8 never hides one.
    others.sort_by_key(|o| o.waiting.is_empty());
    others.truncate(8);
    let Some(run) = run else {
        return Ok(RunView {
            task: None,
            plan: None,
            changes: Vec::new(),
            other_tasks: others,
            guard: Vec::new(),
            board_queued: 0,
        });
    };
    let all = store::changes(tx, &run.id)?;
    let changes = all
        .iter()
        .map(|c| change_view(tx, s, run, c, &all))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let plan_cap = engine::cap(tx, s, run, CapStage::PlanReview, None)?;
    Ok(RunView {
        task: Some(RunTaskView {
            run_id: run.id.clone(),
            title: run.title.clone(),
            status: run.status.clone(),
            attempt: run.attempt,
            visual: run.visual,
            item_id: run.item_id.clone(),
            skipped: run.skipped.clone(),
        }),
        plan: Some(RunPlanView {
            id: run.plan_id.clone(),
            approved: run.plan_approved,
            signed_off: run.plan_signed_off,
            rounds: run.plan_rounds,
            cap: plan_cap,
            missing: engine::plan_missing(s, run),
        }),
        changes,
        other_tasks: others,
        guard: Vec::new(),
        board_queued: 0,
    })
}

fn run_json(
    tx: &Transaction<'_>,
    s: &WorkflowSettings,
    run: Option<&Run>,
    open: &[Run],
) -> rusqlite::Result<Value> {
    let v = run_view(tx, s, run, open)?;
    Ok(serde_json::to_value(v).expect("the run view serialises"))
}

/// `status`: the active task, plus the chat's other open tasks (≤ 8).
pub fn status(tx: &Transaction<'_>, x: &Act<'_>) -> Out {
    let open = store::open_runs(tx, x.chat_id)?;
    let run = store::active_run(tx, x.chat_id)?;
    let mut v = run_json(tx, x.s, run.as_ref(), &open)?;
    crate::postland::add_to_status(tx, x.chat_id, &mut v)?;
    Ok(Ok(v))
}

/// The UI's view of a chat's workflow (`workflow_run_get`): the `status` shape around the chat's
/// current task, which is the active task, else the newest open task with a change waiting on the
/// user (Awaiting your Merge, Land failed, or a draft for review), else the newest open task. So a
/// handed-off or parked task keeps its panel after the orchestrator moves on.
pub fn chat_view(
    tx: &Transaction<'_>,
    s: &WorkflowSettings,
    chat_id: &str,
) -> rusqlite::Result<Value> {
    let open = store::open_runs(tx, chat_id)?;
    let mut current = open.iter().find(|r| r.status == "active");
    if current.is_none() {
        for r in &open {
            if store::changes(tx, &r.id)?.iter().any(waits_on_user) {
                current = Some(r);
                break;
            }
        }
    }
    let current = current.or(open.first());
    let mut v = run_view(tx, s, current, &open)?;
    v.guard = crate::hook::needs_you(tx, chat_id)?;
    v.board_queued = crate::sa_outbox::waiting(tx, chat_id)?;
    Ok(serde_json::to_value(v).expect("the run view serialises"))
}

/// `update {repo, branch, pr?, base?, visual?}`: open or annotate a change; mark the task visual.
pub fn update(tx: &Transaction<'_>, x: &Act<'_>, a: &Value) -> Out {
    let mut run = match active(tx, x.chat_id)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    match a.get("visual").and_then(Value::as_bool) {
        Some(false) => return Ok(Err("Only the user can clear the visual flag.".into())),
        Some(true) if !run.visual => {
            run.visual = true;
            store::save_run(tx, &run, None, x.now)?;
            store::event(tx, x.chat_id, Some(&run), &ev("visual_set"), x.now)?;
        }
        _ => {}
    }
    if a.get("repo").is_none() && a.get("branch").is_none() {
        return Ok(Ok(json!({"runId": run.id, "visual": run.visual})));
    }
    let (repo, branch) = match target(a) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let (pr, base) = match (text(a, "pr", 512), text(a, "base", 255)) {
        (Ok(p), Ok(b)) => (p, b),
        (Err(e), _) | (_, Err(e)) => return Ok(Err(e)),
    };
    if let Some(b) = base {
        if !crate::settings::is_branch_name(b) {
            return Ok(Err("base is not a valid branch name.".into()));
        }
    }
    let existing = store::change(tx, &run.id, &repo, &branch)?;
    if existing.is_none() && store::changes(tx, &run.id)?.len() >= 32 {
        return Ok(Err("A task holds at most 32 changes.".into()));
    }
    if existing.is_none() {
        if let Some(why) = crate::claims::open_refusal(tx, &run, &repo, &branch, x.now)? {
            return Ok(Err(why));
        }
    }
    let mut c = existing.clone().unwrap_or(Change {
        run_id: run.id.clone(),
        repo: repo.clone(),
        branch: branch.clone(),
        state: "open".into(),
        ..Default::default()
    });
    let handed_now = c.handoff_attempt == Some(run.attempt) && c.state != "open";
    if let Some(p) = pr {
        c.pr = Some(p.to_owned());
    }
    let mut voided = false;
    if let Some(b) = base {
        if c.base.as_deref() != Some(b) {
            if handed_now {
                return Ok(Err(
                    "That change was handed off in this attempt and keeps its bound base.".into(),
                ));
            }
            voided = c.approved_tip.is_some();
            c.base = Some(b.to_owned());
            c.approved_tip = None;
            c.approved_tree = None;
            c.approved_base = None;
        }
    }
    store::upsert_change(tx, &c, x.now)?;
    if voided {
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                repo: Some(&repo),
                branch: Some(&branch),
                verdict: Some("approval_voided"),
                body: Some("Retargeted to a new base: it needs a new approval against that base."),
                ..ev("build_observed")
            },
            x.now,
        )?;
    }
    Ok(Ok(
        json!({"runId": run.id, "repo": repo, "branch": branch, "opened": existing.is_none(), "approvalVoided": voided}),
    ))
}

/// `skip {stage, reason}`: auto stages freely (recorded); required stages only with consent.
pub fn skip(tx: &Transaction<'_>, x: &Act<'_>, a: &Value) -> Out {
    skip_with(tx, x, a, Proof::from_args(a))
}

/// [`skip`] with the proof given: the agent's from its arguments, or the user's click.
pub fn skip_with(tx: &Transaction<'_>, x: &Act<'_>, a: &Value, proof: Proof) -> Out {
    let mut run = match active(tx, x.chat_id)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let stage = match need(a, "stage", 32) {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    let reason = match need(a, "reason", MAX_REASON) {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    let st = &x.s.stages;
    let use_ = match stage {
        "plan" => st.plan.use_,
        "planReview" => st.plan_review.use_,
        "review" => st.review.use_,
        "cleanup" => st.cleanup.use_,
        "smoketest" => st.smoketest.use_,
        _ => {
            return Ok(Err(
                "stage must be plan, planReview, review, cleanup or smoketest.".into(),
            ))
        }
    };
    if use_ == StageUse::Off {
        return Ok(Err(format!(
            "The {stage} stage is off; there is nothing to skip."
        )));
    }
    if stage == "smoketest" && run.visual {
        return Ok(Err("This task is marked visual, so its smoketest can't be skipped; only the user can clear the visual flag.".into()));
    }
    if engine::skipped(&run, stage) {
        return Ok(Ok(json!({"skipped": stage, "changed": false})));
    }
    let mut verified = false;
    if use_ == StageUse::Required {
        let t = json!({"stage": stage});
        let why = format!("The {stage} stage is required, so only the user can skip it.");
        if let Err(e) = kinds::consent(tx, x.kinds, x.chat_id, "skip", &t, &proof, &why, x.now)? {
            return Ok(Err(e));
        }
        verified = true;
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                verdict: Some("skip"),
                body: Some(reason),
                ..ev("consent")
            },
            x.now,
        )?;
    }
    run.skipped.push(stage.to_owned());
    store::save_run(tx, &run, None, x.now)?;
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            verdict: Some(stage),
            body: Some(reason),
            ..ev("skip")
        },
        x.now,
    )?;
    Ok(Ok(
        json!({"skipped": stage, "changed": true, "withConsent": verified}),
    ))
}

/// `park {reason}`.
pub fn park(tx: &Transaction<'_>, x: &Act<'_>, a: &Value) -> Out {
    let mut run = match active(tx, x.chat_id)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let reason = match need(a, "reason", MAX_REASON) {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    run.status = "parked".into();
    run.park_reason = Some(reason.to_owned());
    store::save_run(tx, &run, None, x.now)?;
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            body: Some(reason),
            ..ev("parked")
        },
        x.now,
    )?;
    Ok(Ok(json!({"runId": run.id, "status": "parked"})))
}

/// `drop {repo, branch, reason}` on the active or a parked task, or `drop {queued}` to discard a
/// queued follow-up.
pub fn drop(tx: &Transaction<'_>, x: &Act<'_>, a: &Value) -> Out {
    if a.get("queued").is_some() {
        return crate::postland::discard(tx, x, a);
    }
    let (repo, branch) = match target(a) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let reason = match need(a, "reason", MAX_REASON) {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    let mut found = None;
    for r in store::open_runs(tx, x.chat_id)? {
        if r.status == "active" || r.status == "parked" {
            if let Some(c) = store::change(tx, &r.id, &repo, &branch)? {
                found = Some((r, c));
                break;
            }
        }
    }
    let Some((run, mut c)) = found else {
        return Ok(Err(format!(
            "No active or parked task here holds {repo} {branch}."
        )));
    };
    let handed_now = c.handoff_attempt == Some(run.attempt) && c.state != "open";
    match c.state.as_str() {
        "landing" | "pending_land" => return Ok(Err("That change is being merged right now; wait until its land finishes or fails.".into())),
        "awaiting_merge" => return Ok(Err("That change waits for the user's own Merge, Decline or Drop change; Allow can't release it.".into())),
        "land_failed" if handed_now => return Ok(Err("That change's land failed in this attempt; only the user's Retry or Drop change releases it.".into())),
        s if engine::is_terminal(s) => return Ok(Ok(json!({"dropped": false, "state": s}))),
        _ => {}
    }
    if handed_now {
        let t = json!({"runId": run.id, "repo": repo, "branch": branch});
        let why = "That change was handed off in this attempt.";
        if let Err(e) = kinds::consent(
            tx,
            x.kinds,
            x.chat_id,
            "drop",
            &t,
            &Proof::from_args(a),
            why,
            x.now,
        )? {
            return Ok(Err(e));
        }
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                repo: Some(&repo),
                branch: Some(&branch),
                verdict: Some("drop"),
                ..ev("consent")
            },
            x.now,
        )?;
    }
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
            body: Some(reason),
            ..ev("change_dropped")
        },
        x.now,
    )?;
    Ok(Ok(json!({"dropped": true, "repo": repo, "branch": branch})))
}

/// `allowMore {stage, repo?, branch?, reason}` (one self-grant per target, stage and attempt, only
/// at the cap), or with `userQuote` / `grant` and `n` the user's own task-wide Allow more.
pub fn allow_more(tx: &Transaction<'_>, x: &Act<'_>, a: &Value) -> Out {
    allow_more_with(tx, x, a, Proof::from_args(a))
}

/// [`allow_more`] with the proof given: the agent's from its arguments, or the user's click.
pub fn allow_more_with(tx: &Transaction<'_>, x: &Act<'_>, a: &Value, proof: Proof) -> Out {
    let mut run = match active(tx, x.chat_id)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let Some(stage) = a
        .get("stage")
        .and_then(Value::as_str)
        .and_then(CapStage::parse)
    else {
        return Ok(Err("stage must be planReview, review or smoketest.".into()));
    };
    let use_ = match stage {
        CapStage::PlanReview => x.s.stages.plan_review.use_,
        CapStage::Review => x.s.stages.review.use_,
        CapStage::Smoketest => x.s.stages.smoketest.use_,
    };
    if use_ == StageUse::Off && !(stage == CapStage::Smoketest && run.visual) {
        return Ok(Err(format!("The {} stage is off.", stage.name())));
    }
    if proof.any() {
        if a.get("repo").is_some() || a.get("branch").is_some() {
            return Ok(Err(
                "The user's Allow more is task-wide: leave out repo and branch.".into(),
            ));
        }
        let n = a.get("n").and_then(Value::as_i64).unwrap_or(1);
        if !(1..=5).contains(&n) {
            return Ok(Err("n must be 1 to 5.".into()));
        }
        let t = json!({"stage": stage.name(), "n": n});
        if let Err(e) = kinds::consent(tx, x.kinds, x.chat_id, "allowMore", &t, &proof, "", x.now)?
        {
            return Ok(Err(e));
        }
        match stage {
            CapStage::PlanReview => run.plan_extra += n,
            CapStage::Review => run.review_extra += n,
            CapStage::Smoketest => run.smoke_extra += n,
        }
        store::save_run(tx, &run, None, x.now)?;
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                verdict: Some("allowMore"),
                round: Some(n),
                ..ev("consent")
            },
            x.now,
        )?;
        return Ok(Ok(json!({"stage": stage.name(), "added": n, "by": "user"})));
    }
    let reason = match need(a, "reason", MAX_REASON) {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    let (target, rounds, done) = if stage == CapStage::PlanReview {
        if a.get("repo").is_some() {
            return Ok(Err(
                "Lane plans are not in this build; leave out repo and branch for the task's plan."
                    .into(),
            ));
        }
        if run.plan_id.is_none() {
            return Ok(Err("The task has no plan of record.".into()));
        }
        (None, run.plan_rounds, run.plan_approved)
    } else {
        let (repo, branch) = match target(a) {
            Ok(t) => t,
            Err(e) => return Ok(Err(e)),
        };
        let Some(c) = store::change(tx, &run.id, &repo, &branch)? else {
            return Ok(Err(format!("The task has no change {repo} {branch}.")));
        };
        let (rounds, done) = if stage == CapStage::Review {
            (c.review_rounds, engine::approval_holds(&c))
        } else {
            (c.smoke_rounds, c.tip.is_some() && c.smoke_pass_tip == c.tip)
        };
        (Some((repo, branch)), rounds, done)
    };
    let tref = target.as_ref().map(|(r, b)| (r.as_str(), b.as_str()));
    if done {
        return Ok(Err(
            "That target is already approved or passed; it needs no more rounds.".into(),
        ));
    }
    let cap = engine::cap(tx, x.s, &run, stage, tref)?;
    if rounds < cap {
        return Ok(Err(format!(
            "That target is not at its cap ({rounds} of {cap} rounds used)."
        )));
    }
    if engine::self_grants(tx, &run, stage, tref, Some(run.attempt))? > 0 {
        return Ok(Err("You already gave this target its one extra round in this attempt; only the user can allow more now. Stop that loop and explain the blocker.".into()));
    }
    let what = tref
        .map(|(_, b)| format!("on {b}"))
        .unwrap_or_else(|| "on the task's plan".into());
    let row = format!(
        "The orchestrator gave itself 1 more {} round {what}, past your cap {cap}: {reason}",
        stage.name()
    );
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            repo: tref.map(|t| t.0),
            branch: tref.map(|t| t.1),
            verdict: Some(stage.name()),
            body: Some(&row),
            ..ev("cap_self_granted")
        },
        x.now,
    )?;
    Ok(Ok(
        json!({"stage": stage.name(), "added": 1, "by": "orchestrator", "row": row}),
    ))
}
