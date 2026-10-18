//! The chat `api` ops.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `chat_create` | maintenance | [`ChatCreateArgs`] → [`ChatRecord`] |
//! | `chat_list` | blocking | `{}` → `ChatRecord[]` |
//! | `chat_get` | blocking | [`ChatIdArgs`] → [`ChatGetResult`] |
//! | `transcript_page` | heavy | [`TranscriptPageArgs`] → [`TranscriptPage`] |
//! | `chat_send` | maintenance | [`ChatSendArgs`] → [`ChatSendResult`] |
//! | `chat_stop` | async | [`ChatStopArgs`] → [`ChatStopResult`] |
//! | `queue_resume` | maintenance | [`ChatIdArgs`] → `null` |
//! | `submission_action` | maintenance | [`SubmissionActionArgs`] → [`SubmissionActionResult`] |
//! | `interaction_answer` | async | [`InteractionAnswerArgs`] → `null` |
//! | `draft_save` | blocking | [`DraftSaveArgs`] → [`DraftSaveResult`] |
//! | `chat_set_model` | maintenance | [`ChatSetModelArgs`] → `ChatRecord` |
//! | `model_catalogue` | blocking | [`ModelCatalogueArgs`] → [`ModelCatalogue`] |
//! | `tool_stop` | async | [`ToolStopArgs`] → [`ToolStopResult`] |
//! | `chat_compact` | maintenance | [`ChatIdArgs`] → `null` |
//! | `attention_get` | blocking | `{}` → [`AttentionSummary`] |
//!
//! Argument parsing is strict (an unknown field is `bad_args`). Ops that touch a running chat go
//! through the [`ChatRuntime`] service.

use crate::actor::Cmd;
use crate::clock::Clock;
use crate::runtime::ChatRuntime;
use crate::types::*;
use crate::{catalogue, send_behavior, store, submit};
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("chat_create", OpClass::Maintenance, chat_create)
        .blocking("chat_list", OpClass::Blocking, chat_list)
        .blocking("chat_get", OpClass::Blocking, chat_get)
        .blocking("transcript_page", OpClass::Heavy, transcript_page)
        .blocking("chat_send", OpClass::Maintenance, chat_send)
        .async_op("chat_stop", chat_stop)
        .blocking("queue_resume", OpClass::Maintenance, queue_resume)
        .blocking("submission_action", OpClass::Maintenance, submission_action)
        .async_op("interaction_answer", interaction_answer)
        .blocking("draft_save", OpClass::Blocking, draft_save)
        .blocking("chat_set_model", OpClass::Maintenance, chat_set_model)
        .blocking("model_catalogue", OpClass::Blocking, model_catalogue)
        .async_op("tool_stop", tool_stop)
        .blocking("chat_compact", OpClass::Maintenance, chat_compact)
        .blocking("attention_get", OpClass::Blocking, attention_get)
        .blocking("chat_rename", OpClass::Maintenance, chat_rename)
        .async_op("title_suggest", title_suggest)
        .blocking("skills_list", OpClass::Blocking, skills_list)
        .async_op("autocomplete_suggest", autocomplete_suggest)
        // The run-slot queue, main window only like every chats op.
        .blocking("resource_queue_snapshot", OpClass::Blocking, |ctx, _| {
            to_value(&crate::runslots::view(&runtime(ctx)?)?)
        })
        .blocking(
            "resource_queue_start_now",
            OpClass::Maintenance,
            |ctx, args| {
                let (chat, sid) = (
                    str_field(&args, "chatId")?,
                    str_field(&args, "submissionId")?,
                );
                crate::runslots::start_now(&runtime(ctx)?, &chat, &sid)?;
                to_value(&crate::runslots::view(&runtime(ctx)?)?)
            },
        )
        .blocking(
            "resource_queue_cancel",
            OpClass::Maintenance,
            |ctx, args| {
                crate::runslots::cancel(&runtime(ctx)?, &str_field(&args, "submissionId")?)?;
                to_value(&crate::runslots::view(&runtime(ctx)?)?)
            },
        )
        .blocking("run_slots_settings", OpClass::Maintenance, |ctx, args| {
            let rt = runtime(ctx)?;
            match args.get("settings") {
                Some(v) => to_value(&crate::runslots::save_settings(&rt, v)?),
                None => to_value(&crate::runslots::view(&rt)?),
            }
        });
    crate::manage::register(reg);
}

