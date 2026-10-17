//! The `api` ops for action buttons.

use crate::store;
use crate::types::ActionsSaveArgs;
use helm_core::db::Database;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::Value;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("actions_get", OpClass::Blocking, actions_get)
        .blocking("actions_save", OpClass::Maintenance, actions_save);
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

pub(crate) fn actions_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    if !(args.is_null() || args.as_object().is_some_and(|o| o.is_empty())) {
        return Err(ApiError::bad_args("actions_get takes no arguments."));
    }
    let db = ctx.services.need::<Database>()?;
    to_value(&store::get_or_seed(&db)?)
}

pub(crate) fn actions_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ActionsSaveArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    to_value(&store::save(&db, a)?)
}
