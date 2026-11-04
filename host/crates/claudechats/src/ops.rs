//! The four ops.

use crate::fork::{self, valid_session_id};
use crate::parse;
use crate::projects_root;
use crate::store;
use crate::types::*;
use helm_chats::seams::ProjectGates;
use helm_chats::session_log::ClaudeBinding;
use helm_chats::types::ChatEvent;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Records listed and searched at once.
const LIST_LIMIT: i64 = 200;
/// How long a search waits on re-reading changed records; what is left is read by the next one.
const SEARCH_REFRESH_BUDGET: Duration = Duration::from_secs(3);

/// One refresh at a time for the whole app.
static GATE: store::Gate = store::Gate::new();

/// Session ids of the chats Helm ran itself. The CLI writes their records in the same folder as
/// any other chat's, but they are already in Helm's own chat list, so they are not offered here.
/// (A chat's `fork_of` is not listed: it names the outside record the chat carries on from, which
/// stays a Claude Code chat.)
fn helm_sessions(ctx: &OpCtx) -> HashSet<String> {
    let Some(profile) = ctx.services.get::<Profile>() else {
        return HashSet::new();
    };
    let Ok(rd) = std::fs::read_dir(profile.dir.join("sessions")) else {
        return HashSet::new();
    };
    let sessions = profile.dir.join("sessions");
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let chat = name.strip_suffix(".claude.json")?.to_owned();
            ClaudeBinding::load(&sessions, &chat)?.native_session_id
        })
        .collect()
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("claude_chat_list", OpClass::Heavy, list_op)
        .blocking("claude_chat_search", OpClass::Heavy, search_op)
        .blocking("claude_chat_read", OpClass::Heavy, read_op)
        .blocking("claude_chat_fork", OpClass::Maintenance, fork_op);
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn parse_args<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|e| ApiError::bad_args(e.to_string()))
}

pub(crate) fn list_op(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let (chats, source_missing) = match projects_root() {
        None => (Vec::new(), true),
        Some(root) => {
            let own = helm_sessions(ctx);
            let missing = GATE
                .run(true, helm_core::now_ms(), || {
                    store::refresh(&db, &root, &own, None)
                })
                .map_err(sql)?
                .is_some_and(|r| r.source_missing);
            let mut chats = store::list(&db, LIST_LIMIT).map_err(sql)?;
            chats.retain(|c| !own.contains(&c.session_id));
            (chats, missing)
        }
    };
    to_json(&ChatList {
        chats,
        source_missing,
    })
}

pub(crate) fn search_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SearchArgs = parse_args(args)?;
    let query = a.query.trim();
    if query.len() > MAX_QUERY_BYTES {
        return Err(ApiError::bad_args("The search is too long."));
    }
    if query.is_empty() {
        return to_json(&SearchResults { hits: Vec::new() });
    }
    let db = ctx.services.need::<Database>()?;
    match projects_root() {
        None => to_json(&SearchResults { hits: Vec::new() }),
        Some(root) => {
            let own = helm_sessions(ctx);
            GATE.run(false, helm_core::now_ms(), || {
                store::refresh(&db, &root, &own, Some(SEARCH_REFRESH_BUDGET))
            })
            .map_err(sql)?;
            let mut r = store::search(&db, query, helm_core::now_ms()).map_err(sql)?;
            r.hits.retain(|h| !own.contains(&h.session_id));
            to_json(&r)
        }
    }
}

pub(crate) fn read_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ReadArgs = parse_args(args)?;
    if !valid_session_id(&a.session_id) {
        return Err(ApiError::bad_args("That session id is not valid."));
    }
    let db = ctx.services.need::<Database>()?;
    let (path, folder, row_title, updated_at) = store::record(&db, &a.session_id)
        .map_err(sql)?
        .ok_or_else(|| {
            ApiError::from(helm_chats::archive::Refusal::new(
                "not_found",
                "That chat is not in the indexed records.",
            ))
        })?;
    let tr = parse::parse_file(&PathBuf::from(&path)).map_err(|_| {
        ApiError::from(helm_chats::archive::Refusal::new(
            "not_found",
            "That chat's record is no longer on disk.",
        ))
    })?;
    let folder = if tr.cwd.is_empty() {
        folder
    } else {
        tr.cwd.clone()
    };
    let (lines, truncated) = store::view(&tr);
    let read = ChatRead {
        session_id: a.session_id,
        title: if tr.title.is_empty() {
            row_title
        } else {
            tr.title
        },
        folder: folder.clone(),
        updated_at: tr.last_ts.max(updated_at),
        forkable: !folder.is_empty() && Path::new(&folder).is_dir(),
        truncated,
        lines,
    };
    to_json(&read)
}

pub(crate) fn fork_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ForkArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let profile = ctx.services.need::<Profile>()?;
    let gates = ctx.services.get::<ProjectGates>();
    let out =
        fork::fork(&db, &profile.dir, &a.session_id, gates.as_deref()).map_err(ApiError::from)?;
    // Announce the new chat exactly as a schedule's new chat is announced.
    let rev = ctx
        .services
        .get::<helm_chats::clock::Clock>()
        .map_or(0, |c| c.next());
    if let Ok(Some(chat)) = db.snapshot(|tx| helm_chats::store::get_chat(tx, &out.chat_id, rev)) {
        ctx.events.emit(ChatEvent::Chat { chat }.to_app_event());
    }
    to_json(&out)
}
