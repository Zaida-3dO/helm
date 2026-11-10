//! Chats that run on another device.
//!
//! This device keeps a *mirror* of the chats other devices of the household run: their list, and
//! the transcript of the ones opened here (the `devices` crate owns the tables and the traffic). A
//! mirror chat is never in the `chats` table, so nothing local (the worker, triage, schedules,
//! search, backup) ever sees it. What the window asks of such a chat is answered here instead:
//!
//! | op | answered by |
//! |---|---|
//! | `chat_get`, `transcript_page` | the mirror (and the device that runs the chat, for older pages) |
//! | `chat_send`, `chat_stop`, `interaction_answer`, `chat_set_model`, `chat_rename`, `chat_archive`, `chat_restore` | the device that runs the chat |
//! | `draft_save` | the mirror alone: a draft never leaves this PC |
//! | `chat_warm`, `title_suggest`, `autocomplete_suggest`, `skills_list` | quietly, with an empty answer |
//! | `model_catalogue` with a `deviceUid` | the models that device last said it offers |
//! | any other op that names a mirror chat | refused: "This chat runs on {device}. Do that there." |
//!
//! [`Routes`] is the check the dispatcher makes before it runs an op; the devices crate supplies
//! the [`RemoteChats`] it asks, through the [`RemoteSlot`] service.

use crate::types::{
    ChatRenameArgs, ChatSendArgs, ChatSetModelArgs, DraftSaveArgs, InteractionAnswerArgs,
    TranscriptPageArgs,
};
use helm_ipc::{ApiError, BoxFuture, OpCtx, OpInterceptor};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

pub type Fut<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type Res = Result<Value, ApiError>;

/// What the op layer needs from the mirror of other devices' chats. Every answer has the shape
/// the matching local op gives, so the window cannot tell a mirror chat from a local one.
pub trait RemoteChats: Send + Sync {
    /// The device that runs the chat, when the chat is a mirror chat. Quick: no database read.
    fn owns(&self, chat_id: &str) -> Option<String>;
    /// This device's id.
    fn this_device(&self) -> String;
    /// A device's name for a sentence, or its id when it is not known.
    fn device_name(&self, device_uid: &str) -> String;
    /// `{chat, thread}` as `chat_get` answers.
    fn get(&self, chat_id: String) -> Fut<Res>;
    /// A transcript page as `transcript_page` answers.
    fn page(&self, args: TranscriptPageArgs) -> Fut<Res>;
    /// `{submission, chat}` as `chat_send` answers.
    fn send(&self, args: ChatSendArgs) -> Fut<Res>;
    fn stop(&self, chat_id: String) -> Fut<Res>;
    fn answer(&self, args: InteractionAnswerArgs) -> Fut<Res>;
    /// The updated chat record, for each of the three that change one.
    fn set_model(&self, args: ChatSetModelArgs) -> Fut<Res>;
    fn rename(&self, chat_id: String, title: String) -> Fut<Res>;
    fn archive(&self, chat_id: String, archived: bool) -> Fut<Res>;
    /// Keep a draft on this PC only.
    fn draft_save(&self, args: DraftSaveArgs) -> Fut<Res>;
    /// The model catalogue another device last reported.
    fn catalogue(&self, device_uid: String) -> Fut<Res>;
}

/// Where the devices crate hands the app its [`RemoteChats`] once the hub line exists.
#[derive(Default)]
pub struct RemoteSlot(OnceLock<Arc<dyn RemoteChats>>);

impl RemoteSlot {
    pub fn attach(&self, remote: Arc<dyn RemoteChats>) {
        let _ = self.0.set(remote);
    }

    pub fn get(&self) -> Option<Arc<dyn RemoteChats>> {
        self.0.get().cloned()
    }
}

/// The dispatcher's check: a call that names a mirror chat is answered here.
pub struct Routes;

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, ApiError> {
    crate::ops::parse_args(args)
}

impl OpInterceptor for Routes {
    fn intercept(&self, ctx: &OpCtx, op: &str, args: &Value) -> Option<BoxFuture> {
        // The device ops read this PC's copy of other devices' chats themselves.
        if op.starts_with("devices_") {
            return None;
        }
        let remote = ctx.services.get::<RemoteSlot>()?.get()?;
        if op == "model_catalogue" {
            let uid = args.get("deviceUid").and_then(Value::as_str)?;
            if uid == remote.this_device() {
                return None;
            }
            let uid = uid.to_owned();
            return Some(Box::pin(async move { remote.catalogue(uid).await }));
        }
        let chat_id = args.get("chatId").and_then(Value::as_str)?;
        let device = remote.owns(chat_id)?;
        let chat_id = chat_id.to_owned();
        let op = op.to_owned();
        let args = args.clone();
        Some(Box::pin(async move {
            route(remote, device, op, chat_id, args).await
        }))
    }
}

async fn route(
    remote: Arc<dyn RemoteChats>,
    device: String,
    op: String,
    chat_id: String,
    args: Value,
) -> Res {
    match op.as_str() {
        "chat_get" => remote.get(chat_id).await,
        "transcript_page" => remote.page(parse(args)?).await,
        "chat_send" => remote.send(parse(args)?).await,
        "chat_stop" => remote.stop(chat_id).await,
        "interaction_answer" => remote.answer(parse(args)?).await,
        "chat_set_model" => remote.set_model(parse(args)?).await,
        "chat_rename" => {
            let a: ChatRenameArgs = parse(args)?;
            remote.rename(a.chat_id, a.title).await
        }
        "chat_archive" => remote.archive(chat_id, true).await,
        "chat_restore" => remote.archive(chat_id, false).await,
        "draft_save" => remote.draft_save(parse(args)?).await,
        // Nothing to warm, suggest or list for a chat that runs somewhere else.
        "chat_warm" => Ok(Value::Null),
        "title_suggest" => Ok(json!({ "title": "", "titles": [] })),
        "autocomplete_suggest" => Ok(json!({ "completion": "", "retryAfterMs": null })),
        "skills_list" => Ok(json!([])),
        _ => Err(elsewhere(&remote.device_name(&device))),
    }
}

/// The refusal for an op that only the device running the chat can do.
pub fn elsewhere(device_name: &str) -> ApiError {
    ApiError::new(
        "refused",
        format!("This chat runs on {device_name}. Do that there."),
    )
}
