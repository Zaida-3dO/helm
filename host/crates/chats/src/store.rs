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

const CHAT_COLUMNS: &str = concat!(
    "c.id, c.project_id, COALESCE(p.path, ''), c.title, c.status, c.error, ",
    crate::local::model_id_sql!(),
    ", c.thinking, c.draft, c.draft_revision, c.created_at, c.updated_at, c.pinned, \
     c.archived, c.parent_chat_id, c.side_source_id, COALESCE(t.state, 'fallback'), u.token, \
     (SELECT k.id FROM chat_completions k WHERE k.chat_id = c.id AND k.read = 0), \
     EXISTS(SELECT 1 FROM chat_mutes m WHERE m.chat_id = c.id), \
     MAX(c.updated_at, COALESCE(a.last_activity_at, 0)), \
     CASE WHEN c.status IN ('starting', 'running', 'stopping') THEN COALESCE(a.stalled, 0) ELSE 0 END, \
     CASE WHEN c.status IN ('starting', 'running', 'stopping') THEN a.local_ahead ELSE NULL END, ",
    crate::local::started_sql!(),
    ", c.profile_id, c.host_device_id, ",
    "(SELECT json_group_array(s.profile_id) FROM chat_shares s WHERE s.chat_id = c.id), ",
    "CASE WHEN c.status IN ('starting', 'running', 'stopping') THEN a.state ELSE NULL END",
    ", CASE WHEN c.status IN ('starting', 'running', 'stopping') THEN a.queue_reason ELSE NULL END",
    ", c.stack_id, c.prev_session_id, c.next_session_id",
    ", c.sidebar_state"
);

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
        local_ahead: r.get(22)?,
        started: r.get(23)?,
        profile_id: r.get(24)?,
        host_device_id: r.get(25)?,
        shared_with: r
            .get::<_, Option<String>>(26)?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        // The state shape is written by this same build; one it cannot read is no state.
        state: r
            .get::<_, Option<String>>(27)?
            .and_then(|j| serde_json::from_str::<crate::types::ChatState>(&j).ok())
            .map(Box::new),
        queue_reason: r.get(28)?,
        stack_id: r.get(29)?,
        prev_session_id: r.get(30)?,
        next_session_id: r.get(31)?,
        // The state is written by this same build; one it cannot read is no state.
        sidebar_state: r
            .get::<_, Option<String>>(32)?
            .and_then(|j| serde_json::from_str::<crate::types::SidebarState>(&j).ok()),
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

/// Every chat another device may list, archived ones included but not side chats, the most
/// recently active first, at most `limit`. With `since`, only those whose row or activity changed
/// at or after it.
pub fn list_for_devices(
    tx: &Transaction<'_>,
    revision: i64,
    since: Option<i64>,
    limit: i64,
) -> rusqlite::Result<Vec<ChatRecord>> {
    let mut st = tx.prepare(&format!(
        "SELECT {CHAT_COLUMNS} FROM chats c {CHAT_JOINS}
         WHERE c.side_source_id IS NULL
           AND MAX(c.updated_at, COALESCE(a.last_activity_at, 0)) >= ?1
         ORDER BY MAX(c.updated_at, COALESCE(a.last_activity_at, 0)) DESC, c.id LIMIT ?2"
    ))?;
    let rows = st.query_map(params![since.unwrap_or(0), limit], |r| {
        chat_from_row(r, revision)
    })?;
    rows.collect()
}

/// A chat's active message rows whose revision is newer than `since`: the newest `limit`, oldest
/// first, and whether older ones were left out.
pub fn messages_since(
    tx: &Transaction<'_>,
    chat_id: &str,
    since: i64,
    limit: i64,
) -> rusqlite::Result<(Vec<MessageRow>, bool)> {
    let mut st = tx.prepare(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages
         WHERE chat_id = ?1 AND active = 1 AND revision > ?2
         ORDER BY timestamp DESC, id DESC LIMIT ?3"
    ))?;
    let mut rows: Vec<MessageRow> = st
        .query_map(params![chat_id, since, limit + 1], message_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    rows.reverse();
    Ok((rows, more))
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
    create_chat_in(tx, &project, model, effort, now)
}

/// A new chat in `project_id`, a folder project the caller has checked (the
/// new-chat page names the exact project, since several projects may share one folder).
pub fn create_chat_in(
    tx: &Transaction<'_>,
    project_id: &str,
    model: Option<&str>,
    effort: Option<&str>,
    now: i64,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    // A local model id is stored as the server (`provider`) and the server's own model name.
    let (provider, model) =
        crate::local::columns(model).map_err(rusqlite::Error::InvalidParameterName)?;
    tx.execute(
        "INSERT INTO chats(id, project_id, title, status, harness, provider, model, thinking, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'idle', 'claude', ?4, ?5, ?6, ?7, ?7)",
        params![id, project_id, DEFAULT_TITLE, provider, model, effort, now],
    )?;
    Ok(id)
}

/// Boards, as every chat-creating path and the chat tools ask about them. Boards and the chat's
/// sidebar group live in the projects crate, which this crate never depends on, so the answers
/// come through its [`crate::seams::ProjectGate`]. With no gate registered (most tests) there are
/// no boards: nothing is a board and no chat is put on one.
#[derive(Clone, Default)]
pub struct Boards(Option<std::sync::Arc<crate::seams::ProjectGates>>);

impl Boards {
    pub fn of(services: &helm_ipc::Services) -> Self {
        Self(services.get::<crate::seams::ProjectGates>())
    }

    pub fn from_gates(gates: Option<std::sync::Arc<crate::seams::ProjectGates>>) -> Self {
        Self(gates)
    }

    /// Whether `project_id` is a board (archived or not).
    pub fn is_board(&self, tx: &Transaction<'_>, project_id: &str) -> rusqlite::Result<bool> {
        Ok(self.board(tx, project_id)?.is_some())
    }

