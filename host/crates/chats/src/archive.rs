//! Archive and restore at the transaction level.
//!
//! These are `pub` so the projects crate can archive and restore a project's chats inside its own
//! transaction. The dependency runs projects → chats, never the reverse: nothing here reads a
//! projects-crate table. Whether a project is archived is that crate's business; a caller that
//! restores a single chat checks it first (the `chat_restore` op asks [`crate::seams::ProjectGate`]).
//!
//! Neither function touches a worker. The `chat_archive` op retires the idle worker after its
//! transaction commits.

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

pub const BUSY_MESSAGE: &str = "This chat is working; stop the task first, then archive it.";

/// Archive one chat. Refused while the chat is busy; archiving an archived chat changes nothing
/// and answers `Ok(false)` (idempotent).
pub fn archive_in_tx(
    tx: &Transaction<'_>,
    chat_id: &str,
    now: i64,
) -> rusqlite::Result<Result<bool, Refusal>> {
    let row: Option<(String, bool)> = tx
        .query_row(
            "SELECT status, archived FROM chats WHERE id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((status, archived)) = row else {
        return Ok(Err(Refusal::new("not_found", "That chat does not exist.")));
    };
    if archived {
        return Ok(Ok(false));
    }
    if ChatStatus::parse(&status).is_busy() {
        return Ok(Err(Refusal::new("refused", BUSY_MESSAGE)));
    }
    tx.execute(
        "UPDATE chats SET archived = 1, updated_at = ?1 WHERE id = ?2",
        params![now, chat_id],
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
    fn archive_refuses_busy_chats_and_is_idempotent() {
        let (_d, db) = temp_db();
        let c = db
            .write(|tx| store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        db.write(|tx| store::set_status(tx, &c, ChatStatus::Running, None, 2))
            .unwrap();
        let r = db.write(|tx| archive_in_tx(tx, &c, 3)).unwrap();
        assert_eq!(r.unwrap_err().message, BUSY_MESSAGE);
        db.write(|tx| store::set_status(tx, &c, ChatStatus::Idle, None, 4))
            .unwrap();
        assert_eq!(db.write(|tx| archive_in_tx(tx, &c, 5)).unwrap(), Ok(true));
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
