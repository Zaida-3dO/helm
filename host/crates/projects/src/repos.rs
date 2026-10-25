//! The repository list: the git repositories Helm knows, and every folder each one is checked
//! out in. It is bookkeeping only: saving an entry never clones, opens or contacts anything.
//!
//! An entry has a name (its id), an optional GitHub remote (host, owner, name; github.com is the
//! only host for now), its default branch and protected bases, a per-repository "agents may
//! create worktrees" switch and a revision that makes edits compare-and-set.
//!
//! The list fills itself from the folder projects that are real clones (`sync_from_projects`), so
//! a user who never opens a settings page still has one. Removing an entry remembers the folders
//! it listed so the next fill does not bring them back.

use crate::store::Refusal;
use helm_worktrees::{config, offlist, rules};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use std::path::Path;

pub const MAX_ID_BYTES: usize = 512;
pub const MAX_CHECKOUTS: usize = 100;
pub const MAX_LABEL_CHARS: usize = 128;
pub const MAX_PATH_CHARS: usize = 1024;
pub const MAX_LISTED: usize = 50;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS repositories (
             repo_id          TEXT PRIMARY KEY,
             remote_host      TEXT,
             remote_owner     TEXT,
             remote_name      TEXT,
             default_branch   TEXT,
             protected_bases  TEXT NOT NULL DEFAULT '[]',
             revision         INTEGER NOT NULL DEFAULT 0,
             created_at       INTEGER NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS repositories_remote
             ON repositories(lower(remote_host || '/' || remote_owner || '/' || remote_name))
             WHERE remote_name IS NOT NULL;
         CREATE TABLE IF NOT EXISTS repo_checkouts (
             id        TEXT PRIMARY KEY,
             repo_id   TEXT NOT NULL REFERENCES repositories(repo_id) ON DELETE CASCADE,
             path      TEXT NOT NULL,
             path_key  TEXT NOT NULL UNIQUE,
             label     TEXT
         );
         CREATE INDEX IF NOT EXISTS repo_checkouts_repo ON repo_checkouts(repo_id);
         CREATE TABLE IF NOT EXISTS repo_ignored (
             path_key  TEXT PRIMARY KEY
         );
         CREATE TABLE IF NOT EXISTS repo_worktree_settings (
             repo_id   TEXT PRIMARY KEY,
             allowed   INTEGER NOT NULL,
             saved_at  INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS worktree_config (
             id    INTEGER PRIMARY KEY CHECK (id = 1),
             root  TEXT
         );",
    )
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub id: String,
    pub kind: &'static str,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub repo_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    pub default_branch: Option<String>,
    pub protected_bases: Vec<String>,
    pub worktrees_allowed: bool,
    pub revision: i64,
    pub locations: Vec<Location>,
}

fn bad(m: impl Into<String>) -> Refusal {
    Refusal::BadArgs(m.into())
}

fn unsafe_text(s: &str) -> bool {
    s.chars().any(|c| {
        c.is_control()
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
    })
}

pub fn check_repo_id(id: &str) -> Result<(), Refusal> {
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        return Err(bad("A repository id is 1 to 512 bytes."));
    }
    if id != id.trim() {
        return Err(bad("A repository id has no spaces at either end."));
    }
    if unsafe_text(id) {
        return Err(bad("A repository id has a control or invisible character."));
    }
    Ok(())
}

fn remote_url(host: Option<&str>, owner: Option<&str>, name: Option<&str>) -> Option<String> {
    Some(format!("https://{}/{}/{}", host?, owner?, name?))
}

fn load_locations(tx: &Transaction<'_>, repo_id: &str) -> rusqlite::Result<Vec<Location>> {
    let mut st =
        tx.prepare("SELECT id, path, label FROM repo_checkouts WHERE repo_id = ?1 ORDER BY rowid")?;
    let rows = st.query_map([repo_id], |r| {
        Ok(Location {
            id: r.get(0)?,
            kind: "local",
            path: r.get(1)?,
            label: r.get(2)?,
        })
    })?;
    rows.collect()
}

