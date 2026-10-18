//! The task and change rules the host enforces.
//!
//! Pure checks over [`store`] rows: the change edges, the caps, what a change still needs before
//! hand-off, and approval binding. The tool actions ([`crate::actions`], [`crate::reports`]) call
//! these inside one transaction each.

use crate::store::{self, Change, Event, Run};
use crate::types::*;
use rusqlite::Transaction;

pub const TERMINAL: [&str; 4] = ["landed", "flipped", "dropped", "declined"];

pub fn is_terminal(state: &str) -> bool {
    TERMINAL.contains(&state)
}

/// Whether the host lets the change move `from` → `to` (no
/// box edges, no ghost holds). `with_handoff` is true when the move comes with a hand-off.
pub fn edge_allowed(
    from: Option<&str>,
    to: &str,
    with_handoff: bool,
    failed_mode: Option<&str>,
) -> bool {
    match (from, to) {
        (None, "open") => true,
        (Some("open"), "landing_draft" | "pending_land") => with_handoff,
        (Some("land_failed"), "landing_draft") => with_handoff && failed_mode == Some("draft"),
        (Some("landing_draft"), "handed_off" | "land_failed") => true,
        (Some("pending_land" | "land_failed"), "landing" | "awaiting_merge") => true,
        (Some("pending_land"), "land_failed") => true,
        (Some("landing"), "landed" | "land_failed" | "awaiting_merge") => true,
        (Some("awaiting_merge"), "landing" | "declined" | "land_failed") => true,
        (Some("handed_off"), "flipped") => true,
        (Some("handed_off" | "land_failed" | "declined" | "awaiting_merge"), "open") => true,
        (
            Some(
                "open" | "landing_draft" | "handed_off" | "pending_land" | "awaiting_merge"
                | "land_failed",
            ),
            "dropped",
        ) => true,
        _ => false,
    }
}

/// Move a change, refusing (and recording a conflict row) on an edge the host does not allow.
pub fn move_change(
    tx: &Transaction<'_>,
    run: &Run,
    c: &mut Change,
    to: &str,
    with_handoff: bool,
    now: i64,
) -> rusqlite::Result<Result<(), String>> {
    if !edge_allowed(Some(&c.state), to, with_handoff, c.failed_mode.as_deref()) {
        let body = format!(
            "refused move of {} {} from {} to {}",
            c.repo, c.branch, c.state, to
        );
        store::event(
            tx,
            &run.chat_id,
            Some(run),
            &Event {
                kind: "conflict",
                repo: Some(&c.repo),
                branch: Some(&c.branch),
                body: Some(&body),
                ..Default::default()
            },
            now,
        )?;
        return Ok(Err(format!(
            "{} can't move from {} to {}.",
            c.branch, c.state, to
        )));
    }
    c.state = to.to_owned();
    if to != "land_failed" {
        c.failed_mode = None;
    }
    store::upsert_change(tx, c, now)?;
    Ok(Ok(()))
}

/// A stage key for caps and self-grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapStage {
    PlanReview,
    Review,
    Smoketest,
}

impl CapStage {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "planReview" => Some(Self::PlanReview),
            "review" => Some(Self::Review),
            "smoketest" => Some(Self::Smoketest),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::PlanReview => "planReview",
            Self::Review => "review",
            Self::Smoketest => "smoketest",
        }
    }
}

/// The self-grants (`allowMore` without consent) for a target, across every attempt (grants are
/// cumulative).
pub fn self_grants(
    tx: &Transaction<'_>,
    run: &Run,
    stage: CapStage,
    target: Option<(&str, &str)>,
    attempt: Option<i64>,
) -> rusqlite::Result<i64> {
    store::count_events(
        tx,
        &run.id,
        "cap_self_granted",
        attempt,
        target.map(|t| t.0),
        target.map(|t| t.1),
        Some(stage.name()),
    )
}

/// The cap for a target now: the setting, plus the user's task-wide extra, plus self-grants.
pub fn cap(
    tx: &Transaction<'_>,
    s: &WorkflowSettings,
    run: &Run,
    stage: CapStage,
    target: Option<(&str, &str)>,
) -> rusqlite::Result<i64> {
    let (base, extra) = match stage {
        CapStage::PlanReview => (s.stages.plan_review.max_rounds, run.plan_extra),
        CapStage::Review => (s.stages.review.max_rounds, run.review_extra),
        CapStage::Smoketest => (s.stages.smoketest.max_rounds, run.smoke_extra),
    };
    Ok(base as i64 + extra + self_grants(tx, run, stage, target, None)?)
}

