//! AFK's own tables: settings, spans and digest deliveries.
//! Chat ids carry no foreign key, so span and digest history survives a chat's deletion.

use crate::monitor::Source;
use crate::settings::{read_lenient, AfkSettings};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;

/// At most this many chats get a digest per span.
pub const MAX_DIGEST_CHATS: usize = 20;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS afk_settings (
             id     INTEGER PRIMARY KEY CHECK (id = 1),
             value  TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS afk_spans (
             id               TEXT PRIMARY KEY,
             started_at       INTEGER NOT NULL,
             ended_at         INTEGER,
             source           TEXT NOT NULL
                 CHECK (source IN ('manual', 'screenLock', 'sleep', 'idle', 'unanswered')),
             digest_recorded  INTEGER NOT NULL DEFAULT 0
         );
         CREATE UNIQUE INDEX IF NOT EXISTS afk_spans_one_open
             ON afk_spans((ended_at IS NULL)) WHERE ended_at IS NULL;
         CREATE TABLE IF NOT EXISTS afk_digests (
             span_id          TEXT NOT NULL REFERENCES afk_spans(id) ON DELETE CASCADE,
             chat_id          TEXT NOT NULL,
             submission_id    TEXT,
             state            TEXT NOT NULL
                 CHECK (state IN ('pending', 'queued', 'sent', 'failed', 'dismissed')),
             attempts         INTEGER NOT NULL DEFAULT 0,
             error            TEXT,
             next_attempt_at  INTEGER,
             created_at       INTEGER NOT NULL,
             PRIMARY KEY (span_id, chat_id)
         );
         CREATE INDEX IF NOT EXISTS afk_digests_state ON afk_digests(state);
         CREATE TABLE IF NOT EXISTS afk_events (
             id        INTEGER PRIMARY KEY AUTOINCREMENT,
             span_id   TEXT NOT NULL,
             chat_id   TEXT NOT NULL,
             kind      TEXT NOT NULL,
             title     TEXT NOT NULL,
             at        INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS afk_events_span ON afk_events(span_id, chat_id);",
    )
}

pub fn load_settings(tx: &Transaction<'_>) -> rusqlite::Result<AfkSettings> {
    let raw: Option<String> = tx
        .query_row("SELECT value FROM afk_settings WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .map(|v| read_lenient(&v))
        .unwrap_or_default())
}

pub fn save_settings(tx: &Transaction<'_>, s: &AfkSettings) -> rusqlite::Result<()> {
    let v = serde_json::to_string(s).expect("settings serialise");
    tx.execute(
        "INSERT INTO afk_settings (id, value) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET value = excluded.value",
        [v],
    )?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub source: Source,
}

fn span_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Span> {
    let src: String = r.get(3)?;
    Ok(Span {
        id: r.get(0)?,
        started_at: r.get(1)?,
        ended_at: r.get(2)?,
        source: Source::parse(&src).unwrap_or(Source::Manual),
    })
}

pub fn open_span(tx: &Transaction<'_>) -> rusqlite::Result<Option<Span>> {
    tx.query_row(
        "SELECT id, started_at, ended_at, source FROM afk_spans WHERE ended_at IS NULL",
        [],
        span_row,
    )
    .optional()
}

pub fn insert_span(tx: &Transaction<'_>, id: &str, src: Source, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO afk_spans (id, started_at, source) VALUES (?1, ?2, ?3)",
        params![id, now, src.as_str()],
    )?;
    Ok(())
}

/// Accidental: a short span in which AFK handled nothing yields no digest.
pub fn accidental(span: &Span, end: i64, handled: bool) -> bool {
    if handled {
        return false;
    }
    let len = end - span.started_at;
    if span.source.automatic() {
        len < 5 * 60_000
    } else {
        len < 60_000
    }
}

/// The chats that worked during `[start, end]`: a run that overlapped it, or a run left open by a
/// crash that started inside it or is still marked running. Unarchived, newest first, at most 20.
pub fn digest_targets(tx: &Transaction<'_>, start: i64, end: i64) -> rusqlite::Result<Vec<String>> {
    let mut st = tx.prepare(
        "SELECT c.id FROM chats c
         WHERE c.archived = 0 AND EXISTS (
             SELECT 1 FROM runs r WHERE r.chat_id = c.id AND r.started_at < ?2
               AND (r.ended_at > ?1
                    OR (r.ended_at IS NULL AND (r.started_at >= ?1 OR r.status = 'running'))))
         ORDER BY c.updated_at DESC LIMIT ?3",
    )?;
    let rows = st.query_map(params![start, end, MAX_DIGEST_CHATS as i64], |r| r.get(0))?;
    rows.collect()
}

