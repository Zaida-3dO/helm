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

use crate::mirror::{chat_value, local_page, staged_dir, Mirror, PAGE_ROWS, UNCONFIRMED};
use crate::serve::confirm_options;
use crate::wire::{
    code, WireError, MAX_BLOB_BYTES, MAX_CHUNK_BYTES, MAX_SEND_ATTACHMENTS, MAX_UPLOAD_BYTES,
};
use base64::Engine as _;
use helm_chats::remote::{Fut, RemoteBlob, RemoteChats, Res};
use helm_chats::types::{
    ChatSendArgs, ChatSetModelArgs, DraftSaveArgs, Interaction, InteractionAnswerArgs,
    InteractionMethod, SendAction, SendOrigin, TranscriptPageArgs,
};
use helm_ipc::ApiError;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

/// The most a page brought from another PC may weigh, the document and its files together.
const MAX_PAGE_BUNDLE: u64 = 64 * 1024 * 1024;

fn too_big_page() -> ApiError {
    ApiError::new(
        "refused",
        "This page and its files are larger than 64 MiB, so it cannot be opened from another PC.",
    )
}

/// What a chat started on another PC is made from.
pub struct NewChat<'a> {
    pub person_id: &'a str,
    pub project_id: &'a str,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub title: Option<String>,
    pub muted: Option<bool>,
}

/// A page held on another PC, brought across to run here.
pub struct PageBundle {
    /// The pin there: id, title, path, linked chat.
    pub pin: Value,
    pub title: String,
    pub html: String,
    pub files: std::collections::HashMap<String, Vec<u8>>,
}

/// The most a draft may hold, as for a chat that runs here.
const MAX_DRAFT_BYTES: usize = 2 * 1024 * 1024;

/// The largest picture a reply shows from disk, as for a chat that runs here.
const LOCAL_IMAGE_MAX: u64 = 12 * 1024 * 1024;

