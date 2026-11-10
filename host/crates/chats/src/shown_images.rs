//! Pictures an agent shows in the chat, and option boards.
//!
//! Two host tools (`app_show_images`, `app_offer_options`, in the hosttools crate) put pictures in
//! front of the user. The tools check their arguments and call the ops here, which read the files
//! under the same rules as a reply's local images ([`crate::message_images`]: PNG, JPEG, GIF or WebP
//! by their first bytes, no links, shares or Helm's own folders) and **copy the bytes into the
//! database**, so a card keeps showing after the file moves or is deleted, goes with a backup, and
//! goes with the chat when it is deleted.
//!
//! | Table | Holds |
//! |---|---|
//! | `shown_galleries` | one row per `app_show_images` call: id, title, picture count |
//! | `shown_images` | the pictures of a gallery **or** of an option board (`set_id`), by position: bytes, a cached thumbnail, caption |
//! | `option_boards` | one row per `app_offer_options` call: question, options, switches, and the recorded answer |
//!
//! A gallery or board id is a UUID the host made; the chat's transcript names it in the tool's
//! result (`{galleryId, count}` and `{boardId}`), and every read is by (chat, id), so a chat can
//! only ever read its own. A fork copies the galleries and boards its carried history names, under
//! the same ids ([`copy_for_fork`]).
//!
//! Limits: 1 to 20 pictures per gallery, 2 to 12 options per board, 25 MB a picture, 60 MB a call,
//! 20 megapixels a picture, and [`CHAT_CAP`] stored per chat.
//!
//! | Op | Args | Answers |
//! |---|---|---|
//! | `shown_images_add` | `{chatId, title?, images: [{path, caption?}]}` | `{galleryId, count}` |
//! | `option_board_create` | `{chatId, question, options: [{label, detail?, image?}], multiSelect?, allowOther?}` | `{boardId}` |
//! | `shown_gallery_get` | `{chatId, galleryId}` | `{galleryId, title, createdAt, images: [{index, name, caption, mime, width, height}]}` |
//! | `shown_image_get` | `{chatId, galleryId, index}` | `{mime, bytes}` (base64, the stored picture); `galleryId` may also be a board id |
//! | `shown_image_thumb` | same | `{mime, bytes}` (a small copy, or the picture itself when already small) |
//! | `option_board_state` | `{chatId, boardId}` | the board: see [`board_json`] |
//! | `option_board_answer` | `{chatId, boardId, labels?: [string], other?: string}` | the board, now with its answer |

use crate::ops::{not_found, sql};
use crate::{attachments, message_images, thumbs};
use base64::Engine as _;
use helm_core::db::Database;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};

pub const MAX_IMAGES: usize = 20;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 12;
/// One picture, and all the pictures of one call.
pub const MAX_IMAGE_BYTES: usize = attachments::MAX_IMAGE_BYTES;
pub const MAX_CALL_BYTES: usize = attachments::MAX_IMAGE_TOTAL;
/// The most picture bytes one chat may keep.
pub const CHAT_CAP: i64 = 400 * 1024 * 1024;
const MAX_TITLE: usize = 120;
const MAX_CAPTION: usize = 300;
const MAX_QUESTION: usize = 300;
const MAX_LABEL: usize = 80;
const MAX_DETAIL: usize = 300;
const MAX_OTHER: usize = 500;