/// Close the open span at `now`, recording a pending digest per target unless it was accidental.
/// Returns the closed span and how many digests it recorded.
pub fn close_span(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<Option<(Span, usize)>> {
    let Some(mut span) = open_span(tx)? else {
        return Ok(None);
    };
    let end = now.max(span.started_at);
    tx.execute(
        "UPDATE afk_spans SET ended_at = ?1 WHERE id = ?2",
        params![end, span.id],
    )?;
    span.ended_at = Some(end);
    let mut n = 0;
    let handled_chats = event_chats(tx, &span.id)?;
    if !accidental(&span, end, !handled_chats.is_empty()) {
        let mut targets = digest_targets(tx, span.started_at, end)?;
        for c in handled_chats {
            if !targets.contains(&c) && chat_available(tx, &c)? {
                targets.push(c);
            }
        }
        targets.truncate(MAX_DIGEST_CHATS);
        for chat in targets {
            n += tx.execute(
                "INSERT OR IGNORE INTO afk_digests (span_id, chat_id, state, created_at)
                 VALUES (?1, ?2, 'pending', ?3)",
                params![span.id, chat, now],
            )?;
        }
        tx.execute(
            "UPDATE afk_spans SET digest_recorded = 1 WHERE id = ?1",
            [&span.id],
        )?;
    }
    Ok(Some((span, n)))
}

pub fn span(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<Span>> {
    tx.query_row(
        "SELECT id, started_at, ended_at, source FROM afk_spans WHERE id = ?1",
        [id],
        span_row,
    )
    .optional()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestRow {
    pub span_id: String,
    pub chat_id: String,
    pub submission_id: Option<String>,
    pub state: String,
    pub attempts: u32,
    pub next_attempt_at: Option<i64>,
}

/// Rows the delivery loop still owns: pending (due or not) and queued.
pub fn live_digests(tx: &Transaction<'_>) -> rusqlite::Result<Vec<DigestRow>> {
    let mut st = tx.prepare(
        "SELECT span_id, chat_id, submission_id, state, attempts, next_attempt_at
         FROM afk_digests WHERE state IN ('pending', 'queued') ORDER BY created_at, chat_id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(DigestRow {
            span_id: r.get(0)?,
            chat_id: r.get(1)?,
            submission_id: r.get(2)?,
            state: r.get(3)?,
            attempts: r.get::<_, i64>(4)? as u32,
            next_attempt_at: r.get(5)?,
        })
    })?;
    rows.collect()
}

pub fn set_digest_state(
    tx: &Transaction<'_>,
    span_id: &str,
    chat_id: &str,
    state: &str,
    error: Option<&str>,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE afk_digests SET state = ?3, error = ?4 WHERE span_id = ?1 AND chat_id = ?2",
        params![span_id, chat_id, state, error],
    )?;
    Ok(())
}

/// Point the row at a new submission before it is sent, spending one attempt.
pub fn begin_attempt(
    tx: &Transaction<'_>,
    span_id: &str,
    chat_id: &str,
    submission_id: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE afk_digests SET submission_id = ?3, attempts = attempts + 1, error = NULL
         WHERE span_id = ?1 AND chat_id = ?2",
        params![span_id, chat_id, submission_id],
    )?;
    Ok(())
}

pub fn retry_later(
    tx: &Transaction<'_>,
    span_id: &str,
    chat_id: &str,
    error: &str,
    at: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE afk_digests SET state = 'pending', error = ?3, next_attempt_at = ?4
         WHERE span_id = ?1 AND chat_id = ?2",
        params![span_id, chat_id, error, at],
    )?;
    Ok(())
}