pub fn worktrees_allowed(tx: &Transaction<'_>, repo_id: &str) -> rusqlite::Result<bool> {
    let v: Option<i64> = tx
        .query_row(
            "SELECT allowed FROM repo_worktree_settings WHERE repo_id = ?1",
            [repo_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.is_none_or(|v| v != 0))
}

pub fn get(tx: &Transaction<'_>, repo_id: &str) -> Result<Entry, Refusal> {
    let row = tx
        .query_row(
            "SELECT remote_host, remote_owner, remote_name, default_branch, protected_bases, revision
             FROM repositories WHERE repo_id = ?1",
            [repo_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Refusal::NotFound("There is no repository with that id.".into()))?;
    let bases: Vec<String> = serde_json::from_str(&row.4).unwrap_or_default();
    Ok(Entry {
        repo_id: repo_id.to_owned(),
        remote_url: remote_url(row.0.as_deref(), row.1.as_deref(), row.2.as_deref()),
        default_branch: row.3,
        protected_bases: bases,
        worktrees_allowed: worktrees_allowed(tx, repo_id)?,
        revision: row.5,
        locations: load_locations(tx, repo_id)?,
    })
}

pub fn all_ids(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    let mut st = tx.prepare("SELECT repo_id FROM repositories ORDER BY lower(repo_id), repo_id")?;
    let rows = st.query_map([], |r| r.get(0))?;
    rows.collect()
}

pub fn all(tx: &Transaction<'_>) -> Result<Vec<Entry>, Refusal> {
    all_ids(tx)?.iter().map(|id| get(tx, id)).collect()
}

/// The entry whose remote has this key (`github.com/owner/name`, any case).
fn by_remote(tx: &Transaction<'_>, key: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT repo_id FROM repositories
         WHERE remote_name IS NOT NULL
           AND lower(remote_host || '/' || remote_owner || '/' || remote_name) = ?1",
        [key],
        |r| r.get(0),
    )
    .optional()
}

/// Which entry lists this folder as a checkout (exactly).
pub fn owner_of_path(tx: &Transaction<'_>, path: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT repo_id FROM repo_checkouts WHERE path_key = ?1",
        [rules::path_key(path)],
        |r| r.get(0),
    )
    .optional()
}

/// The checkout (deepest) that is `folder` or encloses it, with its entry's id. Compares text
/// only; the disk is never touched.
pub fn enclosing(
    tx: &Transaction<'_>,
    folder: &str,
) -> rusqlite::Result<Option<(String, Location)>> {
    let mut best: Option<(String, Location)> = None;
    let mut st = tx.prepare("SELECT repo_id, id, path, label FROM repo_checkouts")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Location {
                id: r.get(1)?,
                kind: "local",
                path: r.get(2)?,
                label: r.get(3)?,
            },
        ))
    })?;
    for row in rows {
        let (repo, loc) = row?;
        if rules::is_within(folder, &loc.path)
            && best.as_ref().is_none_or(|(_, b)| {
                rules::path_key(&loc.path).len() > rules::path_key(&b.path).len()
            })
        {
            best = Some((repo, loc));
        }
    }
    Ok(best)
}

