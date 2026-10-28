//! The pull request view's Merge, Ready for review and Convert to draft buttons, and what the
//! merge box needs to know about a PR (`pr_land_info`). Main window only: these are ops, never
//! host tools, so no agent reaches them, and each runs once per user click.
//!
//! Everything goes through the land step's own [`Gh`] under the user's `gh` login.
//!
//! - **One read first.** Each write reads the PR once: it must be open, and a merge is pinned to
//!   the head the page showed (`[HeadMoved]` otherwise; nothing merged).
//! - **Tracked PRs.** While Settings → Orchestrator is on, a change of an active, parked or
//!   handed-off task that records the PR (or works on its head branch, when the head lives in the
//!   same repository) holds it until its land step has finished:
//!   - Merge on a change awaiting the user's Merge, at the approved head, is handed to the task's
//!     own Merge (`routed`); its land step merges it with every check intact.
//!   - Ready for review on a change handed off as a draft runs the task's own Ready for review.
//!   - Any other state is an overridable `[Tracked]` refusal built from the host's own facts. The
//!     page may repeat the click with `anyway`.
//!   - While the land step is working on the change: `[TrackedBusy]`, no override. A failed
//!     lookup: `[TrackedUnknown]`, no override.
//! - **Anyway** takes the change's land-step hold, re-reads it under the hold, writes, and only on
//!   GitHub's confirmed success records a `pr_view_override` row (listed under Needs you) and
//!   moves the change (a merge: landed; a ready flip of a handed-off change: flipped).
//! - **Direct merges** (untracked, or anyway) obey the repository's landing rule. A repository
//!   whose own rule is draft is merged by hand, never here, and `anyway` does not change that.
//!   Otherwise the rule's method, or `landing.method` when the repository has no rule.
//! - **Convert to draft** is never refused as tracked: it writes under the change's hold and
//!   leaves the task alone (its land step marks the PR ready again if it lands it).
//!
//! Refusals are `[Class] sentence` strings.

use crate::engine;
use crate::gh::{Gh, GhError};
use crate::land::{self, Flight};
use crate::store::{self, Change, Event, Run};
use crate::types::*;
use helm_core::db::Database;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::time::Duration;

/// How many times a merge is read back before it is reported as `accepted`.
pub const READBACK_TRIES: u32 = 5;
/// The pause between read-backs in the app.
pub const READBACK_PAUSE: Duration = Duration::from_secs(2);

/// One pull request on github.com, checked for shape before anything runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    pub fn new(owner: &str, repo: &str, number: u64) -> Result<Self, String> {
        let owner_ok = !owner.is_empty()
            && owner.len() <= 39
            && !owner.starts_with('-')
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        let repo_ok = !repo.is_empty()
            && repo.len() <= 100
            && repo != "."
            && repo != ".."
            && repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        if !owner_ok || !repo_ok || number == 0 || number > 999_999_999 {
            return Err("[Invalid] That is not a github.com pull request.".into());
        }
        Ok(Self {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            number,
        })
    }
    /// `owner/name` for `gh --repo`.
    fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
    /// The workflow's repository key, `github.com/owner/name` lowercased.
    pub fn key(&self) -> String {
        format!("github.com/{}", self.slug()).to_ascii_lowercase()
    }
    fn label(&self) -> String {
        format!("{}#{}", self.slug(), self.number)
    }
    /// A recorded PR URL names this pull request.
    fn is(&self, url: &str) -> bool {
        let lower = url.trim().trim_end_matches('/').to_ascii_lowercase();
        let want = format!("https://{}/pull/{}", self.key(), self.number);
        lower == want
    }
}

/// What the one read of the PR said.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pr {
    state: String,
    draft: bool,
    head: String,
    head_ref: Option<String>,
    base: String,
    merge_sha: Option<String>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
}

fn args(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| x.to_string()).collect()
}

fn first_line(e: &str) -> String {
    e.trim()
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}

