//! Hard delete and the file cleanup that follows it.
//!
//! One transaction checks the chat, queues the files it leaves behind in `chat_file_cleanup`, and
//! deletes the row: every table hanging off `chats` cascades (messages and their search rows,
//! submissions, queue, runs, entries, marks, completions, titles, queue holds). After it commits,
//! [`run_cleanup`] removes the queued files in batches of [`BATCH`], deleting each batch's rows in
//! one transaction once its files are gone; whatever fails stays queued and is retried at the next
//! startup ([`crate::reconcile_all`]).
//!
//! Cleanup only ever touches strictly patterned names inside the profile's own `sessions/`,
//! `workers/`, `attachments/`, `thumbs/` and `artifacts/` folders ([`allowed`]); anything else in
//! the queue is dropped unremoved, with a log line.

use crate::archive::Refusal;
use crate::types::ChatStatus;
use helm_core::db::Database;
use rusqlite::{params, OptionalExtension, Transaction};
use std::path::Path;

pub const BATCH: usize = 256;

/// What `delete_in_tx` requires of the chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteRule {
    /// The Archived view's Delete: the chat must be archived.
    Archived,
    /// Discarding an unpromoted side chat: no archive needed, but it must still
    /// be a side chat.
    SideChat,
}

fn is_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|u| u.to_string() == s)
}

