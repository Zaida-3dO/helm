//! The land step. The app's own
//! code lands handed-off changes; agents never do.
//!
//! - **Mode** is recomputed from the host's settings at every pass, never read from a worker.
//! - **Merge rule**: re-read the PR; it must be open, its head the approved tip, its base the
//!   approved base and a protected branch. A conflict is final. While GitHub is still computing
//!   mergeability the change waits (every 10 s for 2 min, then every minute up to 15 min, then
//!   "Awaiting your Merge"). A draft is marked ready first. The merge is pinned with
//!   `--match-head-commit`, and it counts only when the PR then reads back MERGED into a
//!   protected branch. A repository rule may pick `fast_forward` instead: the base branch is
//!   moved to the approved head by a non-forced ref update ([`fast_forward`]), so GitHub writes
//!   no commit, and the same MERGED read-back decides.
//! - **Draft rule**: the PR must be open at the approved tip; draft labels are added (a label
//!   the repository lacks is skipped); a PR already ready for review is left alone.
//! - **One land per change at a time** (the flight set), and a merge in flight when Helm quit is
//!   a failed land at the next start, never retried silently.
//! - Nothing runs while the master switch is off.

use crate::engine;
use crate::gh::{gh_repo, pr_number, Gh, GhError};
use crate::store::{self, Change, Event, Run};
use crate::types::*;
use helm_core::db::Database;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

pub const FAST_WAIT_MS: i64 = 10_000;
pub const FAST_PHASE_MS: i64 = 2 * 60_000;
pub const SLOW_WAIT_MS: i64 = 60_000;
pub const BUDGET_MS: i64 = 15 * 60_000;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflow_land_waits (
             run_id     TEXT NOT NULL,
             repo       TEXT NOT NULL,
             branch     TEXT NOT NULL,
             started_at INTEGER NOT NULL,
             next_at    INTEGER NOT NULL,
             PRIMARY KEY (run_id, repo, branch)
         );",
    )
}

/// The changes a land driver is acting on right now.
#[derive(Default)]
pub struct Flight(Mutex<HashSet<String>>);

impl Flight {
    /// Hold a change (`run|repo|branch`); false when someone already holds it.
    pub(crate) fn take(&self, key: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_owned())
    }
    pub(crate) fn give(&self, key: &str) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }
    pub(crate) fn holds(&self, key: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(key)
    }
}

static KICK: OnceLock<Arc<tokio::sync::Notify>> = OnceLock::new();
static FLIGHT: OnceLock<Arc<Flight>> = OnceLock::new();

/// The app's one hold set, shared by the land driver and the PR view's buttons, so a click never
/// writes to a PR while the land step is acting on its change.
pub fn flight() -> Arc<Flight> {
    FLIGHT.get_or_init(|| Arc::new(Flight::default())).clone()
}

/// Wake the land driver now (after a hand-off or a user action).
pub fn kick() {
    if let Some(k) = KICK.get() {
        k.notify_one();
    }
}

