//! The gatekeeper's own tables: `gate_asks` and `gate_settings`.
//!
//! `gate_asks` is the audit trail of every card the gate looked at. Chat ids carry no foreign
//! key, so a record outlives the chat it came from, and it keeps a title snapshot. Every state
//! change is a compare-and-set: the row moves only from the state the caller expects, so two
//! paths that race (the user answering while the gate decides) leave one winner.
//!
//! Rows live 180 days and are pruned by the crate's upkeep step; a decided row nobody reviewed
//! becomes `kept` after seven days and leaves the review list.

use crate::settings::{read_lenient, GateSettings};
use helm_chats::types::GateBrief;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub const KEEP_AFTER_MS: i64 = 7 * 24 * 3_600_000;
pub const RETENTION_MS: i64 = 180 * 24 * 3_600_000;

/// The audit table's definition under `name`. The channel check names every kind of ask the
/// gate keeps, `stop` (a chat's own stop at the end of a turn) among them.
fn asks_ddl(name: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {name} (
             id            TEXT PRIMARY KEY,
             profile_id    TEXT NOT NULL DEFAULT '',
             chat_id       TEXT NOT NULL,
             chat_title    TEXT NOT NULL DEFAULT '',
             channel       TEXT NOT NULL
                 CHECK (channel IN ('question', 'plan', 'flag', 'telegram_ask', 'board_pick', 'stop')),
             request_ref   TEXT NOT NULL,
             asked_at      INTEGER NOT NULL,
             question      TEXT NOT NULL DEFAULT '',
             question_json TEXT NOT NULL DEFAULT '',
             verdict       TEXT NOT NULL
                 CHECK (verdict IN ('checking', 'decided', 'escalated', 'failed', 'passed')),
             pass_reason   TEXT,
             decided_by    TEXT CHECK (decided_by IN ('local', 'claude', 'rule')),
             model         TEXT,
             confidence    REAL,
             risk          TEXT CHECK (risk IN ('low', 'medium', 'high')),
             reason        TEXT,
             choice        TEXT,
             brief_json    TEXT,
             took_ms       INTEGER,
             state         TEXT NOT NULL
                 CHECK (state IN ('open', 'answered', 'parked', 'withdrawn', 'kept',
                                  'vetoed', 'expired')),
             answer        TEXT,
             answered_via  TEXT CHECK (answered_via IN ('helm', 'telegram', 'board', 'chat')),
             answered_at   INTEGER,
             review        TEXT NOT NULL DEFAULT 'pending'
                 CHECK (review IN ('pending', 'fine', 'should_have_asked', 'didnt_need_me')),
             review_note   TEXT,
             stop_class    TEXT
         );"
    )
}

const ASK_INDEXES: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS gate_asks_ref ON gate_asks(chat_id, request_ref);
         CREATE INDEX IF NOT EXISTS gate_asks_asked ON gate_asks(asked_at DESC);
         CREATE INDEX IF NOT EXISTS gate_asks_chat ON gate_asks(chat_id);
         CREATE INDEX IF NOT EXISTS gate_asks_review
             ON gate_asks(verdict, state, review, asked_at DESC);";

/// Every column of `gate_asks`, in the order the rebuild copies them.
const ASK_COLUMNS: &str = "id, profile_id, chat_id, chat_title, channel, request_ref, asked_at,     question, question_json, verdict, pass_reason, decided_by, model, confidence, risk, reason,     choice, brief_json, took_ms, state, answer, answered_via, answered_at, review, review_note,     stop_class";

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS gate_settings (
             profile_id TEXT NOT NULL PRIMARY KEY,
             value      TEXT NOT NULL
         );
         {}
         -- Where a chat's run of automatic nudges stands: how many were sent, and the last
         -- question one answered. It ends with the run, or when the user writes in the chat.
         CREATE TABLE IF NOT EXISTS gate_stop_chains (
             chat_id        TEXT NOT NULL PRIMARY KEY,
             started_at     INTEGER NOT NULL,
             nudges         INTEGER NOT NULL DEFAULT 0,
             last_question  TEXT NOT NULL DEFAULT ''
         );
         -- A board's own working mode; no row means the app's.
         CREATE TABLE IF NOT EXISTS board_working_modes (
             project_id  TEXT NOT NULL PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
             mode        TEXT NOT NULL CHECK (mode IN ('run', 'check_in')),
             updated_at  INTEGER NOT NULL
         );",
        asks_ddl("gate_asks")
    ))?;
    add_stop_class(tx)?;
    tx.execute_batch(ASK_INDEXES)?;
    Ok(())
}

/// The table gained its stop class after the first version was written.
fn add_stop_class(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let has_class: bool = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('gate_asks') WHERE name = 'stop_class'",
        [],
        |r| r.get::<_, i64>(0).map(|n| n > 0),
    )?;
    if !has_class {
        tx.execute("ALTER TABLE gate_asks ADD COLUMN stop_class TEXT", [])?;
    }
    Ok(())
}

