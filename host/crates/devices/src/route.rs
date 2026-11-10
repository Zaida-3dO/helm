//! What the window's chat ops do for a chat that runs on another device.
//!
//! [`Mirror`] answers [`RemoteChats`]: reads come from this PC's copy (older pages from the device
//! itself while it is up), changes go to the device through the hub line, and a change is refused
//! at once, in words, when the device cannot take it:
//!
//! | state | when | the person is told |
//! |---|---|---|
//! | reconnecting | this PC's line to the hub is down | "Reconnecting to the hub… showing the copy on this PC." |
//! | offline | the hub says the device is not up | "{device} is offline (last seen {time}). You can read this chat; sending is off until it's back." |
//! | read-only | the device's switch "Let other devices use chats on this PC" is off | "{device} doesn't let other devices run its chats. You can read this one." |
//! | unconfirmed | the line gave no answer to a request that went out | "{device} didn't confirm this message. It may or may not have arrived." |
//!
//! An unconfirmed send is never sent again by this code: the person decides. Errors carry the
//! codes `reconnecting`, `offline`, `not_allowed` and `unconfirmed`.

use crate::mirror::{chat_value, local_page, Mirror, PAGE_ROWS, UNCONFIRMED};
use crate::serve::confirm_options;
use crate::wire::{code, WireError};
use helm_chats::remote::{Fut, RemoteChats, Res};
use helm_chats::types::{
    ChatSendArgs, ChatSetModelArgs, DraftSaveArgs, Interaction, InteractionAnswerArgs,
    InteractionMethod, SendAction, SendOrigin, TranscriptPageArgs,
};
use helm_ipc::ApiError;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

/// The most a draft may hold, as for a chat that runs here.
const MAX_DRAFT_BYTES: usize = 2 * 1024 * 1024;