/// The type of a picture from its first bytes; `None` for anything that is not PNG, JPEG, GIF
/// or WebP.
pub(crate) fn sniff_image(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// One file staged on this PC for a send to a chat on another device.
struct StagedFile {
    /// `image` or `file`.
    kind: String,
    name: String,
    mime: String,
    /// Where its bytes sit here: a name in this PC's staged folder for a picture, an absolute
    /// path for a file. Only pictures are removed after a send.
    target: String,
    bytes: Vec<u8>,
}

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

    /// Start a chat on `device` for the person, then open it here. `title` and `muted` are
    /// asked of the device outright, so the chat arrives already named and quiet.
    pub async fn create_chat(&self, device: &str, new: NewChat<'_>) -> Result<String, ApiError> {
        let NewChat {
            person_id,
            project_id,
            model,
            effort,
            title,
            muted,
        } = new;
        self.check_live(device)?;
        let mut args = json!({ "personId": person_id, "projectId": project_id });
        if let Some(m) = model {
            args["model"] = json!(m);
        }
        if let Some(e) = effort {
            args["effort"] = json!(e);
        }
        if let Some(t) = title {
            args["title"] = json!(t);
        }
        if let Some(muted) = muted {
            args["muted"] = json!(muted);
        }
        let act = [person_id.to_owned()];
        let made = self
            .ask_as(device, "create", args, "the new chat", &act)
            .await?;
        let id = made
            .get("chatId")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::internal("The new chat has no id."))?
            .to_owned();
        // Hold the device's record of the new chat now, so it can be read, listed and sent to
        // before the device's next list news arrives.
        if let Some(chat) = made.get("chat").filter(|c| c.get("id").is_some()) {
            self.inner
                .apply_chats(device, std::slice::from_ref(chat), None);
        }
        if let Err(e) = self.open_new_chat(device, &id).await {
            log::info!("devices: the new chat was not opened: {e}");
        }
        Ok(id)
    }

    /// Make a project on `device` for the person, then read its projects again so the new one is
    /// listed here at once. Answers the new project's id and kind.
    pub async fn project_create(
        &self,
        device: &str,
        person_id: &str,
        title: &str,
        description: Option<String>,
        folder_project_id: Option<String>,
    ) -> Result<Value, ApiError> {
        self.check_live(device)?;
        let mut args = json!({ "title": title });
        if let Some(d) = description {
            args["description"] = json!(d);
        }
        if let Some(f) = folder_project_id {
            args["folderProjectId"] = json!(f);
        }
        let act = [person_id.to_owned()];
        let made = self
            .ask_as(device, "project_create", args, "that project", &act)
            .await?;
        self.reread_projects(device).await;
        Ok(made)
    }

    /// Pin a project on `device`, or give its board one of that PC's folders, for the person.
    pub async fn project_set(
        &self,
        device: &str,
        person_id: &str,
        project_id: &str,
        pinned: Option<bool>,
        folder_project_id: Option<String>,
    ) -> Result<(), ApiError> {
        self.check_live(device)?;
        let mut args = json!({ "projectId": project_id });
        if let Some(p) = pinned {
            args["pinned"] = json!(p);
        }
        if let Some(f) = folder_project_id {
            args["folderProjectId"] = json!(f);
        }
        let act = [person_id.to_owned()];
        self.ask_as(device, "project_set", args, "that project", &act)
            .await?;
        self.reread_projects(device).await;
        Ok(())
    }

    /// Read the device's projects again (the new-chat page asks when it opens). A read, so it
    /// needs only the line and the device up.
    pub async fn refresh_projects(&self, device: &str) -> Result<(), ApiError> {
        self.check_up(device)?;
        self.inner
            .refresh_hello(device)
            .await
            .map_err(|e| self.failed(device, e, "its projects"))
    }

    /// After a change to the device's projects, take its word on them again. The change is
    /// already made, so a failure here only leaves the old list until the next time.
    async fn reread_projects(&self, device: &str) {
        if let Err(e) = self.inner.refresh_hello(device).await {
            log::info!("devices: projects were not read again: {e}");
        }
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
            // An older Helm does not know the card and upload ops and says "do that elsewhere".
            code::REFUSED
                if e.message == WireError::elsewhere().message
                    && matches!(
                        what,
                        "a file"
                            | "that decision"
                            | "those requests"
                            | "that change"
                            | "that read"
                            | "its pages"
                            | "that page"
                            | "those rows"
                            | "that project"
                            | "that picture"
                            | "that file"
                            | "that board"
                            | "that forward"
                            | "its tool servers"
                    ) =>
            {
                ApiError::new(
                    "refused",
                    format!("{name}'s Helm is too old for this; update it there."),
                )
            }
            code::TIMEOUT | UNCONFIRMED => ApiError::new(
                "unconfirmed",
                format!("{name} didn't confirm {what}. It may or may not have arrived."),
            ),
            _ => ApiError::new(&e.code, e.message),
        }
    }

    /// Ask the device, naming the people the call acts for (the acting person first); an
    /// empty set means the caller PC's own person, as before.
    /// The files staged on this PC for a send to a remote chat, in the order the send names
    /// them, with their bytes read in: pictures `devices_attach` saved here, and files it
    /// referenced by path. An id that matches nothing staged is refused before any words move.
    fn staged_attachments(
        &self,
        chat_id: &str,
        ids: &[String],
    ) -> Result<Vec<StagedFile>, ApiError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .inner
            .db
            .snapshot(|tx| {
                let mut out = Vec::new();
                for id in ids {
                    let row = tx
                        .query_row(
                            "SELECT kind, name, mime, target, bytes
                             FROM mirror_attachments WHERE id = ?1 AND chat_id = ?2",
                            rusqlite::params![id, chat_id],
                            |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, String>(1)?,
                                    r.get::<_, String>(2)?,
                                    r.get::<_, String>(3)?,
                                    r.get::<_, i64>(4)?,
                                ))
                            },
                        )
                        .optional()?;
                    if let Some((kind, name, mime, target, size)) = row {
                        out.push((id.clone(), kind, name, mime, target, size));
                    }
                }
                Ok::<_, rusqlite::Error>(out)
            })
            .map_err(db_err)?;
        let dir = staged_dir(self.inner.db.path());
        let mut files = Vec::with_capacity(ids.len());
        for (_id, kind, name, mime, target, size) in rows {
            if size > MAX_UPLOAD_BYTES as i64 {
                return Err(ApiError::new(
                    "too_large",
                    "A file sent to another device is at most 25 MB.",
                ));
            }
            // A picture or a pasted text was saved here when it was attached; a file stays
            // where it is and is read anew, so it crosses as it stands when the message goes.
            let path = if kind == "image" || kind == "text" {
                dir.join(&target)
            } else {
                std::path::PathBuf::from(&target)
            };
            // A file that grew since it was attached is refused before it is read into memory.
            if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_UPLOAD_BYTES as u64) {
                return Err(ApiError::new(
                    "too_large",
                    "A file sent to another device is at most 25 MB.",
                ));
            }
            let bytes = std::fs::read(&path).map_err(|_| {
                ApiError::new(
                    "refused",
                    "A staged file could not be read; attach it again.",
                )
            })?;
            // The file may have changed since it was attached; the cap is checked anew.
            if bytes.len() > MAX_UPLOAD_BYTES {
                return Err(ApiError::new(
                    "too_large",
                    "A file sent to another device is at most 25 MB.",
                ));
            }
            files.push(StagedFile {
                kind,
                name,
                mime,
                target,
                bytes,
            });
        }
        if files.len() != ids.len() {
            return Err(ApiError::new(
                "refused",
                "An attachment is no longer staged; attach it again.",
            ));
        }
        Ok(files)
    }

    /// Carry one staged file across as an ordered run of chunks, each small enough for the
    /// line. The serving PC joins them and answers once the last has arrived.
    async fn upload_file(
        &self,
        device: &str,
        chat_id: &str,
        f: &StagedFile,
        upload_id: &str,
        act: &[String],
    ) -> Result<(), ApiError> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut offset = 0usize;
        let mut seq = 0u32;
        loop {
            let end = (offset + MAX_CHUNK_BYTES).min(f.bytes.len());
            let eof = end == f.bytes.len();
            let mut args = json!({
                "chatId": chat_id,
                "uploadId": upload_id,
                "seq": seq,
                "eof": eof,
                "data": b64.encode(&f.bytes[offset..end]),
            });
            if seq == 0 {
                // A pasted text is a plain-text file to the PC that runs the chat.
                args["kind"] = json!(if f.kind == "image" { "image" } else { "file" });
                args["name"] = json!(f.name);
                args["mime"] = json!(f.mime);
            }
            self.ask_as(device, "attach_put", args, "a file", act)
                .await?;
            offset = end;
            seq += 1;
            if eof {
                return Ok(());
            }
        }
    }

    /// A send that went through has spent its staged files here.
    fn drop_staged(&self, ids: &[String], staged: &[StagedFile]) {
        let dir = staged_dir(self.inner.db.path());
        let _ = self.inner.db.write(|tx| {
            for id in ids {
                tx.execute("DELETE FROM mirror_attachments WHERE id = ?1", [id])?;
            }
            Ok::<_, rusqlite::Error>(())
        });
        for f in staged {
            if f.kind == "image" || f.kind == "text" {
                let _ = std::fs::remove_file(dir.join(&f.target));
            }
        }
    }

    async fn ask_as(
        &self,
        device: &str,
        op: &str,
        args: Value,
        what: &str,
        act: &[String],
    ) -> Result<Value, ApiError> {
        self.inner
            .rpc
            .call_as(device, op, args, act)
            .await
            .map_err(|e| self.failed(device, e, what))
    }

    /// Answer an Allow card of a chat that runs on `device`. The device's consent store decides
    /// by compare-and-set — whoever answers first wins — and its news closes the card on every
    /// device holding the chat, this one included.
    pub async fn consent_decide(
        &self,
        chat_id: &str,
        request_id: &str,
        decision: &str,
        grant_minutes: Option<i64>,
        note: Option<String>,
    ) -> Result<(), ApiError> {
        let device = self.device_of(chat_id)?;
        self.check_live(&device)?;
        let mut args = json!({
            "chatId": chat_id,
            "requestId": request_id,
            "decision": decision,
        });
        if let Some(m) = grant_minutes {
            args["grantMinutes"] = json!(m);
        }
        if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
            args["note"] = json!(n);
        }
        let act = self.acting(chat_id)?;
        self.ask_as(&device, "consent_decide", args, "that decision", &[act])
            .await?;
        Ok(())
    }

    /// Where the Allow requests of a chat that runs on another PC stand, as that PC's consent
    /// store says. A tool row's chip asks for this so the copy can say Allowed or Refused too.
    pub async fn consent_states(&self, chat_id: &str) -> Result<Value, ApiError> {
        let device = self.device_of(chat_id)?;
        self.check_live(&device)?;
        self.ask_as(
            &device,
            "consent_states",
            json!({ "chatId": chat_id }),
            "those requests",
            &[],
        )
        .await
    }

    /// The line and the device just need to be up: reads answer while the device's run switch
    /// is off (the same rule as a chat's cost).
    fn check_up(&self, device: &str) -> Result<(), ApiError> {
        if !self.line_is_up() {
            return Err(ApiError::new(
                "reconnecting",
                "Reconnecting to the hub… showing the copy on this PC.",
            ));
        }
        match self.device(device) {
            Some(d) if d.online => Ok(()),
            other => Err(offline(
                &self.device_name(device),
                other.and_then(|d| d.last_seen_at).as_deref(),
            )),
        }
    }

    /// The new-chat model/effort/reply-length defaults of the PC that runs this chat, read or
    /// saved from the picker on a mirrored chat. Reading answers with the run switch off;
    /// saving needs it, and carries the acting person.
    pub async fn composer_defaults(
        &self,
        chat_id: &str,
        patch: Option<&Value>,
    ) -> Result<Value, ApiError> {
        let device = self.device_of(chat_id)?;
        match patch {
            None => {
                self.check_up(&device)?;
                self.ask_as(
                    &device,
                    "defaults_get",
                    json!({ "chatId": chat_id }),
                    "those defaults",
                    &[],
                )
                .await
            }
            Some(p) => {
                self.check_live(&device)?;
                let act = self.acting(chat_id)?;
                let mut args = p.clone();
                args["chatId"] = json!(chat_id);
                self.ask_as(&device, "defaults_set", args, "those defaults", &[act])
                    .await
            }
        }
    }

    /// Withdraw or recover one queued message of a mirrored chat, on the PC that runs it.
    pub async fn submission_action(
        &self,
        chat_id: &str,
        submission_id: &str,
        action: &str,
    ) -> Result<(), ApiError> {
        let device = self.device_of(chat_id)?;
        self.check_live(&device)?;
        let act = self.acting(chat_id)?;
        self.ask_as(
            &device,
            "submission",
            json!({ "chatId": chat_id, "submissionId": submission_id, "action": action }),
            "that change",
            &[act],
        )
        .await?;
        Ok(())
    }

    /// The board-item links of mirrored chats, read from the PCs that run them. Each link row
    /// names its chat, so one ask covers a device's whole group; a device that is down simply
    /// contributes nothing and its chats are listed as unreachable.
    pub async fn item_links(&self, chat_ids: &[String]) -> Result<Value, ApiError> {
        let mut links = Vec::new();
        let mut unreachable = Vec::new();
        let mut by_device: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for id in chat_ids {
            if let Some(device) = self.owner(id) {
                by_device.entry(device).or_default().push(id.clone());
            }
        }
        // The device refuses more than 500 ids in one ask.
        let groups: Vec<(String, Vec<String>)> = by_device
            .into_iter()
            .flat_map(|(d, ids)| {
                ids.chunks(500)
                    .map(|c| (d.clone(), c.to_vec()))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (device, ids) in groups {
            if self.check_up(&device).is_err() {
                unreachable.extend(ids);
                continue;
            }
            let got = match self
                .ask_as(
                    &device,
                    "item_links",
                    json!({ "chatIds": ids }),
                    "those links",
                    &[],
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    log::info!("devices: an item-links read did not answer: {e}");
                    unreachable.extend(ids);
                    continue;
                }
            };
            if let Some(rows) = got.get("links").and_then(Value::as_array) {
                links.extend(rows.iter().cloned());
            }
        }
        Ok(json!({ "links": links, "unreachable": unreachable }))
    }

    /// The needs-you and flag rows of mirrored chats, read from the PCs that run them.
    pub async fn attention_rows(&self, chat_ids: &[String]) -> Result<Value, ApiError> {
        let mut needs_you = Vec::new();
        let mut flagged = Vec::new();
        let mut unreachable = Vec::new();
        let mut by_device: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for id in chat_ids {
            if let Some(device) = self.owner(id) {
                by_device.entry(device).or_default().push(id.clone());
            }
        }
        // The device refuses more than 500 ids in one ask.
        let groups: Vec<(String, Vec<String>)> = by_device
            .into_iter()
            .flat_map(|(d, ids)| {
                ids.chunks(500)
                    .map(|c| (d.clone(), c.to_vec()))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (device, ids) in groups {
            if self.check_up(&device).is_err() {
                unreachable.extend(ids);
                continue;
            }
            let got = match self
                .ask_as(
                    &device,
                    "attention",
                    json!({ "chatIds": ids }),
                    "those rows",
                    &[],
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    log::info!("devices: an attention read did not answer: {e}");
                    unreachable.extend(ids);
                    continue;
                }
            };
            for (group, into) in [("needsYou", &mut needs_you), ("flagged", &mut flagged)] {
                if let Some(rows) = got.get(group).and_then(Value::as_array) {
                    into.extend(rows.iter().cloned());
                }
            }
        }
        Ok(json!({ "needsYou": needs_you, "flagged": flagged, "unreachable": unreachable }))
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
        let act = self.acting(chat_id)?;
        self.ask_as(&device, op, args, "that change", &[act])
            .await?;
        self.patch_record(chat_id, patch);
        self.chat_json(chat_id)
    }

    /// Patch this PC's copy after the device that runs the chat accepted a move, pin, nest or
    /// reorder: the placement it answers with for this chat, and the board stamp when the chat
    /// now shows under a different board there.
    fn apply_organize(&self, chat_id: &str, device: &str, answer: &Value) {
        let Some(row) = answer.as_array().and_then(|l| {
            l.iter()
                .find(|r| r.get("chatId").and_then(Value::as_str) == Some(chat_id))
                .cloned()
        }) else {
            return;
        };
        let group = row
            .get("groupProjectId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let board = group.as_deref().and_then(|g| {
            self.inner
                .db
                .snapshot(|tx| {
                    tx.query_row(
                        "SELECT name, colour, kind FROM mirror_projects
                         WHERE device_uid = ?1 AND project_id = ?2",
                        rusqlite::params![device, g],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()
                })
                .ok()
                .flatten()
        });
        let pinned = row.get("pinned").cloned();
        let parent = row.get("parentChatId").cloned();
        let position = row.get("position").cloned();
        self.patch_record(chat_id, move |rec| {
            if let Some(p) = pinned {
                rec.insert("pinned".into(), p);
            }
            if let Some(p) = parent {
                rec.insert("parentChatId".into(), p);
            }
            if let Some(p) = position {
                rec.insert("position".into(), p);
            }
            // A board stamp only changes when the group did; a folder group has none.
            if rec.get("boardId").and_then(Value::as_str) != group.as_deref() {
                match board {
                    Some((name, colour, kind)) if kind == "board" => {
                        rec.insert("boardId".into(), json!(group));
                        rec.insert("boardName".into(), json!(name));
                        rec.insert("boardColour".into(), json!(colour));
                    }
                    _ => {
                        rec.remove("boardId");
                        rec.remove("boardName");
                        rec.remove("boardColour");
                    }
                }
            }
        });
    }

    /// Show the chat to a person on the device that runs it, or stop (the `share` wire op, which
    /// answers with that PC's own `profiles_share_chats`).
    pub async fn share_chat(
        &self,
        chat_id: &str,
        person_id: &str,
        shared: bool,
    ) -> Result<(), ApiError> {
        let device = self.device_of(chat_id)?;
        self.check_live(&device)?;
        let act = self.acting(chat_id)?;
        self.ask_as(
            &device,
            "share",
            json!({ "chatId": chat_id, "personId": person_id, "shared": shared }),
            "that share",
            &[act],
        )
        .await?;
        Ok(())
    }

    /// Hand the chat to one of the profiles the device listed in its hello (the `move_profile`
    /// wire op, answered by that PC's own `profiles_assign_chats`).
    pub async fn move_profile(&self, chat_id: &str, profile_id: &str) -> Result<(), ApiError> {
        let device = self.device_of(chat_id)?;
        self.check_live(&device)?;
        let act = self.acting(chat_id)?;
        self.ask_as(
            &device,
            "move_profile",
            json!({ "chatId": chat_id, "profileId": profile_id }),
            "that move",
            &[act],
        )
        .await?;
        Ok(())
    }

    /// The task-board connection of the PC that runs this chat, as its own board settings have
    /// it: whether a board is set up at all, without ever carrying its address or token.
    pub async fn board_connection(&self, chat_id: &str) -> Result<Value, ApiError> {
        let device = self.device_of(chat_id)?;
        self.check_up(&device)?;
        self.ask_as(
            &device,
            "board_connection",
            json!({ "chatId": chat_id }),
            "that board",
            &[],
        )
        .await
    }

    /// Ask the PC that runs this chat to forward one of its ports here, acting as the person
    /// this chat's changes act as. Answers that PC's id and the forward's.
    pub async fn forward_open(
        &self,
        chat_id: &str,
        port: u16,
    ) -> Result<(String, String), ApiError> {
        let device = self.device_of(chat_id)?;
        self.check_live(&device)?;
        let act = self.acting(chat_id)?;
        let r = self
            .ask_as(
                &device,
                "forward_open",
                json!({ "chatId": chat_id, "port": port }),
                "that forward",
                &[act],
            )
            .await?;
        let id = r["forwardId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ApiError::internal("The other PC did not name the forward."))?;
        Ok((device, id.to_owned()))
    }

    /// Tell `device` a forward it agreed to will not be used after all. Best effort: the
    /// forward ends there by itself when idle.
    pub async fn forward_close(&self, device: &str, forward_id: &str) {
        let _ = self
            .inner
            .rpc
            .call(device, "forward_close", json!({ "forwardId": forward_id }))
            .await;
    }

    /// One read of the task board the PC that runs this chat has set up, sent as that PC's own
    /// `board_read` op and answered the same way. The board's address and token never leave it.
    pub async fn board_read(&self, chat_id: &str, params: Value) -> Result<Value, ApiError> {
        let device = self.device_of(chat_id)?;
        self.check_up(&device)?;
        let mut args = params;
        args["chatId"] = json!(chat_id);
        self.ask_as(&device, "board_read", args, "that board read", &[])
            .await
    }

    /// Stop a board read this PC started on the device that runs the chat, as its own
    /// `board_cancel` does when a request runs past its time.
    pub async fn board_cancel(&self, chat_id: &str, request_id: &str) -> Result<Value, ApiError> {
        let device = self.device_of(chat_id)?;
        self.check_up(&device)?;
        self.ask_as(
            &device,
            "board_cancel",
            json!({ "chatId": chat_id, "requestId": request_id }),
            "that cancel",
            &[],
        )
        .await
    }

    /// The pages pinned on `device` that `person` may open from here, read there: `{pages}`.
    /// Needs the device up and its switch on, as everything that reads its disk does.
    pub async fn pages_on(&self, device: &str, person: &str) -> Result<Value, ApiError> {
        self.check_live(device)?;
        self.ask_as(
            device,
            "pages_list",
            json!({}),
            "its pages",
            &[person.to_owned()],
        )
        .await
    }

    /// The MCP tool servers set up on `device`, with every secret blanked there: for Settings →
    /// MCP's "Copy from another PC". Asked only as the person both PCs share — that PC refuses
    /// anyone else — so it needs the line up and the pairing, not the run switch.
    pub async fn mcp_list(&self, device: &str, person: &str) -> Result<Value, ApiError> {
        self.check_up(device)?;
        self.ask_as(
            device,
            "mcp_list_get",
            json!({}),
            "its tool servers",
            &[person.to_owned()],
        )
        .await
    }

    /// Bring a pinned page across: the document and the files beside it that it names, each
    /// read there in chunks small enough for the line. Nothing else of that PC's disk is asked
    /// for: the page is named by its pin, and the device reads only inside the page's folder.
    pub async fn page_bundle(
        &self,
        device: &str,
        person: &str,
        page_id: &str,
    ) -> Result<PageBundle, ApiError> {
        self.check_live(device)?;
        let act = [person.to_owned()];
        let info = self
            .ask_as(
                device,
                "pages_files",
                json!({ "pageId": page_id }),
                "that page",
                &act,
            )
            .await?;
        let size = info["size"].as_u64().unwrap_or_default();
        let files: Vec<(String, u64)> = info["files"]
            .as_array()
            .map(|l| {
                l.iter()
                    .filter_map(|f| Some((f["path"].as_str()?.to_owned(), f["size"].as_u64()?)))
                    .collect()
            })
            .unwrap_or_default();
        if size + files.iter().map(|(_, n)| n).sum::<u64>() > MAX_PAGE_BUNDLE {
            return Err(too_big_page());
        }
        let html = self
            .read_page_file(device, page_id, None, &act)
            .await
            .and_then(|b| {
                String::from_utf8(b)
                    .map_err(|_| ApiError::new("refused", "The page must be UTF-8 text."))
            })?;
        let mut held = std::collections::HashMap::new();
        for (rel, _) in files {
            let bytes = self
                .read_page_file(device, page_id, Some(&rel), &act)
                .await?;
            held.insert(rel, bytes);
        }
        Ok(PageBundle {
            pin: info["pin"].clone(),
            title: info["title"].as_str().unwrap_or("Page").to_owned(),
            html,
            files: held,
        })
    }

    async fn read_page_file(
        &self,
        device: &str,
        page_id: &str,
        rel: Option<&str>,
        act: &[String],
    ) -> Result<Vec<u8>, ApiError> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let mut args = json!({ "pageId": page_id, "offset": out.len() });
            if let Some(r) = rel {
                args["path"] = json!(r);
            }
            let got = self
                .ask_as(device, "pages_read", args, "that page", act)
                .await?;
            let chunk = b64
                .decode(got["data"].as_str().unwrap_or_default())
                .map_err(|_| ApiError::new("refused", "A page file arrived damaged."))?;
            let eof = got["eof"].as_bool() == Some(true);
            if chunk.is_empty() && !eof {
                return Err(ApiError::new("refused", "A page file stopped arriving."));
            }
            out.extend(chunk);
            if out.len() as u64 > MAX_PAGE_BUNDLE {
                return Err(too_big_page());
            }
            if eof {
                return Ok(out);
            }
        }
    }

    /// What an agent showed in a chat that runs on another device, the pictures of its
    /// messages, and Stop without resending. The gallery and board reads and the board's answer
    /// go to the device as they are; a full picture, or a reply's picture inside the chat's
    /// project folder, comes across in chunks. `None` for any other op.
    async fn shown_op(&self, op: &str, device: &str, chat_id: &str, args: &Value) -> Option<Res> {
        let arg = |k: &str| args.get(k).cloned().unwrap_or(Value::Null);
        Some(match op {
            "shown_gallery_get" => {
                let a = json!({ "chatId": chat_id, "galleryId": arg("galleryId") });
                self.shown_read(device, "shown_gallery", a, "that picture")
                    .await
            }
            "shown_image_thumb" => {
                let a = json!({ "chatId": chat_id, "galleryId": arg("galleryId"), "index": arg("index") });
                self.shown_read(device, "shown_image", a, "that picture")
                    .await
            }
            "option_board_state" => {
                let a = json!({ "chatId": chat_id, "boardId": arg("boardId") });
                self.shown_read(device, "option_board", a, "that board")
                    .await
            }
            "option_board_answer" => self.board_answer(device, chat_id, args).await,
            "shown_image_get" => {
                let r = json!({ "gallery": arg("galleryId"), "index": arg("index") });
                self.picture(device, chat_id, r, MAX_BLOB_BYTES, false)
                    .await
            }
            "attachment_get" if args.get("thumb").and_then(Value::as_bool) != Some(true) => {
                self.full_attachment(device, chat_id, args).await
            }
            "local_image_get" => match args.get("path").and_then(Value::as_str) {
                Some(path) => {
                    let r = json!({ "file": path });
                    self.picture(device, chat_id, r, LOCAL_IMAGE_MAX, true)
                        .await
                }
                None => Err(ApiError::bad_args("path is required.")),
            },
            "chat_stop_no_resend" => self.stop_no_resend(device, chat_id).await,
            _ => return None,
        })
    }

    /// A gallery or board read: the device needs only to be up.
    async fn shown_read(&self, device: &str, wire: &str, args: Value, what: &str) -> Res {
        self.check_up(device)?;
        self.ask_as(device, wire, args, what, &[]).await
    }

    /// The person's choice on an option board, answered on the device; the first answer stands.
    async fn board_answer(&self, device: &str, chat_id: &str, args: &Value) -> Res {
        self.check_live(device)?;
        let act = self.acting(chat_id)?;
        let mut a = json!({ "chatId": chat_id, "boardId": args.get("boardId") });
        for key in ["labels", "other"] {
            if let Some(v) = args.get(key).filter(|v| !v.is_null()) {
                a[key] = v.clone();
            }
        }
        self.ask_as(device, "option_board_answer", a, "that board", &[act])
            .await
    }

    async fn stop_no_resend(&self, device: &str, chat_id: &str) -> Res {
        self.check_live(device)?;
        let act = self.acting(chat_id)?;
        self.ask_as(
            device,
            "stop",
            json!({ "chatId": chat_id, "noResend": true }),
            "the stop",
            &[act],
        )
        .await?;
        let chat = self.chat_json(chat_id)?;
        Ok(json!({ "status": chat["status"] }))
    }

    /// A picture brought across whole, answered as the local picture reads answer:
    /// `{mime, bytes}`. With `sniff`, only a PNG, JPEG, GIF or WebP by its first bytes is shown,
    /// whatever its name.
    async fn picture(&self, device: &str, chat_id: &str, r: Value, cap: u64, sniff: bool) -> Res {
        let act = self.blob_gate(device, chat_id, &r)?;
        let blob = self
            .fetch_blob(device, chat_id, &r, cap, &act)
            .await
            .map_err(|e| self.failed(device, e, "that picture"))?;
        let mime = if sniff {
            sniff_image(&blob.bytes)
                .ok_or_else(|| ApiError::bad_args("That file is not a picture."))?
                .to_owned()
        } else {
            blob.mime
        };
        Ok(json!({
            "mime": mime,
            "bytes": base64::engine::general_purpose::STANDARD.encode(&blob.bytes),
        }))
    }

    /// The file view's answer carried a picture marked oversize instead of its bytes: bring
    /// the whole picture across in chunks and answer with what a local read answers. The same
    /// local rule applies — beyond 8 MiB a picture is not previewed at all.
    async fn complete_oversize_picture(&self, device: &str, chat_id: &str, answer: Value) -> Res {
        let size = answer
            .get("size")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if size > helm_panel::viewer::IMAGE_LIMIT {
            return Err(ApiError::new(
                "refused",
                "The image is larger than 8 MiB, so it is not previewed.",
            ));
        }
        let path = answer
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let act = self.acting(chat_id)?;
        let blob = self
            .fetch_blob(
                device,
                chat_id,
                &json!({ "file": path }),
                helm_panel::viewer::IMAGE_LIMIT,
                &[act],
            )
            .await
            .map_err(|e| self.failed(device, e, "that picture"))?;
        let mut answer = answer;
        if let Some(m) = answer.as_object_mut() {
            m.insert(
                "bytes".into(),
                json!(base64::engine::general_purpose::STANDARD.encode(&blob.bytes)),
            );
            m.remove("oversize");
        }
        Ok(answer)
    }

    /// A picture sent with a message, full size. An older Helm there has no chunked read; its
    /// own answer still brings the pictures that fit one frame.
    async fn full_attachment(&self, device: &str, chat_id: &str, args: &Value) -> Res {
        let r = json!({ "attachment": args.get("attachmentId") });
        let act = self.blob_gate(device, chat_id, &r)?;
        match self
            .fetch_blob(device, chat_id, &r, MAX_BLOB_BYTES, &act)
            .await
        {
            Ok(blob) => Ok(json!({
                "mime": blob.mime,
                "bytes": base64::engine::general_purpose::STANDARD.encode(&blob.bytes),
            })),
            Err(e) if e.code == code::REFUSED && e.message == WireError::elsewhere().message => {
                self.ask_as(device, "attachment_get", args.clone(), "that picture", &[])
                    .await
            }
            Err(e) => Err(self.failed(device, e, "that picture")),
        }
    }

    /// Something big of a chat that runs on another device, brought across whole: a picture
    /// needs the device up, a file or a tool's output (read from its disk) also its switch on.
    /// At most `cap` bytes, checked against what the device says before anything is held.
    pub async fn read_blob(
        &self,
        chat_id: &str,
        r: Value,
        cap: u64,
    ) -> Result<RemoteBlob, ApiError> {
        let device = self.device_of(chat_id)?;
        let act = self.blob_gate(&device, chat_id, &r)?;
        self.fetch_blob(&device, chat_id, &r, cap, &act)
            .await
            .map_err(|e| self.failed(&device, e, "that file"))
    }

    /// The device's state checked for a `blob_read`, and who it acts for.
    fn blob_gate(&self, device: &str, chat_id: &str, r: &Value) -> Result<Vec<String>, ApiError> {
        if r.get("file").is_some() || r.get("output").is_some() {
            self.check_live(device)?;
            Ok(vec![self.acting(chat_id)?])
        } else {
            self.check_up(device)?;
            Ok(Vec::new())
        }
    }

    /// The chunks of one `blob_read`, in order, joined. Each answer must carry the offset asked
    /// for and the same total, and the whole stops at `cap`.
    async fn fetch_blob(
        &self,
        device: &str,
        chat_id: &str,
        r: &Value,
        cap: u64,
        act: &[String],
    ) -> Result<RemoteBlob, WireError> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let damaged = || WireError::new(code::REFUSED, "That arrived damaged; try again.");
        let mut out: Vec<u8> = Vec::new();
        let mut first: Option<(u64, String, String)> = None;
        loop {
            let got = self
                .inner
                .rpc
                .call_as(
                    device,
                    "blob_read",
                    json!({ "chatId": chat_id, "ref": r, "offset": out.len() }),
                    act,
                )
                .await?;
            let total = got["total"].as_u64().ok_or_else(damaged)?;
            if total > cap {
                return Err(WireError::new(
                    code::REFUSED,
                    "That is too large to bring across from the other PC.",
                ));
            }
            let (want, _, _) = first.get_or_insert_with(|| {
                out.reserve(usize::try_from(total).unwrap_or(0));
                (
                    total,
                    got["mime"].as_str().unwrap_or_default().to_owned(),
                    got["name"].as_str().unwrap_or_default().to_owned(),
                )
            });
            if *want != total || got["offset"].as_u64() != Some(out.len() as u64) {
                return Err(damaged());
            }
            let chunk = b64
                .decode(got["data"].as_str().unwrap_or_default())
                .map_err(|_| damaged())?;
            if chunk.is_empty() && (out.len() as u64) < total {
                return Err(damaged());
            }
            out.extend(chunk);
            if out.len() as u64 > total {
                return Err(damaged());
            }
            if out.len() as u64 == total {
                let (_, mime, name) = first.unwrap_or_default();
                return Ok(RemoteBlob {
                    mime,
                    name,
                    bytes: out,
                });
            }
        }
    }

    /// The person a change to this chat acts as, alone on the wire: the active profile's
    /// person, or with "All profiles" the chat owner's person when a profile here is linked to
    /// them, else the home profile's person. A profile with no hub person yet (before the first
    /// sync pass lines it up) is refused in words.
    fn acting(&self, chat_id: &str) -> Result<String, ApiError> {
        self.acting_as(chat_id, None)
    }

    /// The person this PC acts as when no chat is in view (the Pages list): the active
    /// profile's, as for a chat that has no owner of its own.
    pub fn acting_now(&self) -> Result<String, ApiError> {
        self.acting_as("", None)
    }

    /// [`Self::acting`], except that a caller who names the person they act for (a tool call
    /// from a chat, an outside call the hub stamped) is that person whoever the sidebar shows.
    fn acting_as(&self, chat_id: &str, named: Option<&str>) -> Result<String, ApiError> {
        if let Some(p) = named.map(str::trim).filter(|p| !p.is_empty()) {
            return Ok(p.to_owned());
        }
        let chat_person = self
            .inner
            .db
            .snapshot(|tx| {
                let chat_person = tx
                    .query_row(
                        "SELECT record FROM mirror_chats WHERE chat_id = ?1",
                        [chat_id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                    .and_then(|rec| {
                        serde_json::from_str::<Value>(&rec).ok().and_then(|v| {
                            v.get("personId")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                    });
                let has_profiles: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'profiles')",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(false);
                Ok::<_, rusqlite::Error>((chat_person, has_profiles))
            })
            .map_err(db_err)?;
        if !chat_person.1 {
            // An install without a profiles table: the line's own people stand in.
            return self.inner.link.people().into_iter().next().ok_or_else(|| {
                ApiError::new(
                    "refused",
                    "This profile isn't set up on the hub yet; try again in a moment.",
                )
            });
        }
        self.inner
            .db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(acting_person(tx, chat_person.0.as_deref())))
            .map_err(db_err)?
    }
}

/// The acting person for requests about a mirrored chat (see [`Mirror::acting`]).
pub(crate) fn acting_person(
    conn: &Connection,
    chat_person: Option<&str>,
) -> Result<String, ApiError> {
    let not_set_up = || {
        ApiError::new(
            "refused",
            "This profile isn't set up on the hub yet; try again in a moment.",
        )
    };
    let active = helm_core::prefs::read(conn, "activeProfile")
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| s != "all");
    if let Some(profile) = active {
        let person: Option<Option<String>> = conn
            .query_row(
                "SELECT hub_person_id FROM profiles WHERE id = ?1",
                [profile],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?
            .flatten();
        match person {
            Some(Some(p)) => return Ok(p),
            Some(None) => return Err(not_set_up()),
            None => {}
        }
    }
    if let Some(p) = chat_person {
        let linked: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profiles WHERE hub_person_id = ?1)",
                [p],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if linked {
            return Ok(p.to_owned());
        }
    }
    conn.query_row(
        "SELECT hub_person_id FROM profiles WHERE is_home = 1",
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .flatten()
    .ok_or_else(not_set_up)
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

/// The local op → the wire op for the reads of a mirrored chat: they answer while the device's
/// run switch is off, like a cost read.
fn read_op(op: &str) -> Option<&'static str> {
    Some(match op {
        "usage_cost" => "cost",
        "chat_stack" => "chat_stack",
        "stack_find" => "stack_find",
        "chat_verbosity_get" => "verbosity_get",
        "panel_context_get" => "context_on",
        "quick_open" => "quick_open",
        "checkpoint_info" => "checkpoint",
        // The edit window opens on the mirrored text: a read like the transcript itself.
        "message_edit_open" => "edit_open",
        // The `/` menu lists what the chat's own worker reported: a read there, nothing of
        // this PC's skills folders is involved.
        "skills_list" => "skills",
        // The Rename dialog's suggestions are made where the chat's words are, and write
        // nothing: a read there, like the transcript itself.
        "title_suggest" => "title_suggest",
        "chat_folder" => "chat_folder",
        "attachment_get" => "attachment_get",
        "file_read" | "dir_list" => "file_view",
        "file_stat" => "file_stat",
        "archive_list" => "archive_list",
        "path_resolve" => "path_resolve",
        "changes" => "changes",
        "diff" => "diff",
        _ => return None,
    })
}

/// The words on the button or choice that an answer stands for, which the device checks against
/// what its request shows.
fn answer_label(i: &Interaction, a: &InteractionAnswerArgs) -> String {
    if a.cancelled == Some(true) {
        return "Cancel".to_owned();
    }
    if a.goto.is_some() {
        return "Go back a step".to_owned();
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
            // A stack page belongs wholly to the PC that runs the chat: this copy only ever
            // holds the one opened session, so while that PC is up it answers the stack
            // itself — it is also the only side that can walk forward or centre a jump.
            if a.stack == Some(true) && m.inner.device_up(&device) {
                let mut ask = json!({ "chatId": a.chat_id, "stack": true, "limit": PAGE_ROWS });
                if let Some(id) = a
                    .before
                    .as_deref()
                    .and_then(|c| c.trim().parse::<i64>().ok())
                {
                    ask["beforeMessageId"] = json!(id);
                }
                if let Some(id) = a
                    .after
                    .as_deref()
                    .and_then(|c| c.trim().parse::<i64>().ok())
                {
                    ask["afterMessageId"] = json!(id);
                }
                if let Some(t) = a.around {
                    ask["aroundMessageId"] = json!(t);
                }
                match m.inner.rpc.call(&device, "page", ask).await {
                    Ok(v) => return Ok(v),
                    Err(e) => log::info!("devices: a stack page was not read: {e}"),
                }
            }
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
        self.send_as(a, None)
    }

    fn create_chat(&self, a: helm_chats::remote::RemoteCreateChat) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let id = m
                .create_chat(
                    &a.device_uid,
                    NewChat {
                        person_id: &a.person_id,
                        project_id: &a.project_id,
                        model: a.model,
                        effort: a.effort,
                        title: a.title,
                        muted: Some(a.muted),
                    },
                )
                .await?;
            Ok(json!({ "chatId": id }))
        })
    }

    fn send_as(&self, a: ChatSendArgs, person: Option<String>) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let device = m.device_of(&a.chat_id)?;
            m.check_live(&device)?;
            let ids: Vec<String> = a.attachments.clone().unwrap_or_default();
            if ids.len() > MAX_SEND_ATTACHMENTS {
                return Err(ApiError::bad_args(
                    "A message can carry at most 20 attachments.",
                ));
            }
            let steer = a.action != SendAction::Default;
            // Never sent a second time from here: an unconfirmed send is the person's call.
            let act = [m.acting_as(&a.chat_id, person.as_deref())?];
            // The files cross to that PC first, chunk by chunk; the send names the ids they
            // arrived under. A refused upload stops here, before any words move.
            let staged = m.staged_attachments(&a.chat_id, &ids)?;
            let mut uploads = Vec::new();
            for f in &staged {
                let upload_id = uuid::Uuid::new_v4().to_string();
                m.upload_file(&device, &a.chat_id, f, &upload_id, &act)
                    .await?;
                uploads.push(json!({ "uploadId": upload_id }));
            }
            let mut args = json!({
                "chatId": a.chat_id,
                "clientId": a.submission_id,
                "text": a.text,
                "mode": if steer { "steer" } else { "queue" },
            });
            if !uploads.is_empty() {
                args["attachments"] = json!(uploads);
            }
            // A page's send carries its card words across; the chat that runs it shows the same
            // card and knows the page wrote the message, not the person. The far side refuses
            // over-long card words, so clip them here the way a local send does: the fallback
            // title is the pinned page's own title, which carries no cap of its own.
            let clip = |s: &Option<String>, n: usize| -> Option<String> {
                s.as_ref().map(|t| t.chars().take(n).collect::<String>())
            };
            let page_title = clip(&a.page_title, 240);
            let page_overview = clip(&a.page_overview, 2000);
            if a.origin.unwrap_or_default() == SendOrigin::Page {
                args["page"] =
                    json!({ "title": page_title.clone(), "overview": page_overview.clone() });
            }
            m.ask_as(&device, "send", args, "this message", &act)
                .await?;
            // The message arrived: this PC has no further use for the staged files.
            if !ids.is_empty() {
                m.drop_staged(&ids, &staged);
            }
            if a.origin.unwrap_or_default() == SendOrigin::Composer {
                let _ = m.inner.db.write(|tx| {
                    tx.execute(
                        "UPDATE mirror_chats SET draft = '', draft_revision = MAX(draft_revision, ?2)
                         WHERE chat_id = ?1",
                        rusqlite::params![a.chat_id, a.draft_revision],
                    )
                });
            }
            let mut submission = json!({
                "id": a.submission_id,
                "chatId": a.chat_id,
                "text": a.text,
                "mode": if steer { "steer" } else { "prompt" },
                "state": "accepted",
                "origin": if a.origin.unwrap_or_default() == SendOrigin::Page { "page" } else { "typed" },
                "createdAt": helm_core::now_ms(),
            });
            // The page's card words ride the echo too: the queue card names the page at once,
            // before the mirror's own sync brings the stored row back.
            if a.origin.unwrap_or_default() == SendOrigin::Page {
                if let Some(t) = page_title {
                    submission["pageTitle"] = json!(t);
                }
                if let Some(o) = page_overview {
                    submission["pageOverview"] = json!(o);
                }
            }
            Ok(json!({
                "submission": submission,
                "chat": m.chat_json(&a.chat_id)?,
            }))
        })
    }

    fn stop(&self, chat_id: String) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            let device = m.device_of(&chat_id)?;
            m.check_live(&device)?;
            let act = [m.acting(&chat_id)?];
            m.ask_as(
                &device,
                "stop",
                json!({ "chatId": chat_id }),
                "the stop",
                &act,
            )
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
            if let Some(g) = a.goto {
                args["goto"] = json!(g);
            }
            let act = [m.acting(&a.chat_id)?];
            m.ask_as(&device, "answer", args, "that answer", &act)
                .await?;
            Ok(Value::Null)
        })
    }

    fn set_model(&self, a: ChatSetModelArgs) -> Fut<Res> {
        let m = self.clone();
        Box::pin(async move {
            // Either part may be absent — the picker's effort/thinking row changes only the
            // effort, and the model row only the model, as a chat that runs here takes them.
            let mut args = json!({ "chatId": a.chat_id });
            if let Some(model) = &a.model {
                args["model"] = json!(model);
            }
            if let Some(e) = &a.effort {
                args["effort"] = json!(e);
            }
            let (model, effort) = (a.model.clone(), a.effort.clone());
            m.change(&a.chat_id, "set_model", args, move |rec| {
                if let Some(model) = model {
                    rec.insert("model".into(), json!(model));
                }
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

    /// Carry one of the chat menu's ops to the device that runs the chat, answering with what
    /// the same op answers here, so the menu cannot tell the difference.
    fn chat_op(&self, op: &str, args: Value) -> Fut<Res> {
        let m = self.clone();
        let op = op.to_owned();
        Box::pin(async move {
            let chat_id = args
                .get("chatId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let device = m.device_of(&chat_id)?;
            if let Some(answer) = m.shown_op(&op, &device, &chat_id, &args).await {
                return answer;
            }
            // Reads answer while the device's run switch is off.
            if let Some(wire) = read_op(&op) {
                m.check_up(&device)?;
                let answer = m.ask_as(&device, wire, args, "that read", &[]).await?;
                // A picture the file view left out because one frame cannot carry it: bring
                // the whole picture across in chunks, so the viewer gets what a local read
                // would answer.
                if op == "file_read"
                    && answer.get("oversize").and_then(Value::as_bool) == Some(true)
                {
                    return m.complete_oversize_picture(&device, &chat_id, answer).await;
                }
                return Ok(answer);
            }
            // Moving, nesting, pinning and reordering answer with sidebar rows; the one this
            // call is about lives in this copy, so the copy is patched and no rows come back.
            if op == "chat_organize" {
                m.check_live(&device)?;
                let act = m.acting(&chat_id)?;
                let rows = m
                    .ask_as(&device, "organize", args, "that change", &[act])
                    .await?;
                m.apply_organize(&chat_id, &device, &rows);
                return Ok(json!([]));
            }
            // Some actions answer with their own result, not the chat record: the control
            // that asked reads that answer exactly as it does on a chat that runs here.
            if let Some(wire) = match op.as_str() {
                "chat_verbosity_set" => Some("verbosity_set"),
                "tool_stop" => Some("tool_stop"),
                "chat_fork" => Some("fork"),
                "chat_roll_over" => Some("roll_over"),
                "chat_export_markdown" => Some("export_markdown"),
                // The two-phase edit answers with its new submission and the chat, not the
                // chat record alone; the copies' transcript rewinds follow the device's
                // refresh news. A delete answers null; the device's `chat_removed` news is
                // what takes the copy away here. A side chat's resolve is the same shape:
                // promote answers with the record and its `chat` news shows it on the copy,
                // discard answers null and the `chat_removed` news clears it.
                "message_resend" => Some("resend"),
                "chat_delete" => Some("delete"),
                "side_chat_resolve" => Some("side_resolve"),
                _ => None,
            } {
                m.check_live(&device)?;
                let act = m.acting(&chat_id)?;
                let answer = m.ask_as(&device, wire, args, "that change", &[act]).await?;
                // A fork or a roll-over makes a new chat there: hold its record in the copy
                // now, so the control that opens it finds it before the device's news.
                if matches!(wire, "fork" | "roll_over") && answer.get("id").is_some() {
                    m.inner
                        .apply_chats(&device, std::slice::from_ref(&answer), None);
                }
                return Ok(answer);
            }
            // Staging answers with the Changes view, not the chat record: the panel updates
            // itself from the answer exactly as it does on a chat that runs here.
            if op == "changes_stage" || op == "changes_unstage" {
                m.check_live(&device)?;
                let act = m.acting(&chat_id)?;
                let wire = if op == "changes_stage" {
                    "stage"
                } else {
                    "unstage"
                };
                return m.ask_as(&device, wire, args, "that change", &[act]).await;
            }
            let wire = match op.as_str() {
                "chat_compact" => "compact",
                "unread_mark" => "mark_unread",
                "chat_mute" => "mute",
                "chat_set_permission_mode" => "set_permission",
                "chat_warm" => "warm",
                "attention_snooze" => "snooze",
                "attention_address" => "address",
                _ => return Err(helm_chats::remote::elsewhere(&m.device_name(&device))),
            };
            let muted = if op == "chat_mute" {
                args.get("muted").and_then(Value::as_bool)
            } else {
                None
            };
            m.change(&chat_id, wire, args, move |rec| {
                if let Some(on) = muted {
                    rec.insert("muted".into(), json!(on));
                }
            })
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

    fn blob(&self, chat_id: String, r: Value) -> Fut<Result<RemoteBlob, ApiError>> {
        let m = self.clone();
        Box::pin(async move { m.read_blob(&chat_id, r, MAX_BLOB_BYTES).await })
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
                .filter(|c| !c.is_null())
                .ok_or_else(|| {
                    ApiError::new("unavailable", "That device's models are not known yet.")
                })
        })
    }
}
