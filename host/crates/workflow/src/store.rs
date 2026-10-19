//! The task, change and event tables and plain row access.
//!
//! - `workflow_runs`: one row per task; a partial unique index keeps one active task per chat.
//! - `workflow_changes`: one row per (task, repo, branch), with the tip and tree, the build, the
//!   approval it holds and the binding a hand-off made.
//! - `workflow_events`: the append-only record; kinds come from [`EVENT_KINDS`].

use rusqlite::{params, OptionalExtension, Row, Transaction};
use std::cell::RefCell;
use std::collections::BTreeSet;

thread_local! {
    /// The chats whose task, change or event rows this thread wrote since the last
    /// [`take_touched`]. Every write below marks its chat, so the `workflow_run {chatId}` event
    /// follows every run write without each caller having to remember it.
    static TOUCHED: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

fn touch(chat_id: &str) {
    TOUCHED.with(|t| {
        t.borrow_mut().insert(chat_id.to_owned());
    });
}

/// Drain the chats this thread wrote run rows for. A rolled-back write may be in the list; the
/// event it leads to only makes the UI re-read.
pub fn take_touched() -> Vec<String> {
    TOUCHED.with(|t| std::mem::take(&mut *t.borrow_mut()).into_iter().collect())
}

/// Emit `workflow_run {chatId}` for every chat this thread wrote run rows for, and clear the list.
/// Call it on the thread that did the writes, after the transaction.
pub fn emit_touched(events: &dyn helm_ipc::EventSink) {
    emit_chats(events, take_touched());
}

/// Emit `workflow_run {chatId}` for each chat (the UI re-reads through `workflow_run_get`).
pub fn emit_chats(events: &dyn helm_ipc::EventSink, chats: Vec<String>) {
    for chat in chats {
        events.emit(helm_ipc::AppEvent::new(RUN_EVENT).with("chatId", serde_json::json!(chat)));
    }
}

/// The event that tells the UI a chat's workflow run changed.
pub const RUN_EVENT: &str = "workflow_run";

/// The event kinds, append-only. New kinds go last.
pub const EVENT_KINDS: &[&str] = &[
    "run_started",
    "parked",
    "reopened",
    "ended",
    "finished",
    "plan_recorded",
    "plan_round",
    "plan_signoff_waiting",
    "afk_plan_signed_off",
    "review_round",
    "build_observed",
    "cleanup",
    "visual_review",
    "handback_requested",
    "handback_result",
    "gate_denied",
    "skip",
    "visual_set",
    "visual_cleared",
    "edits_allowed",
    "change_dropped",
    "not_handed_off",
    "handoff_requested",
    "handoff_duplicate",
    "land_result",
    "box_verify",
    "awaiting_merge",
    "merge_declined",
    "flip_verify_failed",
    "flipped",
    "conflict",
    "stage_due",
    "stage_report",
    "change_handoff",
    "handoff_refused",
    "followups_queued",
    "claim_takeover",
    "guard_override",
    "marked_unverified",
    "cap_self_granted",
    "pr_view_override",
    "consent",
    "orchestrator_mode",
    "approval_carried",
];

pub const MAX_EVENT_BODY: usize = 256 * 1024;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflow_runs (
             id              TEXT PRIMARY KEY,
             chat_id         TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             title           TEXT NOT NULL,
             item_id         TEXT,
             visual          INTEGER NOT NULL DEFAULT 0,
             attempt         INTEGER NOT NULL DEFAULT 1 CHECK (attempt >= 1),
             status          TEXT NOT NULL CHECK (status IN ('active', 'parked', 'handed_off', 'ended', 'finished')),
             park_reason     TEXT,
             plan_id         TEXT,
             plan_text       TEXT,
             plan_approved   INTEGER NOT NULL DEFAULT 0,
             plan_signed_off INTEGER NOT NULL DEFAULT 0,
             plan_rounds     INTEGER NOT NULL DEFAULT 0,
             plan_extra      INTEGER NOT NULL DEFAULT 0,
             review_extra    INTEGER NOT NULL DEFAULT 0,
             smoke_extra     INTEGER NOT NULL DEFAULT 0,
             skipped         TEXT NOT NULL DEFAULT '[]',
             created_at      INTEGER NOT NULL,
             updated_at      INTEGER NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS workflow_runs_one_active
             ON workflow_runs(chat_id) WHERE status = 'active';
         CREATE INDEX IF NOT EXISTS workflow_runs_chat ON workflow_runs(chat_id, created_at);
         CREATE TABLE IF NOT EXISTS workflow_changes (
             run_id          TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
             repo            TEXT NOT NULL CHECK (repo LIKE '%_/%_/%_'),
             branch          TEXT NOT NULL,
             pr              TEXT,
             base            TEXT,
             tip             TEXT,
             tree            TEXT,
             build_tip       TEXT,
             build_tree      TEXT,
             approved_tip    TEXT,
             approved_tree   TEXT,
             approved_base   TEXT,
             cleanup_tip     TEXT,
             smoke_pass_tip  TEXT,
             smoke_fail_tip  TEXT,
             review_rounds   INTEGER NOT NULL DEFAULT 0,
             smoke_rounds    INTEGER NOT NULL DEFAULT 0,
             state           TEXT NOT NULL CHECK (state IN ('open', 'landing_draft', 'handed_off',
                 'pending_land', 'awaiting_merge', 'landing', 'land_failed', 'landed', 'flipped',
                 'dropped', 'declined')),
             failed_mode     TEXT CHECK (failed_mode IS NULL OR failed_mode IN ('draft', 'merge')),
             mode            TEXT,
             handoff_attempt INTEGER,
             landed_sha      TEXT,
             error           TEXT,
             updated_at      INTEGER NOT NULL,
             PRIMARY KEY (run_id, repo, branch)
         );
         CREATE TABLE IF NOT EXISTS workflow_events (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             chat_id    TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             run_id     TEXT,
             attempt    INTEGER,
             kind       TEXT NOT NULL,
             repo       TEXT,
             branch     TEXT,
             round      INTEGER,
             verdict    TEXT,
             tip        TEXT,
             tree       TEXT,
             plan_id    TEXT,
             body       TEXT CHECK (body IS NULL OR length(body) <= 262144),
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS workflow_events_run ON workflow_events(run_id, kind);
         CREATE UNIQUE INDEX IF NOT EXISTS workflow_events_one_handoff
             ON workflow_events(run_id, attempt) WHERE kind = 'handoff_requested';",
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub chat_id: String,
    pub title: String,
    pub item_id: Option<String>,
    pub visual: bool,
    pub attempt: i64,
    pub status: String,
    pub park_reason: Option<String>,
    pub plan_id: Option<String>,
    pub plan_approved: bool,
    pub plan_signed_off: bool,
    pub plan_rounds: i64,
    pub plan_extra: i64,
    pub review_extra: i64,
    pub smoke_extra: i64,
    pub skipped: Vec<String>,
}

const RUN_COLS: &str = "id, chat_id, title, item_id, visual, attempt, status, park_reason, plan_id,
    plan_approved, plan_signed_off, plan_rounds, plan_extra, review_extra, smoke_extra, skipped";

fn run_of(r: &Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get(0)?,
        chat_id: r.get(1)?,
        title: r.get(2)?,
        item_id: r.get(3)?,
        visual: r.get::<_, i64>(4)? != 0,
        attempt: r.get(5)?,
        status: r.get(6)?,
        park_reason: r.get(7)?,
        plan_id: r.get(8)?,
        plan_approved: r.get::<_, i64>(9)? != 0,
        plan_signed_off: r.get::<_, i64>(10)? != 0,
        plan_rounds: r.get(11)?,
        plan_extra: r.get(12)?,
        review_extra: r.get(13)?,
        smoke_extra: r.get(14)?,
        skipped: serde_json::from_str(&r.get::<_, String>(15)?).unwrap_or_default(),
    })
}

