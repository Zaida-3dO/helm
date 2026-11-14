//! The consent `api` ops. Both are main-window only, like every op: the
//! dispatcher refuses any other caller, so no agent, box, schedule or other chat can
//! decide. `consent_decide` goes through [`crate::apply::decide_and_apply`], which applies an
//! approved request of an applying kind.

use crate::kinds::AllowKinds;
use crate::store;
use crate::types::{ConsentDecideArgs, ConsentListArgs};
use helm_core::db::Database;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::Value;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("consent_list", OpClass::Blocking, consent_list)
        .blocking("consent_decide", OpClass::Blocking, consent_decide);
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn kinds(ctx: &OpCtx) -> std::sync::Arc<AllowKinds> {
    ctx.services
        .get::<AllowKinds>()
        .unwrap_or_else(|| std::sync::Arc::new(AllowKinds::new()))
}

pub(crate) fn consent_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ConsentListArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    to_value(&store::list(
        &db,
        &kinds(ctx),
        &a.chat_id,
        helm_core::now_ms(),
    )?)
}

pub(crate) fn consent_decide(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ConsentDecideArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    // The click in this PC's main window. A request Helm applies itself starts applying here,
    // on its own thread; the click does not wait for it.
    let d = crate::apply::decide_and_apply(&ctx.services, &ctx.events, &a, "app")?;
    to_value(&d.request)
}
