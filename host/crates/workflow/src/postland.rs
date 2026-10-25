//! After the land step: the wrap-up of each landed change, the post-land report, and the queue of
//! follow-ups the orchestrator sees.
//!
//! - **Wrap-up.** When the land step merges a change, the worktree that has its branch checked out
//!   is removed if it is merged, clean and unused (`helm_worktrees::remove`: links taken out first,
//!   git's own non-forced removal, unknown means keep). A worktree whose HEAD is still exactly the
//!   merged commit counts as merged even after a squash. Each outcome is a `stage_report` row.
//! - **Post-land due.** Once every change of a handed-off task is landed, left as a draft for the
//!   user, or dropped, the task gets one `stage_due` row and its chat one note naming what the
//!   wrap-up did and what is queued. A stage agent may then send one `after_land` report for the
//!   oldest such task (`done`, `awaiting_merge` or `not_done`), which settles it.
//! - **Follow-ups.** The follow-ups of an approving code review and of a post-land report are
//!   queued on their task: at most 20 entries and 8 KiB per task, duplicates skipped, each with an
//!   8-hex id. `status` lists the chat's queue; `drop {queued: id}` discards one.

use crate::actions::{Act, Out};
use crate::reports::Report;
use crate::store::{self, Change, Event, Run};
use helm_core::db::Database;
use rusqlite::{params, Transaction};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const MAX_QUEUE: usize = 20;
pub const MAX_QUEUE_BYTES: usize = 8 * 1024;
/// How much of the queue `status` shows.
pub const STATUS_BYTES: usize = 2 * 1024;
/// The verdicts of a post-land report; any of them settles the task's post-land stage.
pub const AFTER_LAND: [&str; 3] = ["done", "awaiting_merge", "not_done"];

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflow_followups (
             run_id     TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
             id         TEXT NOT NULL,
             text       TEXT NOT NULL,
             source     TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             PRIMARY KEY (run_id, id)
         );",
    )
}

fn entry_id(run_id: &str, text: &str) -> String {
    let h = hex::encode(Sha256::digest(format!("{run_id}\0{text}").as_bytes()));
    h[..8].to_owned()
}

/// Queue follow-ups on a task. Returns how many were added.
pub fn queue(
    tx: &Transaction<'_>,
    run: &Run,
    source: &str,
    texts: &[String],
    now: i64,
) -> rusqlite::Result<usize> {
    let (mut n, mut bytes): (usize, usize) = tx.query_row(
        "SELECT count(*), COALESCE(SUM(length(text)), 0) FROM workflow_followups WHERE run_id = ?1",
        [&run.id],
        |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize)),
    )?;
    let mut added = 0;
    for t in texts.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        if n >= MAX_QUEUE || bytes + t.len() > MAX_QUEUE_BYTES {
            break;
        }
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO workflow_followups(run_id, id, text, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run.id, entry_id(&run.id, t), t, source, now],
        )?;
        if inserted > 0 {
            n += 1;
            bytes += t.len();
            added += 1;
        }
    }
    if added > 0 {
        let body = format!("{added} follow-up(s) queued from {source}");
        store::event(
            tx,
            &run.chat_id,
            Some(run),
            &Event {
                kind: "followups_queued",
                round: Some(added as i64),
                body: Some(&body),
                ..Default::default()
            },
            now,
        )?;
    }
    Ok(added)
}

/// `drop {queued: id}`: discard a queued follow-up of one of this chat's tasks.
pub fn discard(tx: &Transaction<'_>, x: &Act<'_>, a: &Value) -> Out {
    let Some(id) = a
        .get("queued")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 16)
    else {
        return Ok(Err(
            "queued must be the id of a queued follow-up (see status).".into(),
        ));
    };
    let n = tx.execute(
        "DELETE FROM workflow_followups WHERE id = ?1
           AND run_id IN (SELECT id FROM workflow_runs WHERE chat_id = ?2)",
        params![id, x.chat_id],
    )?;
    if n == 0 {
        return Ok(Err(format!(
            "No queued follow-up {id} in this chat; status lists them."
        )));
    }
    Ok(Ok(json!({"discarded": id})))
}