/// What one pass decided for a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Landed(String),
    HandedOff(Option<String>),
    Wait(&'static str),
    Failed { mode: &'static str, why: String },
    Awaiting(String),
}

pub(crate) fn key(c: &Change) -> String {
    format!("{}|{}|{}", c.run_id, c.repo, c.branch)
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
}

fn view(gh: &dyn Gh, repo: &str, n: &str) -> Result<Value, Step> {
    let args: Vec<String> = [
        "pr",
        "view",
        n,
        "--repo",
        repo,
        "--json",
        "state,isDraft,headRefOid,baseRefName,mergeable,mergeStateStatus,mergeCommit",
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    match gh.run(&args) {
        Ok(o) if o.ok => {
            serde_json::from_str(&o.stdout).map_err(|_| Step::Wait("gh gave an unreadable answer"))
        }
        Ok(_) => Err(Step::Wait("gh could not read the PR")),
        Err(GhError::Timeout) => Err(Step::Wait("gh timed out")),
        Err(GhError::Missing(m)) => Err(Step::Failed {
            mode: "merge",
            why: format!("gh could not be started: {m}"),
        }),
    }
}

fn protected(s: &WorkflowSettings, base: &str) -> bool {
    s.landing.protected_branches.iter().any(|b| b == base)
}

/// One merge-rule pass over a change in `landing`.
pub fn merge_pass(gh: &dyn Gh, st: &WorkflowSettings, c: &Change) -> Step {
    let fail = |why: String| Step::Failed { mode: "merge", why };
    let (Some(pr), Some(tip)) = (c.pr.as_deref(), c.approved_tip.as_deref()) else {
        return fail("the change has no PR or approved commit recorded".into());
    };
    let (Some(repo), Some(n)) = (gh_repo(&c.repo), pr_number(pr)) else {
        return fail("only github.com pull requests are landed".into());
    };
    if !pr
        .to_ascii_lowercase()
        .contains(&format!("github.com/{}/pull/", repo.to_ascii_lowercase()))
    {
        return fail(format!(
            "the recorded PR is not a pull request of {}",
            c.repo
        ));
    }
    let base_ok = |b: &str| protected(st, b) && c.approved_base.as_deref().is_none_or(|ab| ab == b);
    let v = match view(gh, &repo, &n) {
        Ok(v) => v,
        Err(step) => return step,
    };
    let base = s(&v, "baseRefName");
    let merged_sha = v
        .get("mergeCommit")
        .map(|m| s(m, "oid"))
        .unwrap_or_default();
    match s(&v, "state").as_str() {
        "MERGED" if base_ok(&base) && s(&v, "headRefOid") == tip => {
            return Step::Landed(merged_sha)
        }
        "MERGED" => {
            return fail("the PR was merged with other code or into an unprotected branch".into())
        }
        "OPEN" => {}
        other => return fail(format!("the PR is {}", other.to_lowercase())),
    }
    if s(&v, "headRefOid") != tip {
        return fail("the PR's head moved since the approval; it needs a new review".into());
    }
    if let Some(ab) = &c.approved_base {
        if &base != ab {
            return fail(format!("the PR's base is {base}, not the approved {ab}"));
        }
    }
    if !protected(st, &base) {
        return fail(format!(
            "{base} is not a protected branch, so Helm does not merge into it"
        ));
    }
    match (
        s(&v, "mergeable").as_str(),
        s(&v, "mergeStateStatus").as_str(),
    ) {
        ("CONFLICTING", _) | (_, "DIRTY") => {
            return fail("the PR conflicts with its base: fix it and Retry, or Drop change".into())
        }
        ("UNKNOWN", _) | (_, "UNKNOWN") => {
            return Step::Wait("GitHub is still checking mergeability")
        }
        (_, "BLOCKED") => {
            return Step::Awaiting("required checks or reviews are not satisfied".into())
        }
        _ => {}
    }
    let a = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    if v.get("isDraft").and_then(Value::as_bool).unwrap_or(false) {
        match gh.run(&a(&["pr", "ready", &n, "--repo", &repo])) {
            Ok(o) if o.ok => {}
            _ => return Step::Wait("marking the draft ready did not go through yet"),
        }
    }
    let (_, method) = crate::settings::land_mode(st, &c.repo);
    let merged = match method_flag(method) {
        Some(flag) => gh.run(&a(&[
            "pr",
            "merge",
            &n,
            "--repo",
            &repo,
            flag,
            "--match-head-commit",
            tip,
        ])),
        None => match fast_forward(gh, &repo, &base, tip) {
            Ok(()) => Ok(crate::gh::Out {
                ok: true,
                stdout: String::new(),
                stderr: String::new(),
            }),
            Err(Ff::NotAncestor) => return fail(format!(
                "{base} has commits the approved head does not contain, so it cannot be fast-forwarded: rebase the branch, get it reviewed again, or Drop change"
            )),
            Err(Ff::Unreadable(why)) => return Step::Wait(why),
            Err(Ff::Refused(e)) => Ok(crate::gh::Out {
                ok: false,
                stdout: String::new(),
                stderr: e,
            }),
            Err(Ff::Gh(e)) => Err(e),
        },
    };
    let transient = match &merged {
        Ok(o) if o.ok => false,
        Ok(o) => {
            let e = o.stderr.to_lowercase();
            e.contains("not mergeable at the moment") || e.contains("base branch was modified")
        }
        Err(GhError::Timeout) => true,
        Err(_) => false,
    };
    // A merge counts only when the readback says MERGED (into a protected branch, at our head).
    match view(gh, &repo, &n) {
        Ok(after)
            if s(&after, "state") == "MERGED"
                && base_ok(&s(&after, "baseRefName"))
                && s(&after, "headRefOid") == tip =>
        {
            Step::Landed(
                after
                    .get("mergeCommit")
                    .map(|m| s(m, "oid"))
                    .unwrap_or_default(),
            )
        }
        Ok(after) if s(&after, "state") == "MERGED" => {
            fail("the PR read back merged with other code or into another branch".into())
        }
        _ if transient => Step::Wait("GitHub refused the merge for now"),
        Ok(_) if matches!(&merged, Ok(o) if o.ok && o.stdout.to_lowercase().contains("queue")) => {
            Step::Wait("queued for merge")
        }
        Ok(_) if matches!(&merged, Ok(o) if o.ok) => {
            Step::Wait("gh accepted the merge; waiting for GitHub to show it merged")
        }
        Ok(_) => fail(match merged {
            Ok(o) => format!(
                "the merge failed: {}",
                o.stderr.trim().chars().take(300).collect::<String>()
            ),
            Err(e) => format!("the merge failed: {e:?}"),
        }),
        Err(step) => step,
    }
}

/// The `gh pr merge` flag of a method; `None` for a fast-forward, which is a ref update instead.
pub fn method_flag(m: MergeMethod) -> Option<&'static str> {
    match m {
        MergeMethod::Merge => Some("--merge"),
        MergeMethod::Squash => Some("--squash"),
        MergeMethod::Rebase => Some("--rebase"),
        MergeMethod::FastForward => None,
    }
}

/// Why a fast-forward did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ff {
    /// The base is not an ancestor of the head (or GitHub said the update is not a
    /// fast-forward). Nothing was written.
    NotAncestor,
    /// The comparison could not be read; nothing was written.
    Unreadable(&'static str),
    /// GitHub refused the ref update (branch protection, permissions); nothing was written.
    Refused(String),
    /// `gh` timed out or could not start during the update: it may have gone through.
    Gh(GhError),
}

/// A branch name safe to put into a ref path: what git allows, minus anything that could
/// change the meaning of the API path.
fn plain_branch(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 255
        && !b.contains("..")
        && !b.starts_with('/')
        && !b.ends_with('/')
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'/'))
}