fn str_field(args: &Value, k: &str) -> Result<String, ApiError> {
    args.get(k)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_args(format!("{k} is required.")))
}

/// `skills_list`: what the chat's worker reported; empty until it has run once.
fn skills_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    to_value(&runtime(ctx)?.skills(&a.chat_id))
}

async fn autocomplete_suggest(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: AutocompleteArgs = parse_args(args)?;
    let rt = runtime(&ctx)?;
    to_value(&crate::autocomplete::suggest(&rt, a).await?)
}

fn chat_rename(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatRenameArgs = parse_args(args)?;
    to_value(&crate::titles::rename(
        &runtime(ctx)?,
        &a.chat_id,
        &a.title,
    )?)
}

async fn title_suggest(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    let rt = runtime(&ctx)?;
    to_value(&TitleSuggestResult {
        title: crate::titles::suggest(&rt, &a.chat_id).await?,
    })
}

/// `attention_get`: the summary as it stands. Without a runtime (tests) it is
/// computed from the database alone.
fn attention_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    parse_args::<EmptyArgs>(args)?;
    if let Some(rt) = ctx.services.get::<ChatRuntime>() {
        return to_value(&rt.attention().map_err(sql)?);
    }
    let db = ctx.services.need::<Database>()?;
    let rev = ctx.services.need::<Clock>()?.current();
    to_value(
        &db.snapshot(|tx| crate::attention::compute(tx, &Default::default(), rev))
            .map_err(sql)?,
    )
}

/// `{}` or `null`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

