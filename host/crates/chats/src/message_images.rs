//! Images in the user's own bubble.
//!
//! An image attached in the composer is stored by `attachment_put` and linked to the submission
//! that sent it ([`crate::attachments`]). This module lets the transcript show it afterwards:
//!
//! - [`for_submission`] lists a submission's images (id, name, mime, size) for the user row's
//!   `details.images`. The actor writes them when it writes the row, so they survive a restart.
//! - `attachment_get {chatId, attachmentId, thumb?}` answers `{mime, bytes}` (base64) for one sent
//!   image of that chat. With `thumb: true` the bytes are a small cached copy ([`crate::thumbs`])
//!   for the bubble; without it they are the stored file, for the full-size view. Only images
//!   already linked to a sent message are served, and only to the chat that owns them.

use crate::ops::{not_found, sql};
use crate::types::MessageImage;
use base64::Engine as _;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

pub fn register(reg: &mut OpRegistry) {
    reg.blocking("attachment_get", OpClass::Blocking, attachment_get);
}

/// The images sent with a submission, oldest first; empty when it had none.
pub fn refs_for(conn: &Connection, submission_id: &str) -> rusqlite::Result<Vec<MessageImage>> {
    let mut st = conn.prepare(
        "SELECT id, name, mime, width, height FROM chat_attachments
         WHERE submission_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = st
        .query_map([submission_id], |r| {
            Ok(MessageImage {
                id: r.get(0)?,
                name: r.get(1)?,
                mime: r.get(2)?,
                width: r.get::<_, i64>(3)?.clamp(0, i64::from(u32::MAX)) as u32,
                height: r.get::<_, i64>(4)?.clamp(0, i64::from(u32::MAX)) as u32,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// [`refs_for`] for the actor, which has only the database: a read failure shows no thumbnails
/// rather than failing the row.
pub fn for_submission(db: &Database, submission_id: &str) -> Vec<MessageImage> {
    db.snapshot(|t| refs_for(t, submission_id))
        .unwrap_or_default()
}

/// The `(id, mime)` of a sent image of the chat; `None` when the chat has no such sent image.
/// The id is the stored one (a UUID the attachments module generated), never the request's text.
fn sent_image(
    conn: &Connection,
    chat_id: &str,
    attachment_id: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    conn.query_row(
        "SELECT id, mime FROM chat_attachments
         WHERE id = ?1 AND chat_id = ?2 AND submission_id IS NOT NULL",
        params![attachment_id, chat_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// One sent image's type and bytes, or `None` when the chat has no such sent image.
pub fn read(
    conn: &Connection,
    dir: &Path,
    chat_id: &str,
    attachment_id: &str,
) -> rusqlite::Result<Option<(String, Vec<u8>)>> {
    let Some((id, mime)) = sent_image(conn, chat_id, attachment_id)? else {
        return Ok(None);
    };
    match std::fs::read(dir.join(&id)) {
        Ok(bytes) => Ok(Some((mime, bytes))),
        Err(e) => {
            log::warn!("attachment {id} could not be read: {e}");
            Ok(None)
        }
    }
}

fn field(args: &Value, k: &str) -> Result<String, ApiError> {
    args.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_args(format!("{k} is required.")))
}

fn attachment_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let chat_id = field(&args, "chatId")?;
    let attachment_id = field(&args, "attachmentId")?;
    let db = ctx.services.need::<Database>()?;
    let profile = ctx.services.need::<Profile>()?;
    let dir = profile.path("attachments");
    let (mime, bytes) = if args.get("thumb").and_then(Value::as_bool) == Some(true) {
        let (id, mime) = db
            .snapshot(|t| sent_image(t, &chat_id, &attachment_id))
            .map_err(sql)?
            .ok_or_else(not_found)?;
        crate::thumbs::get(&dir, &profile.path("thumbs"), &id, &mime).ok_or_else(not_found)?
    } else {
        db.snapshot(|t| read(t, &dir, &chat_id, &attachment_id))
            .map_err(sql)?
            .ok_or_else(not_found)?
    };
    Ok(json!({
        "mime": mime,
        "bytes": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0]);
        b
    }

    #[test]
    fn a_sent_image_is_listed_and_read_back_and_nothing_else_is() {
        let (dir, db) = crate::schema::tests::temp_db();
        let att = dir.path().join("attachments");
        let chat = db
            .write(|tx| crate::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        let other = db
            .write(|tx| crate::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        let bytes = png(64, 32);
        let id = db
            .write(|tx| {
                crate::attachments::put(tx, &att, &chat, "shot.png", "image/png", &bytes, 1)
            })
            .unwrap()
            .unwrap()
            .id;
        // Not sent yet: not listed under any submission, and not served.
        assert!(db
            .snapshot(|t| read(t, &att, &chat, &id))
            .unwrap()
            .is_none());
        db.write(|tx| {
            tx.execute(
                "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES ('s1', ?1, 't', 'saved', 1)",
                [&chat],
            )
        })
        .unwrap();
        db.write(|tx| crate::attachments::link(tx, &chat, "s1", std::slice::from_ref(&id)))
            .unwrap()
            .unwrap();
        let refs = for_submission(&db, "s1");
        assert_eq!(
            refs,
            vec![MessageImage {
                id: id.clone(),
                name: "shot.png".into(),
                mime: "image/png".into(),
                width: 64,
                height: 32,
            }]
        );
        assert!(for_submission(&db, "nope").is_empty());
        let (mime, got) = db
            .snapshot(|t| read(t, &att, &chat, &id))
            .unwrap()
            .expect("a sent image is served to its own chat");
        assert_eq!(mime, "image/png");
        assert_eq!(got, bytes);
        assert!(
            db.snapshot(|t| read(t, &att, &other, &id))
                .unwrap()
                .is_none(),
            "another chat cannot read it"
        );
    }
}