pub fn active_run(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Option<Run>> {
    tx.query_row(
        &format!("SELECT {RUN_COLS} FROM workflow_runs WHERE chat_id = ?1 AND status = 'active'"),
        [chat_id],
        run_of,
    )
    .optional()
}

pub fn run(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<Run>> {
    tx.query_row(
        &format!("SELECT {RUN_COLS} FROM workflow_runs WHERE id = ?1"),
        [id],
        run_of,
    )
    .optional()
}

/// The chat's other open tasks (parked, or handed off with a change not yet terminal), newest first.
pub fn open_runs(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Vec<Run>> {
    let mut st = tx.prepare(&format!(
        "SELECT {RUN_COLS} FROM workflow_runs r WHERE chat_id = ?1 AND (status IN ('active', 'parked')
           OR (status = 'handed_off' AND EXISTS (SELECT 1 FROM workflow_changes c WHERE c.run_id = r.id
               AND c.state NOT IN ('landed', 'flipped', 'dropped', 'declined'))))
         ORDER BY created_at DESC LIMIT 16"
    ))?;
    let rows = st.query_map([chat_id], run_of)?;
    rows.collect()
}

pub fn insert_run(
    tx: &Transaction<'_>,
    id: &str,
    chat_id: &str,
    title: &str,
    item_id: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO workflow_runs(id, chat_id, title, item_id, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5)",
        params![id, chat_id, title, item_id, now],
    )?;
    touch(chat_id);
    Ok(())
}

/// Write the mutable run fields back.
pub fn save_run(
    tx: &Transaction<'_>,
    r: &Run,
    plan_text: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE workflow_runs SET title = ?2, item_id = ?3, visual = ?4, attempt = ?5, status = ?6,
             park_reason = ?7, plan_id = ?8, plan_approved = ?9, plan_signed_off = ?10,
             plan_rounds = ?11, plan_extra = ?12, review_extra = ?13, smoke_extra = ?14,
             skipped = ?15, plan_text = COALESCE(?16, plan_text), updated_at = ?17
         WHERE id = ?1",
        params![
            r.id,
            r.title,
            r.item_id,
            r.visual as i64,
            r.attempt,
            r.status,
            r.park_reason,
            r.plan_id,
            r.plan_approved as i64,
            r.plan_signed_off as i64,
            r.plan_rounds,
            r.plan_extra,
            r.review_extra,
            r.smoke_extra,
            serde_json::to_string(&r.skipped).unwrap_or_else(|_| "[]".into()),
            plan_text,
            now
        ],
    )?;
    touch(&r.chat_id);
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub run_id: String,
    pub repo: String,
    pub branch: String,
    pub pr: Option<String>,
    pub base: Option<String>,
    pub tip: Option<String>,
    pub tree: Option<String>,
    pub build_tip: Option<String>,
    pub build_tree: Option<String>,
    pub approved_tip: Option<String>,
    pub approved_tree: Option<String>,
    pub approved_base: Option<String>,
    pub cleanup_tip: Option<String>,
    pub smoke_pass_tip: Option<String>,
    pub smoke_fail_tip: Option<String>,
    pub review_rounds: i64,
    pub smoke_rounds: i64,
    pub state: String,
    pub failed_mode: Option<String>,
    pub mode: Option<String>,
    pub handoff_attempt: Option<i64>,
    pub landed_sha: Option<String>,
    pub error: Option<String>,
}