    /// The board's name, repository, primary folder and archived state; `None` when the id is not
    /// a board.
    pub fn board(
        &self,
        tx: &Transaction<'_>,
        project_id: &str,
    ) -> rusqlite::Result<Option<crate::seams::BoardSummary>> {
        match &self.0 {
            Some(g) => g.0.board(tx, project_id),
            None => Ok(None),
        }
    }

    /// The board the chat is shown under, if it is on one.
    pub fn chat_board(
        &self,
        tx: &Transaction<'_>,
        chat_id: &str,
    ) -> rusqlite::Result<Option<String>> {
        match &self.0 {
            Some(g) => g.0.chat_board(tx, chat_id),
            None => Ok(None),
        }
    }

    /// Put the chat on the board (its `chat_groups` row). False when nothing was written: the id is
    /// not an open board, the chat is a side chat, or there are no boards.
    pub fn put_on_board(
        &self,
        tx: &Transaction<'_>,
        chat_id: &str,
        board_id: &str,
    ) -> rusqlite::Result<bool> {
        match &self.0 {
            Some(g) => g.0.put_on_board(tx, chat_id, board_id),
            None => Ok(false),
        }
    }

    /// The default board: where chats with no other home go.
    pub fn default_board(&self, tx: &Transaction<'_>) -> rusqlite::Result<Option<String>> {
        match &self.0 {
            Some(g) => g.0.default_board(tx),
            None => Ok(None),
        }
    }

    /// The board a new chat in `folder_project_id` goes to when nobody named one.
    pub fn home_board(
        &self,
        tx: &Transaction<'_>,
        folder_project_id: Option<&str>,
    ) -> rusqlite::Result<Option<String>> {
        match &self.0 {
            Some(g) => g.0.home_board(tx, folder_project_id),
            None => Ok(None),
        }
    }
}

/// Whether `project_id` can take a new chat working in `cwd`.
#[derive(Debug, PartialEq, Eq)]
pub enum ProjectFit {
    Fits,
    NotFound,
    /// A board project or a group with no folder of its own.
    NoFolder,
    /// A folder project over another folder.
    OtherFolder,
}

/// Paths compare with either slash and without a trailing one; case-insensitively on Windows.
pub fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        let p = p.trim().replace('\\', "/");
        let p = p.trim_end_matches('/').to_owned();
        if cfg!(windows) {
            p.to_lowercase()
        } else {
            p
        }
    };
    norm(a) == norm(b)
}

pub fn project_fit(
    tx: &Transaction<'_>,
    project_id: &str,
    cwd: &str,
) -> rusqlite::Result<ProjectFit> {
    let row: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT kind, path FROM projects WHERE id = ?1",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        None => ProjectFit::NotFound,
        Some((kind, path)) => match path {
            Some(p) if kind == "folder" && !p.is_empty() => {
                if same_path(&p, cwd) {
                    ProjectFit::Fits
                } else {
                    ProjectFit::OtherFolder
                }
            }
            _ => ProjectFit::NoFolder,
        },
    })
}

/// An untouched new chat ("an untouched draft is discarded"): idle, nothing sent,
/// typed, sent with attachments, run or titled, no child, fork or side chat, no panel review
/// comment, board item link or workflow run, not pinned, archived or a side chat itself. `None` when the chat does not exist. Only such a chat may be discarded without asking.
pub fn untouched(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Option<bool>> {
    let row: Option<(String, String, String, bool, bool, Option<String>)> = tx
        .query_row(
            "SELECT status, draft, title, pinned, archived, side_source_id FROM chats WHERE id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    let Some((status, draft, title, pinned, archived, side)) = row else {
        return Ok(None);
    };
    if status != ChatStatus::Idle.as_str()
        || !draft.trim().is_empty()
        || title != DEFAULT_TITLE
        || pinned
        || archived
        || side.is_some()
    {
        return Ok(Some(false));
    }
    // Attachments and references still waiting in the composer (not on a sent message) do not
    // count: discarding the chat removes them and queues their files in the same transaction.
    let mut sql = String::from(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE chat_id = ?1)
             OR EXISTS(SELECT 1 FROM submissions WHERE chat_id = ?1)
             OR EXISTS(SELECT 1 FROM runs WHERE chat_id = ?1)
             OR EXISTS(SELECT 1 FROM chat_attachments WHERE chat_id = ?1 AND submission_id IS NOT NULL)
             OR EXISTS(SELECT 1 FROM chat_attachment_refs WHERE chat_id = ?1 AND submission_id IS NOT NULL)
             OR EXISTS(SELECT 1 FROM chat_titles WHERE chat_id = ?1 AND state != 'fallback')
             OR EXISTS(SELECT 1 FROM chats WHERE parent_chat_id = ?1 OR side_source_id = ?1)
             OR EXISTS(SELECT 1 FROM chat_forks WHERE chat_id = ?1 OR source_chat_id = ?1)",
    );
    // Work other crates keep against a chat: review comments on its panel, the board items it is
    // linked to and its workflow runs. Their tables belong to those crates, so one that has not
    // been made yet (a database without them) holds nothing.
    for table in ["review_comments", "item_chat_links", "workflow_runs"] {
        let made: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )?;
        if made {
            sql.push_str(&format!(
                "\n             OR EXISTS(SELECT 1 FROM {table} WHERE chat_id = ?1)"
            ));
        }
    }
    let any: bool = tx.query_row(&sql, [chat_id], |r| r.get(0))?;
    Ok(Some(!any))
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