pub fn register(reg: &mut OpRegistry) {
    reg.blocking("shown_images_add", OpClass::Blocking, shown_images_add)
        .blocking("option_board_create", OpClass::Blocking, option_board_create)
        .blocking("shown_gallery_get", OpClass::Blocking, shown_gallery_get)
        .blocking("shown_image_get", OpClass::Blocking, shown_image_get)
        .blocking("shown_image_thumb", OpClass::Blocking, shown_image_thumb)
        .blocking("option_board_state", OpClass::Blocking, option_board_state)
        .blocking("option_board_answer", OpClass::Blocking, option_board_answer);
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS shown_galleries (
             chat_id     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             id          TEXT NOT NULL,
             title       TEXT NOT NULL,
             count       INTEGER NOT NULL,
             created_at  INTEGER NOT NULL,
             PRIMARY KEY (chat_id, id)
         );
         CREATE TABLE IF NOT EXISTS shown_images (
             chat_id     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             set_id      TEXT NOT NULL,
             idx         INTEGER NOT NULL,
             name        TEXT NOT NULL,
             caption     TEXT NOT NULL,
             mime        TEXT NOT NULL,
             width       INTEGER NOT NULL,
             height      INTEGER NOT NULL,
             data        BLOB NOT NULL,
             thumb_mime  TEXT,
             thumb       BLOB,
             PRIMARY KEY (chat_id, set_id, idx)
         );
         CREATE TABLE IF NOT EXISTS option_boards (
             chat_id      TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             id           TEXT NOT NULL,
             question     TEXT NOT NULL,
             options      TEXT NOT NULL,
             multi        INTEGER NOT NULL,
             allow_other  INTEGER NOT NULL,
             answer       TEXT,
             answered_at  INTEGER,
             created_at   INTEGER NOT NULL,
             PRIMARY KEY (chat_id, id)
         );",
    )
}

/// One picture, read and checked, ready to store.
struct Picture {
    name: String,
    caption: String,
    mime: &'static str,
    width: u32,
    height: u32,
    data: Vec<u8>,
    thumb: Option<(&'static str, Vec<u8>)>,
}

/// Text without control characters, trimmed and cut to `max` characters.
fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect()
}

fn opt_text(v: &Value, key: &str, max: usize) -> String {
    clean(v.get(key).and_then(Value::as_str).unwrap_or(""), max)
}

fn field(args: &Value, k: &str) -> Result<String, ApiError> {
    args.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_args(format!("{k} is required.")))
}

/// Read one picture from disk under the local-image rules. `which` names it in a refusal
/// ("Picture 2", "Option 3").
fn load(ctx: &OpCtx, raw: &str, caption: String, which: &str) -> Result<Picture, ApiError> {
    let refuse = |code: &str, msg: &str| ApiError::new(code, format!("{which}: {msg}"));
    let path = message_images::admit_path(ctx, raw.trim())
        .map_err(|e| ApiError::new(&e.code, format!("{which}: {}", e.message)))?;
    if !path.is_absolute() {
        return Err(refuse("bad_args", "the path must be absolute."));
    }
    let data = helm_core::privfs::read_capped(&path, MAX_IMAGE_BYTES as u64).map_err(|e| {
        match e.kind() {
            std::io::ErrorKind::NotFound => refuse("not_found", "that file does not exist."),
            std::io::ErrorKind::InvalidData => {
                refuse("too_large", "a picture can be at most 25 MB.")
            }
            _ => refuse("unavailable", "that picture could not be read."),
        }
    })?;
    let mime = message_images::sniff(&data).ok_or_else(|| {
        refuse(
            "bad_args",
            "that file is not a PNG, JPEG, GIF or WebP picture.",
        )
    })?;
    let (width, height) = attachments::dimensions(mime, &data)
        .ok_or_else(|| refuse("bad_args", "that file is not a readable picture."))?;
    if u64::from(width) * u64::from(height) > attachments::MAX_PIXELS {
        return Err(refuse("too_large", "a picture can be at most 20 megapixels."));
    }
    let name = path
        .file_name()
        .map(|n| clean(&n.to_string_lossy(), attachments::MAX_NAME_BYTES))
        .unwrap_or_default();
    let thumb = thumbs::make(mime, &data);
    Ok(Picture {
        name,
        caption,
        mime,
        width,
        height,
        data,
        thumb,
    })
}

fn chat_exists(db: &Database, chat_id: &str) -> Result<(), ApiError> {
    let found: bool = db
        .snapshot(|t| {
            t.query_row(
                "SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1)",
                [chat_id],
                |r| r.get(0),
            )
        })
        .map_err(sql)?;
    found.then_some(()).ok_or_else(not_found)
}