/// One-time: a table made before stops existed refuses the `stop` channel in its check. Rebuild
/// it under the new definition, keeping every row and index. Safe to run on any table: one that
/// already names `stop` is left alone.
pub fn allow_stop_channel(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let sql: String = tx
        .query_row(
            "SELECT COALESCE(sql, '') FROM sqlite_master WHERE type = 'table' AND name = 'gate_asks'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default();
    if sql.is_empty() || sql.contains("'stop'") {
        return Ok(());
    }
    add_stop_class(tx)?;
    tx.execute_batch(&format!(
        "{}
         INSERT INTO gate_asks_rebuilt ({ASK_COLUMNS}) SELECT {ASK_COLUMNS} FROM gate_asks;
         DROP TABLE gate_asks;
         ALTER TABLE gate_asks_rebuilt RENAME TO gate_asks;
         {ASK_INDEXES}",
        asks_ddl("gate_asks_rebuilt")
    ))
}

/// Whose chat this is, falling back to the machine's current person, then to none. The profiles
/// tables belong to another crate; without them the answer is the empty key.
pub fn chat_profile(tx: &Transaction<'_>, chat_id: &str) -> String {
    let of_chat: Option<String> = tx
        .query_row(
            "SELECT profile_id FROM chats WHERE id = ?1",
            [chat_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Some(p) = of_chat.filter(|p| !p.is_empty()) {
        return p;
    }
    active_profile(tx)
}

/// The person this PC is currently set to: the profile the sidebar shows, or the home profile
/// while none is chosen or all of them are shown (every chat without a profile of its own is
/// given the home one at start). The empty key when there are no profiles at all.
pub fn active_profile(tx: &Transaction<'_>) -> String {
    match helm_core::prefs::read(tx, "activeProfile") {
        Some(Value::String(id)) if !id.is_empty() && id != "all" => id,
        _ => tx
            .query_row("SELECT id FROM profiles WHERE is_home = 1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap_or_default(),
    }
}

pub fn load_settings(tx: &Transaction<'_>, profile_id: &str) -> rusqlite::Result<GateSettings> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM gate_settings WHERE profile_id = ?1",
            [profile_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .map(|v| read_lenient(&v))
        .unwrap_or_default())
}

pub fn save_settings(
    tx: &Transaction<'_>,
    profile_id: &str,
    s: &GateSettings,
) -> rusqlite::Result<()> {
    let v = serde_json::to_string(s).expect("settings serialise");
    tx.execute(
        "INSERT INTO gate_settings (profile_id, value) VALUES (?1, ?2)
         ON CONFLICT(profile_id) DO UPDATE SET value = excluded.value",
        params![profile_id, v],
    )?;
    Ok(())
}

/// One audit row as the UI and the review list see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GateAsk {
    pub id: String,
    pub profile_id: String,
    pub chat_id: String,
    pub chat_title: String,
    pub channel: String,
    pub request_ref: String,
    #[ts(type = "number")]
    pub asked_at: i64,
    /// The question as asked, in one line.
    pub question: String,
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pass_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub risk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub brief: Option<GateBrief>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub took_ms: Option<i64>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answered_via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub answered_at: Option<i64>,
    pub review: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review_note: Option<String>,
    /// For a stop: `blocker`, `limit`, `failure` or `asked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_class: Option<String>,
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<GateAsk> {
    let brief: Option<String> = r.get(16)?;
    Ok(GateAsk {
        id: r.get(0)?,
        profile_id: r.get(1)?,
        chat_id: r.get(2)?,
        chat_title: r.get(3)?,
        channel: r.get(4)?,
        request_ref: r.get(5)?,
        asked_at: r.get(6)?,
        question: r.get(7)?,
        verdict: r.get(8)?,
        pass_reason: r.get(9)?,
        decided_by: r.get(10)?,
        model: r.get(11)?,
        confidence: r.get(12)?,
        risk: r.get(13)?,
        reason: r.get(14)?,
        choice: r.get(15)?,
        brief: brief.and_then(|b| serde_json::from_str(&b).ok()),
        took_ms: r.get(17)?,
        state: r.get(18)?,
        answer: r.get(19)?,
        answered_via: r.get(20)?,
        answered_at: r.get(21)?,
        review: r.get(22)?,
        review_note: r.get(23)?,
        stop_class: r.get(24)?,
    })
}

const COLS: &str = "id, profile_id, chat_id, chat_title, channel, request_ref, asked_at, \
                    question, verdict, pass_reason, decided_by, model, confidence, risk, \
                    reason, choice, brief_json, took_ms, state, answer, \
                    answered_via, answered_at, review, review_note, stop_class";

/// Open a card the gate is holding. One row per (chat, request): a re-request of the same id
/// answers `false` and writes nothing, so a worker replay cannot stack rows.
#[allow(clippy::too_many_arguments)]
pub fn open_ask(
    tx: &Transaction<'_>,
    id: &str,
    profile_id: &str,
    chat_id: &str,
    chat_title: &str,
    request_ref: &str,
    question: &str,
    card_json: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "INSERT INTO gate_asks
             (id, profile_id, chat_id, chat_title, channel, request_ref, asked_at,
              question, question_json, verdict, state)
         VALUES (?1, ?2, ?3, ?4, 'question', ?5, ?6, ?7, ?8, 'checking', 'open')
         ON CONFLICT(chat_id, request_ref) DO NOTHING",
        params![
            id,
            profile_id,
            chat_id,
            chat_title,
            request_ref,
            now,
            question,
            card_json
        ],
    )? > 0)
}

/// Open a turn end the stop judge is holding: one row per (chat, run), so a replay stacks
/// nothing. False when the run already has its row.
pub fn open_stop_ask(
    tx: &Transaction<'_>,
    id: &str,
    profile_id: &str,
    chat_id: &str,
    chat_title: &str,
    run_id: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "INSERT INTO gate_asks
             (id, profile_id, chat_id, chat_title, channel, request_ref, asked_at,
              verdict, state)
         VALUES (?1, ?2, ?3, ?4, 'stop', ?5, ?6, 'checking', 'open')
         ON CONFLICT(chat_id, request_ref) DO NOTHING",
        params![id, profile_id, chat_id, chat_title, run_id, now],
    )? > 0)
}