/// This chat's tasks that finished landing and wait for their post-land report, oldest first.
pub fn due_runs(tx: &Transaction<'_>, chat: &str) -> rusqlite::Result<Vec<Run>> {
    let mut st = tx.prepare(
        "SELECT r.id FROM workflow_runs r WHERE r.chat_id = ?1 AND r.status != 'ended'
           AND EXISTS (SELECT 1 FROM workflow_events e WHERE e.run_id = r.id AND e.kind = 'stage_due')
           AND NOT EXISTS (SELECT 1 FROM workflow_events e WHERE e.run_id = r.id
               AND e.kind = 'stage_report' AND e.verdict IN ('done', 'awaiting_merge', 'not_done'))
         ORDER BY r.created_at LIMIT 8",
    )?;
    let ids = st
        .query_map([chat], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for id in ids {
        out.extend(store::run(tx, &id)?);
    }
    Ok(out)
}

/// The queue and the post-land tasks, added to `status`.
pub fn add_to_status(tx: &Transaction<'_>, chat: &str, v: &mut Value) -> rusqlite::Result<()> {
    let mut st = tx.prepare(
        "SELECT f.id, f.text, f.source, r.title FROM workflow_followups f
         JOIN workflow_runs r ON r.id = f.run_id WHERE r.chat_id = ?1
         ORDER BY f.created_at, f.rowid",
    )?;
    let rows = st
        .query_map([chat], |r| {
            Ok(
                json!({"id": r.get::<_, String>(0)?, "text": r.get::<_, String>(1)?,
                "from": r.get::<_, String>(2)?, "task": r.get::<_, String>(3)?}),
            )
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let total = rows.len();
    let mut shown = Vec::new();
    let mut bytes = 0;
    for r in rows {
        bytes += r["text"].as_str().map_or(0, str::len);
        if bytes > STATUS_BYTES {
            break;
        }
        shown.push(r);
    }
    v["followUps"] = json!({"queued": shown, "total": total,
        "next": if total > 0 { "Turn each into work (a new task or change) or discard it with drop {queued: id}." } else { "" }});
    let due: Vec<Value> = due_runs(tx, chat)?
        .iter()
        .map(|r| json!({"runId": r.id, "title": r.title}))
        .collect();
    if !due.is_empty() {
        v["postLand"] = json!({"due": due, "next": "These tasks finished landing. If one needs post-land work (closing board items, filing follow-ups), dispatch an agent for it; it reports with app_workflow_report kind after_land, which settles the oldest."});
    }
    Ok(())
}

/// An `after_land` report: settles the oldest task of this chat that finished landing.
pub fn after_land_report(tx: &Transaction<'_>, x: &Act<'_>, r: &Report, body: &str) -> Out {
    let Some(run) = due_runs(tx, x.chat_id)?.into_iter().next() else {
        return Ok(Err("Permanent: no task in this chat has finished landing and waits for a post-land report, so it does not count.".into()));
    };
    let verdict = r.verdict.as_deref().unwrap_or("done");
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            kind: "stage_report",
            verdict: Some(verdict),
            body: Some(body),
            ..Default::default()
        },
        x.now,
    )?;
    let queued = queue(
        tx,
        &run,
        "the post-land report",
        &crate::reports::follow_ups(r),
        x.now,
    )?;
    Ok(Ok(
        json!({"recorded": true, "kind": "after_land", "runId": run.id, "task": run.title, "verdict": verdict, "queued": queued}),
    ))
}

/// The worktree that has `branch` checked out, from `git worktree list` in `dir`.
fn worktree_of(git: &helm_worktrees::git::Git, dir: &Path, branch: &str) -> Option<PathBuf> {
    let o = git.run(dir, &["worktree", "list", "--porcelain"])?;
    if !o.ok() {
        return None;
    }
    let want = format!("branch refs/heads/{branch}");
    let mut path = None;
    for line in o.stdout.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if line == want {
            return path;
        }
    }
    None
}

fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

/// Whether a chat that is not archived works in `wt` (its folder is it, or inside it).
fn in_use(db: &Database, wt: &Path) -> Option<bool> {
    let paths: Vec<String> = db
        .snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT DISTINCT p.path FROM chats c JOIN projects p ON p.id = c.project_id
                 WHERE c.archived = 0 AND p.path IS NOT NULL",
            )?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .ok()?;
    let w = norm(wt);
    Some(paths.iter().any(|p| {
        let p = norm(Path::new(p));
        p == w || p.starts_with(&format!("{w}/"))
    }))
}

