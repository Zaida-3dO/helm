//! The device `api` ops.

use crate::identity::{self, DeviceError};
use crate::mirror::{self, Mirror, MirrorSlot};
use base64::Engine as _;
use helm_chats::clock::Clock;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("device_get", OpClass::Blocking, get)
        .blocking("device_rename", OpClass::Maintenance, rename)
        .blocking("devices_list", OpClass::Blocking, devices_list)
        .blocking("devices_chats", OpClass::Blocking, devices_chats)
        .async_op("devices_create_chat", devices_create_chat)
        .async_op("devices_open_chat", devices_open_chat)
        .async_op("devices_attach", devices_attach)
        .async_op("devices_pages", devices_pages)
        .async_op("devices_page_open", devices_page_open)
        .blocking("devices_discard", OpClass::Blocking, devices_discard)
        .blocking(
            "devices_consent_list",
            OpClass::Blocking,
            devices_consent_list,
        )
        .async_op("devices_consent_decide", devices_consent_decide)
        .async_op("devices_consent_state", devices_consent_state)
        .async_op("devices_share_chat", devices_share_chat)
        .async_op("devices_move_profile", devices_move_profile)
        .async_op("devices_projects_refresh", devices_projects_refresh)
        .async_op("devices_project_create", devices_project_create)
        .async_op("devices_project_set", devices_project_set)
        .async_op("devices_submission_action", devices_submission_action)
        .async_op("devices_item_links", devices_item_links)
        .async_op("devices_attention", devices_attention)
        .async_op("devices_composer_defaults", devices_composer_defaults)
        .async_op("devices_board_connection", devices_board_connection)
        .async_op("devices_board_read", devices_board_read)
        .async_op("devices_board_cancel", devices_board_cancel)
        .blocking("devices_typed_on", OpClass::Blocking, devices_typed_on)
        .async_op("devices_file_save", devices_file_save)
        .async_op("devices_forward_offer", devices_forward_offer)
        .async_op("devices_forward_open", devices_forward_open)
        .blocking("devices_forwards", OpClass::Blocking, devices_forwards)
        .blocking(
            "devices_forward_close",
            OpClass::Blocking,
            devices_forward_close,
        );
}

fn fail(e: DeviceError) -> ApiError {
    match e {
        DeviceError::Bad(m) => ApiError::bad_args(m),
        DeviceError::Db(m) => ApiError::internal(m),
    }
}

