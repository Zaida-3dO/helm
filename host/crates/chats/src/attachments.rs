//! Image attachments.
//!
//! - `attachment_put {chatId, name, mime, bytes}` (base64) stores one image under the profile's
//!   private `attachments/` folder and answers `{attachmentId}`. PNG, JPEG, GIF and WebP only; at
//!   most [`MAX_IMAGE_BYTES`] and [`MAX_PIXELS`] (read from the image's own header, never decoded).
//! - `chat_send {attachments: [id…]}` links at most [`MAX_PER_MESSAGE`] of the chat's unsent
//!   attachments to the submission, with at most [`MAX_IMAGE_TOTAL`] of images in all. The actor
//!   sends them to Claude Code as stream-json image blocks with the prompt.
//! - The renderer shows its own copy as a `blob:` URL thumbnail; the host never returns image
//!   bytes for display.
//! - A hard delete removes the files with the chat ([`crate::delete`]).

use crate::archive::Refusal;
use rusqlite::{params, OptionalExtension, Transaction};
use std::path::Path;

pub const MAX_PER_MESSAGE: usize = 20;
pub const MAX_IMAGE_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_IMAGE_TOTAL: usize = 60 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 20_000_000;
pub const MAX_NAME_BYTES: usize = 255;

/// Width and height from an image header, for the formats Claude accepts.
pub fn dimensions(mime: &str, b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| {
        b.get(i..i + 2)
            .map(|s| u16::from_be_bytes([s[0], s[1]]) as u32)
    };
    let be32 = |i: usize| {
        b.get(i..i + 4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    };
    let le16 = |i: usize| {
        b.get(i..i + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]) as u32)
    };
    let le24 = |i: usize| {
        b.get(i..i + 3)
            .map(|s| s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
    };
    match mime {
        "image/png" if b.starts_with(b"\x89PNG\r\n\x1a\n") && b.get(12..16) == Some(b"IHDR") => {
            Some((be32(16)?, be32(20)?))
        }
        "image/gif" if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") => {
            Some((le16(6)?, le16(8)?))
        }
        "image/webp" if b.get(0..4) == Some(b"RIFF") && b.get(8..12) == Some(b"WEBP") => {
            match b.get(12..16)? {
                b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
                b"VP8L" => {
                    let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                    Some(((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1))
                }
                b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
                _ => None,
            }
        }
        "image/jpeg" if b.starts_with(&[0xff, 0xd8]) => {
            // Walk the segments to the first start-of-frame marker.
            let mut i = 2;
            while i + 9 < b.len() {
                if b[i] != 0xff {
                    i += 1;
                    continue;
                }
                let marker = b[i + 1];
                if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                    return Some((be16(i + 7)?, be16(i + 5)?));
                }
                let len = be16(i + 2)? as usize;
                i += 2 + len;
            }
            None
        }
        _ => None,
    }
}

#[derive(Debug)]
pub struct Stored {
    pub id: String,
}

/// Validate and store one image.
pub fn put(
    tx: &Transaction<'_>,
    dir: &Path,
    chat_id: &str,
    name: &str,
    mime: &str,
    bytes: &[u8],
    now: i64,
) -> rusqlite::Result<Result<Stored, Refusal>> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1 AND archived = 0)",
        [chat_id],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(Err(Refusal::new(
            "not_found",
            "That chat does not exist or is archived.",
        )));
    }
    if !matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Ok(Err(Refusal::new(
            "bad_args",
            "Only PNG, JPEG, GIF and WebP images can be attached.",
        )));
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Ok(Err(Refusal::new(
            "too_large",
            "An image can be at most 25 MB.",
        )));
    }
    let Some((w, h)) = dimensions(mime, bytes) else {
        return Ok(Err(Refusal::new(
            "bad_args",
            "That file is not a readable image.",
        )));
    };
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return Ok(Err(Refusal::new(
            "too_large",
            "An image can be at most 20 megapixels.",
        )));
    }
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .chars()
        .take(MAX_NAME_BYTES)
        .collect();
    let id = uuid::Uuid::new_v4().to_string();
    let write = || -> std::io::Result<()> {
        helm_core::privfs::create_private_dir_all(dir)?;
        helm_core::privfs::write_atomic(&dir.join(&id), bytes)
    };
    if let Err(e) = write() {
        return Ok(Err(Refusal::new(
            "internal",
            format!("The image could not be saved: {e}"),
        )));
    }
    tx.execute(
        "INSERT INTO chat_attachments(id, chat_id, name, mime, bytes, width, height, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![id, chat_id, name, mime, bytes.len() as i64, w, h, now],
    )?;
    Ok(Ok(Stored { id }))
}

