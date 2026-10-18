//! Reads and writes of chat rows, submissions and transcript pages.

use crate::types::*;
use rusqlite::{params, OptionalExtension, Row, Transaction};
use serde_json::{json, Value};

pub const PAGE_SIZE: i64 = 100;
pub const MAX_LISTED_CHATS: i64 = 500;
pub const MAX_SUBMISSION_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_TITLE: &str = "New chat";
/// The fallback title's length.
pub const FALLBACK_TITLE_CHARS: usize = 72;

const CHAT_COLUMNS: &str = "c.id, c.project_id, COALESCE(p.path, ''), c.title, c.status, c.error, \
     c.model, c.thinking, c.draft, c.draft_revision, c.created_at, c.updated_at, c.pinned, \
     c.archived, c.parent_chat_id, c.side_source_id, COALESCE(t.state, 'fallback'), u.token, \
     (SELECT k.id FROM chat_completions k WHERE k.chat_id = c.id AND k.read = 0), \
     EXISTS(SELECT 1 FROM chat_mutes m WHERE m.chat_id = c.id), \
     MAX(c.updated_at, COALESCE(a.last_activity_at, 0)),      CASE WHEN c.status IN ('starting', 'running', 'stopping') THEN COALESCE(a.stalled, 0) ELSE 0 END";

/// The joins [`CHAT_COLUMNS`] needs, after `FROM chats c`.
pub(crate) const CHAT_JOINS: &str = "JOIN projects p ON p.id = c.project_id \
     LEFT JOIN chat_titles t ON t.chat_id = c.id LEFT JOIN unread_marks u ON u.chat_id = c.id \
     LEFT JOIN chat_activity a ON a.chat_id = c.id";

fn chat_from_row(r: &Row<'_>, revision: i64) -> rusqlite::Result<ChatRecord> {
    Ok(ChatRecord {
        id: r.get(0)?,
        project_id: r.get(1)?,
        cwd: r.get(2)?,
        title: r.get(3)?,
        status: ChatStatus::parse(&r.get::<_, String>(4)?),
        error: r.get(5)?,
        harness: Harness::Claude,
        model: r.get(6)?,
        effort: r.get(7)?,
        draft: r.get(8)?,
        draft_revision: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        revision,
        pinned: r.get(12)?,
        archived: r.get(13)?,
        parent_chat_id: r.get(14)?,
        side_source_id: r.get(15)?,
        title_state: TitleState::parse(&r.get::<_, String>(16)?),
        unread: UnreadState {
            mark_token: r.get(17)?,
            completion_id: r.get(18)?,
        },
        muted: r.get(19)?,
        last_activity_at: r.get(20)?,
        stalled: r.get(21)?,
    })
}

pub fn get_chat(
    tx: &Transaction<'_>,
    chat_id: &str,
    revision: i64,
) -> rusqlite::Result<Option<ChatRecord>> {
    tx.query_row(
        &format!("SELECT {CHAT_COLUMNS} FROM chats c {CHAT_JOINS} WHERE c.id = ?1"),
        [chat_id],
        |r| chat_from_row(r, revision),
    )
    .optional()
}

pub fn list_chats(tx: &Transaction<'_>, revision: i64) -> rusqlite::Result<Vec<ChatRecord>> {
    let mut st = tx.prepare(&format!(
        "SELECT {CHAT_COLUMNS} FROM chats c {CHAT_JOINS}
         WHERE c.archived = 0 AND c.side_source_id IS NULL
         ORDER BY c.created_at DESC, c.id LIMIT ?1"
    ))?;
    let rows = st.query_map([MAX_LISTED_CHATS], |r| chat_from_row(r, revision))?;
    rows.collect()
}

/// The folder project for `cwd`, created when missing. Paths compare
/// case-insensitively on Windows.
pub fn upsert_folder_project(
    tx: &Transaction<'_>,
    cwd: &str,
    now: i64,
) -> rusqlite::Result<String> {
    let sql = if cfg!(windows) {
        "SELECT id FROM projects WHERE kind = 'folder' AND lower(path) = lower(?1)"
    } else {
        "SELECT id FROM projects WHERE kind = 'folder' AND path = ?1"
    };
    if let Some(id) = tx.query_row(sql, [cwd], |r| r.get(0)).optional()? {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let name = std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| cwd.to_owned());
    tx.execute(
        "INSERT INTO projects(id, kind, name, path, created_at) VALUES (?1, 'folder', ?2, ?3, ?4)",
        params![id, name, cwd, now],
    )?;
    Ok(id)
}

