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
//! | `chat_fork`, `chat_compact`, `chat_roll_over`, `usage_cost`, `chat_export_markdown`, `chat_organize`, `unread_mark`, `chat_mute`, `chat_set_permission_mode`, `chat_verbosity_get`/`set`, `chat_warm`, `chat_stop_no_resend`, `tool_stop`, `checkpoint_info`, `attention_snooze`/`address`, `panel_context_get`, `quick_open`, `chat_folder`, `file_read`, `dir_list`, `file_stat`, `archive_list`, `path_resolve`, `changes`, `diff`, `changes_stage`, `changes_unstage` | the same op on the device that runs the chat (panel paths stay inside the chat's project folder there) |
//! | `shown_gallery_get`, `shown_image_get`, `shown_image_thumb`, `option_board_state`, `option_board_answer`, `attachment_get`, `local_image_get` (with the `chatId`) | the device that runs the chat; full-size pictures and a reply's picture inside the chat's project folder come across in chunks |
//! | `message_output_save` | the output comes across in chunks and is saved on this PC, where the person chose |
//! | `draft_save` | the mirror alone: a draft never leaves this PC |
//! | `title_suggest`, `autocomplete_suggest`, `skills_list` | quietly, with an empty answer |
//! | `model_catalogue` with a `deviceUid` | the models that device last said it offers |
//! | any other op that names a mirror chat | refused: "This chat runs on {device}. Do that there." |
//!
//! [`Routes`] is the check the dispatcher makes before it runs an op; the devices crate supplies
//! the [`RemoteChats`] it asks, through the [`RemoteSlot`] service.

use crate::output_save::FileSaver;
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
    /// The same send, as the named hub person instead of the person the sidebar shows: a call
    /// from a chat or from outside acts as its own person. `None` is [`Self::send`].
    fn send_as(&self, args: ChatSendArgs, person: Option<String>) -> Fut<Res> {
        let _ = person;
        self.send(args)
    }
    fn stop(&self, chat_id: String) -> Fut<Res>;
    fn answer(&self, args: InteractionAnswerArgs) -> Fut<Res>;
    /// The updated chat record, for each of the three that change one.
    fn set_model(&self, args: ChatSetModelArgs) -> Fut<Res>;
    fn rename(&self, chat_id: String, title: String) -> Fut<Res>;
    fn archive(&self, chat_id: String, archived: bool) -> Fut<Res>;
    /// Keep a draft on this PC only.
    fn draft_save(&self, args: DraftSaveArgs) -> Fut<Res>;
    /// Carry a local op across the wire to the device that runs the chat, answering with what it
    /// answers. `args` already names the chat; `op` is one of the ops the chat menu runs.
    fn chat_op(&self, op: &str, args: Value) -> Fut<Res>;
    /// The model catalogue another device last reported.
    fn catalogue(&self, device_uid: String) -> Fut<Res>;
    /// Something big of the chat, brought across whole in chunks. `r` names it as the device's
    /// `blob_read` does: `{output}` (a tool's whole output) or `{file}` (a path inside the
    /// chat's project folder there).
    fn blob(&self, chat_id: String, r: Value) -> Fut<Result<RemoteBlob, ApiError>> {
        let _ = (chat_id, r);
        Box::pin(async {
            Err(ApiError::new(
                "refused",
                "That can't be brought across from the other PC.",
            ))
        })
    }
}

/// Something big of a chat that runs on another device, brought across whole.
pub struct RemoteBlob {
    pub mime: String,
    /// A file name to suggest when it is saved.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Save bytes brought from another PC on this one, where the person chooses in the save dialog:
/// `{path}`, or `{path: null}` when the dialog is cancelled.
pub async fn save_here(saver: Arc<FileSaver>, name: String, bytes: Vec<u8>) -> Res {
    tokio::task::spawn_blocking(move || {
        let chosen = (saver.0)(&name).map_err(|e| ApiError::new("unavailable", e))?;
        let Some(path) = chosen else {
            return Ok(json!({ "path": null }));
        };
        helm_core::privfs::write_atomic(&path, &bytes)
            .map_err(|e| ApiError::new("internal", format!("Could not save the file: {e}")))?;
        Ok(json!({ "path": path.to_string_lossy() }))
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
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
        let args = args.clone();
        // A tool's output is read there and saved here, where the person chose.
        if op == "message_output_save" {
            let saver = ctx.services.get::<FileSaver>();
            return Some(Box::pin(async move {
                save_output(remote, saver, chat_id, args).await
            }));
        }
        let op = op.to_owned();
        Some(Box::pin(async move {
            route(remote, device, op, chat_id, args).await
        }))
    }
}

/// `message_output_save` for a chat that runs on another device.
async fn save_output(
    remote: Arc<dyn RemoteChats>,
    saver: Option<Arc<FileSaver>>,
    chat_id: String,
    args: Value,
) -> Res {
    let message_id = args
        .get("messageId")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::bad_args("messageId is required."))?;
    let saver =
        saver.ok_or_else(|| ApiError::new("unavailable", "Saving is not available here."))?;
    let blob = remote
        .blob(chat_id, json!({ "output": message_id }))
        .await?;
    save_here(saver, blob.name, blob.bytes).await
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
        // The chat menu's other actions: the same op, run on the device that runs the chat.
        "chat_fork"
        | "chat_compact"
        | "chat_roll_over"
        | "usage_cost"
        | "chat_export_markdown"
        | "chat_organize"
        | "unread_mark"
        | "chat_mute"
        // The composer's picks and the chat view's actions, applied where the chat runs.
        | "chat_set_permission_mode"
        | "chat_verbosity_get"
        | "chat_verbosity_set"
        | "panel_context_get"
        | "quick_open"
        | "tool_stop"
        | "checkpoint_info"
        | "attention_snooze"
        | "attention_address"
        | "chat_warm"
        | "chat_stop_no_resend"
        // What an agent showed in the chat, and the pictures a reply names: read there.
        | "shown_gallery_get"
        | "shown_image_get"
        | "shown_image_thumb"
        | "option_board_state"
        | "option_board_answer"
        | "local_image_get"
        // The side panel around a chat: the same ops, run on the device that runs the chat,
        // kept inside that chat's project folder there.
        | "chat_folder"
        | "attachment_get"
        | "file_read"
        | "dir_list"
        | "file_stat"
        | "archive_list"
        | "path_resolve"
        | "changes"
        | "diff"
        | "changes_stage"
        | "changes_unstage" => remote.chat_op(&op, args).await,
        // Nothing to suggest or list for a chat that runs somewhere else.
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
