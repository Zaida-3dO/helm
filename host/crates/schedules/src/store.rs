//! The `schedules` and `schedule_runs` tables and the rules on them
//! (prompts into an existing chat only).
//!
//! A schedule is one JSON [`Definition`] plus its state, next run and a revision that bumps on every
//! write. A run is a *receipt*: one row per claimed slot, unique per (schedule, slot), so a slot
//! is claimed once however many ticks or retries see it.

use crate::timing::{self, Zone};
use crate::types::*;
use helm_ipc::ApiError;
use rusqlite::{params, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

/// Active or paused schedules, at most.
pub const MAX_SCHEDULES: i64 = 256;
/// Occurrences claimed per tick, at most.
pub const MAX_CLAIMS_PER_TICK: usize = 8;
/// Receipts kept per schedule.
pub const KEEP_RUNS: i64 = 100;
const MAX_TITLE: usize = 160;
const MAX_MESSAGE: usize = 8000;
const MAX_REQUEST_ID: usize = 256;
const MAX_ERROR: usize = 240;

/// A store failure: a database error or an error the caller should see as is.
#[derive(Debug)]
pub enum SErr {
    Db(rusqlite::Error),
    Api(ApiError),
}
impl From<rusqlite::Error> for SErr {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}
impl From<ApiError> for SErr {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}
impl From<SErr> for ApiError {
    fn from(e: SErr) -> Self {
        match e {
            SErr::Db(e) => ApiError::internal(e.to_string()),
            SErr::Api(e) => e,
        }
    }
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schedules (
             id               TEXT PRIMARY KEY,
             request_key      TEXT NOT NULL UNIQUE,
             definition       TEXT NOT NULL,
             state            TEXT NOT NULL CHECK (state IN ('active', 'paused', 'completed', 'cancelled')),
             next_run_at      INTEGER,
             calendar_offset  INTEGER NOT NULL DEFAULT 0,
             revision         INTEGER NOT NULL,
             created_at       INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS schedules_due ON schedules(state, next_run_at);
         CREATE TABLE IF NOT EXISTS schedule_runs (
             schedule_id    TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
             scheduled_at   INTEGER NOT NULL,
             started_at     INTEGER NOT NULL,
             status         TEXT NOT NULL,
             chat_id        TEXT NOT NULL,
             submission_id  TEXT NOT NULL,
             error          TEXT,
             PRIMARY KEY (schedule_id, scheduled_at)
         );",
    )
}

/// The submission id for one slot of one schedule: the same id every time it is asked for, so a
/// coalesced or retried run can never be delivered twice (the chat's send is idempotent per id).
/// It has the shape of a version-5 UUID, which the worker expects of a message id.
pub fn submission_id(schedule_id: &str, scheduled_at: i64) -> String {
    let digest = Sha256::digest(format!("helm-schedule:{schedule_id}:{scheduled_at}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(b).to_string()
}

/// The refusal text for a title or message that cannot be saved, or `None`.
pub fn check_text(title: &str, message: &str) -> Option<String> {
    let t = title.trim();
    if t.is_empty() || t.len() > MAX_TITLE || t.chars().any(char::is_control) {
        return Some("The name must be 1 to 160 characters with no control characters.".into());
    }
    if message.trim().is_empty()
        || message.len() > MAX_MESSAGE
        || message
            .chars()
            .any(|c| c.is_control() && !c.is_whitespace())
    {
        return Some("The message must be 1 to 8000 bytes of text.".into());
    }
    None
}

fn bad(m: impl Into<String>) -> SErr {
    SErr::Api(ApiError::bad_args(m))
}
fn conflict(m: impl Into<String>) -> SErr {
    SErr::Api(ApiError::conflict(m))
}
fn not_found() -> SErr {
    SErr::Api(ApiError::new("not_found", "That schedule does not exist."))
}

struct Row {
    id: String,
    def: Definition,
    state: ScheduleState,
    next_run_at: Option<i64>,
    calendar_offset: i32,
    revision: i64,
    created_at: i64,
}

const COLS: &str = "id, definition, state, next_run_at, calendar_offset, revision, created_at";

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let json: String = r.get(1)?;
    let def: Definition = serde_json::from_str(&json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Row {
        id: r.get(0)?,
        def,
        state: ScheduleState::parse(&r.get::<_, String>(2)?),
        next_run_at: r.get(3)?,
        calendar_offset: r.get(4)?,
        revision: r.get(5)?,
        created_at: r.get(6)?,
    })
}

fn load(tx: &Transaction<'_>, id: &str) -> Result<Row, SErr> {
    tx.query_row(
        &format!("SELECT {COLS} FROM schedules WHERE id = ?1"),
        [id],
        row_of,
    )
    .optional()?
    .ok_or_else(not_found)
}

fn chat_title(tx: &Transaction<'_>, chat_id: &str) -> String {
    helm_chats::store::get_chat(tx, chat_id, 0)
        .ok()
        .flatten()
        .map(|c| c.title)
        .unwrap_or_default()
}

fn view(tx: &Transaction<'_>, r: Row) -> Schedule {
    Schedule {
        chat_title: chat_title(tx, &r.def.chat_id),
        id: r.id,
        definition: r.def,
        state: r.state,
        next_run_at: r.next_run_at,
        revision: r.revision,
        created_at: r.created_at,
    }
}

fn save(tx: &Transaction<'_>, r: &Row) -> Result<(), SErr> {
    let json =
        serde_json::to_string(&r.def).map_err(|e| SErr::Api(ApiError::internal(e.to_string())))?;
    tx.execute(
        "UPDATE schedules SET definition = ?2, state = ?3, next_run_at = ?4, calendar_offset = ?5, \
         revision = ?6 WHERE id = ?1",
        params![r.id, json, r.state.as_str(), r.next_run_at, r.calendar_offset, r.revision],
    )?;
    Ok(())
}

/// The destination must be an existing chat that is not archived.
fn check_chat(tx: &Transaction<'_>, chat_id: &str) -> Result<(), SErr> {
    match helm_chats::store::get_chat(tx, chat_id, 0)? {
        None => Err(bad("That chat does not exist.")),
        Some(c) if c.archived => Err(bad("That chat is archived. Restore it first.")),
        Some(_) => Ok(()),
    }
}

fn live_count(tx: &Transaction<'_>) -> Result<i64, SErr> {
    Ok(tx.query_row(
        "SELECT count(*) FROM schedules WHERE state IN ('active', 'paused')",
        [],
        |r| r.get(0),
    )?)
}

pub struct NewSchedule {
    /// The caller's request id (at most 256 characters); repeating it returns the saved schedule.
    pub request_id: String,
    pub definition: Definition,
}

/// Create a schedule. Idempotent: the same request id with the same definition returns the saved
/// schedule; the same id with a different one is refused.
pub fn create(
    tx: &Transaction<'_>,
    zone: &dyn Zone,
    now: i64,
    new: NewSchedule,
) -> Result<Schedule, SErr> {
    if new.request_id.is_empty() || new.request_id.len() > MAX_REQUEST_ID {
        return Err(bad("The request id must be 1 to 256 characters."));
    }
    let mut def = new.definition;
    def.title = def.title.trim().to_owned();
    let key = format!("ui:{}", new.request_id);
    if let Some(existing) = tx
        .query_row(
            &format!("SELECT {COLS} FROM schedules WHERE request_key = ?1"),
            [&key],
            row_of,
        )
        .optional()?
    {
        if existing.def == def {
            return Ok(view(tx, existing));
        }
        return Err(conflict(
            "That request id was already used for a different schedule. List schedules before retrying.",
        ));
    }
    if let Some(m) = check_text(&def.title, &def.message) {
        return Err(bad(m));
    }
    if let Some(m) = timing::check(&def.timing) {
        return Err(bad(m));
    }
    check_chat(tx, &def.chat_id)?;
    if live_count(tx)? >= MAX_SCHEDULES {
        return Err(bad("There are already 256 schedules. Cancel one first."));
    }
    let next = timing::first_run(&def.timing, now, zone)
        .ok_or_else(|| bad("That time has already passed. Pick a time in the future."))?;
    let row = Row {
        id: uuid::Uuid::new_v4().to_string(),
        def,
        state: ScheduleState::Active,
        next_run_at: Some(next),
        calendar_offset: zone.offset_secs(next),
        revision: 1,
        created_at: now,
    };
    let json = serde_json::to_string(&row.def)
        .map_err(|e| SErr::Api(ApiError::internal(e.to_string())))?;
    tx.execute(
        "INSERT INTO schedules(id, request_key, definition, state, next_run_at, calendar_offset, revision, created_at) \
         VALUES (?1, ?2, ?3, 'active', ?4, ?5, 1, ?6)",
        params![row.id, key, json, next, row.calendar_offset, now],
    )?;
    Ok(view(tx, row))
}

/// The parts of a schedule an update may change.
#[derive(Default)]
pub struct Update {
    pub title: Option<String>,
    pub message: Option<String>,
    pub timing: Option<Timing>,
    pub chat_id: Option<String>,
    pub paused: Option<bool>,
}

/// Change a schedule. `revision` must be the one the caller read.
pub fn update(
    tx: &Transaction<'_>,
    zone: &dyn Zone,
    now: i64,
    id: &str,
    revision: i64,
    u: Update,
) -> Result<Schedule, SErr> {
    let mut r = load(tx, id)?;
    if r.state == ScheduleState::Cancelled {
        return Err(conflict("A cancelled schedule cannot be changed."));
    }
    if r.revision != revision {
        return Err(conflict("This schedule changed. Reload before editing."));
    }
    if let Some(t) = u.title {
        r.def.title = t.trim().to_owned();
    }
    if let Some(m) = u.message {
        r.def.message = m;
    }
    if let Some(m) = check_text(&r.def.title, &r.def.message) {
        return Err(bad(m));
    }
    if let Some(c) = u.chat_id {
        if c != r.def.chat_id {
            check_chat(tx, &c)?;
            r.def.chat_id = c;
        }
    }
    if let Some(t) = u.timing {
        if let Some(m) = timing::check(&t) {
            return Err(bad(m));
        }
        let next = timing::first_run(&t, now, zone)
            .ok_or_else(|| bad("That time has already passed. Pick a time in the future."))?;
        r.def.timing = t;
        r.next_run_at = Some(next);
        // A finished schedule given a new time waits for the user to switch it back on.
        if r.state == ScheduleState::Completed {
            r.state = ScheduleState::Paused;
        }
    }
    match u.paused {
        Some(true) if r.state == ScheduleState::Active => r.state = ScheduleState::Paused,
        Some(false) if r.state != ScheduleState::Active => {
            if r.state == ScheduleState::Completed && r.next_run_at.is_none() {
                return Err(bad("Choose a new time first."));
            }
            check_chat(tx, &r.def.chat_id)?;
            match r.next_run_at {
                None => return Err(bad("Choose a new time first.")),
                // Slots that passed while it was paused are not replayed.
                Some(due) if due <= now => {
                    r.next_run_at = timing::next_after(&r.def.timing, due, now, zone);
                    if r.next_run_at.is_none() {
                        return Err(bad("Choose a new time first."));
                    }
                }
                Some(_) => {}
            }
            r.state = ScheduleState::Active;
        }
        _ => {}
    }
    r.calendar_offset = r.next_run_at.map_or(0, |n| zone.offset_secs(n));
    r.revision += 1;
    save(tx, &r)?;
    Ok(view(tx, r))
}

/// Cancel a schedule (idempotent: a cancelled one is returned as it is).
pub fn cancel(tx: &Transaction<'_>, id: &str, revision: i64) -> Result<Schedule, SErr> {
    let mut r = load(tx, id)?;
    if r.state == ScheduleState::Cancelled {
        return Ok(view(tx, r));
    }
    if r.revision != revision {
        return Err(conflict("This schedule changed. Reload before editing."));
    }
    r.state = ScheduleState::Cancelled;
    r.next_run_at = None;
    r.revision += 1;
    save(tx, &r)?;
    Ok(view(tx, r))
}

/// Schedules, newest first. Cancelled and completed ones only when asked for.
pub fn list(tx: &Transaction<'_>, include_inactive: bool) -> Result<Vec<Schedule>, SErr> {
    let sql = if include_inactive {
        format!("SELECT {COLS} FROM schedules ORDER BY created_at DESC, id")
    } else {
        format!("SELECT {COLS} FROM schedules WHERE state IN ('active', 'paused') ORDER BY created_at DESC, id")
    };
    let mut st = tx.prepare(&sql)?;
    let rows = st.query_map([], row_of)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().map(|r| view(tx, r)).collect())
}

/// The newest 20 receipts of a schedule.
pub fn runs(tx: &Transaction<'_>, id: &str) -> Result<Vec<Run>, SErr> {
    let mut st = tx.prepare(
        "SELECT schedule_id, scheduled_at, started_at, status, chat_id, error FROM schedule_runs \
         WHERE schedule_id = ?1 ORDER BY scheduled_at DESC LIMIT 20",
    )?;
    let rows = st
        .query_map([id], |r| {
            Ok(Run {
                schedule_id: r.get(0)?,
                scheduled_at: r.get(1)?,
                started_at: r.get(2)?,
                status: RunStatus::parse(&r.get::<_, String>(3)?),
                chat_id: r.get(4)?,
                error: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One occurrence claimed this tick, ready to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub schedule_id: String,
    pub scheduled_at: i64,
    pub chat_id: String,
    pub title: String,
    pub message: String,
    pub submission_id: String,
}

/// Whether a chat should not be sent a scheduled prompt right now: a turn is running, or it still
/// holds submissions that have not been delivered.
fn chat_busy(tx: &Transaction<'_>, chat: &helm_chats::types::ChatRecord) -> Result<bool, SErr> {
    if chat.status.is_busy() {
        return Ok(true);
    }
    Ok(!helm_chats::store::pending(tx, &chat.id)?.is_empty())
}

/// Claim up to [`MAX_CLAIMS_PER_TICK`] due occurrences, oldest first, in one transaction.
///
/// - A destination chat that is busy is **not claimed**: the run waits and is tried again next tick.
/// - A destination that is gone or archived writes a `blocked` receipt and pauses the schedule.
/// - Otherwise a `dispatching` receipt is written and the schedule moves to the first slot after
///   `now`, which is what folds every missed slot into this one run.
///
/// A slot already holding a receipt is not sent again; the schedule just advances. Returns the
/// sends to make and how many occurrences were claimed in all (blocked ones included).
pub fn claim_due(
    tx: &Transaction<'_>,
    zone: &dyn Zone,
    now: i64,
) -> Result<(Vec<Claim>, usize), SErr> {
    let due: Vec<Row> = {
        let mut st = tx.prepare(&format!(
            "SELECT {COLS} FROM schedules WHERE state = 'active' AND next_run_at IS NOT NULL \
             AND next_run_at <= ?1 ORDER BY next_run_at, id LIMIT {MAX_SCHEDULES}"
        ))?;
        let rows = st
            .query_map([now], row_of)?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut claims = Vec::new();
    let mut claimed = 0usize;
    for mut r in due {
        if claimed >= MAX_CLAIMS_PER_TICK {
            break;
        }
        let Some(slot) = r.next_run_at else { continue };
        let chat = helm_chats::store::get_chat(tx, &r.def.chat_id, 0)?;
        let usable = chat.as_ref().filter(|c| !c.archived);
        let sid = submission_id(&r.id, slot);
        let Some(chat) = usable else {
            let why = if chat.is_some() {
                "The chat is archived."
            } else {
                "The chat no longer exists."
            };
            insert_receipt(
                tx,
                &r.id,
                slot,
                now,
                RunStatus::Blocked,
                &r.def.chat_id,
                &sid,
                Some(why),
            )?;
            r.state = ScheduleState::Paused;
            r.revision += 1;
            save(tx, &r)?;
            claimed += 1;
            continue;
        };
        if chat_busy(tx, chat)? {
            continue;
        }
        let fresh = insert_receipt(
            tx,
            &r.id,
            slot,
            now,
            RunStatus::Dispatching,
            &r.def.chat_id,
            &sid,
            None,
        )?;
        r.next_run_at = timing::next_after(&r.def.timing, slot, now, zone);
        if r.next_run_at.is_none() {
            r.state = ScheduleState::Completed;
        }
        r.calendar_offset = r.next_run_at.map_or(0, |n| zone.offset_secs(n));
        r.revision += 1;
        save(tx, &r)?;
        prune(tx, &r.id)?;
        claimed += 1;
        if fresh {
            claims.push(Claim {
                schedule_id: r.id.clone(),
                scheduled_at: slot,
                chat_id: r.def.chat_id.clone(),
                title: r.def.title.clone(),
                message: r.def.message.clone(),
                submission_id: sid,
            });
        }
    }
    Ok((claims, claimed))
}

#[allow(clippy::too_many_arguments)]
fn insert_receipt(
    tx: &Transaction<'_>,
    schedule_id: &str,
    slot: i64,
    now: i64,
    status: RunStatus,
    chat_id: &str,
    submission_id: &str,
    error: Option<&str>,
) -> Result<bool, SErr> {
    let n = tx.execute(
        "INSERT OR IGNORE INTO schedule_runs(schedule_id, scheduled_at, started_at, status, chat_id, submission_id, error) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![schedule_id, slot, now, status.as_str(), chat_id, submission_id, error],
    )?;
    Ok(n == 1)
}

/// Keep the newest [`KEEP_RUNS`] receipts of a schedule.
fn prune(tx: &Transaction<'_>, id: &str) -> Result<(), SErr> {
    tx.execute(
        "DELETE FROM schedule_runs WHERE schedule_id = ?1 AND scheduled_at NOT IN \
         (SELECT scheduled_at FROM schedule_runs WHERE schedule_id = ?1 ORDER BY scheduled_at DESC LIMIT ?2)",
        params![id, KEEP_RUNS],
    )?;
    Ok(())
}

/// Record how a claimed send ended. The error is cut to 240 characters.
pub fn finish(
    tx: &Transaction<'_>,
    claim: &Claim,
    status: RunStatus,
    error: Option<&str>,
) -> Result<(), SErr> {
    let error = error.map(|e| e.chars().take(MAX_ERROR).collect::<String>());
    tx.execute(
        "UPDATE schedule_runs SET status = ?3, error = ?4 WHERE schedule_id = ?1 AND scheduled_at = ?2",
        params![claim.schedule_id, claim.scheduled_at, status.as_str(), error],
    )?;
    Ok(())
}

/// At startup: a receipt still `dispatching` belongs to a send Helm closed in the middle of. If
/// the chat holds the submission (the id is the slot's own) it was delivered; otherwise nobody can
/// tell, and it becomes `uncertain` and is never replayed.
pub fn recover(tx: &Transaction<'_>) -> Result<usize, SErr> {
    let stuck: Vec<(String, i64, String)> = {
        let mut st = tx.prepare(
            "SELECT schedule_id, scheduled_at, submission_id FROM schedule_runs WHERE status = 'dispatching'",
        )?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (id, slot, sub) in &stuck {
        let landed: bool = tx
            .query_row("SELECT 1 FROM submissions WHERE id = ?1", [sub], |_| {
                Ok(true)
            })
            .optional()?
            .unwrap_or(false);
        let (status, note) = if landed {
            (RunStatus::Started, None)
        } else {
            (
                RunStatus::Uncertain,
                Some("Helm closed during the send; not replayed."),
            )
        };
        tx.execute(
            "UPDATE schedule_runs SET status = ?3, error = ?4 WHERE schedule_id = ?1 AND scheduled_at = ?2",
            params![id, slot, status.as_str(), note],
        )?;
    }
    Ok(stuck.len())
}

/// Weekly schedules follow the clock: when the UTC offset at the next run no longer matches the
/// one remembered (a daylight-saving change, a new timezone), the next run is worked out again.
/// Returns how many were moved.
pub fn refresh_calendar(tx: &Transaction<'_>, zone: &dyn Zone, now: i64) -> Result<usize, SErr> {
    let rows: Vec<Row> = {
        let mut st = tx.prepare(&format!(
            "SELECT {COLS} FROM schedules WHERE state = 'active' AND next_run_at IS NOT NULL AND next_run_at > ?1"
        ))?;
        let rows = st
            .query_map([now], row_of)?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut moved = 0;
    for mut r in rows {
        let (Timing::Weekly { .. }, Some(next)) = (&r.def.timing, r.next_run_at) else {
            continue;
        };
        if zone.offset_secs(next) == r.calendar_offset {
            continue;
        }
        r.next_run_at = timing::first_run(&r.def.timing, now, zone);
        r.calendar_offset = r.next_run_at.map_or(0, |n| zone.offset_secs(n));
        r.revision += 1;
        save(tx, &r)?;
        moved += 1;
    }
    Ok(moved)
}
