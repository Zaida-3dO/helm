//! The device `api` ops.

use crate::identity::{self, DeviceError};
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("device_get", OpClass::Blocking, get)
        .blocking("device_rename", OpClass::Maintenance, rename);
}

fn fail(e: DeviceError) -> ApiError {
    match e {
        DeviceError::Bad(m) => ApiError::bad_args(m),
        DeviceError::Db(m) => ApiError::internal(m),
    }
}

fn get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let (uid, name) = db
        .write(identity::ensure)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(json!({ "uid": uid, "name": name }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenameArgs {
    name: String,
}

fn rename(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: RenameArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let (uid, name) = db
        .write(|tx| identity::rename(tx, &a.name))
        .map_err(fail)?;
    ctx.events.emit(AppEvent::new("device_changed"));
    Ok(json!({ "uid": uid, "name": name }))
}
