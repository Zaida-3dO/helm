//! The consent `api` ops. Both are main-window only, like every op: the
//! dispatcher refuses any other caller, so no agent, box, schedule or other chat can
//! decide.

use crate::kinds::AllowKinds;
use crate::store;
use crate::types::{ConsentDecideArgs, ConsentListArgs};
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
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
    let db = ctx.services.need::<Database>()?;
    let v = store::decide(
        &db,
        &kinds(ctx),
        &a.request_id,
        a.decision,
        a.grant_minutes,
        a.note.as_deref(),
        helm_core::now_ms(),
    )?;
    // A signal only: the card is rebuilt from consent_list, never from an event payload.
    ctx.events
        .emit(AppEvent::new("consent").with("chatId", v.chat_id.clone()));
    // The decision is owed to the agent: wake the delivery loop.
    if let Some(w) = ctx.services.get::<crate::ConsentWake>() {
        w.0.notify_one();
    }
    to_value(&v)
}
