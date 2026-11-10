//! The saved-for-later `api` ops.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `saved_save` | maintenance | `{chatId?, messageId?, captureKey?, title, body, remindAt?, savedBy?}` to `{item, already}`; emits `saved` |
//! | `saved_list` | blocking | `{state?, query?, page?}` to a [`crate::store::SavedPage`] |
//! | `saved_due` | blocking | `{}` to `{items}`: the open items whose reminder is due now |
//! | `saved_update` | maintenance | `{id, title?, body?, state?, remindAt?, expectRevision?, remove?, byChat?, restore?}` to `{item}` or `{id, removed}`; emits `saved` |
//! | `saved_note` | maintenance | `{id, text, authorType?, authorId?}` to `{note}`; emits `saved` |
//! | `saved_comments` | blocking | `{id}` to `{notes}` (oldest first) |
//!
//! `remove` hides an item and keeps it for a week; `restore` undoes that (the Saved pane's Undo).
//! `savedBy` and `byChat` are set by the chat tools only: a chat may remove just what it saved.

use crate::store::{self, StoreError, Update};
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::{json, Value};

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn fail(e: StoreError) -> ApiError {
    match e {
        StoreError::Bad(m) => ApiError::bad_args(m),
        StoreError::NotFound => ApiError::new("not_found", e.message()),
        StoreError::Conflict => ApiError::conflict(e.message()),
        StoreError::Full => ApiError::new("refused", e.message()),
        StoreError::Host(_) => ApiError::internal(e.message()),
    }
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveArgs {
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    message_id: Option<i64>,
    #[serde(default)]
    capture_key: Option<String>,
    title: String,
    body: String,
    #[serde(default)]
    remind_at: Option<i64>,
    /// The chat whose tool saved this item; it alone may remove it later.
    #[serde(default)]
    saved_by: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    page: i64,
}

/// Tells an absent field (`None`) from an explicit `null` (`Some(None)`).
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
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    state: Option<String>,
    // A present null clears the reminder; a missing field leaves it alone.
    #[serde(default, deserialize_with = "double_option")]
    remind_at: Option<Option<i64>>,
    #[serde(default)]
    expect_revision: Option<i64>,
    #[serde(default)]
    remove: bool,
    /// The chat asking to remove, when a chat is; the user's own removals leave it out.
    #[serde(default)]
    by_chat: Option<String>,
    /// Bring back an item removed in the last week.
    #[serde(default)]
    restore: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NoteArgs {
    id: String,
    text: String,
    #[serde(default)]
    author_type: Option<String>,
    #[serde(default)]
    author_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdArg {
    id: String,
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("saved_save", OpClass::Maintenance, save)
        .blocking("saved_list", OpClass::Blocking, list)
        .blocking("saved_update", OpClass::Maintenance, update)
        .blocking("saved_note", OpClass::Maintenance, note)
        .blocking("saved_comments", OpClass::Blocking, comments)
        .blocking("saved_due", OpClass::Blocking, due);
}

fn save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SaveArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let (item, already) = db
        .write(|tx| {
            store::save(
                tx,
                &store::NewItem {
                    chat_id: a.chat_id.as_deref(),
                    message_id: a.message_id,
                    capture_key: a.capture_key.as_deref(),
                    title: &a.title,
                    body: &a.body,
                    remind_at: a.remind_at,
                    saved_by: a.saved_by.as_deref(),
                },
                helm_core::now_ms(),
            )
        })
        .map_err(fail)?;
    if !already {
        ctx.events
            .emit(AppEvent::new("saved").with("ids", json!([item.id])));
    }
    Ok(json!({"item": item, "already": already}))
}

fn list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ListArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let page = db
        .snapshot(|tx| store::list(tx, a.state.as_deref(), a.query.as_deref(), a.page))
        .map_err(fail)?;
    to_value(&page)
}

fn update(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: UpdateArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    if a.restore {
        let item = db
            .write(|tx| store::restore(tx, &a.id, helm_core::now_ms()))
            .map_err(fail)?;
        ctx.events
            .emit(AppEvent::new("saved").with("ids", json!([item.id.clone()])));
        return Ok(json!({"item": item}));
    }
    if a.remove {
        db.write(|tx| store::remove(tx, &a.id, a.by_chat.as_deref(), helm_core::now_ms()))
            .map_err(fail)?;
        ctx.events
            .emit(AppEvent::new("saved").with("ids", json!([a.id.clone()])));
        return Ok(json!({"id": a.id, "removed": true}));
    }
    let item = db
        .write(|tx| {
            store::update(
                tx,
                &a.id,
                &Update {
                    title: a.title,
                    body: a.body,
                    state: a.state,
                    remind_at: a.remind_at,
                    expect_revision: a.expect_revision,
                },
                helm_core::now_ms(),
            )
        })
        .map_err(fail)?;
    ctx.events
        .emit(AppEvent::new("saved").with("ids", json!([item.id.clone()])));
    Ok(json!({"item": item}))
}

fn note(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: NoteArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let n = db
        .write(|tx| {
            store::add_note(
                tx,
                &a.id,
                a.author_type.as_deref().unwrap_or("user"),
                a.author_id.as_deref(),
                &a.text,
                helm_core::now_ms(),
            )
        })
        .map_err(fail)?;
    ctx.events
        .emit(AppEvent::new("saved").with("ids", json!([n.item_id])));
    Ok(json!({"note": n}))
}

/// The open items whose reminder is due now: what a window that has just started shows.
fn due(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let svc = ctx.services.need::<crate::service::SavedService>()?;
    Ok(json!({"items": svc.due_now(helm_core::now_ms())}))
}

fn comments(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: IdArg = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let notes = db.snapshot(|tx| store::notes(tx, &a.id)).map_err(fail)?;
    to_value(&json!({"notes": notes}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reminder sent as `null` clears it; one left out is left alone; a number sets it.
    #[test]
    fn a_null_reminder_is_told_from_a_missing_one() {
        for (args, want) in [
            (json!({"id": "a"}), None),
            (json!({"id": "a", "remindAt": null}), Some(None)),
            (json!({"id": "a", "remindAt": 5000}), Some(Some(5000))),
        ] {
            let parsed: UpdateArgs = parse(args.clone()).unwrap();
            assert_eq!(parsed.remind_at, want, "{args}");
        }
    }
}