const CHANGE_COLS: &str = "run_id, repo, branch, pr, base, tip, tree, build_tip, build_tree,
    approved_tip, approved_tree, approved_base, cleanup_tip, smoke_pass_tip, smoke_fail_tip,
    review_rounds, smoke_rounds, state, failed_mode, mode, handoff_attempt, landed_sha, error";

fn change_of(r: &Row<'_>) -> rusqlite::Result<Change> {
    Ok(Change {
        run_id: r.get(0)?,
        repo: r.get(1)?,
        branch: r.get(2)?,
        pr: r.get(3)?,
        base: r.get(4)?,
        tip: r.get(5)?,
        tree: r.get(6)?,
        build_tip: r.get(7)?,
        build_tree: r.get(8)?,
        approved_tip: r.get(9)?,
        approved_tree: r.get(10)?,
        approved_base: r.get(11)?,
        cleanup_tip: r.get(12)?,
        smoke_pass_tip: r.get(13)?,
        smoke_fail_tip: r.get(14)?,
        review_rounds: r.get(15)?,
        smoke_rounds: r.get(16)?,
        state: r.get(17)?,
        failed_mode: r.get(18)?,
        mode: r.get(19)?,
        handoff_attempt: r.get(20)?,
        landed_sha: r.get(21)?,
        error: r.get(22)?,
    })
}