/// Fast-forward `base` of `owner/name` to `head`: first ask GitHub whether `base` is an ancestor
/// of `head` (refused before any write when it is not), then a **non-forced** ref update. GitHub
/// itself refuses an update that is not a fast-forward too, so the base can never be rewound or
/// rewritten. The caller reads the PR back to see it merged.
pub fn fast_forward(gh: &dyn Gh, repo: &str, base: &str, head: &str) -> Result<(), Ff> {
    let hex = head.len() == 40 && head.bytes().all(|b| b.is_ascii_hexdigit());
    if !plain_branch(base) || !hex {
        return Err(Ff::Refused(format!(
            "Helm does not fast-forward the branch {base:?}"
        )));
    }
    let a = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let compare = gh.run(&a(&[
        "api",
        &format!("repos/{repo}/compare/{base}...{head}"),
        "--jq",
        ".status",
    ]));
    match compare {
        Ok(o) if o.ok => match o.stdout.trim() {
            "ahead" | "identical" => {}
            "behind" | "diverged" => return Err(Ff::NotAncestor),
            _ => return Err(Ff::Unreadable("GitHub gave an unreadable comparison")),
        },
        Ok(_) => return Err(Ff::Unreadable("GitHub could not compare the branches")),
        Err(GhError::Timeout) => return Err(Ff::Unreadable("gh timed out")),
        Err(e) => return Err(Ff::Gh(e)),
    }
    let sha = format!("sha={head}");
    match gh.run(&a(&[
        "api",
        "-X",
        "PATCH",
        &format!("repos/{repo}/git/refs/heads/{base}"),
        "-f",
        &sha,
        "-F",
        "force=false",
    ])) {
        Ok(o) if o.ok => Ok(()),
        Ok(o) => {
            let e = format!("{} {}", o.stderr, o.stdout);
            if e.to_lowercase().contains("fast forward")
                || e.to_lowercase().contains("fast-forward")
            {
                Err(Ff::NotAncestor)
            } else {
                Err(Ff::Refused(e.trim().chars().take(300).collect()))
            }
        }
        Err(e) => Err(Ff::Gh(e)),
    }
}

