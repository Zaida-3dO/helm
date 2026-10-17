//! The project and sidebar `api` ops.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `project_list` | blocking | [`ProjectListArgs`] → `ProjectNode[]` (flat; the tree comes from `parentId`) |
//! | `project_create` | maintenance | [`ProjectCreateArgs`] → [`ProjectNode`] |
//! | `project_update` | maintenance | [`ProjectUpdateArgs`] → [`ProjectNode`] |
//! | `project_archive` / `project_restore` | maintenance | [`ProjectIdArgs`] → [`ProjectNode`] |
//! | `sidebar_page` | heavy | [`SidebarPageArgs`] → [`SidebarPage`] |
//! | `sidebar_rows` | blocking | [`SidebarRowsArgs`] → `SidebarRow[]` |
//! | `chat_organize` | maintenance | [`ChatOrganizeArgs`] → `SidebarRow[]` (the rows that moved) |
//!
//! Events: `project {project}` after a project changes, `sidebar {chatIds}` (a re-fetch signal)
//! after rows move.

use crate::store::{self, Refusal};
use crate::types::*;
use helm_chats::clock::Clock;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("project_list", OpClass::Blocking, project_list)
        .blocking("project_create", OpClass::Maintenance, project_create)
        .blocking("project_update", OpClass::Maintenance, project_update)
        .blocking("project_archive", OpClass::Maintenance, project_archive)
        .blocking("project_restore", OpClass::Maintenance, project_restore)
        .blocking("sidebar_page", OpClass::Heavy, sidebar_page)
        .blocking("sidebar_rows", OpClass::Blocking, sidebar_rows)
        .blocking("chat_organize", OpClass::Maintenance, chat_organize);
    crate::board::register_ops(reg);
}

pub(crate) fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ApiError> {
    let args = if args.is_null() {
        Value::Object(Default::default())
    } else {
        args
    };
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

pub(crate) fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

impl From<Refusal> for ApiError {
    fn from(r: Refusal) -> Self {
        match r {
            Refusal::BadArgs(m) => ApiError::bad_args(m),
            Refusal::NotFound(m) => ApiError::new("not_found", m),
            Refusal::Conflict(m) => ApiError::conflict(m),
            Refusal::Refused(m) => ApiError::new("refused", m),
            Refusal::Sql(e) => ApiError::internal(e.to_string()),
        }
    }
}

pub(crate) fn emit_project(ctx: &OpCtx, node: &ProjectNode) {
    if let Ok(v) = serde_json::to_value(node) {
        ctx.events.emit(AppEvent::new("project").with("project", v));
    }
}

pub(crate) fn emit_sidebar(ctx: &OpCtx, ids: &[String]) {
    if !ids.is_empty() {
        ctx.events
            .emit(AppEvent::new("sidebar").with("chatIds", serde_json::json!(ids)));
    }
}

fn project_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ProjectListArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let list = db.snapshot(|tx| store::list_projects(tx, a.include_archived.unwrap_or(false)))?;
    to_value(&list)
}

fn project_create(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ProjectCreateArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let node = db.write(|tx| {
        let id = store::create_project(tx, &a, helm_core::now_ms())?;
        store::get_project(tx, &id)
    })?;
    emit_project(ctx, &node);
    to_value(&node)
}

fn project_update(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ProjectUpdateArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let node = db.write(|tx| {
        store::update_project(tx, &a)?;
        store::get_project(tx, &a.project_id)
    })?;
    emit_project(ctx, &node);
    to_value(&node)
}

fn archive_or_restore(ctx: &OpCtx, args: Value, archive: bool) -> Result<Value, ApiError> {
    let a: ProjectIdArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let (node, ids) = db.write(|tx| {
        let ids = if archive {
            store::archive_project(tx, &a.project_id)?
        } else {
            store::restore_project(tx, &a.project_id)?
        };
        Ok::<_, Refusal>((store::get_project(tx, &a.project_id)?, ids))
    })?;
    emit_project(ctx, &node);
    emit_sidebar(ctx, &ids);
    to_value(&node)
}

fn project_archive(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    archive_or_restore(ctx, args, true)
}

fn project_restore(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    archive_or_restore(ctx, args, false)
}

fn revision(ctx: &OpCtx) -> i64 {
    ctx.services
        .get::<Clock>()
        .map(|c| c.current())
        .unwrap_or(0)
}

fn sidebar_page(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SidebarPageArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let rev = revision(ctx);
    to_value(&db.snapshot(|tx| store::sidebar_page(tx, &a, rev))?)
}

fn sidebar_rows(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SidebarRowsArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let rev = revision(ctx);
    to_value(&db.snapshot(|tx| store::sidebar_rows(tx, &a.chat_ids, rev))?)
}

fn chat_organize(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatOrganizeArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let ids = db.write(|tx| store::organize(tx, &a))?;
    let rev = ctx.services.get::<Clock>().map(|c| c.next()).unwrap_or(0);
    let rows = db.snapshot(|tx| store::sidebar_rows(tx, &ids, rev))?;
    emit_sidebar(ctx, &ids);
    to_value(&rows)
}
