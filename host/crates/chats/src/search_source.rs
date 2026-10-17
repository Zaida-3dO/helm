//! The read-only source of searchable message text, for the search crate.
//!
//! The search crate builds its index on this and on nothing else, so the chats crate's rules stay
//! in one place. A row is searchable when it is:
//! - a user or assistant message with text (tool output, thinking and context rows are excluded);
//! - on the active branch (an edit's abandoned branch is inactive);
//! - in a chat that is not an unpromoted side chat;
//! - in an unarchived chat, unless the caller asks for archived chats too (search's Archived scope).
//!
//! The rules live in the SQL view `chat_search_source` (created by this crate's initializer), so a
//! search crate may also join it directly. Core's `message_search` FTS table covers every user and
//! assistant row; [`matching`] filters its hits through the view.

use rusqlite::{params, Transaction};

/// The view's name.
pub const VIEW: &str = "chat_search_source";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchText {
    pub message_id: i64,
    pub chat_id: String,
    /// `user` or `assistant`.
    pub role: String,
    pub text: String,
    pub timestamp: i64,
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SearchText> {
    Ok(SearchText {
        message_id: r.get(0)?,
        chat_id: r.get(1)?,
        role: r.get(2)?,
        text: r.get(3)?,
        timestamp: r.get(4)?,
    })
}

/// Searchable rows with `message_id > after`, ascending, at most `limit` (for building an index).
pub fn page(
    tx: &Transaction<'_>,
    after: i64,
    limit: i64,
    include_archived: bool,
) -> rusqlite::Result<Vec<SearchText>> {
    let mut st = tx.prepare_cached(
        "SELECT message_id, chat_id, role, text, timestamp FROM chat_search_source
         WHERE message_id > ?1 AND (?3 OR archived = 0) ORDER BY message_id LIMIT ?2",
    )?;
    let rows = st.query_map(params![after, limit, include_archived], row)?;
    rows.collect()
}

/// Whether one message is searchable right now (to re-check a cached hit).
pub fn is_searchable(
    tx: &Transaction<'_>,
    message_id: i64,
    include_archived: bool,
) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_search_source WHERE message_id = ?1 AND (?2 OR archived = 0))",
        params![message_id, include_archived],
        |r| r.get(0),
    )
}

/// Full-text hits for an FTS5 `MATCH` expression, best first, filtered through the view. The
/// caller builds and escapes the expression.
pub fn matching(
    tx: &Transaction<'_>,
    fts_query: &str,
    limit: i64,
    include_archived: bool,
) -> rusqlite::Result<Vec<SearchText>> {
    let mut st = tx.prepare_cached(
        "SELECT s.message_id, s.chat_id, s.role, s.text, s.timestamp
         FROM message_search f JOIN chat_search_source s ON s.message_id = f.rowid
         WHERE message_search MATCH ?1 AND (?3 OR s.archived = 0)
         ORDER BY f.rank LIMIT ?2",
    )?;
    let rows = st.query_map(params![fts_query, limit, include_archived], row)?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;
    use crate::store;

    #[test]
    fn the_source_excludes_inactive_branches_side_chats_and_archived_chats() {
        let (_d, db) = temp_db();
        let (a, side, arch) = db
            .write(|tx| {
                let a = store::create_chat(tx, "C:/w", None, None, 1)?;
                let side = store::create_chat(tx, "C:/w", None, None, 1)?;
                let arch = store::create_chat(tx, "C:/w", None, None, 1)?;
                tx.execute("UPDATE chats SET side_source_id = ?1 WHERE id = ?2", [&a, &side])?;
                tx.execute("UPDATE chats SET archived = 1 WHERE id = ?1", [&arch])?;
                let ins = "INSERT INTO messages(chat_id, source_key, role, text, timestamp, active) VALUES (?1, ?2, ?3, ?4, 1, ?5)";
                tx.execute(ins, params![a, "k1", "user", "zebra active", 1])?;
                tx.execute(ins, params![a, "k2", "user", "zebra abandoned", 0])?;
                tx.execute(ins, params![a, "k3", "toolResult", "zebra tool", 1])?;
                tx.execute(ins, params![side, "k4", "assistant", "zebra side", 1])?;
                tx.execute(ins, params![arch, "k5", "assistant", "zebra archived", 1])?;
                Ok::<_, rusqlite::Error>((a, side, arch))
            })
            .unwrap();
        let texts = |v: Vec<SearchText>| v.into_iter().map(|t| t.text).collect::<Vec<_>>();
        let open = db.snapshot(|tx| page(tx, 0, 100, false)).unwrap();
        assert_eq!(texts(open), ["zebra active"]);
        let all = db.snapshot(|tx| page(tx, 0, 100, true)).unwrap();
        assert_eq!(texts(all), ["zebra active", "zebra archived"]);
        let hits = db.snapshot(|tx| matching(tx, "zebra*", 10, false)).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chat_id, a);
        let side_id: i64 = db
            .snapshot(|tx| {
                tx.query_row("SELECT id FROM messages WHERE source_key = 'k4'", [], |r| {
                    r.get(0)
                })
            })
            .unwrap();
        assert!(!db.snapshot(|tx| is_searchable(tx, side_id, true)).unwrap());
        // Promoting the side chat makes its text searchable.
        db.write(|tx| {
            tx.execute(
                "UPDATE chats SET side_source_id = NULL WHERE id = ?1",
                [&side],
            )
        })
        .unwrap();
        assert!(db.snapshot(|tx| is_searchable(tx, side_id, false)).unwrap());
        let _ = arch;
    }
}
