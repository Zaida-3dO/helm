//! The gatekeeper's `api` ops. UI-only: no agent tool reads or writes the audit trail, so
//! the tool surface does not grow.
//!
//! | op | args | result |
//! |---|---|---|
//! | `gate_list` | `{filter?, query?, limit?}` | `{rows: GateAsk[]}` for the current person — `filter` is `all` (default), `decided`, `asked` (the ones kept for the user), `failed` or `review` (decided rows still waiting for the user's mark) |
//! | `gate_review` | `{id, review, note?}` | mark a decided row `fine` or `should_have_asked`, or an escalated one `didnt_need_me` (note at most 300 characters); any other pairing is refused |
//! | `gate_change_answer` | `{id, answer}` | the user's correction of a decided row; it goes into the chat as an Answer-origin message |
//! | `gate_settings_get` | `{}` | the record for the current person, plus the pause state and the bar it implies |
//! | `gate_settings_save` | `{settings}` | strict save, then the view |
//!
//! Every change also pushes a `gate` event carrying the ask id, the chat and what changed.

use crate::service::GateService;
use crate::settings::{self, GateSettings};
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("gate_list", OpClass::Blocking, |ctx, args| {
        let a: ListArgs = parse(args)?;
        let limit = a.limit.unwrap_or(200).clamp(1, 500);
        let rows = svc(ctx)?.list(
            a.filter.as_deref().unwrap_or("all"),
            a.query.as_deref().unwrap_or(""),
            limit,
            helm_core::now_ms(),
        )?;
        Ok(json!({ "rows": rows }))
    })
    .blocking("gate_review", OpClass::Blocking, |ctx, args| {
        let a: ReviewArgs = parse(args)?;
        to_value(&svc(ctx)?.review(&a.id, &a.review, a.note.as_deref())?)
    })
    .blocking("gate_change_answer", OpClass::Maintenance, |ctx, args| {
        let a: ChangeArgs = parse(args)?;
        to_value(&svc(ctx)?.change_answer(&a.id, &a.answer)?)
    })
    .blocking("gate_settings_get", OpClass::Blocking, |ctx, _| {
        to_value(&view(ctx)?)
    })
    .blocking("gate_settings_save", OpClass::Maintenance, |ctx, args| {
        let a: SettingsArgs = parse(args)?;
        let s = settings::parse_strict(&a.settings).map_err(ApiError::bad_args)?;
        save(ctx, &s)?;
        to_value(&view(ctx)?)
    });
}

/// The whole view the settings page shows: the record, the bar it implies, and whether the
/// gate is currently paused after repeated failures.
fn view(ctx: &OpCtx) -> Result<Value, ApiError> {
    let s = current_settings(ctx)?;
    let paused = svc(ctx)?.paused_until(helm_core::now_ms());
    Ok(json!({
        "enabled": s.enabled,
        "askFrequency": s.ask.as_str(),
        "policy": s.policy,
        "defaultPolicy": settings::DEFAULT_POLICY,
        "model": s.model,
        "bar": s.bar(),
        "paused": paused.is_some(),
        "pausedUntil": paused,
    }))
}

/// The record of the person this PC is currently set to; chats with no person and machines
/// with none use the same fallback key.
fn current_settings(ctx: &OpCtx) -> Result<GateSettings, ApiError> {
    let db = ctx.services.need::<helm_core::db::Database>()?;
    db.snapshot(|tx| crate::store::load_settings(tx, &crate::store::active_profile(tx)))
        .map_err(|e: rusqlite::Error| ApiError::internal(e.to_string()))
}

fn save(ctx: &OpCtx, s: &GateSettings) -> Result<(), ApiError> {
    let db = ctx.services.need::<helm_core::db::Database>()?;
    let owned = s.clone();
    db.write(|tx| crate::store::save_settings(tx, &crate::store::active_profile(tx), &owned))
        .map_err(|e: rusqlite::Error| ApiError::internal(e.to_string()))?;
    ctx.events
        .emit(helm_ipc::AppEvent::new("gate").with("what", "settings"));
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewArgs {
    id: String,
    review: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChangeArgs {
    id: String,
    answer: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArgs {
    settings: Value,
}

fn svc(ctx: &OpCtx) -> Result<Arc<GateService>, ApiError> {
    ctx.services.need::<GateService>()
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}
