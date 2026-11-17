//! What the Claude plan windows say right now, read from the row the usage meter keeps.
//!
//! The rules are the governor's bands, applied to the stored reading instead of the live
//! snapshot: a window whose reset time has passed counts as unknown (that is what releases a
//! capacity hold when the window reopens), a reading the meter read too long ago answers nothing,
//! and the fuller of the two windows wins. `None` means nothing is known, which never holds
//! anything and never stops anything.
//!
//! The row itself belongs to the usage crate; this reads it by name and field, the same way the
//! queue does, so crates far down the tree (chats, governor) can ask the question without a
//! dependency cycle.

use crate::db::Database;
use rusqlite::OptionalExtension;

/// The percent of the plan windows at which a Claude turn start holds (0 switches the hold off).
pub const DEFAULT_CLAUDE_HOLD_PCT: u64 = 95;

/// A usage reading older than this answers nothing. The usage meter keeps its own copy of this
/// line for the snapshot it shows; this is the same distance for readers of the stored row.
const STALE_AFTER_MS: i64 = 15 * 60_000;

/// The plan windows as they stand: the fuller of the live windows, and when it reopens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Capacity {
    /// The higher percent in use, 0 to 100.
    pub pct: f64,
    /// When that window resets.
    pub until: Option<i64>,
}

impl Capacity {
    pub fn full_at(&self, hold_pct: u64) -> bool {
        self.pct >= hold_pct as f64
    }
}

/// Read the stored usage row. `None` for a missing row or a body that does not parse.
fn stored(db: &Database) -> Option<serde_json::Value> {
    let body: rusqlite::Result<Option<String>> = db.snapshot(|tx| {
        tx.query_row("SELECT body FROM plan_usage WHERE id = 1", [], |r| r.get(0))
            .optional()
    });
    match body {
        Ok(Some(b)) => serde_json::from_str(&b).ok(),
        _ => None,
    }
}

/// The plan windows, with dead readings dropped. `None` means nothing is known.
pub fn capacity(db: &Database, now: i64) -> Option<Capacity> {
    let stored = stored(db)?;
    // Freshness is the whole reading's, not one window's: the newest window read sets the age,
    // exactly as the meter's stale line does (per-model windows count towards it there too).
    let newest = ["fiveHour", "weekly"]
        .iter()
        .filter_map(|k| stored.get(*k)?.get("at")?.as_i64())
        .chain(
            stored
                .get("perModel")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|m| m.get("at")?.as_i64()),
        )
        .max()?;
    if now - newest > STALE_AFTER_MS {
        return None;
    }
    let mut best: Option<Capacity> = None;
    for key in ["fiveHour", "weekly"] {
        let Some(w) = stored.get(key) else { continue };
        let pct = w.get("pct")?.as_f64()?;
        if !pct.is_finite() {
            continue;
        }
        let resets = w.get("resetsAt").and_then(|v| v.as_i64());
        if resets.is_some_and(|r| r <= now) {
            continue;
        }
        if best.is_none_or(|b| pct > b.pct) {
            best = Some(Capacity { pct, until: resets });
        }
    }
    best
}

/// The capacity hold threshold as the user set it (`claudeHoldPct`; 0 switches the hold off).
pub fn hold_pct(db: &Database) -> u64 {
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(crate::prefs::read(tx, "claudeHoldPct")))
        .ok()
        .flatten()
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_CLAUDE_HOLD_PCT)
}