/// One draft-rule pass over a change in `landing_draft`.
pub fn draft_pass(gh: &dyn Gh, st: &WorkflowSettings, c: &Change) -> Step {
    let fail = |why: String| Step::Failed { mode: "draft", why };
    let Some(pr) = c.pr.as_deref() else {
        return fail(
            "no PR is recorded: open a draft PR and record it with app_workflow update {pr}".into(),
        );
    };
    let (Some(repo), Some(n)) = (gh_repo(&c.repo), pr_number(pr)) else {
        return fail("only github.com pull requests are landed".into());
    };
    let v = match view(gh, &repo, &n) {
        Ok(v) => v,
        Err(Step::Failed { why, .. }) => return fail(why),
        Err(step) => return step,
    };
    if s(&v, "state") != "OPEN" {
        return fail(format!("the PR is {}", s(&v, "state").to_lowercase()));
    }
    if Some(s(&v, "headRefOid").as_str()) != c.approved_tip.as_deref() {
        return fail("the PR's head is not the approved commit".into());
    }
    if !v.get("isDraft").and_then(Value::as_bool).unwrap_or(false) {
        return Step::HandedOff(Some(
            "the PR was already ready for review, so Helm left it as it is".into(),
        ));
    }
    let mut skipped = Vec::new();
    for label in &st.landing.draft_labels {
        let args: Vec<String> = ["pr", "edit", &n, "--repo", &repo, "--add-label", label]
            .iter()
            .map(|x| x.to_string())
            .collect();
        if !matches!(gh.run(&args), Ok(o) if o.ok) {
            skipped.push(label.clone());
        }
    }
    Step::HandedOff((!skipped.is_empty()).then(|| {
        format!(
            "labels not added (the repository may lack them): {}",
            skipped.join(", ")
        )
    }))
}