pub fn skipped(run: &Run, stage: &str) -> bool {
    run.skipped.iter().any(|x| x == stage)
}

fn needed(u: StageUse, run: &Run, stage: &str) -> bool {
    u == StageUse::Required || (u == StageUse::Auto && !skipped(run, stage))
}

/// What the task's plan still needs before a build counts.
pub fn plan_missing(s: &WorkflowSettings, run: &Run) -> Option<String> {
    if needed(s.stages.plan.use_, run, "plan") && run.plan_id.is_none() {
        return Some("the task has no plan of record yet; adopt one with app_workflow plan".into());
    }
    if run.plan_id.is_some() || needed(s.stages.plan.use_, run, "plan") {
        if needed(s.stages.plan_review.use_, run, "planReview") && !run.plan_approved {
            return Some("the plan of record is not approved by a plan review yet".into());
        }
        if s.plan_sign_off == SignOff::Me && !run.plan_signed_off {
            return Some("the plan waits for the user's sign-off".into());
        }
    }
    None
}

/// Whether the change's approval holds at its latest commit: same tip, or same tree.
pub fn approval_holds(c: &Change) -> bool {
    match (&c.approved_tip, &c.tip) {
        (Some(a), Some(t)) if a == t => true,
        _ => matches!((&c.approved_tree, &c.tree), (Some(a), Some(t)) if a == t),
    }
}

/// What one open change still needs before it can be handed off. Empty means it holds.
pub fn missing(s: &WorkflowSettings, run: &Run, c: &Change, all: &[Change]) -> Vec<String> {
    let mut out = Vec::new();
    let name = &c.branch;
    if let Some(p) = plan_missing(s, run) {
        out.push(format!("{name}: {p}"));
    }
    let Some(tip) = &c.tip else {
        out.push(format!("{name}: no build has been reported yet"));
        return out;
    };
    if s.stages.review.use_ == StageUse::Required {
        if !approval_holds(c) {
            out.push(format!(
                "{name}: no review approval at its latest commit {}",
                &tip[..7.min(tip.len())]
            ));
        }
    } else {
        let build_code = c.build_tip.as_ref() == Some(tip)
            || matches!((&c.build_tree, &c.tree), (Some(a), Some(b)) if a == b);
        if !build_code {
            out.push(format!(
                "{name}: its latest commit is not the build agent's code"
            ));
        }
    }
    if c.pr.is_some()
        && needed(s.stages.cleanup.use_, run, "cleanup")
        && c.cleanup_tip.as_ref() != Some(tip)
    {
        out.push(format!("{name}: no clean-up report at its latest commit"));
    }
    if c.smoke_fail_tip.as_ref() == Some(tip) {
        out.push(format!("{name}: a smoketest failed at its latest commit"));
    } else if (s.stages.smoketest.use_ == StageUse::Required || run.visual)
        && c.smoke_pass_tip.as_ref() != Some(tip)
    {
        out.push(format!("{name}: no smoketest pass at its latest commit"));
    }
    if let Some(base) = &c.base {
        let (mode, _) = crate::settings::land_mode(s, &c.repo);
        let stacked = all.iter().any(|o| {
            o.repo == c.repo && &o.branch == base && !is_terminal(&o.state) && o.branch != c.branch
        });
        if stacked && mode == LandMode::Merge {
            out.push(format!("{name}: stacked on {base}; wait for it to land, then retarget onto a protected branch"));
        }
    }
    out
}