fn stored_bytes(tx: &Connection, chat_id: &str) -> rusqlite::Result<i64> {
    tx.query_row(
        "SELECT COALESCE(SUM(length(data) + COALESCE(length(thumb), 0)), 0) FROM shown_images WHERE chat_id = ?1",
        [chat_id],
        |r| r.get(0),
    )
}

fn chat_full() -> ApiError {
    ApiError::new(
        "too_large",
        "This chat already holds as many shown pictures as Helm keeps (400 MB).",
    )
}

fn insert_pictures(
    tx: &Transaction<'_>,
    chat_id: &str,
    set_id: &str,
    pics: &[(usize, Picture)],
) -> rusqlite::Result<()> {
    for (idx, p) in pics {
        let idx = *idx as i64;
        tx.execute(
            "INSERT INTO shown_images(chat_id, set_id, idx, name, caption, mime, width, height, data, thumb_mime, thumb)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                chat_id,
                set_id,
                idx,
                p.name,
                p.caption,
                p.mime,
                p.width,
                p.height,
                p.data,
                p.thumb.as_ref().map(|t| t.0),
                p.thumb.as_ref().map(|t| t.1.as_slice()),
            ],
        )?;
    }
    Ok(())
}

fn shown_images_add(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let chat_id = field(&args, "chatId")?;
    let title = opt_text(&args, "title", MAX_TITLE);
    let items = args
        .get("images")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_args("images is required."))?;
    if !(1..=MAX_IMAGES).contains(&items.len()) {
        return Err(ApiError::bad_args("Show 1 to 20 pictures per call."));
    }
    let db = ctx.services.need::<Database>()?;
    chat_exists(&db, &chat_id)?;
    let mut pics = Vec::with_capacity(items.len());
    let mut total = 0usize;
    for (i, item) in items.iter().enumerate() {
        let which = format!("Picture {}", i + 1);
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| ApiError::bad_args(format!("{which}: path is required.")))?;
        let pic = load(ctx, path, opt_text(item, "caption", MAX_CAPTION), &which)?;
        total += pic.data.len();
        if total > MAX_CALL_BYTES {
            return Err(ApiError::new(
                "too_large",
                "One call can show at most 60 MB of pictures.",
            ));
        }
        pics.push((i, pic));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = helm_core::now_ms();
    let count = pics.len();
    db.write(|tx| {
        if stored_bytes(tx, &chat_id)? + total as i64 > CHAT_CAP {
            return Ok::<_, rusqlite::Error>(Err(chat_full()));
        }
        tx.execute(
            "INSERT INTO shown_galleries(chat_id, id, title, count, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![chat_id, id, title, count as i64, now],
        )?;
        insert_pictures(tx, &chat_id, &id, &pics)?;
        Ok(Ok(()))
    })
    .map_err(sql)??;
    Ok(json!({"galleryId": id, "count": count}))
}

fn option_board_create(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let chat_id = field(&args, "chatId")?;
    let question = opt_text(&args, "question", MAX_QUESTION);
    if question.is_empty() {
        return Err(ApiError::bad_args("question is required."));
    }
    let items = args
        .get("options")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_args("options is required."))?;
    if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&items.len()) {
        return Err(ApiError::bad_args("Offer 2 to 12 options."));
    }
    let multi = args.get("multiSelect").and_then(Value::as_bool) == Some(true);
    let allow_other = args.get("allowOther").and_then(Value::as_bool) != Some(false);
    let db = ctx.services.need::<Database>()?;
    chat_exists(&db, &chat_id)?;
    let mut options = Vec::with_capacity(items.len());
    let mut pics = Vec::new();
    let mut total = 0usize;
    for (i, item) in items.iter().enumerate() {
        let which = format!("Option {}", i + 1);
        let label = opt_text(item, "label", MAX_LABEL);
        if label.is_empty() {
            return Err(ApiError::bad_args(format!("{which}: label is required.")));
        }
        if options
            .iter()
            .any(|o: &Value| o["label"].as_str() == Some(label.as_str()))
        {
            return Err(ApiError::bad_args(format!(
                "{which}: two options are both called \"{label}\"; labels must differ."
            )));
        }
        let mut has_image = false;
        if let Some(path) = item
            .get("image")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
        {
            let pic = load(ctx, path, String::new(), &which)?;
            total += pic.data.len();
            if total > MAX_CALL_BYTES {
                return Err(ApiError::new(
                    "too_large",
                    "One board can hold at most 60 MB of pictures.",
                ));
            }
            pics.push((i, pic));
            has_image = true;
        }
        options.push(json!({
            "label": label,
            "detail": opt_text(item, "detail", MAX_DETAIL),
            "image": has_image,
        }));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = helm_core::now_ms();
    db.write(|tx| {
        if stored_bytes(tx, &chat_id)? + total as i64 > CHAT_CAP {
            return Ok::<_, rusqlite::Error>(Err(chat_full()));
        }
        tx.execute(
            "INSERT INTO option_boards(chat_id, id, question, options, multi, allow_other, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                chat_id,
                id,
                question,
                Value::Array(options).to_string(),
                multi,
                allow_other,
                now
            ],
        )?;
        insert_pictures(tx, &chat_id, &id, &pics)?;
        Ok(Ok(()))
    })
    .map_err(sql)??;
    Ok(json!({"boardId": id}))
}

