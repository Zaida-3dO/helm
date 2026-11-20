//! A guard that restarts the hub when its async runtime stops making progress.
//!
//! The container's healthcheck only marks the container unhealthy; Docker does not
//! restart an unhealthy container by itself. This module supplies the restart: an
//! async task stamps the current time into a shared counter every [`BEAT_EVERY`],
//! and a plain thread (which keeps running even when the runtime is wedged) checks
//! that stamp every [`CHECK_EVERY`]. When the stamp is older than the limit, the
//! process logs one line and exits, and the container's restart policy brings it
//! back a moment later.
//!
//! The limit comes from `HUB_WATCHDOG_SECS` (default 180, `0` turns the watchdog
//! off). The decision itself is [`overdue`], a pure function of the last beat,
//! now and the limit, so it can be tested without wedging anything.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How often the async task stamps the beat counter.
const BEAT_EVERY: Duration = Duration::from_secs(10);

/// How often the watchdog thread inspects the beat counter.
const CHECK_EVERY: Duration = Duration::from_secs(5);

/// The limit used when `HUB_WATCHDOG_SECS` is unset or unusable.
const DEFAULT_LIMIT_SECS: u64 = 180;

/// The last beat: Unix milliseconds of the most recent stamp, 0 before the first.
static LAST_BEAT_MS: AtomicU64 = AtomicU64::new(0);

/// Start the watchdog: the beating task and the checking thread. A `limit` of
/// zero means off and nothing is started. The caller must be inside a tokio
/// runtime, which is what the beating task runs on.
pub fn start(limit: Duration) {
    if limit.is_zero() {
        return;
    }
    let limit_ms = limit.as_millis() as u64;
    let now = now_ms();
    LAST_BEAT_MS.store(now, Ordering::Relaxed);

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(BEAT_EVERY).await;
            LAST_BEAT_MS.store(now_ms(), Ordering::Relaxed);
        }
    });

    // A plain thread on purpose: it must stay alive when the async runtime is
    // the thing that has stopped.
    std::thread::spawn(move || loop {
        std::thread::sleep(CHECK_EVERY);
        let last = LAST_BEAT_MS.load(Ordering::Relaxed);
        let now = now_ms();
        if overdue(last, now, limit_ms) {
            eprintln!(
                "hub watchdog: the runtime has not answered for {} s, exiting so the container restarts",
                limit.as_secs()
            );
            // Exit non-zero so the restart policy treats it as a failure.
            std::process::exit(70);
        }
    });
}

/// Whether the beat has gone stale: `now` is at least `limit_ms` past
/// `last_beat_ms`. A limit of zero is off, never overdue, and a beat from the
/// future (a clock jump backwards) reads as fresh rather than overdue.
fn overdue(last_beat_ms: u64, now_ms: u64, limit_ms: u64) -> bool {
    limit_ms > 0 && now_ms.saturating_sub(last_beat_ms) >= limit_ms
}

/// The watchdog limit from the raw `HUB_WATCHDOG_SECS` value: seconds as given,
/// the default when unset or not a number, and off for `0`.
pub fn limit_from_env(value: Option<&str>) -> Duration {
    match value.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(secs) => Duration::from_secs(secs),
        None => Duration::from_secs(DEFAULT_LIMIT_SECS),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_table() {
        // (last beat ms, now ms, limit ms, overdue?)
        let cases = [
            // fresh beats are never overdue
            (1_000, 1_000, 180_000, false),
            (1_000, 100_000, 180_000, false),
            // just under, exactly at, and past the limit
            (0, 179_999, 180_000, false),
            (0, 180_000, 180_000, true),
            (0, 500_000, 180_000, true),
            // a limit of zero is off
            (0, 10_000_000, 0, false),
            // a clock jump backwards leaves the beat in the future: not overdue
            (900_000, 100_000, 180_000, false),
        ];
        for (last, now, limit, want) in cases {
            assert_eq!(
                overdue(last, now, limit),
                want,
                "overdue({last}, {now}, {limit})"
            );
        }
    }

    #[test]
    fn limit_table() {
        // (env value, expected seconds)
        let cases = [
            (None, 180),
            (Some("180"), 180),
            (Some("0"), 0),
            (Some("60"), 60),
            (Some(" 45 "), 45),
            (Some(""), 180),
            (Some("five"), 180),
            (Some("-1"), 180),
        ];
        for (value, want) in cases {
            assert_eq!(
                limit_from_env(value),
                Duration::from_secs(want),
                "limit for {value:?}"
            );
        }
    }
}
