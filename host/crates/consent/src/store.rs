//! The consent store.
//!
//! | Table | Holds |
//! |---|---|
//! | `consent_requests` | Allow requests and, once allowed, their grant (pending → allowed/denied/expired; allowed → used/expired); for an applying kind also `outcome` (what applying did) and `decided_via` (where the user answered: `app`) |
//! | `consent_asks` | refusals the app recorded itself; each opens a 2 h words episode for (chat, kind, target) |
//! | `consent_records` | one row per use of a proof (grant or words); a submission is spent at most once |
//!
//! Only [`decide`] mints a grant, and it is reached only through the main-window `consent_decide`
//! op. [`spend_grant`] and [`spend_words`] run on the caller's own transaction, after every check
//! that needs no action, so the spend and the action's first change commit together. An applying
//! kind's grant is spent only by [`spend_for_apply`], by request id, just before Helm applies it.

use crate::kinds::{first_difference, target_hash, AllowKind, AllowKinds, Canonical, MAX_TARGET};
use crate::types::{ConsentList, ConsentRequest, Decision, RequestState};
use crate::words;
use helm_core::db::Database;
use helm_ipc::ApiError;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;

pub const MINUTE: i64 = 60_000;
pub const HOUR: i64 = 60 * MINUTE;
/// A pending request's life.
pub const PENDING_LIFE: i64 = 24 * HOUR;
/// A words episode's length after a recorded refusal.
pub const EPISODE: i64 = 2 * HOUR;
/// How many typed messages after the refusal may verify it.
pub const EPISODE_MESSAGES: usize = 2;
pub const MAX_PENDING_PER_CHAT: i64 = 4;
pub const MAX_CREATED_PER_HOUR: i64 = 20;
pub const MAX_PENDING_APP: i64 = 50;
pub const DEFAULT_GRANT_MINUTES: i64 = 30;
pub const AGENT_GRANT_MINUTES: std::ops::RangeInclusive<i64> = 5..=120;
pub const USER_GRANT_MINUTES: std::ops::RangeInclusive<i64> = 5..=240;
pub const MAX_REASON: usize = 1_000;
pub const MAX_EVIDENCE: usize = 4_000;
pub const MAX_NOTE: usize = 500;
pub const MAX_DECISION_MESSAGE: usize = 2_048;
/// A refusal body is cut to this before the Allow hint, so the hint is never cut.
pub const MAX_REFUSAL_BODY: usize = 1_600;
pub const LIST_PENDING: i64 = 50;
pub const LIST_DECIDED: i64 = 20;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS consent_requests (
             id                TEXT PRIMARY KEY,
             chat_id           TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             kind              TEXT NOT NULL,
             target            TEXT NOT NULL,
             target_hash       TEXT NOT NULL,
             summary           TEXT NOT NULL,
             reason            TEXT NOT NULL,
             evidence          TEXT NOT NULL DEFAULT '',
             tool_use_id       TEXT,
             state             TEXT NOT NULL
                 CHECK (state IN ('pending', 'allowed', 'denied', 'expired', 'used')),
             grant_minutes     INTEGER NOT NULL,
             created_at        INTEGER NOT NULL,
             expires_at        INTEGER NOT NULL,
             decided_at        INTEGER,
             grant_expires_at  INTEGER,
             note              TEXT,
             used_at           INTEGER,
             delivered_at      INTEGER
         );
         CREATE INDEX IF NOT EXISTS consent_requests_chat
             ON consent_requests(chat_id, state, created_at);
         CREATE INDEX IF NOT EXISTS consent_requests_undelivered
             ON consent_requests(state) WHERE delivered_at IS NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS consent_requests_tool_use
             ON consent_requests(chat_id, tool_use_id) WHERE tool_use_id IS NOT NULL;
         CREATE TABLE IF NOT EXISTS consent_asks (
             id           TEXT PRIMARY KEY,
             chat_id      TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             kind         TEXT NOT NULL,
             target_hash  TEXT NOT NULL,
             created_at   INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS consent_asks_episode
             ON consent_asks(chat_id, kind, target_hash, created_at);
         CREATE TABLE IF NOT EXISTS consent_records (
             id             TEXT PRIMARY KEY,
             chat_id        TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             kind           TEXT NOT NULL,
             target_hash    TEXT NOT NULL,
             proof          TEXT NOT NULL CHECK (proof IN ('grant', 'words')),
             request_id     TEXT,
             submission_id  TEXT,
             words          TEXT,
             used_by        TEXT NOT NULL,
             created_at     INTEGER NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS consent_records_submission
             ON consent_records(submission_id) WHERE submission_id IS NOT NULL;",
    )?;
    // Columns added after the table first shipped: added once, checked on every open.
    for (column, ty) in [("outcome", "TEXT"), ("decided_via", "TEXT")] {
        let has: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info('consent_requests') WHERE name = ?1)",
            [column],
            |r| r.get(0),
        )?;
        if !has {
            tx.execute_batch(&format!(
                "ALTER TABLE consent_requests ADD COLUMN {column} {ty}"
            ))?;
        }
    }
    Ok(())
}

