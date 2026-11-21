//! Merging readings ("newer data wins per window") and keeping the last one in the
//! database so a launch shows it at once, marked stale.

use crate::types::*;
use helm_core::db::Database;
use rusqlite::{params, OptionalExtension, Transaction};

/// A reading is stale after 15 minutes.
pub const STALE_AFTER_MS: i64 = 15 * 60_000;

/// A number the control read gave is not replaced by a chat's estimate for the same window while it
/// is this young: the chat's line is a guess from its own traffic, the control read is the figure.
pub const CONTROL_HOLD_MS: i64 = 15 * 60_000;

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

/// True when a control read brings a number **below** the one held for the same window (five_hour
/// or weekly). Usage only climbs inside a window — a window that began again carries a new reset
/// time — so a lower number for the same window is a suspect answer, worth one confirming
/// re-read before it is accepted.
pub fn below_held(
    s: &StoredUsage,
    five_hour: &Option<(Option<f64>, Option<i64>)>,
    weekly: &Option<(Option<f64>, Option<i64>)>,
) -> bool {
    let low = |held: &Option<UsageWindow>, w: &Option<(Option<f64>, Option<i64>)>| match (held, w) {
        (Some(h), Some((Some(pct), resets))) => {
            h.pct.is_some_and(|hp| *pct < hp) && same_window(h.resets_at, *resets)
        }
        _ => false,
    };
    low(&s.five_hour, five_hour) || low(&s.weekly, weekly)
}

