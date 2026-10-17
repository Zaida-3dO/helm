//! Reads and writes behind the project and sidebar ops. Every function takes the caller's
//! transaction, so an op is one atomic
//! write or one consistent snapshot.

use crate::schema::ARCHIVED_REFUSAL;
use crate::types::*;
use helm_chats::types::ChatStatus;
use rusqlite::{params, OptionalExtension, Row, Transaction};
use std::collections::{HashMap, HashSet};

/// Rows per sidebar page.
pub const PAGE_ROWS: usize = 500;
/// Most chat ids one `sidebar_rows` call may name.
pub const MAX_ROW_IDS: usize = 200;
/// Deepest nesting index for projects and for chats (three levels).
pub const MAX_DEPTH: usize = 2;
/// Name limit in bytes.
pub const MAX_NAME_BYTES: usize = 120;

#[derive(Debug)]
pub enum Refusal {
    BadArgs(String),
    NotFound(String),
    Conflict(String),
    Refused(String),
    Sql(rusqlite::Error),
}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Self {
        // The archived-project triggers surface as a plain refusal, not an internal error.
        if let rusqlite::Error::SqliteFailure(_, Some(msg)) = &e {
            if msg.contains(ARCHIVED_REFUSAL) {
                return Refusal::Refused(ARCHIVED_REFUSAL.into());
            }
        }
        Refusal::Sql(e)
    }
}

pub type Res<T> = Result<T, Refusal>;

fn bad(msg: &str) -> Refusal {
    Refusal::BadArgs(msg.into())
}

// ---- names ----------------------------------------------------------------------------------

/// The trimmed name, or why it is refused: 1–120 bytes, not `.`/`..`, no control characters and
/// none of `/ \:`.
pub fn validate_name(raw: &str) -> Res<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(bad("A project name cannot be blank."));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(bad("A project name can be at most 120 bytes."));
    }
    if name == "." || name == ".." {
        return Err(bad("A project cannot be named . or .."));
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return Err(bad(
            "A project name cannot contain / \\ : or control characters.",
        ));
    }
    Ok(name.to_owned())
}

/// A colour is one of the eight named colours or `#rrggbb`, stored lowercase.
pub fn validate_colour(raw: &str) -> Res<String> {
    const NAMED: [&str; 8] = [
        "red", "orange", "yellow", "green", "teal", "blue", "purple", "pink",
    ];
    let c = raw.trim().to_ascii_lowercase();
    let hex = c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit());
    if NAMED.contains(&c.as_str()) || hex {
        Ok(c)
    } else {
        Err(bad("A colour is a named colour or #rrggbb."))
    }
}

/// An icon is one emoji grapheme (at most 128 bytes, no ASCII letters or digits) or a library icon
/// `lib:<name>`. Full emoji validation and the 64-icon catalogue arrive with the icon dialog.
pub fn validate_icon(raw: &str) -> Res<String> {
    let s = raw.trim();
    if let Some(name) = s.strip_prefix("lib:") {
        if !name.is_empty()
            && name.len() <= 40
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Ok(s.to_owned());
        }
        return Err(bad("Unknown library icon."));
    }
    if s.is_empty()
        || s.len() > 128
        || s.chars()
            .any(|c| c.is_ascii_alphanumeric() || c.is_control())
    {
        return Err(bad("An icon is a single emoji or a library icon."));
    }
    Ok(s.to_owned())
}

// ---- projects -------------------------------------------------------------------------------

const PROJECT_SELECT: &str = "SELECT p.id, p.kind, p.name, p.path, p.color, p.created_at,
        COALESCE(l.pinned, 0), l.position, COALESCE(l.collapsed, 0), l.icon, COALESCE(l.archived, 0),
        l.title_override, l.primary_folder_project_id, COALESCE(l.revision, 0),
        pp.parent_id, pp.source,
        b.board_url, b.board_project_id, b.title, b.headline, b.area, b.repo, b.priority,
        b.last_activity, b.stale
     FROM projects p
     LEFT JOIN project_local l ON l.project_id = p.id
     LEFT JOIN project_parents pp ON pp.project_id = p.id
     LEFT JOIN board_projects b ON b.project_id = p.id";