/// Rows scanned per `chat_find` step.
pub const FIND_STEP: i64 = 2000;
/// The longest query searched, as in the find bar.
pub const FIND_MAX_QUERY: usize = 256;
const FIND_MAX_PER_ROW: i64 = 10_000;
/// Default and ceiling for a `stack_find` row list.
pub const STACK_FIND_DEFAULT: i64 = 50;
const STACK_FIND_MAX: i64 = 200;

fn count_in(hay: &str, needle: &str) -> i64 {
    let mut n = 0;
    let mut from = 0;
    while n < FIND_MAX_PER_ROW {
        match hay[from..].find(needle) {
            Some(i) => {
                n += 1;
                from += i + needle.len();
            }
            None => break,
        }
    }
    n
}

/// Find in chat over the whole stored history (active branch): the rows of one scan step whose
/// text contains `query`, case-insensitively, with the same notion of searchable text as the find
/// bar (`ui/src/transcript/find.ts`). Returns the cursor to continue from while rows remain.
pub fn find_step(
    tx: &Transaction<'_>,
    args: &ChatFindArgs,
) -> rusqlite::Result<Option<ChatFindResult>> {
    let chat = &args.chat_id;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1)",
        [chat],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let q: String = args
        .query
        .chars()
        .take(FIND_MAX_QUERY)
        .collect::<String>()
        .to_lowercase();
    if q.trim().is_empty() {
        return Ok(Some(ChatFindResult {
            rows: Vec::new(),
            next_cursor: None,
        }));
    }
    let activity = args.include_activity.unwrap_or(false);
    let from = match args.after.as_deref() {
        Some(c) => match parse_cursor(tx, chat, c)? {
            Some(f) => Some(f),
            None => {
                return Ok(Some(ChatFindResult {
                    rows: Vec::new(),
                    next_cursor: None,
                }))
            }
        },
        None => None,
    };
    let where_from = if from.is_some() {
        "AND (timestamp, id) > (?2, ?3)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, role, text, thinking, tool_name,
                CASE WHEN json_valid(details) AND json_type(details, '$.toolArgs') = 'text'
                     THEN json_extract(details, '$.toolArgs') END
         FROM messages WHERE chat_id = ?1 AND active = 1 {where_from}
         ORDER BY timestamp, id LIMIT ?4"
    );
    let (t, i) = from.unwrap_or((0, 0));
    let mut st = tx.prepare(&sql)?;
    let mut rows = st.query(params![chat, t, i, FIND_STEP])?;
    let mut found = Vec::new();
    let mut scanned = 0;
    let mut last = 0;
    while let Some(r) = rows.next()? {
        scanned += 1;
        last = r.get::<_, i64>(0)?;
        let role: String = r.get(1)?;
        let text: String = r.get(2)?;
        let thinking: String = r.get(3)?;
        let hay = match role.as_str() {
            "user" => text,
            "assistant" if activity && !thinking.is_empty() => format!(
                "{thinking}
{text}"
            ),
            "assistant" => text,
            _ if !activity => continue,
            "toolResult" => {
                let name: Option<String> = r.get(4)?;
                let args: Option<String> = r.get(5)?;
                [name.unwrap_or_default(), args.unwrap_or_default(), text].join(
                    "
",
                )
            }
            _ => text,
        };
        let count = count_in(&hay.to_lowercase(), &q);
        if count > 0 {
            found.push(FoundRow { id: last, count });
        }
    }
    Ok(Some(ChatFindResult {
        rows: found,
        next_cursor: (scanned == FIND_STEP).then(|| last.to_string()),
    }))
}

/// One transcript page: the latest 100 by default, or 100 before/after a
/// cursor, or 100 around a message id. With `stack` the page covers the whole roll-over
/// stack: the walk runs on into the neighbouring session when one runs out, and the page
/// carries `sessionMarks` where the sessions meet.
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
    if args.stack == Some(true) {
        return stack_page(tx, args, revision);
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
        session_marks: Vec::new(),
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
        session_marks: Vec::new(),
        revision,
    })
}

// ---------------------------------------------------------------------------------------------
// Stack paging: the whole roll-over stack read as one transcript
// ---------------------------------------------------------------------------------------------

/// How far a stack walk follows the session links before it stops. The links are built by
/// roll over and its backfill, both of which chain at most this deep; the guard only keeps a
/// hand-edited database from spinning here.
const STACK_HOPS: usize = 50;

/// A stack cursor or jump resolves to its own session anywhere in the stack: message ids are
/// one table, so the row says where it came from.
fn stack_row(tx: &Transaction<'_>, id: i64) -> rusqlite::Result<Option<(String, (i64, i64))>> {
    tx.query_row(
        "SELECT chat_id, timestamp, id FROM messages WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))),
    )
    .optional()
}

/// Whether two sessions belong to the same roll-over stack (or are the same chat).
fn same_stack(tx: &Transaction<'_>, asked: &str, session: &str) -> rusqlite::Result<bool> {
    if asked == session {
        return Ok(true);
    }
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chats a JOIN chats b \
            ON a.stack_id IS NOT NULL AND a.stack_id = b.stack_id \
            WHERE a.id = ?1 AND b.id = ?2)",
        [asked, session],
        |r| r.get(0),
    )
}

/// The neighbouring session of the stack: the one before, or the one after.
fn stack_neighbour(
    tx: &Transaction<'_>,
    session: &str,
    newer: bool,
) -> rusqlite::Result<Option<String>> {
    let col = if newer {
        "next_session_id"
    } else {
        "prev_session_id"
    };
    tx.query_row(
        &format!("SELECT {col} FROM chats WHERE id = ?1"),
        [session],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .map(|row| row.flatten())
}

/// The stack's newest session: walk forward from any session until the chain ends.
fn stack_latest(tx: &Transaction<'_>, session: &str) -> rusqlite::Result<String> {
    let mut cur = session.to_owned();
    for _ in 0..STACK_HOPS {
        match stack_neighbour(tx, &cur, true)? {
            Some(n) => cur = n,
            None => break,
        }
    }
    Ok(cur)
}

fn session_has_rows(tx: &Transaction<'_>, session: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE chat_id = ?1 AND active = 1)",
        [session],
        |r| r.get(0),
    )
}

