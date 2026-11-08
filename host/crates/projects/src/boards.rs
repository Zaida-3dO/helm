//! Boards as the place every chat is shown.
//!
//! A chat runs in a folder project and is shown under a board project (its `chat_groups` row).
//! This module holds what the rest of Helm asks about that:
//!
//! - the answers behind the chats crate's project gate ([`board_summary`], [`chat_board`],
//!   [`put_on_board`], [`default_board`], [`home_board`]), so every path that makes a chat can
//!   put it on a board;
//! - the default board: a setting, else the board named [`DEFAULT_BOARD_NAME`]. Boards named in
//!   [`HIDDEN_BOARD_NAMES`] are left out of pickers;
//! - [`fill_primary_folders`], run on every sync: a board with no folder whose repository names
//!   exactly one folder gets that folder. A folder already set is never replaced;
//! - the one-time move of chats onto boards ([`migrate_chats_onto_boards`]), run after a
//!   successful sync so it never runs before there are boards to move chats onto. A chat it could
//!   only send to the default board follows a board folder set later
//!   ([`rehome_fallback_chats`]), so where it ends up does not depend on which came first;
//! - `board_project_create`: a new board project made on Agent Standup itself, with its folder.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `default_board_get` | blocking | `{}` → [`DefaultBoardView`] |
//! | `default_board_set` | maintenance | `{boardId: string \| null}` → [`DefaultBoardView`] |
//! | `board_project_create` | async | [`BoardProjectCreateArgs`] → `ProjectNode` (the new board) |

use crate::board::{ctx_credentials, test_override};
use crate::ops::{emit_project, emit_sidebar, parse_args, to_value};
use crate::store::{self, Refusal};
use helm_chats::seams::BoardSummary;
use helm_core::db::Database;
use helm_core::prefs::PrefKey;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use helm_standup::{BoardProject, Client, StandupError};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// The board chats with no other home go to while the setting is unset.
pub const DEFAULT_BOARD_NAME: &str = "Life Admin";
/// Boards no project picker offers (the default board may still be one if the user picks it).
pub const HIDDEN_BOARD_NAMES: &[&str] = &["Inbox"];
const DEFAULT_BOARD: PrefKey = PrefKey::classified("defaultBoard");

/// The one-time move's marker in `counters` (the same convention as the one-time migrations).
pub const MIGRATION: &str = "projects-chats-onto-boards";
/// Where the one-time move writes what it did (an internal preference row, one JSON value).
pub const MIGRATION_LOG_KEY: &str = "migration:chatsOntoBoardsLog";
/// Linked items the move asks the board about, at most.
pub const MAX_ITEM_LOOKUPS: usize = 200;
/// Parent steps climbed from an item to its project, or from a group to a board above it, at most.
const MAX_HOPS: usize = 6;

/// Why the one-time move put a chat where it did (the log's `why`).
const WHY_LINKED: &str = "linked item";
const WHY_ABOVE: &str = "board above its group";
const WHY_FOLDER: &str = "folder";
const WHY_DEFAULT: &str = "default board";

/// An open board: not archived or closed here and still on the board.
const OPEN_BOARD: &str = "p.kind = 'board' AND COALESCE(l.archived, 0) = 0
     AND COALESCE(l.closed, 0) = 0 AND COALESCE(b.stale, 0) = 0";
const BOARD_JOINS: &str = "LEFT JOIN project_local l ON l.project_id = p.id
     LEFT JOIN board_projects b ON b.project_id = p.id";

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("default_board_get", OpClass::Blocking, default_board_get)
        .blocking("default_board_set", OpClass::Maintenance, default_board_set)
        .async_op("board_project_create", board_project_create);
}

// ---- what the chats crate asks ----------------------------------------------------------------

/// The board with this id; `None` when it is a folder project or unknown.
pub fn board_summary(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<BoardSummary>> {
    tx.query_row(
        "SELECT p.id, COALESCE(l.title_override, b.title, p.name), b.repo,
                l.primary_folder_project_id, f.path, COALESCE(l.archived, 0), COALESCE(b.stale, 0),
                COALESCE(l.closed, 0)
         FROM projects p
         LEFT JOIN project_local l ON l.project_id = p.id
         LEFT JOIN board_projects b ON b.project_id = p.id
         LEFT JOIN projects f ON f.id = l.primary_folder_project_id
         WHERE p.id = ?1 AND p.kind = 'board'",
        [id],
        |r| {
            Ok(BoardSummary {
                id: r.get(0)?,
                name: r.get(1)?,
                repo: r.get(2)?,
                folder_project_id: r.get(3)?,
                folder_path: r.get(4)?,
                archived: r.get::<_, i64>(5)? != 0,
                stale: r.get::<_, i64>(6)? != 0,
                closed: r.get::<_, i64>(7)? != 0,
            })
        },
    )
    .optional()
}

/// The board the chat is shown under, if it is on one.
pub fn chat_board(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT g.project_id FROM chat_groups g JOIN projects p ON p.id = g.project_id
         WHERE g.chat_id = ?1 AND p.kind = 'board'",
        [chat_id],
        |r| r.get(0),
    )
    .optional()
}