/// `nothing` finishes each refusal ("nothing was merged", "it was not marked ready", ...).
fn read(gh: &dyn Gh, pr: &PrRef, nothing: &str) -> Result<Pr, String> {
    let n = pr.number.to_string();
    let out = gh.run(&args(&[
        "pr",
        "view",
        &n,
        "--repo",
        &pr.slug(),
        "--json",
        "state,isDraft,headRefOid,headRefName,baseRefName,isCrossRepository,mergeCommit",
    ]));
    let v: Value = match out {
        Ok(o) if o.ok => serde_json::from_str(&o.stdout).map_err(|_| {
            format!("[GitHub] gh gave an unreadable answer about the pull request; {nothing}.")
        })?,
        Ok(o) => {
            return Err(format!(
                "[GitHub] GitHub could not read the pull request ({}); {nothing}.",
                first_line(&o.stderr)
            ))
        }
        Err(GhError::Timeout) => {
            return Err(format!(
                "[GitHub] gh timed out reading the pull request; {nothing}."
            ))
        }
        Err(GhError::Missing(m)) => {
            return Err(format!(
                "[GitHub] gh could not be started ({m}); {nothing}. Install the GitHub CLI and sign in with it."
            ))
        }
    };
    let cross = v
        .get("isCrossRepository")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok(Pr {
        state: s(&v, "state"),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        head: s(&v, "headRefOid"),
        head_ref: (!cross)
            .then(|| s(&v, "headRefName"))
            .filter(|h| !h.is_empty()),
        base: s(&v, "baseRefName"),
        merge_sha: v
            .get("mergeCommit")
            .map(|m| s(m, "oid"))
            .filter(|x| !x.is_empty()),
    })
}

fn closed(pr: &Pr, nothing: &str) -> Result<(), String> {
    match pr.state.as_str() {
        "OPEN" => Ok(()),
        "MERGED" => Err(format!(
            "[Closed] This pull request is already merged; {nothing}."
        )),
        _ => Err(format!("[Closed] This pull request is closed; {nothing}.")),
    }
}

// ---- which task tracks a PR -------------------------------------------------------------------

/// The task change that tracks a pull request.
#[derive(Clone, Debug)]
pub struct Tracking {
    pub run: Run,
    pub change: Change,
    /// Not finished landing: it holds the PR back.
    pub blocking: bool,
}

/// The land step is acting on the change or about to.
fn busy_state(state: &str) -> bool {
    matches!(state, "pending_land" | "landing" | "landing_draft")
}

/// The change of an active, parked or handed-off task that tracks the PR: one that records its
/// URL, or works on its head branch when the head is in the same repository. Among changes that
/// hold the PR back the earliest claimant wins; otherwise the latest of the rest.
pub fn tracking(
    tx: &Transaction<'_>,
    pr: &PrRef,
    head_branch: Option<&str>,
) -> rusqlite::Result<Option<Tracking>> {
    let mut st = tx.prepare(
        "SELECT c.run_id, c.repo, c.branch FROM workflow_changes c
           JOIN workflow_runs r ON r.id = c.run_id
          WHERE lower(c.repo) = ?1 AND r.status IN ('active', 'parked', 'handed_off')
          ORDER BY c.rowid",
    )?;
    let keys = st
        .query_map([pr.key()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut rest = None;
    for (run_id, repo, branch) in keys {
        let (Some(change), Some(run)) = (
            store::change(tx, &run_id, &repo, &branch)?,
            store::run(tx, &run_id)?,
        ) else {
            continue;
        };
        let by_url = change.pr.as_deref().is_some_and(|u| pr.is(u));
        let by_branch = head_branch.is_some_and(|h| h == change.branch);
        if !by_url && !by_branch {
            continue;
        }
        let blocking = !engine::is_terminal(&change.state);
        let t = Tracking {
            run,
            change,
            blocking,
        };
        if blocking {
            return Ok(Some(t));
        }
        rest = Some(t);
    }
    Ok(rest)
}

/// A task title as the page may show it: no control or bidi characters, at most 80 characters.
fn clean_title(t: &str) -> String {
    let clean: String = t
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c as u32, 0x200E..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069)
        })
        .collect();
    let clean = clean.trim();
    if clean.chars().count() > 80 {
        format!("{}…", clean.chars().take(79).collect::<String>())
    } else {
        clean.to_owned()
    }
}

