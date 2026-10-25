//! The meter's `api` ops.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `usage_get` | blocking | `{}` to [`UsageSnapshot`](crate::types::UsageSnapshot) |
//! | `request_modes` | heavy | `{since, until}` (epoch ms) to [`RequestModes`](crate::modes::RequestModes) |
//! | `usage_refresh` | blocking | `{}` to the snapshot at once; a read starts when the 60 s floor allows, and its result arrives as the `plan_usage` event (the snapshot's fields) |
//! | `usage_cost` | blocking | `{chatId}` or `{projectId}` to [`CostReport`](crate::types::CostReport): tokens, the reported cost and the estimated weekly share ([`crate::summary::cost_report`]) |

use crate::modes::{ModesArgs, ModesService};
use crate::service::UsageService;
use crate::summary::cost_report;
use crate::types::CostScope;
use helm_core::db::Database;
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
    .blocking("request_modes", OpClass::Heavy, request_modes)
    .blocking("usage_cost", OpClass::Blocking, usage_cost);
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CostArgs {
    chat_id: Option<String>,
    project_id: Option<String>,
}

fn usage_cost(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CostArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let (scope, id) = match (a.chat_id, a.project_id) {
        (Some(c), None) => (CostScope::Chat, c),
        (None, Some(p)) => (CostScope::Project, p),
        _ => {
            return Err(ApiError::bad_args(
                "usage_cost takes a chatId or a projectId.",
            ))
        }
    };
    let now = helm_core::now_ms();
    let weekly = ctx.services.get::<UsageService>().and_then(|svc| {
        let w = svc.snapshot(now).reading?.weekly?;
        Some((w.pct?, w.resets_at?))
    });
    let db = ctx.services.need::<Database>()?;
    let report =
        cost_report(&db, scope, &id, weekly, now).map_err(|e| ApiError::new("refused", e))?;
    to_value(&report)
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
