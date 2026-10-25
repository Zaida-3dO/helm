//! Archive and restore at the transaction level.
//!
//! These are `pub` so the projects crate can archive and restore a project's chats inside its own
//! transaction. The dependency runs projects → chats, never the reverse: nothing here reads a
//! projects-crate table. Whether a project is archived is that crate's business; a caller that
//! restores a single chat checks it first (the `chat_restore` op asks [`crate::seams::ProjectGate`]).
//!
//! Neither function touches a worker. The `chat_archive` op tells the chat's actor after its
//! transaction commits: an idle worker is retired, a working one is stopped in the background
//! and closed if the stop is slow. Archiving never waits for either.

use crate::store;
use crate::types::*;
use rusqlite::{params, OptionalExtension, Transaction};

/// A refusal a caller can show as it is (`code` is an `ApiError` code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<Refusal> for helm_ipc::ApiError {
    fn from(r: Refusal) -> Self {
        helm_ipc::ApiError::new(r.code, r.message)
    }
}

/// Archive one chat, busy or not; archiving an archived chat changes nothing and answers
/// `Ok(false)` (idempotent). Nothing queued for the chat sends afterwards: a message waiting for a
/// run slot is cancelled (it reads Not sent) and every message still queued reads Not delivered,
/// so its text can be taken back after a restore.
pub fn archive_in_tx(
    tx: &Transaction<'_>,
    chat_id: &str,
    now: i64,
) -> rusqlite::Result<Result<bool, Refusal>> {
    let row: Option<bool> = tx
        .query_row("SELECT archived FROM chats WHERE id = ?1", [chat_id], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(archived) = row else {
        return Ok(Err(Refusal::new("not_found", "That chat does not exist.")));
    };
    if archived {
        return Ok(Ok(false));
    }
    tx.execute(
        "UPDATE chats SET archived = 1, updated_at = ?1 WHERE id = ?2",
        params![now, chat_id],
    )?;
    tx.execute(
        "UPDATE submissions SET state = 'rejected' WHERE state = 'saved'
         AND id IN (SELECT submission_id FROM slot_waits WHERE chat_id = ?1)",
        [chat_id],
    )?;
    tx.execute("DELETE FROM slot_waits WHERE chat_id = ?1", [chat_id])?;
    tx.execute(
        "UPDATE submissions SET state = 'uncertain' WHERE chat_id = ?1 AND state = 'queued'",
        [chat_id],
    )?;
    Ok(Ok(true))
}

/// Restore one chat. Restoring a chat that is not archived answers `Ok(false)`.
pub fn restore_in_tx(
    tx: &Transaction<'_>,
    chat_id: &str,
    now: i64,
) -> rusqlite::Result<Result<bool, Refusal>> {
    let archived: Option<bool> = tx
        .query_row("SELECT archived FROM chats WHERE id = ?1", [chat_id], |r| {
            r.get(0)
        })
        .optional()?;
    match archived {
        None => Ok(Err(Refusal::new("not_found", "That chat does not exist."))),
        Some(false) => Ok(Ok(false)),
        Some(true) => {
            tx.execute(
                "UPDATE chats SET archived = 0, updated_at = ?1 WHERE id = ?2",
                params![now, chat_id],
            )?;
            Ok(Ok(true))
        }
    }
}

/// The chat as the UI sees it, at `revision`.
pub fn chat_record(
    tx: &Transaction<'_>,
    chat_id: &str,
    revision: i64,
) -> rusqlite::Result<Option<ChatRecord>> {
    store::get_chat(tx, chat_id, revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;

    #[test]
    fn archive_takes_busy_chats_at_once_and_is_idempotent() {
        let (_d, db) = temp_db();
        let c = db
            .write(|tx| store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        db.write(|tx| store::set_status(tx, &c, ChatStatus::Running, None, 2))
            .unwrap();
        // A queued follow-up and a message waiting for a run slot stay unsent.
        db.write(|tx| {
            for (id, state) in [("s1", "queued"), ("s2", "saved")] {
                tx.execute(
                    "INSERT INTO submissions(id, chat_id, text, state, created_at)
                     VALUES (?1, ?2, 'hi', ?3, 1)",
                    params![id, c, state],
                )?;
            }
            tx.execute(
                "INSERT INTO queued_submissions(submission_id, chat_id, position) VALUES ('s1', ?1, 0)",
                [&c],
            )?;
            tx.execute(
                "INSERT INTO slot_waits(submission_id, chat_id, since, head) VALUES ('s2', ?1, 1, 0)",
                [&c],
            )
        })
        .unwrap();
        assert_eq!(db.write(|tx| archive_in_tx(tx, &c, 3)).unwrap(), Ok(true));
        let col = |sql: &str| -> Vec<String> {
            db.snapshot(|tx| {
                let mut st = tx.prepare(sql)?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()
            })
            .unwrap()
        };
        assert_eq!(col("SELECT status FROM chats"), ["running"]);
        assert!(col("SELECT submission_id FROM slot_waits").is_empty());
        assert_eq!(
            col("SELECT state FROM submissions ORDER BY id"),
            ["uncertain", "rejected"]
        );
        db.write(|tx| store::set_status(tx, &c, ChatStatus::Idle, None, 4))
            .unwrap();
        assert_eq!(db.write(|tx| archive_in_tx(tx, &c, 5)).unwrap(), Ok(false));
        assert_eq!(db.write(|tx| archive_in_tx(tx, &c, 6)).unwrap(), Ok(false));
        let rec = db.snapshot(|tx| chat_record(tx, &c, 1)).unwrap().unwrap();
        assert!(rec.archived);
        assert!(
            db.snapshot(|tx| store::list_chats(tx, 1))
                .unwrap()
                .is_empty(),
            "archived chats leave the list"
        );
        assert_eq!(db.write(|tx| restore_in_tx(tx, &c, 7)).unwrap(), Ok(true));
        assert_eq!(db.write(|tx| restore_in_tx(tx, &c, 8)).unwrap(), Ok(false));
        assert_eq!(
            db.write(|tx| archive_in_tx(tx, "nope", 9))
                .unwrap()
                .unwrap_err()
                .code,
            "not_found"
        );
    }
}
