//! The meter's `api` ops.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `usage_get` | blocking | `{}` to [`UsageSnapshot`](crate::types::UsageSnapshot) |
//! | `request_modes` | heavy | `{since, until}` (epoch ms) to [`RequestModes`](crate::modes::RequestModes) |
//! | `usage_refresh` | blocking | `{}` to the snapshot at once; a read starts when the 60 s floor allows, and its result arrives as the `plan_usage` event (the snapshot's fields) |

use crate::modes::{ModesArgs, ModesService};
use crate::service::UsageService;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::Value;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("usage_get", OpClass::Blocking, |ctx, args| {
        no_args(&args, "usage_get")?;
        to_value(
            &ctx.services
                .need::<UsageService>()?
                .snapshot(helm_core::now_ms()),
        )
    })
    .blocking("usage_refresh", OpClass::Blocking, refresh)
    .blocking("request_modes", OpClass::Heavy, request_modes);
}

fn no_args(args: &Value, op: &str) -> Result<(), ApiError> {
    if args.is_null() || args.as_object().is_some_and(|o| o.is_empty()) {
        Ok(())
    } else {
        Err(ApiError::bad_args(format!("{op} takes no arguments.")))
    }
}

fn refresh(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    no_args(&args, "usage_refresh")?;
    let svc = ctx.services.need::<UsageService>()?;
    to_value(&svc.refresh(helm_core::now_ms()))
}

fn request_modes(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ModesArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let svc = ctx.services.need::<ModesService>()?;
    to_value(&svc.request_modes(&a, helm_core::now_ms())?)
}

pub(crate) fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}