fn state_words(state: &str) -> &'static str {
    match state {
        "open" => "is still being worked on",
        "landing_draft" => "is being handed off as a draft",
        "handed_off" => "was handed off as a draft for your review",
        "awaiting_merge" => "is waiting for your Merge",
        "land_failed" => "failed to land",
        "pending_land" | "landing" => "is being landed",
        _ => "is tracked by it",
    }
}

fn routes(c: &Change) -> Option<String> {
    match c.state.as_str() {
        "awaiting_merge" => Some("merge".into()),
        "handed_off" => Some("ready".into()),
        _ => None,
    }
}

const UNKNOWN: &str =
    "[TrackedUnknown] Helm could not check whether a task tracks this pull request";

fn busy_text(t: &Tracking, nothing: &str) -> String {
    format!(
        "[TrackedBusy] The task \"{}\" is landing {} right now; try again once it finishes. {}.",
        clean_title(&t.run.title),
        t.change.branch,
        capital(nothing)
    )
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// The overridable refusal, from host facts only.
fn tracked_text(t: &Tracking, head: &str, why: Option<&str>, anyway: &str) -> String {
    let approved = t.change.approved_tip.as_deref() == Some(head);
    let facts = match why {
        Some(w) => format!(
            "the task could not take it: {}",
            w.trim_end_matches('.')
                .chars()
                .take(300)
                .collect::<String>()
        ),
        None => format!(
            "its change {} {}, and the head {} the commit review approved",
            t.change.branch,
            state_words(&t.change.state),
            if approved { "is" } else { "is not" }
        ),
    };
    format!(
        "[Tracked] The task \"{}\" tracks this pull request: {facts}. {anyway}",
        clean_title(&t.run.title)
    )
}

// ---- pr_land_info -----------------------------------------------------------------------------

/// The repository's rule as the PR view applies it: an explicit draft rule refuses, otherwise
/// the rule's method or `landing.method`.
pub fn rule(st: &WorkflowSettings, pr: &PrRef) -> PrRule {
    match st.landing.repos.iter().find(|r| r.repo == pr.key()) {
        Some(r) => PrRule {
            mode: r.mode,
            method: r.method.unwrap_or(st.landing.method),
            explicit: true,
        },
        None => PrRule {
            mode: LandMode::Merge,
            method: st.landing.method,
            explicit: false,
        },
    }
}

pub fn info(
    db: &Database,
    flight: &Flight,
    pr: &PrRef,
    head_branch: Option<&str>,
) -> Result<PrLandInfo, String> {
    let (st, t) = db
        .snapshot(|tx| {
            Ok::<_, rusqlite::Error>((crate::settings::load(tx)?, tracking(tx, pr, head_branch)?))
        })
        .map_err(|_| format!("{UNKNOWN}."))?;
    Ok(PrLandInfo {
        repo: pr.key(),
        workflow_on: st.enabled,
        rule: rule(&st, pr),
        tracked: t.map(|t| PrTracked {
            busy: busy_state(&t.change.state) || flight.holds(&land::key(&t.change)),
            blocking: t.blocking && st.enabled,
            routes: routes(&t.change),
            run_id: t.run.id.clone(),
            chat_id: t.run.chat_id.clone(),
            title: clean_title(&t.run.title),
            status: t.run.status.clone(),
            repo: t.change.repo.clone(),
            branch: t.change.branch.clone(),
            state: t.change.state.clone(),
            approved_tip: t.change.approved_tip.clone(),
        }),
    })
}

// ---- the writes -------------------------------------------------------------------------------

/// How a write call is set up: `gh`, the hold set, the read-back pause and the clock.
pub struct Ctx<'a> {
    pub db: &'a Database,
    pub gh: &'a dyn Gh,
    pub flight: &'a Flight,
    pub pause: Duration,
    pub now: i64,
}

fn result(outcome: PrOutcome) -> PrWriteResult {
    PrWriteResult {
        outcome,
        method: None,
        merged_sha: None,
        run_id: None,
        branch: None,
        state: None,
        overridden: false,
        record_error: None,
    }
}