/// A task's changes in insertion order (claim order follows it).
pub fn changes(tx: &Transaction<'_>, run_id: &str) -> rusqlite::Result<Vec<Change>> {
    let mut st = tx.prepare(&format!(
        "SELECT {CHANGE_COLS} FROM workflow_changes WHERE run_id = ?1 ORDER BY rowid"
    ))?;
    let rows = st.query_map([run_id], change_of)?;
    rows.collect()
}

pub fn change(
    tx: &Transaction<'_>,
    run_id: &str,
    repo: &str,
    branch: &str,
) -> rusqlite::Result<Option<Change>> {
    tx.query_row(
        &format!(
            "SELECT {CHANGE_COLS} FROM workflow_changes WHERE run_id = ?1 AND repo = ?2 AND branch = ?3"
        ),
        params![run_id, repo, branch],
        change_of,
    )
    .optional()
}

/// Changes in the given states across every task, for the land step.
pub fn changes_in(tx: &Transaction<'_>, states: &[&str]) -> rusqlite::Result<Vec<Change>> {
    let list = states
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "")))
        .collect::<Vec<_>>()
        .join(", ");
    let mut st = tx.prepare(&format!(
        "SELECT {CHANGE_COLS} FROM workflow_changes WHERE state IN ({list}) ORDER BY rowid"
    ))?;
    let rows = st.query_map([], change_of)?;
    rows.collect()
}

pub fn upsert_change(tx: &Transaction<'_>, c: &Change, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        &format!(
            "INSERT INTO workflow_changes({CHANGE_COLS}, updated_at) VALUES
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
             ON CONFLICT(run_id, repo, branch) DO UPDATE SET pr = excluded.pr, base = excluded.base,
                 tip = excluded.tip, tree = excluded.tree, build_tip = excluded.build_tip,
                 build_tree = excluded.build_tree, approved_tip = excluded.approved_tip,
                 approved_tree = excluded.approved_tree, approved_base = excluded.approved_base,
                 cleanup_tip = excluded.cleanup_tip, smoke_pass_tip = excluded.smoke_pass_tip,
                 smoke_fail_tip = excluded.smoke_fail_tip, review_rounds = excluded.review_rounds,
                 smoke_rounds = excluded.smoke_rounds, state = excluded.state,
                 failed_mode = excluded.failed_mode, mode = excluded.mode,
                 handoff_attempt = excluded.handoff_attempt, landed_sha = excluded.landed_sha,
                 error = excluded.error, updated_at = excluded.updated_at"
        ),
        params![
            c.run_id,
            c.repo,
            c.branch,
            c.pr,
            c.base,
            c.tip,
            c.tree,
            c.build_tip,
            c.build_tree,
            c.approved_tip,
            c.approved_tree,
            c.approved_base,
            c.cleanup_tip,
            c.smoke_pass_tip,
            c.smoke_fail_tip,
            c.review_rounds,
            c.smoke_rounds,
            c.state,
            c.failed_mode,
            c.mode,
            c.handoff_attempt,
            c.landed_sha,
            c.error,
            now
        ],
    )?;
    if let Some(chat) = tx
        .query_row(
            "SELECT chat_id FROM workflow_runs WHERE id = ?1",
            [&c.run_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        touch(&chat);
    }
    Ok(())
}