/// Show the chat under the board (replacing any group it had). False, with nothing written, when
/// the id is not a board that is open here or the chat is a side chat (never shown in the
/// sidebar) or unknown.
pub fn put_on_board(tx: &Transaction<'_>, chat_id: &str, board_id: &str) -> rusqlite::Result<bool> {
    let ok: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects p LEFT JOIN project_local l ON l.project_id = p.id
                       WHERE p.id = ?1 AND p.kind = 'board' AND COALESCE(l.archived, 0) = 0)
            AND EXISTS(SELECT 1 FROM chats WHERE id = ?2 AND side_source_id IS NULL)",
        [board_id, chat_id],
        |r| r.get(0),
    )?;
    if !ok {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO chat_groups(chat_id, project_id) VALUES (?1, ?2)
         ON CONFLICT(chat_id) DO UPDATE SET project_id = excluded.project_id",
        [chat_id, board_id],
    )?;
    Ok(true)
}

fn is_open_board(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM projects p {BOARD_JOINS} WHERE p.id = ?1 AND {OPEN_BOARD})"),
        [id],
        |r| r.get(0),
    )
}

/// The stored default-board setting, as written (it may name a board that has since gone).
fn chosen_default(tx: &Transaction<'_>) -> Option<String> {
    helm_core::prefs::read(tx, DEFAULT_BOARD.name())
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
}

/// The open board shown as `name` (its local name or its board title, ignoring case). A board
/// whose work is all finished comes last.
pub fn board_named(tx: &Transaction<'_>, name: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        &format!(
            "SELECT p.id FROM projects p {BOARD_JOINS}
             WHERE {OPEN_BOARD}
               AND (lower(trim(COALESCE(l.title_override, b.title, p.name))) = lower(?1)
                    OR lower(trim(COALESCE(b.title, ''))) = lower(?1))
             ORDER BY COALESCE(b.completed, 0), p.created_at DESC, p.id LIMIT 1"
        ),
        [name.trim()],
        |r| r.get(0),
    )
    .optional()
}

/// Where chats with no other home go: the board the setting names while it is open, else the
/// open board named [`DEFAULT_BOARD_NAME`]. `None` before any board has synced.
pub fn default_board(tx: &Transaction<'_>) -> rusqlite::Result<Option<String>> {
    if let Some(id) = chosen_default(tx) {
        if is_open_board(tx, &id)? {
            return Ok(Some(id));
        }
    }
    board_named(tx, DEFAULT_BOARD_NAME)
}

/// The one open, unfinished board whose folder is `folder_project_id`; `None` when there is no
/// such board or several. Folders compare by path, so two folder projects over the same folder
/// count as one folder. A board that shares its folder with another is never chosen here: the
/// choice would be a guess, so only a board whose folder is its own is picked by folder.
pub fn folder_board(
    tx: &Transaction<'_>,
    folder_project_id: &str,
) -> rusqlite::Result<Option<String>> {
    let want: Option<String> = tx
        .query_row(
            "SELECT path FROM projects WHERE id = ?1",
            [folder_project_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|p| !p.trim().is_empty())
        .map(|p| norm_path(&p));
    let mut st = tx.prepare_cached(&format!(
        "SELECT p.id, l.primary_folder_project_id, f.path FROM projects p {BOARD_JOINS}
         LEFT JOIN projects f ON f.id = l.primary_folder_project_id
         WHERE {OPEN_BOARD} AND COALESCE(b.completed, 0) = 0
           AND l.primary_folder_project_id IS NOT NULL"
    ))?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let ids: Vec<&String> = rows
        .iter()
        .filter(|(_, folder, path)| match (&want, path) {
            (Some(w), Some(p)) => norm_path(p) == *w,
            _ => folder == folder_project_id,
        })
        .map(|(id, _, _)| id)
        .collect();
    Ok(match ids.as_slice() {
        [only] => Some((*only).clone()),
        _ => None,
    })
}

/// The board a new chat goes to when nobody named one: the board over its folder
/// ([`folder_board`]), else the default board.
pub fn home_board(
    tx: &Transaction<'_>,
    folder_project_id: Option<&str>,
) -> rusqlite::Result<Option<String>> {
    if let Some(f) = folder_project_id {
        if let Some(b) = folder_board(tx, f)? {
            return Ok(Some(b));
        }
    }
    default_board(tx)
}

/// Boards no picker offers ([`HIDDEN_BOARD_NAMES`]).
pub fn hidden_boards(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut st = tx.prepare(&format!(
        "SELECT p.id FROM projects p {BOARD_JOINS}
         WHERE p.kind = 'board'
           AND (lower(trim(COALESCE(l.title_override, b.title, p.name))) = lower(?1)
                OR lower(trim(COALESCE(b.title, ''))) = lower(?1))
         ORDER BY p.id"
    ))?;
    for name in HIDDEN_BOARD_NAMES {
        for id in st.query_map([*name], |r| r.get::<_, String>(0))? {
            out.push(id?);
        }
    }
    Ok(out)
}

// ---- folders filled in on sync ----------------------------------------------------------------

/// A folder path compared the way Helm compares folders: either slash, no trailing one,
/// ignoring case on Windows.
fn norm_path(p: &str) -> String {
    let p = p.trim().replace('\\', "/");
    let p = p.trim_end_matches('/').to_owned();
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p
    }
}

/// The last part of a path or a repository name (`owner/name`, `name.git`), lowercase.
fn last_part(s: &str) -> String {
    let s = s.trim().replace('\\', "/");
    let s = s.trim_end_matches('/');
    let last = s.rsplit('/').next().unwrap_or(s);
    last.strip_suffix(".git").unwrap_or(last).to_lowercase()
}