/// The wrap-up of one landed change: remove its worktree if that is safe. `None` when the change
/// has no worktree Helm can find.
fn wrap_up(db: &Database, chat_id: &str, c: &Change) -> Option<(bool, String)> {
    let cwd = crate::wftools::chat_cwd(db, chat_id)?;
    let git = helm_worktrees::git::Git::locate()?;
    let wt = worktree_of(&git, &cwd, &c.branch)?;
    // Only a worktree of the change's own repository.
    if crate::gates::repo_of(&wt).as_deref() != Some(c.repo.as_str()) {
        return None;
    }
    let tip = c.approved_tip.clone().unwrap_or_default();
    let shown = wt.to_string_lossy().into_owned();
    Some(
        match helm_worktrees::remove::remove_landed(&git, &wt, &tip, in_use(db, &wt)) {
            helm_worktrees::remove::Outcome::Removed { .. } => {
                (true, format!("removed the worktree {shown}"))
            }
            helm_worktrees::remove::Outcome::Kept(why) => {
                (false, format!("kept the worktree {shown}: {why}"))
            }
        },
    )
}

/// After the land step wrote a change's outcome: the wrap-up for a landed change, then the
/// task's post-land due check. Git runs outside any transaction. Notes are queued on this thread.
pub fn after_step(db: &Database, before: &Change, now: i64) -> rusqlite::Result<()> {
    let Some((run, c)) = db.snapshot(|tx| {
        Ok::<_, rusqlite::Error>(
            match (
                store::run(tx, &before.run_id)?,
                store::change(tx, &before.run_id, &before.repo, &before.branch)?,
            ) {
                (Some(r), Some(c)) => Some((r, c)),
                _ => None,
            },
        )
    })?
    else {
        return Ok(());
    };
    let wrapped = if c.state == "landed" && c.state != before.state {
        wrap_up(db, &run.chat_id, &c)
    } else {
        None
    };
    db.write(|tx| {
        if let Some((removed, body)) = &wrapped {
            store::event(
                tx,
                &run.chat_id,
                Some(&run),
                &Event {
                    kind: "stage_report",
                    repo: Some(&c.repo),
                    branch: Some(&c.branch),
                    verdict: Some(if *removed {
                        "worktree_removed"
                    } else {
                        "worktree_kept"
                    }),
                    body: Some(body),
                    ..Default::default()
                },
                now,
            )?;
        }
        land_complete(tx, &run.id, now).map(|_| ())
    })
}

/// When every change of a handed-off task is landed, left as a draft, or dropped (and one of them
/// is not dropped), mark its post-land stage due once per attempt and tell its chat.
pub fn land_complete(tx: &Transaction<'_>, run_id: &str, now: i64) -> rusqlite::Result<bool> {
    let Some(run) = store::run(tx, run_id)? else {
        return Ok(false);
    };
    if run.status != "handed_off" {
        return Ok(false);
    }
    let all = store::changes(tx, &run.id)?;
    let settled = all.iter().all(|c| {
        matches!(
            c.state.as_str(),
            "landed" | "flipped" | "dropped" | "handed_off"
        )
    });
    if !settled || all.iter().all(|c| c.state == "dropped") {
        return Ok(false);
    }
    if store::count_events(
        tx,
        &run.id,
        "stage_due",
        Some(run.attempt),
        None,
        None,
        None,
    )? > 0
    {
        return Ok(false);
    }
    let merged = all
        .iter()
        .filter(|c| matches!(c.state.as_str(), "landed" | "flipped"))
        .count();
    let drafts = all.iter().filter(|c| c.state == "handed_off").count();
    let summary = format!("{merged} merged, {drafts} left as drafts for the user");
    store::event(
        tx,
        &run.chat_id,
        Some(&run),
        &Event {
            kind: "stage_due",
            verdict: Some("after_land"),
            body: Some(&summary),
            ..Default::default()
        },
        now,
    )?;
    let mut st = tx.prepare(
        "SELECT body FROM workflow_events WHERE run_id = ?1 AND kind = 'stage_report'
           AND verdict IN ('worktree_removed', 'worktree_kept') ORDER BY id",
    )?;
    let wrapped = st
        .query_map([&run.id], |r| r.get::<_, Option<String>>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let queued: i64 = tx.query_row(
        "SELECT count(*) FROM workflow_followups WHERE run_id = ?1",
        [&run.id],
        |r| r.get(0),
    )?;
    let text = format!(
        "Workflow: task \"{}\" has finished landing ({summary}). Wrap-up: {}. Follow-ups queued: {queued} (app_workflow status lists them). If the task needs post-land work, such as closing its board item or filing the follow-ups, dispatch an agent for it; it reports with app_workflow_report kind after_land.",
        crate::mode::tooltip_reason(&run.title),
        if wrapped.is_empty() {
            "no worktree to remove".to_owned()
        } else {
            wrapped.join("; ")
        }
    );
    crate::notes::queue(
        &run.chat_id,
        text,
        true,
        format!("wf-landed-{}-{}", run.id, run.attempt),
    );
    Ok(true)
}