/// The tracking change that holds the PR back, or `None` when nothing does (the workflow is off,
/// no task tracks it, or its change has finished).
fn holder(cx: &Ctx<'_>, pr: &PrRef, head_branch: Option<&str>) -> Result<Option<Tracking>, String> {
    let (st, t) = cx
        .db
        .snapshot(|tx| {
            Ok::<_, rusqlite::Error>((crate::settings::load(tx)?, tracking(tx, pr, head_branch)?))
        })
        .map_err(|_| String::new())?;
    Ok(t.filter(|t| t.blocking && st.enabled))
}

/// Run `write` under the change's land-step hold, re-reading the change under it: a change that
/// no longer holds the PR back makes it an ordinary write (`None`), a different change or one the
/// land step took meanwhile is busy. The hold is always released and the land step woken.
fn under_hold<T>(
    cx: &Ctx<'_>,
    t: &Tracking,
    pr: &PrRef,
    head_branch: Option<&str>,
    nothing: &str,
    write: impl FnOnce(Option<&Tracking>) -> Result<T, String>,
) -> Result<T, String> {
    let k = land::key(&t.change);
    if !cx.flight.take(&k) {
        return Err(busy_text(t, nothing));
    }
    let out = (|| {
        let again = holder(cx, pr, head_branch).map_err(|_| format!("{UNKNOWN}; {nothing}."))?;
        match &again {
            Some(a) if land::key(&a.change) != k || busy_state(&a.change.state) => {
                Err(busy_text(a, nothing))
            }
            _ => write(again.as_ref()),
        }
    })();
    cx.flight.give(&k);
    land::kick();
    out
}

/// Record a write that went past a task: one `pr_view_override` row (listed under Needs you),
/// the change moved (a merge: landed; a ready flip of a handed-off change: flipped) and a note in
/// the task's chat. Returns the change's state afterwards.
fn record(
    cx: &Ctx<'_>,
    t: &Tracking,
    pr: &PrRef,
    action: &str,
    head: &str,
    merged_sha: Option<&str>,
) -> rusqlite::Result<String> {
    let state = cx.db.write(|tx| {
        let Some(mut c) = store::change(tx, &t.run.id, &t.change.repo, &t.change.branch)? else {
            return Ok(t.change.state.clone());
        };
        let run = store::run(tx, &t.run.id)?.unwrap_or_else(|| t.run.clone());
        let was = c.state.clone();
        let approved = c.approved_tip.clone();
        let words = if action == "merge" {
            "Merge anyway"
        } else {
            "Mark ready anyway"
        };
        let body = json!({
            "gate": "pr",
            "command": format!("{words} · {}", pr.label()),
            "verified": false,
            "action": action,
            "origin": "pr_view",
            "was": was,
            "head": head,
            "approvedTip": approved,
            "headApproved": approved.as_deref() == Some(head),
            "sha": merged_sha,
        })
        .to_string();
        store::event(
            tx,
            &run.chat_id,
            Some(&run),
            &Event {
                kind: "pr_view_override",
                repo: Some(&c.repo),
                branch: Some(&c.branch),
                verdict: Some(action),
                tip: Some(head),
                body: Some(&body),
                ..Default::default()
            },
            cx.now,
        )?;
        let note = if action == "merge" && !engine::is_terminal(&c.state) {
            // Landed outside the land step: no state edge leads here, so it is written as is.
            c.state = "landed".into();
            c.landed_sha = merged_sha.map(str::to_owned);
            c.error = None;
            c.failed_mode = None;
            store::upsert_change(tx, &c, cx.now)?;
            land::clear_wait(tx, &c)?;
            store::event(
                tx,
                &run.chat_id,
                Some(&run),
                &Event {
                    kind: "land_result",
                    repo: Some(&c.repo),
                    branch: Some(&c.branch),
                    verdict: Some("landed"),
                    tip: merged_sha,
                    body: Some("Merged from the pull request view with Merge anyway."),
                    ..Default::default()
                },
                cx.now,
            )?;
            Some(format!(
                "You merged {} ({}) from the pull request view with Merge anyway, past this task's land step. The task records it as landed.",
                c.branch,
                pr.label()
            ))
        } else if action == "ready" && c.state == "handed_off" {
            if engine::move_change(tx, &run, &mut c, "flipped", false, cx.now)?.is_ok() {
                store::event(
                    tx,
                    &run.chat_id,
                    Some(&run),
                    &Event {
                        kind: "flipped",
                        repo: Some(&c.repo),
                        branch: Some(&c.branch),
                        tip: Some(head),
                        body: Some("Marked ready from the pull request view with Mark ready anyway."),
                        ..Default::default()
                    },
                    cx.now,
                )?;
            }
            Some(format!(
                "You marked {} ready for review from the pull request view with Mark ready anyway.",
                pr.label()
            ))
        } else {
            Some(format!(
                "You used {words} on {} from the pull request view while this task's change {} {}; the change is left as it is.",
                pr.label(),
                c.branch,
                state_words(&c.state)
            ))
        };
        if let Some(text) = note {
            crate::notes::queue(
                &run.chat_id,
                text,
                false,
                format!("wf-prview-{}-{}", run.id, cx.now),
            );
        }
        Ok::<_, rusqlite::Error>(c.state)
    })?;
    if action == "merge" {
        // The wrap-up and the post-land due check, as after the land step's own merge.
        if let Err(e) = crate::postland::after_step(cx.db, &t.change, cx.now) {
            log::warn!("post-land step for {}: {e}", t.change.branch);
        }
    }
    Ok(state)
}