/// One event row. `body` is cut to [`MAX_EVENT_BODY`] bytes.
#[derive(Clone, Debug, Default)]
pub struct Event<'a> {
    pub kind: &'a str,
    pub repo: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub round: Option<i64>,
    pub verdict: Option<&'a str>,
    pub tip: Option<&'a str>,
    pub tree: Option<&'a str>,
    pub plan_id: Option<&'a str>,
    pub body: Option<&'a str>,
}

pub fn event(
    tx: &Transaction<'_>,
    chat_id: &str,
    run: Option<&Run>,
    e: &Event<'_>,
    now: i64,
) -> rusqlite::Result<i64> {
    debug_assert!(
        EVENT_KINDS.contains(&e.kind),
        "unknown event kind {}",
        e.kind
    );
    let body = e.body.map(|b| {
        let mut cut = b.len().min(MAX_EVENT_BODY);
        while !b.is_char_boundary(cut) {
            cut -= 1;
        }
        &b[..cut]
    });
    tx.execute(
        "INSERT INTO workflow_events(chat_id, run_id, attempt, kind, repo, branch, round, verdict,
             tip, tree, plan_id, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            chat_id,
            run.map(|r| r.id.as_str()),
            run.map(|r| r.attempt),
            e.kind,
            e.repo,
            e.branch,
            e.round,
            e.verdict,
            e.tip,
            e.tree,
            e.plan_id,
            body,
            now
        ],
    )?;
    let id = tx.last_insert_rowid();
    touch(chat_id);
    // The board record this event maps to, for tasks linked to an item (Agent Standup).
    crate::sa_outbox::on_event(tx, run, e, id, now)?;
    Ok(id)
}

/// How many events of `kind` this task has in `attempt` for the given target.
pub fn count_events(
    tx: &Transaction<'_>,
    run_id: &str,
    kind: &str,
    attempt: Option<i64>,
    repo: Option<&str>,
    branch: Option<&str>,
    verdict: Option<&str>,
) -> rusqlite::Result<i64> {
    tx.query_row(
        "SELECT count(*) FROM workflow_events WHERE run_id = ?1 AND kind = ?2
           AND (?3 IS NULL OR attempt = ?3) AND repo IS ?4 AND branch IS ?5
           AND (?6 IS NULL OR verdict = ?6)",
        params![run_id, kind, attempt, repo, branch, verdict],
        |r| r.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_run_write_marks_its_chat() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude"), ("b", "claude")]);
        let _ = take_touched();
        let w = |f: &dyn Fn(&Transaction<'_>) -> rusqlite::Result<()>| {
            db.write(|tx| f(tx)).unwrap();
            take_touched()
        };
        assert_eq!(w(&|tx| insert_run(tx, "r", "a", "T", None, 1)), ["a"]);
        let run = db.snapshot(|tx| super::run(tx, "r")).unwrap().unwrap();
        assert_eq!(w(&|tx| save_run(tx, &run, None, 2)), ["a"]);
        let c = Change {
            run_id: "r".into(),
            repo: "github.com/o/r".into(),
            branch: "x".into(),
            state: "open".into(),
            ..Default::default()
        };
        assert_eq!(w(&|tx| upsert_change(tx, &c, 3)), ["a"]);
        let e = Event {
            kind: "parked",
            ..Default::default()
        };
        assert_eq!(w(&|tx| event(tx, "b", None, &e, 4).map(|_| ())), ["b"]);
        assert!(w(&|_| Ok(())).is_empty(), "a read marks nothing");
        // Draining clears the list.
        db.write(|tx| insert_run(tx, "r2", "b", "T", None, 5))
            .unwrap();
        assert_eq!(take_touched(), ["b"]);
        assert!(take_touched().is_empty());
    }

    #[test]
    fn event_kinds_are_unique_and_44() {
        let mut k = super::EVENT_KINDS.to_vec();
        k.sort();
        k.dedup();
        assert_eq!(k.len(), 44);
        assert_eq!(super::EVENT_KINDS.len(), 44);
    }
}
