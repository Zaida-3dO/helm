//! When a schedule next fires.
//!
//! Three kinds: once, every N seconds, and weekly at a local time on chosen days. The maths is
//! pure: it takes the clock reading and a [`Zone`] that answers "what is the UTC offset at this
//! instant", so tests drive daylight-saving changes without touching the machine's timezone.
//!
//! **Coalescing.** The next run is always the first slot after *now*, so any number of missed
//! slots (the app was quit, the machine slept, the chat was busy) collapse into the one run that
//! claims them.

use crate::types::Timing;

pub const MIN_INTERVAL_SECS: u64 = 60;
pub const MAX_INTERVAL_SECS: u64 = 31_536_000;

const DAY_MS: i64 = 86_400_000;

/// The UTC offset in force at an instant.
pub trait Zone: Send + Sync {
    /// Seconds east of UTC at `utc_ms`.
    fn offset_secs(&self, utc_ms: i64) -> i32;
}

/// The machine's local timezone.
pub struct LocalZone;

impl Zone for LocalZone {
    fn offset_secs(&self, utc_ms: i64) -> i32 {
        use chrono::{Local, Offset, TimeZone};
        match chrono::DateTime::from_timestamp_millis(utc_ms) {
            Some(utc) => Local
                .offset_from_utc_datetime(&utc.naive_utc())
                .fix()
                .local_minus_utc(),
            None => 0,
        }
    }
}

/// A constant offset (tests and fallbacks).
pub struct FixedZone(pub i32);

impl Zone for FixedZone {
    fn offset_secs(&self, _utc_ms: i64) -> i32 {
        self.0
    }
}

/// The validation message for a timing that can never run, or `None` when it is well formed.
pub fn check(t: &Timing) -> Option<String> {
    match t {
        Timing::Once { at } => {
            (*at < 0 || *at > 253_402_300_799_000).then(|| "Pick a valid date and time.".into())
        }
        Timing::Interval { seconds, .. } => (!(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS)
            .contains(seconds))
        .then(|| "Repeat between every 1 minute and every 365 days.".into()),
        Timing::Weekly { hour, minute, days } => {
            if *hour > 23 || *minute > 59 {
                return Some("Pick a time of day between 00:00 and 23:59.".into());
            }
            if days.is_empty() || days.len() > 7 || days.iter().any(|d| *d > 6) {
                return Some("Pick at least one weekday.".into());
            }
            let mut seen = [false; 7];
            for d in days {
                if std::mem::replace(&mut seen[*d as usize], true) {
                    return Some("Each weekday can be picked once.".into());
                }
            }
            None
        }
    }
}

/// The first run of a new or re-timed schedule, which must lie after `now`. `None` when it
/// would not (a one-time date in the past, a start time already gone).
pub fn first_run(t: &Timing, now: i64, zone: &dyn Zone) -> Option<i64> {
    match t {
        Timing::Once { at } => (*at > now).then_some(*at),
        Timing::Interval { seconds, start_at } => {
            let at = start_at.unwrap_or_else(|| now + (*seconds as i64) * 1000);
            (at > now).then_some(at)
        }
        Timing::Weekly { hour, minute, days } => next_weekly(zone, now, *hour, *minute, days),
    }
}

/// The run after an occurrence that was due at `due` and is being claimed at `now`. `None` for a
/// one-time schedule, which completes. Skipped slots are not replayed.
pub fn next_after(t: &Timing, due: i64, now: i64, zone: &dyn Zone) -> Option<i64> {
    match t {
        Timing::Once { .. } => None,
        Timing::Interval { seconds, .. } => {
            let period = (*seconds as i64) * 1000;
            let behind = (now - due).max(0);
            Some(due + (behind / period + 1) * period)
        }
        Timing::Weekly { hour, minute, days } => next_weekly(zone, now, *hour, *minute, days),
    }
}

/// The next local `hour:minute` on one of `days` (Monday = 0) strictly after `after`. A time that
/// does not exist (spring forward) is skipped; one that happens twice (fall back) uses the
/// earlier instant.
pub fn next_weekly(zone: &dyn Zone, after: i64, hour: u8, minute: u8, days: &[u8]) -> Option<i64> {
    if days.is_empty() {
        return None;
    }
    let local_now = after + zone.offset_secs(after) as i64 * 1000;
    let today = local_now.div_euclid(DAY_MS);
    for k in 0..=14 {
        let date = today + k;
        // 1970-01-01 was a Thursday, which is 3 with Monday = 0.
        let weekday = (date + 3).rem_euclid(7) as u8;
        if !days.contains(&weekday) {
            continue;
        }
        let wall = date * DAY_MS + (hour as i64 * 60 + minute as i64) * 60_000;
        if let Some(u) = wall_to_utc(zone, wall) {
            if u > after {
                return Some(u);
            }
        }
    }
    None
}