/// One merge by the repository's rule, pinned to `head`, then read back.
fn direct_merge(
    cx: &Ctx<'_>,
    pr: &PrRef,
    read_pr: &Pr,
    head: &str,
) -> Result<PrWriteResult, String> {
    const NOTHING: &str = "nothing was merged";
    let st = cx
        .db
        .snapshot(crate::settings::load)
        .map_err(|e| format!("[GitHub] Helm could not read its settings ({e}); {NOTHING}."))?;
    let r = rule(&st, pr);
    if r.mode == LandMode::Draft {
        return Err(format!(
            "[DraftRule] This repository's landing rule is draft: it is merged by hand on GitHub, never from Helm. {}.",
            capital(NOTHING)
        ));
    }
    if read_pr.draft {
        return Err(format!(
            "[Draft] This pull request is still a draft: mark it ready for review first. {}.",
            capital(NOTHING)
        ));
    }
    let slug = pr.slug();
    let n = pr.number.to_string();
    match land::method_flag(r.method) {
        Some(flag) => match cx.gh.run(&args(&[
            "pr",
            "merge",
            &n,
            "--repo",
            &slug,
            flag,
            "--match-head-commit",
            head,
        ])) {
            Ok(o) if o.ok => {}
            Ok(o) => {
                return Err(format!(
                    "[Refused] GitHub refused the merge: {}. {}.",
                    first_line(&o.stderr),
                    capital(NOTHING)
                ))
            }
            Err(GhError::Timeout) => {
                return Err(
                    "[Uncertain] gh timed out during the merge, so it may have gone through; refresh to check."
                        .into(),
                )
            }
            Err(GhError::Missing(m)) => {
                return Err(format!(
                    "[GitHub] gh could not be started ({m}); {NOTHING}."
                ))
            }
        },
        None => match land::fast_forward(cx.gh, &slug, &read_pr.base, head) {
            Ok(()) => {}
            Err(land::Ff::NotAncestor) => {
                return Err(format!(
                    "[NotFastForward] {} has commits this pull request does not contain, so it cannot be fast-forwarded: update the branch first. {}.",
                    read_pr.base,
                    capital(NOTHING)
                ))
            }
            Err(land::Ff::Unreadable(why)) => {
                return Err(format!(
                    "[GitHub] Helm could not check the branches ({why}); {NOTHING}."
                ))
            }
            Err(land::Ff::Refused(e)) => {
                return Err(format!(
                    "[Refused] GitHub refused the fast-forward: {e}. {}.",
                    capital(NOTHING)
                ))
            }
            Err(land::Ff::Gh(GhError::Timeout)) => {
                return Err(
                    "[Uncertain] gh timed out moving the branch, so it may have gone through; refresh to check."
                        .into(),
                )
            }
            Err(land::Ff::Gh(GhError::Missing(m))) => {
                return Err(format!(
                    "[GitHub] gh could not be started ({m}); {NOTHING}."
                ))
            }
        },
    }
    // It counts only once GitHub reads it back merged.
    for i in 0..READBACK_TRIES {
        if i > 0 {
            std::thread::sleep(cx.pause);
        }
        if let Ok(after) = read(cx.gh, pr, NOTHING) {
            if after.state == "MERGED" {
                return Ok(PrWriteResult {
                    method: Some(r.method),
                    merged_sha: after.merge_sha,
                    ..result(PrOutcome::Merged)
                });
            }
        }
    }
    Ok(PrWriteResult {
        method: Some(r.method),
        ..result(PrOutcome::Accepted)
    })
}