/// Why consent was not given. The text is safe to show the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

impl Refusal {
    fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Self {
        Self(format!("The consent store failed: {e}"))
    }
}

/// One use of a proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsentRecord {
    pub id: String,
    pub proof: &'static str,
    pub request_id: Option<String>,
    pub submission_id: Option<String>,
}

fn kind<'a>(kinds: &'a AllowKinds, id: &str) -> Result<&'a AllowKind, Refusal> {
    kinds
        .get(id)
        .ok_or_else(|| Refusal::new(format!("There is no Allow action called \"{id}\".")))
}

fn build(
    tx: &Transaction<'_>,
    k: &AllowKind,
    chat_id: &str,
    target: &Value,
) -> Result<(Canonical, String), Refusal> {
    let c = (k.canonical)(tx, chat_id, target).map_err(Refusal)?;
    let json = crate::kinds::canonical_json(&c.target);
    if json.len() > MAX_TARGET {
        return Err(Refusal::new("The target is too large."));
    }
    let hash = target_hash(&c.target);
    Ok((c, hash))
}

fn cut(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// A summary as stored: an ordinary kind's on one line; an applying kind's as lines, each cut to
/// 240 characters and the whole to 1,200.
fn card_summary(summary: &str, applying: bool) -> String {
    use crate::kinds::{MAX_SUMMARY, MAX_SUMMARY_LINE};
    if !applying {
        return cut(&summary.replace(['\r', '\n'], " "), MAX_SUMMARY_LINE);
    }
    let lines: Vec<String> = summary
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| cut(l, MAX_SUMMARY_LINE))
        .collect();
    cut(&lines.join("\n"), MAX_SUMMARY)
}

/// The first line of a summary: the card's title.
pub fn summary_title(summary: &str) -> &str {
    summary.lines().next().unwrap_or("")
}

/// What an agent asks for (`app_request_allow`).
pub struct NewRequest<'a> {
    pub chat_id: &'a str,
    pub kind: &'a str,
    pub target: &'a Value,
    pub reason: &'a str,
    pub evidence: &'a str,
    pub grant_minutes: Option<i64>,
    pub tool_use_id: Option<&'a str>,
}