fn db_err(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn not_found() -> ApiError {
    ApiError::new("not_found", "That chat does not exist.")
}

/// "2026-11-10T14:02:11Z" as "2026-11-10 14:02 UTC".
fn seen_text(iso: Option<&str>) -> String {
    match iso {
        Some(s) if s.len() >= 16 && s.is_char_boundary(10) && s.is_char_boundary(16) => {
            format!("{} {} UTC", &s[..10], &s[11..16])
        }
        _ => "never".to_owned(),
    }
}

impl Mirror {
    fn chat_json(&self, chat_id: &str) -> Result<Value, ApiError> {
        self.inner
            .db
            .snapshot(|tx| chat_value(tx, chat_id))
            .map_err(db_err)?
            .ok_or_else(not_found)
    }

    fn device_of(&self, chat_id: &str) -> Result<String, ApiError> {
        self.owner(chat_id).ok_or_else(not_found)
    }

    /// The device must be up, this PC's line too, and (for anything that runs, changes or
    /// answers) the device's switch on.
    fn check_live(&self, device: &str) -> Result<(), ApiError> {
        if !self.line_is_up() {
            return Err(ApiError::new(
                "reconnecting",
                "Reconnecting to the hub… showing the copy on this PC.",
            ));
        }
        let name = self.device_name(device);
        match self.device(device) {
            Some(d) if d.online => {
                if d.run {
                    Ok(())
                } else {
                    Err(not_allowed(&name))
                }
            }
            other => Err(offline(
                &name,
                other.and_then(|d| d.last_seen_at).as_deref(),
            )),
        }
    }

    /// Start a chat on `device` for the person, then open it here.
    pub async fn create_chat(
        &self,
        device: &str,
        person_id: &str,
        project_id: &str,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<String, ApiError> {
        self.check_live(device)?;
        let mut args = json!({ "personId": person_id, "projectId": project_id });
        if let Some(m) = model {
            args["model"] = json!(m);
        }
        if let Some(e) = effort {
            args["effort"] = json!(e);
        }
        let made = self.ask(device, "create", args, "the new chat").await?;
        let id = made
            .get("chatId")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::internal("The new chat has no id."))?
            .to_owned();
        if let Err(e) = self.open_new_chat(device, &id).await {
            log::info!("devices: the new chat was not opened: {e}");
        }
        Ok(id)
    }

    /// A failed request to the device, in the person's words.
    fn failed(&self, device: &str, e: WireError, what: &str) -> ApiError {
        let name = self.device_name(device);
        match e.code.as_str() {
            code::OFFLINE if e.message.contains("not connected to the hub") => ApiError::new(
                "reconnecting",
                "Reconnecting to the hub… showing the copy on this PC.",
            ),
            code::OFFLINE => offline(
                &name,
                self.device(device).and_then(|d| d.last_seen_at).as_deref(),
            ),
            code::NOT_ALLOWED => not_allowed(&name),
            code::TIMEOUT | UNCONFIRMED => ApiError::new(
                "unconfirmed",
                format!("{name} didn't confirm {what}. It may or may not have arrived."),
            ),
            _ => ApiError::new(&e.code, e.message),
        }
    }

    async fn ask(
        &self,
        device: &str,
        op: &str,
        args: Value,
        what: &str,
    ) -> Result<Value, ApiError> {
        self.inner
            .rpc
            .call(device, op, args)
            .await
            .map_err(|e| self.failed(device, e, what))
    }

    /// Ask the device for a change, then show it in the copy here at once; the device's own news
    /// (with a newer revision) replaces it.
    async fn change(
        &self,
        chat_id: &str,
        op: &str,
        args: Value,
        patch: impl FnOnce(&mut serde_json::Map<String, Value>),
    ) -> Res {
        let device = self.device_of(chat_id)?;
        self.check_live(&device)?;
        self.ask(&device, op, args, "that change").await?;
        self.patch_record(chat_id, patch);
        self.chat_json(chat_id)
    }
}

fn offline(name: &str, last_seen: Option<&str>) -> ApiError {
    ApiError::new(
        "offline",
        format!(
            "{name} is offline (last seen {}). You can read this chat; sending is off until it's back.",
            seen_text(last_seen)
        ),
    )
}

fn not_allowed(name: &str) -> ApiError {
    ApiError::new(
        "not_allowed",
        format!("{name} doesn't let other devices run its chats. You can read this one."),
    )
}

/// The words on the button or choice that an answer stands for, which the device checks against
/// what its request shows.
fn answer_label(i: &Interaction, a: &InteractionAnswerArgs) -> String {
    if a.cancelled == Some(true) {
        return "Cancel".to_owned();
    }
    match i.method {
        InteractionMethod::Select => a.value.clone().unwrap_or_default(),
        InteractionMethod::Confirm => {
            let options = confirm_options(i);
            let yes = options
                .iter()
                .find(|o| matches!(o.as_str(), "Approve plan" | "Allow"))
                .or_else(|| options.first())
                .cloned()
                .unwrap_or_default();
            if a.confirmed == Some(true) {
                yes
            } else {
                options
                    .iter()
                    .find(|o| **o != yes)
                    .cloned()
                    .unwrap_or_else(|| "Deny".to_owned())
            }
        }
        InteractionMethod::Input | InteractionMethod::Editor => {
            let title = i.title.as_deref().unwrap_or_default().trim().to_owned();
            if title.is_empty() {
                "Answer".to_owned()
            } else {
                title
            }
        }
        InteractionMethod::Dismiss => "Dismiss".to_owned(),
    }
}

impl RemoteChats for Mirror {
    fn owns(&self, chat_id: &str) -> Option<String> {
        self.owner(chat_id)
    }

    fn this_device(&self) -> String {
        self.inner.self_uid.clone()
    }

    fn device_name(&self, device_uid: &str) -> String {
        Mirror::device_name(self, device_uid)
    }

    fn get(&self, chat_id: String) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let out = m
                .inner
                .db
                .snapshot(|tx| {
                    let Some(chat) = chat_value(tx, &chat_id)? else {
                        return Ok::<_, rusqlite::Error>(None);
                    };
                    let stored: Option<String> = tx
                        .query_row(
                            "SELECT thread FROM mirror_chats WHERE chat_id = ?1",
                            [&chat_id],
                            |r| r.get(0),
                        )
                        .optional()?
                        .flatten();
                    let stored = stored
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .filter(Value::is_object);
                    let thread = match stored {
                        Some(t) => t,
                        // Nothing heard yet: the thread of a chat with no worker.
                        None => {
                            serde_json::from_value::<helm_chats::types::ChatRecord>(chat.clone())
                                .ok()
                                .and_then(|rec| {
                                    let rev = rec.revision;
                                    helm_chats::ops::stored_thread(tx, &rec, rev).ok()
                                })
                                .and_then(|t| serde_json::to_value(t).ok())
                                .unwrap_or(Value::Null)
                        }
                    };
                    Ok(Some(json!({ "chat": chat, "thread": thread })))
                })
                .map_err(db_err)?
                .ok_or_else(not_found)?;
            // Showing a chat is what starts listening to it.
            m.open_chat_later(&chat_id);
            Ok(out)
        })
    }

    fn page(&self, a: TranscriptPageArgs) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let set = [a.before.is_some(), a.after.is_some(), a.around.is_some()]
                .iter()
                .filter(|b| **b)
                .count();
            if set > 1 {
                return Err(ApiError::bad_args(
                    "Give at most one of before, after and around.",
                ));
            }
            let device = m.device_of(&a.chat_id)?;
            let local = m
                .inner
                .db
                .snapshot(|tx| {
                    local_page(
                        tx,
                        &a.chat_id,
                        a.before.as_deref(),
                        a.after.as_deref(),
                        a.around,
                        a.inclusive.unwrap_or(false),
                    )
                })
                .map_err(db_err)?
                .ok_or_else(not_found)?;
            let live = m.inner.device_up(&device);
            let mut page = local.page;
            let have = page["messages"].as_array().map_or(0, Vec::len);
            if let Some(before) = a.before.as_deref() {
                // The copy ran out before the page was full, or never held that row: the
                // device has the older rows.
                let wanted = live
                    && (local.cursor_unknown
                        || (have < PAGE_ROWS as usize && local.older_on_device));
                if let (true, Ok(id)) = (wanted, before.trim().parse::<i64>()) {
                    let asked = m
                        .inner
                        .rpc
                        .call(
                            &device,
                            "page",
                            json!({ "chatId": a.chat_id, "beforeMessageId": id, "limit": PAGE_ROWS }),
                        )
                        .await;
                    match asked {
                        Ok(v) => return Ok(v),
                        Err(e) => log::info!("devices: an older page was not read: {e}"),
                    }
                }
            } else if a.after.is_none()
                && live
                && local.older_on_device
                && page["hasOlder"] != json!(true)
            {
                // The copy starts here, but the device has more before it.
                if let Some(first) = page["messages"]
                    .get(0)
                    .and_then(|r| r.get("id"))
                    .and_then(Value::as_i64)
                {
                    page["hasOlder"] = json!(true);
                    page["olderCursor"] = json!(first.to_string());
                }
            }
            Ok(page)
        })
    }

    fn send(&self, a: ChatSendArgs) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let device = m.device_of(&a.chat_id)?;
            m.check_live(&device)?;
            if a.attachments.as_ref().is_some_and(|v| !v.is_empty()) {
                return Err(ApiError::new(
                    "refused",
                    "Images and files can't be sent to a chat on another device yet.",
                ));
            }
            let steer = a.action != SendAction::Default;
            let args = json!({
                "chatId": a.chat_id,
                "clientId": a.submission_id,
                "text": a.text,
                "mode": if steer { "steer" } else { "queue" },
            });
            // Never sent a second time from here: an unconfirmed send is the person's call.
            m.ask(&device, "send", args, "this message").await?;
            if a.origin.unwrap_or_default() == SendOrigin::Composer {
                let _ = m.inner.db.write(|tx| {
                    tx.execute(
                        "UPDATE mirror_chats SET draft = '', draft_revision = MAX(draft_revision, ?2)
                         WHERE chat_id = ?1",
                        rusqlite::params![a.chat_id, a.draft_revision],
                    )
                });
            }
            Ok(json!({
                "submission": {
                    "id": a.submission_id,
                    "chatId": a.chat_id,
                    "text": a.text,
                    "mode": if steer { "steer" } else { "prompt" },
                    "state": "accepted",
                    "origin": "typed",
                    "createdAt": helm_core::now_ms(),
                },
                "chat": m.chat_json(&a.chat_id)?,
            }))
        })
    }

    fn stop(&self, chat_id: String) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let device = m.device_of(&chat_id)?;
            m.check_live(&device)?;
            m.ask(&device, "stop", json!({ "chatId": chat_id }), "the stop")
                .await?;
            let chat = m.chat_json(&chat_id)?;
            Ok(json!({ "status": chat["status"] }))
        })
    }

    fn answer(&self, a: InteractionAnswerArgs) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let device = m.device_of(&a.chat_id)?;
            m.check_live(&device)?;
            if a.goto.is_some() {
                return Err(ApiError::new(
                    "refused",
                    "Going back a step can only be done on the PC this chat runs on.",
                ));
            }
            // The request as this PC last heard it; its words are the label the device checks.
            let stored: Option<String> = m
                .inner
                .db
                .snapshot(|tx| {
                    tx.query_row(
                        "SELECT thread FROM mirror_chats WHERE chat_id = ?1",
                        [&a.chat_id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()
                })
                .map_err(db_err)?
                .flatten();
            let waiting = stored
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|t| t.get("interactions").cloned())
                .and_then(|l| l.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| serde_json::from_value::<Interaction>(v).ok())
                .find(|i| i.id == a.request_id)
                .ok_or_else(|| ApiError::new("refused", "That request is no longer waiting."))?;
            let mut args = json!({
                "chatId": a.chat_id,
                "requestId": a.request_id,
                "label": answer_label(&waiting, &a),
            });
            if let Some(v) = &a.value {
                args["value"] = json!(v);
            }
            if let Some(c) = a.confirmed {
                args["confirmed"] = json!(c);
            }
            if let Some(c) = a.cancelled {
                args["cancelled"] = json!(c);
            }
            m.ask(&device, "answer", args, "that answer").await?;
            Ok(Value::Null)
        })
    }

    fn set_model(&self, a: ChatSetModelArgs) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let Some(model) = a.model.clone() else {
                return Err(ApiError::bad_args("Pick a model."));
            };
            let mut args = json!({ "chatId": a.chat_id, "model": model });
            if let Some(e) = &a.effort {
                args["effort"] = json!(e);
            }
            let effort = a.effort.clone();
            m.change(&a.chat_id, "set_model", args, move |rec| {
                rec.insert("model".into(), json!(model));
                if let Some(e) = effort {
                    rec.insert("effort".into(), json!(e));
                }
            })
            .await
        })
    }

    fn rename(&self, chat_id: String, title: String) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let shown = title.trim().to_owned();
            m.change(
                &chat_id,
                "rename",
                json!({ "chatId": chat_id, "title": title }),
                move |rec| {
                    rec.insert("title".into(), json!(shown));
                    rec.insert("titleState".into(), json!("manual"));
                },
            )
            .await
        })
    }

    fn archive(&self, chat_id: String, archived: bool) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            m.change(
                &chat_id,
                "archive",
                json!({ "chatId": chat_id, "archived": archived }),
                move |rec| {
                    rec.insert("archived".into(), json!(archived));
                },
            )
            .await
        })
    }

    /// A draft stays on this PC: it is never sent to the device.
    fn draft_save(&self, a: DraftSaveArgs) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            if a.text.len() > MAX_DRAFT_BYTES {
                return Err(ApiError::bad_args("The draft is larger than 2 MB."));
            }
            let (changed, stored) = m
                .inner
                .db
                .write(|tx| {
                    let n = tx.execute(
                        "UPDATE mirror_chats SET draft = ?2, draft_revision = ?3
                         WHERE chat_id = ?1 AND ?3 > draft_revision",
                        rusqlite::params![a.chat_id, a.text, a.revision],
                    )?;
                    let stored: Option<i64> = tx
                        .query_row(
                            "SELECT draft_revision FROM mirror_chats WHERE chat_id = ?1",
                            [&a.chat_id],
                            |r| r.get(0),
                        )
                        .optional()?;
                    Ok::<_, rusqlite::Error>((n, stored))
                })
                .map_err(db_err)?;
            match (changed, stored) {
                (1, _) => {
                    m.emit_chat(&a.chat_id);
                    Ok(json!({ "revision": a.revision }))
                }
                (_, Some(stored)) => Err(ApiError::new(
                    "stale",
                    format!("A newer draft is already saved (revision {stored})."),
                )),
                _ => Err(not_found()),
            }
        })
    }

    fn catalogue(&self, device_uid: String) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let hello: Option<String> = m
                .inner
                .db
                .snapshot(|tx| {
                    tx.query_row(
                        "SELECT hello FROM devices WHERE uid = ?1",
                        [&device_uid],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()
                })
                .map_err(db_err)?
                .flatten();
            hello
                .and_then(|h| serde_json::from_str::<Value>(&h).ok())
                .and_then(|h| h.get("catalogue").cloned())
                .ok_or_else(|| {
                    ApiError::new("unavailable", "That device's models are not known yet.")
                })
        })
    }
}