/// Give every board that has no folder, and whose repository names exactly one folder (by the
/// folder's own name), that folder. A board whose folder is already set, by the user or earlier,
/// is never touched. Several projects over the one folder count as one folder; the one with the
/// most chats is used. Returns the folder projects boards gained.
pub fn fill_primary_folders(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    let boards: Vec<(String, String)> = {
        let mut st = tx.prepare(
            "SELECT p.id, b.repo FROM projects p
             JOIN board_projects b ON b.project_id = p.id
             LEFT JOIN project_local l ON l.project_id = p.id
             WHERE p.kind = 'board' AND l.primary_folder_project_id IS NULL
               AND b.repo IS NOT NULL AND trim(b.repo) != ''",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if boards.is_empty() {
        return Ok(Vec::new());
    }
    let folders: Vec<(String, String)> = {
        let mut st = tx.prepare(
            "SELECT p.id, p.path FROM projects p
             LEFT JOIN project_local l ON l.project_id = p.id
             WHERE p.kind = 'folder' AND p.path IS NOT NULL AND p.path != ''
               AND COALESCE(l.archived, 0) = 0
             ORDER BY (SELECT count(*) FROM chats c WHERE c.project_id = p.id) DESC,
                      p.created_at, p.id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut filled = Vec::new();
    for (board, repo) in boards {
        let want = last_part(&repo);
        if want.is_empty() {
            continue;
        }
        let matches: Vec<&(String, String)> = folders
            .iter()
            .filter(|(_, path)| last_part(path) == want)
            .collect();
        let mut paths: Vec<String> = matches.iter().map(|(_, p)| norm_path(p)).collect();
        paths.sort();
        paths.dedup();
        if paths.len() != 1 {
            continue;
        }
        tx.execute(
            "INSERT OR IGNORE INTO project_local(project_id) VALUES (?1)",
            [&board],
        )?;
        let set = tx.execute(
            "UPDATE project_local SET primary_folder_project_id = ?2, revision = revision + 1
             WHERE project_id = ?1 AND primary_folder_project_id IS NULL",
            params![board, matches[0].0],
        )?;
        if set > 0 {
            filled.push(matches[0].0.clone());
        }
    }
    Ok(filled)
}

// ---- the one-time move of chats onto boards ---------------------------------------------------

/// An Agent Standup id as item links store it: lowercase hex without hyphens, at least 8 long.
fn norm_id(raw: &str) -> Option<String> {
    let s: String = raw
        .trim()
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (s.len() >= 8 && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

/// A full id in the board's own form (8-4-4-4-12), or the id as it is.
fn hyphenate(id: &str) -> String {
    if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!(
            "{}-{}-{}-{}-{}",
            &id[..8],
            &id[8..12],
            &id[12..16],
            &id[16..20],
            &id[20..]
        )
    } else {
        id.to_owned()
    }
}

fn table_exists(tx: &Transaction<'_>, name: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )
}

/// The open board whose board id is `item` (an id or a prefix of one, either way round).
fn open_board_for_item(tx: &Transaction<'_>, item: &str) -> rusqlite::Result<Option<String>> {
    let Some(item) = norm_id(item) else {
        return Ok(None);
    };
    tx.prepare_cached(
        "SELECT p.id FROM board_projects b JOIN projects p ON p.id = b.project_id
         LEFT JOIN project_local l ON l.project_id = p.id
         WHERE COALESCE(l.archived, 0) = 0 AND b.stale = 0
           AND length(replace(lower(b.board_project_id), '-', '')) >= 8
           AND (replace(lower(b.board_project_id), '-', '') LIKE ?1 || '%'
                OR ?1 LIKE replace(lower(b.board_project_id), '-', '') || '%')
         ORDER BY p.created_at DESC, p.id LIMIT 1",
    )?
    .query_row([&item], |r| r.get(0))
    .optional()
}

/// What the board said about linked items: item id → the id of the project it sits under (both
/// in the stored form).
pub type ItemProjects = HashMap<String, String>;

/// The board of the chat's linked Agent Standup items: its main links first, then the newest.
/// An item that is itself a board project names that board; otherwise `learnt` may say which
/// project it sits under.
fn linked_board(
    tx: &Transaction<'_>,
    chat_id: &str,
    learnt: &ItemProjects,
) -> rusqlite::Result<Option<String>> {
    let items: Vec<String> = {
        let mut st = tx.prepare_cached(
            "SELECT item_id FROM item_chat_links WHERE chat_id = ?1
             ORDER BY is_main DESC, linked_at DESC",
        )?;
        let rows = st.query_map([chat_id], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for item in items {
        let Some(item) = norm_id(&item) else {
            continue;
        };
        if let Some(b) = open_board_for_item(tx, &item)? {
            return Ok(Some(b));
        }
        if let Some(project) = learnt.get(&item) {
            if let Some(b) = open_board_for_item(tx, project)? {
                return Ok(Some(b));
            }
        }
    }
    Ok(None)
}

/// The move has not run yet and a sync has brought boards in.
pub fn migration_pending(tx: &Transaction<'_>) -> rusqlite::Result<bool> {
    let done: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM counters WHERE key = ?1)",
        [helm_core::db::registry::marker_key(MIGRATION)],
        |r| r.get(0),
    )?;
    if done {
        return Ok(false);
    }
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM board_sync_state WHERE id = 1 AND last_sync_at IS NOT NULL)
            AND EXISTS(SELECT 1 FROM board_projects WHERE stale = 0)",
        [],
        |r| r.get(0),
    )
}

/// A chat not on a board today (side chats never are).
struct Unplaced {
    id: String,
    title: String,
    parent: Option<String>,
    /// The folder project it runs in.
    folder: String,
    /// The project it is shown under, and its name.
    from_id: String,
    from: String,
}

fn unplaced_chats(tx: &Transaction<'_>) -> rusqlite::Result<Vec<Unplaced>> {
    let mut st = tx.prepare(
        "SELECT c.id, c.title, c.parent_chat_id, c.project_id, gp.id, gp.name FROM chats c
         LEFT JOIN chat_groups g ON g.chat_id = c.id
         JOIN projects gp ON gp.id = COALESCE(g.project_id, c.project_id)
         WHERE c.side_source_id IS NULL AND gp.kind != 'board'
         ORDER BY c.created_at, c.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Unplaced {
            id: r.get(0)?,
            title: r.get(1)?,
            parent: r.get(2)?,
            folder: r.get(3)?,
            from_id: r.get(4)?,
            from: r.get(5)?,
        })
    })?;
    rows.collect()
}