/// Sidebar order of projects: pinned first, then position (unset sorts after set), newest
/// creation first, then id.
const PROJECT_ORDER: &str =
    "ORDER BY COALESCE(l.pinned, 0) DESC, l.position IS NULL, l.position, p.created_at DESC, p.id";

fn project_from_row(r: &Row<'_>, counts: &HashMap<String, i64>) -> rusqlite::Result<ProjectNode> {
    let id: String = r.get(0)?;
    let kind = ProjectKind::parse(&r.get::<_, String>(1)?);
    let name: String = r.get(2)?;
    let icon: Option<String> = r.get(9)?;
    let title_override: Option<String> = r.get(11)?;
    let board = match r.get::<_, Option<String>>(16)? {
        Some(url) => Some(BoardInfo {
            board_project_id: r.get(17)?,
            url,
            title: r.get(18)?,
            headline: r.get(19)?,
            area: r.get(20)?,
            repo: r.get(21)?,
            priority: r.get(22)?,
            last_activity: r.get(23)?,
            stale: r.get::<_, i64>(24)? != 0,
            edited_locally: title_override.is_some() || icon.is_some(),
        }),
        None => None,
    };
    let shown = match (&board, &title_override) {
        (_, Some(t)) => t.clone(),
        (Some(b), None) => b.title.clone(),
        _ => name,
    };
    Ok(ProjectNode {
        chat_count: counts.get(&id).copied().unwrap_or(0),
        id,
        kind,
        name: shown,
        path: r.get(3)?,
        colour: r.get(4)?,
        created_at: r.get(5)?,
        pinned: r.get::<_, i64>(6)? != 0,
        position: r.get(7)?,
        collapsed: r.get::<_, i64>(8)? != 0,
        icon,
        archived: r.get::<_, i64>(10)? != 0,
        primary_folder_project_id: r.get(12)?,
        revision: r.get(13)?,
        parent_id: r.get(14)?,
        parent_source: r.get::<_, Option<String>>(15)?.map(|s| {
            if s == "board" {
                ParentSource::Board
            } else {
                ParentSource::Local
            }
        }),
        board,
    })
}