/// Parse op arguments strictly; `null` counts as `{}`.
pub fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ApiError> {
    let args = if args.is_null() {
        Value::Object(Default::default())
    } else {
        args
    };
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn runtime(ctx: &OpCtx) -> Result<ChatRuntime, ApiError> {
    Ok((*ctx.services.need::<ChatRuntime>()?).clone())
}

fn chat_send(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatSendArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let archived_project = db
        .snapshot(|tx| {
            let p: Option<String> = rusqlite::OptionalExtension::optional(tx.query_row(
                "SELECT project_id FROM chats WHERE id = ?1",
                [&a.chat_id],
                |r| r.get(0),
            ))?;
            match p {
                Some(p) => crate::manage::project_archived(ctx, tx, &p),
                None => Ok(false),
            }
        })
        .map_err(sql)?;
    if archived_project {
        return Err(ApiError::new(
            "refused",
            "This chat's project is archived; your message is kept in the draft.",
        ));
    }
    let builder = ctx.services.get::<crate::seams::SendContext>();
    to_value(&submit::chat_send(&runtime(ctx)?, a, builder.as_deref())?)
}

async fn chat_stop(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatStopArgs = parse_args(args)?;
    let rt = runtime(&ctx)?;
    let status = rt
        .ask(
            &a.chat_id,
            |reply| Cmd::Stop {
                continue_queue: a.continue_queue,
                reply,
            },
            rt.limits().stop + std::time::Duration::from_secs(5),
        )
        .await?;
    to_value(&ChatStopResult { status })
}

fn queue_resume(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    submit::queue_resume(&runtime(ctx)?, &a.chat_id)?;
    Ok(Value::Null)
}

fn submission_action(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SubmissionActionArgs = parse_args(args)?;
    to_value(&submit::submission_action(&runtime(ctx)?, a)?)
}

async fn interaction_answer(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: InteractionAnswerArgs = parse_args(args)?;
    let rt = runtime(&ctx)?;
    let mut m = serde_json::Map::new();
    m.insert("id".into(), Value::String(a.request_id));
    if let Some(v) = a.value {
        m.insert("value".into(), Value::String(v));
    }
    if let Some(v) = a.confirmed {
        m.insert("confirmed".into(), Value::Bool(v));
    }
    if let Some(v) = a.cancelled {
        m.insert("cancelled".into(), Value::Bool(v));
    }
    if let Some(v) = a.goto {
        m.insert("goto".into(), Value::from(v));
    }
    rt.command(&a.chat_id, Cmd::Answer(m))?;
    Ok(Value::Null)
}

fn chat_set_model(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatSetModelArgs = parse_args(args)?;
    to_value(&submit::chat_set_model(&runtime(ctx)?, a)?)
}

async fn tool_stop(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ToolStopArgs = parse_args(args)?;
    let rt = runtime(&ctx)?;
    let caps = rt
        .live_thread(&a.chat_id)
        .map(|t| t.capabilities)
        .unwrap_or_default();
    if !caps.stop_tool {
        return Err(ApiError::new(
            "unsupported",
            "This chat cannot stop a single tool call.",
        ));
    }
    let r = rt
        .ask(
            &a.chat_id,
            |reply| Cmd::ToolStop {
                tool_call_id: a.tool_call_id.clone(),
                reply,
            },
            std::time::Duration::from_secs(15),
        )
        .await?;
    match r {
        Ok(stopped) => to_value(&ToolStopResult { stopped }),
        Err(e) => Err(ApiError::new("refused", e)),
    }
}

fn chat_compact(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    let rt = runtime(ctx)?;
    let r = rt.handle().block_on(rt.ask(
        &a.chat_id,
        |reply| Cmd::Compact { reply },
        std::time::Duration::from_secs(30),
    ))?;
    r.map_err(|e| ApiError::new("refused", e))?;
    Ok(Value::Null)
}

pub(crate) fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

pub(crate) fn not_found() -> ApiError {
    ApiError::new("not_found", "That chat does not exist.")
}

fn chat_create(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatCreateArgs = parse_args(args)?;
    let cwd = a.cwd.trim();
    let path = std::path::Path::new(cwd);
    if cwd.is_empty() || !path.is_absolute() {
        return Err(ApiError::bad_args("The folder must be an absolute path."));
    }
    if !path.is_dir() {
        return Err(ApiError::bad_args("That folder does not exist."));
    }
    if let Some(e) = a.effort.as_deref() {
        if !catalogue::is_effort(e) {
            return Err(ApiError::bad_args(
                "Effort must be low, medium, high, xhigh or max.",
            ));
        }
    }
    let effort = a.effort.unwrap_or_else(|| catalogue::DEFAULT_EFFORT.into());
    let db = ctx.services.need::<Database>()?;
    let clock = ctx.services.need::<Clock>()?;
    let id = db
        .write(|tx| {
            let project = store::upsert_folder_project(tx, cwd, helm_core::now_ms())?;
            if crate::manage::project_archived(ctx, tx, &project)? {
                return Ok(None);
            }
            store::create_chat(
                tx,
                cwd,
                a.model.as_deref(),
                Some(&effort),
                helm_core::now_ms(),
            )
            .map(Some)
        })
        .map_err(sql)?
        .ok_or_else(|| {
            ApiError::new(
                "refused",
                "This folder's project is archived; restore it to start new chats.",
            )
        })?;
    let rev = clock.next();
    let chat = db
        .snapshot(|tx| store::get_chat(tx, &id, rev))
        .map_err(sql)?
        .ok_or_else(not_found)?;
    ctx.events
        .emit(ChatEvent::Chat { chat: chat.clone() }.to_app_event());
    to_value(&chat)
}

fn chat_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    parse_args::<EmptyArgs>(args)?;
    let db = ctx.services.need::<Database>()?;
    let rev = ctx.services.need::<Clock>()?.current();
    to_value(&db.snapshot(|tx| store::list_chats(tx, rev)).map_err(sql)?)
}

/// The thread state as stored: what a chat with no live worker shows.
pub fn stored_thread(
    tx: &rusqlite::Transaction<'_>,
    chat: &ChatRecord,
    revision: i64,
) -> rusqlite::Result<ThreadState> {
    let held: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_queue_holds WHERE chat_id = ?1)",
        [&chat.id],
        |r| r.get(0),
    )?;
    let sb = send_behavior::resolve(helm_core::prefs::read(tx, "sendBehavior").as_ref());
    Ok(ThreadState {
        chat_id: chat.id.clone(),
        status: chat.status,
        permission_mode: None,
        interactions: Vec::new(),
        live_tools: Vec::new(),
        background_tasks: Vec::new(),
        compacting: None,
        capabilities: Capabilities::default(),
        send_behavior: sb,
        queue_held: held,
        revision,
    })
}

fn chat_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let rev = ctx.services.need::<Clock>()?.current();
    let out = db
        .snapshot(|tx| {
            let Some(chat) = store::get_chat(tx, &a.chat_id, rev)? else {
                return Ok(None);
            };
            let thread = stored_thread(tx, &chat, rev)?;
            Ok::<_, rusqlite::Error>(Some(ChatGetResult { chat, thread }))
        })
        .map_err(sql)?
        .ok_or_else(not_found)?;
    let mut out = out;
    // A chat with an actor shows its live thread (interactions, tools, capabilities).
    if let Some(rt) = ctx.services.get::<ChatRuntime>() {
        if let Some(mut live) = rt.live_thread(&a.chat_id) {
            live.revision = rev;
            live.status = out.chat.status;
            out.thread = live;
        }
    }
    to_value(&out)
}

