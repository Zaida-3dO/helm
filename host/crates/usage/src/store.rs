//! Merging readings ("newer data wins per window") and keeping the last one in the
//! database so a launch shows it at once, marked stale.

use crate::types::*;
use helm_core::db::Database;
use rusqlite::{params, OptionalExtension, Transaction};

/// A reading is stale after 15 minutes.
pub const STALE_AFTER_MS: i64 = 15 * 60_000;

/// Reset times of one window drift by milliseconds between reads; this is "the same window".
const SAME_WINDOW_MS: i64 = 5 * 60_000;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS plan_usage (
             id          INTEGER PRIMARY KEY CHECK (id = 1),
             body        TEXT NOT NULL,
             updated_at  INTEGER NOT NULL
         );",
    )
}

pub fn load(db: &Database) -> StoredUsage {
    let body: rusqlite::Result<Option<String>> = db.snapshot(|tx| {
        tx.query_row("SELECT body FROM plan_usage WHERE id = 1", [], |r| r.get(0))
            .optional()
    });
    match body {
        Ok(Some(b)) => serde_json::from_str(&b).unwrap_or_default(),
        Ok(None) => StoredUsage::default(),
        Err(e) => {
            log::warn!("plan usage could not be read: {e}");
            StoredUsage::default()
        }
    }
}

pub fn save(db: &Database, s: &StoredUsage, now: i64) {
    let Ok(body) = serde_json::to_string(s) else {
        return;
    };
    let r: rusqlite::Result<()> = db.write(|tx| {
        tx.execute(
            "INSERT INTO plan_usage (id, body, updated_at) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
            params![body, now],
        )?;
        Ok(())
    });
    if let Err(e) = r {
        log::warn!("plan usage could not be saved: {e}");
    }
}

fn same_window(a: Option<i64>, b: Option<i64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < SAME_WINDOW_MS,
        _ => true,
    }
}

/// The window to keep when `new` arrives over `old`.
pub fn merge_window(old: Option<&UsageWindow>, new: UsageWindow) -> UsageWindow {
    let Some(o) = old else { return new };
    if new.at < o.at {
        return o.clone();
    }
    // A chat's rate-limit line often carries a reset time but no number. It must not wipe a
    // number we hold for the same window; a different reset time is a new window, so it replaces.
    if new.pct.is_none() && o.pct.is_some() && same_window(o.resets_at, new.resets_at) {
        let mut kept = o.clone();
        kept.at = new.at.max(o.at);
        return kept;
    }
    new
}

/// A control reading replaces what it covers; windows it does not mention are kept.
pub fn apply_control(
    s: &mut StoredUsage,
    plan: Option<String>,
    five_hour: Option<(Option<f64>, Option<i64>)>,
    weekly: Option<(Option<f64>, Option<i64>)>,
    per_model: Vec<(String, Option<f64>, Option<i64>)>,
    at: i64,
) {
    let win = |w: (Option<f64>, Option<i64>)| UsageWindow {
        pct: w.0,
        resets_at: w.1,
        at,
        source: UsageSource::Control,
    };
    if plan.is_some() {
        s.plan = plan;
    }
    if let Some(w) = five_hour {
        s.five_hour = Some(merge_window(s.five_hour.as_ref(), win(w)));
    }
    if let Some(w) = weekly {
        s.weekly = Some(merge_window(s.weekly.as_ref(), win(w)));
    }
    // The control answer lists every per-model window the plan has, so it replaces the list.
    s.per_model = per_model
        .into_iter()
        .map(|(model, pct, resets_at)| ModelUsage {
            model,
            pct,
            resets_at,
            at,
            source: UsageSource::Control,
        })
        .collect();
}

/// One window from a chat's `rate_limit_event`.
pub struct Passive {
    pub kind: String,
    pub pct: Option<f64>,
    pub resets_at: Option<i64>,
}

/// Merge a chat's rate-limit windows. Returns whether anything changed.
pub fn apply_passive(s: &mut StoredUsage, windows: &[Passive], at: i64) -> bool {
    let before = s.clone();
    for w in windows {
        let new = UsageWindow {
            pct: w.pct,
            resets_at: w.resets_at,
            at,
            source: UsageSource::RateLimitEvent,
        };
        match w.kind.as_str() {
            "five_hour" => s.five_hour = Some(merge_window(s.five_hour.as_ref(), new)),
            "seven_day" => s.weekly = Some(merge_window(s.weekly.as_ref(), new)),
            "seven_day_opus" | "seven_day_sonnet" => {
                let model = if w.kind == "seven_day_opus" {
                    "Opus"
                } else {
                    "Sonnet"
                };
                let old = s.per_model.iter().position(|m| m.model == model);
                let prev = old.map(|i| UsageWindow {
                    pct: s.per_model[i].pct,
                    resets_at: s.per_model[i].resets_at,
                    at: s.per_model[i].at,
                    source: s.per_model[i].source,
                });
                let kept = merge_window(prev.as_ref(), new);
                let m = ModelUsage {
                    model: model.into(),
                    pct: kept.pct,
                    resets_at: kept.resets_at,
                    at: kept.at,
                    source: kept.source,
                };
                match old {
                    Some(i) => s.per_model[i] = m,
                    None => s.per_model.push(m),
                }
            }
            _ => {}
        }
    }
    *s != before
}

