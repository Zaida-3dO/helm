//! AFK's `api` ops. UI-only: no agent tool reaches AFK, the preference is user-only.
//!
//! | op | args | result |
//! |---|---|---|
//! | `afk_state` | `{}` | [`AfkView`](crate::service::AfkView) |
//! | `afk_settings` | `{settings}` | strict save, then the view |
//! | `afk_set` | `{on}` | the user's switch: on opens a manual span, off ends any span |
//! | `afk_retry` / `afk_dismiss_failures` | `{spanId}` | retry or dismiss failed digests |
//! | `afk_presence` | `{focused?, input?}` | window focus and in-window input |
//!
//! Every change also pushes an `afk` event carrying the view.

use crate::service::AfkService;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::Value;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("afk_state", OpClass::Blocking, |ctx, _| {
        to_value(&svc(ctx)?.view()?)
    })
    .blocking("afk_settings", OpClass::Blocking, |ctx, args| {
        let a: SettingsArgs = parse(args)?;
        to_value(&svc(ctx)?.save_settings(&a.settings, helm_core::now_ms())?)
    })
    .blocking("afk_set", OpClass::Blocking, |ctx, args| {
        let a: SetArgs = parse(args)?;
        to_value(&svc(ctx)?.set(a.on, helm_core::now_ms())?)
    })
    .blocking("afk_retry", OpClass::Blocking, |ctx, args| {
        let a: SpanArgs = parse(args)?;
        to_value(&svc(ctx)?.retry(&a.span_id, helm_core::now_ms())?)
    })
    .blocking("afk_dismiss_failures", OpClass::Blocking, |ctx, args| {
        let a: SpanArgs = parse(args)?;
        to_value(&svc(ctx)?.dismiss(&a.span_id)?)
    })
    .blocking("afk_presence", OpClass::Blocking, |ctx, args| {
        let a: PresenceArgs = parse(args)?;
        svc(ctx)?.presence(a.focused, a.input, helm_core::now_ms());
        Ok(Value::Null)
    });
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArgs {
    settings: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    on: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SpanArgs {
    span_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresenceArgs {
    #[serde(default)]
    focused: Option<bool>,
    #[serde(default)]
    input: bool,
}

fn svc(ctx: &OpCtx) -> Result<std::sync::Arc<AfkService>, ApiError> {
    ctx.services.need::<AfkService>()
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}
