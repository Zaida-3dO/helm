//! The stage agents' report tool, `app_workflow_report`.
//!
//! Rules (the per-agent hook attribution waits for the later hook wiring):
//! - only a sub-agent may report; the orchestrator itself never does;
//! - each `plan_review`, `code_review` and `visual_review` report is one round of its loop;
//! - the tree of a reported tip is read by the host from git **before** the transaction and
//!   never taken from the agent; an unknown tree counts as a different tree;
//! - unknown fields are refused, a tip is a full 40-character sha, a repo is `host/owner/name`.

use crate::actions::{self, Act, Out};
use crate::engine::{self, CapStage};
use crate::git::is_sha;
use crate::store::{self, Change, Event};
use crate::types::*;
use rusqlite::Transaction;
use serde::Deserialize;
use serde_json::{json, Value};

pub const MAX_SUMMARY: usize = 8000;
pub const MAX_FOLLOW_UP: usize = 300;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Report {
    pub kind: String,
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub tip: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub pr: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub findings: Option<Vec<Value>>,
    #[serde(default)]
    pub follow_ups: Option<Vec<String>>,
    #[serde(default)]
    pub evidence: Option<Vec<Value>>,
}

/// Parse and shape-check a report. A refusal here is *correctable*: nothing was recorded.
pub fn parse(args: Value) -> Result<Report, String> {
    let r: Report = serde_json::from_value(args).map_err(|e| {
        format!("Correctable: {e}. Nothing was recorded; fix the report and send it again.")
    })?;
    let bad = |m: &str| {
        Err(format!(
            "Correctable: {m}. Nothing was recorded; fix the report and send it again."
        ))
    };
    if r.summary.chars().count() > MAX_SUMMARY {
        return bad("summary is longer than 8000 characters");
    }
    if let Some(t) = &r.tip {
        if !is_sha(t) {
            return bad("tip must be the full 40-character lowercase commit sha");
        }
    }
    if r.findings.as_ref().is_some_and(|f| f.len() > 50) {
        return bad("findings holds at most 50 entries");
    }
    if r.follow_ups.as_ref().is_some_and(|f| f.len() > 20) {
        return bad("followUps holds at most 20 entries");
    }
    if r.evidence.as_ref().is_some_and(|f| f.len() > 20) {
        return bad("evidence holds at most 20 entries");
    }
    let (verdicts, needs): (&[&str], &[&str]) = match r.kind.as_str() {
        "plan_review" => (&["approve", "changes_required"], &[]),
        "build" => (&[], &["repo", "branch", "tip"]),
        "code_review" => (
            &["lgtm", "lgtm_with_nits", "changes_required"],
            &["repo", "branch", "tip", "base"],
        ),
        "cleanup" => (&[], &["repo", "branch", "tip", "pr"]),
        "visual_review" => (&["pass", "fail", "pending"], &["repo", "branch"]),
        "after_land" => {
            if r.repo.is_some()
                || r.branch.is_some()
                || r.tip.is_some()
                || r.base.is_some()
                || r.pr.is_some()
            {
                return bad("an after_land report names no change: leave out repo, branch, tip, base and pr");
            }
            (&crate::postland::AFTER_LAND[..], &[][..])
        }
        _ => return bad(
            "kind must be plan_review, build, code_review, cleanup, visual_review or after_land",
        ),
    };
    match (&r.verdict, verdicts.is_empty()) {
        (Some(_), true) => return bad(&format!("a {} report has no verdict", r.kind)),
        (None, false) => return bad(&format!("verdict is required: {}", verdicts.join(", "))),
        (Some(v), false) if !verdicts.contains(&v.as_str()) => {
            return bad(&format!("verdict must be {}", verdicts.join(", ")))
        }
        _ => {}
    }
    for f in needs {
        let present = match *f {
            "repo" => r.repo.is_some(),
            "branch" => r.branch.is_some(),
            "tip" => r.tip.is_some(),
            "base" => r.base.is_some(),
            "pr" => r.pr.is_some(),
            _ => true,
        };
        if !present {
            return bad(&format!("a {} report needs {f}", r.kind));
        }
    }
    Ok(r)
}

/// The reported change: repo, branch and its row if it exists.
type Target = (String, String, Option<Change>);

