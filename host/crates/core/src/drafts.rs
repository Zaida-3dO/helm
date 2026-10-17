//! Draft saves with compare-and-set on `chats.draft_revision`.
//!
//! This module provides the storage rule only; the composer calls it.

use crate::db::Database;
use crate::revisions::{self, RevisionError};

/// Drafts are capped at 2 MB.
pub const MAX_DRAFT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum DraftError {
    #[error("no such chat")]
    NotFound,
    #[error("draft is larger than 2 MB")]
    TooLong,
    #[error(transparent)]
    Revision(#[from] RevisionError),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

/// Save `text` as the chat's draft if `expected_revision` is still current. Returns the new
/// revision. A stale revision is refused and nothing changes.
pub fn save_draft(
    db: &Database,
    chat_id: &str,
    text: &str,
    expected_revision: i64,
) -> Result<i64, DraftError> {
    if text.len() > MAX_DRAFT_BYTES {
        return Err(DraftError::TooLong);
    }
    db.write(|tx| {
        let current: Option<i64> = tx
            .query_row(
                "SELECT draft_revision FROM chats WHERE id = ?1",
                [chat_id],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })?;
        let current = current.ok_or(DraftError::NotFound)?;
        let next = revisions::check_stored(current, expected_revision)?;
        tx.execute(
            "UPDATE chats SET draft = ?1, draft_revision = ?2, updated_at = ?3 WHERE id = ?4",
            (text, next, crate::now_ms(), chat_id),
        )?;
        revisions::bump_global(tx)?;
        Ok(next)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{tests::temp_db, MigrationRegistry};

    fn chat(db: &Database) {
        db.write(|tx| {
            tx.execute(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('c', 'p', 0, 0)",
                [],
            )
        })
        .map(|_: usize| ())
        .unwrap();
    }

    fn draft(db: &Database) -> (String, i64) {
        db.snapshot(|tx| {
            tx.query_row(
                "SELECT draft, draft_revision FROM chats WHERE id = 'c'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap()
    }

    #[test]
    fn a_stale_revision_is_refused_and_changes_nothing() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        chat(&db);
        assert_eq!(save_draft(&db, "c", "one", 0).unwrap(), 1);
        let stale = save_draft(&db, "c", "two", 0).unwrap_err();
        assert!(matches!(
            stale,
            DraftError::Revision(RevisionError::Stale { .. })
        ));
        assert_eq!(draft(&db), ("one".to_owned(), 1));
        assert_eq!(save_draft(&db, "c", "two", 1).unwrap(), 2);
        assert_eq!(draft(&db), ("two".to_owned(), 2));
    }

    #[test]
    fn unknown_chat_and_oversize_are_refused() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        assert!(matches!(
            save_draft(&db, "x", "t", 0),
            Err(DraftError::NotFound)
        ));
        let big = "a".repeat(MAX_DRAFT_BYTES + 1);
        assert!(matches!(
            save_draft(&db, "x", &big, 0),
            Err(DraftError::TooLong)
        ));
    }
}