/// Whether a queued relative path is one cleanup may remove.
pub fn allowed(rel: &str) -> bool {
    let mut parts = rel.split('/');
    let (Some(dir), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    match dir {
        "sessions" => [".jsonl", ".jsonl.torn", ".claude.json"]
            .iter()
            .any(|ext| name.strip_suffix(ext).is_some_and(is_uuid)),
        "workers" | "attachments" | "thumbs" if is_uuid(name) => true,
        "artifacts" => name.strip_suffix(".json").is_some_and(|h| {
            h.len() == 64
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }),
        _ => false,
    }
}

/// Check, queue the files and delete the row. `Ok(Err(_))` is a refusal and changes nothing.
pub fn delete_in_tx(
    tx: &Transaction<'_>,
    chat_id: &str,
    rule: DeleteRule,
    now: i64,
) -> rusqlite::Result<Result<(), Refusal>> {
    let row: Option<(String, bool, Option<String>)> = tx
        .query_row(
            "SELECT status, archived, side_source_id FROM chats WHERE id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((status, archived, side)) = row else {
        return Ok(Err(Refusal::new("not_found", "That chat does not exist.")));
    };
    match rule {
        DeleteRule::Archived if !archived => {
            return Ok(Err(Refusal::new(
                "refused",
                "Only archived chats can be deleted; archive it first.",
            )))
        }
        DeleteRule::SideChat if side.is_none() => {
            return Ok(Err(Refusal::new(
                "refused",
                "This side chat was promoted; archive it to remove it.",
            )))
        }
        _ => {}
    }
    if ChatStatus::parse(&status).is_busy() {
        return Ok(Err(Refusal::new(
            "refused",
            "This chat is working; stop it before deleting.",
        )));
    }
    let mut rels = vec![
        format!("sessions/{chat_id}.jsonl"),
        format!("sessions/{chat_id}.jsonl.torn"),
        format!("sessions/{chat_id}.claude.json"),
        format!("workers/{chat_id}"),
    ];
    {
        // Artifacts no other chat references.
        let mut st = tx.prepare(
            "SELECT DISTINCT artifact FROM messages WHERE chat_id = ?1 AND artifact IS NOT NULL
               AND artifact NOT IN (SELECT artifact FROM messages WHERE chat_id != ?1 AND artifact IS NOT NULL)",
        )?;
        let names = st.query_map([chat_id], |r| r.get::<_, String>(0))?;
        for n in names {
            rels.push(format!("artifacts/{}", n?));
        }
    }
    {
        // Files saved for the chat from the phone, each in its own folder.
        let mut st = tx.prepare(
            "SELECT target FROM chat_attachment_refs WHERE chat_id = ?1 AND kind = 'file'",
        )?;
        let targets = st.query_map([chat_id], |r| r.get::<_, String>(0))?;
        for t in targets {
            if let Some(rel) = crate::attachments::saved_folder(&t?) {
                rels.push(rel);
            }
        }
    }
    {
        let mut st = tx.prepare("SELECT id FROM chat_attachments WHERE chat_id = ?1")?;
        let ids = st.query_map([chat_id], |r| r.get::<_, String>(0))?;
        for id in ids {
            let id = id?;
            rels.push(format!("thumbs/{id}"));
            rels.push(format!("attachments/{id}"));
        }
    }
    for rel in rels {
        tx.execute(
            "INSERT OR IGNORE INTO chat_file_cleanup(rel, queued_at) VALUES (?1, ?2)",
            params![rel, now],
        )?;
    }
    tx.execute("DELETE FROM chats WHERE id = ?1", [chat_id])?;
    Ok(Ok(()))
}

/// Remove queued files under `profile_root`. Returns how many queue rows were settled.
pub fn run_cleanup(db: &Database, profile_root: &Path) -> usize {
    let mut settled = 0;
    loop {
        let batch: Vec<String> = db
            .snapshot(|tx| {
                let mut st = tx.prepare(
                    "SELECT rel FROM chat_file_cleanup ORDER BY queued_at, rel LIMIT ?1",
                )?;
                let rows = st.query_map([BATCH as i64], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
            })
            .unwrap_or_default();
        if batch.is_empty() {
            return settled;
        }
        let mut done = Vec::new();
        for rel in &batch {
            if !allowed(rel) {
                log::error!("file cleanup: refusing to remove {rel:?}; dropped from the queue");
                done.push(rel.clone());
                continue;
            }
            let path = profile_root.join(rel);
            // A saved file's folder is a real folder (never a link) under attachments/.
            let folder = rel.starts_with("attachments/")
                && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
            let r = if rel.starts_with("workers/") || folder {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match r {
                Ok(()) => done.push(rel.clone()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => done.push(rel.clone()),
                Err(e) => {
                    log::warn!("file cleanup: {rel} not removed yet ({e}); retried at startup")
                }
            }
        }
        let n = done.len();
        let _ = db.write(|tx| {
            for rel in &done {
                tx.execute("DELETE FROM chat_file_cleanup WHERE rel = ?1", [rel])?;
            }
            Ok::<_, rusqlite::Error>(())
        });
        settled += n;
        if n == 0 {
            // Nothing in this batch could be removed; stop rather than spin.
            return settled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;
    use crate::store;

    #[test]
    fn only_patterned_names_inside_the_profile_folders_are_allowed() {
        let id = uuid::Uuid::new_v4().to_string();
        assert!(allowed(&format!("sessions/{id}.jsonl")));
        assert!(allowed(&format!("sessions/{id}.claude.json")));
        assert!(allowed(&format!("workers/{id}")));
        assert!(allowed(&format!("attachments/{id}")));
        assert!(allowed(&format!("thumbs/{id}")));
        assert!(!allowed("thumbs/not-an-id"));
        assert!(allowed(&format!("artifacts/{}.json", "a".repeat(64))));
        assert!(!allowed(&format!("sessions/{}.jsonl", id.to_uppercase())));
        assert!(!allowed(&format!("workers/{id}/../..")));
        assert!(!allowed("workers/.."));
        assert!(!allowed(&format!("artifacts/{}.json", "g".repeat(64))));
        assert!(!allowed(&format!("../sessions/{id}.jsonl")));
        assert!(!allowed(&format!("db/{id}.jsonl")));
    }

    #[test]
    fn delete_needs_archived_idle_and_removes_rows_files_and_search_text() {
        let (dir, db) = temp_db();
        let root = dir.path().join("profile");
        let (a, _b) = db
            .write(|tx| {
                let a = store::create_chat(tx, "C:/w", None, None, 1)?;
                let b = store::create_chat(tx, "C:/w", None, None, 1)?;
                let ins = "INSERT INTO messages(chat_id, source_key, role, text, timestamp, artifact) VALUES (?1, ?2, 'user', ?3, 1, ?4)";
                tx.execute(ins, params![a, "k1", "walrus", "a".repeat(64) + ".json"])?;
                tx.execute(ins, params![a, "k2", "walrus two", "b".repeat(64) + ".json"])?;
                tx.execute(ins, params![b, "k3", "other", "b".repeat(64) + ".json"])?;
                Ok::<_, rusqlite::Error>((a, b))
            })
            .unwrap();
        for rel in [
            format!("sessions/{a}.jsonl"),
            format!("workers/{a}/launch.json"),
            format!("artifacts/{}.json", "a".repeat(64)),
            format!("artifacts/{}.json", "b".repeat(64)),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        let refused = db
            .write(|tx| delete_in_tx(tx, &a, DeleteRule::Archived, 2))
            .unwrap()
            .unwrap_err();
        assert!(refused.message.contains("archive it first"));
        db.write(|tx| {
            tx.execute(
                "UPDATE chats SET archived = 1, status = 'running' WHERE id = ?1",
                [&a],
            )
        })
        .unwrap();
        let busy = db
            .write(|tx| delete_in_tx(tx, &a, DeleteRule::Archived, 2))
            .unwrap();
        assert!(busy.is_err(), "a busy chat is never deleted");
        db.write(|tx| tx.execute("UPDATE chats SET status = 'idle' WHERE id = ?1", [&a]))
            .unwrap();
        db.write(|tx| delete_in_tx(tx, &a, DeleteRule::Archived, 3))
            .unwrap()
            .unwrap();
        let (chats, msgs, hits): (i64, i64, i64) = db
            .snapshot(|tx| {
                Ok::<_, rusqlite::Error>((
                    tx.query_row("SELECT count(*) FROM chats WHERE id = ?1", [&a], |r| {
                        r.get(0)
                    })?,
                    tx.query_row(
                        "SELECT count(*) FROM messages WHERE chat_id = ?1",
                        [&a],
                        |r| r.get(0),
                    )?,
                    tx.query_row(
                        "SELECT count(*) FROM message_search WHERE message_search MATCH 'walrus'",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!((chats, msgs, hits), (0, 0, 0));
        assert_eq!(
            run_cleanup(&db, &root),
            5,
            "four named files plus the unshared artifact"
        );
        assert!(!root.join(format!("sessions/{a}.jsonl")).exists());
        assert!(!root.join(format!("workers/{a}")).exists());
        assert!(!root
            .join(format!("artifacts/{}.json", "a".repeat(64)))
            .exists());
        assert!(
            root.join(format!("artifacts/{}.json", "b".repeat(64)))
                .exists(),
            "an artifact another chat references is kept"
        );
        assert_eq!(run_cleanup(&db, &root), 0, "the queue is empty");
    }

    #[test]
    fn deleting_a_chat_removes_its_images_their_thumbnails_and_its_saved_files() {
        let (d, db) = temp_db();
        let root = d.path().to_path_buf();
        let chat = db
            .write(|tx| store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        db.write(|tx| {
            tx.execute(
                "INSERT INTO chat_attachments(id, chat_id, name, mime, bytes, width, height, created_at)
                 VALUES (?1, ?2, 'i', 'image/png', 1, 1, 1, 1)",
                params![id, chat],
            )?;
            tx.execute("UPDATE chats SET archived = 1 WHERE id = ?1", [&chat])
        })
        .unwrap();
        for rel in [format!("attachments/{id}"), format!("thumbs/{id}")] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        // A file saved from the phone goes with its folder.
        let dir = root.join("attachments");
        let mine = std::fs::read_dir(&dir).unwrap().count();
        let saved = db
            .write(|tx| {
                tx.execute("UPDATE chats SET archived = 0 WHERE id = ?1", [&chat])?;
                let r = crate::attachments::put_file(tx, &dir, &chat, "report.pdf", b"%PDF", 1)?;
                tx.execute("UPDATE chats SET archived = 1 WHERE id = ?1", [&chat])?;
                Ok::<_, rusqlite::Error>(r)
            })
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(&saved.target).unwrap(), b"%PDF");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), mine + 1);
        db.write(|tx| delete_in_tx(tx, &chat, DeleteRule::Archived, 2))
            .unwrap()
            .unwrap();
        run_cleanup(&db, &root);
        assert!(!root.join(format!("attachments/{id}")).exists());
        assert!(!root.join(format!("thumbs/{id}")).exists());
        assert!(!Path::new(&saved.target).exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn a_side_chat_discard_skips_the_archive_rule_but_needs_a_side_chat() {
        let (_d, db) = temp_db();
        let (src, side) = db
            .write(|tx| {
                let src = store::create_chat(tx, "C:/w", None, None, 1)?;
                let side = store::create_chat(tx, "C:/w", None, None, 1)?;
                tx.execute(
                    "UPDATE chats SET side_source_id = ?1 WHERE id = ?2",
                    [&src, &side],
                )?;
                Ok::<_, rusqlite::Error>((src, side))
            })
            .unwrap();
        assert!(db
            .write(|tx| delete_in_tx(tx, &src, DeleteRule::SideChat, 1))
            .unwrap()
            .is_err());
        db.write(|tx| delete_in_tx(tx, &side, DeleteRule::SideChat, 1))
            .unwrap()
            .unwrap();
    }
}