/// Whether Claude's plan windows read at or over the hold threshold right now. `Some` gives the
/// fuller window and when it reopens; `None` is "not full" (including unknown).
pub fn claude_full(db: &Database, now: i64) -> Option<Capacity> {
    let hold = hold_pct(db);
    if hold == 0 {
        return None;
    }
    let cap = capacity(db, now)?;
    cap.full_at(hold).then_some(cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MigrationRegistry;
    use serde_json::json;

    /// A temp database with just the usage row, the way the usage crate keeps it.
    fn usage_db(body: Option<serde_json::Value>) -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let reg = MigrationRegistry::new();
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS plan_usage (
                     id INTEGER PRIMARY KEY CHECK (id = 1), body TEXT NOT NULL, updated_at INTEGER NOT NULL);",
            )?;
            if let Some(b) = body {
                tx.execute(
                    "INSERT INTO plan_usage(id, body, updated_at) VALUES (1, ?1, 0)",
                    [b.to_string()],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        (dir, db)
    }

    #[test]
    fn the_windows_answer_full_only_while_live_and_fresh() {
        let now = 1_000_000_000_000i64;
        let win =
            |pct: f64, resets: i64, at: i64| json!({"pct": pct, "resetsAt": resets, "at": at});
        // A live window past the line: full, and it says when it reopens.
        let (_d, db) = usage_db(Some(
            json!({"weekly": win(96.0, now + 3_600_000, now - 60_000)}),
        ));
        let cap = capacity(&db, now).expect("the window answers");
        assert!(cap.full_at(95));
        assert_eq!(cap.until, Some(now + 3_600_000));
        // The fuller of two live windows answers.
        drop(db);
        let (_d, db) = usage_db(Some(json!({
            "fiveHour": win(40.0, now + 900_000, now - 60_000),
            "weekly": win(97.5, now + 86_400_000, now - 120_000),
        })));
        assert_eq!(capacity(&db, now).unwrap().pct, 97.5);
        // A window whose reset has passed answers nothing: that is the automatic release.
        drop(db);
        let (_d, db) = usage_db(Some(json!({"weekly": win(99.0, now - 1, now - 60_000)})));
        assert!(capacity(&db, now).is_none());
        // A reading younger than one window but older than the stale line answers nothing either
        // (unknown, never full); a fresher window anywhere keeps the reading alive.
        drop(db);
        let (_d, db) = usage_db(Some(
            json!({"weekly": win(99.0, now + 3_600_000, now - STALE_AFTER_MS - 1)}),
        ));
        assert!(capacity(&db, now).is_none());
        drop(db);
        let (_d, db) = usage_db(Some(json!({
            "weekly": win(99.0, now + 3_600_000, now - STALE_AFTER_MS - 1),
            "perModel": [{"model": "Opus", "at": now - 60_000}],
        })));
        assert_eq!(
            capacity(&db, now).unwrap().pct,
            99.0,
            "a fresh line keeps it"
        );
        // Under the line: not full. And with no reading at all: unknown, not full.
        drop(db);
        let (_d, db) = usage_db(Some(
            json!({"weekly": win(94.0, now + 3_600_000, now - 60_000)}),
        ));
        assert!(!capacity(&db, now).unwrap().full_at(95));
        drop(db);
        let (_d, db) = usage_db(None);
        assert!(capacity(&db, now).is_none());
    }

    #[test]
    fn the_hold_line_is_the_users_and_off_means_never_full() {
        let now = 1_000_000_000_000i64;
        let win = |pct: f64| json!({"pct": pct, "resetsAt": now + 3_600_000, "at": now - 60_000});
        let (_d, db) = usage_db(Some(json!({"weekly": win(96.0)})));
        assert_eq!(hold_pct(&db), DEFAULT_CLAUDE_HOLD_PCT);
        assert!(claude_full(&db, now).is_some());
        db.write(|tx| crate::prefs::set(tx, crate::prefs::CLAUDE_HOLD_PCT, &serde_json::json!(0)))
            .unwrap();
        assert!(claude_full(&db, now).is_none(), "0 switches the hold off");
        db.write(|tx| crate::prefs::set(tx, crate::prefs::CLAUDE_HOLD_PCT, &serde_json::json!(99)))
            .unwrap();
        assert!(
            claude_full(&db, now).is_none(),
            "96 is under a 99 percent line"
        );
    }
}