fn set_and_chat(args: &Value, set_key: &str) -> Result<(String, String), ApiError> {
    Ok((field(args, "chatId")?, field(args, set_key)?))
}

fn shown_gallery_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let (chat_id, id) = set_and_chat(&args, "galleryId")?;
    let db = ctx.services.need::<Database>()?;
    db.snapshot(|t| gallery_json(t, &chat_id, &id))
        .map_err(sql)?
        .ok_or_else(not_found)
}

fn gallery_json(t: &Connection, chat_id: &str, id: &str) -> rusqlite::Result<Option<Value>> {
    let Some((title, created_at)) = t
        .query_row(
            "SELECT title, created_at FROM shown_galleries WHERE chat_id = ?1 AND id = ?2",
            params![chat_id, id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let mut st = t.prepare(
        "SELECT idx, name, caption, mime, width, height FROM shown_images
         WHERE chat_id = ?1 AND set_id = ?2 ORDER BY idx",
    )?;
    let images = st
        .query_map(params![chat_id, id], |r| {
            Ok(json!({
                "index": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "caption": r.get::<_, String>(2)?,
                "mime": r.get::<_, String>(3)?,
                "width": r.get::<_, i64>(4)?,
                "height": r.get::<_, i64>(5)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(json!({
        "galleryId": id,
        "title": title,
        "createdAt": created_at,
        "images": images,
    })))
}

/// `{mime, bytes}` for one stored picture, or the thumbnail when asked for and one was made.
fn image_bytes(
    t: &Connection,
    chat_id: &str,
    id: &str,
    index: i64,
    thumb: bool,
) -> rusqlite::Result<Option<(String, Vec<u8>)>> {
    t.query_row(
        "SELECT mime, data, thumb_mime, thumb FROM shown_images
         WHERE chat_id = ?1 AND set_id = ?2 AND idx = ?3",
        params![chat_id, id, index],
        |r| {
            let thumbed: Option<(String, Vec<u8>)> = match (
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<Vec<u8>>>(3)?,
            ) {
                (Some(m), Some(b)) if thumb => Some((m, b)),
                _ => None,
            };
            Ok(thumbed.unwrap_or((r.get(0)?, r.get(1)?)))
        },
    )
    .optional()
}

fn image_op(ctx: &OpCtx, args: Value, thumb: bool) -> Result<Value, ApiError> {
    let (chat_id, id) = set_and_chat(&args, "galleryId")?;
    let index = args
        .get("index")
        .and_then(Value::as_i64)
        .filter(|i| (0..MAX_IMAGES as i64).contains(i))
        .ok_or_else(|| ApiError::bad_args("index is required."))?;
    let db = ctx.services.need::<Database>()?;
    let (mime, bytes) = db
        .snapshot(|t| image_bytes(t, &chat_id, &id, index, thumb))
        .map_err(sql)?
        .ok_or_else(not_found)?;
    Ok(json!({
        "mime": mime,
        "bytes": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

fn shown_image_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    image_op(ctx, args, false)
}

fn shown_image_thumb(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    image_op(ctx, args, true)
}

/// A board as the card reads it:
/// `{boardId, question, multiSelect, allowOther, createdAt, options: [{index, label, detail,
/// hasImage}], answer}`; `answer` is `null` until the user chooses, then
/// `{labels: [string], other: string|null, answeredAt}`. An option's picture is read with
/// `shown_image_thumb` / `shown_image_get` using the board id and the option's `index`.
fn board_json(t: &Connection, chat_id: &str, id: &str) -> rusqlite::Result<Option<Value>> {
    type Row = (String, String, bool, bool, Option<String>, Option<i64>, i64);
    let row: Option<Row> = t
        .query_row(
            "SELECT question, options, multi, allow_other, answer, answered_at, created_at
             FROM option_boards WHERE chat_id = ?1 AND id = ?2",
            params![chat_id, id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((question, options, multi, allow_other, answer, answered_at, created_at)) = row else {
        return Ok(None);
    };
    let options: Vec<Value> = serde_json::from_str::<Vec<Value>>(&options)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, o)| {
            json!({
                "index": i,
                "label": o["label"],
                "detail": o["detail"],
                "hasImage": o["image"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    let answer = answer
        .and_then(|a| serde_json::from_str::<Value>(&a).ok())
        .map(|mut a| {
            a["answeredAt"] = json!(answered_at);
            a
        });
    Ok(Some(json!({
        "boardId": id,
        "question": question,
        "multiSelect": multi,
        "allowOther": allow_other,
        "createdAt": created_at,
        "options": options,
        "answer": answer,
    })))
}

fn option_board_state(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let (chat_id, id) = set_and_chat(&args, "boardId")?;
    let db = ctx.services.need::<Database>()?;
    db.snapshot(|t| board_json(t, &chat_id, &id))
        .map_err(sql)?
        .ok_or_else(not_found)
}

/// Record the user's choice. One answer per board: a second is a conflict, so a card that was
/// answered in another window stays as it was answered. The labels must be the board's own (an
/// "other" text only where the board allows it), exactly one pick for a single-choice board.
fn option_board_answer(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let (chat_id, id) = set_and_chat(&args, "boardId")?;
    let mut labels: Vec<String> = Vec::new();
    for l in args
        .get("labels")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let l = l
            .as_str()
            .ok_or_else(|| ApiError::bad_args("labels must be strings."))?;
        if !labels.iter().any(|x| x == l) {
            labels.push(l.to_owned());
        }
    }
    let other = opt_text(&args, "other", MAX_OTHER);
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    db.write(|tx| {
        let Some(board) = board_json(tx, &chat_id, &id)? else {
            return Ok::<_, rusqlite::Error>(Err(not_found()));
        };
        if !board["answer"].is_null() {
            return Ok(Err(ApiError::conflict("This board was already answered.")));
        }
        let known: Vec<&str> = board["options"]
            .as_array()
            .map(|a| a.iter().filter_map(|o| o["label"].as_str()).collect())
            .unwrap_or_default();
        if let Some(bad) = labels.iter().find(|l| !known.contains(&l.as_str())) {
            return Ok(Err(ApiError::bad_args(format!(
                "\"{bad}\" is not one of the board's options."
            ))));
        }
        if !other.is_empty() && board["allowOther"] != json!(true) {
            return Ok(Err(ApiError::bad_args("This board has no Other choice.")));
        }
        let picks = labels.len() + usize::from(!other.is_empty());
        let multi = board["multiSelect"] == json!(true);
        if picks == 0 || (!multi && picks != 1) {
            return Ok(Err(ApiError::bad_args(if multi {
                "Choose at least one option."
            } else {
                "Choose exactly one option."
            })));
        }
        let answer = json!({"labels": labels, "other": (!other.is_empty()).then_some(&other)});
        tx.execute(
            "UPDATE option_boards SET answer = ?3, answered_at = ?4
             WHERE chat_id = ?1 AND id = ?2 AND answer IS NULL",
            params![chat_id, id, answer.to_string(), now],
        )?;
        Ok(Ok(board_json(tx, &chat_id, &id)?))
    })
    .map_err(sql)??
    .ok_or_else(not_found)
}

/// Give a fork the galleries and boards its carried history names: every gallery or board of
/// `from` whose id appears in `history` (the fork's entries, as text) is copied to `to` under the
/// same id, answer included. The fork's own tool cards then resolve against the fork.
pub fn copy_for_fork(
    tx: &Transaction<'_>,
    from: &str,
    to: &str,
    history: &str,
) -> rusqlite::Result<()> {
    let ids = |query: &str| -> rusqlite::Result<Vec<String>> {
        let mut st = tx.prepare(query)?;
        let rows = st.query_map([from], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    };
    let named = |all: Vec<String>| -> Vec<String> {
        all.into_iter().filter(|id| history.contains(id)).collect()
    };
    let galleries = named(ids("SELECT id FROM shown_galleries WHERE chat_id = ?1")?);
    let boards = named(ids("SELECT id FROM option_boards WHERE chat_id = ?1")?);
    for id in &galleries {
        tx.execute(
            "INSERT OR IGNORE INTO shown_galleries(chat_id, id, title, count, created_at)
             SELECT ?2, id, title, count, created_at FROM shown_galleries WHERE chat_id = ?1 AND id = ?3",
            params![from, to, id],
        )?;
    }
    for id in &boards {
        tx.execute(
            "INSERT OR IGNORE INTO option_boards(chat_id, id, question, options, multi, allow_other, answer, answered_at, created_at)
             SELECT ?2, id, question, options, multi, allow_other, answer, answered_at, created_at
             FROM option_boards WHERE chat_id = ?1 AND id = ?3",
            params![from, to, id],
        )?;
    }
    for id in galleries.iter().chain(&boards) {
        tx.execute(
            "INSERT OR IGNORE INTO shown_images(chat_id, set_id, idx, name, caption, mime, width, height, data, thumb_mime, thumb)
             SELECT ?2, set_id, idx, name, caption, mime, width, height, data, thumb_mime, thumb
             FROM shown_images WHERE chat_id = ?1 AND set_id = ?3",
            params![from, to, id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_ipc::{NullSink, Services};
    use std::sync::Arc;

    fn png(w: u32, h: u32, fill: u8) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0, fill]);
        b
    }

    struct Rig {
        dir: tempfile::TempDir,
        ctx: OpCtx,
        db: Arc<Database>,
        chat: String,
    }

    fn rig() -> Rig {
        let (dir, db) = crate::schema::tests::temp_db();
        let db = Arc::new(db);
        let chat = db
            .write(|tx| crate::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        let mut s = Services::new();
        s.insert(db.clone());
        let ctx = OpCtx {
            services: Arc::new(s),
            events: Arc::new(NullSink),
        };
        Rig {
            dir,
            ctx,
            db,
            chat,
        }
    }

    impl Rig {
        fn file(&self, name: &str, bytes: &[u8]) -> String {
            let p = self.dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            p.to_string_lossy().into_owned()
        }
        fn call(
            &self,
            f: fn(&OpCtx, Value) -> Result<Value, ApiError>,
            mut args: Value,
        ) -> Result<Value, ApiError> {
            args["chatId"] = json!(self.chat);
            f(&self.ctx, args)
        }
    }

    /// A gallery keeps its pictures after the files are gone, answers by chat only, is refused
    /// with the picture's number when one is bad, and goes into a fork under the same id.
    #[test]
    fn a_gallery_is_stored_read_back_and_forked() {
        let r = rig();
        let a = r.file("a.png", &png(4, 3, 1));
        let b = r.file("b.png", &png(8, 6, 2));
        let text = r.file("t.png", b"just words");
        let made = r
            .call(
                shown_images_add,
                json!({"title": " Banner ", "images": [
                    {"path": a, "caption": "First"}, {"path": b}]}),
            )
            .unwrap();
        assert_eq!(made["count"], 2);
        let id = made["galleryId"].as_str().unwrap().to_owned();
        std::fs::remove_file(&a).unwrap();
        let g = r
            .call(shown_gallery_get, json!({"galleryId": id}))
            .unwrap();
        assert_eq!(g["title"], "Banner");
        assert_eq!(g["images"][0]["caption"], "First");
        assert_eq!(g["images"][1]["width"], 8);
        let got = r
            .call(shown_image_get, json!({"galleryId": id, "index": 0}))
            .unwrap();
        assert_eq!(got["mime"], "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(got["bytes"].as_str().unwrap())
                .unwrap(),
            png(4, 3, 1)
        );
        // A small picture's thumbnail is the picture itself.
        assert_eq!(
            r.call(shown_image_thumb, json!({"galleryId": id, "index": 1}))
                .unwrap()["mime"],
            "image/png"
        );
        // Refusals: the bad picture is named, nothing is stored, and strangers read nothing.
        let before: i64 = r
            .db
            .snapshot(|t| t.query_row("SELECT count(*) FROM shown_images", [], |x| x.get(0)))
            .unwrap();
        for (args, code, says) in [
            (json!({"images": []}), "bad_args", "1 to 20"),
            (
                json!({"images": [{"path": b}, {"path": text}]}),
                "bad_args",
                "Picture 2",
            ),
            (
                json!({"images": [{"path": "relative/a.png"}]}),
                "refused",
                "Picture 1",
            ),
            (json!({"images": [{"path": a}]}), "refused", "Picture 1"),
        ] {
            let e = r.call(shown_images_add, args).unwrap_err();
            assert_eq!((e.code.as_str(), e.message.contains(says)), (code, true), "{e:?}");
        }
        let after: i64 = r
            .db
            .snapshot(|t| t.query_row("SELECT count(*) FROM shown_images", [], |x| x.get(0)))
            .unwrap();
        assert_eq!(before, after);
        let mut other = json!({"chatId": "nope", "galleryId": id, "index": 0});
        assert_eq!(shown_image_get(&r.ctx, other.clone()).unwrap_err().code, "not_found");
        other["chatId"] = json!(r.chat);
        other["index"] = json!(5);
        assert_eq!(shown_image_get(&r.ctx, other).unwrap_err().code, "not_found");

        // A fork carries a gallery its history names, and only that one.
        let unnamed = r
            .call(shown_images_add, json!({"images": [{"path": b}]}))
            .unwrap()["galleryId"]
            .as_str()
            .unwrap()
            .to_owned();
        let fork = r
            .db
            .write(|tx| crate::store::create_chat(tx, "C:/w", None, None, 2))
            .unwrap();
        r.db.write(|tx| copy_for_fork(tx, &r.chat, &fork, &format!("{{\"galleryId\":\"{id}\"}}")))
            .unwrap();
        let in_fork = |gid: &str| {
            shown_gallery_get(&r.ctx, json!({"chatId": fork, "galleryId": gid})).is_ok()
        };
        assert!(in_fork(&id) && !in_fork(&unnamed));
        assert_eq!(
            shown_image_get(&r.ctx, json!({"chatId": fork, "galleryId": id, "index": 1})).unwrap()
                ["mime"],
            "image/png"
        );
        // Deleting the chat takes its pictures with it.
        r.db.write(|tx| tx.execute("DELETE FROM chats WHERE id = ?1", [&r.chat]))
            .unwrap();
        assert!(!r
            .db
            .snapshot(|t| t.query_row(
                "SELECT EXISTS(SELECT 1 FROM shown_images WHERE chat_id = ?1)",
                [&r.chat],
                |x| x.get::<_, bool>(0)
            ))
            .unwrap());
    }

    /// A board checks its options, keeps its pictures, takes exactly one valid answer and shows it
    /// afterwards.
    #[test]
    fn an_option_board_takes_one_valid_answer() {
        let r = rig();
        let a = r.file("a.png", &png(4, 3, 1));
        let opts = |n: usize| -> Vec<Value> {
            (0..n)
                .map(|i| json!({"label": format!("Colour {i}"), "detail": "d", "image": a}))
                .collect()
        };
        for (args, says) in [
            (json!({"question": "Pick", "options": opts(1)}), "2 to 12"),
            (json!({"question": "Pick", "options": opts(13)}), "2 to 12"),
            (json!({"question": " ", "options": opts(2)}), "question"),
            (
                json!({"question": "Pick", "options": [{"label": "A"}, {"label": "A"}]}),
                "labels must differ",
            ),
            (
                json!({"question": "Pick", "options": [{"label": "A"}, {"label": "B", "image": "relative.png"}]}),
                "Option 2",
            ),
        ] {
            let e = r.call(option_board_create, args).unwrap_err();
            assert!(e.message.contains(says), "{e:?}");
        }
        let single = r
            .call(
                option_board_create,
                json!({"question": "Pick one", "options": opts(3)}),
            )
            .unwrap()["boardId"]
            .as_str()
            .unwrap()
            .to_owned();
        let state = r.call(option_board_state, json!({"boardId": single})).unwrap();
        assert_eq!(state["options"][2]["label"], "Colour 2");
        assert_eq!(state["options"][1]["hasImage"], true);
        assert_eq!(state["allowOther"], true);
        assert!(state["answer"].is_null());
        assert_eq!(
            r.call(shown_image_thumb, json!({"galleryId": single, "index": 2}))
                .unwrap()["mime"],
            "image/png"
        );
        for (args, says) in [
            (json!({}), "exactly one"),
            (json!({"labels": ["Colour 0", "Colour 1"]}), "exactly one"),
            (json!({"labels": ["Colour 0"], "other": "teal"}), "exactly one"),
            (json!({"labels": ["Purple"]}), "not one of"),
        ] {
            let mut args = args;
            args["boardId"] = json!(single);
            let e = r.call(option_board_answer, args).unwrap_err();
            assert_eq!(e.code, "bad_args", "{e:?}");
            assert!(e.message.contains(says), "{e:?}");
        }
        let done = r
            .call(
                option_board_answer,
                json!({"boardId": single, "other": "Teal, please"}),
            )
            .unwrap();
        assert_eq!(done["answer"]["other"], "Teal, please");
        assert_eq!(done["answer"]["labels"], json!([]));
        assert!(done["answer"]["answeredAt"].is_i64());
        let again = r
            .call(option_board_answer, json!({"boardId": single, "labels": ["Colour 0"]}))
            .unwrap_err();
        assert_eq!(again.code, "conflict");
        assert_eq!(
            r.call(option_board_state, json!({"boardId": single})).unwrap()["answer"]["other"],
            "Teal, please"
        );

        // Multi-select without an Other choice.
        let multi = r
            .call(
                option_board_create,
                json!({"question": "Pick some", "multiSelect": true, "allowOther": false,
                       "options": [{"label": "A"}, {"label": "B"}, {"label": "C"}]}),
            )
            .unwrap()["boardId"]
            .as_str()
            .unwrap()
            .to_owned();
        let e = r
            .call(option_board_answer, json!({"boardId": multi, "other": "D"}))
            .unwrap_err();
        assert!(e.message.contains("no Other"), "{e:?}");
        let done = r
            .call(
                option_board_answer,
                json!({"boardId": multi, "labels": ["C", "A", "C"]}),
            )
            .unwrap();
        assert_eq!(done["answer"]["labels"], json!(["C", "A"]));
        assert!(done["answer"]["other"].is_null());
        assert_eq!(
            r.call(shown_image_get, json!({"galleryId": multi, "index": 0}))
                .unwrap_err()
                .code,
            "not_found",
            "an option without a picture has none to read"
        );

        // A fork keeps the board and its answer.
        let fork = r
            .db
            .write(|tx| crate::store::create_chat(tx, "C:/w", None, None, 2))
            .unwrap();
        r.db.write(|tx| copy_for_fork(tx, &r.chat, &fork, &single)).unwrap();
        let kept = option_board_state(&r.ctx, json!({"chatId": fork, "boardId": single})).unwrap();
        assert_eq!(kept["answer"]["other"], "Teal, please");
        assert!(option_board_state(&r.ctx, json!({"chatId": fork, "boardId": multi})).is_err());
    }
}