/// Non-archived, non-side chats per shown group, in one grouped read.
fn chat_counts(tx: &Transaction<'_>) -> rusqlite::Result<HashMap<String, i64>> {
    let mut st = tx.prepare(
        "SELECT COALESCE(g.project_id, c.project_id), count(*) FROM chats c
         LEFT JOIN chat_groups g ON g.chat_id = c.id
         WHERE c.archived = 0 AND c.side_source_id IS NULL
         GROUP BY 1",
    )?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

pub fn list_projects(tx: &Transaction<'_>, include_archived: bool) -> Res<Vec<ProjectNode>> {
    let counts = chat_counts(tx)?;
    let filter = if include_archived {
        ""
    } else {
        "WHERE COALESCE(l.archived, 0) = 0"
    };
    let mut st = tx.prepare(&format!("{PROJECT_SELECT} {filter} {PROJECT_ORDER}"))?;
    let rows = st.query_map([], |r| project_from_row(r, &counts))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_project(tx: &Transaction<'_>, id: &str) -> Res<ProjectNode> {
    let counts = chat_counts(tx)?;
    tx.query_row(&format!("{PROJECT_SELECT} WHERE p.id = ?1"), [id], |r| {
        project_from_row(r, &counts)
    })
    .optional()?
    .ok_or_else(|| Refusal::NotFound("That project does not exist.".into()))
}

fn ensure_local(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO project_local(project_id) VALUES (?1)",
        [id],
    )?;
    Ok(())
}

fn is_archived(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row(
            "SELECT archived FROM project_local WHERE project_id = ?1",
            [id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0)
        != 0)
}

fn exists(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row("SELECT 1 FROM projects WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Validate a folder path: absolute and an existing directory.
fn validate_path(raw: &str) -> Res<String> {
    let p = raw.trim();
    let path = std::path::Path::new(p);
    if p.is_empty() || !path.is_absolute() {
        return Err(bad("The folder must be an absolute path."));
    }
    if !path.is_dir() {
        return Err(bad("That folder does not exist."));
    }
    Ok(p.to_owned())
}

/// Create a folder project. With a path it points at that folder (several projects
/// may share one); without one it is a local group with no folder of its own.
/// A new project goes to the top of the unpinned group.
pub fn create_project(tx: &Transaction<'_>, a: &ProjectCreateArgs, now: i64) -> Res<String> {
    let name = validate_name(&a.name)?;
    let path = match a.path.as_deref() {
        Some(p) if !p.trim().is_empty() => Some(validate_path(p)?),
        _ => None,
    };
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO projects(id, kind, name, path, created_at) VALUES (?1, 'folder', ?2, ?3, ?4)",
        params![id, name, path, now],
    )?;
    if let Some(parent) = a.parent_id.as_deref() {
        set_parent(tx, &id, Some(parent))?;
    }
    let parent = parent_of(tx, &id)?;
    let top: Option<i64> = tx.query_row(
        "SELECT min(l.position) FROM project_local l
         LEFT JOIN project_parents pp ON pp.project_id = l.project_id
         WHERE l.pinned = 0 AND l.project_id != ?1 AND pp.parent_id IS ?2",
        params![id, parent],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO project_local(project_id, position) VALUES (?1, ?2)",
        params![id, top.map(|t| t - 1).unwrap_or(0)],
    )?;
    Ok(id)
}

fn parent_of(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT parent_id FROM project_parents WHERE project_id = ?1",
        [id],
        |r| r.get(0),
    )
    .optional()
}

/// Ancestors of `id`, nearest first, stopping at a cycle (which nothing should ever write).
fn project_ancestors(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::from([id.to_owned()]);
    let mut cur = id.to_owned();
    while let Some(p) = parent_of(tx, &cur)? {
        if !seen.insert(p.clone()) || out.len() > 16 {
            break;
        }
        out.push(p.clone());
        cur = p;
    }
    Ok(out)
}

/// How many levels sit below `id` (0 = no children).
fn project_height(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<usize> {
    let mut st =
        tx.prepare_cached("SELECT project_id FROM project_parents WHERE parent_id = ?1")?;
    let mut level = vec![id.to_owned()];
    let mut seen = HashSet::from([id.to_owned()]);
    let mut height = 0;
    loop {
        let mut next = Vec::new();
        for p in &level {
            for c in st.query_map([p], |r| r.get::<_, String>(0))? {
                let c = c?;
                if seen.insert(c.clone()) {
                    next.push(c);
                }
            }
        }
        if next.is_empty() || height > 16 {
            return Ok(height);
        }
        height += 1;
        level = next;
    }
}

/// Nest `id` under `parent` (or move it to the top level), as a local edit. Refuses
/// self-nesting, cycles and anything that would put the project or its subtree past depth index 2.
pub fn set_parent(tx: &Transaction<'_>, id: &str, parent: Option<&str>) -> Res<()> {
    let Some(parent) = parent else {
        // Moving to the top level is itself a local edit: it wins over any board parent later.
        tx.execute("DELETE FROM project_parents WHERE project_id = ?1", [id])?;
        tx.execute(
            "INSERT OR IGNORE INTO project_top_level(project_id) VALUES (?1)",
            [id],
        )?;
        return Ok(());
    };
    if parent == id {
        return Err(bad("A project cannot be nested under itself."));
    }
    if !exists(tx, parent)? {
        return Err(Refusal::NotFound(
            "That parent project does not exist.".into(),
        ));
    }
    let ancestors = project_ancestors(tx, parent)?;
    if ancestors.iter().any(|a| a == id) {
        return Err(bad(
            "A project cannot be nested under one of its own subprojects.",
        ));
    }
    let depth = ancestors.len() + 1;
    if depth + project_height(tx, id)? > MAX_DEPTH {
        return Err(bad("Projects nest at most three levels deep."));
    }
    tx.execute("DELETE FROM project_top_level WHERE project_id = ?1", [id])?;
    tx.execute(
        "INSERT INTO project_parents(project_id, parent_id, source) VALUES (?1, ?2, 'local')
         ON CONFLICT(project_id) DO UPDATE SET parent_id = excluded.parent_id, source = 'local'",
        [id, parent],
    )?;
    Ok(())
}

/// The ids of `id`'s siblings in its pin group, in sidebar order (including `id`).
fn project_siblings(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Vec<String>> {
    let parent = parent_of(tx, id)?;
    let pinned = tx
        .query_row(
            "SELECT pinned FROM project_local WHERE project_id = ?1",
            [id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    let mut st = tx.prepare(&format!(
        "SELECT p.id FROM projects p
         LEFT JOIN project_local l ON l.project_id = p.id
         LEFT JOIN project_parents pp ON pp.project_id = p.id
         WHERE pp.parent_id IS ?1 AND COALESCE(l.pinned, 0) = ?2 AND COALESCE(l.archived, 0) = 0
         {PROJECT_ORDER}"
    ))?;
    let rows = st.query_map(params![parent, pinned], |r| r.get(0))?;
    rows.collect()
}

/// Move `id` before `before` (or last) within its sibling pin group. A move that changes nothing
/// writes nothing; otherwise every position in the group is rewritten 0..n.
fn reorder_project(tx: &Transaction<'_>, id: &str, before: Option<&str>) -> Res<bool> {
    let current = project_siblings(tx, id)?;
    let next = reorder(&current, id, before)
        .ok_or_else(|| bad("A project can only move within its own pin group."))?;
    if next == current {
        return Ok(false);
    }
    for (i, p) in next.iter().enumerate() {
        ensure_local(tx, p)?;
        tx.execute(
            "UPDATE project_local SET position = ?2 WHERE project_id = ?1",
            params![p, i as i64],
        )?;
    }
    Ok(true)
}

/// `items` with `id` moved before `before` (or to the end). `None` when `id` or `before` is not in
/// the list.
pub fn reorder(items: &[String], id: &str, before: Option<&str>) -> Option<Vec<String>> {
    if !items.iter().any(|x| x == id) {
        return None;
    }
    let mut out: Vec<String> = items.iter().filter(|x| *x != id).cloned().collect();
    match before {
        Some(b) if b == id => return Some(items.to_vec()),
        Some(b) => {
            let at = out.iter().position(|x| x == b)?;
            out.insert(at, id.to_owned());
        }
        None => out.push(id.to_owned()),
    }
    Some(out)
}

fn any_busy_chat(
    tx: &Transaction<'_>,
    project_id: &str,
    folder_only: bool,
) -> rusqlite::Result<bool> {
    let sql = if folder_only {
        "SELECT 1 FROM chats c WHERE c.project_id = ?1
           AND c.status IN ('starting', 'running', 'stopping') LIMIT 1"
    } else {
        "SELECT 1 FROM chats c LEFT JOIN chat_groups g ON g.chat_id = c.id
         WHERE COALESCE(g.project_id, c.project_id) = ?1
           AND c.status IN ('starting', 'running', 'stopping') LIMIT 1"
    };
    Ok(tx
        .query_row(sql, [project_id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Apply a `project_update` (compare-and-set on the project's revision). Returns the new revision.
pub fn update_project(tx: &Transaction<'_>, a: &ProjectUpdateArgs) -> Res<i64> {
    let node = get_project(tx, &a.project_id)?;
    if node.revision != a.revision {
        return Err(Refusal::Conflict(
            "This project changed meanwhile; reload and try again.".into(),
        ));
    }
    let id = a.project_id.as_str();
    ensure_local(tx, id)?;
    if let Some(name) = &a.name {
        let name = validate_name(name)?;
        match node.kind {
            // Local edits to a board project stay local: the board title is untouched.
            ProjectKind::Board => {
                let title = node.board.as_ref().map(|b| b.title.as_str());
                let over = if title == Some(name.as_str()) {
                    None
                } else {
                    Some(name)
                };
                tx.execute(
                    "UPDATE project_local SET title_override = ?2 WHERE project_id = ?1",
                    params![id, over],
                )?;
            }
            ProjectKind::Folder => {
                tx.execute(
                    "UPDATE projects SET name = ?2 WHERE id = ?1",
                    params![id, name],
                )?;
            }
        }
    }
    if let Some(icon) = &a.icon {
        let icon = icon.as_deref().map(validate_icon).transpose()?;
        tx.execute(
            "UPDATE project_local SET icon = ?2 WHERE project_id = ?1",
            params![id, icon],
        )?;
    }
    if let Some(colour) = &a.colour {
        let colour = colour.as_deref().map(validate_colour).transpose()?;
        tx.execute(
            "UPDATE projects SET color = ?2 WHERE id = ?1",
            params![id, colour],
        )?;
    }
    if let Some(c) = a.collapsed {
        tx.execute(
            "UPDATE project_local SET collapsed = ?2 WHERE project_id = ?1",
            params![id, c as i64],
        )?;
    }
    if let Some(parent) = &a.parent_id {
        set_parent(tx, id, parent.as_deref())?;
    }
    if let Some(p) = a.pinned {
        if p != node.pinned {
            // Pinning moves the project to the end of its new pin group.
            tx.execute(
                "UPDATE project_local SET pinned = ?2, position = NULL WHERE project_id = ?1",
                params![id, p as i64],
            )?;
        }
    }
    if let Some(before) = &a.before_project_id {
        reorder_project(tx, id, before.as_deref())?;
    }
    if let Some(path) = &a.path {
        if node.kind == ProjectKind::Board {
            return Err(bad(
                "A board project has no folder of its own; set its primary folder.",
            ));
        }
        let path = path.as_deref().map(validate_path).transpose()?;
        if path != node.path {
            // The run location changes, so every chat that runs here must be idle.
            if any_busy_chat(tx, id, true)? {
                return Err(Refusal::Refused(
                    "Stop all tasks in this project first.".into(),
                ));
            }
            if path.is_none() && has_chats_running_here(tx, id)? {
                return Err(bad("Chats run in this folder, so it cannot be cleared."));
            }
            tx.execute(
                "UPDATE projects SET path = ?2 WHERE id = ?1",
                params![id, path],
            )?;
        }
    }
    if let Some(pf) = &a.primary_folder_project_id {
        if let Some(pf) = pf.as_deref() {
            let k: Option<(String, Option<String>)> = tx
                .query_row("SELECT kind, path FROM projects WHERE id = ?1", [pf], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            match k {
                Some((kind, Some(_))) if kind == "folder" => {}
                _ => {
                    return Err(bad(
                        "The primary folder must be a folder project with a folder.",
                    ))
                }
            }
        }
        tx.execute(
            "UPDATE project_local SET primary_folder_project_id = ?2 WHERE project_id = ?1",
            params![id, pf],
        )?;
    }
    let rev: i64 = tx.query_row(
        "UPDATE project_local SET revision = revision + 1 WHERE project_id = ?1 RETURNING revision",
        [id],
        |r| r.get(0),
    )?;
    Ok(rev)
}

fn has_chats_running_here(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM chats WHERE project_id = ?1 LIMIT 1",
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Archive a project and every chat shown under it, recording exactly which chats this archived.
/// Refuses while any of them is busy. Returns the chat ids it archived.
pub fn archive_project(tx: &Transaction<'_>, id: &str) -> Res<Vec<String>> {
    if !exists(tx, id)? {
        return Err(Refusal::NotFound("That project does not exist.".into()));
    }
    if is_archived(tx, id)? {
        return Ok(Vec::new());
    }
    if any_busy_chat(tx, id, false)? {
        return Err(Refusal::Refused(
            "Stop all tasks in this project first.".into(),
        ));
    }
    let ids: Vec<String> = {
        let mut st = tx.prepare(
            "SELECT c.id FROM chats c LEFT JOIN chat_groups g ON g.chat_id = c.id
             WHERE COALESCE(g.project_id, c.project_id) = ?1 AND c.archived = 0",
        )?;
        let rows = st.query_map([id], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let now = helm_core::now_ms();
    for c in &ids {
        // The chats crate's own archive step (busy chats were refused above).
        if let Err(r) = helm_chats::archive::archive_in_tx(tx, c, now)? {
            return Err(Refusal::Refused(r.message));
        }
        tx.execute(
            "INSERT OR IGNORE INTO project_archive_members(project_id, chat_id) VALUES (?1, ?2)",
            [id, c],
        )?;
    }
    ensure_local(tx, id)?;
    tx.execute(
        "UPDATE project_local SET archived = 1, revision = revision + 1 WHERE project_id = ?1",
        [id],
    )?;
    Ok(ids)
}

/// Restore a project and exactly the chats its archive archived; chats archived on their own
/// beforehand stay archived. A chat that also belongs to another archived project stays archived.
pub fn restore_project(tx: &Transaction<'_>, id: &str) -> Res<Vec<String>> {
    if !exists(tx, id)? {
        return Err(Refusal::NotFound("That project does not exist.".into()));
    }
    ensure_local(tx, id)?;
    tx.execute(
        "UPDATE project_local SET archived = 0, revision = revision + 1 WHERE project_id = ?1",
        [id],
    )?;
    let ids: Vec<String> = {
        let mut st = tx.prepare(
            "SELECT m.chat_id FROM project_archive_members m
             JOIN chats c ON c.id = m.chat_id
             LEFT JOIN chat_groups g ON g.chat_id = c.id
             WHERE m.project_id = ?1 AND c.archived = 1
               AND NOT EXISTS (SELECT 1 FROM project_local l WHERE l.archived = 1
                               AND l.project_id IN (c.project_id, g.project_id))",
        )?;
        let rows = st.query_map([id], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let now = helm_core::now_ms();
    for c in &ids {
        if let Err(r) = helm_chats::archive::restore_in_tx(tx, c, now)? {
            return Err(Refusal::Refused(r.message));
        }
    }
    tx.execute(
        "DELETE FROM project_archive_members WHERE project_id = ?1",
        [id],
    )?;
    Ok(ids)
}

// ---- sidebar rows ---------------------------------------------------------------------------

const ROW_SELECT: &str = "SELECT c.id, c.title, COALESCE(g.project_id, c.project_id), c.project_id,
        COALESCE(p.path, ''), c.parent_chat_id, c.status, c.pinned, c.archived, c.position,
        c.created_at, c.updated_at,
        EXISTS (SELECT 1 FROM chats k WHERE k.parent_chat_id = c.id AND k.archived = c.archived
                AND k.side_source_id IS NULL)
     FROM chats c
     JOIN projects p ON p.id = c.project_id
     LEFT JOIN chat_groups g ON g.chat_id = c.id";

/// Chat order inside a group: pinned, then position (unset after set, newest
/// first), then id.
const CHAT_ORDER: &str = "c.pinned DESC, c.position IS NULL, c.position, c.created_at DESC, c.id";

struct RawRow {
    row: SidebarRow,
}

fn row_from(r: &Row<'_>, revision: i64) -> rusqlite::Result<RawRow> {
    let status = ChatStatus::parse(&r.get::<_, String>(6)?);
    let pinned = r.get::<_, i64>(7)? != 0;
    Ok(RawRow {
        row: SidebarRow {
            chat_id: r.get(0)?,
            title: r.get(1)?,
            group_project_id: r.get(2)?,
            folder_project_id: r.get(3)?,
            folder_path: r.get(4)?,
            depth: 0,
            parent_chat_id: r.get(5)?,
            has_children: r.get::<_, i64>(12)? != 0,
            status,
            // Unread and lingering come from the attention summary and the live thread;
            // the host fills what the database knows.
            attention: row_attention(status, false, false, pinned),
            pinned,
            archived: r.get::<_, i64>(8)? != 0,
            position: r.get(9)?,
            created_at: r.get(10)?,
            updated_at: r.get(11)?,
            revision,
        },
    })
}

struct ChatLink {
    parent: Option<String>,
    group: String,
    archived: bool,
}

fn chat_link(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<ChatLink>> {
    tx.prepare_cached(
        "SELECT c.parent_chat_id, COALESCE(g.project_id, c.project_id), c.archived
         FROM chats c LEFT JOIN chat_groups g ON g.chat_id = c.id WHERE c.id = ?1",
    )?
    .query_row([id], |r| {
        Ok(ChatLink {
            parent: r.get(0)?,
            group: r.get(1)?,
            archived: r.get::<_, i64>(2)? != 0,
        })
    })
    .optional()
}

/// Ancestors of a chat that it is drawn under: same group and same archived state, nearest first,
/// cut at a cycle and at `limit`.
fn chat_ancestors(tx: &Transaction<'_>, id: &str, limit: usize) -> rusqlite::Result<Vec<String>> {
    let Some(me) = chat_link(tx, id)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut seen = HashSet::from([id.to_owned()]);
    let mut next = me.parent.clone();
    while let Some(p) = next {
        if out.len() >= limit || !seen.insert(p.clone()) {
            break;
        }
        match chat_link(tx, &p)? {
            Some(l) if l.group == me.group && l.archived == me.archived => {
                out.push(p);
                next = l.parent;
            }
            _ => break,
        }
    }
    Ok(out)
}

fn chat_height(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<usize> {
    let mut st = tx.prepare_cached("SELECT id FROM chats WHERE parent_chat_id = ?1")?;
    let mut level = vec![id.to_owned()];
    let mut seen = HashSet::from([id.to_owned()]);
    let mut height = 0;
    loop {
        let mut next = Vec::new();
        for p in &level {
            for c in st.query_map([p], |r| r.get::<_, String>(0))? {
                let c = c?;
                if seen.insert(c.clone()) {
                    next.push(c);
                }
            }
        }
        if next.is_empty() || height > 16 {
            return Ok(height);
        }
        height += 1;
        level = next;
    }
}

fn finish(tx: &Transaction<'_>, raw: Vec<RawRow>) -> rusqlite::Result<Vec<SidebarRow>> {
    raw.into_iter()
        .map(|mut r| {
            r.row.depth = chat_ancestors(tx, &r.row.chat_id, MAX_DEPTH)?.len() as u8;
            Ok(r.row)
        })
        .collect()
}

/// One page of a sidebar view: at most [`PAGE_ROWS`] rows and the cursor of the next page.
pub fn sidebar_page(tx: &Transaction<'_>, a: &SidebarPageArgs, revision: i64) -> Res<SidebarPage> {
    let offset: usize = match a.cursor.as_deref() {
        None => 0,
        Some(c) => c.parse().map_err(|_| bad("Unknown sidebar cursor."))?,
    };
    let (filter, order) = match a.view {
        SidebarView::Projects => ("c.archived = 0", CHAT_ORDER),
        SidebarView::Archived => ("c.archived = 1", CHAT_ORDER),
        // Activity: newest activity first, across every non-archived project.
        SidebarView::Activity => (
            "c.archived = 0 AND NOT EXISTS (SELECT 1 FROM project_local l WHERE l.archived = 1
                 AND l.project_id IN (c.project_id, g.project_id))",
            "c.updated_at DESC, c.id",
        ),
    };
    let group = if a.group_id.is_some() {
        "AND COALESCE(g.project_id, c.project_id) = ?3"
    } else {
        "AND ?3 IS NULL"
    };
    let mut st = tx.prepare(&format!(
        "{ROW_SELECT} WHERE c.side_source_id IS NULL AND {filter} {group}
         ORDER BY {order} LIMIT ?1 OFFSET ?2"
    ))?;
    let raw: Vec<RawRow> = st
        .query_map(
            params![(PAGE_ROWS + 1) as i64, offset as i64, a.group_id],
            |r| row_from(r, revision),
        )?
        .collect::<rusqlite::Result<_>>()?;
    let more = raw.len() > PAGE_ROWS;
    let raw: Vec<RawRow> = raw.into_iter().take(PAGE_ROWS).collect();
    let next_cursor = more.then(|| (offset + PAGE_ROWS).to_string());
    Ok(SidebarPage {
        rows: finish(tx, raw)?,
        next_cursor,
    })
}

/// The rows for `ids`, in the order given; unknown ids are skipped.
pub fn sidebar_rows(tx: &Transaction<'_>, ids: &[String], revision: i64) -> Res<Vec<SidebarRow>> {
    if ids.len() > MAX_ROW_IDS {
        return Err(bad("At most 200 chat ids per call."));
    }
    let mut st = tx.prepare(&format!("{ROW_SELECT} WHERE c.id = ?1"))?;
    let mut raw = Vec::new();
    for id in ids {
        if let Some(r) = st.query_row([id], |r| row_from(r, revision)).optional()? {
            raw.push(r);
        }
    }
    Ok(finish(tx, raw)?)
}

// ---- chat organisation ----------------------------------------------------------------------

struct ChatBasics {
    folder: String,
    pinned: bool,
    side: bool,
}

fn chat_basics(tx: &Transaction<'_>, id: &str) -> Res<ChatBasics> {
    tx.query_row(
        "SELECT project_id, pinned, side_source_id IS NOT NULL FROM chats WHERE id = ?1",
        [id],
        |r| {
            Ok(ChatBasics {
                folder: r.get(0)?,
                pinned: r.get::<_, i64>(1)? != 0,
                side: r.get(2)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound("That chat does not exist.".into()))
}

/// Siblings of a chat for ordering: same group, same pin state, same parent, same archived state.
fn chat_siblings(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Vec<String>> {
    let mut st = tx.prepare(&format!(
        "SELECT c.id FROM chats c LEFT JOIN chat_groups g ON g.chat_id = c.id,
             (SELECT COALESCE(g2.project_id, c2.project_id) AS grp, c2.pinned AS pin,
                     c2.parent_chat_id AS par, c2.archived AS arc
              FROM chats c2 LEFT JOIN chat_groups g2 ON g2.chat_id = c2.id WHERE c2.id = ?1) me
         WHERE COALESCE(g.project_id, c.project_id) = me.grp AND c.pinned = me.pin
           AND c.parent_chat_id IS me.par AND c.archived = me.arc AND c.side_source_id IS NULL
         ORDER BY {CHAT_ORDER}"
    ))?;
    let rows = st.query_map([id], |r| r.get(0))?;
    rows.collect()
}

/// Apply `chat_organize`. Returns every chat id whose row
/// changed, the organised chat first.
pub fn organize(tx: &Transaction<'_>, a: &ChatOrganizeArgs) -> Res<Vec<String>> {
    let me = chat_basics(tx, &a.chat_id)?;
    if me.side {
        return Err(bad("A side chat is not shown in the sidebar."));
    }
    let id = a.chat_id.as_str();
    let mut changed = vec![id.to_owned()];
    if let Some(group) = a.group_project_id.as_deref() {
        if !exists(tx, group)? {
            return Err(Refusal::NotFound("That project does not exist.".into()));
        }
        if is_archived(tx, group)? {
            return Err(Refusal::Refused(ARCHIVED_REFUSAL.into()));
        }
        if group == me.folder {
            tx.execute("DELETE FROM chat_groups WHERE chat_id = ?1", [id])?;
        } else {
            tx.execute(
                "INSERT INTO chat_groups(chat_id, project_id) VALUES (?1, ?2)
                 ON CONFLICT(chat_id) DO UPDATE SET project_id = excluded.project_id",
                [id, group],
            )?;
        }
    }
    if let Some(parent) = &a.parent_chat_id {
        match parent.as_deref() {
            None => {
                tx.execute("UPDATE chats SET parent_chat_id = NULL WHERE id = ?1", [id])?;
            }
            Some(p) => {
                if p == id {
                    return Err(bad("A chat cannot be nested under itself."));
                }
                let pl = chat_link(tx, p)?
                    .ok_or_else(|| Refusal::NotFound("That parent chat does not exist.".into()))?;
                let ml = chat_link(tx, id)?.expect("checked above");
                if pl.group != ml.group || pl.archived != ml.archived {
                    return Err(bad(
                        "A chat can only nest under a chat in the same project.",
                    ));
                }
                let ancestors = chat_ancestors(tx, p, 16)?;
                if ancestors.iter().any(|x| x == id) {
                    return Err(bad(
                        "A chat cannot be nested under one of its own started chats.",
                    ));
                }
                if ancestors.len() + 1 + chat_height(tx, id)? > MAX_DEPTH {
                    return Err(bad("Chats nest at most three levels deep."));
                }
                tx.execute(
                    "UPDATE chats SET parent_chat_id = ?2 WHERE id = ?1",
                    [id, p],
                )?;
                changed.push(p.to_owned());
            }
        }
    }
    if let Some(p) = a.pinned {
        if p != me.pinned {
            tx.execute(
                "UPDATE chats SET pinned = ?2, position = NULL WHERE id = ?1",
                params![id, p as i64],
            )?;
        }
    }
    if a.before_chat_id.is_some() || a.to_end == Some(true) {
        let current = chat_siblings(tx, id)?;
        let next = reorder(&current, id, a.before_chat_id.as_deref()).ok_or_else(|| {
            bad("A chat can only be reordered among chats of its own project and pin state.")
        })?;
        if next != current {
            for (i, c) in next.iter().enumerate() {
                tx.execute(
                    "UPDATE chats SET position = ?2 WHERE id = ?1",
                    params![c, i as i64],
                )?;
                if c != id {
                    changed.push(c.clone());
                }
            }
        }
    }
    changed.dedup();
    Ok(changed)
}