fn wait_row(tx: &Transaction<'_>, c: &Change) -> rusqlite::Result<Option<(i64, i64)>> {
    tx.query_row(
        "SELECT started_at, next_at FROM workflow_land_waits WHERE run_id = ?1 AND repo = ?2 AND branch = ?3",
        params![c.run_id, c.repo, c.branch],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

pub(crate) fn clear_wait(tx: &Transaction<'_>, c: &Change) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM workflow_land_waits WHERE run_id = ?1 AND repo = ?2 AND branch = ?3",
        params![c.run_id, c.repo, c.branch],
    )?;
    Ok(())
}

fn result_event(
    tx: &Transaction<'_>,
    run: &Run,
    c: &Change,
    verdict: &str,
    tip: Option<&str>,
    body: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    store::event(
        tx,
        &run.chat_id,
        Some(run),
        &Event {
            kind: "land_result",
            repo: Some(&c.repo),
            branch: Some(&c.branch),
            verdict: Some(verdict),
            tip,
            body,
            ..Default::default()
        },
        now,
    )?;
    Ok(())
}

/// Write a step's outcome for a change, re-reading it first so a user's action in between wins.
pub fn apply(tx: &Transaction<'_>, before: &Change, step: &Step, now: i64) -> rusqlite::Result<()> {
    let Some(mut c) = store::change(tx, &before.run_id, &before.repo, &before.branch)? else {
        return Ok(());
    };
    if c.state != before.state || c.approved_tip != before.approved_tip {
        return Ok(());
    }
    let Some(run) = store::run(tx, &c.run_id)? else {
        return Ok(());
    };
    match step {
        Step::Landed(sha) => {
            c.landed_sha = Some(sha.clone()).filter(|s| !s.is_empty());
            c.error = None;
            if engine::move_change(tx, &run, &mut c, "landed", false, now)?.is_ok() {
                clear_wait(tx, &c)?;
                result_event(
                    tx,
                    &run,
                    &c,
                    "landed",
                    c.landed_sha.clone().as_deref(),
                    None,
                    now,
                )?;
            }
        }
        Step::HandedOff(note) => {
            c.error = note.clone();
            if engine::move_change(tx, &run, &mut c, "handed_off", false, now)?.is_ok() {
                result_event(
                    tx,
                    &run,
                    &c,
                    "handed_off",
                    c.approved_tip.clone().as_deref(),
                    note.as_deref(),
                    now,
                )?;
            }
        }
        Step::Failed { mode, why } => {
            c.error = Some(why.clone());
            c.failed_mode = Some((*mode).to_owned());
            if engine::move_change(tx, &run, &mut c, "land_failed", false, now)?.is_ok() {
                clear_wait(tx, &c)?;
                result_event(tx, &run, &c, "failed", None, Some(why), now)?;
            }
        }
        Step::Awaiting(why) => {
            c.error = Some(why.clone());
            if engine::move_change(tx, &run, &mut c, "awaiting_merge", false, now)?.is_ok() {
                clear_wait(tx, &c)?;
                store::event(
                    tx,
                    &run.chat_id,
                    Some(&run),
                    &Event {
                        kind: "awaiting_merge",
                        repo: Some(&c.repo),
                        branch: Some(&c.branch),
                        body: Some(why),
                        ..Default::default()
                    },
                    now,
                )?;
            }
        }
        Step::Wait(why) => {
            let started = wait_row(tx, &c)?.map(|w| w.0).unwrap_or(now);
            let spent = now - started;
            if spent >= BUDGET_MS && c.state == "landing_draft" {
                return apply(
                    tx,
                    &c,
                    &Step::Failed {
                        mode: "draft",
                        why: format!("the draft land could not finish within 15 minutes ({why})"),
                    },
                    now,
                );
            }
            if spent >= BUDGET_MS {
                return apply(
                    tx,
                    &c,
                    &Step::Awaiting(format!("still not mergeable after 15 minutes ({why})")),
                    now,
                );
            }
            let next = now
                + if spent < FAST_PHASE_MS {
                    FAST_WAIT_MS
                } else {
                    SLOW_WAIT_MS
                };
            tx.execute(
                "INSERT INTO workflow_land_waits(run_id, repo, branch, started_at, next_at) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(run_id, repo, branch) DO UPDATE SET next_at = excluded.next_at",
                params![c.run_id, c.repo, c.branch, started, next],
            )?;
        }
    }
    Ok(())
}

/// At start: a merge that was in flight when Helm quit becomes a failed land.
pub fn recover(db: &Database, now: i64) -> rusqlite::Result<usize> {
    db.write(|tx| {
        let stuck = store::changes_in(tx, &["landing"])?;
        for c in &stuck {
            apply(tx, c, &Step::Failed { mode: "merge", why: "a merge was in flight when Helm last quit; check the PR, then Retry or Drop change".into() }, now)?;
        }
        tx.execute("DELETE FROM workflow_land_waits", [])?;
        Ok(stuck.len())
    })
}

/// One pass of the land driver over every change that is due. Network calls happen outside any
/// transaction; each outcome is written in its own.
pub fn pass(db: &Database, gh: &dyn Gh, flight: &Flight, now: i64) -> rusqlite::Result<usize> {
    let (st, due) = db.snapshot(|tx| {
        let st = crate::settings::load(tx)?;
        let mut due = Vec::new();
        for c in store::changes_in(tx, &["pending_land", "landing", "landing_draft"])? {
            let ready =
                c.state == "pending_land" || wait_row(tx, &c)?.is_none_or(|(_, next)| next <= now);
            if ready {
                due.push(c);
            }
        }
        Ok::<_, rusqlite::Error>((st, due))
    })?;
    if !st.enabled {
        return Ok(0);
    }
    let mut done = 0;
    for mut c in due {
        let k = key(&c);
        if !flight.take(&k) {
            continue;
        }
        let (mode, _) = crate::settings::land_mode(&st, &c.repo);
        let step = match (c.state.as_str(), mode) {
            ("pending_land", LandMode::Draft) => Step::Failed {
                mode: "draft",
                why: "the repository's rule is now draft; Retry runs the draft land".into(),
            },
            ("pending_land", LandMode::Merge) => {
                // Route: pending_land → landing, then the first merge pass.
                let routed = db.write(|tx| {
                    let Some(run) = store::run(tx, &c.run_id)? else {
                        return Ok(false);
                    };
                    Ok::<_, rusqlite::Error>(
                        engine::move_change(tx, &run, &mut c, "landing", false, now)?.is_ok(),
                    )
                })?;
                if !routed {
                    flight.give(&k);
                    continue;
                }
                merge_pass(gh, &st, &c)
            }
            ("landing", LandMode::Draft) => Step::Failed {
                mode: "draft",
                why: "the repository's rule is now draft; Retry runs the draft land".into(),
            },
            ("landing", LandMode::Merge) => merge_pass(gh, &st, &c),
            _ => draft_pass(gh, &st, &c),
        };
        db.write(|tx| apply(tx, &c, &step, now))?;
        // The wrap-up and the post-land due check, outside the land's own transaction.
        if matches!(step, Step::Landed(_) | Step::HandedOff(_)) {
            if let Err(e) = crate::postland::after_step(db, &c, now) {
                log::warn!("post-land step for {}: {e}", c.branch);
            }
        }
        flight.give(&k);
        done += 1;
    }
    Ok(done)
}

/// Start the land driver: recover, then a pass every 10 s or whenever [`kick`] is called.
pub fn start_driver(handle: &tokio::runtime::Handle, ctx: &helm_ipc::OpCtx) {
    let Some(db) = ctx.services.get::<Database>() else {
        return;
    };
    let notify = KICK
        .get_or_init(|| Arc::new(tokio::sync::Notify::new()))
        .clone();
    let events = ctx.events.clone();
    let services = ctx.services.clone();
    handle.spawn(async move {
        let d = db.clone();
        // The chats each pass wrote run rows for are read on the pass's own thread.
        if let Ok(chats) = tokio::task::spawn_blocking(move || {
            let _ = recover(&d, helm_core::now_ms());
            store::take_touched()
        })
        .await
        {
            store::emit_chats(&*events, chats);
        }
        let flight = flight();
        loop {
            let (d, f) = (db.clone(), flight.clone());
            let rt = services.get::<helm_chats::ChatRuntime>();
            let chats = tokio::task::spawn_blocking(move || {
                if let Err(e) = pass(&d, &crate::gh::GhCli, &f, helm_core::now_ms()) {
                    log::warn!("land step: {e}");
                }
                // The notes a finished land wrote (the post-land due note) go out from here.
                let notes = crate::notes::take_pending();
                if let Some(rt) = rt {
                    crate::notes::deliver_pending(&d, &rt, notes);
                }
                store::take_touched()
            })
            .await
            .unwrap_or_default();
            store::emit_chats(&*events, chats);
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(FAST_WAIT_MS as u64)) => {}
                _ = notify.notified() => {}
            }
        }
    });
}