/// File a request (pending). Idempotent per tool-use id; a second ask for the same pending target
/// returns the pending one.
pub fn request(
    db: &Database,
    kinds: &AllowKinds,
    r: NewRequest<'_>,
    now: i64,
) -> Result<ConsentRequest, Refusal> {
    let k = kind(kinds, r.kind)?;
    let reason = r.reason.trim();
    if reason.is_empty() || reason.chars().count() > MAX_REASON {
        return Err(Refusal::new("The reason must be 1-1000 characters."));
    }
    if r.evidence.chars().count() > MAX_EVIDENCE {
        return Err(Refusal::new(
            "The evidence must be at most 4000 characters.",
        ));
    }
    let minutes = r.grant_minutes.unwrap_or(DEFAULT_GRANT_MINUTES);
    if !AGENT_GRANT_MINUTES.contains(&minutes) {
        return Err(Refusal::new("expiresIn must be 5-120 minutes."));
    }
    db.write::<_, Refusal>(|tx| {
        if let Some(id) = r.tool_use_id {
            if let Some(existing) = by_tool_use(tx, r.chat_id, id)? {
                return Ok(view(tx, kinds, &existing)?.expect("row exists"));
            }
        }
        let (c, hash) = build(tx, k, r.chat_id, r.target)?;
        let same: Option<String> = tx
            .query_row(
                "SELECT id FROM consent_requests WHERE chat_id = ?1 AND kind = ?2
                 AND target_hash = ?3 AND state = 'pending' AND expires_at > ?4",
                params![r.chat_id, k.id, hash, now],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = same {
            return Ok(view(tx, kinds, &id)?.expect("row exists"));
        }
        let count = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> rusqlite::Result<i64> {
            tx.query_row(sql, p, |row| row.get(0))
        };
        let pending_chat = count(
            "SELECT COUNT(*) FROM consent_requests WHERE chat_id = ?1 AND state = 'pending' AND expires_at > ?2",
            &[&r.chat_id, &now],
        )?;
        if pending_chat >= MAX_PENDING_PER_CHAT {
            return Err(Refusal::new(
                "This chat already has 4 Allow requests waiting for the user.",
            ));
        }
        let hour = count(
            "SELECT COUNT(*) FROM consent_requests WHERE chat_id = ?1 AND created_at > ?2",
            &[&r.chat_id, &(now - HOUR)],
        )?;
        if hour >= MAX_CREATED_PER_HOUR {
            return Err(Refusal::new(
                "This chat has made 20 Allow requests in the last hour.",
            ));
        }
        let app = count(
            "SELECT COUNT(*) FROM consent_requests WHERE state = 'pending' AND expires_at > ?1",
            &[&now],
        )?;
        if app >= MAX_PENDING_APP {
            return Err(Refusal::new(
                "50 Allow requests are already waiting across Helm.",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO consent_requests(id, chat_id, kind, target, target_hash, summary, reason,
                 evidence, tool_use_id, state, grant_minutes, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?11, ?12)",
            params![
                id,
                r.chat_id,
                k.id,
                crate::kinds::canonical_json(&c.target),
                hash,
                card_summary(&c.summary, kinds.applies_on_allow(k.id)),
                reason,
                r.evidence,
                r.tool_use_id,
                minutes,
                now,
                now + PENDING_LIFE
            ],
        )?;
        Ok(view(tx, kinds, &id)?.expect("just inserted"))
    })
}

fn by_tool_use(tx: &Transaction<'_>, chat: &str, id: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT id FROM consent_requests WHERE chat_id = ?1 AND tool_use_id = ?2",
        [chat, id],
        |r| r.get(0),
    )
    .optional()
}

const COLUMNS: &str = "id, chat_id, kind, summary, reason, evidence, state, grant_minutes,
    created_at, expires_at, decided_at, grant_expires_at, note, outcome";

fn row_to_view(kinds: &AllowKinds, r: &rusqlite::Row<'_>) -> rusqlite::Result<ConsentRequest> {
    let kind: String = r.get(2)?;
    Ok(ConsentRequest {
        id: r.get(0)?,
        chat_id: r.get(1)?,
        label: kinds
            .get(&kind)
            .map(|k| k.label.to_owned())
            .unwrap_or_else(|| kind.clone()),
        summary: r.get(3)?,
        reason: r.get(4)?,
        evidence: r.get(5)?,
        state: RequestState::parse(&r.get::<_, String>(6)?),
        grant_minutes: r.get(7)?,
        created_at: r.get(8)?,
        expires_at: r.get(9)?,
        decided_at: r.get(10)?,
        grant_expires_at: r.get(11)?,
        note: r.get(12)?,
        applies_on_allow: kinds.applies_on_allow(&kind),
        outcome: r.get(13)?,
        kind,
    })
}

fn view(
    tx: &Transaction<'_>,
    kinds: &AllowKinds,
    id: &str,
) -> rusqlite::Result<Option<ConsentRequest>> {
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM consent_requests WHERE id = ?1"),
        [id],
        |r| row_to_view(kinds, r),
    )
    .optional()
}

/// The chat's cards: pending (≤ 50, oldest first, unexpired) and decided (≤ 20, newest first).
pub fn list(
    db: &Database,
    kinds: &AllowKinds,
    chat_id: &str,
    now: i64,
) -> Result<ConsentList, ApiError> {
    db.snapshot(|tx| {
        let mut st = tx.prepare(&format!(
            "SELECT {COLUMNS} FROM consent_requests WHERE chat_id = ?1 AND state = 'pending'
             AND expires_at > ?2 ORDER BY created_at, id LIMIT {LIST_PENDING}"
        ))?;
        let pending = st
            .query_map(params![chat_id, now], |r| row_to_view(kinds, r))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut st = tx.prepare(&format!(
            "SELECT {COLUMNS} FROM consent_requests WHERE chat_id = ?1 AND state <> 'pending'
             ORDER BY COALESCE(decided_at, expires_at) DESC, id LIMIT {LIST_DECIDED}"
        ))?;
        let decided = st
            .query_map([chat_id], |r| row_to_view(kinds, r))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ConsentList { pending, decided })
    })
    .map_err(|e: rusqlite::Error| ApiError::internal(e.to_string()))
}