pub fn create_chat(
    tx: &Transaction<'_>,
    cwd: &str,
    model: Option<&str>,
    effort: Option<&str>,
    now: i64,
) -> rusqlite::Result<String> {
    let project = upsert_folder_project(tx, cwd, now)?;
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO chats(id, project_id, title, status, harness, model, thinking, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'idle', 'claude', ?4, ?5, ?6, ?6)",
        params![id, project, DEFAULT_TITLE, model, effort, now],
    )?;
    Ok(id)
}

pub fn set_status(
    tx: &Transaction<'_>,
    chat_id: &str,
    status: ChatStatus,
    error: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE chats SET status = ?1, error = ?2, updated_at = ?3 WHERE id = ?4",
        params![status.as_str(), error, now, chat_id],
    )?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum DraftOutcome {
    Saved(i64),
    Stale { stored: i64 },
    NotFound,
}

/// The draft rule: the client proposes `max(now, prev + 1)` and the
/// host refuses anything not greater than what it holds. A refusal changes nothing.
pub fn save_draft(
    tx: &Transaction<'_>,
    chat_id: &str,
    text: &str,
    revision: i64,
    now: i64,
) -> rusqlite::Result<DraftOutcome> {
    let stored: Option<i64> = tx
        .query_row(
            "SELECT draft_revision FROM chats WHERE id = ?1",
            [chat_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(stored) = stored else {
        return Ok(DraftOutcome::NotFound);
    };
    if revision <= stored || revision > helm_core::revisions::MAX_REVISION {
        return Ok(DraftOutcome::Stale { stored });
    }
    tx.execute(
        "UPDATE chats SET draft = ?1, draft_revision = ?2, updated_at = ?3 WHERE id = ?4",
        params![text, revision, now, chat_id],
    )?;
    Ok(DraftOutcome::Saved(revision))
}

// ---------------------------------------------------------------------------------------------
// Messages and pages
// ---------------------------------------------------------------------------------------------

fn message_from_row(r: &Row<'_>) -> rusqlite::Result<MessageRow> {
    let role = match r.get::<_, String>(3)?.as_str() {
        "user" => MessageRole::User,
        "toolResult" => MessageRole::ToolResult,
        "context" => MessageRole::Context,
        _ => MessageRole::Assistant,
    };
    let status = match r.get::<_, String>(9)?.as_str() {
        "saved" => MessageStatus::Saved,
        "streaming" => MessageStatus::Streaming,
        "error" => MessageStatus::Error,
        "interrupted" => MessageStatus::Interrupted,
        _ => MessageStatus::Complete,
    };
    let details_raw: Option<String> = r.get(14)?;
    let mut details_v: Value = details_raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    let thinking_tail = details_v
        .as_object_mut()
        .and_then(|m| m.remove("thinkingTail"))
        .and_then(|v| v.as_str().map(str::to_owned));
    let details: MessageDetails = serde_json::from_value(details_v).unwrap_or_default();
    Ok(MessageRow {
        id: r.get(0)?,
        chat_id: r.get(1)?,
        source_key: r.get(2)?,
        role,
        text: r.get(4)?,
        thinking: r.get(5)?,
        thinking_tail,
        tool_name: r.get(6)?,
        tool_call_id: r.get(7)?,
        status,
        timestamp: r.get(10)?,
        sequence: r.get(11)?,
        revision: r.get(12)?,
        artifact: r.get(13)?,
        details,
    })
}

const MESSAGE_COLUMNS: &str =
    "id, chat_id, source_key, role, text, thinking, tool_name, tool_call_id, \
     active, status, timestamp, sequence, revision, artifact, details";

pub fn message_by_key(
    tx: &Transaction<'_>,
    chat_id: &str,
    source_key: &str,
) -> rusqlite::Result<Option<MessageRow>> {
    tx.query_row(
        &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE chat_id = ?1 AND source_key = ?2"),
        [chat_id, source_key],
        message_from_row,
    )
    .optional()
}

/// A page cursor is a message row id (the UI also builds cursors from row ids when its 600-row
/// cap drops rows). Resolved to the row's `(timestamp, id)` order key.
fn parse_cursor(
    tx: &Transaction<'_>,
    chat_id: &str,
    c: &str,
) -> rusqlite::Result<Option<(i64, i64)>> {
    let Ok(id) = c.trim().parse::<i64>() else {
        return Ok(None);
    };
    tx.query_row(
        "SELECT timestamp, id FROM messages WHERE chat_id = ?1 AND id = ?2",
        params![chat_id, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

fn cursor_of(m: &MessageRow) -> String {
    m.id.to_string()
}

fn select_page(
    tx: &Transaction<'_>,
    chat_id: &str,
    newer: bool,
    from: Option<(i64, i64)>,
    inclusive: bool,
    limit: i64,
) -> rusqlite::Result<Vec<MessageRow>> {
    let (cmp, order) = match (newer, inclusive) {
        (false, false) => ("<", "DESC"),
        (false, true) => ("<=", "DESC"),
        (true, false) => (">", "ASC"),
        (true, true) => (">=", "ASC"),
    };
    let where_from = if from.is_some() {
        format!("AND (timestamp, id) {cmp} (?2, ?3)")
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages WHERE chat_id = ?1 AND active = 1 {where_from}
         ORDER BY timestamp {order}, id {order} LIMIT ?4"
    );
    let (t, i) = from.unwrap_or((0, 0));
    let mut st = tx.prepare(&sql)?;
    let rows = st.query_map(params![chat_id, t, i, limit], message_from_row)?;
    let mut v: Vec<MessageRow> = rows.collect::<rusqlite::Result<_>>()?;
    if !newer {
        v.reverse();
    }
    Ok(v)
}

fn exists_beyond(
    tx: &Transaction<'_>,
    chat_id: &str,
    m: Option<&MessageRow>,
    newer: bool,
) -> rusqlite::Result<bool> {
    let Some(m) = m else { return Ok(false) };
    let cmp = if newer { ">" } else { "<" };
    tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE chat_id = ?1 AND active = 1 AND (timestamp, id) {cmp} (?2, ?3))"
        ),
        params![chat_id, m.timestamp, m.id],
        |r| r.get(0),
    )
}

/// One transcript page: the latest 100 by default, or 100 before/after a
/// cursor, or 100 around a message id.
pub fn transcript_page(
    tx: &Transaction<'_>,
    args: &TranscriptPageArgs,
    revision: i64,
) -> rusqlite::Result<Option<TranscriptPage>> {
    let chat = &args.chat_id;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1)",
        [chat],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let inclusive = args.inclusive.unwrap_or(false);
    let messages = if let Some(target) = args.around {
        let at: Option<(i64, i64)> = tx
            .query_row(
                "SELECT timestamp, id FROM messages WHERE chat_id = ?1 AND id = ?2",
                params![chat, target],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match at {
            Some(at) => {
                let mut older = select_page(tx, chat, false, Some(at), false, PAGE_SIZE / 2)?;
                let newer = select_page(tx, chat, true, Some(at), true, PAGE_SIZE / 2)?;
                older.extend(newer);
                older
            }
            None => select_page(tx, chat, false, None, false, PAGE_SIZE)?,
        }
    } else if let Some(c) = args.before.as_deref() {
        let from = parse_cursor(tx, chat, c)?;
        if from.is_none() {
            return Ok(Some(empty_page(tx, chat, revision)?));
        }
        select_page(tx, chat, false, from, inclusive, PAGE_SIZE)?
    } else if let Some(c) = args.after.as_deref() {
        let from = parse_cursor(tx, chat, c)?;
        if from.is_none() {
            return Ok(Some(empty_page(tx, chat, revision)?));
        }
        select_page(tx, chat, true, from, inclusive, PAGE_SIZE)?
    } else {
        select_page(tx, chat, false, None, false, PAGE_SIZE)?
    };
    let has_older = exists_beyond(tx, chat, messages.first(), false)?;
    let has_newer = exists_beyond(tx, chat, messages.last(), true)?;
    Ok(Some(TranscriptPage {
        chat_id: chat.clone(),
        older_cursor: messages.first().filter(|_| has_older).map(cursor_of),
        newer_cursor: messages.last().filter(|_| has_newer).map(cursor_of),
        has_older,
        has_newer,
        messages,
        runs: runs(tx, chat)?,
        pending: pending(tx, chat)?,
        revision,
    }))
}

fn empty_page(tx: &Transaction<'_>, chat: &str, revision: i64) -> rusqlite::Result<TranscriptPage> {
    Ok(TranscriptPage {
        chat_id: chat.to_owned(),
        messages: Vec::new(),
        has_older: false,
        has_newer: false,
        older_cursor: None,
        newer_cursor: None,
        runs: runs(tx, chat)?,
        pending: pending(tx, chat)?,
        revision,
    })
}

fn runs(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Vec<RunSummary>> {
    let mut st = tx.prepare(
        "SELECT id, status, started_at, ended_at FROM runs WHERE chat_id = ?1
         ORDER BY started_at DESC LIMIT 100",
    )?;
    let rows = st.query_map([chat_id], |r| {
        Ok(RunSummary {
            id: r.get(0)?,
            status: r.get(1)?,
            started_at: r.get(2)?,
            ended_at: r.get(3)?,
        })
    })?;
    let mut v: Vec<RunSummary> = rows.collect::<rusqlite::Result<_>>()?;
    v.reverse();
    Ok(v)
}

// ---------------------------------------------------------------------------------------------
// Submissions
// ---------------------------------------------------------------------------------------------

pub fn mode_str(m: SubmissionMode) -> &'static str {
    match m {
        SubmissionMode::Prompt => "prompt",
        SubmissionMode::Steer => "steer",
        SubmissionMode::FollowUp => "followUp",
    }
}

pub fn state_str(s: SubmissionState) -> &'static str {
    match s {
        SubmissionState::Saved => "saved",
        SubmissionState::Queued => "queued",
        SubmissionState::Dispatching => "dispatching",
        SubmissionState::Accepted => "accepted",
        SubmissionState::Delivered => "delivered",
        SubmissionState::Complete => "complete",
        SubmissionState::Rejected => "rejected",
        SubmissionState::Uncertain => "uncertain",
        SubmissionState::Withdrawn => "withdrawn",
        SubmissionState::Superseded => "superseded",
    }
}

fn parse_state(s: &str) -> SubmissionState {
    match s {
        "saved" => SubmissionState::Saved,
        "queued" => SubmissionState::Queued,
        "dispatching" => SubmissionState::Dispatching,
        "accepted" => SubmissionState::Accepted,
        "delivered" => SubmissionState::Delivered,
        "complete" => SubmissionState::Complete,
        "rejected" => SubmissionState::Rejected,
        "withdrawn" => SubmissionState::Withdrawn,
        "superseded" => SubmissionState::Superseded,
        _ => SubmissionState::Uncertain,
    }
}

fn parse_mode(s: &str) -> SubmissionMode {
    match s {
        "steer" => SubmissionMode::Steer,
        "followUp" => SubmissionMode::FollowUp,
        _ => SubmissionMode::Prompt,
    }
}

const SUBMISSION_SELECT: &str = "SELECT s.id, s.chat_id, s.text, s.mode, s.state, \
     COALESCE(m.origin, 'typed'), q.position, s.created_at, m.priority \
     FROM submissions s LEFT JOIN submission_meta m ON m.submission_id = s.id \
     LEFT JOIN queued_submissions q ON q.submission_id = s.id";

fn submission_from_row(r: &Row<'_>) -> rusqlite::Result<Submission> {
    Ok(Submission {
        id: r.get(0)?,
        chat_id: r.get(1)?,
        text: r.get(2)?,
        mode: parse_mode(&r.get::<_, String>(3)?),
        state: parse_state(&r.get::<_, String>(4)?),
        origin: SubmissionOrigin::parse(&r.get::<_, String>(5)?),
        position: r.get(6)?,
        created_at: r.get(7)?,
    })
}

pub fn get_submission(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<Submission>> {
    tx.query_row(
        &format!("{SUBMISSION_SELECT} WHERE s.id = ?1"),
        [id],
        submission_from_row,
    )
    .optional()
}

/// Submissions not yet delivered, queue order first, then creation time; at most 100. This
/// includes `uncertain` ones awaiting recovery and `rejected` ones ("saved but could not start"),
/// so the queue UI can offer Recover; both leave the list once recovered.
pub fn pending(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Vec<Submission>> {
    let mut st = tx.prepare(&format!(
        "{SUBMISSION_SELECT} WHERE s.chat_id = ?1
           AND s.state IN ('saved', 'queued', 'dispatching', 'accepted', 'uncertain', 'rejected')
         ORDER BY q.position IS NULL, q.position, s.created_at, s.id LIMIT 100"
    ))?;
    let rows = st.query_map([chat_id], submission_from_row)?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;
    use helm_core::db::Database;

    fn chat(db: &Database) -> String {
        db.write(|tx| create_chat(tx, "C:/work/repo", Some("opus"), None, 5))
            .unwrap()
    }

    #[test]
    fn create_upserts_one_folder_project_per_path() {
        let (_d, db) = temp_db();
        let a = chat(&db);
        let b = db
            .write(|tx| create_chat(tx, "c:/WORK/repo", None, None, 6))
            .unwrap();
        let (ca, cb, projects) = db
            .snapshot(|tx| {
                let ca = get_chat(tx, &a, 1)?.unwrap();
                let cb = get_chat(tx, &b, 1)?.unwrap();
                let n: i64 = tx.query_row("SELECT count(*) FROM projects", [], |r| r.get(0))?;
                Ok::<_, rusqlite::Error>((ca, cb, n))
            })
            .unwrap();
        if cfg!(windows) {
            assert_eq!(projects, 1);
            assert_eq!(ca.project_id, cb.project_id);
        }
        assert_eq!(ca.cwd, "C:/work/repo");
        assert_eq!(ca.title, DEFAULT_TITLE);
        assert_eq!(ca.model.as_deref(), Some("opus"));
        let list = db.snapshot(|tx| list_chats(tx, 1)).unwrap();
        assert_eq!(list[0].id, b, "newest first");
    }

    /// A proposed revision must be greater than the stored one; a refusal changes nothing.
    #[test]
    fn draft_revisions_must_increase() {
        let (_d, db) = temp_db();
        let c = chat(&db);
        let save = |text: &str, rev: i64| db.write(|tx| save_draft(tx, &c, text, rev, 1)).unwrap();
        assert_eq!(save("one", 1000), DraftOutcome::Saved(1000));
        assert_eq!(save("stale", 1000), DraftOutcome::Stale { stored: 1000 });
        assert_eq!(save("older", 999), DraftOutcome::Stale { stored: 1000 });
        let rec = db.snapshot(|tx| get_chat(tx, &c, 1)).unwrap().unwrap();
        assert_eq!((rec.draft.as_str(), rec.draft_revision), ("one", 1000));
        assert_eq!(save("two", 1001), DraftOutcome::Saved(1001));
        assert_eq!(
            db.write(|tx| save_draft(tx, "nope", "x", 5, 1)).unwrap(),
            DraftOutcome::NotFound
        );
    }

    fn add_messages(db: &Database, chat: &str, n: i64) {
        db.write(|tx| {
            for i in 0..n {
                tx.execute(
                    "INSERT INTO messages(chat_id, source_key, role, text, status, timestamp) VALUES (?1, ?2, 'user', ?3, 'complete', ?4)",
                    params![chat, format!("k{i}"), format!("t{i}"), 1000 + i],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
    }

    fn page(db: &Database, args: TranscriptPageArgs) -> TranscriptPage {
        db.snapshot(|tx| transcript_page(tx, &args, 1))
            .unwrap()
            .unwrap()
    }

    fn args(chat: &str) -> TranscriptPageArgs {
        TranscriptPageArgs {
            chat_id: chat.into(),
            before: None,
            after: None,
            around: None,
            inclusive: None,
        }
    }

    /// Pages of 100 with before, after and around.
    #[test]
    fn pages_hold_one_hundred_and_walk_both_ways() {
        let (_d, db) = temp_db();
        let c = chat(&db);
        add_messages(&db, &c, 250);
        let last = page(&db, args(&c));
        assert_eq!(last.messages.len(), 100);
        assert_eq!(last.messages.last().unwrap().text, "t249");
        assert!(last.has_older && !last.has_newer);
        let older = page(
            &db,
            TranscriptPageArgs {
                before: last.older_cursor.clone(),
                ..args(&c)
            },
        );
        assert_eq!(older.messages.first().unwrap().text, "t50");
        assert_eq!(older.messages.last().unwrap().text, "t149");
        let oldest = page(
            &db,
            TranscriptPageArgs {
                before: older.older_cursor.clone(),
                ..args(&c)
            },
        );
        assert_eq!(oldest.messages.len(), 50);
        assert!(!oldest.has_older && oldest.has_newer);
        let newer = page(
            &db,
            TranscriptPageArgs {
                after: oldest.newer_cursor.clone(),
                ..args(&c)
            },
        );
        assert_eq!(newer.messages.first().unwrap().text, "t50");
        let target = older.messages[10].id;
        let around = page(
            &db,
            TranscriptPageArgs {
                around: Some(target),
                ..args(&c)
            },
        );
        assert_eq!(around.messages.len(), 100);
        assert_eq!(
            around.messages[50].id, target,
            "the target sits in the middle"
        );
        // The UI builds cursors from row ids (600-row cap): a bare id works as a cursor.
        let by_id = page(
            &db,
            TranscriptPageArgs {
                before: Some(target.to_string()),
                ..args(&c)
            },
        );
        assert_eq!(by_id.messages.last().unwrap().id, older.messages[9].id);
    }
}