fn fresh_id(tx: &Transaction<'_>, wanted: &str) -> Result<String, Refusal> {
    let exists = |id: &str| -> rusqlite::Result<bool> {
        tx.query_row(
            "SELECT 1 FROM repositories WHERE repo_id = ?1",
            [id],
            |_| Ok(()),
        )
        .optional()
        .map(|o| o.is_some())
    };
    if !exists(wanted)? {
        return Ok(wanted.to_owned());
    }
    for n in 2..1000 {
        let id = format!("{wanted}-{n}");
        if !exists(&id)? {
            return Ok(id);
        }
    }
    Err(bad("Too many repositories share that name."))
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "repository".into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registered {
    Added,
    Created,
    AlreadyListed,
}

fn insert_checkout(
    tx: &Transaction<'_>,
    repo_id: &str,
    path: &str,
    label: Option<&str>,
) -> Result<String, Refusal> {
    let n: i64 = tx.query_row(
        "SELECT count(*) FROM repo_checkouts WHERE repo_id = ?1",
        [repo_id],
        |r| r.get(0),
    )?;
    if n as usize >= MAX_CHECKOUTS {
        return Err(bad("A repository lists at most 100 checkouts."));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let key = rules::path_key(path);
    tx.execute(
        "INSERT INTO repo_checkouts(id, repo_id, path, path_key, label) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, repo_id, path, key, label],
    )?;
    tx.execute("DELETE FROM repo_ignored WHERE path_key = ?1", [&key])?;
    Ok(id)
}

/// List the clone at `dir` (its `.git` must be a folder), joining the entry for its GitHub
/// remote or making a new one. A folder with no usable remote becomes an entry named after the
/// folder. Never touches an entry that already lists the folder.
pub fn add_clone(
    tx: &Transaction<'_>,
    dir: &Path,
    id_hint: Option<&str>,
    now: i64,
) -> Result<(String, Registered), Refusal> {
    let path = dir.to_string_lossy().into_owned();
    if let Some(owner) = owner_of_path(tx, &path)? {
        return Ok((owner, Registered::AlreadyListed));
    }
    let remote = config::origin_url(dir).and_then(|u| config::parse_remote(&u));
    if let Some(r) = &remote {
        if let Some(existing) = by_remote(tx, &r.key())? {
            insert_checkout(tx, &existing, &path, None)?;
            bump(tx, &existing)?;
            return Ok((existing, Registered::Added));
        }
    }
    let wanted = match (id_hint, &remote) {
        (Some(h), _) => {
            check_repo_id(h)?;
            h.to_owned()
        }
        (None, Some(r)) => format!("{}/{}/{}", r.host, r.owner, r.name),
        (None, None) => folder_name(dir),
    };
    let id = if id_hint.is_some() {
        // A named repository must be a new name: never bend an existing entry.
        let taken = tx
            .query_row(
                "SELECT 1 FROM repositories WHERE repo_id = ?1",
                [&wanted],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if taken {
            return Err(bad("A repository with that id is already listed."));
        }
        wanted
    } else {
        fresh_id(tx, &wanted)?
    };
    let branch = config::default_branch(dir);
    let bases = serde_json::to_string(&vec![branch.clone().unwrap_or_else(|| "main".into())])
        .unwrap_or_else(|_| "[]".into());
    tx.execute(
        "INSERT INTO repositories(repo_id, remote_host, remote_owner, remote_name, default_branch,
                                  protected_bases, revision, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
        params![
            id,
            remote.as_ref().map(|r| r.host.clone()),
            remote.as_ref().map(|r| r.owner.clone()),
            remote.as_ref().map(|r| r.name.clone()),
            branch,
            bases,
            now
        ],
    )?;
    insert_checkout(tx, &id, &path, None)?;
    Ok((id, Registered::Created))
}

fn bump(tx: &Transaction<'_>, repo_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE repositories SET revision = revision + 1 WHERE repo_id = ?1",
        [repo_id],
    )
    .map(|_| ())
}

/// Fill the list from the folder projects that are real clones. Returns how many entries or
/// checkouts were added. Folders the user removed from the list stay out.
pub fn sync_from_projects(tx: &Transaction<'_>, now: i64) -> Result<usize, Refusal> {
    let mut st = tx.prepare(
        "SELECT p.path FROM projects p
         LEFT JOIN project_local l ON l.project_id = p.id
         WHERE p.kind = 'folder' AND p.path IS NOT NULL AND COALESCE(l.archived, 0) = 0
         ORDER BY p.created_at, p.id",
    )?;
    let paths: Vec<String> = st
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(st);
    let mut added = 0;
    for p in paths {
        let dir = Path::new(&p);
        let key = rules::path_key(&p);
        if !config::is_clone_root(dir) {
            continue;
        }
        if owner_of_path(tx, &p)?.is_some()
            || tx
                .query_row(
                    "SELECT 1 FROM repo_ignored WHERE path_key = ?1",
                    [&key],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
        {
            continue;
        }
        if add_clone(tx, dir, None, now).is_ok() {
            added += 1;
        }
    }
    Ok(added)
}

/// What a chat's tool may do to an entry. `expected_revision` makes it compare-and-set.
#[derive(Debug, Default, Clone)]
pub struct Patch {
    pub repo_id: String,
    pub expected_revision: i64,
    pub remote_url: Option<String>,
    pub add_locations: Vec<(String, Option<String>)>,
    pub remove_location_ids: Vec<String>,
    pub remove: bool,
    pub protected_bases: Option<Vec<String>>,
}

fn check_location(path: &str, label: Option<&str>) -> Result<(), Refusal> {
    if path.is_empty() || path.chars().count() > MAX_PATH_CHARS || unsafe_text(path) {
        return Err(bad(
            "A checkout path is 1 to 1,024 characters with no control characters.",
        ));
    }
    let p = Path::new(path);
    if !p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(bad("A checkout path is absolute, with no . or .. parts."));
    }
    if !p.is_dir() {
        return Err(bad("A checkout path must be an existing folder."));
    }
    if let Some(l) = label {
        if l.chars().count() > MAX_LABEL_CHARS || unsafe_text(l) {
            return Err(bad(
                "A checkout label is at most 128 characters with no control characters.",
            ));
        }
    }
    Ok(())
}

/// Apply a patch in the caller's transaction. A repository with worktrees off keeps its
/// checkouts and its place in the list: only its remote URL can change.
pub fn update(tx: &Transaction<'_>, patch: &Patch) -> Result<Entry, Refusal> {
    let cur = get(tx, &patch.repo_id)?;
    if cur.revision != patch.expected_revision {
        return Err(Refusal::Conflict(
            "This repository changed since you looked at it; read it again first.".into(),
        ));
    }
    let touches_places =
        patch.remove || !patch.add_locations.is_empty() || !patch.remove_location_ids.is_empty();
    if touches_places && !cur.worktrees_allowed {
        return Err(Refusal::Refused(
            "Worktrees are switched off for this repository, so its checkouts are locked; ask the user to change that in Settings.".into(),
        ));
    }
    if patch.remove {
        if patch.remote_url.is_some()
            || !patch.add_locations.is_empty()
            || !patch.remove_location_ids.is_empty()
        {
            return Err(bad("Removing a repository takes only its id and revision."));
        }
        for l in &cur.locations {
            tx.execute(
                "INSERT OR IGNORE INTO repo_ignored(path_key) VALUES (?1)",
                [rules::path_key(&l.path)],
            )?;
        }
        tx.execute(
            "DELETE FROM repo_checkouts WHERE repo_id = ?1",
            [&patch.repo_id],
        )?;
        tx.execute(
            "DELETE FROM repositories WHERE repo_id = ?1",
            [&patch.repo_id],
        )?;
        return Ok(cur);
    }
    if patch.add_locations.len() > 4 {
        return Err(bad("Add at most 4 checkouts at a time."));
    }
    if let Some(url) = &patch.remote_url {
        if unsafe_text(url) {
            return Err(bad("A remote URL has no control or invisible characters."));
        }
        let r = config::parse_remote(url).ok_or_else(|| {
            bad("The remote must be a GitHub address like https://github.com/owner/name (no credentials).")
        })?;
        if let Some(other) = by_remote(tx, &r.key())? {
            if other != patch.repo_id {
                return Err(Refusal::Conflict(format!(
                    "{other} already uses that remote."
                )));
            }
        }
        tx.execute(
            "UPDATE repositories SET remote_host = ?2, remote_owner = ?3, remote_name = ?4 WHERE repo_id = ?1",
            params![patch.repo_id, r.host, r.owner, r.name],
        )?;
    }
    for id in &patch.remove_location_ids {
        let loc = cur.locations.iter().find(|l| &l.id == id).ok_or_else(|| {
            Refusal::NotFound("That repository has no checkout with that id.".into())
        })?;
        tx.execute(
            "INSERT OR IGNORE INTO repo_ignored(path_key) VALUES (?1)",
            [rules::path_key(&loc.path)],
        )?;
        tx.execute("DELETE FROM repo_checkouts WHERE id = ?1", [id])?;
    }
    for (path, label) in &patch.add_locations {
        check_location(path, label.as_deref())?;
        if let Some(owner) = owner_of_path(tx, path)? {
            return Err(Refusal::Conflict(format!(
                "That folder is already listed under {owner}."
            )));
        }
        insert_checkout(tx, &patch.repo_id, path, label.as_deref())?;
    }
    if let Some(bases) = &patch.protected_bases {
        if bases.len() > 20
            || bases.iter().any(|b| {
                b.is_empty()
                    || b.chars().count() > 200
                    || unsafe_text(b)
                    || b.contains(char::is_whitespace)
            })
        {
            return Err(bad("Protected bases are at most 20 branch names."));
        }
        tx.execute(
            "UPDATE repositories SET protected_bases = ?2 WHERE repo_id = ?1",
            params![
                patch.repo_id,
                serde_json::to_string(bases).unwrap_or_else(|_| "[]".into())
            ],
        )?;
    }
    bump(tx, &patch.repo_id)?;
    get(tx, &patch.repo_id)
}

/// The user's switch. Kept apart from the entry so it survives edits and a remove and re-add.
pub fn set_worktrees(
    tx: &Transaction<'_>,
    repo_id: &str,
    allowed: bool,
    now: i64,
) -> Result<Entry, Refusal> {
    get(tx, repo_id)?;
    tx.execute(
        "INSERT INTO repo_worktree_settings(repo_id, allowed, saved_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(repo_id) DO UPDATE SET allowed = ?2, saved_at = ?3",
        params![repo_id, allowed as i64, now],
    )?;
    bump(tx, repo_id)?;
    get(tx, repo_id)
}

/// The result of an agent registering the folder it works in.
pub fn register_checkout(
    tx: &Transaction<'_>,
    folder: &Path,
    id_hint: Option<&str>,
    now: i64,
) -> Result<(String, Registered), Refusal> {
    let path = folder.to_string_lossy().into_owned();
    if !config::is_clone_root(folder) {
        return Err(Refusal::Refused(
            "This folder is not the root of a clone (a linked worktree or a subfolder cannot be registered).".into(),
        ));
    }
    if let Some((repo, _)) = enclosing(tx, &path)? {
        return Ok((repo, Registered::AlreadyListed));
    }
    let remote = config::origin_url(folder).and_then(|u| config::parse_remote(&u));
    if let Some(r) = &remote {
        if let Some(existing) = by_remote(tx, &r.key())? {
            if !worktrees_allowed(tx, &existing)? {
                return Err(Refusal::Refused(format!(
                    "Worktrees are switched off for {existing}, so its checkouts cannot be changed from a chat."
                )));
            }
        }
    } else if id_hint.is_none() {
        // No usable remote: the folder name is the id, and it must be free.
        let name = folder_name(folder);
        if tx
            .query_row(
                "SELECT 1 FROM repositories WHERE repo_id = ?1",
                [&name],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(Refusal::Refused(format!(
                "This folder has no GitHub remote and the name {name} is already a repository; pass a repository id."
            )));
        }
    }
    tx.execute(
        "DELETE FROM repo_ignored WHERE path_key = ?1",
        [rules::path_key(&path)],
    )?;
    add_clone(tx, folder, id_hint, now)
}

/// The Off list lines: every checkout of every repository whose worktrees are off, resolved on
/// disk when possible.
pub fn off_lines(tx: &Transaction<'_>) -> rusqlite::Result<Vec<offlist::Line>> {
    let mut st = tx.prepare(
        "SELECT c.repo_id, c.path FROM repo_checkouts c
         JOIN repo_worktree_settings s ON s.repo_id = c.repo_id AND s.allowed = 0
         ORDER BY c.repo_id, c.rowid",
    )?;
    let rows = st.query_map([], |r| {
        Ok(offlist::Line {
            repo_id: r.get(0)?,
            path: offlist::plain_canonical(Path::new(&r.get::<_, String>(1)?)),
        })
    })?;
    rows.collect()
}

pub fn root(tx: &Transaction<'_>) -> rusqlite::Result<std::path::PathBuf> {
    let v: Option<Option<String>> = tx
        .query_row("SELECT root FROM worktree_config WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(v.flatten()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(rules::default_root))
}

/// Set the root (`None` goes back to the default). A root that cannot hold a worktree under the
/// length budget is refused.
pub fn set_root(
    tx: &Transaction<'_>,
    new_root: Option<&str>,
) -> Result<std::path::PathBuf, Refusal> {
    if let Some(r) = new_root {
        let p = Path::new(r);
        rules::check_root(p).map_err(|e| bad(e.to_string()))?;
        rules::plan_path(p, "repo", "a-name").map_err(|e| bad(e.to_string()))?;
    }
    tx.execute(
        "INSERT INTO worktree_config(id, root) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET root = ?1",
        [new_root],
    )?;
    Ok(root(tx)?)
}
