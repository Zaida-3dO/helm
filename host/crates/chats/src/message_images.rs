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
//! - A chat that carries a message over from another one (a fork copies the history, an edit sends
//!   the message again) gets its own copy of the message's images, made by [`copy_file`] and
//!   [`insert_copy`]: a new attachment row owned by the new chat, and a hard link to the stored file
//!   (a plain copy where links are not possible). So its thumbnails resolve on their own, and
//!   deleting either chat leaves the other's images.

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
    reg.blocking("attachment_get", OpClass::Blocking, attachment_get)
        .blocking("local_image_get", OpClass::Blocking, local_image_get);
}

/// The largest picture a reply or a tool card will show from disk.
const LOCAL_IMAGE_MAX: u64 = 12 * 1024 * 1024;

/// The type of a picture from its first bytes; `None` for anything that is not PNG, JPEG, GIF or WebP.
fn sniff(b: &[u8]) -> Option<&'static str> {
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

/// `local_image_get {path}` answers `{mime, bytes}` (base64) for a picture on this computer that a
/// reply or a tool card points at. The path must be absolute and the file a regular one (a link
/// is not followed), at most 12 MiB, and its first bytes must be a PNG, JPEG, GIF or WebP: nothing
/// else is ever read back, whatever its name.
fn local_image_get(_ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty() && p.len() <= crate::attachments::MAX_PATH_BYTES)
        .ok_or_else(|| ApiError::bad_args("path is required."))?;
    let (mime, bytes) = read_local_image(Path::new(path))?;
    Ok(json!({
        "mime": mime,
        "bytes": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

fn read_local_image(path: &Path) -> Result<(&'static str, Vec<u8>), ApiError> {
    if !path.is_absolute() {
        return Err(ApiError::bad_args("The path must be absolute."));
    }
    let bytes = helm_core::privfs::read_capped(path, LOCAL_IMAGE_MAX).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            not_found()
        } else {
            ApiError::new("unavailable", "That picture could not be read.")
        }
    })?;
    let mime = sniff(&bytes).ok_or_else(|| ApiError::bad_args("That file is not a picture."))?;
    Ok((mime, bytes))
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
pub(crate) fn sent_image(
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

/// Give the new attachment `new_id` the bytes of the stored image `old_id`: a hard link, or a copy
/// where the file system cannot link. The ids are the host's own UUIDs, never request text.
pub fn copy_file(dir: &Path, old_id: &str, new_id: &str) -> std::io::Result<()> {
    helm_core::privfs::create_private_dir_all(dir)?;
    let (from, to) = (dir.join(old_id), dir.join(new_id));
    std::fs::hard_link(&from, &to).or_else(|_| std::fs::copy(&from, &to).map(|_| ()))
}

/// Record `new_id` as `to_chat`'s own sent image, a copy of the row of `old_id` that is a sent image
/// of `from_chat`. `holder` fills the row's submission column: the submission the copy is sent with
/// (an edit), or a marker naming the chat it was carried into (a fork). Returns whether the source
/// image existed.
pub fn insert_copy(
    tx: &rusqlite::Transaction<'_>,
    old_id: &str,
    from_chat: &str,
    new_id: &str,
    to_chat: &str,
    holder: &str,
) -> rusqlite::Result<bool> {
    let n = tx.execute(
        "INSERT INTO chat_attachments(id, chat_id, name, mime, bytes, width, height, submission_id, created_at)
         SELECT ?1, ?2, name, mime, bytes, width, height, ?3, created_at FROM chat_attachments
         WHERE id = ?4 AND chat_id = ?5 AND submission_id IS NOT NULL",
        params![new_id, to_chat, holder, old_id, from_chat],
    )?;
    Ok(n > 0)
}

/// The image ids a row's or entry's `details` lists.
pub fn ids_in(details: &Value) -> Vec<String> {
    details
        .get("images")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| i.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
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
    fn a_local_picture_is_read_only_when_it_really_is_one() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, b: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, b).unwrap();
            p
        };
        let ok = write("a.png", &png(4, 4));
        assert_eq!(read_local_image(&ok).unwrap().0, "image/png");
        // The name does not matter; the first bytes do.
        assert_eq!(
            read_local_image(&write("b.txt", &png(4, 4))).unwrap().0,
            "image/png"
        );
        assert_eq!(
            read_local_image(&write("c.jpg", &[0xff, 0xd8, 0xff, 0xe0, 0]))
                .unwrap()
                .0,
            "image/jpeg"
        );
        assert_eq!(
            read_local_image(&write("d.gif", b"GIF89a....")).unwrap().0,
            "image/gif"
        );
        assert_eq!(
            read_local_image(&write("e.webp", b"RIFF....WEBPVP8 "))
                .unwrap()
                .0,
            "image/webp"
        );
        let refused = [
            read_local_image(&write("notes.png", b"hello, not a picture")),
            read_local_image(&dir.path().join("missing.png")),
            read_local_image(dir.path()),
            read_local_image(Path::new("relative/a.png")),
        ];
        let codes: Vec<_> = refused.into_iter().map(|r| r.unwrap_err().code).collect();
        assert_eq!(codes[0], "bad_args");
        assert_eq!(codes[1], "not_found");
        assert_eq!(codes[3], "bad_args");
        assert!(codes[2] == "unavailable" || codes[2] == "bad_args");
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