fn transcript_page(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: TranscriptPageArgs = parse_args(args)?;
    let set = [a.before.is_some(), a.after.is_some(), a.around.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if set > 1 {
        return Err(ApiError::bad_args(
            "Give at most one of before, after and around.",
        ));
    }
    let db = ctx.services.need::<Database>()?;
    let rev = ctx.services.need::<Clock>()?.current();
    let page = db
        .snapshot(|tx| store::transcript_page(tx, &a, rev))
        .map_err(sql)?
        .ok_or_else(not_found)?;
    to_value(&page)
}

fn draft_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: DraftSaveArgs = parse_args(args)?;
    if a.text.len() > store::MAX_SUBMISSION_BYTES {
        return Err(ApiError::bad_args("The draft is larger than 2 MB."));
    }
    let db = ctx.services.need::<Database>()?;
    let clock = ctx.services.need::<Clock>()?;
    let outcome = db
        .write(|tx| store::save_draft(tx, &a.chat_id, &a.text, a.revision, helm_core::now_ms()))
        .map_err(sql)?;
    match outcome {
        store::DraftOutcome::Saved(revision) => {
            let rev = clock.next();
            if let Some(chat) = db
                .snapshot(|tx| store::get_chat(tx, &a.chat_id, rev))
                .map_err(sql)?
            {
                ctx.events.emit(ChatEvent::Chat { chat }.to_app_event());
            }
            to_value(&DraftSaveResult { revision })
        }
        store::DraftOutcome::Stale { stored } => Err(ApiError::new(
            "stale",
            format!("A newer draft is already saved (revision {stored})."),
        )),
        store::DraftOutcome::NotFound => Err(not_found()),
    }
}