/// The reading to show, or `None` when no window is held.
pub fn reading(s: &StoredUsage, now: i64) -> Option<PlanUsageReading> {
    let mut newest: Option<(i64, UsageSource)> = None;
    let mut note = |at: i64, src: UsageSource| {
        if newest.is_none_or(|(t, _)| at >= t) {
            newest = Some((at, src));
        }
    };
    if let Some(w) = &s.five_hour {
        note(w.at, w.source);
    }
    if let Some(w) = &s.weekly {
        note(w.at, w.source);
    }
    for m in &s.per_model {
        note(m.at, m.source);
    }
    let (fetched_at, source) = newest?;
    Some(PlanUsageReading {
        plan: s.plan.clone(),
        five_hour: s.five_hour.clone(),
        weekly: s.weekly.clone(),
        per_model: s.per_model.clone(),
        source,
        fetched_at,
        stale: now - fetched_at > STALE_AFTER_MS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(pct: Option<f64>, resets: Option<i64>, at: i64, source: UsageSource) -> UsageWindow {
        UsageWindow {
            pct,
            resets_at: resets,
            at,
            source,
        }
    }

    #[test]
    fn newer_wins_per_window_and_older_never_overwrites() {
        let old = w(Some(10.0), Some(1_000_000), 100, UsageSource::Control);
        let newer = w(
            Some(20.0),
            Some(1_000_000),
            200,
            UsageSource::RateLimitEvent,
        );
        assert_eq!(merge_window(Some(&old), newer.clone()).pct, Some(20.0));
        let older = w(Some(5.0), Some(1_000_000), 50, UsageSource::RateLimitEvent);
        assert_eq!(merge_window(Some(&old), older).pct, Some(10.0));
        assert_eq!(merge_window(None, newer.clone()), newer);
    }

    #[test]
    fn a_reset_only_line_keeps_the_number_for_the_same_window_only() {
        let old = w(Some(40.0), Some(1_000_000), 100, UsageSource::Control);
        let same = w(None, Some(1_000_500), 200, UsageSource::RateLimitEvent);
        let kept = merge_window(Some(&old), same);
        assert_eq!(kept.pct, Some(40.0));
        assert_eq!(
            kept.source,
            UsageSource::Control,
            "the number is still the control's"
        );
        // A reset time an hour later is a new window: the old number no longer applies.
        let fresh = w(
            None,
            Some(1_000_000 + 3_600_000),
            200,
            UsageSource::RateLimitEvent,
        );
        assert_eq!(merge_window(Some(&old), fresh).pct, None);
    }

    #[test]
    fn control_replaces_the_model_list_and_passive_updates_one_model() {
        let mut s = StoredUsage::default();
        apply_control(
            &mut s,
            Some("max".into()),
            Some((Some(5.0), Some(10))),
            Some((Some(50.0), Some(20))),
            vec![("Fable".into(), Some(1.0), Some(20))],
            100,
        );
        assert_eq!(s.per_model.len(), 1);
        let changed = apply_passive(
            &mut s,
            &[Passive {
                kind: "seven_day_sonnet".into(),
                pct: Some(30.0),
                resets_at: Some(20),
            }],
            200,
        );
        assert!(changed);
        assert_eq!(s.per_model.len(), 2);
        // The next control answer lists the plan's models afresh.
        apply_control(
            &mut s,
            None,
            None,
            None,
            vec![("Fable".into(), Some(2.0), Some(20))],
            300,
        );
        assert_eq!(s.per_model.len(), 1);
        assert_eq!(
            s.plan.as_deref(),
            Some("max"),
            "a missing plan keeps the known one"
        );
    }

    #[test]
    fn staleness_is_fifteen_minutes_from_the_newest_window() {
        let mut s = StoredUsage::default();
        assert!(reading(&s, 0).is_none());
        apply_control(&mut s, None, Some((Some(1.0), None)), None, vec![], 1_000);
        assert!(!reading(&s, 1_000 + STALE_AFTER_MS).unwrap().stale);
        assert!(reading(&s, 1_001 + STALE_AFTER_MS).unwrap().stale);
        // A fresher chat line makes the whole reading fresh again.
        apply_passive(
            &mut s,
            &[Passive {
                kind: "seven_day".into(),
                pct: Some(2.0),
                resets_at: None,
            }],
            1_000 + STALE_AFTER_MS,
        );
        let r = reading(&s, 1_001 + STALE_AFTER_MS).unwrap();
        assert!(!r.stale);
        assert_eq!(r.source, UsageSource::RateLimitEvent);
    }

    #[test]
    fn the_last_reading_survives_a_restart() {
        let mut reg = helm_core::db::MigrationRegistry::new();
        reg.initializer("plan-usage", init);
        let dir = tempfile::tempdir().unwrap();
        let db = helm_core::db::Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        assert_eq!(load(&db), StoredUsage::default());
        let mut s = StoredUsage::default();
        apply_control(
            &mut s,
            Some("max".into()),
            Some((Some(5.0), Some(7))),
            None,
            vec![],
            100,
        );
        save(&db, &s, 100);
        save(&db, &s, 101);
        assert_eq!(load(&db), s);
    }
}