/// Linked items of chats not on a board that the mirror alone cannot place: full ids only (the
/// board is asked by id), at most [`MAX_ITEM_LOOKUPS`].
pub fn items_to_look_up(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    if !table_exists(tx, "item_chat_links")? {
        return Ok(Vec::new());
    }
    let raw: Vec<String> = {
        let mut st = tx.prepare(
            "SELECT DISTINCT k.item_id FROM item_chat_links k
             JOIN chats c ON c.id = k.chat_id
             LEFT JOIN chat_groups g ON g.chat_id = c.id
             JOIN projects gp ON gp.id = COALESCE(g.project_id, c.project_id)
             WHERE c.side_source_id IS NULL AND gp.kind != 'board'
             ORDER BY k.item_id",
        )?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::new();
    for item in raw {
        let Some(item) = norm_id(&item) else {
            continue;
        };
        if item.len() != 32 || open_board_for_item(tx, &item)?.is_some() {
            continue;
        }
        out.push(item);
        if out.len() >= MAX_ITEM_LOOKUPS {
            break;
        }
    }
    Ok(out)
}

/// Ask the board which project each item sits under, climbing parents. `None` when the board
/// stopped answering (the move then waits for the next sync); an item it does not know is
/// skipped.
pub fn learn_item_projects(client: &Client, items: &[String]) -> Option<ItemProjects> {
    let mut out = ItemProjects::new();
    for item in items {
        let mut cur = hyphenate(item);
        let mut root = None;
        for _ in 0..MAX_HOPS {
            match client.item(&cur) {
                Ok(v) => {
                    let it = v.get("item").unwrap_or(&v);
                    match it.get("parentId").and_then(Value::as_str) {
                        Some(p) if !p.trim().is_empty() => cur = p.trim().to_owned(),
                        _ => {
                            root = Some(cur.clone());
                            break;
                        }
                    }
                }
                Err(
                    StandupError::Unreachable(_)
                    | StandupError::Unauthorized
                    | StandupError::NotConfigured
                    | StandupError::NotStarted,
                ) => return None,
                Err(e) => {
                    log::info!("moving chats onto boards: item {item} was not read: {e}");
                    break;
                }
            }
        }
        if let Some(r) = root.as_deref().and_then(norm_id) {
            out.insert(item.clone(), r);
        }
    }
    Some(out)
}

/// What the one-time move did, as written to [`MIGRATION_LOG_KEY`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationLog {
    pub at: i64,
    pub default_board_id: String,
    pub moved: Vec<MovedChat>,
    /// Chats whose parent landed on another board: they are no longer nested under it.
    pub unnested: Vec<Unnested>,
    /// Chats moved again later because a board's folder was set or changed
    /// ([`rehome_fallback_chats`]), oldest first.
    #[serde(default)]
    pub rehomed: Vec<Rehomed>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovedChat {
    pub chat_id: String,
    pub title: String,
    /// The project it was shown under, and its name.
    pub from_id: String,
    pub from: String,
    pub to_id: String,
    pub to: String,
    /// `linked item`, `board above its group`, `folder` or `default board`.
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unnested {
    pub chat_id: String,
    pub parent_chat_id: String,
}

/// A chat the one-time move could only send to the default board, moved again once a board
/// folder changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rehomed {
    pub chat_id: String,
    pub at: i64,
    pub from_id: String,
    pub to_id: String,
    pub to: String,
    /// `folder` (the one board over its folder) or `default board` (no single board is).
    pub why: String,
}

/// The chats a board folder change moved, and the nesting it dropped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rehoming {
    pub moved: Vec<Rehomed>,
    pub unnested: Vec<Unnested>,
}

/// The nearest open board above `group` in the project tree (climbing at most [`MAX_HOPS`]
/// parents, stopping at a loop).
fn board_above(tx: &Transaction<'_>, group: &str) -> rusqlite::Result<Option<String>> {
    let mut seen = vec![group.to_owned()];
    for _ in 0..MAX_HOPS {
        let cur = seen.last().expect("starts with the group").clone();
        let parent: Option<String> = tx
            .query_row(
                "SELECT parent_id FROM project_parents WHERE project_id = ?1",
                [&cur],
                |r| r.get(0),
            )
            .optional()?;
        let Some(parent) = parent else {
            return Ok(None);
        };
        if seen.contains(&parent) {
            return Ok(None);
        }
        if is_open_board(tx, &parent)? {
            return Ok(Some(parent));
        }
        seen.push(parent);
    }
    Ok(None)
}

