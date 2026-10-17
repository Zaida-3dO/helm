//! The revision clock for UI events: the UI drops events whose revision is not newer.
//!
//! One process-wide counter, seeded at startup with the larger of the database's global revision
//! and the current Unix time in milliseconds, so it keeps rising across restarts without being
//! written back. Rules that keep reads and events consistent:
//!
//! - a writer **commits first, then** takes [`Clock::next`] for the event it emits;
//! - a reader takes [`Clock::current`] **before** it reads, and returns that as the revision.
//!
//! So an event for a change the read did not see is always newer than the read, and an event the
//! UI drops as stale described a change the read already contained.

use std::sync::atomic::{AtomicI64, Ordering};

pub struct Clock(AtomicI64);

impl Clock {
    pub fn new(seed: i64) -> Self {
        Self(AtomicI64::new(seed.max(helm_core::now_ms())))
    }

    pub fn next(&self) -> i64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn current(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_is_always_newer_than_current() {
        let c = Clock::new(0);
        let r = c.current();
        assert!(r >= helm_core::now_ms() - 1000);
        assert!(c.next() > r);
        assert!(c.next() > c.current() - 1);
        let big = Clock::new(i64::from(u32::MAX) * 1000);
        assert_eq!(big.current(), i64::from(u32::MAX) * 1000);
    }
}