/// The user's decision in this PC's main window: [`decide_via`] with `app`.
pub fn decide(
    db: &Database,
    kinds: &AllowKinds,
    request_id: &str,
    decision: Decision,
    grant_minutes: Option<i64>,
    note: Option<&str>,
    now: i64,
) -> Result<ConsentRequest, ApiError> {
    decide_via(
        db,
        kinds,
        request_id,
        decision,
        grant_minutes,
        note,
        "app",
        now,
    )
}

/// The user's decision: one compare-and-set out of `pending`. A repeat, an outdated card or a
/// race is refused with `conflict`, so nothing is applied twice. `via` records where the user
/// answered.
#[allow(clippy::too_many_arguments)]
pub fn decide_via(
    db: &Database,
    kinds: &AllowKinds,
    request_id: &str,
    decision: Decision,
    grant_minutes: Option<i64>,
    note: Option<&str>,
    via: &str,
    now: i64,
) -> Result<ConsentRequest, ApiError> {
    if let Some(n) = note {
        if n.chars().count() > MAX_NOTE {
            return Err(ApiError::bad_args(
                "The note must be at most 500 characters.",
            ));
        }
    }
    if let Some(m) = grant_minutes {
        if !USER_GRANT_MINUTES.contains(&m) {
            return Err(ApiError::bad_args("A grant lasts 5-240 minutes."));
        }
    }
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    db.write::<_, rusqlite::Error>(|tx| {
        let changed = match decision {
            Decision::Allow => tx.execute(
                "UPDATE consent_requests SET state = 'allowed', decided_at = ?2,
                     grant_minutes = COALESCE(?3, grant_minutes),
                     grant_expires_at = ?2 + COALESCE(?3, grant_minutes) * 60000,
                     note = ?4, decided_via = ?5, delivered_at = NULL
                 WHERE id = ?1 AND state = 'pending' AND expires_at > ?2",
                params![request_id, now, grant_minutes, note, via],
            )?,
            Decision::Deny => tx.execute(
                "UPDATE consent_requests SET state = 'denied', decided_at = ?2, note = ?3,
                     decided_via = ?4, delivered_at = NULL
                 WHERE id = ?1 AND state = 'pending' AND expires_at > ?2",
                params![request_id, now, note, via],
            )?,
        };
        Ok((changed, view(tx, kinds, request_id)?))
    })
    .map_err(|e| ApiError::internal(e.to_string()))
    .and_then(|(changed, v)| match (changed, v) {
        (1, Some(v)) => Ok(v),
        (_, None) => Err(ApiError::new("not_found", "No such Allow request.")),
        _ => Err(ApiError::conflict(
            "This request was already answered or has expired.",
        )),
    })
}

