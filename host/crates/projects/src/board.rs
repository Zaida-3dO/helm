//! The Agent Standup connection, the read-only sync and the mirror.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `board_connection_get` | blocking | `{}` → [`BoardConnection`] |
//! | `board_connection_save` | maintenance | [`BoardConnectionSaveArgs`] → [`BoardConnection`] |
//! | `board_sync` | async | `{}` → [`BoardSyncState`] |
//!
//! The address lives in `board_connection`; the token in the credential store, write-only
//! from the UI, never logged and never returned. The board is never a dependency: nothing but
//! `board_sync` waits on it, and a failed sync leaves the mirror as it was with a plain-sentence
//! error. The UI calls `board_sync` on launch, on Refresh, every 10 minutes while visible and on
//! focus after 2 minutes. A board project missing from a complete sync is marked stale, never
//! deleted; its chats stay where they are.
//!
//! Debug builds only: `HELM_TEST_STANDUP_URL` and `HELM_TEST_STANDUP_TOKEN` override the stored
//! connection, so checks can point the sync at `fakes/board`.

use crate::ops::{parse_args, to_value};
use crate::types::*;
use helm_core::credentials::CredentialStore;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, AppEvent, EventSink, OpClass, OpCtx, OpRegistry, Services};
use helm_platform::Platform;
use helm_standup::{BoardLink, BoardProject, Client, StandupError, Token};
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// No built-in board address: the user enters theirs in Settings → Projects.
pub const DEFAULT_URL: &str = "";
const CRED_NAME: &str = "agent-standup";
const CRED_IDENTITY: &str = "agent-standup-token";
pub const TEST_URL_ENV: &str = "HELM_TEST_STANDUP_URL";
pub const TEST_TOKEN_ENV: &str = "HELM_TEST_STANDUP_TOKEN";

static SYNCING: AtomicBool = AtomicBool::new(false);

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("board_connection_get", OpClass::Blocking, connection_get)
        .blocking(
            "board_connection_save",
            OpClass::Maintenance,
            connection_save,
        )
        .async_op("board_sync", sync);
}

/// Called once at startup (registrations::services): the shared [`BoardLink`] that the search crate's
/// search attaches to, filled from the stored connection without reaching the board, and the
/// project gate the chats crate asks before a new chat or a single-chat restore.
pub fn register_services(services: &mut Services, _events: &Arc<dyn EventSink>) {
    let link = Arc::new(BoardLink::default());
    if let (Some(db), Some(profile), Some(platform)) = (
        services.get::<Database>(),
        services.get::<Profile>(),
        services.get::<Platform>(),
    ) {
        let creds = credentials(&profile, &platform);
        link.set(resolve(&db, &creds).ok());
    }
    services.insert(link);
    services.insert(Arc::new(helm_chats::seams::ProjectGates(Arc::new(Gate))));
}

/// An archived project refuses new chats and single-chat restores.
struct Gate;

impl helm_chats::seams::ProjectGate for Gate {
    fn project_archived(&self, tx: &Transaction<'_>, project_id: &str) -> rusqlite::Result<bool> {
        Ok(tx
            .query_row(
                "SELECT archived FROM project_local WHERE project_id = ?1",
                [project_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            != 0)
    }
}

pub(crate) fn credentials(profile: &Profile, platform: &Platform) -> CredentialStore {
    CredentialStore::new(
        &profile.path("credentials"),
        &profile.hash,
        platform.keychain.clone(),
    )
}

pub(crate) fn ctx_credentials(ctx: &OpCtx) -> Result<CredentialStore, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let platform = ctx.services.need::<Platform>()?;
    Ok(credentials(&profile, &platform))
}

fn stored_url(db: &Database) -> String {
    db.snapshot(|tx| {
        tx.query_row("SELECT url FROM board_connection WHERE id = 1", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()
    })
    .ok()
    .flatten()
    .unwrap_or_else(|| DEFAULT_URL.to_owned())
}

fn test_override() -> Option<(String, Option<Token>)> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let url = std::env::var(TEST_URL_ENV)
        .ok()
        .filter(|u| !u.trim().is_empty())?;
    let token = std::env::var(TEST_TOKEN_ENV).ok().and_then(Token::new);
    Some((url, token))
}

fn stored_token(creds: &CredentialStore) -> Option<Token> {
    let bytes = creds.read(CRED_NAME, CRED_IDENTITY).ok()?;
    Token::new(String::from_utf8(bytes).ok()?)
}

/// The client for the current connection, or `NotConfigured`.
pub(crate) fn resolve(db: &Database, creds: &CredentialStore) -> Result<Client, StandupError> {
    let (url, token) = match test_override() {
        Some((u, t)) => (u, t),
        None => (stored_url(db), stored_token(creds)),
    };
    if url.trim().is_empty() {
        return Err(StandupError::NotConfigured);
    }
    let token = token.ok_or(StandupError::NotConfigured)?;
    Client::new(&url, token)
}

fn connection(db: &Database, creds: &CredentialStore) -> BoardConnection {
    match test_override() {
        Some((url, token)) => BoardConnection {
            url,
            has_token: token.is_some(),
        },
        None => BoardConnection {
            url: stored_url(db),
            has_token: stored_token(creds).is_some(),
        },
    }
}

fn connection_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    to_value(&connection(&db, &ctx_credentials(ctx)?))
}

