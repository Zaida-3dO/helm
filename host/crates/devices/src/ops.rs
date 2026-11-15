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
        .blocking(
            "devices_consent_list",
            OpClass::Blocking,
            devices_consent_list,
        )
        .async_op("devices_consent_decide", devices_consent_decide)
        .async_op("devices_consent_state", devices_consent_state);
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
    device_uid: String,
    /// The profile whose chats show (its own and those shared with it); none shows every chat.
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    archived: Option<bool>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

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
/// chats table; another's from this PC's copy of them.
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
                Ok((
                    mirror::mirror_chat_list(tx, &a.device_uid, a.profile_id.as_deref(), archived)?,
                    mirror::mirror_projects(tx, &a.device_uid)?,
                ))
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
    /// `image` or `file`.
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
        other => Err(ApiError::bad_args(format!(
            "kind must be image or file, not {other}."
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
                Some(s) => serde_json::from_str::<Value>(&s).unwrap_or_else(|_| json!([])),
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
