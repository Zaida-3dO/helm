//! The schedules `api` ops.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `schedule_list` | blocking | `{includeInactive?}` to [`ScheduleList`] (schedules newest first, the templates, the host clock) |
//! | `schedule_runs` | blocking | `{scheduleId}` to [`RunList`] (the newest 20 receipts) |
//! | `schedule_create` | maintenance | `{requestId, title, message, timing, kind?, destination?, chatId?, projectId?, createdByChat?, template?}` to [`Schedule`]; repeating a request id returns the saved schedule |
//! | `schedule_update` | maintenance | `{scheduleId, revision, title?, message?, timing?, kind?, destination?, chatId?, projectId?, paused?}` to [`Schedule`] |
//! | `schedule_cancel` | maintenance | `{scheduleId, revision}` to [`Schedule`] |
//!
//! | `reminder_list` | blocking | `{}` to [`ReminderList`] (reminders that came due and are not dismissed) |
//! | `reminder_dismiss` | maintenance | `{reminderId?}` (all of them when absent) to `{dismissed}` |
//!
//! Every change emits the `schedules` event; the pane re-reads on it.

use crate::scheduler::SchedulesService;
use crate::store::{self, NewSchedule, Update};
use crate::templates;
use crate::types::*;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    include_inactive: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunsArgs {
    schedule_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateArgs {
    request_id: String,
    title: String,
    message: String,
    timing: Timing,
    #[serde(default)]
    kind: ScheduleKind,
    #[serde(default)]
    destination: Destination,
    #[serde(default)]
    chat_id: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    created_by_chat: Option<String>,
    #[serde(default)]
    template: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateArgs {
    schedule_id: String,
    revision: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    timing: Option<Timing>,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    kind: Option<ScheduleKind>,
    #[serde(default)]
    destination: Option<Destination>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    created_by_chat: Option<String>,
    #[serde(default)]
    paused: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DismissArgs {
    #[serde(default)]
    reminder_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelArgs {
    schedule_id: String,
    revision: i64,
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("schedule_list", OpClass::Blocking, list)
        .blocking("schedule_runs", OpClass::Blocking, runs)
        .blocking("schedule_create", OpClass::Maintenance, create)
        .blocking("schedule_update", OpClass::Maintenance, update)
        .blocking("schedule_cancel", OpClass::Maintenance, cancel)
        .blocking("reminder_list", OpClass::Blocking, reminder_list)
        .blocking("reminder_dismiss", OpClass::Maintenance, reminder_dismiss);
}

fn list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ListArgs = parse(args)?;
    let svc = ctx.services.need::<SchedulesService>()?;
    let schedules = svc.db.snapshot(|tx| store::list(tx, a.include_inactive))?;
    to_value(&ScheduleList {
        schedules,
        templates: templates::templates(),
        now: helm_core::now_ms(),
    })
}

fn runs(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: RunsArgs = parse(args)?;
    let svc = ctx.services.need::<SchedulesService>()?;
    let runs = svc.db.snapshot(|tx| store::runs(tx, &a.schedule_id))?;
    to_value(&RunList { runs })
}

fn create(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CreateArgs = parse(args)?;
    if let Some(t) = &a.template {
        if templates::by_id(t).is_none() {
            return Err(ApiError::bad_args("That template does not exist."));
        }
    }
    let svc = ctx.services.need::<SchedulesService>()?;
    let now = helm_core::now_ms();
    let s = svc.db.write(|tx| {
        store::create(
            tx,
            svc.zone.as_ref(),
            now,
            NewSchedule {
                request_id: a.request_id,
                definition: Definition {
                    title: a.title,
                    message: a.message,
                    timing: a.timing,
                    kind: a.kind,
                    destination: a.destination,
                    chat_id: a.chat_id,
                    project_id: a.project_id,
                    created_by_chat: a.created_by_chat,
                    template: a.template,
                },
            },
        )
    })?;
    svc.changed();
    to_value(&s)
}

fn update(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: UpdateArgs = parse(args)?;
    let svc = ctx.services.need::<SchedulesService>()?;
    let now = helm_core::now_ms();
    let s = svc.db.write(|tx| {
        store::update(
            tx,
            svc.zone.as_ref(),
            now,
            &a.schedule_id,
            a.revision,
            Update {
                title: a.title,
                message: a.message,
                timing: a.timing,
                chat_id: a.chat_id,
                kind: a.kind,
                destination: a.destination,
                project_id: a.project_id,
                created_by_chat: a.created_by_chat,
                paused: a.paused,
            },
        )
    })?;
    svc.changed();
    to_value(&s)
}

fn cancel(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CancelArgs = parse(args)?;
    let svc = ctx.services.need::<SchedulesService>()?;
    let s = svc
        .db
        .write(|tx| store::cancel(tx, &a.schedule_id, a.revision))?;
    svc.changed();
    to_value(&s)
}

fn reminder_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    parse::<NoArgs>(args)?;
    let svc = ctx.services.need::<SchedulesService>()?;
    let reminders = svc.db.snapshot(store::reminders)?;
    to_value(&ReminderList { reminders })
}

fn reminder_dismiss(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: DismissArgs = parse(args)?;
    let svc = ctx.services.need::<SchedulesService>()?;
    let n = svc
        .db
        .write(|tx| store::dismiss_reminders(tx, a.reminder_id.as_deref()))?;
    if n > 0 {
        svc.changed();
    }
    Ok(serde_json::json!({"dismissed": n}))
}