fn connection_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: BoardConnectionSaveArgs = parse_args(args)?;
    let url = if a.url.trim().is_empty() {
        String::new()
    } else {
        helm_standup::normalize_url(&a.url).map_err(|e| ApiError::bad_args(e.to_string()))?
    };
    let db = ctx.services.need::<Database>()?;
    let creds = ctx_credentials(ctx)?;
    match a.token.as_deref().map(str::trim) {
        Some("") => creds
            .forget(CRED_NAME)
            .map_err(|_| ApiError::internal("The token could not be removed."))?,
        // The store's error is never formatted: it must not risk carrying the input.
        Some(t) => creds
            .write(CRED_NAME, CRED_IDENTITY, t.as_bytes())
            .map_err(|_| {
                ApiError::internal("The token could not be saved in the credential store.")
            })?,
        None => {}
    }
    db.write(|tx| {
        tx.execute(
            "INSERT INTO board_connection(id, url) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET url = excluded.url",
            [&url],
        )
    })
    .map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some(link) = ctx.services.get::<BoardLink>() {
        link.set(resolve(&db, &creds).ok());
    }
    to_value(&connection(&db, &creds))
}

/// The sync state; `project_count` counts live board projects of the current address only.
pub fn read_state(
    tx: &Transaction<'_>,
    configured: bool,
    url: Option<&str>,
) -> rusqlite::Result<BoardSyncState> {
    let row: Option<(Option<i64>, Option<String>)> = tx
        .query_row(
            "SELECT last_sync_at, error FROM board_sync_state WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM board_projects WHERE stale = 0 AND board_url IS ?1",
        [url],
        |r| r.get(0),
    )?;
    let (last_sync_at, error) = row.unwrap_or((None, None));
    Ok(BoardSyncState {
        configured,
        syncing: SYNCING.load(Ordering::SeqCst),
        last_sync_at,
        error,
        project_count: count,
    })
}

fn emit_state(ctx: &OpCtx, s: &BoardSyncState) {
    if let Ok(v) = serde_json::to_value(s) {
        ctx.events
            .emit(AppEvent::new("board_sync").with("state", v));
    }
}

/// Record a sync's outcome. A failed sync keeps the last good time, so "last sync" stays true.
pub fn record_result(tx: &Transaction<'_>, now: i64, error: Option<&str>) -> rusqlite::Result<()> {
    let ok = error.is_none();
    tx.execute(
        "INSERT INTO board_sync_state(id, last_sync_at, error) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET
             last_sync_at = CASE WHEN ?3 THEN excluded.last_sync_at ELSE board_sync_state.last_sync_at END,
             error = excluded.error",
        params![ok.then_some(now), error, ok],
    )?;
    Ok(())
}

fn internal(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

async fn sync(ctx: OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let creds = ctx_credentials(&ctx)?;
    let client = match resolve(&db, &creds) {
        Ok(c) => c,
        Err(e) => {
            let msg = e.to_string();
            db.write(|tx| record_result(tx, helm_core::now_ms(), Some(&msg)))
                .map_err(internal)?;
            let s = db
                .snapshot(|tx| read_state(tx, false, None))
                .map_err(internal)?;
            emit_state(&ctx, &s);
            return to_value(&s);
        }
    };
    if let Some(link) = ctx.services.get::<BoardLink>() {
        link.set(Some(client.clone()));
    }
    let base = client.base().to_owned();
    // One sync at a time; a second caller gets the current state with `syncing: true`.
    if SYNCING.swap(true, Ordering::SeqCst) {
        return to_value(
            &db.snapshot(|tx| read_state(tx, true, Some(&base)))
                .map_err(internal)?,
        );
    }
    // Cleared on every exit, including a cancelled or panicking sync.
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            SYNCING.store(false, Ordering::SeqCst);
        }
    }
    let clear = Clear;
    if let Ok(s) = db.snapshot(|tx| read_state(tx, true, Some(&base))) {
        emit_state(&ctx, &s);
    }
    let fetched = tokio::task::spawn_blocking(move || client.list_projects())
        .await
        .unwrap_or_else(|_| Err(StandupError::BadResponse("the sync stopped".into())));
    let now = helm_core::now_ms();
    let outcome = db.write(|tx| match &fetched {
        Ok((projects, complete)) => {
            apply_sync(tx, &base, projects, *complete, now)?;
            record_result(tx, now, None)
        }
        Err(e) => {
            log::warn!("Agent Standup sync failed: {e}");
            record_result(tx, now, Some(&e.to_string()))
        }
    });
    drop(clear);
    outcome.map_err(internal)?;
    let s = db
        .snapshot(|tx| read_state(tx, true, Some(&base)))
        .map_err(internal)?;
    emit_state(&ctx, &s);
    to_value(&s)
}