/// Merge from the PR view: `sha` is the head the page showed.
pub fn merge(cx: &Ctx<'_>, pr: &PrRef, sha: &str, anyway: bool) -> Result<PrWriteResult, String> {
    const NOTHING: &str = "nothing was merged";
    if sha.len() != 40 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("[Invalid] The head commit must be 40 lowercase hex characters.".into());
    }
    let p = read(cx.gh, pr, NOTHING)?;
    closed(&p, NOTHING)?;
    if p.head != sha {
        return Err(format!(
            "[HeadMoved] New commits since you looked; refresh first. {}.",
            capital(NOTHING)
        ));
    }
    let head_branch = p.head_ref.as_deref();
    let Some(t) = holder(cx, pr, head_branch).map_err(|_| format!("{UNKNOWN}; {NOTHING}."))? else {
        return direct_merge(cx, pr, &p, sha);
    };
    if busy_state(&t.change.state) || cx.flight.holds(&land::key(&t.change)) {
        return Err(busy_text(&t, NOTHING));
    }
    let method = rule(
        &cx.db
            .snapshot(crate::settings::load)
            .map_err(|_| format!("{UNKNOWN}; {NOTHING}."))?,
        pr,
    )
    .method;
    let anyway_words = format!(
        "Merge anyway merges it here ({}) and records it on the task as landed outside its land step. {}.",
        method_words(method),
        capital(NOTHING)
    );
    if !anyway {
        if t.change.state == "awaiting_merge" && t.change.approved_tip.as_deref() == Some(sha) {
            let routed = cx
                .db
                .write(|tx| {
                    land::user_action(
                        tx,
                        &t.run.id,
                        &t.change.repo,
                        &t.change.branch,
                        "merge",
                        cx.now,
                    )
                })
                .map_err(|_| format!("{UNKNOWN}; {NOTHING}."))?;
            return match routed {
                Ok(state) => {
                    land::kick();
                    Ok(PrWriteResult {
                        run_id: Some(t.run.id.clone()),
                        branch: Some(t.change.branch.clone()),
                        state: Some(state),
                        ..result(PrOutcome::Routed)
                    })
                }
                Err(why) => Err(tracked_text(&t, sha, Some(&why), &anyway_words)),
            };
        }
        return Err(tracked_text(&t, sha, None, &anyway_words));
    }
    under_hold(cx, &t, pr, head_branch, NOTHING, |held| {
        let mut out = direct_merge(cx, pr, &p, sha)?;
        if let (Some(held), PrOutcome::Merged) = (held, out.outcome) {
            out.overridden = true;
            out.run_id = Some(held.run.id.clone());
            out.branch = Some(held.change.branch.clone());
            match record(cx, held, pr, "merge", sha, out.merged_sha.as_deref()) {
                Ok(state) => out.state = Some(state),
                Err(e) => out.record_error = Some(e.to_string()),
            }
        }
        Ok(out)
    })
}

fn method_words(m: MergeMethod) -> &'static str {
    match m {
        MergeMethod::Merge => "a merge commit",
        MergeMethod::Squash => "squashed",
        MergeMethod::Rebase => "rebased",
        MergeMethod::FastForward => "fast-forwarded",
    }
}

fn gh_write(gh: &dyn Gh, a: &[&str], what: &str, nothing: &str) -> Result<(), String> {
    match gh.run(&args(a)) {
        Ok(o) if o.ok => Ok(()),
        Ok(o) => Err(format!(
            "[Refused] GitHub refused it: {}. {}.",
            first_line(&o.stderr),
            capital(nothing)
        )),
        Err(GhError::Timeout) => Err(format!(
            "[Uncertain] gh timed out while {what}, so it may have gone through; refresh to check."
        )),
        Err(GhError::Missing(m)) => Err(format!(
            "[GitHub] gh could not be started ({m}); {nothing}."
        )),
    }
}

