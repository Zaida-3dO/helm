//! The Agent Standup outbox: stage records for tasks linked to a board item, queued in the same
//! transaction as the workflow event they describe and sent later by [`flush`].
//!
//! | Workflow event | Board record |
//! |---|---|
//! | `run_started` | transition to `executing` |
//! | `plan_recorded` | `plan` artifact |
//! | `plan_round` | `plan_review` artifact (approve → `lgtm`) |
//! | `build_observed` | `commit` artifact at the tip |
//! | `review_round` (approved or changes required) | `code_review` artifact bound to the tip |
//! | `change_handoff` of a change with a PR | `pull_request` artifact, `open` |
//! | `land_result` landed | `commit` artifact (the merged sha) and `pull_request` `merged` |
//!
//! A task is linked by its own `itemId`, else by the chat's main item link (helm-itemlinks); a
//! chat linked only for reference reports nowhere.
//! Every artifact body starts with a fixed marker naming the row, which is how the read-back
//! recognises a record whose outcome was unknown.

use crate::sa_writer::{SendError, Target, WriteTransport};
use crate::store::{Event, Run};
use helm_core::db::Database;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};

pub const MAX_ATTEMPTS: i64 = 8;
pub const CREATED_BY: &str = "helm";

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflow_standup_outbox (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             chat_id    TEXT NOT NULL,
             run_id     TEXT,
             item_id    TEXT NOT NULL,
             op         TEXT NOT NULL CHECK (op IN ('artifact', 'transition')),
             payload    TEXT NOT NULL,
             idem       TEXT NOT NULL UNIQUE,
             state      TEXT NOT NULL CHECK (state IN ('pending', 'unknown', 'sent', 'failed')),
             attempts   INTEGER NOT NULL DEFAULT 0,
             last_error TEXT,
             next_at    INTEGER NOT NULL,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS workflow_standup_outbox_due ON workflow_standup_outbox(state, next_at);",
    )
}

/// The board item a task reports to: its own, else its chat's main linked item.
pub fn item_of(tx: &Transaction<'_>, run: &Run) -> rusqlite::Result<Option<String>> {
    if let Some(i) = run.item_id.as_ref().filter(|i| !i.trim().is_empty()) {
        return Ok(Some(i.clone()));
    }
    let has: bool = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'item_chat_links'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !has {
        return Ok(None);
    }
    tx.query_row(
        "SELECT item_id FROM item_chat_links WHERE chat_id = ?1 AND is_main = 1 ORDER BY linked_at DESC LIMIT 1",
        [&run.chat_id],
        |r| r.get(0),
    )
    .optional()
}

pub fn marker(idem: &str) -> String {
    format!("[helm {idem}]")
}

fn queue(
    tx: &Transaction<'_>,
    run: &Run,
    item: &str,
    op: &str,
    idem: &str,
    payload: Value,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO workflow_standup_outbox(chat_id, run_id, item_id, op, payload, idem, state, next_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?7, ?7)",
        params![run.chat_id, run.id, item, op, payload.to_string(), idem, now],
    )?;
    Ok(())
}

fn artifact(kind: &str, idem: &str, text: &str) -> Value {
    let body: String = format!("{} {text}", marker(idem))
        .chars()
        .take(8000)
        .collect();
    json!({"artifactKind": kind, "body": body, "createdByType": "agent", "createdById": CREATED_BY})
}