/// The divider's words come from the older session's own row, and its roll date from the
/// session that followed it.
fn session_label(tx: &Transaction<'_>, session: &str) -> rusqlite::Result<(String, i64)> {
    tx.query_row(
        "SELECT title, created_at FROM chats WHERE id = ?1",
        [session],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map(|row| row.unwrap_or_else(|| (session.to_owned(), 0)))
}

/// More rows before the page's oldest row: in its own session, or in an earlier session of
/// the stack.
fn stack_has_older(tx: &Transaction<'_>, first: Option<&MessageRow>) -> rusqlite::Result<bool> {
    let Some(m) = first else { return Ok(false) };
    if exists_beyond(tx, &m.chat_id, Some(m), false)? {
        return Ok(true);
    }
    let mut cur = m.chat_id.clone();
    for _ in 0..STACK_HOPS {
        match stack_neighbour(tx, &cur, false)? {
            Some(p) => {
                if session_has_rows(tx, &p)? {
                    return Ok(true);
                }
                cur = p;
            }
            None => break,
        }
    }
    Ok(false)
}

/// The same beyond the page's newest row.
fn stack_has_newer(tx: &Transaction<'_>, last: Option<&MessageRow>) -> rusqlite::Result<bool> {
    let Some(m) = last else { return Ok(false) };
    if exists_beyond(tx, &m.chat_id, Some(m), true)? {
        return Ok(true);
    }
    let mut cur = m.chat_id.clone();
    for _ in 0..STACK_HOPS {
        match stack_neighbour(tx, &cur, true)? {
            Some(n) => {
                if session_has_rows(tx, &n)? {
                    return Ok(true);
                }
                cur = n;
            }
            None => break,
        }
    }
    Ok(false)
}

/// One direction of the walk: `limit` rows from `start_session`, running on into the
/// neighbouring sessions of the stack when one runs out. Each session's block is oldest
/// first; the blocks are returned newest session first for an older walk (the caller
/// reverses them) and oldest first for a newer walk.
fn stack_walk(
    tx: &Transaction<'_>,
    start_session: &str,
    start_key: Option<(i64, i64)>,
    inclusive: bool,
    newer: bool,
    limit: i64,
) -> rusqlite::Result<Vec<MessageRow>> {
    let mut blocks: Vec<Vec<MessageRow>> = Vec::new();
    let mut need = limit;
    let mut session = start_session.to_owned();
    let mut key = start_key;
    let mut first_step = true;
    for _ in 0..=STACK_HOPS {
        let rows = select_page(tx, &session, newer, key, first_step && inclusive, need)?;
        if !rows.is_empty() {
            need -= rows.len() as i64;
            blocks.push(rows);
        }
        first_step = false;
        key = None;
        if need <= 0 {
            break;
        }
        match stack_neighbour(tx, &session, newer)? {
            Some(n) => session = n,
            None => break,
        }
    }
    if newer {
        Ok(blocks.into_iter().flatten().collect())
    } else {
        Ok(blocks.into_iter().rev().flatten().collect())
    }
}

/// A stack page: the same three shapes as a single chat's, but every walk runs on across
/// session boundaries and the page says where they sit. A stack always starts at its newest
/// session: that is where the chat lives now, whoever asks.
fn stack_page(
    tx: &Transaction<'_>,
    args: &TranscriptPageArgs,
    revision: i64,
) -> rusqlite::Result<Option<TranscriptPage>> {
    let chat = &args.chat_id;
    let inclusive = args.inclusive.unwrap_or(false);
    // Which session the walk starts in: a cursor or a jump may name a row of any session in
    // this stack. A cursor outside it is not this chat's history: an empty page, as an
    // unknown cursor is today.
    let start_session = stack_latest(tx, chat)?;
    let mut cursor_session: Option<String> = None;
    let mut messages: Vec<MessageRow> = Vec::new();
    let mut walked_newer = false;
    let mut built = false;
    if args.before.is_some() || args.after.is_some() {
        let newer = args.after.is_some();
        walked_newer = newer;
        let c = if newer {
            args.after.as_deref()
        } else {
            args.before.as_deref()
        }
        .unwrap_or_default();
        let Ok(id) = c.trim().parse::<i64>() else {
            return Ok(Some(empty_page(tx, chat, revision)?));
        };
        let Some((session, key)) = stack_row(tx, id)? else {
            return Ok(Some(empty_page(tx, chat, revision)?));
        };
        if !same_stack(tx, chat, &session)? {
            return Ok(Some(empty_page(tx, chat, revision)?));
        }
        cursor_session = Some(session.clone());
        messages = stack_walk(tx, &session, Some(key), inclusive, newer, PAGE_SIZE)?;
        built = true;
    } else if let Some(target) = args.around {
        if let Some((session, key)) = stack_row(tx, target)? {
            if same_stack(tx, chat, &session)? {
                // Half below the target, half from it down: the same shape as one chat's,
                // each half free to run on across a boundary.
                let older = stack_walk(tx, &session, Some(key), false, false, PAGE_SIZE / 2)?;
                let newer = stack_walk(tx, &session, Some(key), true, true, PAGE_SIZE / 2)?;
                messages = older;
                messages.extend(newer);
                built = true;
            }
        }
    }
    if !built {
        messages = stack_walk(tx, &start_session, None, false, false, PAGE_SIZE)?;
    }
    let has_older = stack_has_older(tx, messages.first())?;
    let has_newer = stack_has_newer(tx, messages.last())?;
    let session_marks = stack_marks(tx, &messages, cursor_session.as_deref(), walked_newer)?;
    Ok(Some(TranscriptPage {
        chat_id: chat.clone(),
        older_cursor: messages.first().filter(|_| has_older).map(cursor_of),
        newer_cursor: messages.last().filter(|_| has_newer).map(cursor_of),
        has_older,
        has_newer,
        messages,
        runs: runs(tx, chat)?,
        pending: pending(tx, chat)?,
        session_marks,
        revision,
    }))
}

/// Where the page's rows change session: a mark per meeting point, oldest first. A mark
/// missing one anchor means the other side of that boundary sits in the neighbouring page —
/// the page before this one for an older walk, the one after for a newer walk.
fn stack_marks(
    tx: &Transaction<'_>,
    messages: &[MessageRow],
    cursor_session: Option<&str>,
    walked_newer: bool,
) -> rusqlite::Result<Vec<SessionMark>> {
    let mut marks: Vec<SessionMark> = Vec::new();
    if let (Some(cs), Some(first)) = (cursor_session, messages.first()) {
        if walked_newer && first.chat_id != cs {
            let (title, _) = session_label(tx, cs)?;
            let rolled_over_at = match stack_neighbour(tx, cs, true)? {
                Some(n) => session_label(tx, &n)?.1,
                None => 0,
            };
            marks.push(SessionMark {
                above_message_id: None,
                below_message_id: Some(first.id),
                session_id: cs.to_owned(),
                title,
                rolled_over_at,
            });
        }
    }
    for w in messages.windows(2) {
        let (above, below) = (&w[0], &w[1]);
        if above.chat_id == below.chat_id {
            continue;
        }
        let (title, _) = session_label(tx, &above.chat_id)?;
        marks.push(SessionMark {
            above_message_id: Some(above.id),
            below_message_id: Some(below.id),
            session_id: above.chat_id.clone(),
            title,
            rolled_over_at: session_label(tx, &below.chat_id)?.1,
        });
    }
    if let (Some(cs), Some(last)) = (cursor_session, messages.last()) {
        if !walked_newer && last.chat_id != cs {
            let (title, _) = session_label(tx, &last.chat_id)?;
            let rolled_over_at = match stack_neighbour(tx, &last.chat_id, true)? {
                Some(n) => session_label(tx, &n)?.1,
                None => 0,
            };
            marks.push(SessionMark {
                above_message_id: Some(last.id),
                below_message_id: None,
                session_id: last.chat_id.clone(),
                title,
                rolled_over_at,
            });
        }
    }
    Ok(marks)
}

/// Find in the whole roll-over stack: every session searched with the same rule as the find
/// bar, newest session first, newest row first, stopped at `limit`.
pub fn stack_find(
    tx: &Transaction<'_>,
    args: &StackFindArgs,
) -> rusqlite::Result<Option<StackFindResult>> {
    let Some(stack) = crate::rollover::stack_of(tx, &args.chat_id)? else {
        return Ok(None);
    };
    let q: String = args
        .query
        .chars()
        .take(FIND_MAX_QUERY)
        .collect::<String>()
        .to_lowercase();
    if q.trim().is_empty() {
        return Ok(Some(StackFindResult {
            rows: Vec::new(),
            truncated: false,
        }));
    }
    let activity = args.include_activity.unwrap_or(false);
    let limit = args
        .limit
        .unwrap_or(STACK_FIND_DEFAULT)
        .clamp(1, STACK_FIND_MAX);
    let mut rows: Vec<StackFound> = Vec::new();
    let mut truncated = false;
    'sessions: for session in stack.sessions.iter().rev() {
        let mut st = tx.prepare(
            "SELECT id, role, text, thinking, tool_name,
                    CASE WHEN json_valid(details) AND json_type(details, '$.toolArgs') = 'text'
                         THEN json_extract(details, '$.toolArgs') END
             FROM messages WHERE chat_id = ?1 AND active = 1
             ORDER BY timestamp DESC, id DESC",
        )?;
        let mut found = st.query([&session.id])?;
        while let Some(r) = found.next()? {
            let id: i64 = r.get(0)?;
            let role: String = r.get(1)?;
            let text: String = r.get(2)?;
            let thinking: String = r.get(3)?;
            let hay = match role.as_str() {
                "user" => text,
                "assistant" if activity && !thinking.is_empty() => format!(
                    "{thinking}
{text}"
                ),
                "assistant" => text,
                _ if !activity => continue,
                "toolResult" => {
                    let name: Option<String> = r.get(4)?;
                    let tool_args: Option<String> = r.get(5)?;
                    [
                        name.unwrap_or_default(),
                        tool_args.unwrap_or_default(),
                        text,
                    ]
                    .join(
                        "
",
                    )
                }
                _ => text,
            };
            let count = count_in(&hay.to_lowercase(), &q);
            if count > 0 {
                if rows.len() as i64 >= limit {
                    truncated = true;
                    break 'sessions;
                }
                rows.push(StackFound {
                    session_id: session.id.clone(),
                    id,
                    count,
                });
            }
        }
    }
    Ok(Some(StackFindResult { rows, truncated }))
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
    }
}