fn change_of(
    tx: &Transaction<'_>,
    run_id: &str,
    r: &Report,
) -> rusqlite::Result<Result<Target, String>> {
    let a = json!({"repo": r.repo, "branch": r.branch});
    let (repo, branch) = match actions::target(&a) {
        Ok(t) => t,
        Err(e) => return Ok(Err(format!("Correctable: {e}"))),
    };
    let c = store::change(tx, run_id, &repo, &branch)?;
    Ok(Ok((repo, branch, c)))
}

pub fn follow_ups(r: &Report) -> Vec<String> {
    r.follow_ups
        .iter()
        .flatten()
        .map(|f| {
            f.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(MAX_FOLLOW_UP)
                .collect()
        })
        .collect()
}

/// Record a report. `is_child` comes from the host-tool channel, `tree` from git.
pub fn report(
    tx: &Transaction<'_>,
    x: &Act<'_>,
    is_child: bool,
    tree: Option<String>,
    r: Report,
) -> Out {
    if !is_child {
        return Ok(Err("Only a stage agent the orchestrator dispatched can report; the orchestrator never reports itself.".into()));
    }
    if r.kind == "after_land" {
        let body = json!({"summary": r.summary, "followUps": follow_ups(&r)}).to_string();
        return crate::postland::after_land_report(tx, x, &r, &body);
    }
    let mut run = match actions::active(tx, x.chat_id)? {
        Ok(run) => run,
        Err(_) => {
            return Ok(Err(
                "Permanent: this chat has no active task, so the report does not count.".into(),
            ))
        }
    };
    let st = &x.s.stages;
    let verdict = r.verdict.clone();
    let body = json!({"summary": r.summary, "findings": r.findings, "followUps": follow_ups(&r), "evidence": r.evidence}).to_string();
    if r.kind == "plan_review" {
        if st.plan_review.use_ == StageUse::Off {
            return Ok(Err("Permanent: plan review is off.".into()));
        }
        let Some(pid) = run.plan_id.clone() else {
            return Ok(Err(
                "Permanent: the task has no plan of record to review.".into()
            ));
        };
        let cap = engine::cap(tx, x.s, &run, CapStage::PlanReview, None)?;
        if run.plan_rounds >= cap {
            return Ok(Err(format!(
                "Permanent: the plan is at its review cap ({cap})."
            )));
        }
        run.plan_rounds += 1;
        let approved = verdict.as_deref() == Some("approve");
        if approved {
            run.plan_approved = true;
        }
        store::save_run(tx, &run, None, x.now)?;
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                kind: "plan_round",
                round: Some(run.plan_rounds),
                verdict: verdict.as_deref(),
                plan_id: Some(&pid),
                body: Some(&body),
                ..Default::default()
            },
            x.now,
        )?;
        return Ok(Ok(
            json!({"recorded": true, "round": run.plan_rounds, "cap": cap, "planApproved": run.plan_approved}),
        ));
    }
    let (repo, branch, existing) = match change_of(tx, &run.id, &r)? {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let tip = r.tip.clone();
    let mut c = match (&r.kind[..], existing) {
        ("build", None) => {
            if store::changes(tx, &run.id)?.len() >= 32 {
                return Ok(Err("Permanent: a task holds at most 32 changes.".into()));
            }
            if let Some(why) = crate::claims::open_refusal(tx, &run, &repo, &branch, x.now)? {
                return Ok(Err(format!("Permanent: {why} Tell the orchestrator.")));
            }
            Change {
                run_id: run.id.clone(),
                repo: repo.clone(),
                branch: branch.clone(),
                state: "open".into(),
                ..Default::default()
            }
        }
        (_, None) => {
            return Ok(Err(format!(
                "Permanent: the task has no change {repo} {branch}."
            )))
        }
        (_, Some(c)) => c,
    };
    if c.state != "open" {
        if matches!(r.kind.as_str(), "build" | "cleanup") && c.handoff_attempt == Some(run.attempt)
        {
            store::event(tx, x.chat_id, Some(&run), &Event {
                kind: "conflict", repo: Some(&repo), branch: Some(&branch), tip: tip.as_deref(),
                body: Some("a commit was reported on a change already handed off in this attempt; the approval stays bound to the handed-off commit"),
                ..Default::default()
            }, x.now)?;
        }
        return Ok(Err(format!(
            "Permanent: {branch} is {} and takes no reports.",
            c.state
        )));
    }
    let ev = |kind: &'static str, verdict: Option<&str>, round: Option<i64>| {
        (kind, verdict.map(str::to_owned), round)
    };
    let (kind, ev_verdict, round) = match r.kind.as_str() {
        "build" => {
            if let Some(why) = engine::plan_missing(x.s, &run) {
                store::event(
                    tx,
                    x.chat_id,
                    Some(&run),
                    &Event {
                        kind: "gate_denied",
                        repo: Some(&repo),
                        branch: Some(&branch),
                        verdict: Some("build"),
                        body: Some(&why),
                        ..Default::default()
                    },
                    x.now,
                )?;
                return Ok(Err(format!(
                    "Permanent: the build gate refuses this build: {why}."
                )));
            }
            c.tip = tip.clone();
            c.tree = tree.clone();
            c.build_tip = tip.clone();
            c.build_tree = tree.clone();
            if r.base.is_some() {
                c.base = r.base.clone();
            }
            if r.pr.is_some() {
                c.pr = r.pr.clone();
            }
            let voided = c.approved_tip.is_some() && !engine::approval_holds(&c);
            if voided {
                c.approved_tip = None;
                c.approved_tree = None;
                c.approved_base = None;
            }
            ev("build_observed", voided.then_some("approval_voided"), None)
        }
        "code_review" => {
            if st.review.use_ == StageUse::Off {
                return Ok(Err("Permanent: code review is off.".into()));
            }
            let cap = engine::cap(tx, x.s, &run, CapStage::Review, Some((&repo, &branch)))?;
            if c.review_rounds >= cap {
                return Ok(Err(format!(
                    "Permanent: {branch} is at its review cap ({cap})."
                )));
            }
            c.review_rounds += 1;
            let at_latest = c.tip.is_some() && c.tip == tip;
            let approves = matches!(verdict.as_deref(), Some("lgtm" | "lgtm_with_nits"));
            let outcome = if approves && at_latest {
                c.approved_tip = tip.clone();
                c.approved_tree = c.tree.clone().or(tree.clone());
                c.approved_base = r.base.clone();
                "approved"
            } else if !at_latest {
                "stale"
            } else {
                "changes_required"
            };
            ev("review_round", Some(outcome), Some(c.review_rounds))
        }
        "cleanup" => {
            let basis_tree = if st.review.use_ == StageUse::Required {
                c.approved_tree.clone()
            } else {
                c.build_tree.clone()
            };
            let kept = tree.is_some() && tree == basis_tree;
            c.tip = tip.clone();
            c.tree = tree.clone();
            c.pr = r.pr.clone();
            if kept {
                c.cleanup_tip = tip.clone();
            } else if c.approved_tip.is_some() {
                c.approved_tip = None;
                c.approved_tree = None;
                c.approved_base = None;
            }
            ev(
                "cleanup",
                Some(if kept { "kept_tree" } else { "tree_changed" }),
                None,
            )
        }
        _ => {
            // visual_review
            let Some(at) = c.tip.clone() else {
                return Ok(Err(format!(
                    "Permanent: {branch} has no commit to smoketest yet."
                )));
            };
            let cap = engine::cap(tx, x.s, &run, CapStage::Smoketest, Some((&repo, &branch)))?;
            if c.smoke_rounds >= cap {
                return Ok(Err(format!(
                    "Permanent: {branch} is at its smoketest cap ({cap})."
                )));
            }
            c.smoke_rounds += 1;
            match verdict.as_deref() {
                Some("pass") => c.smoke_pass_tip = Some(at),
                Some("fail") => c.smoke_fail_tip = Some(at),
                _ => {}
            }
            ev("visual_review", verdict.as_deref(), Some(c.smoke_rounds))
        }
    };
    store::upsert_change(tx, &c, x.now)?;
    // An approving review's follow-ups are work for after the change: queue them on the task.
    if kind == "review_round" && ev_verdict.as_deref() == Some("approved") {
        crate::postland::queue(tx, &run, &branch, &follow_ups(&r), x.now)?;
    }
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            kind,
            repo: Some(&repo),
            branch: Some(&branch),
            round,
            verdict: ev_verdict.as_deref(),
            tip: tip.as_deref(),
            tree: tree.as_deref(),
            body: Some(&body),
            ..Default::default()
        },
        x.now,
    )?;
    Ok(Ok(json!({
        "recorded": true, "kind": r.kind, "outcome": ev_verdict, "round": round,
        "approved": engine::approval_holds(&c), "treeKnown": tree.is_some(),
    })))
}