/// What the judge made of one held stop, for the record.
pub struct StopRecord<'a> {
    /// `decided` (a nudge), `escalated` (a stop), `passed` (done) or `failed`.
    pub verdict: &'a str,
    pub pass_reason: Option<&'a str>,
    /// Who decided: `claude` after a model's answer, `rule` for the host's own table.
    pub decided_by: &'a str,
    pub model: Option<&'a str>,
    pub confidence: Option<f64>,
    /// What the chat would have asked.
    pub question: &'a str,
    pub reason: Option<&'a str>,
    /// The default the nudge told it to take.
    pub choice: Option<&'a str>,
    pub stop_class: Option<&'a str>,
    pub took_ms: i64,
    /// `open`, or `withdrawn` when the verdict never reached the held turn end.
    pub state: &'a str,
}

/// Move a checking stop row to its outcome, once. False when another path already settled it.
pub fn finish_stop_ask(
    tx: &Transaction<'_>,
    id: &str,
    r: &StopRecord<'_>,
) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "UPDATE gate_asks SET verdict = ?2, pass_reason = ?3, decided_by = ?4, model = ?5,
                 confidence = ?6, question = ?7, reason = ?8, choice = ?9, stop_class = ?10,
                 took_ms = ?11, state = ?12
         WHERE id = ?1 AND verdict = 'checking' AND channel = 'stop'",
        params![
            id,
            r.verdict,
            r.pass_reason,
            r.decided_by,
            r.model,
            r.confidence,
            r.question,
            r.reason,
            r.choice,
            r.stop_class,
            r.took_ms,
            r.state
        ],
    )? > 0)
}

/// One audit row for a turn end the judge let through without a model, with the reason.
#[allow(clippy::too_many_arguments)]
pub fn pass_stop(
    tx: &Transaction<'_>,
    id: &str,
    profile_id: &str,
    chat_id: &str,
    chat_title: &str,
    run_id: &str,
    why: &str,
    now: i64,
) -> rusqlite::Result<()> {
    if open_stop_ask(tx, id, profile_id, chat_id, chat_title, run_id, now)? {
        tx.execute(
            "UPDATE gate_asks SET verdict = 'passed', pass_reason = ?2, decided_by = 'rule'
             WHERE id = ?1",
            params![id, why],
        )?;
    }
    Ok(())
}

/// The kinds of message that are the user's own side of a conversation: they end a run of
/// nudges. Notes Helm or another chat sent do not.
const USER_SIDE: &str =
    "'typed', 'answer', 'remote', 'telegram', 'voice', 'action', 'notification'";

/// A run of nudges older than this starts again.
pub const CHAIN_MAX_AGE_MS: i64 = 6 * 3_600_000;

/// Where the chat's current run of nudges stands: how many were sent and the last question
/// one answered. A run ends when the user writes in the chat after it started, or after six
/// hours; either way it reads as not started.
pub fn chain(tx: &Transaction<'_>, chat_id: &str, now: i64) -> rusqlite::Result<(i64, String)> {
    let row: Option<(i64, i64, String)> = tx
        .query_row(
            "SELECT started_at, nudges, last_question FROM gate_stop_chains WHERE chat_id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((started, nudges, last)) = row else {
        return Ok((0, String::new()));
    };
    let spoke: bool = tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM submissions s
                            JOIN submission_meta m ON m.submission_id = s.id
                            WHERE s.chat_id = ?1 AND s.created_at > ?2
                              AND m.origin IN ({USER_SIDE}))"
        ),
        params![chat_id, started],
        |r| r.get(0),
    )?;
    if spoke || now - started > CHAIN_MAX_AGE_MS {
        end_chain(tx, chat_id)?;
        return Ok((0, String::new()));
    }
    Ok((nudges, last))
}

/// Count one more nudge, remembering the question it answered.
pub fn add_nudge(
    tx: &Transaction<'_>,
    chat_id: &str,
    question: &str,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO gate_stop_chains (chat_id, started_at, nudges, last_question)
         VALUES (?1, ?2, 1, ?3)
         ON CONFLICT(chat_id) DO UPDATE SET nudges = nudges + 1, last_question = ?3",
        params![chat_id, now, question],
    )?;
    Ok(())
}

/// The run is over: the next stop starts a new count.
pub fn end_chain(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM gate_stop_chains WHERE chat_id = ?1", [chat_id])?;
    Ok(())
}