/// Called for every workflow event: queue the board record it maps to, if the task is linked.
pub fn on_event(
    tx: &Transaction<'_>,
    run: Option<&Run>,
    e: &Event<'_>,
    event_id: i64,
    now: i64,
) -> rusqlite::Result<()> {
    let Some(run) = run else { return Ok(()) };
    let mapped = matches!(
        e.kind,
        "run_started"
            | "plan_recorded"
            | "plan_round"
            | "build_observed"
            | "review_round"
            | "change_handoff"
            | "land_result"
    );
    if !mapped {
        return Ok(());
    }
    let Some(item) = item_of(tx, run)? else {
        return Ok(());
    };
    let idem = format!("{}-{event_id}", run.id);
    let on = |a: Option<&str>| a.unwrap_or("").to_owned();
    match e.kind {
        // Only an item still on deck is moved; one already further along is left alone.
        "run_started" => queue(
            tx,
            run,
            &item,
            "transition",
            &idem,
            json!({"to": "executing", "expectedFrom": "on_deck"}),
            now,
        ),
        "plan_recorded" => {
            let text: Option<String> = tx
                .query_row(
                    "SELECT plan_text FROM workflow_runs WHERE id = ?1",
                    [&run.id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let body = format!(
                "Plan of record {} adopted in Helm.\n\n{}",
                on(e.plan_id),
                text.unwrap_or_default()
            );
            queue(
                tx,
                run,
                &item,
                "artifact",
                &idem,
                artifact("plan", &idem, &body),
                now,
            )
        }
        "plan_round" => {
            let verdict = if e.verdict == Some("approve") {
                "lgtm"
            } else {
                "changes_required"
            };
            let mut a = artifact(
                "plan_review",
                &idem,
                &format!(
                    "Plan review round {} of plan {}: {verdict}.",
                    e.round.unwrap_or(0),
                    on(e.plan_id)
                ),
            );
            a["verdict"] = json!(verdict);
            queue(tx, run, &item, "artifact", &idem, a, now)
        }
        "build_observed" => {
            let Some(tip) = e.tip else { return Ok(()) };
            let mut a = artifact(
                "commit",
                &idem,
                &format!("Build reported on {} {}.", on(e.repo), on(e.branch)),
            );
            a["commitSha"] = json!(tip);
            a["ref"] = json!(on(e.branch));
            queue(tx, run, &item, "artifact", &idem, a, now)
        }
        "review_round" => {
            let (Some(tip), Some(v)) = (e.tip, e.verdict) else {
                return Ok(());
            };
            let verdict = match v {
                "approved" => "lgtm",
                "changes_required" => "changes_required",
                _ => return Ok(()), // a stale round approves nothing and binds to no commit
            };
            let mut a = artifact(
                "code_review",
                &idem,
                &format!(
                    "Code review round {} of {} {}: {verdict}.",
                    e.round.unwrap_or(0),
                    on(e.repo),
                    on(e.branch)
                ),
            );
            a["verdict"] = json!(verdict);
            a["commitSha"] = json!(tip);
            queue(tx, run, &item, "artifact", &idem, a, now)
        }
        "change_handoff" => {
            let pr: Option<String> = tx
                .query_row(
                    "SELECT pr FROM workflow_changes WHERE run_id = ?1 AND repo = ?2 AND branch = ?3",
                    params![run.id, on(e.repo), on(e.branch)],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let Some(pr) = pr.filter(|p| p.starts_with("https://")) else {
                return Ok(());
            };
            let mut a = artifact("pull_request", &idem, "open");
            // The board reads a pull_request body as its status, so the marker goes in ref only.
            a["body"] = json!("open");
            a["ref"] = json!(pr);
            queue(tx, run, &item, "artifact", &idem, a, now)
        }
        "land_result" if e.verdict == Some("landed") => {
            let Some(sha) = e.tip else { return Ok(()) };
            let mut c = artifact(
                "commit",
                &idem,
                &format!(
                    "Landed {} {} by Helm's land step.",
                    on(e.repo),
                    on(e.branch)
                ),
            );
            c["commitSha"] = json!(sha);
            queue(
                tx,
                run,
                &item,
                "artifact",
                &format!("{idem}-commit"),
                c,
                now,
            )?;
            let pr: Option<String> = tx
                .query_row(
                    "SELECT pr FROM workflow_changes WHERE run_id = ?1 AND repo = ?2 AND branch = ?3",
                    params![run.id, on(e.repo), on(e.branch)],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            if let Some(pr) = pr.filter(|p| p.starts_with("https://")) {
                queue(
                    tx,
                    run,
                    &item,
                    "artifact",
                    &format!("{idem}-pr"),
                    json!({"artifactKind": "pull_request", "body": "merged", "ref": pr, "createdByType": "agent", "createdById": CREATED_BY}),
                    now,
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flushed {
    pub sent: usize,
    pub unknown: usize,
    pub failed: usize,
    pub waiting: usize,
}

struct Row {
    id: i64,
    item: String,
    op: String,
    payload: Value,
    idem: String,
    state: String,
    attempts: i64,
}

fn backoff(attempts: i64) -> i64 {
    (60_000_i64 << attempts.clamp(0, 6)).min(3_600_000)
}

/// Whether a record whose outcome was unknown is already on the board.
fn landed(target: &Target, t: &dyn WriteTransport, r: &Row) -> Result<bool, SendError> {
    if r.op == "transition" {
        let item = target.item(t, &r.item)?;
        let state = item
            .get("item")
            .unwrap_or(&item)
            .get("state")
            .and_then(Value::as_str);
        return Ok(state == r.payload.get("to").and_then(Value::as_str));
    }
    let kind = r
        .payload
        .get("artifactKind")
        .and_then(Value::as_str)
        .unwrap_or("");
    let list = target.artifacts(t, &r.item, kind)?;
    let mark = marker(&r.idem);
    let want_ref = r.payload.get("ref").and_then(Value::as_str);
    let want_body = r.payload.get("body").and_then(Value::as_str);
    Ok(list
        .get("artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|a| {
            let preview = a
                .get("bodyPreview")
                .or_else(|| a.get("body"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if kind == "pull_request" {
                // Its body is a bare status, so match on the PR and the status.
                a.get("ref").and_then(Value::as_str) == want_ref && Some(preview) == want_body
            } else {
                preview.starts_with(&mark)
            }
        }))
}

/// How many of a chat's records are still waiting to reach the board (not yet sent, not given up).
pub fn waiting(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<u32> {
    tx.query_row(
        "SELECT count(*) FROM workflow_standup_outbox WHERE chat_id = ?1 AND state IN ('pending', 'unknown')",
        [chat_id],
        |r| r.get(0),
    )
}

/// Send what is due. Network calls happen outside any transaction.
pub fn flush(
    db: &Database,
    target: &Target,
    t: &dyn WriteTransport,
    now: i64,
    max: usize,
) -> rusqlite::Result<Flushed> {
    let rows: Vec<Row> = db.snapshot(|tx| {
        let mut st = tx.prepare(
            "SELECT id, item_id, op, payload, idem, state, attempts FROM workflow_standup_outbox
             WHERE state IN ('pending', 'unknown') AND next_at <= ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = st.query_map(params![now, max as i64], |r| {
            Ok(Row {
                id: r.get(0)?,
                item: r.get(1)?,
                op: r.get(2)?,
                payload: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
                idem: r.get(4)?,
                state: r.get(5)?,
                attempts: r.get(6)?,
            })
        })?;
        rows.collect()
    })?;
    let mut out = Flushed::default();
    for r in rows {
        let outcome = (|| {
            if r.state == "unknown" && landed(target, t, &r)? {
                return Ok(());
            }
            match r.op.as_str() {
                "transition" => target.transition(t, &r.item, &r.payload).map(|_| ()),
                _ => target.artifact(t, &r.item, &r.payload).map(|_| ()),
            }
        })();
        let attempts = r.attempts + 1;
        let (state, err, next) = match outcome {
            Ok(()) => ("sent", None, now),
            Err(SendError::Refused(s, m)) => ("failed", Some(format!("{s}: {m}")), now),
            Err(_) if attempts >= MAX_ATTEMPTS => {
                ("failed", Some("gave up after 8 attempts".to_owned()), now)
            }
            Err(SendError::NotSent(m)) => (
                if r.state == "unknown" {
                    "unknown"
                } else {
                    "pending"
                },
                Some(m),
                now + backoff(attempts),
            ),
            Err(SendError::Unknown(m)) => ("unknown", Some(m), now + backoff(attempts)),
        };
        match state {
            "sent" => out.sent += 1,
            "failed" => out.failed += 1,
            "unknown" => out.unknown += 1,
            _ => out.waiting += 1,
        }
        if let Some(e) = &err {
            log::info!("Agent Standup record {} is {state}: {e}", r.idem);
        }
        db.write(|tx| {
            tx.execute(
                "UPDATE workflow_standup_outbox SET state = ?2, attempts = ?3, last_error = ?4, next_at = ?5, updated_at = ?6 WHERE id = ?1",
                params![r.id, state, attempts, err, next, now],
            )
        })?;
    }
    Ok(out)
}
