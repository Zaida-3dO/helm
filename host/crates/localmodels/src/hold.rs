//! The graphics-card hold: a note, set by a chat, that something is using the card and sending to a
//! chat on a local model would reload the model server and spoil it (a benchmark run, say).
//!
//! The person is warned, not stopped: while a hold is set the composer of every local-model chat
//! says why and asks once before the first send. Another chat is stopped: `app_send_message` to a
//! chat on a local model and `app_create_chat` on a local model are refused with the reason and
//! the time the hold ends. Schedules and wait deliveries are not held. The lock in
//! [`crate::lock`] is the one that refuses loads. A hold ends by itself: two hours by default, eight at most, or when the same tool clears it. It
//! lives in memory, so a restart of Helm drops it.

use crate::service::LocalService;
use crate::types::LockView;
use serde::Deserialize;
use std::sync::Mutex;

/// How long a hold lasts when the caller names no time, and the longest one it may ask for.
pub const DEFAULT_HOURS: f64 = 2.0;
pub const MAX_HOURS: f64 = 8.0;

/// A reason is one line of at most this many characters.
const MAX_REASON: usize = 160;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub reason: String,
    /// When it ends by itself, in ms since the epoch.
    pub until: i64,
}

/// The arguments of `local_hold_set`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HoldArgs {
    pub reason: String,
    #[serde(default)]
    pub hours: Option<f64>,
}

/// The reason as one plain line, or why it cannot be used.
pub fn clean_reason(raw: &str) -> Result<String, String> {
    let one: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_REASON)
        .collect();
    let one = one.trim().trim_end_matches(['.', ':']).trim().to_owned();
    if one.is_empty() {
        Err("A hold needs a reason: one short line the composers show, such as \"Benchmark running\".".into())
    } else {
        Ok(one)
    }
}

/// When a hold set at `now` ends: the default when no time is named, never more than the longest.
pub fn expiry(hours: Option<f64>, now: i64) -> Result<i64, String> {
    let h = hours.unwrap_or(DEFAULT_HOURS);
    if !h.is_finite() || h <= 0.0 {
        return Err("hours must be a number above 0.".into());
    }
    Ok(now + (h.min(MAX_HOURS) * 3_600_000.0) as i64)
}

/// The one hold there can be.
#[derive(Default)]
pub struct HoldCell(Mutex<Option<Hold>>);

impl HoldCell {
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Hold>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, hold: Hold) {
        *self.slot() = Some(hold);
    }

    /// Whether there was a hold to clear.
    pub fn clear(&self) -> bool {
        self.slot().take().is_some()
    }

    /// The hold in force at `now`; one that has run out is dropped.
    pub fn current(&self, now: i64) -> Option<Hold> {
        let mut slot = self.slot();
        if slot.as_ref().is_some_and(|h| h.until <= now) {
            *slot = None;
        }
        slot.clone()
    }
}

impl LocalService {
    /// Set the hold, replacing any earlier one. Answers the lock view with the hold in it.
    pub fn set_hold(&self, reason: &str, hours: Option<f64>) -> Result<LockView, String> {
        let now = helm_core::now_ms();
        let hold = Hold {
            reason: clean_reason(reason)?,
            until: expiry(hours, now)?,
        };
        self.hold.set(hold);
        self.emit_lock();
        Ok(self.lock_view())
    }

    /// Clear the hold; clearing none is not an error.
    pub fn clear_hold(&self) -> LockView {
        if self.hold.clear() {
            self.emit_lock();
        }
        self.lock_view()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_one_plain_line_and_times_are_bounded() {
        let ok = |s: &str| clean_reason(s).unwrap();
        assert_eq!(ok("  Benchmark\n running. "), "Benchmark running");
        assert_eq!(ok("GPU benchmark:"), "GPU benchmark");
        assert_eq!(ok(&"x".repeat(500)).chars().count(), MAX_REASON);
        assert!(clean_reason("  \n ").is_err());
        assert!(clean_reason(".").is_err());

        let h = 3_600_000;
        assert_eq!(expiry(None, 0), Ok(2 * h));
        assert_eq!(expiry(Some(0.5), 0), Ok(h / 2));
        assert_eq!(expiry(Some(100.0), 0), Ok(8 * h), "capped at eight hours");
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(expiry(Some(bad), 0).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_hold_ends_when_it_runs_out_or_is_cleared() {
        let cell = HoldCell::default();
        assert_eq!(cell.current(0), None);
        assert!(!cell.clear());
        let hold = Hold {
            reason: "Benchmark running".into(),
            until: 1_000,
        };
        cell.set(hold.clone());
        assert_eq!(cell.current(999), Some(hold.clone()));
        assert_eq!(cell.current(1_000), None, "ends at its time");
        assert_eq!(cell.current(0), None, "and stays ended");
        cell.set(hold);
        assert!(cell.clear());
        assert_eq!(cell.current(0), None);
    }
}
