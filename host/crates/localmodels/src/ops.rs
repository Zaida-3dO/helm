//! The `api` ops for Settings > Local models.
//!
//! | Op | Class | Args | Result |
//! |---|---|---|---|
//! | `local_models_state` | blocking | none | `LocalModelsState` |
//! | `local_models_save` | maintenance | `{servers?, ollamaContext?, keepAliveMinutes?, strataNeedGb?, tools?, compareWith?}` | `LocalModelsState` |
//! | `local_server_key_set` | maintenance | `{serverId, key}` | `LocalModelsState` |
//! | `local_server_key_forget` | maintenance | `{serverId}` | `LocalModelsState` |
//! | `local_server_test` | blocking | `{serverId}` | `TestResult` |
//! | `local_models_refresh` | blocking | none | `RefreshResult` |
//!
//! No op returns a key: the state only says whether one is set. Test and refresh are the only ops
//! that contact a server, and they do so only when called.

use crate::service::LocalService;
use crate::settings::Changes;
use crate::types::{KeyArgs, RefreshResult, ServerIdArgs};
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

fn svc(ctx: &OpCtx) -> Result<Arc<LocalService>, ApiError> {
    ctx.services.need::<LocalService>()
}

fn parse<T: DeserializeOwned>(args: Value, op: &str) -> Result<T, ApiError> {
    serde_json::from_value(args)
        .map_err(|e| ApiError::bad_args(format!("{op} was given arguments it does not take: {e}")))
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn refused(message: String) -> ApiError {
    ApiError::new("refused", message)
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("local_models_state", OpClass::Blocking, |ctx, _| {
        to_value(&svc(ctx)?.view())
    })
    .blocking("local_models_save", OpClass::Maintenance, |ctx, args| {
        let changes: Changes = parse(args, "local_models_save")?;
        to_value(&svc(ctx)?.save(&changes).map_err(refused)?)
    })
    .blocking("local_server_key_set", OpClass::Maintenance, |ctx, args| {
        // A bad argument must not echo the key back in its message.
        let a: KeyArgs = serde_json::from_value(args).map_err(|_| {
            ApiError::bad_args("local_server_key_set takes {serverId, key}, both text.")
        })?;
        let s = svc(ctx)?;
        s.set_key(&a.server_id, &a.key).map_err(refused)?;
        to_value(&s.view())
    })
    .blocking(
        "local_server_key_forget",
        OpClass::Maintenance,
        |ctx, args| {
            let a: ServerIdArgs = parse(args, "local_server_key_forget")?;
            let s = svc(ctx)?;
            s.forget_key(&a.server_id).map_err(refused)?;
            to_value(&s.view())
        },
    )
    .blocking("local_server_test", OpClass::Blocking, |ctx, args| {
        let a: ServerIdArgs = parse(args, "local_server_test")?;
        to_value(&svc(ctx)?.test(&a.server_id).map_err(refused)?)
    })
    .blocking("local_models_refresh", OpClass::Blocking, |ctx, _| {
        let s = svc(ctx)?;
        to_value(&RefreshResult {
            models: s.refresh(),
            notes: s.notes(),
        })
    });
}