pub fn state_str(s: SubmissionState) -> &'static str {
    match s {
        SubmissionState::Saved => "saved",
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
        _ => SubmissionMode::Prompt,
    }
}

/// A submission with no origin row reads as `unknown`, which parses to `app`: never the user's own
/// typing, so it cannot pass for consent.
const SUBMISSION_SELECT: &str = "SELECT s.id, s.chat_id, s.text, s.mode, s.state, \
     COALESCE(m.origin, 'unknown'), s.created_at, m.priority, m.page_title, m.page_overview \
     FROM submissions s LEFT JOIN submission_meta m ON m.submission_id = s.id";

fn submission_from_row(r: &Row<'_>) -> rusqlite::Result<Submission> {
    Ok(Submission {
        id: r.get(0)?,
        chat_id: r.get(1)?,
        text: r.get(2)?,
        mode: parse_mode(&r.get::<_, String>(3)?),
        state: parse_state(&r.get::<_, String>(4)?),
        origin: SubmissionOrigin::parse(&r.get::<_, String>(5)?),
        created_at: r.get(6)?,
        images: None,
        refs: None,
        page_title: r.get(8)?,
        page_overview: r.get(9)?,
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

/// Messages sent but not yet read or settled, oldest first; at most 100. This includes
/// `uncertain` ones ("not sent") and `rejected` ones ("not delivered"), whose text the user can
/// take back to the composer; both leave the list once taken back.
pub fn pending(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Vec<Submission>> {
    let mut st = tx.prepare(&format!(
        "{SUBMISSION_SELECT} WHERE s.chat_id = ?1
           AND s.state IN ('saved', 'dispatching', 'accepted', 'uncertain', 'rejected')
         ORDER BY s.created_at, s.id LIMIT 100"
    ))?;
    let mut rows = st
        .query_map([chat_id], submission_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    // Each waiting message carries its attachments with it, so the waiting bubble shows the
    // pictures and files it sent, not just the text. The bytes stay on the host: the bubble
    // fetches a thumbnail by id when it is on screen (`attachment_get`).
    for s in &mut rows {
        let images = crate::message_images::refs_for(tx, &s.id)?;
        let refs = crate::attachments::refs_for(tx, &s.id)?;
        if !images.is_empty() {
            s.images = Some(images);
        }
        if !refs.is_empty() {
            s.refs = Some(
                refs.into_iter()
                    .map(|r| crate::types::MessageRef {
                        kind: r.kind,
                        name: r.name,
                        target: r.target,
                    })
                    .collect(),
            );
        }
    }
    Ok(rows)
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

    #[test]
    fn paths_match_with_either_slash_and_no_trailing_one() {
        assert!(same_path("C:/work/repo", "C:\\work\\repo\\"));
        assert!(!same_path("C:/work/repo", "C:/work/repo2"));
        assert_eq!(same_path("C:/Work", "c:/work"), cfg!(windows));
    }

    #[test]
    fn a_project_fits_only_as_a_folder_project_over_that_folder() {
        let (_d, db) = temp_db();
        let c = chat(&db);
        let fit = |p: &str, cwd: &str| db.write(|tx| project_fit(tx, p, cwd)).unwrap();
        let project = db
            .snapshot(|tx| get_chat(tx, &c, 1))
            .unwrap()
            .unwrap()
            .project_id;
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, kind, name, path, created_at) VALUES
                   ('group', 'folder', 'Group', NULL, 1),
                   ('board', 'board', 'Board', NULL, 1)",
            )
        })
        .unwrap();
        assert_eq!(fit(&project, "C:/work/repo/"), ProjectFit::Fits);
        assert_eq!(fit(&project, "C:/work/other"), ProjectFit::OtherFolder);
        assert_eq!(fit("group", "C:/work/repo"), ProjectFit::NoFolder);
        assert_eq!(fit("board", "C:/work/repo"), ProjectFit::NoFolder);
        assert_eq!(fit("nope", "C:/work/repo"), ProjectFit::NotFound);
    }

    /// Each kind of content makes a chat "touched"; only a bare new chat is not.
    #[test]
    fn only_a_bare_new_chat_is_untouched() {
        let (_d, db) = temp_db();
        let is = |id: &str| db.write(|tx| untouched(tx, id)).unwrap();
        assert_eq!(is("missing"), None);
        let bare = chat(&db);
        assert_eq!(is(&bare), Some(true));
        // The tables other crates own, reduced to the column the check reads.
        db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE review_comments(chat_id TEXT);
                 CREATE TABLE item_chat_links(chat_id TEXT);
                 CREATE TABLE workflow_runs(chat_id TEXT);",
            )
        })
        .unwrap();
        let touched: [&str; 16] = [
            "UPDATE chats SET draft = 'typed' WHERE id = ?1",
            "UPDATE chats SET title = 'Renamed' WHERE id = ?1",
            "UPDATE chats SET pinned = 1 WHERE id = ?1",
            "UPDATE chats SET archived = 1 WHERE id = ?1",
            "UPDATE chats SET status = 'running' WHERE id = ?1",
            "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES ('s-' || ?1, ?1, 't', 'saved', 1)",
            "INSERT INTO messages(chat_id, source_key, role, timestamp) VALUES (?1, 'k', 'user', 1)",
            "INSERT INTO runs(id, chat_id, status, started_at) VALUES ('r-' || ?1, ?1, 'done', 1)",
            "INSERT INTO chat_attachments(id, chat_id, name, mime, bytes, width, height, submission_id, created_at) VALUES ('a-' || ?1, ?1, 'i', 'image/png', 1, 1, 1, 's', 1)",
            "INSERT INTO chat_attachment_refs(id, chat_id, kind, target, name, submission_id, created_at) VALUES ('r-' || ?1, ?1, 'path', 't', 'n', 's', 1)",
            "INSERT INTO chat_titles(chat_id, state, updated_at) VALUES (?1, 'generated', 1)",
            "INSERT INTO chats(id, project_id, title, created_at, updated_at, parent_chat_id) SELECT 'child-' || id, project_id, 'x', 1, 1, id FROM chats WHERE id = ?1",
            "INSERT INTO chat_forks(fork_id, source_chat_id, chat_id, created_at) VALUES ('f-' || ?1, ?1, ?1, 1)",
            "INSERT INTO review_comments(chat_id) VALUES (?1)",
            "INSERT INTO item_chat_links(chat_id) VALUES (?1)",
            "INSERT INTO workflow_runs(chat_id) VALUES (?1)",
        ];
        for sql in touched {
            let c = chat(&db);
            db.write(|tx| tx.execute(sql, [&c])).unwrap();
            assert_eq!(is(&c), Some(false), "{sql}");
        }
        // A whitespace-only draft, the fallback title state and an attachment or file reference
        // not yet on a sent message still count as untouched.
        let c = chat(&db);
        db.write(|tx| {
            tx.execute("UPDATE chats SET draft = '  ' WHERE id = ?1", [&c])?;
            tx.execute(
                "INSERT INTO chat_attachments(id, chat_id, name, mime, bytes, width, height, created_at) VALUES ('u-' || ?1, ?1, 'i', 'image/png', 1, 1, 1, 1)",
                [&c],
            )?;
            tx.execute(
                "INSERT INTO chat_attachment_refs(id, chat_id, kind, target, name, created_at) VALUES ('ur-' || ?1, ?1, 'path', 't', 'n', 1)",
                [&c],
            )?;
            tx.execute(
                "INSERT INTO chat_titles(chat_id, state, updated_at) VALUES (?1, 'fallback', 1)",
                [&c],
            )
        })
        .unwrap();
        assert_eq!(is(&c), Some(true));
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
            stack: None,
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

    // -----------------------------------------------------------------------------------------
    // Stack paging: the roll-over stack read as one transcript
    // -----------------------------------------------------------------------------------------

    /// Three sessions joined as one stack, oldest first, with distinct titles and dates.
    fn stack3(db: &Database) -> [String; 3] {
        let s = [chat(db), chat(db), chat(db)];
        db.write(|tx| {
            for (i, id) in s.iter().enumerate() {
                let n = i + 1;
                tx.execute(
                    "UPDATE chats SET title = ?2, created_at = ?3, stack_id = ?4,
                        prev_session_id = ?5, next_session_id = ?6 WHERE id = ?1",
                    rusqlite::params![
                        id,
                        format!("session {n}"),
                        100 * n as i64,
                        s[0],
                        if n > 1 { Some(s[i - 1].as_str()) } else { None },
                        if n < 3 { Some(s[i + 1].as_str()) } else { None }
                    ],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        s
    }

    fn add_tagged(db: &Database, chat: &str, tag: &str, n: i64, ts0: i64) {
        db.write(|tx| {
            for i in 0..n {
                tx.execute(
                    "INSERT INTO messages(chat_id, source_key, role, text, status, timestamp) VALUES (?1, ?2, 'user', ?3, 'complete', ?4)",
                    rusqlite::params![chat, format!("{tag}k{i}"), format!("{tag}{i}"), ts0 + i],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
    }

    fn row_id(db: &Database, text: &str) -> i64 {
        db.snapshot(|tx| {
            tx.query_row("SELECT id FROM messages WHERE text = ?1", [text], |r| {
                r.get(0)
            })
        })
        .unwrap()
    }

    /// Paging up from the newest session runs on into the older ones: one order across all
    /// three, every row exactly once, and a mark where the sessions meet.
    #[test]
    fn stack_pages_run_across_sessions_in_one_order() {
        let (_d, db) = temp_db();
        let s = stack3(&db);
        add_tagged(&db, &s[0], "a", 30, 1000);
        add_tagged(&db, &s[1], "b", 90, 2000);
        add_tagged(&db, &s[2], "c", 120, 3000);
        let mut pages: Vec<TranscriptPage> = Vec::new();
        let mut cursor = None;
        for _ in 0..10 {
            let p = page(
                &db,
                TranscriptPageArgs {
                    before: cursor.clone(),
                    stack: Some(true),
                    ..args(&s[2])
                },
            );
            cursor = p.older_cursor.clone();
            let last = pages.len();
            pages.push(p);
            if !pages[last].has_older {
                break;
            }
        }
        let mut all: Vec<String> = Vec::new();
        for p in pages.iter().rev() {
            all.extend(p.messages.iter().map(|m| m.text.clone()));
        }
        let mut want: Vec<String> = Vec::new();
        for (tag, n) in [("a", 30), ("b", 90), ("c", 120)] {
            want.extend((0..n).map(|i| format!("{tag}{i}")));
        }
        assert_eq!(all, want, "one order, no row twice, none missing");
        assert_eq!(pages.len(), 3, "100 + 100 + the rest");
        let marks: Vec<&SessionMark> = pages.iter().flat_map(|p| &p.session_marks).collect();
        assert_eq!(marks.len(), 2, "one mark per session boundary");
        // Where session 2 meets session 3: the divider sits over the last session-2 row of
        // that page and under the first session-3 row, naming session 2 and its roll date.
        let bc = marks.iter().find(|m| m.session_id == s[1]).unwrap();
        assert_eq!(bc.title, "session 2");
        assert_eq!(bc.above_message_id, Some(row_id(&db, "b89")));
        assert_eq!(bc.below_message_id, Some(row_id(&db, "c0")));
        assert_eq!(
            bc.rolled_over_at, 300,
            "session 3 opened when session 2 rolled over"
        );
        let ab = marks.iter().find(|m| m.session_id == s[0]).unwrap();
        assert_eq!(ab.above_message_id, Some(row_id(&db, "a29")));
        assert_eq!(ab.below_message_id, Some(row_id(&db, "b0")));
        assert_eq!(ab.rolled_over_at, 200);
    }

    /// A jump into an older session centres there, runs on to the newer sessions below it,
    /// and the following `after` page continues where it left off.
    #[test]
    fn stack_jumps_and_forward_pages_cross_sessions_too() {
        let (_d, db) = temp_db();
        let s = stack3(&db);
        add_tagged(&db, &s[0], "a", 30, 1000);
        add_tagged(&db, &s[1], "b", 90, 2000);
        add_tagged(&db, &s[2], "c", 120, 3000);
        let target = row_id(&db, "a15");
        let p = page(
            &db,
            TranscriptPageArgs {
                around: Some(target),
                stack: Some(true),
                ..args(&s[2])
            },
        );
        let texts: Vec<String> = p.messages.iter().map(|m| m.text.clone()).collect();
        assert_eq!(
            texts.first().unwrap(),
            "a0",
            "older half runs to the session's start"
        );
        assert!(
            texts.contains(&"b10".to_owned()),
            "the newer half runs on into session 2"
        );
        assert_eq!(
            texts
                .windows(2)
                .filter(|w| w[0] == "a29" && w[1] == "b0")
                .count(),
            1,
            "the two runs meet once, in order"
        );
        assert_eq!(p.session_marks.len(), 1);
        assert_eq!(p.session_marks[0].session_id, s[0]);
        assert!(p.has_newer);
        let next = page(
            &db,
            TranscriptPageArgs {
                after: p.newer_cursor.clone(),
                stack: Some(true),
                ..args(&s[2])
            },
        );
        let first = next.messages.first().unwrap();
        assert_eq!(
            first.text, "b35",
            "the page after continues right where the jump left off"
        );
        // The forward page crosses into session 3, and says where the sessions meet.
        assert!(
            next.messages.iter().any(|m| m.text == "c0"),
            "the forward page reaches session 3"
        );
        let bc = next
            .session_marks
            .iter()
            .find(|m| m.session_id == s[1])
            .unwrap();
        assert_eq!(bc.above_message_id, Some(row_id(&db, "b89")));
        assert_eq!(bc.below_message_id, Some(row_id(&db, "c0")));
    }

    /// `stack_find` searches every session and answers newest session first, newest row
    /// first; the limit truncates; tool and thinking rows answer only with activity on.
    #[test]
    fn stack_find_searches_every_session_newest_first() {
        let (_d, db) = temp_db();
        let s = stack3(&db);
        add_tagged(&db, &s[0], "a", 5, 1000);
        add_tagged(&db, &s[1], "b", 5, 2000);
        add_tagged(&db, &s[2], "c", 5, 3000);
        db.write(|tx| {
            tx.execute("UPDATE messages SET text = text || ' and a zebra' WHERE text = 'a2'", [])?;
            tx.execute("UPDATE messages SET text = 'zebra crossing' WHERE text = 'b4'", [])?;
            tx.execute("UPDATE messages SET text = 'the second zebra' WHERE text = 'c1'", [])?;
            tx.execute("UPDATE messages SET text = 'zebra at the top' WHERE text = 'c4'", [])?;
            tx.execute(
                "INSERT INTO messages(chat_id, source_key, role, text, status, timestamp) VALUES (?1, 'xk', 'toolResult', 'zebra in a tool', 'complete', 3100)",
                [&s[2]],
            )
        })
        .unwrap();
        let find = |q: &str, activity: bool, limit: Option<i64>| -> StackFindResult {
            db.snapshot(|tx| {
                stack_find(
                    tx,
                    &StackFindArgs {
                        chat_id: s[2].clone(),
                        query: q.into(),
                        include_activity: activity.then_some(true),
                        limit,
                    },
                )
            })
            .unwrap()
            .unwrap()
        };
        let hits = find("ZEBRA", false, None);
        let got: Vec<String> = hits
            .rows
            .iter()
            .map(|h| {
                let text: String = db
                    .snapshot(|tx| {
                        tx.query_row("SELECT text FROM messages WHERE id = ?1", [h.id], |r| {
                            r.get(0)
                        })
                    })
                    .unwrap();
                let session = if h.session_id == s[0] {
                    "a"
                } else if h.session_id == s[1] {
                    "b"
                } else {
                    "c"
                };
                format!("{session}:{text}")
            })
            .collect();
        // Asking from the newest session still finds the word only an older session has,
        // newest session first, newest row first inside each.
        assert_eq!(
            got,
            [
                "c:zebra at the top",
                "c:the second zebra",
                "b:zebra crossing",
                "a:a2 and a zebra",
            ],
            "case-insensitive, every session, one order"
        );
        assert!(!hits.truncated);
        let capped = find("zebra", false, Some(2));
        assert_eq!(capped.rows.len(), 2);
        assert!(capped.truncated, "the limit cut the list short");
        let with_tools = find("zebra", true, None);
        assert_eq!(
            with_tools.rows.len(),
            5,
            "the tool row answers only with activity on"
        );
        // A lone chat finds its own rows; a chat that does not exist is not found.
        let (_d2, db2) = temp_db();
        let lone = chat(&db2);
        add_tagged(&db2, &lone, "z", 3, 1000);
        let one = db2
            .snapshot(|tx| {
                stack_find(
                    tx,
                    &StackFindArgs {
                        chat_id: lone,
                        query: "z0".into(),
                        include_activity: None,
                        limit: None,
                    },
                )
            })
            .unwrap()
            .unwrap();
        assert_eq!(one.rows.len(), 1);
        assert!(db2
            .snapshot(|tx| stack_find(
                tx,
                &StackFindArgs {
                    chat_id: "nope".into(),
                    query: "z".into(),
                    include_activity: None,
                    limit: None
                },
            ))
            .unwrap()
            .is_none());
    }

    /// A chat no roll over touched answers a stack page exactly as a plain page.
    #[test]
    fn a_lone_chat_pages_the_same_with_the_stack_flag() {
        let (_d, db) = temp_db();
        let c = chat(&db);
        add_messages(&db, &c, 150);
        let plain = page(&db, args(&c));
        let stacked = page(
            &db,
            TranscriptPageArgs {
                stack: Some(true),
                ..args(&c)
            },
        );
        assert_eq!(stacked, plain);
        assert!(stacked.session_marks.is_empty());
    }
}