/// Each moved chat with its parent, then each child of a moved chat with that chat (each chat
/// once).
fn nesting_pairs(tx: &Transaction<'_>, moved: &[&str]) -> rusqlite::Result<Vec<(String, String)>> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut parent_of = tx.prepare_cached(
        "SELECT parent_chat_id FROM chats WHERE id = ?1 AND parent_chat_id IS NOT NULL",
    )?;
    for m in moved {
        if let Some(p) = parent_of
            .query_row([m], |r| r.get::<_, String>(0))
            .optional()?
        {
            pairs.push(((*m).to_owned(), p));
        }
    }
    let mut kids_of = tx.prepare_cached(
        "SELECT id FROM chats WHERE parent_chat_id = ?1 AND side_source_id IS NULL",
    )?;
    for m in moved {
        let kids = kids_of
            .query_map([m], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for k in kids {
            if !pairs.iter().any(|(c, _)| *c == k) {
                pairs.push((k, (*m).to_owned()));
            }
        }
    }
    Ok(pairs)
}

/// Unnest each (chat, parent) pair now on different boards, so no parent shows an expander with
/// nothing under it.
fn unnest_across_boards(
    tx: &Transaction<'_>,
    pairs: Vec<(String, String)>,
) -> rusqlite::Result<Vec<Unnested>> {
    let mut out = Vec::new();
    for (chat, parent) in pairs {
        if chat_board(tx, &chat)? != chat_board(tx, &parent)? {
            tx.execute(
                "UPDATE chats SET parent_chat_id = NULL WHERE id = ?1",
                [&chat],
            )?;
            out.push(Unnested {
                chat_id: chat,
                parent_chat_id: parent,
            });
        }
    }
    Ok(out)
}

