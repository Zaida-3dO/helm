//! The ranking boost: a small lift for chats the user
//! cares about right now, multiplied into the age-decayed text score.
//!
//! B = 0.25 if pinned + 0.25 if running (starting or stopping count) + 0.25 if in the current
//! project + 0.15 * ln(1 + decayed opens), capped at 0.5. The score is multiplied by (1 + B); B is
//! never added. Opens are counted per chat with a 14-day half-life, at most 5,000 chats kept.

use helm_core::db::Database;
use rusqlite::{params, Transaction};

pub const HALF_LIFE_MS: f64 = 14.0 * 24.0 * 60.0 * 60.0 * 1000.0;
pub const MAX_ROWS: i64 = 5_000;
pub const BOOST_CAP: f64 = 0.5;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS search_opens (
             chat_id     TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             opens       REAL NOT NULL,
             updated_at  INTEGER NOT NULL
         );",
    )
}

/// `opens` as of `now`, after the half-life decay since `updated_at`.
pub fn decayed(opens: f64, updated_at: i64, now: i64) -> f64 {
    let age = (now - updated_at).max(0) as f64;
    opens * 0.5_f64.powf(age / HALF_LIFE_MS)
}

/// The facts about one chat that the boost reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    pub pinned: bool,
    pub running: bool,
    pub current_project: bool,
    pub decayed_opens: f64,
}

pub fn boost(f: Facts) -> f64 {
    let mut b = 0.0;
    if f.pinned {
        b += 0.25;
    }
    if f.running {
        b += 0.25;
    }
    if f.current_project {
        b += 0.25;
    }
    b += 0.15 * (1.0 + f.decayed_opens.max(0.0)).ln();
    b.min(BOOST_CAP)
}

/// Count one open of `chat_id` and trim to the newest [`MAX_ROWS`].
pub fn record_open(db: &Database, chat_id: &str, now: i64) -> rusqlite::Result<()> {
    record_open_capped(db, chat_id, now, MAX_ROWS)
}

fn record_open_capped(db: &Database, chat_id: &str, now: i64, cap: i64) -> rusqlite::Result<()> {
    db.write(|tx| {
        let cur: Option<(f64, i64)> = tx
            .query_row(
                "SELECT opens, updated_at FROM search_opens WHERE chat_id = ?1",
                [chat_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let opens = cur.map_or(1.0, |(o, at)| decayed(o, at, now) + 1.0);
        tx.execute(
            "INSERT INTO search_opens(chat_id, opens, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(chat_id) DO UPDATE SET opens = excluded.opens, updated_at = excluded.updated_at",
            params![chat_id, opens, now],
        )?;
        tx.execute(
            "DELETE FROM search_opens WHERE chat_id NOT IN (
                 SELECT chat_id FROM search_opens ORDER BY updated_at DESC, chat_id LIMIT ?1)",
            [cap],
        )?;
        Ok(())
    })
}

/// Forget every recorded open.
pub fn clear(db: &Database) -> rusqlite::Result<()> {
    db.write(|tx| {
        tx.execute("DELETE FROM search_opens", [])?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    fn db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        crate::register_migrations(&mut reg);
        let db = Database::open(&dir.path().join("t.db"), &reg).unwrap();
        (dir, db)
    }

    fn chat(db: &Database, n: i64) -> String {
        db.write(|tx| helm_chats::store::create_chat(tx, "C:/a", None, None, n))
            .unwrap()
    }

    #[test]
    fn the_boost_adds_its_parts_and_caps_at_half() {
        assert_eq!(boost(Facts::default()), 0.0);
        let one = |f: Facts| (boost(f) * 1000.0).round() / 1000.0;
        assert_eq!(
            one(Facts {
                pinned: true,
                ..Facts::default()
            }),
            0.25
        );
        assert_eq!(
            one(Facts {
                running: true,
                ..Facts::default()
            }),
            0.25
        );
        assert_eq!(
            one(Facts {
                current_project: true,
                ..Facts::default()
            }),
            0.25
        );
        assert_eq!(
            one(Facts {
                pinned: true,
                running: true,
                current_project: true,
                decayed_opens: 50.0
            }),
            0.5,
            "capped"
        );
        let opens = boost(Facts {
            decayed_opens: 1.0,
            ..Facts::default()
        });
        assert!((opens - 0.15 * 2.0_f64.ln()).abs() < 1e-9);
    }

    #[test]
    fn opens_halve_every_fourteen_days() {
        let now = 100 * DAY;
        assert!((decayed(8.0, now - 14 * DAY, now) - 4.0).abs() < 1e-9);
        assert!((decayed(8.0, now - 28 * DAY, now) - 2.0).abs() < 1e-9);
        assert_eq!(decayed(8.0, now, now), 8.0);
    }

    #[test]
    fn recording_an_open_accumulates_with_decay() {
        let (_d, db) = db();
        let id = chat(&db, 1);
        record_open(&db, &id, 1_000 * DAY).unwrap();
        record_open(&db, &id, 1_000 * DAY).unwrap();
        record_open(&db, &id, 1_014 * DAY).unwrap();
        let (opens, at): (f64, i64) = db
            .snapshot(|tx| {
                tx.query_row("SELECT opens, updated_at FROM search_opens", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
            })
            .unwrap();
        assert!(
            (opens - 2.0).abs() < 1e-9,
            "2 decayed by half, plus 1: {opens}"
        );
        assert_eq!(at, 1_014 * DAY);
    }

    #[test]
    fn only_the_newest_chats_up_to_the_cap_are_kept() {
        let (_d, db) = db();
        let ids: Vec<String> = (0..8).map(|i| chat(&db, i)).collect();
        for (i, id) in ids.iter().enumerate() {
            record_open_capped(&db, id, 1_000 + i as i64, 5).unwrap();
        }
        let kept: Vec<String> = db
            .snapshot(|tx| {
                let mut st = tx.prepare("SELECT chat_id FROM search_opens ORDER BY updated_at")?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
            })
            .unwrap();
        assert_eq!(kept, ids[3..].to_vec(), "the three oldest were dropped");
        assert_eq!(MAX_ROWS, 5_000);
    }

    #[test]
    fn clearing_forgets_everything_and_deleting_a_chat_drops_its_row() {
        let (_d, db) = db();
        let a = chat(&db, 1);
        let b = chat(&db, 2);
        record_open(&db, &a, 5).unwrap();
        record_open(&db, &b, 5).unwrap();
        db.write(|tx| tx.execute("DELETE FROM chats WHERE id = ?1", [&a]))
            .unwrap();
        let count = |db: &Database| -> i64 {
            db.snapshot(|tx| tx.query_row("SELECT count(*) FROM search_opens", [], |r| r.get(0)))
                .unwrap()
        };
        assert_eq!(count(&db), 1);
        clear(&db).unwrap();
        assert_eq!(count(&db), 0);
    }
}
