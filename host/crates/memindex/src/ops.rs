//! The ops behind Settings → Memory. Every one is the user's own: an agent reaches the same
//! service through `app_memory_index`, where adding or removing a source needs an Allow card.

use crate::service::{Service, SettingsChanges, SourceInput};
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

fn service(ctx: &OpCtx) -> Result<Arc<Service>, ApiError> {
    ctx.services
        .get::<Service>()
        .ok_or_else(|| ApiError::new("unavailable", "Memory indexing is not available here."))
}

fn parse<T: for<'de> Deserialize<'de>>(v: Value, shape: &str) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|_| ApiError::bad_args(format!("Expected {shape}.")))
}

fn state(svc: &Service) -> Result<Value, ApiError> {
    serde_json::to_value(svc.view()).map_err(|e| ApiError::internal(e.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdArgs {
    id: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReindexArgs {
    source_id: Option<String>,
    #[serde(default)]
    force: bool,
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("memindex_state", OpClass::Blocking, |ctx, _| {
        state(&*service(ctx)?)
    })
    .blocking("memindex_settings_save", OpClass::Maintenance, |ctx, v| {
        let svc = service(ctx)?;
        let c: SettingsChanges = parse(v, "enabled, serverId, model, nightly (all optional)")?;
        svc.save_settings(c).map_err(ApiError::bad_args)?;
        state(&svc)
    })
    .blocking("memindex_sources_save", OpClass::Maintenance, |ctx, v| {
        let svc = service(ctx)?;
        let input: SourceInput = parse(
            v,
            "a source: id?, kind?, path?, personId?, scope?, include?, exclude?, enabled?",
        )?;
        svc.save_source(input).map_err(ApiError::bad_args)?;
        state(&svc)
    })
    .blocking("memindex_sources_remove", OpClass::Maintenance, |ctx, v| {
        let svc = service(ctx)?;
        let a: IdArgs = parse(v, "{id}")?;
        svc.remove_source(&a.id)
            .map_err(|e| ApiError::new("refused", e))?;
        state(&svc)
    })
    .blocking("memindex_reindex", OpClass::Blocking, |ctx, v| {
        let svc = service(ctx)?;
        let a: ReindexArgs = if v.is_null() {
            ReindexArgs::default()
        } else {
            parse(v, "{sourceId?, force?}")?
        };
        svc.reindex(a.source_id, a.force);
        Ok(json!({"started": true}))
    });
}