/// The submission's state, if it exists.
pub fn submission_state(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row("SELECT state FROM submissions WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .optional()
}

/// `(exists, archived)` for a chat.
pub fn chat_available(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row("SELECT archived FROM chats WHERE id = ?1", [id], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?
        .is_some_and(|a| a == 0))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestView {
    pub chat_id: String,
    pub title: String,
    pub state: String,
    pub attempts: u32,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanDigests {
    pub span: Span,
    pub digests: Vec<DigestView>,
}

fn digests_of(tx: &Transaction<'_>, span_id: &str) -> rusqlite::Result<Vec<DigestView>> {
    let mut st = tx.prepare(
        "SELECT d.chat_id, COALESCE(c.title, ''), d.state, d.attempts, d.error
         FROM afk_digests d LEFT JOIN chats c ON c.id = d.chat_id
         WHERE d.span_id = ?1 ORDER BY d.created_at, d.chat_id",
    )?;
    let rows = st.query_map([span_id], |r| {
        Ok(DigestView {
            chat_id: r.get(0)?,
            title: r.get(1)?,
            state: r.get(2)?,
            attempts: r.get::<_, i64>(3)? as u32,
            error: r.get(4)?,
        })
    })?;
    rows.collect()
}

/// The most recently closed span and its digests, for Settings.
pub fn last_closed(tx: &Transaction<'_>) -> rusqlite::Result<Option<SpanDigests>> {
    let Some(span) = tx
        .query_row(
            "SELECT id, started_at, ended_at, source FROM afk_spans
             WHERE ended_at IS NOT NULL ORDER BY ended_at DESC, started_at DESC LIMIT 1",
            [],
            span_row,
        )
        .optional()?
    else {
        return Ok(None);
    };
    let digests = digests_of(tx, &span.id)?;
    Ok(Some(SpanDigests { span, digests }))
}

/// The latest span with failed digests nobody dismissed.
pub fn failures(tx: &Transaction<'_>) -> rusqlite::Result<Option<SpanDigests>> {
    let Some(id) = tx
        .query_row(
            "SELECT d.span_id FROM afk_digests d JOIN afk_spans s ON s.id = d.span_id
             WHERE d.state = 'failed' ORDER BY s.ended_at DESC LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let Some(span) = span(tx, &id)? else {
        return Ok(None);
    };
    let digests = digests_of(tx, &id)?
        .into_iter()
        .filter(|d| d.state == "failed")
        .collect();
    Ok(Some(SpanDigests { span, digests }))
}

/// Failed rows back to pending with fresh attempts.
pub fn retry_failed(tx: &Transaction<'_>, span_id: &str) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE afk_digests SET state = 'pending', attempts = 0, error = NULL, next_attempt_at = NULL
         WHERE span_id = ?1 AND state = 'failed'",
        [span_id],
    )
}

pub fn dismiss_failed(tx: &Transaction<'_>, span_id: &str) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE afk_digests SET state = 'dismissed' WHERE span_id = ?1 AND state = 'failed'",
        [span_id],
    )
}

pub fn any_span(tx: &Transaction<'_>) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row("SELECT 1 FROM afk_spans LIMIT 1", [], |_| Ok(()))
        .optional()?
        .is_some())
}

/// What AFK did with one ask, kept for the digest.
pub fn record_event(
    tx: &Transaction<'_>,
    span_id: &str,
    chat_id: &str,
    kind: &str,
    title: &str,
    at: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO afk_events (span_id, chat_id, kind, title, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![span_id, chat_id, kind, title, at],
    )?;
    Ok(())
}

/// Chats AFK handled something in during the span.
pub fn event_chats(tx: &Transaction<'_>, span_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut st = tx.prepare(
        "SELECT chat_id FROM afk_events WHERE span_id = ?1 GROUP BY chat_id ORDER BY max(at) DESC",
    )?;
    let rows = st.query_map([span_id], |r| r.get(0))?;
    rows.collect()
}

pub struct EventRow {
    pub kind: String,
    pub title: String,
    pub at: i64,
}

/// The events of one chat in one span, oldest first (at most 40).
pub fn chat_events(
    tx: &Transaction<'_>,
    span_id: &str,
    chat_id: &str,
) -> rusqlite::Result<Vec<EventRow>> {
    let mut st = tx.prepare(
        "SELECT kind, title, at FROM afk_events WHERE span_id = ?1 AND chat_id = ?2
         ORDER BY id LIMIT 40",
    )?;
    let rows = st.query_map(params![span_id, chat_id], |r| {
        Ok(EventRow {
            kind: r.get(0)?,
            title: r.get(1)?,
            at: r.get(2)?,
        })
    })?;
    rows.collect()
}

/// Allow requests the chat filed since `since` that nobody decided (still waiting, or expired).
/// The consent tables belong to another crate; without them the answer is none.
pub fn undecided_allows(tx: &Transaction<'_>, chat_id: &str, since: i64) -> Vec<String> {
    let Ok(mut st) = tx.prepare(
        "SELECT summary FROM consent_requests
         WHERE chat_id = ?1 AND created_at >= ?2 AND state IN ('pending', 'expired')
         ORDER BY created_at LIMIT 20",
    ) else {
        return vec![];
    };
    st.query_map(params![chat_id, since], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}