/// Put every chat that is not on a board onto one, once: the board of its linked Agent Standup
/// item when Helm can tell it, else the board its group is nested under, else the one board
/// whose folder it runs in, else the default board. A chat and its parent that land on the same
/// board stay nested; a chat whose parent lands on another board is no longer nested under it
/// (logged, so it can be put back). Writes [`MIGRATION_LOG_KEY`] and the marker.
///
/// Does nothing (and stays pending) until a sync has brought boards in and a default board can be
/// found; `Ok(None)` then, and once it has run.
pub fn migrate_chats_onto_boards(
    tx: &Transaction<'_>,
    now: i64,
    learnt: &ItemProjects,
) -> rusqlite::Result<Option<MigrationLog>> {
    if !migration_pending(tx)? {
        return Ok(None);
    }
    let Some(default) = default_board(tx)? else {
        log::info!(
            "moving chats onto boards waits for a default board (set one, or a board named {DEFAULT_BOARD_NAME})"
        );
        return Ok(None);
    };
    let links = table_exists(tx, "item_chat_links")?;
    let mut names: HashMap<String, String> = HashMap::new();
    let mut out = MigrationLog {
        at: now,
        default_board_id: default.clone(),
        moved: Vec::new(),
        unnested: Vec::new(),
        rehomed: Vec::new(),
    };
    let mut parents: Vec<(String, String)> = Vec::new();
    for Unplaced {
        id: chat,
        title,
        parent,
        folder,
        from_id,
        from,
    } in unplaced_chats(tx)?
    {
        let linked = if links {
            linked_board(tx, &chat, learnt)?
        } else {
            None
        };
        let (to, why) = match linked {
            Some(b) => (b, WHY_LINKED),
            None => match board_above(tx, &from_id)? {
                Some(b) => (b, WHY_ABOVE),
                None => match folder_board(tx, &folder)? {
                    Some(b) => (b, WHY_FOLDER),
                    None => (default.clone(), WHY_DEFAULT),
                },
            },
        };
        if !put_on_board(tx, &chat, &to)? {
            continue;
        }
        if !names.contains_key(&to) {
            let n = board_summary(tx, &to)?.map(|b| b.name).unwrap_or_default();
            names.insert(to.clone(), n);
        }
        if let Some(p) = parent {
            parents.push((chat.clone(), p));
        }
        out.moved.push(MovedChat {
            to: names.get(&to).cloned().unwrap_or_default(),
            chat_id: chat,
            title,
            from_id,
            from,
            to_id: to,
            why: why.to_owned(),
        });
    }
    // Children of moved chats count too: a child already on a board whose parent moved elsewhere.
    for m in &out.moved {
        let mut st = tx.prepare_cached(
            "SELECT id FROM chats WHERE parent_chat_id = ?1 AND side_source_id IS NULL",
        )?;
        let kids = st
            .query_map([&m.chat_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for k in kids {
            if !parents.iter().any(|(c, _)| *c == k) {
                parents.push((k, m.chat_id.clone()));
            }
        }
    }
    for (chat, parent) in parents {
        if chat_board(tx, &chat)? != chat_board(tx, &parent)? {
            tx.execute(
                "UPDATE chats SET parent_chat_id = NULL WHERE id = ?1",
                [&chat],
            )?;
            out.unnested.push(Unnested {
                chat_id: chat,
                parent_chat_id: parent,
            });
        }
    }
    let record = serde_json::to_value(&out).unwrap_or(Value::Null);
    if let Err(e) = helm_core::prefs::set_raw(tx, MIGRATION_LOG_KEY, &record) {
        log::warn!("moving chats onto boards: the record was not kept: {e}");
    }
    tx.execute(
        "INSERT INTO counters(key, value) VALUES (?1, ?2)",
        params![helm_core::db::registry::marker_key(MIGRATION), now],
    )?;
    let count = |why: &str| out.moved.iter().filter(|m| m.why == why).count();
    log::info!(
        "moved {} chats onto boards ({} by their linked item, {} by the board above their group, {} by their folder, the rest to the default board; {} no longer nested); the list is in the {MIGRATION_LOG_KEY} preference",
        out.moved.len(),
        count(WHY_LINKED),
        count(WHY_ABOVE),
        count(WHY_FOLDER),
        out.unnested.len()
    );
    Ok(Some(out))
}

/// After a board's folder is set, changed or cleared (`folders`: its folder before and after).
///
/// Every chat the one-time move could only send to the default board, that runs in one of
/// `folders` and still sits where the move (or an earlier call of this) left it, goes where the
/// move would put it now: the one board over its folder, else the default board the move used.
/// So where such a chat ends up does not depend on whether a board's folder was set before the
/// move ran or after. A chat the user has moved since is left alone. Each move is added to the
/// move's log. Does nothing before the move has run.
pub fn rehome_fallback_chats(
    tx: &Transaction<'_>,
    folders: &[String],
    now: i64,
) -> rusqlite::Result<Rehoming> {
    let mut out = Rehoming::default();
    if folders.is_empty() {
        return Ok(out);
    }
    let Some(raw) = helm_core::prefs::read(tx, MIGRATION_LOG_KEY) else {
        return Ok(out);
    };
    let mut log: MigrationLog = match serde_json::from_value(raw) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("the record of moving chats onto boards is unreadable: {e}");
            return Ok(out);
        }
    };
    let mut names: HashMap<String, String> = HashMap::new();
    for m in log.moved.iter().filter(|m| m.why == WHY_DEFAULT) {
        let left_on = log
            .rehomed
            .iter()
            .rev()
            .find(|r| r.chat_id == m.chat_id)
            .map_or(m.to_id.as_str(), |r| r.to_id.as_str());
        let folder: Option<String> = tx
            .query_row(
                "SELECT project_id FROM chats WHERE id = ?1 AND side_source_id IS NULL",
                [&m.chat_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(folder) = folder.filter(|f| folders.contains(f)) else {
            continue;
        };
        if chat_board(tx, &m.chat_id)?.as_deref() != Some(left_on) {
            continue;
        }
        let (to, why) = match folder_board(tx, &folder)? {
            Some(b) => (b, WHY_FOLDER),
            None => (log.default_board_id.clone(), WHY_DEFAULT),
        };
        if to == left_on || !put_on_board(tx, &m.chat_id, &to)? {
            continue;
        }
        if !names.contains_key(&to) {
            let n = board_summary(tx, &to)?.map(|b| b.name).unwrap_or_default();
            names.insert(to.clone(), n);
        }
        out.moved.push(Rehomed {
            chat_id: m.chat_id.clone(),
            at: now,
            from_id: left_on.to_owned(),
            to: names.get(&to).cloned().unwrap_or_default(),
            to_id: to,
            why: why.to_owned(),
        });
    }
    if out.moved.is_empty() {
        return Ok(out);
    }
    let ids: Vec<&str> = out.moved.iter().map(|m| m.chat_id.as_str()).collect();
    let pairs = nesting_pairs(tx, &ids)?;
    out.unnested = unnest_across_boards(tx, pairs)?;
    log.rehomed.extend(out.moved.iter().cloned());
    log.unnested.extend(out.unnested.iter().cloned());
    let record = serde_json::to_value(&log).unwrap_or(Value::Null);
    if let Err(e) = helm_core::prefs::set_raw(tx, MIGRATION_LOG_KEY, &record) {
        log::warn!("moving chats onto boards: the record was not kept: {e}");
    }
    log::info!(
        "a board folder changed: moved {} chats the one-time move had put on the default board ({} no longer nested); listed in the {MIGRATION_LOG_KEY} preference",
        out.moved.len(),
        out.unnested.len()
    );
    Ok(out)
}

/// Tell the window about chats that moved between groups: their rows, then the projects they
/// left and joined, so each header's chat count is current (an emptied folder group drops out).
pub(crate) fn emit_moves<'a>(
    ctx: &OpCtx,
    db: &Database,
    chats: Vec<String>,
    projects: impl IntoIterator<Item = &'a str>,
) {
    emit_sidebar(ctx, &chats);
    let mut seen = HashSet::new();
    for p in projects {
        if !seen.insert(p) {
            continue;
        }
        match db.snapshot(|tx| store::get_project(tx, p)) {
            Ok(node) => emit_project(ctx, &node),
            Err(e) => log::debug!("project {p} after a move: {e:?}"),
        }
    }
}

/// [`emit_moves`] for what [`rehome_fallback_chats`] did.
pub(crate) fn emit_rehoming(ctx: &OpCtx, db: &Database, r: &Rehoming) {
    if r.moved.is_empty() {
        return;
    }
    let chats = r
        .moved
        .iter()
        .map(|m| m.chat_id.clone())
        .chain(r.unnested.iter().map(|u| u.chat_id.clone()))
        .collect();
    let projects = r
        .moved
        .iter()
        .flat_map(|m| [m.from_id.as_str(), m.to_id.as_str()]);
    emit_moves(ctx, db, chats, projects);
}

/// After a successful sync: run the one-time move if it is still pending, asking the board where
/// linked items sit first. Never fails the sync; a problem is logged and the move waits for the
/// next sync.
pub(crate) async fn after_sync(ctx: &OpCtx, db: &Database, client: Client) {
    let ready = db.snapshot(|tx| {
        Ok::<_, rusqlite::Error>(migration_pending(tx)? && default_board(tx)?.is_some())
    });
    match ready {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            log::warn!("moving chats onto boards: {e}");
            return;
        }
    }
    let items = match db.snapshot(items_to_look_up) {
        Ok(i) => i,
        Err(e) => {
            log::warn!("moving chats onto boards: {e}");
            return;
        }
    };
    let learnt = if items.is_empty() {
        Some(ItemProjects::new())
    } else {
        tokio::task::spawn_blocking(move || learn_item_projects(&client, &items))
            .await
            .ok()
            .flatten()
    };
    let Some(learnt) = learnt else {
        log::info!("moving chats onto boards waits for the next sync: the board stopped answering");
        return;
    };
    match db.write(|tx| migrate_chats_onto_boards(tx, helm_core::now_ms(), &learnt)) {
        Ok(Some(record)) => {
            // The moved chats and the ones no longer nested, then every group they left or
            // joined, so emptied folder groups drop out of the sidebar at once.
            let chats = record
                .moved
                .iter()
                .map(|m| m.chat_id.clone())
                .chain(record.unnested.iter().map(|u| u.chat_id.clone()))
                .collect();
            let projects = record
                .moved
                .iter()
                .flat_map(|m| [m.from_id.as_str(), m.to_id.as_str()]);
            emit_moves(ctx, db, chats, projects);
        }
        Ok(None) => {}
        Err(e) => log::warn!("moving chats onto boards failed, and will be tried again: {e}"),
    }
}