fn get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let (uid, name) = db
        .write(identity::ensure)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(json!({ "uid": uid, "name": name }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenameArgs {
    name: String,
}

fn rename(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: RenameArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let (uid, name) = db.write(|tx| identity::rename(tx, &a.name)).map_err(fail)?;
    ctx.events.emit(AppEvent::new("device_changed"));
    Ok(json!({ "uid": uid, "name": name }))
}

// ---------------------------------------------------------------------------------------------
// The household's devices and their chats
// ---------------------------------------------------------------------------------------------

fn mirror_of(ctx: &OpCtx) -> Option<Mirror> {
    ctx.services.get::<MirrorSlot>().and_then(|s| s.get())
}

fn no_hub() -> ApiError {
    ApiError::new("offline", "This PC is not connected to a household hub.")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// `devices_list`: every device of the household (this one first) with how it reads right now:
/// `state` is `live`, `readonly`, `offline` or `reconnecting`.
fn devices_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    serde_json::from_value::<NoArgs>(if args.is_null() { json!({}) } else { args })
        .map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let up = mirror_of(ctx).is_some_and(|m| m.line_is_up());
    db.snapshot(|tx| mirror::devices_view(tx, up))
        .map_err(|e| ApiError::internal(e.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatsArgs {
    /// The device whose chats show, or `all` for every device held on this PC.
    device_uid: String,
    /// The profile whose chats show (its own and those shared with it); none shows every chat.
    #[serde(default)]
    profile_id: Option<String>,
    /// Widen the copy past the profile: `any` shows every person's chat; a person id shows
    /// that person's. Absent keeps the profile's own view.
    #[serde(default)]
    who: Option<String>,
    #[serde(default)]
    archived: Option<bool>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

/// A `deviceUid` naming every device this PC holds a copy of, instead of one.
const ALL_DEVICES: &str = "all";

/// The most chats one `devices_chats` page carries.
const MAX_CHATS_PAGE: usize = 200;

fn project_rows(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut q =
        conn.prepare("SELECT id, name, COALESCE(color, ''), kind FROM projects ORDER BY name")?;
    let rows = q.query_map([], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "name": r.get::<_, String>(1)?,
            "colour": r.get::<_, String>(2)?,
            "kind": r.get::<_, String>(3)?,
        }))
    })?;
    rows.collect()
}

/// `devices_chats`: a page of a device's chats for a profile. This device's own come from the
/// chats table; another's from this PC's copy of them. `deviceUid: "all"` and `who: "any"` (or a
/// person id) are the sidebar's widened views over the copy: the copy always holds everything,
/// the view only picks what shows.
fn devices_chats(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatsArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let archived = a.archived.unwrap_or(false);
    let rev = ctx.services.get::<Clock>().map_or(0, |c| c.current());
    let (all, projects) = db
        .snapshot(|tx| {
            let this = identity::self_uid(tx).unwrap_or_default();
            if a.device_uid == this {
                let mut chats: Vec<_> = helm_chats::store::list_for_devices(tx, rev, None, 5000)?
                    .into_iter()
                    .filter(|c| c.archived == archived)
                    .filter(|c| match a.profile_id.as_deref() {
                        Some(p) => c.profile_id == p || c.shared_with.iter().any(|s| s == p),
                        None => true,
                    })
                    .collect();
                chats.sort_by_key(|c| std::cmp::Reverse(c.last_activity_at));
                let chats = chats
                    .iter()
                    .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                Ok::<_, rusqlite::Error>((chats, project_rows(tx)?))
            } else {
                // The copy holds every chat of every device; the view only chooses what shows.
                let mut view = mirror::ChatView::for_profile(tx, a.profile_id.as_deref())?;
                match a.who.as_deref() {
                    Some("any") => view.who = mirror::WhoFilter::Any,
                    Some(p) if !p.trim().is_empty() => {
                        view.who = mirror::WhoFilter::Person(p.trim().to_owned())
                    }
                    _ => {}
                }
                let all_devices = a.device_uid == ALL_DEVICES;
                view.device = (!all_devices).then(|| a.device_uid.clone());
                let projects = if all_devices {
                    mirror::mirror_projects_all(tx)?
                } else {
                    mirror::mirror_projects(tx, &a.device_uid)?
                };
                Ok((mirror::chats_for_view(tx, &view, archived)?, projects))
            }
        })
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let offset = a.offset.unwrap_or(0);
    let limit = a.limit.unwrap_or(50).clamp(1, MAX_CHATS_PAGE);
    let total = all.len();
    let chats: Vec<Value> = all.into_iter().skip(offset).take(limit).collect();
    Ok(json!({
        "chats": chats,
        "projects": projects,
        "more": offset + chats.len() < total,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateChatArgs {
    device_uid: String,
    project_id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}

/// The hub person of the profile the sidebar shows (the home profile when it shows all).
fn active_person(conn: &Connection) -> Option<String> {
    let active = helm_core::prefs::read(conn, "activeProfile")
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| s != "all");
    let profile: String = match active {
        Some(id) => id,
        None => conn
            .query_row("SELECT id FROM profiles WHERE is_home = 1", [], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()?,
    };
    conn.query_row(
        "SELECT hub_person_id FROM profiles WHERE id = ?1",
        [profile],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .flatten()
}

/// `devices_create_chat`: start a chat on another device for the active profile's person, and
/// open it here. Uses that device's projects and models.
async fn devices_create_chat(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CreateChatArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    if a.device_uid == mirror.inner.self_uid {
        return Err(ApiError::bad_args(
            "That is this PC: start the chat the usual way.",
        ));
    }
    let person = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(active_person(tx)))
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::new(
                "refused",
                "This profile isn't set up on the hub yet; try again in a moment.",
            )
        })?;
    let id = mirror
        .create_chat(&a.device_uid, &person, &a.project_id, a.model, a.effort)
        .await?;
    Ok(json!({ "chatId": id }))
}

/// The person the sidebar acts as, or a refusal in words.
fn acting_person_here(ctx: &OpCtx) -> Result<String, ApiError> {
    let db = ctx.services.need::<Database>()?;
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(active_person(tx)))
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::new(
                "refused",
                "This profile isn't set up on the hub yet; try again in a moment.",
            )
        })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceArgs {
    device_uid: String,
}

/// `devices_projects_refresh`: ask a device for its projects again and answer with them as
/// `devices_chats` lists them. A device says its projects only when this PC connects to it, so
/// the new-chat page asks when it opens.
async fn devices_projects_refresh(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: DeviceArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror.refresh_projects(&a.device_uid).await?;
    let db = ctx.services.need::<Database>()?;
    let projects = db
        .snapshot(|tx| mirror::mirror_projects(tx, &a.device_uid))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(json!({ "chats": [], "projects": projects, "more": false }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectCreateOpArgs {
    device_uid: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    folder_project_id: Option<String>,
}

/// `devices_project_create`: make a project on another device for the active profile's person.
/// The device makes a board when it has a task board connected and a folder when it has not.
async fn devices_project_create(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ProjectCreateOpArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    let person = acting_person_here(&ctx)?;
    mirror
        .project_create(
            &a.device_uid,
            &person,
            &a.title,
            a.description,
            a.folder_project_id,
        )
        .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectSetOpArgs {
    device_uid: String,
    project_id: String,
    #[serde(default)]
    pinned: Option<bool>,
    #[serde(default)]
    folder_project_id: Option<String>,
}

/// `devices_project_set`: pin a project of another device, or give its board a folder.
async fn devices_project_set(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ProjectSetOpArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    let person = acting_person_here(&ctx)?;
    mirror
        .project_set(
            &a.device_uid,
            &person,
            &a.project_id,
            a.pinned,
            a.folder_project_id,
        )
        .await?;
    Ok(json!({}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenChatArgs {
    chat_id: String,
}

/// `devices_open_chat`: listen to a mirrored chat (its rows arrive as events), waiting for the
/// first of them. Reading the copy needs no call; an offline device answers at once with nothing
/// to wait for.
async fn devices_open_chat(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: OpenChatArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror
        .open_chat(&a.chat_id)
        .await
        .map_err(|e| ApiError::new(&e.code, e.message))?;
    Ok(json!({}))
}

// ---------------------------------------------------------------------------------------------
// Attachments for a chat that runs on another device
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttachArgs {
    chat_id: String,
    /// `image`, `file` or `text`.
    kind: String,
    name: String,
    /// The image's mime type and bytes, base64 (an image).
    #[serde(default)]
    mime: Option<String>,
    #[serde(default)]
    data: Option<String>,
    /// The file's path on this PC (a file): it is read when the message goes, not now.
    #[serde(default)]
    path: Option<String>,
    /// The words themselves (a text): saved here under the id, sent as a plain-text file.
    #[serde(default)]
    text: Option<String>,
}

/// `devices_discard`: take back one staged attachment of a chat that runs on another device,
/// the row and, for a picture or a pasted text, the file saved here.
fn devices_discard(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Args {
        chat_id: String,
        attachment_id: String,
    }
    let a: Args = serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let dir = mirror::staged_dir(db.path());
    let staged = db
        .write(|tx| {
            let row: Option<(String, String)> = tx
                .query_row(
                    "SELECT kind, target FROM mirror_attachments WHERE id = ?1 AND chat_id = ?2",
                    [&a.attachment_id, &a.chat_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            tx.execute(
                "DELETE FROM mirror_attachments WHERE id = ?1 AND chat_id = ?2",
                [&a.attachment_id, &a.chat_id],
            )?;
            Ok::<_, rusqlite::Error>(row)
        })
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some((kind, target)) = staged {
        if kind == "image" || kind == "text" {
            let _ = std::fs::remove_file(dir.join(target));
        }
    }
    Ok(json!({}))
}

/// `devices_attach`: stage an image or a file on this PC for the next message to a chat that
/// runs on another device. The composer uses this instead of the chats' own `attachment_put`,
/// which only knows chats that run here. Answers `{attachmentId}`; the send names it.
async fn devices_attach(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: AttachArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let id = uuid::Uuid::new_v4().to_string();
    match a.kind.as_str() {
        "image" => {
            let data = a
                .data
                .ok_or_else(|| ApiError::bad_args("An image comes with its bytes."))?;
            // Base64 is about 4/3 of the bytes: refuse an oversized payload before decoding it.
            if data.len() > helm_chats::attachments::MAX_IMAGE_BYTES / 3 * 4 + 8 {
                return Err(ApiError::new("too_large", "An image can be at most 25 MB."));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.as_bytes())
                .map_err(|_| ApiError::bad_args("bytes must be base64."))?;
            let dir = mirror::staged_dir(db.path());
            // The same checks as a local attachment: mime, size, pixels, stillness. The file
            // is written now, under a fresh id, and stays until the send has gone through.
            let staged = match helm_chats::attachments::stage_image(
                &dir,
                &a.name,
                a.mime.as_deref().unwrap_or(""),
                &bytes,
            ) {
                Ok(s) => s,
                Err(r) => return Err(ApiError::from(r)),
            };
            let spec = Staged {
                id: &staged.id,
                chat_id: &a.chat_id,
                kind: "image",
                name: &staged.name,
                mime: &staged.mime,
                target: &staged.id,
                bytes: bytes.len() as i64,
            };
            match stage_row(&db, &spec) {
                Ok(()) => Ok(json!({ "attachmentId": staged.id })),
                Err(e) => {
                    // No row: the file would be left behind with nothing to name it.
                    let _ = std::fs::remove_file(dir.join(&staged.id));
                    Err(e)
                }
            }
        }
        "file" => {
            let path = a
                .path
                .ok_or_else(|| ApiError::bad_args("A file comes with its path on this PC."))?;
            let meta = std::fs::metadata(&path)
                .map_err(|_| ApiError::new("not_found", "That file is not on this PC."))?;
            if !meta.is_file() {
                return Err(ApiError::bad_args("Only files can be attached."));
            }
            if meta.len() > helm_chats::attachments::MAX_FILE_BYTES as u64 {
                return Err(ApiError::new("too_large", "A file can be at most 25 MB."));
            }
            let spec = Staged {
                id: &id,
                chat_id: &a.chat_id,
                kind: "file",
                name: &a.name,
                mime: "",
                target: &path,
                bytes: meta.len() as i64,
            };
            stage_row(&db, &spec)?;
            Ok(json!({ "attachmentId": id }))
        }
        "text" => {
            // A long paste on a chat that runs elsewhere: the words become a plain-text file
            // staged here, exactly what the composer's long-paste path does for a local chat.
            let text = a
                .text
                .ok_or_else(|| ApiError::bad_args("A text comes with its words."))?;
            if text.len() > helm_chats::attachments::MAX_FILE_BYTES {
                return Err(ApiError::new("too_large", "A file can be at most 25 MB."));
            }
            let dir = mirror::staged_dir(db.path());
            helm_core::privfs::create_private_dir_all(&dir)
                .map_err(|_| ApiError::internal("The staged folder could not be made."))?;
            std::fs::write(dir.join(&id), text.as_bytes())
                .map_err(|_| ApiError::internal("The text could not be staged."))?;
            let spec = Staged {
                id: &id,
                chat_id: &a.chat_id,
                kind: "text",
                name: &a.name,
                mime: "text/plain",
                target: &id,
                bytes: text.len() as i64,
            };
            match stage_row(&db, &spec) {
                Ok(()) => Ok(json!({ "attachmentId": id })),
                Err(e) => {
                    let _ = std::fs::remove_file(dir.join(&id));
                    Err(e)
                }
            }
        }
        other => Err(ApiError::bad_args(format!(
            "kind must be image, file or text, not {other}."
        ))),
    }
}

/// One attachment checked and ready to record for a mirrored chat.
struct Staged<'a> {
    id: &'a str,
    chat_id: &'a str,
    kind: &'a str,
    name: &'a str,
    mime: &'a str,
    /// A name in this PC's staged folder (picture) or the file's own path (file).
    target: &'a str,
    bytes: i64,
}

/// Record one staged attachment for a chat that is a copy of another device's. Only mirrored
/// chats take one; a chat keeps at most the send limit of them waiting.
fn stage_row(db: &Database, s: &Staged<'_>) -> Result<(), ApiError> {
    let (chat_id, id, kind, name, mime, target, bytes) =
        (s.chat_id, s.id, s.kind, s.name, s.mime, s.target, s.bytes);
    db.write(|tx| {
        let mirrored: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM mirror_chats WHERE chat_id = ?1)",
            [chat_id],
            |r| r.get(0),
        )?;
        if !mirrored {
            return Ok(Err(ApiError::new(
                "not_found",
                "That chat does not run on another device of this household.",
            )));
        }
        let waiting: i64 = tx.query_row(
            "SELECT count(*) FROM mirror_attachments WHERE chat_id = ?1",
            [chat_id],
            |r| r.get(0),
        )?;
        if waiting >= helm_chats::attachments::MAX_PER_MESSAGE as i64 {
            return Ok(Err(ApiError::new(
                "refused",
                "A message can carry at most 20 attachments.",
            )));
        }
        tx.execute(
            "INSERT INTO mirror_attachments(id, chat_id, kind, name, mime, target, bytes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![id, chat_id, kind, name, mime, target, bytes, helm_core::now_ms()],
        )?;
        Ok(Ok(()))
    })
    .map_err(|e: rusqlite::Error| ApiError::internal(e.to_string()))?
}

// ---------------------------------------------------------------------------------------------
// Allow cards of a chat that runs on another device
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConsentIdArgs {
    chat_id: String,
}

/// `devices_consent_list`: the Allow cards waiting on a mirrored chat, as this PC last heard them
/// from the device that runs it. The window re-reads this on each `consent` event, exactly as it
/// does for a chat that runs here.
fn devices_consent_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ConsentIdArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let db = ctx.services.need::<Database>()?;
    let cards = db
        .snapshot(|tx| {
            let stored: Option<String> = tx
                .query_row(
                    "SELECT consent FROM mirror_chats WHERE chat_id = ?1",
                    [&a.chat_id],
                    |r| r.get(0),
                )
                .optional()?;
            Ok::<_, rusqlite::Error>(match stored {
                Some(s) => serde_json::from_str::<Value>(&s)
                    .ok()
                    .filter(Value::is_array)
                    .unwrap_or_else(|| json!([])),
                None => json!([]),
            })
        })
        .map_err(|e: rusqlite::Error| ApiError::internal(e.to_string()))?;
    Ok(json!({ "pending": cards }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConsentDecideLocalArgs {
    chat_id: String,
    request_id: String,
    decision: String,
    #[serde(default)]
    grant_minutes: Option<i64>,
    #[serde(default)]
    note: Option<String>,
}

/// `devices_consent_decide`: answer an Allow card of a chat that runs on another device. The
/// store on that PC decides by compare-and-set, so the first answer on any device wins; the
/// device then pushes the closed card to every PC holding the chat.
async fn devices_consent_decide(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ConsentDecideLocalArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    if !matches!(a.decision.as_str(), "allow" | "deny") {
        return Err(ApiError::bad_args("A decision is allow or deny."));
    }
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror
        .consent_decide(
            &a.chat_id,
            &a.request_id,
            &a.decision,
            a.grant_minutes,
            a.note,
        )
        .await?;
    Ok(json!({}))
}

/// `devices_consent_state`: ask the device that runs a mirrored chat where its Allow requests
/// stand, so a tool row on the copy can say Allowed or Refused as one does on a local chat.
async fn devices_consent_state(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ConsentIdArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror.consent_states(&a.chat_id).await
}

// ---------------------------------------------------------------------------------------------
// Sharing and profile moves for a chat that runs on another device
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShareChatArgs {
    chat_id: String,
    /// One of the hub people the device announced in its hello.
    person_id: String,
    shared: bool,
}

/// `devices_share_chat`: show a mirrored chat to one more person on the PC that runs it, or
/// stop. The other chat menus cross the wire through the intercepted local ops; the sharing ops
/// name a list of chats, not one, so they need an op of their own here.
async fn devices_share_chat(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ShareChatArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror
        .share_chat(&a.chat_id, &a.person_id, a.shared)
        .await?;
    Ok(json!({}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MoveProfileArgs {
    chat_id: String,
    /// One of the profiles the device listed in its hello.
    profile_id: String,
}

/// `devices_move_profile`: hand a mirrored chat to one of the profiles on the PC that runs it.
async fn devices_move_profile(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: MoveProfileArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror.move_profile(&a.chat_id, &a.profile_id).await?;
    Ok(json!({}))
}

// ---------------------------------------------------------------------------------------------
// Queue actions and sidebar reads for chats that run on other devices
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmissionActionArgs {
    chat_id: String,
    submission_id: String,
    /// `withdraw` or `recover`.
    action: String,
}

/// `devices_submission_action`: withdraw or recover one queued message of a chat that runs on
/// another PC. The queue lives there; the change comes back through the copy's `pending` feed.
async fn devices_submission_action(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SubmissionActionArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    if !matches!(a.action.as_str(), "withdraw" | "recover") {
        return Err(ApiError::bad_args("An action is withdraw or recover."));
    }
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror
        .submission_action(&a.chat_id, &a.submission_id, &a.action)
        .await?;
    Ok(json!({}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatIdsListArgs {
    chat_ids: Vec<String>,
}

fn parse_chat_ids(args: Value) -> Result<Vec<String>, ApiError> {
    let a: ChatIdsListArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    if a.chat_ids.len() > 500 {
        return Err(ApiError::bad_args("Ask for at most 500 chats."));
    }
    Ok(a.chat_ids)
}

/// `devices_item_links`: the board-item links of named mirrored chats, read from the PCs that
/// run them. The answer is the same shape as the local `item_links_for_chats`, so the sidebar
/// cannot tell a mirror chat's links from its own.
async fn devices_item_links(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let ids = parse_chat_ids(args)?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror.item_links(&ids).await
}

/// `devices_attention`: the needs-you and idle-check flag rows of named mirrored chats, read
/// from the PCs that run them, so a copy gets the same markers a local chat shows.
async fn devices_attention(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let ids = parse_chat_ids(args)?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror.attention_rows(&ids).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ComposerDefaultsArgs {
    chat_id: String,
    /// Absent reads the device's new-chat model/effort/reply-length; present saves them.
    #[serde(default)]
    patch: Option<Value>,
}

/// `devices_composer_defaults`: the reply-length follow and the "make default" of the model
/// picker, for a chat that runs on another PC — read or saved on the PC that runs the chat.
async fn devices_composer_defaults(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ComposerDefaultsArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    if let Some(patch) = &a.patch {
        // Only the three fields the picker owns; anything else is refused at the wire.
        let allowed = matches!(
            patch.as_object().map(|m| {
                m.keys()
                    .all(|k| matches!(k.as_str(), "model" | "effort" | "responseVerbosity"))
            }),
            Some(true)
        );
        if !allowed {
            return Err(ApiError::bad_args(
                "The defaults are the model, the effort and the reply length.",
            ));
        }
    }
    mirror.composer_defaults(&a.chat_id, a.patch.as_ref()).await
}

// ---------------------------------------------------------------------------------------------
// The task board of the PC that runs a mirrored chat
// ---------------------------------------------------------------------------------------------

/// The chat a board call names, and what is left of the args. The board's own read parameters
/// pass through untouched: the device that runs the chat answers with its own board op, and its
/// address and token never come across.
fn split_board_args(args: Value) -> Result<(String, Value), ApiError> {
    let mut args = args;
    let map = args
        .as_object_mut()
        .ok_or_else(|| ApiError::bad_args("The board call names a chat."))?;
    let chat_id = match map.remove("chatId") {
        Some(Value::String(s)) if !s.trim().is_empty() => s,
        _ => return Err(ApiError::bad_args("The board call names a chat.")),
    };
    Ok((chat_id, Value::Object(map.clone())))
}

/// `devices_pages`: the pages pinned on the other PCs of the household, the acting person's own
/// first, one entry per PC: `{devices: [{deviceUid, name, pages, problem?}]}`. A PC that is
/// offline, or has its switch off, answers with a `problem` in words and no pages.
async fn devices_pages(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    serde_json::from_value::<NoArgs>(if args.is_null() { json!({}) } else { args })
        .map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    let db = ctx.services.need::<Database>()?;
    // With no hub person to act as yet there is nothing to ask.
    let Ok(person) = mirror.acting_now() else {
        return Ok(json!({ "devices": [] }));
    };
    let peers = db
        .snapshot(|tx| mirror::load_devices(tx))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut asks = Vec::new();
    for d in peers.into_iter().filter(|d| !d.is_self) {
        let (m, person) = (mirror.clone(), person.clone());
        asks.push(tokio::spawn(async move {
            let (pages, problem) = match m.pages_on(&d.uid, &person).await {
                Ok(v) => (v["pages"].clone(), Value::Null),
                Err(e) => (json!([]), json!({ "code": e.code, "message": e.message })),
            };
            json!({ "deviceUid": d.uid, "name": d.name, "pages": pages, "problem": problem })
        }));
    }
    let mut devices = Vec::new();
    for a in asks {
        if let Ok(v) = a.await {
            devices.push(v);
        }
    }
    Ok(json!({ "devices": devices }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageOpenArgs {
    device_uid: String,
    page_id: String,
}

/// `devices_page_open`: a page pinned on another PC, brought across and opened in a session
/// here. Answers as `page_open` does (the session to load, its saved state, its pin there), so
/// the page frame runs it the same way. The page's saved state is kept on this PC under the
/// device and the pin; its messages go to the chat the pin names, which is the chat there.
/// The network follows the pin as always, and the pin — with the decision — lives on the PC
/// that owns the page: a page that came from a pin there runs online.
async fn devices_page_open(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PageOpenArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    let db = ctx.services.need::<Database>()?;
    let server = ctx.services.need::<helm_pages::PageServer>()?;
    let person = mirror.acting_now()?;
    let b = mirror
        .page_bundle(&a.device_uid, &person, &a.page_id)
        .await?;
    let key = format!("remote:{}:{}", a.device_uid, a.page_id);
    let shown = b.pin["path"].as_str().unwrap_or_default().to_owned();
    let network = b.pin.get("id").and_then(Value::as_str).is_some();
    let opened = server
        .open_bundle(&key, &shown, b.html, b.files, network)
        .map_err(|e| ApiError::new("refused", e.message().to_owned()))?;
    let (state, revision) = db
        .snapshot(|tx| helm_pages::store::state_get(tx, &key))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut out = opened.to_json();
    out["title"] = json!(b.title);
    out["state"] = state;
    out["stateRevision"] = json!(revision);
    out["pins"] = json!([b.pin]);
    Ok(out)
}

/// `devices_board_connection`: whether the PC that runs a mirrored chat has a task board set
/// up, without its address or token. The Tasks tile shows only when it answers ready.
async fn devices_board_connection(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let (chat_id, _) = split_board_args(args)?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror.board_connection(&chat_id).await
}

/// `devices_board_read`: one read of the task board of the PC that runs a mirrored chat, sent
/// as that PC's own `board_read` and answered the same way, so the Tasks view cannot tell the
/// difference. The `action` and its parameters are the board's own.
async fn devices_board_read(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let (chat_id, params) = split_board_args(args)?;
    if params.get("action").and_then(Value::as_str).is_none() {
        return Err(ApiError::bad_args("The board read names an action."));
    }
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror.board_read(&chat_id, params).await
}

/// `devices_board_cancel`: stop a board read this PC started, on the PC that runs the chat.
async fn devices_board_cancel(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let (chat_id, mut params) = split_board_args(args)?;
    let Some(request_id) = params
        .as_object_mut()
        .and_then(|m| m.remove("requestId"))
        .and_then(|v| v.as_str().map(str::to_owned))
    else {
        return Err(ApiError::bad_args("The cancel names a request."));
    };
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    mirror.board_cancel(&chat_id, &request_id).await
}

// ---------------------------------------------------------------------------------------------
// Where the person is, and files of a chat that runs on another device
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TypedOnArgs {
    chat_id: String,
}

/// `devices_typed_on`: the PC the last message of a chat that runs here was typed on, when that
/// was another device: `{deviceUid, name, at}`, or `null` when it was typed on this PC or nothing
/// is known since Helm started.
fn devices_typed_on(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: TypedOnArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let typed = ctx
        .services
        .get::<crate::EventTap>()
        .and_then(|t| t.service())
        .and_then(|s| s.typed_on(&a.chat_id));
    Ok(serde_json::to_value(typed).unwrap_or(Value::Null))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileSaveArgs {
    chat_id: String,
    /// The file on the PC that runs the chat, inside the chat's project folder there.
    path: String,
}

/// `devices_file_save`: a copy of one file of a chat that runs on another device, saved on this
/// PC where the person chooses: `{path}`, or `{path: null}` when the dialog is cancelled. The
/// file comes across in chunks, at most 64 MiB, and only from inside the chat's project folder.
async fn devices_file_save(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: FileSaveArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    if mirror.owner(&a.chat_id).is_none() {
        return Err(ApiError::new(
            "not_found",
            "That chat does not run on another device of this household.",
        ));
    }
    let saver = ctx
        .services
        .get::<helm_chats::output_save::FileSaver>()
        .ok_or_else(|| ApiError::new("unavailable", "Saving is not available here."))?;
    let blob = mirror
        .read_blob(
            &a.chat_id,
            json!({ "file": a.path }),
            crate::wire::MAX_BLOB_BYTES,
        )
        .await?;
    let name = if blob.name.is_empty() {
        "file".to_owned()
    } else {
        blob.name
    };
    helm_chats::remote::save_here(saver, name, blob.bytes).await
}

// ---------------------------------------------------------------------------------------------
// Ports forwarded between PCs
// ---------------------------------------------------------------------------------------------

fn service_of(ctx: &OpCtx) -> Option<crate::Service> {
    ctx.services
        .get::<crate::EventTap>()
        .and_then(|t| t.service())
}

fn wire(e: crate::wire::WireError) -> ApiError {
    ApiError::new(&e.code, e.message)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForwardArgs {
    chat_id: String,
    port: u16,
    #[serde(default)]
    title: Option<String>,
}

/// The longest title a forward keeps.
const MAX_FORWARD_TITLE: usize = 80;

/// A forward's title as lists show it: one line, at most [`MAX_FORWARD_TITLE`] characters.
fn forward_title(t: Option<String>) -> String {
    t.unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_FORWARD_TITLE)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A forward as the window lists it: the engine's fields and the name of the PC the port is on.
fn forward_json(mirror: Option<&Mirror>, f: crate::tunnel::ViewInfo) -> Value {
    let name = mirror
        .map(|m| m.device_name(&f.device_uid))
        .unwrap_or_else(|| f.device_uid.clone());
    let mut v = serde_json::to_value(f).unwrap_or_else(|_| json!({}));
    v["deviceName"] = json!(name);
    v
}

/// `devices_forward_offer`: an agent of a chat that runs here offers a port of this PC to the
/// person: `{port, title, typedOn: {deviceUid, name} | null, samePc}`. When their last message
/// came from another PC, that PC opens the forward when the offer arrives; this checks now what
/// that open would be refused for (the port policy, nothing listening, the switch off), so the
/// agent hears it in words. When they type here nothing needs forwarding.
async fn devices_forward_offer(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ForwardArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    if mirror_of(&ctx).is_some_and(|m| m.owner(&a.chat_id).is_some()) {
        return Err(ApiError::new(
            "refused",
            "That chat runs on another PC; its ports are offered there.",
        ));
    }
    let service = service_of(&ctx);
    let reserved = service
        .as_ref()
        .map(|s| s.reserved_ports())
        .unwrap_or_default();
    crate::tunnel::check_port(a.port, &reserved).map_err(wire)?;
    let typed = service.as_ref().and_then(|s| s.typed_on(&a.chat_id));
    if let Some(s) = &service {
        if typed.is_some() && !s.run_allowed() {
            return Err(ApiError::new(
                "not_allowed",
                "This PC doesn't let other devices use its chats, so its ports can't be opened from another PC. Turn on \"Let other devices use chats on this PC\" first.",
            ));
        }
        s.tunnels().check(a.port, &reserved).await.map_err(wire)?;
    }
    Ok(json!({
        "port": a.port,
        "title": forward_title(a.title),
        "typedOn": typed.as_ref().map(|t| json!({ "deviceUid": t.device_uid, "name": t.name })),
        "samePc": typed.is_none(),
    }))
}

/// `devices_forward_open`: forward a port of the PC that runs a copied chat to this PC's
/// loopback, as `Forward`. The same port of the same PC already forwarded is answered as it is.
async fn devices_forward_open(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ForwardArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(&ctx).ok_or_else(no_hub)?;
    let tunnels = service_of(&ctx).ok_or_else(no_hub)?.tunnels();
    let device = mirror.owner(&a.chat_id).ok_or_else(|| {
        ApiError::new(
            "not_found",
            "That chat does not run on another device of this household.",
        )
    })?;
    // The PC there checks again, with its own listeners; this spares a round trip.
    crate::tunnel::check_port(a.port, &[]).map_err(wire)?;
    if let Some(f) = tunnels.viewing(&device, a.port) {
        return Ok(forward_json(Some(&mirror), f));
    }
    let (device, forward_id) = mirror.forward_open(&a.chat_id, a.port).await?;
    let spec = crate::tunnel::ViewSpec {
        host_uid: device.clone(),
        forward_id: forward_id.clone(),
        port: a.port,
        chat_id: a.chat_id,
        title: forward_title(a.title),
    };
    match tunnels.view_start(spec).await {
        Ok(f) => Ok(forward_json(Some(&mirror), f)),
        Err(e) => {
            mirror.forward_close(&device, &forward_id).await;
            Err(wire(e))
        }
    }
}

/// `devices_forwards`: every port forwarded to this PC now, oldest first: `{forwards}`.
fn devices_forwards(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    serde_json::from_value::<NoArgs>(if args.is_null() { json!({}) } else { args })
        .map_err(|e| ApiError::bad_args(e.to_string()))?;
    let mirror = mirror_of(ctx);
    let forwards: Vec<Value> = service_of(ctx)
        .map(|s| s.tunnels().forwards())
        .unwrap_or_default()
        .into_iter()
        .map(|f| forward_json(mirror.as_ref(), f))
        .collect();
    Ok(json!({ "forwards": forwards }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForwardCloseArgs {
    forward_id: String,
}

/// `devices_forward_close`: stop a port forwarded to this PC; the PC it comes from is told.
/// A forward that has already ended is not an error.
fn devices_forward_close(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ForwardCloseArgs =
        serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))?;
    if let Some(s) = service_of(ctx) {
        s.tunnels().view_stop(&a.forward_id);
    }
    Ok(json!({}))
}
