//! The four ops.

use crate::fork::{self, valid_session_id};
use crate::parse;
use crate::projects_root;
use crate::store;
use crate::types::*;
use helm_chats::seams::ProjectGates;
use helm_chats::types::ChatEvent;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Records listed and searched at once.
const LIST_LIMIT: i64 = 200;

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
            let missing = store::refresh(&db, &root).map_err(sql)?;
            (store::list(&db, LIST_LIMIT).map_err(sql)?, missing)
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
            store::refresh(&db, &root).map_err(sql)?;
            let r = store::search(&db, query, helm_core::now_ms()).map_err(sql)?;
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