/// The user's actions on a handed-off change (the main window's task row):
/// `merge` an awaiting change, `retry` a failed land, `drop` it, or `decline` the merge (a
/// handed-off task parks). The land step re-verifies everything before any merge. Ready for
/// review is [`ready`], since it calls GitHub before it writes.
pub fn user_action(
    tx: &Transaction<'_>,
    run_id: &str,
    repo: &str,
    branch: &str,
    action: &str,
    now: i64,
) -> rusqlite::Result<Result<String, String>> {
    let Some(mut run) = store::run(tx, run_id)? else {
        return Ok(Err("No such task.".into()));
    };
    let Some(mut c) = store::change(tx, run_id, repo, branch)? else {
        return Ok(Err("That task has no such change.".into()));
    };
    let was = c.state.clone();
    let (to, with_handoff) = match (action, c.state.as_str()) {
        ("merge", "awaiting_merge") => ("landing", false),
        ("retry", "land_failed") if c.failed_mode.as_deref() == Some("draft") => {
            ("landing_draft", true)
        }
        ("retry", "land_failed") => ("landing", false),
        ("drop", "landing" | "pending_land") => {
            return Ok(Err(
                "That change is being merged right now; wait until its land finishes or fails."
                    .into(),
            ))
        }
        ("drop", s) if !engine::is_terminal(s) => ("dropped", false),
        ("decline", "awaiting_merge") => ("declined", false),
        (a, s) if matches!(a, "merge" | "retry" | "drop" | "decline") => {
            return Ok(Err(format!("{a} does not apply to a change that is {s}.")))
        }
        _ => return Ok(Err("action must be merge, retry, drop or decline.".into())),
    };
    c.error = None;
    if let Err(e) = engine::move_change(tx, &run, &mut c, to, with_handoff, now)? {
        return Ok(Err(e));
    }
    clear_wait(tx, &c)?;
    let kind = match action {
        "drop" => "change_dropped",
        "decline" => "merge_declined",
        _ => "land_result",
    };
    store::event(
        tx,
        &run.chat_id,
        Some(&run),
        &Event {
            kind,
            repo: Some(repo),
            branch: Some(branch),
            verdict: Some(action),
            ..Default::default()
        },
        now,
    )?;
    // Decline, or Drop change on an awaiting change, parks a handed-off task; an active task goes
    // on.
    let release = action == "decline" || (action == "drop" && was == "awaiting_merge");
    if release && run.status == "handed_off" {
        run.status = "parked".into();
        run.park_reason = Some(if action == "decline" {
            format!("You declined the merge of {branch}.")
        } else {
            format!("You dropped the change {branch}.")
        });
        store::save_run(tx, &run, None, now)?;
    }
    Ok(Ok(c.state))
}