fn model_catalogue(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ModelCatalogueArgs = parse_args(args)?;
    let profile = ctx.services.need::<Profile>()?;
    if a.refresh == Some(true) {
        // Re-read from a live worker's get_state when one runs.
        if let Some(rt) = ctx.services.get::<ChatRuntime>() {
            rt.handle().block_on(rt.refresh_catalogue());
        }
    }
    to_value(&catalogue::load(&profile.path("")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use helm_ipc::{Caller, Dispatcher, RecordingSink, Services};
    use serde_json::json;
    use std::sync::Arc;

    pub struct Harness {
        pub _dir: tempfile::TempDir,
        pub api: Dispatcher,
        pub sink: Arc<RecordingSink>,
        pub work: std::path::PathBuf,
        pub db: Arc<Database>,
    }

    pub fn harness() -> Harness {
        harness_with(|_| {})
    }

    pub fn harness_with(extra: impl FnOnce(&mut Services)) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::open(&dir.path().join("prof")).unwrap();
        let reg = crate::schema::tests::registry();
        let db = Arc::new(Database::open(&profile.db_path(), &reg).unwrap());
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let mut s = Services::new();
        s.insert(Arc::new(profile));
        s.insert(db.clone());
        s.insert(Arc::new(Clock::new(0)));
        extra(&mut s);
        let sink = Arc::new(RecordingSink::default());
        let mut ops = OpRegistry::new();
        register_ops(&mut ops);
        let ctx = OpCtx {
            services: Arc::new(s),
            events: sink.clone(),
        };
        Harness {
            _dir: dir,
            api: Dispatcher::new(ops, ctx, false),
            sink,
            work,
            db,
        }
    }

    pub fn main() -> Caller {
        Caller {
            window_label: "main".into(),
            url: "http://tauri.localhost/".into(),
        }
    }

    #[test]
    fn every_contract_op_is_registered_in_its_class() {
        let mut reg = OpRegistry::new();
        register_ops(&mut reg);
        let expected = [
            ("chat_create", OpClass::Maintenance),
            ("chat_list", OpClass::Blocking),
            ("chat_get", OpClass::Blocking),
            ("transcript_page", OpClass::Heavy),
            ("chat_send", OpClass::Maintenance),
            ("chat_stop", OpClass::Async),
            ("queue_resume", OpClass::Maintenance),
            ("submission_action", OpClass::Maintenance),
            ("interaction_answer", OpClass::Async),
            ("draft_save", OpClass::Blocking),
            ("chat_set_model", OpClass::Maintenance),
            ("model_catalogue", OpClass::Blocking),
            ("tool_stop", OpClass::Async),
            ("chat_compact", OpClass::Maintenance),
            ("attention_get", OpClass::Blocking),
            ("chat_rename", OpClass::Maintenance),
            ("title_suggest", OpClass::Async),
            ("skills_list", OpClass::Blocking),
            ("autocomplete_suggest", OpClass::Async),
            ("resource_queue_snapshot", OpClass::Blocking),
            ("resource_queue_start_now", OpClass::Maintenance),
            ("resource_queue_cancel", OpClass::Maintenance),
            ("run_slots_settings", OpClass::Maintenance),
            ("chat_archive", OpClass::Maintenance),
            ("chat_restore", OpClass::Maintenance),
            ("chat_delete", OpClass::Maintenance),
            ("unread_mark", OpClass::Maintenance),
            ("unread_clear", OpClass::Maintenance),
            ("completion_ack", OpClass::Maintenance),
            ("chat_fork", OpClass::Maintenance),
            ("message_edit_open", OpClass::Blocking),
            ("message_resend", OpClass::Maintenance),
            ("attachment_put", OpClass::Maintenance),
            ("side_chat_resolve", OpClass::Maintenance),
            ("chat_mute", OpClass::Maintenance),
            ("attention_address", OpClass::Maintenance),
            ("attention_snooze", OpClass::Maintenance),
        ];
        for (name, class) in expected {
            assert_eq!(reg.class_of(name), Some(class), "{name}");
        }
        assert_eq!(reg.names().count(), expected.len());
    }

    #[tokio::test]
    async fn create_get_list_and_page_a_chat() {
        let h = harness();
        let bad = h
            .api
            .call(&main(), "chat_create", json!({"cwd": "relative/path"}))
            .await
            .unwrap_err();
        assert_eq!(bad.code, "bad_args");
        let chat: ChatRecord = serde_json::from_value(
            h.api
                .call(
                    &main(),
                    "chat_create",
                    json!({"cwd": h.work.to_string_lossy()}),
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(chat.status, ChatStatus::Idle);
        assert_eq!(
            chat.effort.as_deref(),
            Some("xhigh"),
            "new chats default to xhigh"
        );
        assert_eq!(h.sink.kinds(), ["chat"]);
        let got: ChatGetResult = serde_json::from_value(
            h.api
                .call(&main(), "chat_get", json!({"chatId": chat.id}))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(got.thread.send_behavior, send_behavior::HOME_DEFAULTS);
        assert!(got.chat.revision >= chat.revision);
        let list = h.api.call(&main(), "chat_list", json!({})).await.unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        let page: TranscriptPage = serde_json::from_value(
            h.api
                .call(&main(), "transcript_page", json!({"chatId": chat.id}))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(page.messages.is_empty() && !page.has_older);
        let missing = h
            .api
            .call(&main(), "chat_get", json!({"chatId": "nope"}))
            .await
            .unwrap_err();
        assert_eq!(missing.code, "not_found");
    }

    #[tokio::test]
    async fn stale_drafts_are_refused_with_code_stale() {
        let h = harness();
        let chat = h
            .api
            .call(
                &main(),
                "chat_create",
                json!({"cwd": h.work.to_string_lossy()}),
            )
            .await
            .unwrap();
        let id = chat["id"].as_str().unwrap();
        let ok = h
            .api
            .call(
                &main(),
                "draft_save",
                json!({"chatId": id, "text": "hi", "revision": 50}),
            )
            .await
            .unwrap();
        assert_eq!(ok["revision"], 50);
        let stale = h
            .api
            .call(
                &main(),
                "draft_save",
                json!({"chatId": id, "text": "old", "revision": 50}),
            )
            .await
            .unwrap_err();
        assert_eq!(stale.code, "stale");
    }

    #[tokio::test]
    async fn the_catalogue_falls_back_with_xhigh() {
        let h = harness();
        let c: ModelCatalogue = serde_json::from_value(
            h.api
                .call(&main(), "model_catalogue", json!({"harness": "claude"}))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(c.source, CatalogueSource::Fallback);
        assert_eq!(c.default_effort, "xhigh");
    }
}