/// Link the send's attachments to its submission, checking the per-message limits.
pub fn link(
    tx: &Transaction<'_>,
    chat_id: &str,
    submission_id: &str,
    ids: &[String],
) -> rusqlite::Result<Result<(), Refusal>> {
    if ids.len() > MAX_PER_MESSAGE {
        return Ok(Err(Refusal::new(
            "too_large",
            "A message can carry at most 20 attachments.",
        )));
    }
    let mut total = 0usize;
    for id in ids {
        let row: Option<(i64, Option<String>)> = tx
            .query_row(
                "SELECT bytes, submission_id FROM chat_attachments WHERE id = ?1 AND chat_id = ?2",
                params![id, chat_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((b, None)) => total += b as usize,
            _ => {
                return Ok(Err(Refusal::new(
                    "bad_args",
                    "An attachment is missing or already sent; attach it again.",
                )))
            }
        }
    }
    if total > MAX_IMAGE_TOTAL {
        return Ok(Err(Refusal::new(
            "too_large",
            "A message can carry at most 60 MB of images.",
        )));
    }
    for id in ids {
        tx.execute(
            "UPDATE chat_attachments SET submission_id = ?1 WHERE id = ?2",
            params![submission_id, id],
        )?;
    }
    Ok(Ok(()))
}

/// The submission's images as stream-json image blocks: `[{data (base64), mimeType}]`.
pub fn images_for(
    tx: &Transaction<'_>,
    dir: &Path,
    submission_id: &str,
) -> rusqlite::Result<Vec<serde_json::Value>> {
    use base64::Engine as _;
    let mut st = tx.prepare(
        "SELECT id, mime FROM chat_attachments WHERE submission_id = ?1 ORDER BY created_at, id",
    )?;
    let rows: Vec<(String, String)> = st
        .query_map([submission_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (id, mime) in rows {
        match std::fs::read(dir.join(&id)) {
            Ok(bytes) => out.push(serde_json::json!({
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                "mimeType": mime,
            })),
            Err(e) => log::warn!("attachment {id} could not be read: {e}"),
        }
    }
    Ok(out)
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
    fn headers_give_dimensions() {
        assert_eq!(dimensions("image/png", &png(640, 480)), Some((640, 480)));
        let gif = b"GIF89a\x40\x01\xf0\x00".to_vec();
        assert_eq!(dimensions("image/gif", &gif), Some((320, 240)));
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 17, 8];
        jpeg.extend(300u16.to_be_bytes());
        jpeg.extend(500u16.to_be_bytes());
        jpeg.extend([3, 0, 0]);
        assert_eq!(dimensions("image/jpeg", &jpeg), Some((500, 300)));
        assert_eq!(dimensions("image/png", b"not a png at all, sorry"), None);
        assert_eq!(
            dimensions("image/jpeg", &png(1, 1)),
            None,
            "the mime must match the bytes"
        );
    }

    #[test]
    fn limits_are_enforced_on_put_and_link() {
        let (dir, db) = crate::schema::tests::temp_db();
        let att = dir.path().join("attachments");
        let chat = db
            .write(|tx| crate::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        let put1 = |name: &str, mime: &str, b: &[u8]| {
            db.write(|tx| put(tx, &att, &chat, name, mime, b, 1))
                .unwrap()
        };
        assert!(put1("a.txt", "text/plain", b"hi").is_err(), "images only");
        assert_eq!(
            put1("big.png", "image/png", &png(5000, 5000))
                .unwrap_err()
                .code,
            "too_large",
            "over 20 MP"
        );
        assert!(put1("bad.png", "image/png", b"\x89PNG nope").is_err());
        let ok = put1("ok.png", "image/png", &png(100, 100)).unwrap();
        assert!(att.join(&ok.id).is_file());
        let sub = db
            .write(|tx| {
                tx.execute(
                    "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES ('s1', ?1, 't', 'saved', 1)",
                    [&chat],
                )
            })
            .unwrap();
        let _ = sub;
        let too_many: Vec<String> = (0..21).map(|i| format!("x{i}")).collect();
        assert!(db
            .write(|tx| link(tx, &chat, "s1", &too_many))
            .unwrap()
            .is_err());
        db.write(|tx| link(tx, &chat, "s1", std::slice::from_ref(&ok.id)))
            .unwrap()
            .unwrap();
        let again = db
            .write(|tx| link(tx, &chat, "s1", std::slice::from_ref(&ok.id)))
            .unwrap();
        assert!(again.is_err(), "an attachment is sent once");
        let imgs = db.snapshot(|tx| images_for(tx, &att, "s1")).unwrap();
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0]["mimeType"], "image/png");
    }
}