/// Upsert one complete sync's projects into the mirror.
///
/// - A new board project becomes a `board` row in the core `projects` table plus its mirror row,
///   at the top level (Agent Standup projects have no parents; nesting is local only).
/// - A known one takes the board's title and fields, unless the incoming `lastActivity` is older
///   than the stored one (an out-of-order read), which is ignored.
/// - Whether the project's work is all finished is written on every sync, older read or not:
///   finishing or reopening work need not move `lastActivity` forward, and a flag held back by
///   that rule could hide a live project (or keep showing a done one) until the next edit.
/// - Local state (rename, icon, colour, pin, order, collapse, nesting) is never touched.
/// - Every project of this board address missing from a complete sync is marked stale, never
///   deleted. A sync cut short at the page cap marks nothing stale.
///
/// Returns how many projects the sync named.
pub fn apply_sync(
    tx: &Transaction<'_>,
    url: &str,
    projects: &[BoardProject],
    complete: bool,
    now: i64,
) -> rusqlite::Result<usize> {
    let mut seen = std::collections::HashSet::new();
    for (i, p) in projects.iter().enumerate() {
        seen.insert(p.id.as_str());
        let known: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT project_id, last_activity FROM board_projects
                 WHERE board_url = ?1 AND board_project_id = ?2",
                [url, &p.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match known {
            Some((pid, stored)) => {
                let older = matches!((&stored, &p.last_activity), (Some(s), Some(n)) if n < s);
                if older {
                    tx.execute(
                        "UPDATE board_projects SET stale = 0, synced_at = ?2, completed = ?3
                         WHERE project_id = ?1",
                        params![pid, now, p.completed()],
                    )?;
                    continue;
                }
                tx.execute(
                    "UPDATE board_projects SET title = ?2, headline = ?3, area = ?4, repo = ?5,
                         priority = ?6, last_activity = ?7, stale = 0, synced_at = ?8,
                         completed = ?9
                     WHERE project_id = ?1",
                    params![
                        pid,
                        p.title,
                        p.headline,
                        p.area,
                        p.repo,
                        p.priority,
                        p.last_activity,
                        now,
                        p.completed()
                    ],
                )?;
                tx.execute(
                    "UPDATE projects SET name = ?2 WHERE id = ?1",
                    params![pid, p.title],
                )?;
            }
            None => {
                let pid = uuid::Uuid::new_v4().to_string();
                // Newer sorts first, so the board's own order is kept among new projects.
                tx.execute(
                    "INSERT INTO projects(id, kind, name, created_at) VALUES (?1, 'board', ?2, ?3)",
                    params![pid, p.title, now - i as i64],
                )?;
                tx.execute(
                    "INSERT INTO board_projects(project_id, board_url, board_project_id, title,
                         headline, area, repo, priority, last_activity, stale, synced_at, completed)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11)",
                    params![
                        pid,
                        url,
                        p.id,
                        p.title,
                        p.headline,
                        p.area,
                        p.repo,
                        p.priority,
                        p.last_activity,
                        now,
                        p.completed()
                    ],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO project_local(project_id) VALUES (?1)",
                    [&pid],
                )?;
            }
        }
    }
    if !complete {
        return Ok(projects.len());
    }
    let mut st = tx.prepare(
        "SELECT project_id, board_project_id FROM board_projects WHERE board_url = ?1 AND stale = 0",
    )?;
    let gone: Vec<String> = st
        .query_map([url], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .filter(|(_, b)| !seen.contains(b.as_str()))
        .map(|(p, _)| p)
        .collect();
    for pid in gone {
        tx.execute(
            "UPDATE board_projects SET stale = 1 WHERE project_id = ?1",
            [&pid],
        )?;
    }
    Ok(projects.len())
}
