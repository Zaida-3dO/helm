//! The profile `api` ops (the table is in the crate docs).

use crate::store::{self, Patch, ProfileError};
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("profiles_list", OpClass::Blocking, list)
        .blocking("profiles_create", OpClass::Maintenance, create)
        .blocking("profiles_update", OpClass::Maintenance, update)
        .blocking("profiles_delete", OpClass::Maintenance, delete)
        .blocking("profiles_set_active", OpClass::Maintenance, set_active)
        .blocking("profiles_assign_chats", OpClass::Maintenance, assign_chats)
        .blocking("profiles_share_chats", OpClass::Maintenance, share_chats)
        .blocking("profiles_chat_ids", OpClass::Blocking, chat_ids)
        .blocking(
            "chat_fields_snapshot",
            OpClass::Blocking,
            chat_fields_snapshot,
        );
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn fail(e: ProfileError) -> ApiError {
    match e {
        ProfileError::Bad(m) => ApiError::bad_args(m),
        ProfileError::NotFound => ApiError::new("not_found", e.message()),
        ProfileError::Db(m) => ApiError::internal(m),
    }
}

fn view(db: &Database) -> Result<Value, ApiError> {
    let v = db
        .snapshot(|tx| store::view(tx))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

/// Run a change, tell the UI, and answer with the new view (plus `extra` fields).
fn changed(
    ctx: &OpCtx,
    f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<Value, ProfileError>,
) -> Result<Value, ApiError> {
    changed_with(ctx, true, f)
}

/// [`changed`]; `wake_sync` false leaves the knowledge loop alone, for a change (which profile
/// the sidebar shows) that moves no layer and no person.
fn changed_with(
    ctx: &OpCtx,
    wake_sync: bool,
    f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<Value, ProfileError>,
) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let extra = db.write(f).map_err(fail)?;
    ctx.events.emit(AppEvent::new("profiles_changed"));
    // A profile change changes which layers sync and who this PC acts for on the hub: wake the
    // knowledge loop the way the app's event tap does for knowledge edits.
    if let Some(t) = ctx
        .services
        .get::<helm_skills::sync::Trigger>()
        .filter(|_| wake_sync)
    {
        t.poke();
    }
    let mut out = view(&db)?;
    if let (Some(o), Some(e)) = (out.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    Ok(out)
}

fn list(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    view(&db)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateArgs {
    name: String,
    #[serde(default)]
    colour: Option<String>,
    #[serde(default)]
    hub_person_id: Option<String>,
}

fn create(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CreateArgs = parse(args)?;
    changed(ctx, |tx| {
        let p = store::create(
            tx,
            &a.name,
            a.colour.as_deref(),
            a.hub_person_id.as_deref(),
            helm_core::now_ms(),
        )?;
        Ok(json!({ "profile": p }))
    })
}

/// Tells an absent field from an explicit `null`.
fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateArgs {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    colour: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    hub_person_id: Option<Option<String>>,
    #[serde(default)]
    about: Option<String>,
}

fn update(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: UpdateArgs = parse(args)?;
    changed(ctx, |tx| {
        let p = store::update(
            tx,
            &a.id,
            Patch {
                name: a.name,
                colour: a.colour,
                hub_person_id: a.hub_person_id,
                about: a.about,
            },
        )?;
        Ok(json!({ "profile": p }))
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdArgs {
    id: String,
}

fn delete(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: IdArgs = parse(args)?;
    changed(ctx, |tx| {
        store::delete(tx, &a.id)?;
        Ok(json!({}))
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActiveArgs {
    id: Option<String>,
}

fn set_active(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ActiveArgs = parse(args)?;
    changed_with(ctx, false, |tx| {
        store::set_active(tx, a.id.as_deref())?;
        Ok(json!({}))
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssignArgs {
    chat_ids: Vec<String>,
    profile_id: String,
}

fn assign_chats(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: AssignArgs = parse(args)?;
    let out = changed(ctx, |tx| {
        let n = store::assign(tx, &a.chat_ids, &a.profile_id)?;
        Ok(json!({ "assigned": n }))
    })?;
    announce(ctx, &a.chat_ids);
    Ok(out)
}

/// The chats' records changed: the window and the devices watching this PC hear each one.
fn announce(ctx: &OpCtx, chat_ids: &[String]) {
    if let Some(rt) = ctx.services.get::<helm_chats::ChatRuntime>() {
        for id in chat_ids {
            helm_chats::submit::announce_chat(&rt, id);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShareArgs {
    chat_ids: Vec<String>,
    profile_id: String,
    shared: bool,
}

fn share_chats(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ShareArgs = parse(args)?;
    let out = changed(ctx, |tx| {
        let n = store::share(tx, &a.chat_ids, &a.profile_id, a.shared)?;
        Ok(json!({ "changed": n }))
    })?;
    announce(ctx, &a.chat_ids);
    Ok(out)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatIdsArgs {
    profile_id: String,
}

/// `chat_fields_snapshot`: a chat's mute, profile and sharing, as the change ring files them around
/// a chat's edit (see [`crate::chat_fields`]).
fn chat_fields_snapshot(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct A {
        chat_id: String,
    }
    let a: A = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    db.snapshot(|tx| crate::chat_fields::snapshot(tx, &a.chat_id))
        .map_err(|e| ApiError::new("not_found", e.to_string()))
}

fn chat_ids(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdsArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let ids = db
        .snapshot(|tx| store::chat_ids_for(tx, &a.profile_id))
        .map_err(fail)?;
    Ok(json!({ "chatIds": ids }))
}