/// Ready for review from the PR view.
pub fn ready(cx: &Ctx<'_>, pr: &PrRef, anyway: bool) -> Result<PrWriteResult, String> {
    const NOTHING: &str = "it was not marked ready";
    let p = read(cx.gh, pr, NOTHING)?;
    closed(&p, NOTHING)?;
    if !p.draft {
        return Ok(result(PrOutcome::AlreadyReady));
    }
    let n = pr.number.to_string();
    let slug = pr.slug();
    let write = || {
        gh_write(
            cx.gh,
            &["pr", "ready", &n, "--repo", &slug],
            "marking it ready",
            NOTHING,
        )
    };
    let head_branch = p.head_ref.as_deref();
    let Some(t) = holder(cx, pr, head_branch).map_err(|_| format!("{UNKNOWN}; {NOTHING}."))? else {
        write()?;
        return Ok(result(PrOutcome::Ready));
    };
    if busy_state(&t.change.state) || cx.flight.holds(&land::key(&t.change)) {
        return Err(busy_text(&t, NOTHING));
    }
    let anyway_words = format!(
        "Mark ready anyway marks it ready here and records it on the task. {}.",
        capital(NOTHING)
    );
    if !anyway {
        if t.change.state == "handed_off" {
            let r = land::ready(
                cx.db,
                cx.gh,
                &t.run.id,
                &t.change.repo,
                &t.change.branch,
                cx.now,
            )
            .map_err(|_| format!("{UNKNOWN}; {NOTHING}."))?;
            return match r {
                Ok(state) => Ok(PrWriteResult {
                    run_id: Some(t.run.id.clone()),
                    branch: Some(t.change.branch.clone()),
                    state: Some(state),
                    ..result(PrOutcome::Routed)
                }),
                Err(why) => Err(tracked_text(&t, &p.head, Some(&why), &anyway_words)),
            };
        }
        return Err(tracked_text(&t, &p.head, None, &anyway_words));
    }
    under_hold(cx, &t, pr, head_branch, NOTHING, |held| {
        write()?;
        let mut out = result(PrOutcome::Ready);
        if let Some(held) = held {
            out.overridden = true;
            out.run_id = Some(held.run.id.clone());
            out.branch = Some(held.change.branch.clone());
            match record(cx, held, pr, "ready", &p.head, None) {
                Ok(state) => out.state = Some(state),
                Err(e) => out.record_error = Some(e.to_string()),
            }
        }
        Ok(out)
    })
}

/// Convert to draft from the PR view: never refused as tracked, written under the change's hold.
pub fn draft(cx: &Ctx<'_>, pr: &PrRef) -> Result<PrWriteResult, String> {
    const NOTHING: &str = "it was not converted";
    let p = read(cx.gh, pr, NOTHING)?;
    closed(&p, NOTHING)?;
    if p.draft {
        return Ok(result(PrOutcome::AlreadyDraft));
    }
    let n = pr.number.to_string();
    let slug = pr.slug();
    let write = || {
        gh_write(
            cx.gh,
            &["pr", "ready", &n, "--repo", &slug, "--undo"],
            "converting it to draft",
            NOTHING,
        )
    };
    let head_branch = p.head_ref.as_deref();
    match holder(cx, pr, head_branch).map_err(|_| format!("{UNKNOWN}; {NOTHING}."))? {
        None => {
            write()?;
            Ok(result(PrOutcome::Converted))
        }
        Some(t) => under_hold(cx, &t, pr, head_branch, NOTHING, |held| {
            write()?;
            Ok(match held {
                Some(h) => PrWriteResult {
                    run_id: Some(h.run.id.clone()),
                    branch: Some(h.change.branch.clone()),
                    state: Some(h.change.state.clone()),
                    ..result(PrOutcome::Tracked)
                },
                None => result(PrOutcome::Converted),
            })
        }),
    }
}
