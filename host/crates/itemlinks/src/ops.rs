//! The item-link `api` ops.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `item_links_for_chats` | blocking | `{chatIds}` (at most 2,000) to [`ItemLinksForChats`] |
//! | `item_links_for_item` | blocking | `{itemId}` to [`ItemLinksForChats`] |
//! | `item_link_set_main` | maintenance | `{itemId, chatId}` (the user's choice) to [`ItemLinksForChats`] for that item; emits `item_links {itemIds}` |
//! | `item_link_add` | maintenance | `{itemId, chatId, title?, main?}` (the user's choice: a new chat made for a task) to [`ItemLinksForChats`] for that item; emits `item_links {itemIds}` |
//! | `item_open` | blocking | `{itemId}` to [`ItemOpen`](crate::types::ItemOpen): one `chatId`, or `candidates` |

use crate::store;
use crate::types::ItemLinksForChats;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_CHAT_IDS: usize = 2_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatIds {
    chat_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ItemArg {
    item_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetMain {
    item_id: String,
    chat_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Add {
    item_id: String,
    chat_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    main: bool,
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("item_links_for_chats", OpClass::Blocking, for_chats)
        .blocking("item_links_for_item", OpClass::Blocking, for_item)
        .blocking("item_link_set_main", OpClass::Maintenance, set_main)
        .blocking("item_link_add", OpClass::Maintenance, add)
        .blocking("item_open", OpClass::Blocking, open);
}

fn for_chats(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIds = parse(args)?;
    if a.chat_ids.len() > MAX_CHAT_IDS {
        return Err(ApiError::bad_args("Too many chats in one request."));
    }
    let db = ctx.services.need::<Database>()?;
    let links = db
        .snapshot(|tx| store::for_chats(tx, &a.chat_ids))
        .map_err(sql)?;
    to_value(&ItemLinksForChats { links })
}

fn for_item(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ItemArg = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let links = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(store::for_item(tx, &a.item_id)))
        .map_err(sql)?
        .map_err(ApiError::bad_args)?;
    to_value(&ItemLinksForChats { links })
}

fn set_main(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SetMain = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let id = db
        .write(|tx| Ok::<_, rusqlite::Error>(store::set_main(tx, &a.item_id, &a.chat_id)))
        .map_err(sql)?
        .map_err(ApiError::conflict)?;
    ctx.events
        .emit(AppEvent::new("item_links").with("itemIds", json!([id])));
    let links = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(store::for_item(tx, &id)))
        .map_err(sql)?
        .map_err(ApiError::bad_args)?;
    to_value(&ItemLinksForChats { links })
}

/// The user links a chat to an item (the Tasks view's "New chat in this task"). Only Helm's own
/// table changes; nothing is written to the board.
fn add(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: Add = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let id = db
        .write(|tx| {
            let linked = store::link(
                tx,
                &a.chat_id,
                &a.item_id,
                a.main,
                crate::types::LinkedBy::User,
                helm_core::now_ms(),
            );
            if let (Ok(l), Some(t)) = (&linked, &a.title) {
                store::set_title(tx, &l.item_id, t)?;
            }
            Ok::<_, rusqlite::Error>(linked)
        })
        .map_err(sql)?
        .map_err(ApiError::bad_args)?
        .item_id;
    ctx.events
        .emit(AppEvent::new("item_links").with("itemIds", json!([id])));
    let links = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(store::for_item(tx, &id)))
        .map_err(sql)?
        .map_err(ApiError::bad_args)?;
    to_value(&ItemLinksForChats { links })
}

fn open(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ItemArg = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let r = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(store::open(tx, &a.item_id)))
        .map_err(sql)?
        .map_err(|m| ApiError::new("not_found", m))?;
    to_value(&r)
}