// ---- the default-board setting ----------------------------------------------------------------

/// `default_board_get` / `default_board_set`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultBoardView {
    /// The board chats with no other home go to now; `null` before any board has synced.
    pub board_id: Option<String>,
    pub name: Option<String>,
    /// The setting as stored; `null` = the board named "Life Admin".
    pub chosen: Option<String>,
    /// Boards no picker offers.
    pub hidden_board_ids: Vec<String>,
}

pub fn default_board_view(tx: &Transaction<'_>) -> rusqlite::Result<DefaultBoardView> {
    let board_id = default_board(tx)?;
    let name = match &board_id {
        Some(b) => board_summary(tx, b)?.map(|s| s.name),
        None => None,
    };
    Ok(DefaultBoardView {
        board_id,
        name,
        chosen: chosen_default(tx),
        hidden_board_ids: hidden_boards(tx)?,
    })
}

/// Set (or with `None` clear) the default board. Only an open board can be chosen.
pub fn set_default_board(tx: &Transaction<'_>, board: Option<&str>) -> store::Res<()> {
    let value = match board {
        Some(b) => {
            if !is_open_board(tx, b)? {
                return Err(Refusal::BadArgs(
                    "Pick an open board; folders are where chats run, not where they are shown."
                        .into(),
                ));
            }
            json!(b)
        }
        None => Value::Null,
    };
    helm_core::prefs::set(tx, DEFAULT_BOARD, &value).map_err(|e| match e {
        helm_core::prefs::PrefError::Sql(e) => Refusal::Sql(e),
        other => Refusal::BadArgs(other.to_string()),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DefaultBoardSetArgs {
    board_id: Option<String>,
}

fn default_board_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    parse_args::<EmptyArgs>(args)?;
    let db = ctx.services.need::<Database>()?;
    to_value(&db.snapshot(default_board_view).map_err(sql)?)
}

fn default_board_set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: DefaultBoardSetArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let view = db.write(|tx| {
        set_default_board(tx, a.board_id.as_deref())?;
        Ok::<_, Refusal>(default_board_view(tx)?)
    })?;
    to_value(&view)
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

// ---- a new board project made on Agent Standup ------------------------------------------------

/// `board_project_create`: Helm's "New project".
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardProjectCreateArgs {
    pub title: String,
    /// One plain description; it becomes the project's body on the board.
    #[serde(default)]
    pub description: Option<String>,
    /// The folder its new chats start in (an absolute path to an existing folder).
    #[serde(default)]
    pub folder: Option<String>,
    /// The board area; defaults to the default board's area, else `general`.
    #[serde(default)]
    pub area: Option<String>,
}

/// Longest project title sent to the board, in characters.
pub const MAX_TITLE_CHARS: usize = 200;
/// Longest description sent to the board, in bytes.
pub const MAX_DESCRIPTION_BYTES: usize = 16 * 1024;
const POST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE: u64 = 1024 * 1024;

/// The one way a new project leaves Helm: a JSON POST. Tests replace it.
pub trait BoardPoster: Send + Sync {
    /// `(status, body)`, or why nothing came back.
    fn post(&self, url: &str, bearer: &str, body: &str) -> Result<(u16, String), String>;
}

/// The real poster: ureq, redirects off, statuses returned rather than raised, body capped.
pub struct HttpPoster {
    agent: ureq::Agent,
}

impl Default for HttpPoster {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(POST_TIMEOUT))
            .user_agent("helm-home")
            .build()
            .new_agent();
        Self { agent }
    }
}

impl BoardPoster for HttpPoster {
    fn post(&self, url: &str, bearer: &str, body: &str) -> Result<(u16, String), String> {
        let mut res = self
            .agent
            .post(url)
            .header("Authorization", &format!("Bearer {bearer}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(body)
            .map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let text = res
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE)
            .read_to_string()
            .unwrap_or_default();
        Ok((status, text))
    }
}

/// The checked title, description and folder of a new project.
fn check_new(a: &BoardProjectCreateArgs) -> Result<(String, String, Option<String>), ApiError> {
    let title = a.title.trim();
    if title.is_empty() {
        return Err(ApiError::bad_args("A project needs a title."));
    }
    if title.chars().count() > MAX_TITLE_CHARS || title.chars().any(char::is_control) {
        return Err(ApiError::bad_args(
            "A project title is one line of at most 200 characters.",
        ));
    }
    let description = a.description.as_deref().unwrap_or("").trim().to_owned();
    if description.len() > MAX_DESCRIPTION_BYTES {
        return Err(ApiError::bad_args(
            "A project description can be at most 16 KB.",
        ));
    }
    let folder = match a.folder.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => {
            let p = std::path::Path::new(f);
            if !p.is_absolute() {
                return Err(ApiError::bad_args("The folder must be an absolute path."));
            }
            if !p.is_dir() {
                return Err(ApiError::bad_args("That folder does not exist."));
            }
            Some(f.to_owned())
        }
        None => None,
    };
    Ok((title.to_owned(), description, folder))
}