/// Record a refusal the app made itself, opening a 2 h words episode for this target.
pub fn record_ask(
    tx: &Transaction<'_>,
    kinds: &AllowKinds,
    chat_id: &str,
    kind_id: &str,
    target: &Value,
    now: i64,
) -> Result<(), Refusal> {
    let k = kind(kinds, kind_id)?;
    let (_, hash) = build(tx, k, chat_id, target)?;
    tx.execute(
        "INSERT INTO consent_asks(id, chat_id, kind, target_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![uuid::Uuid::new_v4().to_string(), chat_id, k.id, hash, now],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_record(
    tx: &Transaction<'_>,
    chat_id: &str,
    kind: &str,
    hash: &str,
    proof: &'static str,
    request_id: Option<&str>,
    submission: Option<(&str, &str)>,
    used_by: &str,
    now: i64,
) -> rusqlite::Result<ConsentRecord> {
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO consent_records(id, chat_id, kind, target_hash, proof, request_id,
             submission_id, words, used_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            chat_id,
            kind,
            hash,
            proof,
            request_id,
            submission.map(|s| s.0),
            submission.map(|s| s.1),
            cut(used_by, 64),
            now
        ],
    )?;
    Ok(ConsentRecord {
        id,
        proof,
        request_id: request_id.map(str::to_owned),
        submission_id: submission.map(|s| s.0.to_owned()),
    })
}

