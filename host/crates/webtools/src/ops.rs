//! The ops behind Settings > Web search. None of them returns the key, and their errors are fixed
//! sentences that never carry what was typed.

use crate::settings::EVENT;
use crate::{store, WebSearch};
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

/// What Settings may see: whether a key is set, never the key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub has_key: bool,
    pub offer_in_claude_chats: bool,
    /// The settings file "Import from OpenClaw" will read: the last one used, else the default.
    pub import_path: String,
    pub default_import_path: String,
}

fn view(web: &WebSearch) -> View {
    let s = web.settings();
    let default_import_path = store::default_import_path();
    View {
        has_key: web.has_key(),
        offer_in_claude_chats: s.offer_in_claude_chats,
        import_path: s.import_path.unwrap_or_else(|| default_import_path.clone()),
        default_import_path,
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn web(ctx: &OpCtx) -> Result<Arc<WebSearch>, ApiError> {
    ctx.services.need::<WebSearch>()
}

fn changed(ctx: &OpCtx) {
    ctx.events.emit(AppEvent::new(EVENT));
}

/// Read the arguments; a failure never echoes them (a key may be among them).
fn args<T: for<'de> Deserialize<'de>>(v: Value, shape: &str) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|_| ApiError::bad_args(format!("Expected {shape}.")))
}

/// Change the settings record in one transaction.
fn update(web: &WebSearch, f: impl FnOnce(&mut store::Settings)) -> Result<(), ApiError> {
    web.db
        .write(|tx| {
            let mut s = store::load(tx);
            f(&mut s);
            store::save(tx, &s)
        })
        .map_err(|_| ApiError::internal("The web search settings could not be saved."))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveArgs {
    #[serde(default)]
    offer_in_claude_chats: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportArgs {
    path: String,
}

pub(crate) fn get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    to_value(&view(&*web(ctx)?))
}

pub(crate) fn save(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: SaveArgs = args(v, "offerInClaudeChats (true or false)")?;
    let web = web(ctx)?;
    if let Some(on) = a.offer_in_claude_chats {
        update(&web, |s| s.offer_in_claude_chats = on)?;
        changed(ctx);
    }
    to_value(&view(&web))
}

pub(crate) fn set_key(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: KeyArgs = args(v, "key")?;
    let web = web(ctx)?;
    store::set_key(&web.creds, &a.key).map_err(ApiError::bad_args)?;
    changed(ctx);
    to_value(&view(&web))
}

pub(crate) fn forget_key(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let web = web(ctx)?;
    store::forget_key(&web.creds).map_err(ApiError::internal)?;
    changed(ctx);
    to_value(&view(&web))
}

/// One search with the stored key: `{ok, message}`. A refusal from Brave is an answer, not an
/// error, so the page can show it beside the button.
pub(crate) fn test(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    Ok(match web(ctx)?.search("web search test", 1, None) {
        Ok(hits) => json!({
            "ok": true,
            "message": format!("Brave search works ({} result{} for a test search).", hits.len(), if hits.len() == 1 { "" } else { "s" }),
        }),
        Err(refusal) => json!({"ok": false, "message": refusal.0}),
    })
}

/// Read the key from an OpenClaw settings file once, seal it, and answer only `imported`.
pub(crate) fn import(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: ImportArgs = args(v, "path")?;
    let web = web(ctx)?;
    let key = store::read_openclaw_key(&a.path).map_err(ApiError::bad_args)?;
    store::set_key(&web.creds, &key).map_err(ApiError::bad_args)?;
    let path = a.path.trim().to_owned();
    update(&web, |s| {
        s.import_path = (path != store::default_import_path()).then_some(path);
    })?;
    changed(ctx);
    Ok(json!({"imported": true}))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("web_search_get", OpClass::Blocking, get)
        .blocking("web_search_save", OpClass::Maintenance, save)
        .blocking("web_search_set_key", OpClass::Maintenance, set_key)
        .blocking("web_search_forget_key", OpClass::Maintenance, forget_key)
        .blocking("web_search_test", OpClass::Blocking, test)
        .blocking("web_search_import", OpClass::Maintenance, import);
}