/// The area a new project is filed under when none is given.
fn default_area(tx: &Transaction<'_>) -> rusqlite::Result<String> {
    let area: Option<String> = match default_board(tx)? {
        Some(b) => tx
            .query_row(
                "SELECT area FROM board_projects WHERE project_id = ?1",
                [&b],
                |r| r.get(0),
            )
            .optional()?
            .flatten(),
        None => None,
    };
    Ok(area
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| "general".to_owned()))
}

/// POST the project to the board; its board id. The token never appears in what is returned.
pub fn post_project(
    poster: &dyn BoardPoster,
    base: &str,
    token: &str,
    title: &str,
    description: &str,
    area: &str,
) -> Result<String, ApiError> {
    let body = json!({
        "title": title,
        "body": description,
        "area": area,
        "originType": "auto",
    })
    .to_string();
    let scrub = |s: &str| {
        if token.len() >= 4 {
            s.replace(token, "[token]")
        } else {
            s.to_owned()
        }
    };
    let (status, text) = poster
        .post(&format!("{base}/api/projects"), token, &body)
        .map_err(|e| {
            log::warn!("new board project not sent: {}", scrub(&e));
            ApiError::new(
                "unreachable",
                "Couldn't reach Agent Standup; the project was not made.",
            )
        })?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    match status {
        200..=299 => v
            .get("item")
            .unwrap_or(&v)
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(|id| id.trim().to_owned())
            .ok_or_else(|| {
                ApiError::internal(
                    "Agent Standup made the project but did not say its id; it appears after the next sync.",
                )
            }),
        401 | 403 => Err(ApiError::new(
            "refused",
            "Agent Standup refused the token; check it in Settings, Projects.",
        )),
        s => {
            let why = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(|m| scrub(&m.chars().take(300).collect::<String>()))
                .unwrap_or_else(|| format!("it answered {s}"));
            Err(ApiError::new(
                "refused",
                format!("Agent Standup did not make the project: {why}"),
            ))
        }
    }
}

/// Mirror a board project the board has just made, so it shows at once, and give it its folder
/// (made a folder project when needed). Returns the new board's project id.
pub fn record_new_board(
    tx: &Transaction<'_>,
    url: &str,
    board_project_id: &str,
    title: &str,
    area: &str,
    folder: Option<&str>,
    now: i64,
) -> store::Res<String> {
    let project = BoardProject {
        id: board_project_id.to_owned(),
        title: title.to_owned(),
        headline: None,
        area: Some(area.to_owned()),
        repo: None,
        priority: None,
        last_activity: None,
        total: None,
        finished: None,
    };
    // Not a complete read: nothing else is marked stale.
    crate::board::apply_sync(tx, url, std::slice::from_ref(&project), false, now)?;
    let id: String = tx.query_row(
        "SELECT project_id FROM board_projects WHERE board_url = ?1 AND board_project_id = ?2",
        [url, board_project_id],
        |r| r.get(0),
    )?;
    if let Some(f) = folder {
        let folder_project = helm_chats::store::upsert_folder_project(tx, f, now)?;
        tx.execute(
            "INSERT OR IGNORE INTO project_local(project_id) VALUES (?1)",
            [&id],
        )?;
        tx.execute(
            "UPDATE project_local SET primary_folder_project_id = ?2, revision = revision + 1
             WHERE project_id = ?1",
            params![id, folder_project],
        )?;
    }
    Ok(id)
}

async fn board_project_create(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: BoardProjectCreateArgs = parse_args(args)?;
    let (title, description, folder) = check_new(&a)?;
    let db = ctx.services.need::<Database>()?;
    let creds = ctx_credentials(&ctx)?;
    let (url, token) = match test_override() {
        Some((u, t)) => (u, t),
        None => (
            crate::board::stored_url(&db),
            crate::board::stored_token(&creds),
        ),
    };
    let (Some(token), false) = (token, url.trim().is_empty()) else {
        return Err(ApiError::new(
            "refused",
            StandupError::NotConfigured.to_string(),
        ));
    };
    let base = helm_standup::normalize_url(&url).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let area = match a.area.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_owned(),
        None => db.snapshot(default_area).map_err(sql)?,
    };
    let poster: std::sync::Arc<dyn BoardPoster> = match ctx.services.get::<PosterService>() {
        Some(p) => p.0.clone(),
        None => std::sync::Arc::new(HttpPoster::default()),
    };
    let (b, t, ti, d, ar) = (
        base.clone(),
        token,
        title.clone(),
        description,
        area.clone(),
    );
    let board_id = tokio::task::spawn_blocking(move || {
        post_project(poster.as_ref(), &b, t.expose(), &ti, &d, &ar)
    })
    .await
    .map_err(|_| ApiError::internal("The new project was not sent."))??;
    let node = db.write(|tx| {
        let id = record_new_board(
            tx,
            &base,
            &board_id,
            &title,
            &area,
            folder.as_deref(),
            helm_core::now_ms(),
        )?;
        store::get_project(tx, &id)
    })?;
    emit_project(&ctx, &node);
    to_value(&node)
}

/// A replacement [`BoardPoster`] (tests and checks register one as a service).
#[derive(Clone)]
pub struct PosterService(pub std::sync::Arc<dyn BoardPoster>);