/// Turning a chat's Orchestrator off parks its active task in the same transaction.
pub fn park_for_off(tx: &Transaction<'_>, chat_id: &str, now: i64) -> rusqlite::Result<bool> {
    let Some(mut run) = store::active_run(tx, chat_id)? else {
        return Ok(false);
    };
    run.status = "parked".into();
    run.park_reason = Some("Orchestrator was turned off for this chat".into());
    store::save_run(tx, &run, None, now)?;
    store::event(
        tx,
        chat_id,
        Some(&run),
        &Event {
            kind: "parked",
            body: run.park_reason.as_deref(),
            ..Default::default()
        },
        now,
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_contract_edges_are_allowed() {
        assert!(edge_allowed(None, "open", false, None));
        assert!(!edge_allowed(None, "landed", false, None));
        assert!(
            !edge_allowed(Some("open"), "pending_land", false, None),
            "needs a hand-off"
        );
        assert!(edge_allowed(Some("open"), "pending_land", true, None));
        assert!(edge_allowed(
            Some("land_failed"),
            "landing_draft",
            true,
            Some("draft")
        ));
        assert!(!edge_allowed(
            Some("land_failed"),
            "landing_draft",
            true,
            Some("merge")
        ));
        assert!(!edge_allowed(Some("open"), "landed", true, None));
        assert!(!edge_allowed(Some("landed"), "open", false, None));
        assert!(
            !edge_allowed(Some("landing"), "dropped", false, None),
            "never mid-land"
        );
        assert!(
            !edge_allowed(Some("dropped"), "open", false, None),
            "a drop is final"
        );
    }

    fn change(tip: &str, tree: &str) -> Change {
        Change {
            repo: "github.com/o/r".into(),
            branch: "feat-x".into(),
            tip: Some(tip.into()),
            tree: Some(tree.into()),
            build_tip: Some(tip.into()),
            build_tree: Some(tree.into()),
            state: "open".into(),
            ..Default::default()
        }
    }

    fn run() -> Run {
        Run {
            id: "r".into(),
            chat_id: "c".into(),
            title: "t".into(),
            item_id: None,
            visual: false,
            attempt: 1,
            status: "active".into(),
            park_reason: None,
            plan_id: Some("p".into()),
            plan_approved: true,
            plan_signed_off: false,
            plan_rounds: 1,
            plan_extra: 0,
            review_extra: 0,
            smoke_extra: 0,
            skipped: vec![],
        }
    }

    #[test]
    fn an_approval_holds_only_at_its_tip_or_an_identical_tree() {
        let mut c = change("a", "t1");
        c.approved_tip = Some("a".into());
        c.approved_tree = Some("t1".into());
        assert!(approval_holds(&c));
        c.tip = Some("b".into());
        c.tree = Some("t1".into());
        assert!(approval_holds(&c), "same tree");
        c.tree = Some("t2".into());
        assert!(!approval_holds(&c));
        c.tree = None;
        assert!(!approval_holds(&c), "an unknown tree counts as different");
    }

    #[test]
    fn missing_names_each_unmet_stage() {
        let s = WorkflowSettings::default();
        let mut c = change("a", "t1");
        c.pr = Some("https://github.com/o/r/pull/3".into());
        let m = missing(&s, &run(), &c, &[]);
        assert!(m.iter().any(|x| x.contains("no review approval")), "{m:?}");
        assert!(m.iter().any(|x| x.contains("clean-up")), "{m:?}");
        c.approved_tip = Some("a".into());
        c.cleanup_tip = Some("a".into());
        assert!(missing(&s, &run(), &c, &[]).is_empty());
        let mut r = run();
        r.visual = true;
        assert!(missing(&s, &r, &c, &[])
            .iter()
            .any(|x| x.contains("smoketest pass")));
        c.smoke_fail_tip = Some("a".into());
        assert!(missing(&s, &run(), &c, &[])
            .iter()
            .any(|x| x.contains("failed")));
        let mut unplanned = run();
        unplanned.plan_approved = false;
        c.smoke_fail_tip = None;
        assert!(missing(&s, &unplanned, &c, &[])
            .iter()
            .any(|x| x.contains("not approved")));
    }

    #[test]
    fn skipping_an_auto_plan_lifts_the_plan_requirement() {
        let mut s = WorkflowSettings::default();
        s.stages.plan.use_ = StageUse::Auto;
        s.stages.plan_review.use_ = StageUse::Auto;
        let mut r = run();
        r.plan_id = None;
        r.plan_approved = false;
        assert!(plan_missing(&s, &r).is_some());
        r.skipped = vec!["plan".into()];
        assert_eq!(plan_missing(&s, &r), None);
    }
}