/// The UTC instant whose local wall time is `wall` (both in ms); the earlier one when there are
/// two, `None` when there is none.
fn wall_to_utc(zone: &dyn Zone, wall: i64) -> Option<i64> {
    let before = zone.offset_secs(wall - DAY_MS) as i64 * 1000;
    let after = zone.offset_secs(wall + DAY_MS) as i64 * 1000;
    [wall - before, wall - after]
        .into_iter()
        .filter(|u| u + zone.offset_secs(*u) as i64 * 1000 == wall)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    /// A zone that moves from one offset to another at an instant (a DST change).
    struct Switch {
        at: i64,
        before: i32,
        after: i32,
    }
    impl Zone for Switch {
        fn offset_secs(&self, utc_ms: i64) -> i32 {
            if utc_ms < self.at {
                self.before
            } else {
                self.after
            }
        }
    }

    // A Monday 00:00 UTC (2026-10-19).
    const MON: i64 = 20_745 * DAY_MS;

    #[test]
    fn the_test_anchor_is_a_monday() {
        assert_eq!((MON.div_euclid(DAY_MS) + 3).rem_euclid(7), 0);
    }

    #[test]
    fn interval_missed_slots_collapse_into_one_next_slot_on_the_grid() {
        let t = Timing::Interval {
            seconds: 1800,
            start_at: None,
        };
        let due = MON + 10 * HOUR;
        // Five slots missed (2h 40m late): the next is the first grid slot after now.
        let now = due + 2 * HOUR + 40 * 60_000;
        assert_eq!(
            next_after(&t, due, now, &FixedZone(0)),
            Some(due + 3 * HOUR)
        );
        // Claimed on time: just one period on.
        assert_eq!(
            next_after(&t, due, due + 5, &FixedZone(0)),
            Some(due + 1_800_000)
        );
        // Claimed exactly on a later slot: strictly after now.
        assert_eq!(
            next_after(&t, due, due + 1_800_000, &FixedZone(0)),
            Some(due + 3_600_000)
        );
    }

    #[test]
    fn once_completes_and_must_be_in_the_future() {
        let t = Timing::Once { at: MON + HOUR };
        assert_eq!(first_run(&t, MON, &FixedZone(0)), Some(MON + HOUR));
        assert_eq!(first_run(&t, MON + 2 * HOUR, &FixedZone(0)), None);
        assert_eq!(next_after(&t, MON + HOUR, MON + HOUR, &FixedZone(0)), None);
    }

    #[test]
    fn interval_first_run_is_start_or_one_period_out() {
        let z = FixedZone(0);
        let t = Timing::Interval {
            seconds: 60,
            start_at: None,
        };
        assert_eq!(first_run(&t, MON, &z), Some(MON + 60_000));
        let s = Timing::Interval {
            seconds: 60,
            start_at: Some(MON + HOUR),
        };
        assert_eq!(first_run(&s, MON, &z), Some(MON + HOUR));
        let past = Timing::Interval {
            seconds: 60,
            start_at: Some(MON - 1),
        };
        assert_eq!(first_run(&past, MON, &z), None);
    }

    #[test]
    fn weekly_picks_the_next_matching_local_time() {
        // Monday 12:00 in +2 is 10:00 UTC. From 09:00 UTC (11:00 local) it is today; from the
        // run itself it is Wednesday.
        let z = FixedZone(2 * 3600);
        let days = [0u8, 2];
        let today_noon = MON + 10 * HOUR;
        assert_eq!(
            next_weekly(&z, MON + 9 * HOUR, 12, 0, &days),
            Some(today_noon)
        );
        assert_eq!(
            next_weekly(&z, today_noon, 12, 0, &days),
            Some(today_noon + 2 * DAY_MS)
        );
    }

    #[test]
    fn weekly_uses_local_days_not_utc_days() {
        // 23:30 UTC on Sunday is already Monday 01:30 in +2, so "Monday 01:00" is next week.
        let z = FixedZone(2 * 3600);
        let sun_2330 = MON - HOUR / 2;
        let next = next_weekly(&z, sun_2330, 1, 0, &[0]).unwrap();
        // Next Monday 01:00 local = Sunday 23:00 UTC a week on.
        assert_eq!(next, MON + 7 * DAY_MS - HOUR);
    }

    #[test]
    fn a_spring_forward_time_that_does_not_exist_is_skipped() {
        // The offset goes 0 to +1h at Mon 01:00 UTC, so local 01:30 on Monday never happens
        // (local time jumps from 01:00 to 02:00). Tuesday 01:30 local is Tuesday 00:30 UTC (offset +1).
        let z = Switch {
            at: MON + HOUR,
            before: 0,
            after: 3600,
        };
        let next = next_weekly(&z, MON - HOUR, 1, 30, &[0, 1]).unwrap();
        assert_eq!(next, MON + DAY_MS + 30 * 60_000);
    }

    #[test]
    fn a_fall_back_time_that_happens_twice_uses_the_earlier_instant() {
        // The offset goes +1h to +0 at Mon 01:00 UTC: local 01:30 happens at 00:30 UTC (offset
        // +1) and again at 01:30 UTC (offset +0). The earlier one wins.
        let z = Switch {
            at: MON + HOUR,
            before: 3600,
            after: 0,
        };
        let next = next_weekly(&z, MON - HOUR, 1, 30, &[0]).unwrap();
        assert_eq!(next, MON + 30 * 60_000);
    }

    #[test]
    fn check_rejects_what_can_never_run() {
        assert!(check(&Timing::Interval {
            seconds: 59,
            start_at: None
        })
        .is_some());
        assert!(check(&Timing::Interval {
            seconds: 60,
            start_at: None
        })
        .is_none());
        assert!(check(&Timing::Interval {
            seconds: MAX_INTERVAL_SECS + 1,
            start_at: None
        })
        .is_some());
        assert!(check(&Timing::Weekly {
            hour: 24,
            minute: 0,
            days: vec![0]
        })
        .is_some());
        assert!(check(&Timing::Weekly {
            hour: 1,
            minute: 0,
            days: vec![]
        })
        .is_some());
        assert!(check(&Timing::Weekly {
            hour: 1,
            minute: 0,
            days: vec![1, 1]
        })
        .is_some());
        assert!(check(&Timing::Weekly {
            hour: 1,
            minute: 0,
            days: vec![7]
        })
        .is_some());
        assert!(check(&Timing::Weekly {
            hour: 23,
            minute: 59,
            days: vec![0, 6]
        })
        .is_none());
    }
}