/// Spend this chat's Allow grant for exactly this target, on the action's own transaction. A
/// refusal leaves any grant allowed.
pub fn spend_grant(
    tx: &Transaction<'_>,
    kinds: &AllowKinds,
    chat_id: &str,
    kind_id: &str,
    target: &Value,
    used_by: &str,
    now: i64,
) -> Result<ConsentRecord, Refusal> {
    let k = kind(kinds, kind_id)?;
    if kinds.applies_on_allow(k.id) {
        return Err(Refusal::new(
            "Helm applies this itself when the user approves; there is nothing to spend.",
        ));
    }
    let (c, hash) = build(tx, k, chat_id, target)?;
    let grant: Option<String> = tx
        .query_row(
            "SELECT id FROM consent_requests WHERE chat_id = ?1 AND kind = ?2 AND target_hash = ?3
             AND state = 'allowed' AND grant_expires_at > ?4 ORDER BY decided_at LIMIT 1",
            params![chat_id, k.id, hash, now],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = grant {
        let n = tx.execute(
            "UPDATE consent_requests SET state = 'used', used_at = ?2,
                 delivered_at = COALESCE(delivered_at, ?2)
             WHERE id = ?1 AND state = 'allowed'",
            params![id, now],
        )?;
        if n == 1 {
            return Ok(insert_record(
                tx,
                chat_id,
                k.id,
                &hash,
                "grant",
                Some(&id),
                None,
                used_by,
                now,
            )?);
        }
    }
    // Name the first differing field when the chat holds a grant for something else.
    let other: Option<String> = tx
        .query_row(
            "SELECT target FROM consent_requests WHERE chat_id = ?1 AND kind = ?2
             AND state = 'allowed' AND grant_expires_at > ?3 LIMIT 1",
            params![chat_id, k.id, now],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(t) = other.and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        return Err(Refusal::new(format!(
            "The user's Allow is for a different {}.",
            first_difference(&t, &c.target)
        )));
    }
    Err(Refusal::new(format!(
        "There is no unused Allow from the user for: {}.",
        c.summary
    )))
}

/// Verify the user's own typed words for this target without spending them. `Ok` carries the
/// submission id and the matched sentence.
pub fn verify_words(
    tx: &Transaction<'_>,
    kinds: &AllowKinds,
    chat_id: &str,
    kind_id: &str,
    target: &Value,
    now: i64,
) -> Result<(String, String), Refusal> {
    let k = kind(kinds, kind_id)?;
    if !k.accepts_words {
        return Err(Refusal::new(
            "This step needs the user's click on an Allow card.",
        ));
    }
    let (c, hash) = build(tx, k, chat_id, target)?;
    let ask: Option<i64> = tx
        .query_row(
            "SELECT MAX(created_at) FROM consent_asks WHERE chat_id = ?1 AND kind = ?2
             AND target_hash = ?3 AND created_at > ?4",
            params![chat_id, k.id, hash, now - EPISODE],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(asked) = ask else {
        return Err(Refusal::new(
            "Ask first: make the call once without consent so Helm records the refusal, then ask \
             the user in this chat.",
        ));
    };
    // The first two typed messages after the refusal, within the episode. Only `typed` is the user
    // typing: the early `user` spelling is rewritten to `typed` once at open (chats schema), so
    // a row that took the column's `user` default, or any app-originated origin, never counts.
    let mut st = tx.prepare(
        "SELECT s.id, s.text, EXISTS (SELECT 1 FROM consent_records r WHERE r.submission_id = s.id)
         FROM submissions s JOIN submission_meta m ON m.submission_id = s.id
         WHERE s.chat_id = ?1 AND m.origin = 'typed'
           AND s.created_at > ?2 AND s.created_at <= ?3
         ORDER BY s.created_at, s.id LIMIT ?4",
    )?;
    let rows = st
        .query_map(
            params![
                chat_id,
                asked,
                (asked + EPISODE).min(now),
                EPISODE_MESSAGES as i64
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut why = "the user has not answered in this chat since the refusal";
    for (id, text, spent) in rows {
        if spent {
            why = "the user's message was already used";
            continue;
        }
        match words::check(&text, &c.fixed_form) {
            Ok(sentence) => return Ok((id, sentence)),
            Err(e) => why = e,
        }
    }
    Err(Refusal::new(format!(
        "The user's own words do not confirm this ({why}). They must type exactly: {}",
        c.fixed_form
    )))
}

/// Verify and spend the user's words, on the action's own transaction.
pub fn spend_words(
    tx: &Transaction<'_>,
    kinds: &AllowKinds,
    chat_id: &str,
    kind_id: &str,
    target: &Value,
    used_by: &str,
    now: i64,
) -> Result<ConsentRecord, Refusal> {
    let (submission, sentence) = verify_words(tx, kinds, chat_id, kind_id, target, now)?;
    let k = kind(kinds, kind_id)?;
    let (_, hash) = build(tx, k, chat_id, target)?;
    Ok(insert_record(
        tx,
        chat_id,
        k.id,
        &hash,
        "words",
        None,
        Some((&submission, &sentence)),
        used_by,
        now,
    )?)
}

/// What an approved applying request asks Helm to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Approved {
    pub request_id: String,
    pub chat_id: String,
    pub kind: String,
    pub target: Value,
}

/// Spend an applying kind's grant, by request id: one compare-and-set from `allowed` to `used`,
/// committed before anything is applied. `None` when it is not (or no longer) an unexpired
/// grant of an applying kind, so nothing may be applied.
pub fn spend_for_apply(
    db: &Database,
    kinds: &AllowKinds,
    request_id: &str,
    now: i64,
) -> Result<Option<Approved>, ApiError> {
    db.write::<_, rusqlite::Error>(|tx| {
        let row: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT chat_id, kind, target, target_hash FROM consent_requests
                 WHERE id = ?1 AND state = 'allowed' AND grant_expires_at > ?2",
                params![request_id, now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((chat_id, kind, target, hash)) = row else {
            return Ok(None);
        };
        if !kinds.applies_on_allow(&kind) {
            return Ok(None);
        }
        // The note waits for the outcome: upkeep delivers a used row once it has one.
        let n = tx.execute(
            "UPDATE consent_requests SET state = 'used', used_at = ?2, delivered_at = NULL
             WHERE id = ?1 AND state = 'allowed'",
            params![request_id, now],
        )?;
        if n != 1 {
            return Ok(None);
        }
        insert_record(
            tx,
            &chat_id,
            &kind,
            &hash,
            "grant",
            Some(request_id),
            None,
            "apply",
            now,
        )?;
        Ok(Some(Approved {
            request_id: request_id.to_owned(),
            chat_id,
            kind,
            target: serde_json::from_str(&target).unwrap_or(Value::Null),
        }))
    })
    .map_err(|e| ApiError::internal(e.to_string()))
}

/// Longest stored outcome.
pub const MAX_OUTCOME: usize = 1_200;

/// How long an applying request may sit used with no outcome before upkeep reports that Helm
/// stopped while applying it.
pub const APPLY_STALE: i64 = 10 * MINUTE;

/// Record what applying did, once; the agent's note goes out at the next upkeep.
pub fn set_outcome(db: &Database, request_id: &str, outcome: &str) -> Result<(), ApiError> {
    db.write::<_, rusqlite::Error>(|tx| {
        tx.execute(
            "UPDATE consent_requests SET outcome = ?2
             WHERE id = ?1 AND state = 'used' AND outcome IS NULL",
            params![request_id, cut(outcome, MAX_OUTCOME)],
        )
        .map(|_| ())
    })
    .map_err(|e| ApiError::internal(e.to_string()))
}

/// The fixed Allow hint appended to a refusal that consent could unlock.
/// The body is cut to 1,600 characters first, so the hint is never cut or doubled. No
/// hint while a request for this target is pending or after the user denied one.
pub fn with_allow_hint(
    tx: &Transaction<'_>,
    kinds: &AllowKinds,
    chat_id: &str,
    kind_id: &str,
    target: &Value,
    body: &str,
) -> Result<String, Refusal> {
    let k = kind(kinds, kind_id)?;
    let (_, hash) = build(tx, k, chat_id, target)?;
    let hint = format!(
        "To ask for Allow instead (action \"{}\"), call app_request_allow {{action, target, reason}} to ask the user.",
        k.id
    );
    let body = body.strip_suffix(&hint).unwrap_or(body).trim_end();
    let blocked: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM consent_requests WHERE chat_id = ?1 AND kind = ?2
         AND target_hash = ?3 AND state IN ('pending', 'denied'))",
        params![chat_id, k.id, hash],
        |r| r.get(0),
    )?;
    let body = cut(body, MAX_REFUSAL_BODY);
    Ok(if blocked {
        body
    } else {
        format!("{body} {hint}")
    })
}

/// A decision owed to an agent, as an app note under a fixed submission id (delivered at most
/// once).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub request_id: String,
    pub chat_id: String,
    pub submission_id: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Upkeep {
    /// Chats whose cards changed (send each a `consent` event).
    pub changed_chats: Vec<String>,
    /// Decisions not yet delivered, including the ones this tick expired.
    pub deliveries: Vec<Delivery>,
}

/// The one-minute tick: expire overdue requests and grants, prune old asks, and list the
/// decisions still owed to agents (the caller sends them, then calls [`mark_delivered`]).
pub fn upkeep(db: &Database, kinds: &AllowKinds, now: i64) -> Result<Upkeep, ApiError> {
    db.write::<_, rusqlite::Error>(|tx| {
        let mut changed: Vec<String> = Vec::new();
        {
            let mut st = tx.prepare(
                "UPDATE consent_requests SET state = 'expired', delivered_at = NULL
                 WHERE state = 'pending' AND expires_at <= ?1 RETURNING chat_id",
            )?;
            changed.extend(
                st.query_map([now], |r| r.get(0))?
                    .collect::<Result<Vec<String>, _>>()?,
            );
            let mut st = tx.prepare(
                "UPDATE consent_requests SET state = 'expired'
                 WHERE state = 'allowed' AND grant_expires_at <= ?1 RETURNING chat_id",
            )?;
            changed.extend(
                st.query_map([now], |r| r.get(0))?
                    .collect::<Result<Vec<String>, _>>()?,
            );
        }
        tx.execute(
            "DELETE FROM consent_asks WHERE created_at <= ?1",
            [now - PENDING_LIFE],
        )?;
        changed.sort();
        changed.dedup();
        let mut st = tx.prepare(&format!(
            "SELECT {COLUMNS}, used_at FROM consent_requests
             WHERE delivered_at IS NULL AND state <> 'pending'
             ORDER BY COALESCE(decided_at, expires_at), id"
        ))?;
        let deliveries = st
            .query_map([], |r| {
                Ok((row_to_view(kinds, r)?, r.get::<_, Option<i64>>(14)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            // A request Helm applies itself is owed its note once applied: never while only
            // allowed (applying is about to start), and while used only once the outcome is
            // written or applying has plainly stopped.
            .filter(|(v, used_at)| {
                !v.applies_on_allow
                    || match v.state {
                        RequestState::Allowed => false,
                        RequestState::Used => {
                            v.outcome.is_some() || used_at.is_none_or(|u| u <= now - APPLY_STALE)
                        }
                        _ => true,
                    }
            })
            .map(|(v, _)| Delivery {
                // The request id is the submission id: a UUID, fixed per request, so a
                // retried delivery is the same submission and is held at most once.
                submission_id: v.id.clone(),
                text: decision_text(&v),
                request_id: v.id,
                chat_id: v.chat_id,
            })
            .collect();
        Ok(Upkeep {
            changed_chats: changed,
            deliveries,
        })
    })
    .map_err(|e| ApiError::internal(e.to_string()))
}

pub fn mark_delivered(db: &Database, request_id: &str, now: i64) -> Result<(), ApiError> {
    db.write::<_, rusqlite::Error>(|tx| {
        tx.execute(
            "UPDATE consent_requests SET delivered_at = ?2 WHERE id = ?1 AND delivered_at IS NULL",
            params![request_id, now],
        )
        .map(|_| ())
    })
    .map_err(|e| ApiError::internal(e.to_string()))
}

/// The app note an agent gets for a decision (≤ 2,048 characters; never the user's words).
pub fn decision_text(v: &ConsentRequest) -> String {
    if v.applies_on_allow {
        return applied_text(v);
    }
    let what = format!("{}: {}", v.label, v.summary);
    let mut s = match (v.state, v.decided_at) {
        (RequestState::Allowed, _) | (RequestState::Used, _) => format!(
            "[Helm] The user allowed your request ({what}). The grant is single-use and lasts {} minutes; make the call now.",
            v.grant_minutes
        ),
        (RequestState::Denied, _) => format!("[Helm] The user denied your request ({what})."),
        (RequestState::Expired, None) => {
            format!("[Helm] Your Allow request ({what}) expired without an answer.")
        }
        (RequestState::Expired, Some(_)) => {
            format!("[Helm] The user's Allow for ({what}) expired unused.")
        }
        (RequestState::Pending, _) => format!("[Helm] Your Allow request ({what}) is waiting."),
    };
    if let Some(n) = &v.note {
        s.push_str(&format!(" The user's note: {n}"));
    }
    cut(&s, MAX_DECISION_MESSAGE)
}

/// The note for a request Helm applies itself: never a grant to spend, always what changed.
fn applied_text(v: &ConsentRequest) -> String {
    let title = summary_title(&v.summary);
    let mut s = match (v.state, v.decided_at, &v.outcome) {
        (RequestState::Used, _, Some(outcome)) => {
            format!("[Helm] The user approved: {title}. {outcome}")
        }
        (RequestState::Used, _, None) => format!(
            "[Helm] The user approved: {title}. Helm stopped while applying these changes; check with app_get_settings."
        ),
        (RequestState::Allowed, _, _) => {
            format!("[Helm] The user approved: {title}. Helm is applying it now.")
        }
        (RequestState::Denied, _, _) => {
            format!("[Helm] The user denied your request ({title}). Nothing changed.")
        }
        (RequestState::Expired, Some(_), _) => format!(
            "[Helm] The user approved ({title}), but Helm stopped before applying it. Nothing changed; stage it again if it is still wanted."
        ),
        (RequestState::Expired, None, _) => {
            format!("[Helm] Your request ({title}) expired without an answer. Nothing changed.")
        }
        (RequestState::Pending, _, _) => format!("[Helm] Your request ({title}) is waiting."),
    };
    if let Some(n) = &v.note {
        s.push_str(&format!(" The user's note: {n}"));
    }
    cut(&s, MAX_DECISION_MESSAGE)
}