/// Move a checking row to its verdict, once. False when another path already settled it.
#[allow(clippy::too_many_arguments)]
pub fn finish_ask(
    tx: &Transaction<'_>,
    id: &str,
    verdict: &str,
    pass_reason: Option<&str>,
    decided_by: Option<&str>,
    model: Option<&str>,
    confidence: Option<f64>,
    risk: Option<&str>,
    reason: Option<&str>,
    choice: Option<&str>,
    brief: Option<&GateBrief>,
    took_ms: i64,
    state: &str,
    answered_via: Option<&str>,
    now: i64,
) -> rusqlite::Result<bool> {
    let brief_json = brief.map(|b| serde_json::to_string(b).expect("brief serialises"));
    let answered_at = matches!(state, "answered").then_some(now);
    Ok(tx.execute(
        "UPDATE gate_asks SET verdict = ?2, pass_reason = ?3, decided_by = ?4, model = ?5,
                 confidence = ?6, risk = ?7, reason = ?8, choice = ?9, brief_json = ?10,
                 took_ms = ?11, state = ?12, answered_via = ?13, answered_at = ?14
         WHERE id = ?1 AND verdict = 'checking'",
        params![
            id,
            verdict,
            pass_reason,
            decided_by,
            model,
            confidence,
            risk,
            reason,
            choice,
            brief_json,
            took_ms,
            state,
            answered_via,
            answered_at
        ],
    )? > 0)
}

pub fn ask(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<GateAsk>> {
    tx.query_row(
        &format!("SELECT {COLS} FROM gate_asks WHERE id = ?1"),
        [id],
        row,
    )
    .optional()
}

/// Filters for the list: `all`, `decided`, `asked` (the ones that reached the user), `failed`,
/// and `review` — decided rows still waiting for their seven days or their verdict from the
/// user. Search matches the question, the chat title, the choice and the reason, taking the
/// words literally (`%` and `_` are not wildcards).
///
/// Only one person's rows come back, with the rows that belong to no one. A verdict that never
/// reached its card (`withdrawn`: the card was closed before Helm answered) shows under `all`
/// only, so the other filters never list a choice Helm did not make.
pub fn list(
    tx: &Transaction<'_>,
    profile_id: &str,
    filter: &str,
    query: &str,
    limit: i64,
    now: i64,
) -> rusqlite::Result<Vec<GateAsk>> {
    let where_ = match filter {
        "decided" => "verdict = 'decided' AND state != 'withdrawn'",
        "asked" => "verdict = 'escalated' AND state != 'withdrawn'",
        "failed" => "verdict = 'failed' AND state != 'withdrawn'",
        "review" => "verdict = 'decided' AND state = 'open' AND review = 'pending'",
        _ => "1",
    };
    let q = query.trim();
    let like = if q.is_empty() {
        String::new()
    } else {
        let escaped = q
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    };
    let sql = format!(
        "SELECT {COLS} FROM gate_asks WHERE {where_} \
         AND (profile_id = ?4 OR profile_id = '') \
         AND (?1 = '' OR question LIKE ?1 ESCAPE '\\' OR chat_title LIKE ?1 ESCAPE '\\' \
              OR COALESCE(choice, '') LIKE ?1 ESCAPE '\\' \
              OR COALESCE(reason, '') LIKE ?1 ESCAPE '\\') \
         AND asked_at >= ?3 \
         ORDER BY asked_at DESC LIMIT ?2"
    );
    let mut st = tx.prepare(&sql)?;
    let rows = st.query_map(params![like, limit, now - RETENTION_MS, profile_id], row)?;
    rows.collect()
}

/// Whether a review mark fits a verdict: "fine" and "should have asked me" mark a decision
/// Helm made, "didn't need me" marks a question it handed to the user.
pub fn mark_fits(verdict: &str, review: &str) -> bool {
    match review {
        "fine" | "should_have_asked" => verdict == "decided",
        "didnt_need_me" => verdict == "escalated",
        _ => false,
    }
}

/// The user's marked examples for one person: at most six "should have asked me" (things the
/// gate decided that it should not have) and six "didn't need me" (things it asked about that
/// it need not have), newest first, each cut to 400 characters. They ride along the next job
/// as few-shot lines.
pub fn examples(tx: &Transaction<'_>, profile_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut st = tx.prepare(
        "SELECT review, COALESCE(question, ''), COALESCE(review_note, '')
         FROM gate_asks
         WHERE profile_id = ?1 AND review IN ('should_have_asked', 'didnt_need_me')
         ORDER BY asked_at DESC LIMIT 24",
    )?;
    let rows = st.query_map([profile_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let cut = |s: &str| -> String { s.chars().take(400).collect() };
    let mut out = Vec::new();
    let mut of = [0usize; 2];
    for r in rows {
        let (kind, question, note) = r?;
        let idx = usize::from(kind == "didnt_need_me");
        if of[idx] >= 6 || question.trim().is_empty() {
            continue;
        }
        of[idx] += 1;
        out.push(serde_json::json!({
            "kind": kind,
            "question": cut(&question),
            "note": note.map(|n| cut(&n)).unwrap_or_default(),
        }));
    }
    Ok(out)
}

/// Mark one row reviewed, once, with a mark that fits its verdict ([`mark_fits`]). The row as
/// it stands comes back either way; `None` when it is gone.
pub fn review(
    tx: &Transaction<'_>,
    id: &str,
    review: &str,
    note: Option<&str>,
) -> rusqlite::Result<Option<GateAsk>> {
    tx.execute(
        "UPDATE gate_asks SET review = ?2, review_note = ?3
         WHERE id = ?1 AND review = 'pending'
           AND ((?2 IN ('fine', 'should_have_asked') AND verdict = 'decided')
                OR (?2 = 'didnt_need_me' AND verdict = 'escalated'))",
        params![id, review, note],
    )?;
    ask(tx, id)
}

/// The user changed their mind about a decided row: it leaves the review list.
pub fn veto(tx: &Transaction<'_>, id: &str, now: i64) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "UPDATE gate_asks SET state = 'vetoed', answered_at = ?2
         WHERE id = ?1 AND verdict = 'decided' AND state IN ('open', 'kept')",
        params![id, now],
    )? > 0)
}

