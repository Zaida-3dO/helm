//! Size caps on tables that only grow. Each is a log of past events that nothing reads beyond its
//! recent end, so keeping the newest rows loses nothing anyone looks at. The caps are generous: they
//! stop a profile growing for years without bound, not trim what a person would notice.
//!
//! The cap is applied when the database opens. A table that does not exist yet (its feature did
//! not register) is skipped. Tables that hold records someone may need to look back on (chats,
//! messages, consent records, workflow events) are not listed.

use rusqlite::Connection;

/// (table, the column that orders it oldest to newest, most rows kept).
pub const CAPS: &[(&str, &str, i64)] = &[
    ("afk_events", "id", 100_000),
    ("schedule_runs", "rowid", 50_000),
    ("agent_feedback", "id", 10_000),
    ("consent_asks", "rowid", 50_000),
];

/// Trim every capped table to its newest rows. Answers how many rows were removed.
pub fn trim(conn: &Connection) -> rusqlite::Result<usize> {
    let mut removed = 0;
    for &(table, order, max) in CAPS {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )?;
        if !exists {
            continue;
        }
        let rows: i64 =
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        if rows <= max {
            continue;
        }
        removed += conn.execute(
            &format!(
                "DELETE FROM {table} WHERE rowid NOT IN
                     (SELECT rowid FROM {table} ORDER BY {order} DESC LIMIT ?1)"
            ),
            [max],
        )?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capped_table_keeps_its_newest_rows_and_a_missing_one_is_skipped() {
        let conn = Connection::open_in_memory().unwrap();
        // None of the listed tables exist yet.
        assert_eq!(trim(&conn).unwrap(), 0);
        conn.execute_batch(
            "CREATE TABLE agent_feedback (id INTEGER PRIMARY KEY AUTOINCREMENT, note TEXT);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10010)
             INSERT INTO agent_feedback(note) SELECT 'n' || i FROM n;",
        )
        .unwrap();
        assert_eq!(trim(&conn).unwrap(), 10);
        let (count, oldest): (i64, i64) = conn
            .query_row("SELECT count(*), min(id) FROM agent_feedback", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((count, oldest), (10_000, 11));
        assert_eq!(trim(&conn).unwrap(), 0, "already within its cap");
    }
}
