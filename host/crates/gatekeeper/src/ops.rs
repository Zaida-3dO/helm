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
//! | `workmode_get` | `{chatId?}` | the working mode view: the app's mode, the board the chat is shown under and its own mode, the mode the chat works in and where it was set, every board override, and the counts |
//! | `workmode_set` | `{scope, boardId?, mode, chatId?}` | `scope` is `app` or `board`; `mode` is `run`, `check_in` or `null` (clear: the app's default, or the app's mode for a board); answers with the view. Busy chats whose mode changed get one note from Helm |
//!
//! Every change also pushes a `gate` event carrying the ask id, the chat and what changed
//! (`workmode` for the working mode). No agent tool reads or writes the working mode.

use crate::service::GateService;
use crate::settings::{self, GateSettings};
use crate::workmode;
use helm_chats::ChatRuntime;
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
    })
    .blocking("workmode_get", OpClass::Blocking, |ctx, args| {
        let a: ModeGetArgs = parse(args)?;
        to_value(&mode_view(ctx, a.chat_id.as_deref())?)
    })
    .blocking("workmode_set", OpClass::Maintenance, |ctx, args| {
        let a: ModeSetArgs = parse(args)?;
        set_mode(ctx, &a)?;
        to_value(&mode_view(ctx, a.chat_id.as_deref())?)
    });
}

/// The working-mode view for a chat (the app's own with none), with how many chats wait on the
/// user right now.
fn mode_view(ctx: &OpCtx, chat_id: Option<&str>) -> Result<workmode::WorkModeView, ApiError> {
    let db = ctx.services.need::<helm_core::db::Database>()?;
    let waiting = ctx
        .services
        .get::<ChatRuntime>()
        .and_then(|rt| rt.attention().ok())
        .map_or(0, |s| s.needs_you.len() as u32);
    workmode::view(&db, chat_id, waiting, helm_core::now_ms())
        .map_err(|e| ApiError::internal(e.to_string()))
}

/// Change the app's mode or one board's. Busy chats whose own mode changed are told once, as a
/// note from Helm that lands at their next tool boundary; idle chats get nothing (a note would
/// start a turn) and read the change in their next turn instead.
fn set_mode(ctx: &OpCtx, a: &ModeSetArgs) -> Result<(), ApiError> {
    let db = ctx.services.need::<helm_core::db::Database>()?;
    let mode = match a.mode.as_deref() {
        None => None,
        Some(m) => Some(workmode::Mode::parse(m).ok_or_else(|| {
            ApiError::bad_args(format!(
                "\"{m}\" is not a working mode: use \"run\", \"check_in\" or null."
            ))
        })?),
    };
    let board = match a.scope.as_str() {
        "app" => None,
        "board" => Some(
            a.board_id
                .as_deref()
                .filter(|b| !b.is_empty())
                .ok_or_else(|| ApiError::bad_args("A board needs its boardId."))?,
        ),
        other => {
            return Err(ApiError::bad_args(format!(
                "\"{other}\" is not a scope: use \"app\" or \"board\"."
            )))
        }
    };
    let rt = ctx.services.get::<ChatRuntime>();
    let busy = rt.as_ref().map(|rt| rt.busy_chats()).unwrap_or_default();
    let modes = |db: &helm_core::db::Database| -> Vec<(String, workmode::Mode)> {
        db.snapshot(|tx| {
            Ok::<_, rusqlite::Error>(
                busy.iter()
                    .map(|c| (c.clone(), workmode::effective(tx, c).mode))
                    .collect(),
            )
        })
        .unwrap_or_default()
    };
    let before = modes(&db);
    let now = helm_core::now_ms();
    let known = db
        .write(|tx| match board {
            None => workmode::set_app(tx, mode).map(|()| true),
            Some(b) => workmode::set_board(tx, b, mode, now),
        })
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if !known {
        return Err(ApiError::new("not_found", "That is not a board."));
    }
    if let Some(rt) = rt {
        for ((chat, was), (_, now_mode)) in before.iter().zip(modes(&db)) {
            if *was != now_mode {
                let _ = helm_chats::submit::host_send(
                    &rt,
                    helm_chats::submit::HostSend {
                        chat_id: chat,
                        submission_id: &uuid::Uuid::new_v4().to_string(),
                        text: &workmode::switch_line(now_mode),
                        origin: helm_chats::types::SubmissionOrigin::App,
                        page: None,
                    },
                );
            }
        }
    }
    ctx.events.emit(
        helm_ipc::AppEvent::new("workmode")
            .with("scope", a.scope.clone())
            .with("boardId", board.unwrap_or_default().to_owned()),
    );
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModeGetArgs {
    #[serde(default)]
    chat_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModeSetArgs {
    scope: String,
    #[serde(default)]
    board_id: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
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