/// Undo a [`veto`] whose correction never reached the chat: the row goes back to `was`.
pub fn unveto(tx: &Transaction<'_>, id: &str, was: &str) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "UPDATE gate_asks SET state = ?2, answered_at = NULL
         WHERE id = ?1 AND state = 'vetoed' AND ?2 IN ('open', 'kept')",
        params![id, was],
    )? > 0)
}

/// Decided rows nobody reviewed in seven days leave the review list (they stay in history).
pub fn mark_kept(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE gate_asks SET state = 'kept'
         WHERE verdict = 'decided' AND state = 'open' AND review = 'pending' AND asked_at < ?1",
        [now - KEEP_AFTER_MS],
    )
}

/// Rows older than the retention are pruned.
pub fn prune(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "DELETE FROM gate_stop_chains WHERE started_at < ?1",
        [now - CHAIN_MAX_AGE_MS],
    )?;
    tx.execute(
        "DELETE FROM gate_asks WHERE asked_at < ?1",
        [now - RETENTION_MS],
    )
}

/// A `checking` row the process never finished (a crash between holding and deciding) is
/// settled as failed, so nothing stays "checking" forever.
pub fn settle_orphans(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE gate_asks SET verdict = 'failed', reason = 'The check did not finish.'
         WHERE verdict = 'checking' AND asked_at < ?1",
        [now - 5 * 60_000],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AskFrequency;
    use helm_core::db::{Database, MigrationRegistry};
    use std::sync::Arc;

    const T0: i64 = 1_800_000_000_000;
    const DAY: i64 = 24 * 3_600_000;

    fn brief() -> GateBrief {
        GateBrief {
            problem: "Rebuild or wait?".into(),
            options: vec![helm_chats::types::GateBriefOption {
                label: "Rebuild".into(),
                means: "start again".into(),
                pros: "clean".into(),
                cons: "slow".into(),
                cost: "20 min".into(),
            }],
            recommendation: "Rebuild".into(),
            why: "it is already broken".into(),
        }
    }

    fn env() -> (tempfile::TempDir, Arc<Database>) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        reg.initializer("gate", init);
        let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
        (dir, db)
    }

    /// The table as it was before stops existed: no `stop` channel and no stop class.
    const OLD_TABLE: &str = "DROP TABLE gate_asks;
        CREATE TABLE gate_asks (
             id TEXT PRIMARY KEY, profile_id TEXT NOT NULL DEFAULT '', chat_id TEXT NOT NULL,
             chat_title TEXT NOT NULL DEFAULT '',
             channel TEXT NOT NULL
                 CHECK (channel IN ('question', 'plan', 'flag', 'telegram_ask', 'board_pick')),
             request_ref TEXT NOT NULL, asked_at INTEGER NOT NULL,
             question TEXT NOT NULL DEFAULT '', question_json TEXT NOT NULL DEFAULT '',
             verdict TEXT NOT NULL
                 CHECK (verdict IN ('checking', 'decided', 'escalated', 'failed', 'passed')),
             pass_reason TEXT, decided_by TEXT, model TEXT, confidence REAL, risk TEXT,
             reason TEXT, choice TEXT, brief_json TEXT, took_ms INTEGER,
             state TEXT NOT NULL
                 CHECK (state IN ('open', 'answered', 'parked', 'withdrawn', 'kept',
                                  'vetoed', 'expired')),
             answer TEXT, answered_via TEXT, answered_at INTEGER,
             review TEXT NOT NULL DEFAULT 'pending', review_note TEXT
         );
         CREATE UNIQUE INDEX gate_asks_ref ON gate_asks(chat_id, request_ref);
         INSERT INTO gate_asks (id, chat_id, channel, request_ref, asked_at, question, verdict, state, choice, review)
             VALUES ('old1', 'c1', 'question', 'r1', 1799999999990, 'Which file?', 'decided', 'open', 'a.txt', 'fine'),
                    ('old2', 'c1', 'question', 'r2', 1799999999991, 'Which dir?', 'escalated', 'open', NULL, 'pending');";

    #[test]
    fn stops_have_their_own_rows_runs_and_a_table_that_can_hold_them() {
        let (_dir, db) = env();
        let chat = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, T0))
            .unwrap();
        // A table made before stops existed keeps every row and index through the rebuild,
        // and then takes a stop; one that is already current is left alone.
        db.write(|tx| tx.execute_batch(OLD_TABLE)).unwrap();
        assert!(
            db.write(|tx| open_stop_ask(tx, "s0", "", "c1", "t", "run-0", T0))
                .is_err(),
            "the old table refuses a stop"
        );
        db.write(allow_stop_channel).unwrap();
        let after = db
            .snapshot(|tx| list(tx, "", "all", "", 50, T0 + 10))
            .unwrap();
        let kept: Vec<(&str, &str, Option<&str>)> = after
            .iter()
            .map(|r| (r.id.as_str(), r.review.as_str(), r.choice.as_deref()))
            .collect();
        assert_eq!(
            kept,
            [("old2", "pending", None), ("old1", "fine", Some("a.txt"))]
        );
        assert!(after.iter().all(|r| r.stop_class.is_none()));
        // Running both again changes nothing.
        db.write(init).unwrap();
        db.write(allow_stop_channel).unwrap();
        let indexes: i64 = db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name IN
                     ('gate_asks_ref', 'gate_asks_asked', 'gate_asks_chat', 'gate_asks_review')",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(indexes, 4);

        // A stop is one row per run, and its outcome is kept once.
        assert!(db
            .write(|tx| open_stop_ask(tx, "s1", "", &chat, "Fix it", "run-1", T0))
            .unwrap());
        assert!(
            !db.write(|tx| open_stop_ask(tx, "s2", "", &chat, "Fix it", "run-1", T0))
                .unwrap(),
            "a replay of the same run stacks nothing"
        );
        let record = |class: Option<&'static str>, verdict: &'static str| StopRecord {
            verdict,
            pass_reason: None,
            decided_by: "claude",
            model: Some("sonnet"),
            confidence: Some(0.9),
            question: "Start phase two?",
            reason: Some("it is next"),
            choice: None,
            stop_class: class,
            took_ms: 800,
            state: "open",
        };
        assert!(db
            .write(|tx| finish_stop_ask(tx, "s1", &record(Some("blocker"), "escalated")))
            .unwrap());
        assert!(
            !db.write(|tx| finish_stop_ask(tx, "s1", &record(None, "decided")))
                .unwrap(),
            "the first outcome stands"
        );
        let row = db.snapshot(|tx| ask(tx, "s1")).unwrap().unwrap();
        assert_eq!(
            (
                row.channel.as_str(),
                row.verdict.as_str(),
                row.stop_class.as_deref()
            ),
            ("stop", "escalated", Some("blocker"))
        );
        // A stop is a question Helm handed over, so it lists under "asked" and can be marked.
        let asked = db
            .snapshot(|tx| list(tx, "", "asked", "", 50, T0 + 10))
            .unwrap();
        assert!(asked.iter().any(|r| r.id == "s1"));
        assert!(mark_fits(&row.verdict, "didnt_need_me"));
        // A passed turn end is audited without a judgement.
        db.write(|tx| pass_stop(tx, "s3", "", &chat, "Fix it", "run-3", "limit", T0))
            .unwrap();
        let passed = db.snapshot(|tx| ask(tx, "s3")).unwrap().unwrap();
        assert_eq!(
            (passed.verdict.as_str(), passed.pass_reason.as_deref()),
            ("passed", Some("limit"))
        );

        // The count of nudges in a run: it grows, the user writing ends it, Helm's own notes
        // do not, and a run left for hours starts again.
        let add = |q: &str, at: i64| db.write(|tx| add_nudge(tx, &chat, q, at)).unwrap();
        let state = |at: i64| db.write(|tx| chain(tx, &chat, at)).unwrap();
        assert_eq!(state(T0), (0, String::new()));
        add("one", T0);
        add("two", T0 + 1);
        assert_eq!(state(T0 + 2), (2, "two".into()));
        let says = |id: &str, origin: &str, at: i64| {
            db.write(|tx| {
                tx.execute(
                    "INSERT INTO submissions (id, chat_id, text, mode, state, created_at)
                     VALUES (?1, ?2, 'x', 'prompt', 'complete', ?3)",
                    params![id, chat, at],
                )?;
                tx.execute(
                    "INSERT INTO submission_meta (submission_id, origin) VALUES (?1, ?2)",
                    params![id, origin],
                )
            })
            .unwrap();
        };
        says("note-a", "app", T0 + 5);
        says("note-b", "other_chat", T0 + 6);
        says("note-c", "typed", T0 - 5);
        assert_eq!(
            state(T0 + 7).0,
            2,
            "notes, and words from before the run, never end it"
        );
        says("note-d", "telegram", T0 + 8);
        assert_eq!(
            state(T0 + 9),
            (0, String::new()),
            "the user writing ends the run"
        );
        add("three", T0 + 20);
        assert_eq!(state(T0 + 21).0, 1);
        assert_eq!(
            state(T0 + 20 + CHAIN_MAX_AGE_MS + 1).0,
            0,
            "an old run starts again"
        );
        add("four", T0 + 30);
        db.write(|tx| prune(tx, T0 + 30 + CHAIN_MAX_AGE_MS + 1))
            .unwrap();
        assert_eq!(state(T0 + 31).0, 0);
    }

    #[test]
    fn one_table_walks_the_whole_lifecycle() {
        let (_dir, db) = env();
        let chat = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, T0))
            .unwrap();

        // A held card opens one row; a replay of the same request stacks nothing.
        let opened = db
            .write(|tx| {
                open_ask(
                    tx,
                    "a1",
                    "",
                    &chat,
                    "Fix the report",
                    "req-1",
                    "Which file?",
                    "{}",
                    T0,
                )
            })
            .unwrap();
        assert!(opened);
        let again = db
            .write(|tx| {
                open_ask(
                    tx,
                    "a9",
                    "",
                    &chat,
                    "Fix the report",
                    "req-1",
                    "Which file?",
                    "{}",
                    T0 + 1,
                )
            })
            .unwrap();
        assert!(!again, "the same (chat, request) never gets a second row");
        assert_eq!(
            db.snapshot(|tx| list(tx, "", "all", "", 50, T0))
                .unwrap()
                .len(),
            1
        );

        // Every state change is a compare-and-set: the winner's verdict stands.
        let first = db
            .write(|tx| {
                finish_ask(
                    tx,
                    "a1",
                    "decided",
                    None,
                    Some("claude"),
                    Some("sonnet"),
                    Some(0.8),
                    Some("low"),
                    Some("A default fits here"),
                    Some("Yes"),
                    None,
                    1_200,
                    "open",
                    None,
                    T0 + 2_000,
                )
            })
            .unwrap();
        let second = db
            .write(|tx| {
                finish_ask(
                    tx,
                    "a1",
                    "escalated",
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(&brief()),
                    900,
                    "open",
                    None,
                    T0 + 2_100,
                )
            })
            .unwrap();
        assert!(first && !second);
        let row = db.snapshot(|tx| ask(tx, "a1")).unwrap().unwrap();
        assert_eq!(row.verdict, "decided");
        assert_eq!(row.state, "open");
        assert_eq!(row.choice.as_deref(), Some("Yes"));
        assert_eq!(row.confidence, Some(0.8));

        // Review marks stick once; the first mark wins.
        let marked = db
            .write(|tx| review(tx, "a1", "should_have_asked", Some("too clever")))
            .unwrap()
            .unwrap();
        assert_eq!(marked.review, "should_have_asked");
        let retaken = db
            .write(|tx| review(tx, "a1", "fine", None))
            .unwrap()
            .unwrap();
        assert_eq!(retaken.review, "should_have_asked");

        // Marked rows ride the next job as examples, each note cut short.
        let ex = db.snapshot(|tx| examples(tx, "")).unwrap();
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0]["kind"], "should_have_asked");
        assert_eq!(ex[0]["note"], "too clever");

        // A second decided row, nobody reviews it: after seven days it is kept and leaves
        // the review list, and a later change of mind still works.
        db.write(|tx| {
            open_ask(tx, "a2", "", &chat, "t", "req-2", "Which name?", "{}", T0)?;
            finish_ask(
                tx,
                "a2",
                "decided",
                None,
                Some("claude"),
                Some("sonnet"),
                Some(0.9),
                Some("low"),
                Some("r"),
                Some("Left"),
                None,
                10,
                "open",
                None,
                T0 + 10,
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        assert_eq!(
            db.snapshot(|tx| list(tx, "", "review", "", 50, T0 + DAY))
                .unwrap()
                .len(),
            1,
            "only the unreviewed decided row waits"
        );
        assert_eq!(db.write(|tx| mark_kept(tx, T0 + 8 * DAY)).unwrap(), 1);
        assert!(db
            .snapshot(|tx| list(tx, "", "review", "", 50, T0 + 8 * DAY))
            .unwrap()
            .is_empty());
        assert!(db.write(|tx| veto(tx, "a2", T0 + 8 * DAY + 1)).unwrap());
        assert_eq!(
            db.snapshot(|tx| ask(tx, "a2")).unwrap().unwrap().state,
            "vetoed"
        );

        // An escalated row carries the write-up; answering it lets "didn't need me" mark it.
        db.write(|tx| {
            open_ask(
                tx,
                "a3",
                "",
                &chat,
                "t",
                "req-3",
                "Deploy anyway?",
                "{}",
                T0,
            )?;
            finish_ask(
                tx,
                "a3",
                "escalated",
                None,
                None,
                Some("sonnet"),
                Some(0.4),
                Some("high"),
                Some("this is live"),
                None,
                Some(&brief()),
                900,
                "open",
                None,
                T0 + 900,
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        let row = db.snapshot(|tx| ask(tx, "a3")).unwrap().unwrap();
        assert_eq!(row.brief.as_ref().unwrap().problem, "Rebuild or wait?");
        assert_eq!(row.brief.as_ref().unwrap().options.len(), 1);
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute(
                "UPDATE gate_asks SET state = 'answered', answered_via = 'chat', answered_at = ?1
                 WHERE id = 'a3'",
                [T0 + 5_000],
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        let marked = db
            .write(|tx| review(tx, "a3", "didnt_need_me", None))
            .unwrap()
            .unwrap();
        assert_eq!(marked.review, "didnt_need_me");
        let ex = db.snapshot(|tx| examples(tx, "")).unwrap();
        assert_eq!(ex.len(), 2, "both kinds come back");

        // List filters and search.
        assert_eq!(
            db.snapshot(|tx| list(tx, "", "decided", "", 50, T0 + DAY))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            db.snapshot(|tx| list(tx, "", "asked", "", 50, T0 + DAY))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.snapshot(|tx| list(tx, "", "all", "Which file", 50, T0 + DAY))
                .unwrap()
                .len(),
            1,
            "search finds the question"
        );

        // Retention prunes; a held card the process never finished settles as failed.
        db.write(|tx| {
            open_ask(
                tx,
                "a4",
                "",
                &chat,
                "old",
                "req-4",
                "q",
                "{}",
                T0 - 200 * DAY,
            )?;
            open_ask(
                tx,
                "a5",
                "",
                &chat,
                "stuck",
                "req-5",
                "q",
                "{}",
                T0 - 10 * 60_000,
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        assert_eq!(db.write(|tx| prune(tx, T0)).unwrap(), 1);
        assert!(db.snapshot(|tx| ask(tx, "a4")).unwrap().is_none());
        assert_eq!(db.write(|tx| settle_orphans(tx, T0)).unwrap(), 1);
        assert_eq!(
            db.snapshot(|tx| ask(tx, "a5")).unwrap().unwrap().verdict,
            "failed"
        );

        // A verdict that never reached its card is listed under "all" only.
        db.write(|tx| {
            open_ask(tx, "a6", "", &chat, "t", "req-6", "Which colour?", "{}", T0)?;
            finish_ask(
                tx,
                "a6",
                "decided",
                None,
                Some("claude"),
                Some("sonnet"),
                Some(0.9),
                Some("low"),
                Some("r"),
                Some("Blue"),
                None,
                10,
                "withdrawn",
                None,
                T0 + 10,
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        let ids = |filter: &str, q: &str, profile: &str| -> Vec<String> {
            db.snapshot(|tx| list(tx, profile, filter, q, 50, T0 + DAY))
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect()
        };
        assert!(ids("all", "", "").contains(&"a6".to_owned()));
        for filter in ["decided", "asked", "failed", "review"] {
            assert!(
                !ids(filter, "", "").contains(&"a6".to_owned()),
                "a withdrawn row is not listed under {filter}"
            );
        }
        // Search takes % and _ literally.
        assert!(ids("all", "%", "").is_empty(), "no question holds a %");
        assert!(ids("all", "_", "").is_empty(), "nor an underscore");
        assert_eq!(ids("all", "Which colour", ""), ["a6"]);

        // Each person sees their own rows and the ones that belong to no one.
        db.write(|tx| {
            open_ask(tx, "p1-a", "p1", &chat, "t", "req-p1", "Mine?", "{}", T0)?;
            open_ask(tx, "p2-a", "p2", &chat, "t", "req-p2", "Theirs?", "{}", T0)?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        let p1 = ids("all", "", "p1");
        assert!(p1.contains(&"p1-a".to_owned()) && p1.contains(&"a6".to_owned()));
        assert!(!p1.contains(&"p2-a".to_owned()), "never another person's");
        assert!(!ids("all", "", "").contains(&"p1-a".to_owned()));

        // A mark has to fit the verdict.
        for (verdict, mark, fits) in [
            ("decided", "fine", true),
            ("decided", "should_have_asked", true),
            ("decided", "didnt_need_me", false),
            ("escalated", "didnt_need_me", true),
            ("escalated", "fine", false),
            ("escalated", "should_have_asked", false),
            ("failed", "fine", false),
        ] {
            assert_eq!(mark_fits(verdict, mark), fits, "{verdict} {mark}");
        }
        let unfit = db
            .write(|tx| review(tx, "a6", "didnt_need_me", None))
            .unwrap()
            .unwrap();
        assert_eq!(
            unfit.review, "pending",
            "the store refuses a mark that does not fit"
        );

        // A change whose message never went out is put back.
        assert!(
            !db.write(|tx| veto(tx, "a6", T0)).unwrap(),
            "a withdrawn row cannot be changed"
        );
        assert!(db.write(|tx| veto(tx, "a1", T0)).unwrap());
        assert!(db.write(|tx| unveto(tx, "a1", "open")).unwrap());
        assert_eq!(
            db.snapshot(|tx| ask(tx, "a1")).unwrap().unwrap().state,
            "open"
        );
        assert!(
            !db.write(|tx| unveto(tx, "a1", "open")).unwrap(),
            "only a vetoed row is put back"
        );

        // Settings save and load per person; another person gets the defaults.
        let s = GateSettings {
            enabled: false,
            ask: AskFrequency::More,
            policy: Some("mine".into()),
            model: "opus".into(),
        };
        db.write(|tx| save_settings(tx, "p1", &s)).unwrap();
        assert_eq!(db.snapshot(|tx| load_settings(tx, "p1")).unwrap(), s);
        assert_eq!(
            db.snapshot(|tx| load_settings(tx, "p2")).unwrap(),
            GateSettings::default()
        );

        // Whose chat: a chat with no person follows the machine's current person; without
        // the profiles tables at all the answer is the empty key.
        assert_eq!(
            db.snapshot(|tx| Ok::<String, rusqlite::Error>(chat_profile(tx, &chat)))
                .unwrap(),
            ""
        );
        db.write(|tx| {
            helm_core::prefs::set_raw(tx, "activeProfile", &Value::String("someone".into()))?;
            Ok::<(), helm_core::prefs::PrefError>(())
        })
        .unwrap();
        assert_eq!(
            db.snapshot(|tx| Ok::<String, rusqlite::Error>(chat_profile(tx, &chat)))
                .unwrap(),
            "someone"
        );
        assert_eq!(
            db.snapshot(|tx| Ok::<String, rusqlite::Error>(chat_profile(tx, "no-such-chat")))
                .unwrap(),
            "someone"
        );
        // While no profile is chosen, or all of them are shown, the person is the home
        // profile: the one every chat without its own is given at start.
        db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS profiles(id TEXT PRIMARY KEY, is_home INTEGER);
                 INSERT INTO profiles(id, is_home) VALUES ('home-1', 1), ('other', 0);",
            )
        })
        .unwrap();
        for pref in [
            Value::String("all".into()),
            Value::Null,
            Value::String("".into()),
        ] {
            db.write(|tx| {
                helm_core::prefs::set_raw(tx, "activeProfile", &pref)?;
                Ok::<(), helm_core::prefs::PrefError>(())
            })
            .unwrap();
            assert_eq!(
                db.snapshot(|tx| Ok::<String, rusqlite::Error>(active_profile(tx)))
                    .unwrap(),
                "home-1",
                "{pref:?}"
            );
        }
    }
}