/// One Ready-for-review pass over a handed-off change: the PR must be open at the
/// approved commit; a draft is marked ready and the flip label removed. `Ok` carries a note for
/// the change (a label that could not be removed), `Err` why it was not flipped.
pub fn flip_pass(gh: &dyn Gh, st: &WorkflowSettings, c: &Change) -> Result<Option<String>, String> {
    let (Some(pr), Some(tip)) = (c.pr.as_deref(), c.approved_tip.as_deref()) else {
        return Err("the change has no PR or approved commit recorded".into());
    };
    let (Some(repo), Some(n)) = (gh_repo(&c.repo), pr_number(pr)) else {
        return Err("only github.com pull requests are handled".into());
    };
    let v = match view(gh, &repo, &n) {
        Ok(v) => v,
        Err(Step::Failed { why, .. }) => return Err(why),
        Err(Step::Wait(w)) => return Err(format!("GitHub could not be read ({w}); try again")),
        Err(_) => return Err("GitHub could not be read; try again".into()),
    };
    if s(&v, "state") != "OPEN" {
        return Err(format!("the PR is {}", s(&v, "state").to_lowercase()));
    }
    if s(&v, "headRefOid") != tip {
        return Err("the PR's head is not the approved commit".into());
    }
    let run = |args: &[&str]| {
        let a: Vec<String> = args.iter().map(|x| x.to_string()).collect();
        matches!(gh.run(&a), Ok(o) if o.ok)
    };
    if v.get("isDraft").and_then(Value::as_bool).unwrap_or(false)
        && !run(&["pr", "ready", &n, "--repo", &repo])
    {
        return Err("marking the PR ready for review did not go through; try again".into());
    }
    if let Some(label) = st.landing.flip_label.as_deref() {
        if !run(&["pr", "edit", &n, "--repo", &repo, "--remove-label", label]) {
            return Ok(Some(format!("the label {label} could not be removed")));
        }
    }
    Ok(None)
}

