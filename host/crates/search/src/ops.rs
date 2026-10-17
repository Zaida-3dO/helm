//! The `chat_search` op.

use crate::types::{OpenArgs, SearchArgs};
use crate::{search, signals};
use helm_core::db::Database;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::Value;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("chat_search", OpClass::Heavy, chat_search)
        .blocking(
            "search_open_record",
            OpClass::Maintenance,
            search_open_record,
        )
        .blocking(
            "search_signals_clear",
            OpClass::Maintenance,
            search_signals_clear,
        );
}

pub(crate) fn chat_search(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SearchArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    let r = search::run(&db, &a, now)?;
    serde_json::to_value(&r).map_err(|e| ApiError::internal(e.to_string()))
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

/// Count one open of a chat (the user chose it). A chat that does not exist is ignored.
pub(crate) fn search_open_record(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: OpenArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let known: bool = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1)",
                [&a.chat_id],
                |r| r.get(0),
            )
        })
        .map_err(sql)?;
    if known {
        signals::record_open(&db, &a.chat_id, helm_core::now_ms()).map_err(sql)?;
    }
    Ok(Value::Null)
}

pub(crate) fn search_signals_clear(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    signals::clear(&db).map_err(sql)?;
    Ok(Value::Null)
}