/// The window to keep when `new` arrives over `old`.
pub fn merge_window(old: Option<&UsageWindow>, new: UsageWindow) -> UsageWindow {
    let Some(o) = old else { return new };
    if new.at < o.at {
        return o.clone();
    }
    // A reading with a reset time but no number (a chat's rate-limit line, or a control read that
    // came back without figures) must not wipe a number we hold for the same window, and must not
    // make it look newer than it is: the number keeps the time it was read. A different reset time
    // is a new window, so it replaces.
    if new.pct.is_none() && o.pct.is_some() && same_window(o.resets_at, new.resets_at) {
        return o.clone();
    }
    // A chat's estimate does not replace a control number that is still fresh. After that it may
    // (the number then shows as an estimate), and a new window always does.
    if new.source == UsageSource::RateLimitEvent
        && o.source == UsageSource::Control
        && o.pct.is_some()
        && new.at - o.at < CONTROL_HOLD_MS
        && same_window(o.resets_at, new.resets_at)
    {
        return o.clone();
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
    // The control answer lists every per-model window the plan has, so it replaces the list; a
    // window it gives no number for keeps the number held for the same model.
    let held = std::mem::take(&mut s.per_model);
    s.per_model = per_model
        .into_iter()
        .map(|(model, pct, resets_at)| {
            let prev = held.iter().find(|m| m.model == model).map(|m| UsageWindow {
                pct: m.pct,
                resets_at: m.resets_at,
                at: m.at,
                source: m.source,
            });
            let kept = merge_window(
                prev.as_ref(),
                UsageWindow {
                    pct,
                    resets_at,
                    at,
                    source: UsageSource::Control,
                },
            );
            ModelUsage {
                model,
                pct: kept.pct,
                resets_at: kept.resets_at,
                at: kept.at,
                source: kept.source,
            }
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
        let newer = w(Some(20.0), Some(1_000_000), 200, UsageSource::Control);
        assert_eq!(merge_window(Some(&old), newer.clone()).pct, Some(20.0));
        let older = w(Some(5.0), Some(1_000_000), 50, UsageSource::RateLimitEvent);
        assert_eq!(merge_window(Some(&old), older).pct, Some(10.0));
        assert_eq!(merge_window(None, newer.clone()), newer);
    }

    /// A control number below the held one for the same window is suspect; a higher one, a new
    /// window, a missing number either side, and nothing held are all taken at once.
    #[test]
    fn only_a_lower_number_for_the_same_window_needs_confirmation() {
        let mut s = StoredUsage::default();
        apply_control(
            &mut s,
            None,
            Some((Some(40.0), Some(1_000_000))),
            Some((Some(60.0), Some(9_000_000))),
            vec![],
            100,
        );
        assert!(below_held(&s, &Some((Some(30.0), Some(1_000_400))), &None));
        assert!(below_held(&s, &None, &Some((Some(10.0), Some(9_000_300)))));
        assert!(
            !below_held(&s, &Some((Some(41.0), Some(1_000_400))), &None),
            "higher is fine"
        );
        assert!(
            !below_held(&s, &Some((Some(30.0), Some(1_000_000 + 3_600_000))), &None),
            "a new window may legitimately sit lower"
        );
        assert!(
            !below_held(&s, &Some((None, Some(1_000_000))), &None),
            "no number in the read, nothing to compare"
        );
        let empty = StoredUsage::default();
        assert!(!below_held(
            &empty,
            &Some((Some(0.0), Some(1_000_000))),
            &None
        ));
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
        assert_eq!(kept.at, 100, "the number keeps the time it was read");
        // A reset time an hour later is a new window: the old number no longer applies.
        let fresh = w(
            None,
            Some(1_000_000 + 3_600_000),
            200,
            UsageSource::RateLimitEvent,
        );
        assert_eq!(merge_window(Some(&old), fresh).pct, None);
    }

    /// A chat's estimate does not replace a control number under 15 minutes old for the same
    /// window, and does after; a new window and a control read always replace.
    #[test]
    fn a_chats_estimate_waits_out_a_fresh_control_number() {
        let old = w(Some(10.0), Some(1_000_000), 100, UsageSource::Control);
        let est = |pct, resets, at| w(Some(pct), Some(resets), at, UsageSource::RateLimitEvent);
        let held = merge_window(Some(&old), est(30.0, 1_000_400, 100 + CONTROL_HOLD_MS - 1));
        assert_eq!(
            (held.pct, held.source, held.at),
            (Some(10.0), UsageSource::Control, 100)
        );
        let after = merge_window(Some(&old), est(30.0, 1_000_400, 100 + CONTROL_HOLD_MS));
        assert_eq!(
            (after.pct, after.source),
            (Some(30.0), UsageSource::RateLimitEvent)
        );
        let fresh_window = merge_window(Some(&old), est(2.0, 1_000_000 + 3_600_000, 200));
        assert_eq!(fresh_window.pct, Some(2.0), "a new window replaces at once");
        let control = w(Some(12.0), Some(1_000_000), 300, UsageSource::Control);
        assert_eq!(merge_window(Some(&old), control).pct, Some(12.0));
        // The same through the store: the pill and the summary both read these numbers.
        let mut s = StoredUsage::default();
        apply_control(
            &mut s,
            None,
            Some((Some(10.0), Some(1_000_000))),
            None,
            vec![],
            100,
        );
        let line = || {
            [Passive {
                kind: "five_hour".into(),
                pct: Some(30.0),
                resets_at: Some(1_000_000),
            }]
        };
        assert!(
            !apply_passive(&mut s, &line(), 200),
            "nothing changed, so nothing is signalled"
        );
        assert_eq!(reading(&s, 300).unwrap().five_hour.unwrap().pct, Some(10.0));
        let at = 100 + CONTROL_HOLD_MS;
        assert!(apply_passive(&mut s, &line(), at));
        assert_eq!(reading(&s, at).unwrap().five_hour.unwrap().pct, Some(30.0));
    }

    #[test]
    fn a_control_read_without_numbers_keeps_the_old_ones_and_their_time() {
        let mut s = StoredUsage::default();
        apply_control(
            &mut s,
            None,
            Some((Some(10.0), Some(1_000_000))),
            None,
            vec![("Fable".into(), Some(3.0), Some(20))],
            100,
        );
        apply_control(
            &mut s,
            None,
            Some((None, Some(1_000_100))),
            None,
            vec![("Fable".into(), None, Some(20))],
            500,
        );
        let five = s.five_hour.clone().unwrap();
        assert_eq!((five.pct, five.at), (Some(10.0), 100));
        assert_eq!((s.per_model[0].pct, s.per_model[0].at), (Some(3.0), 100));
        // A window that is new (a later reset) with no number stays unknown.
        apply_control(
            &mut s,
            None,
            Some((None, Some(9_000_000))),
            None,
            vec![],
            600,
        );
        assert_eq!(s.five_hour.unwrap().pct, None);
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
