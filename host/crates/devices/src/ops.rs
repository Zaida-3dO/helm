//! The device `api` ops.

use crate::identity::{self, DeviceError};
use crate::mirror::{self, Mirror, MirrorSlot};
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
        .async_op("devices_open_chat", devices_open_chat);
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
    let (uid, name) = db
        .write(|tx| identity::rename(tx, &a.name))
        .map_err(fail)?;
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
            .query_row("SELECT id FROM profiles WHERE is_home = 1", [], |r| r.get(0))
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
                "This profile is not linked to a person on the hub, so it can't start a chat on another device.",
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