/// The user's Ready for review on a handed-off change. The steps are:
///
/// 1. Read the change, the settings and the chat's Orchestrator state (a snapshot, no lock held).
///    The chat must have Orchestrator on, as for Drop change and a draft Retry.
/// 2. [`flip_pass`], which talks to GitHub, with no transaction open and no lock held.
/// 3. The write, which re-reads the change and the chat's state first: an action or a switch-off
///    in between wins. The caller must therefore not hold the global maintenance lock; the op is
///    a plain blocking one.
///
/// A failed check leaves the change handed off with the reason on it and a `flip_verify_failed`
/// row.
pub fn ready(
    db: &Database,
    gh: &dyn Gh,
    run_id: &str,
    repo: &str,
    branch: &str,
    now: i64,
) -> rusqlite::Result<Result<String, String>> {
    let (st, run, c, chat_on) = db.snapshot(|tx| {
        let st = crate::settings::load(tx)?;
        let run = store::run(tx, run_id)?;
        let chat_on = match &run {
            Some(r) => crate::mode::in_effect(tx, &r.chat_id, &st)?,
            None => false,
        };
        Ok::<_, rusqlite::Error>((st, run, store::change(tx, run_id, repo, branch)?, chat_on))
    })?;
    let Some(run) = run else {
        return Ok(Err("No such task.".into()));
    };
    let status_before = run.status;
    let Some(c) = c else {
        return Ok(Err("That task has no such change.".into()));
    };
    if !st.enabled {
        return Ok(Err(
            "Orchestrator is off in Settings, so Helm marks nothing ready.".into(),
        ));
    }
    if !chat_on {
        return Ok(Err(crate::user::CHAT_OFF.into()));
    }
    if c.state != "handed_off" {
        return Ok(Err(format!(
            "ready does not apply to a change that is {}.",
            c.state
        )));
    }
    let outcome = flip_pass(gh, &st, &c);
    db.write(|tx| {
        let (Some(mut now_c), Some(run)) = (
            store::change(tx, run_id, repo, branch)?,
            store::run(tx, run_id)?,
        ) else {
            return Ok(Err("That task has no such change.".into()));
        };
        // The chat or the master switch may have been turned off while GitHub was asked.
        let now_st = crate::settings::load(tx)?;
        if !now_st.enabled {
            return Ok(Err(
                "Orchestrator is off in Settings, so Helm marks nothing ready.".into(),
            ));
        }
        if !crate::mode::in_effect(tx, &run.chat_id, &now_st)? {
            return Ok(Err(crate::user::CHAT_OFF.into()));
        }
        if run.status != status_before {
            return Ok(Err(format!(
                "That task changed while Helm was checking {branch}; it is {} now.",
                run.status
            )));
        }
        if now_c.state != "handed_off" || now_c.approved_tip != c.approved_tip {
            return Ok(Err(format!(
                "{branch} changed while Helm was checking it; it is {} now.",
                now_c.state
            )));
        }
        match outcome {
            Ok(note) => {
                now_c.error = note.clone();
                if let Err(e) = engine::move_change(tx, &run, &mut now_c, "flipped", false, now)? {
                    return Ok(Err(e));
                }
                store::event(
                    tx,
                    &run.chat_id,
                    Some(&run),
                    &Event {
                        kind: "flipped",
                        repo: Some(repo),
                        branch: Some(branch),
                        tip: now_c.approved_tip.as_deref(),
                        body: note.as_deref(),
                        ..Default::default()
                    },
                    now,
                )?;
                Ok(Ok(now_c.state))
            }
            Err(why) => {
                now_c.error = Some(format!("Not marked ready: {why}."));
                store::upsert_change(tx, &now_c, now)?;
                store::event(
                    tx,
                    &run.chat_id,
                    Some(&run),
                    &Event {
                        kind: "flip_verify_failed",
                        repo: Some(repo),
                        branch: Some(branch),
                        body: Some(&why),
                        ..Default::default()
                    },
                    now,
                )?;
                Ok(Err(format!("{branch} was not marked ready: {why}.")))
            }
        }
    })
}
