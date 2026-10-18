//! Auto-AFK's decision function, all-chats mode. Pure: its whole memory is
//! [`MonitorState`], and every input arrives as a [`Signal`]. The service applies what it returns.
//!
//! Windows sources: session lock and unlock from WTS notifications, sleep and
//! resume from power broadcasts, idle seconds from the system's last-input time, and window focus
//! and in-window input from the UI's presence reports.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Manual,
    ScreenLock,
    Sleep,
    Idle,
    Unanswered,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::ScreenLock => "screenLock",
            Self::Sleep => "sleep",
            Self::Idle => "idle",
            Self::Unanswered => "unanswered",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "manual" => Self::Manual,
            "screenLock" => Self::ScreenLock,
            "sleep" => Self::Sleep,
            "idle" => Self::Idle,
            "unanswered" => Self::Unanswered,
            _ => return None,
        })
    }

    pub fn automatic(self) -> bool {
        self != Self::Manual
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Locked {
        at: i64,
    },
    Unlocked,
    /// Posted `lockGraceSeconds` after a lock.
    LockGrace {
        at: i64,
    },
    /// Carries when the OS sent it, so one handled after the following wake is dropped.
    WillSleep {
        sent_at: i64,
    },
    DidWake {
        at: i64,
    },
    Focused(bool),
    /// Input in Helm's own window (the UI's presence report).
    WindowInput {
        at: i64,
    },
    /// Every 30 s. `idle_secs`: seconds since the last system-wide input, when readable.
    /// `oldest_ask`: when the longest-waiting question or approval began, if any.
    Tick {
        now: i64,
        idle_secs: Option<u64>,
        oldest_ask: Option<i64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    On(Source),
    /// End the open span only if this automatic source opened it.
    OffIf(Source),
}

/// What the decision needs from the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cfg {
    pub enabled: bool,
    pub screen_lock: bool,
    pub grace_ms: i64,
    pub sleep: bool,
    pub unanswered_ms: Option<i64>,
    pub idle_ms: Option<i64>,
    /// `auto.end = manual`: nothing ends a span automatically.
    pub end_manual: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonitorState {
    pub lock_at: Option<i64>,
    pub sleep_at: Option<i64>,
    pub wake_at: Option<i64>,
    pub last_span_end: Option<i64>,
    pub focused: bool,
    pub last_tick: Option<i64>,
    pub window_input_at: Option<i64>,
    /// The open span's source (kept in step by the service).
    pub open: Option<Source>,
}

impl MonitorState {
    /// The service opened a span.
    pub fn opened(&mut self, s: Source) {
        self.open = Some(s);
    }

    /// The service closed the span at `at`.
    pub fn closed(&mut self, at: i64) {
        self.open = None;
        self.last_span_end = Some(at);
    }

    fn lock_past_grace(&self, cfg: &Cfg, now: i64) -> bool {
        cfg.screen_lock && self.lock_at.is_some_and(|l| now - l >= cfg.grace_ms)
    }
}

pub fn decide(st: &mut MonitorState, cfg: &Cfg, sig: Signal) -> Option<Action> {
    // Memory first, whatever the settings.
    match sig {
        Signal::Locked { at } => st.lock_at = Some(at),
        Signal::Unlocked => st.lock_at = None,
        Signal::WillSleep { sent_at } => {
            if st.wake_at.is_some_and(|w| w > sent_at) {
                return None; // stale: handled after the following wake
            }
            st.sleep_at = Some(sent_at);
        }
        Signal::DidWake { at } => st.wake_at = Some(at),
        Signal::Focused(f) => st.focused = f,
        Signal::WindowInput { at } => st.window_input_at = Some(at),
        Signal::LockGrace { .. } | Signal::Tick { .. } => {}
    }
    if !cfg.enabled {
        if let Signal::Tick { now, .. } = sig {
            st.last_tick = Some(now);
        }
        return None;
    }
    let open = st.open;
    match sig {
        Signal::Locked { .. } => (open.is_none() && cfg.screen_lock && cfg.grace_ms == 0)
            .then_some(Action::On(Source::ScreenLock)),
        Signal::LockGrace { at } => (open.is_none() && st.lock_past_grace(cfg, at))
            .then_some(Action::On(Source::ScreenLock)),
        Signal::Unlocked => match open {
            Some(s @ (Source::ScreenLock | Source::Sleep)) if !cfg.end_manual => {
                Some(Action::OffIf(s))
            }
            _ => None,
        },
        Signal::WillSleep { .. } => {
            (open.is_none() && cfg.sleep).then_some(Action::On(Source::Sleep))
        }
        Signal::DidWake { .. } | Signal::Focused(_) | Signal::WindowInput { .. } => None,
        Signal::Tick {
            now,
            idle_secs,
            oldest_ask,
        } => {
            let prev = st.last_tick.replace(now);
            let last_input = idle_secs.map(|s| now - (s as i64) * 1000);
            // Input on the lock screen (typing the password) is not the user coming back.
            let locked = st.lock_at.is_some();
            let input_since =
                |t: Option<i64>| !locked && last_input.is_some_and(|i| t.is_some_and(|t| i > t));
            match open {
                None => {
                    if st.lock_past_grace(cfg, now) {
                        return Some(Action::On(Source::ScreenLock));
                    }
                    if let (Some(ask), Some(wait)) = (oldest_ask, cfg.unanswered_ms) {
                        let from = ask.max(st.last_span_end.unwrap_or(i64::MIN));
                        if now - from >= wait {
                            return Some(Action::On(Source::Unanswered));
                        }
                    }
                    if let (Some(idle), Some(wait)) = (idle_secs, cfg.idle_ms) {
                        if !locked && (idle as i64) * 1000 >= wait {
                            return Some(Action::On(Source::Idle));
                        }
                    }
                    None
                }
                Some(_) if cfg.end_manual => None,
                Some(Source::Manual) => None,
                Some(Source::Idle) => input_since(prev).then_some(Action::OffIf(Source::Idle)),
                Some(Source::Unanswered) => {
                    let window = st.focused
                        && st
                            .window_input_at
                            .is_some_and(|w| prev.is_some_and(|p| w > p));
                    (window || (st.focused && input_since(prev)))
                        .then_some(Action::OffIf(Source::Unanswered))
                }
                Some(s @ (Source::Sleep | Source::ScreenLock)) => {
                    // No lock recorded, and input since waking (or since the span began, when a
                    // restart left it open with the lock state unknown).
                    let since = st.wake_at.or(prev);
                    input_since(since).then_some(Action::OffIf(s))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Cfg {
        Cfg {
            enabled: true,
            screen_lock: true,
            grace_ms: 0,
            sleep: true,
            unanswered_ms: Some(30 * 60_000),
            idle_ms: Some(30 * 60_000),
            end_manual: false,
        }
    }

    fn tick(now: i64, idle: u64, ask: Option<i64>) -> Signal {
        Signal::Tick {
            now,
            idle_secs: Some(idle),
            oldest_ask: ask,
        }
    }

    #[test]
    fn disabled_only_remembers() {
        let mut st = MonitorState::default();
        let c = Cfg {
            enabled: false,
            ..cfg()
        };
        assert_eq!(decide(&mut st, &c, Signal::Locked { at: 5 }), None);
        assert_eq!(st.lock_at, Some(5));
        assert_eq!(decide(&mut st, &c, tick(10, 99_999, None)), None);
    }

    #[test]
    fn lock_with_and_without_grace() {
        let mut st = MonitorState::default();
        assert_eq!(
            decide(&mut st, &cfg(), Signal::Locked { at: 0 }),
            Some(Action::On(Source::ScreenLock))
        );
        let mut st = MonitorState::default();
        let g = Cfg {
            grace_ms: 60_000,
            ..cfg()
        };
        assert_eq!(decide(&mut st, &g, Signal::Locked { at: 0 }), None);
        assert_eq!(decide(&mut st, &g, Signal::LockGrace { at: 59_999 }), None);
        assert_eq!(
            decide(&mut st, &g, Signal::LockGrace { at: 60_000 }),
            Some(Action::On(Source::ScreenLock))
        );
        // A lock released inside the grace never fires.
        let mut st = MonitorState::default();
        decide(&mut st, &g, Signal::Locked { at: 0 });
        decide(&mut st, &g, Signal::Unlocked);
        assert_eq!(decide(&mut st, &g, Signal::LockGrace { at: 60_000 }), None);
        assert_eq!(decide(&mut st, &g, tick(90_000, 0, None)), None);
    }

    #[test]
    fn unlock_ends_only_a_lock_or_sleep_span() {
        for (src, ends) in [
            (Source::ScreenLock, true),
            (Source::Sleep, true),
            (Source::Manual, false),
            (Source::Idle, false),
        ] {
            let mut st = MonitorState::default();
            st.opened(src);
            let got = decide(&mut st, &cfg(), Signal::Unlocked);
            assert_eq!(got.is_some(), ends, "{src:?}");
        }
        let mut st = MonitorState::default();
        st.opened(Source::ScreenLock);
        let m = Cfg {
            end_manual: true,
            ..cfg()
        };
        assert_eq!(decide(&mut st, &m, Signal::Unlocked), None);
    }

    #[test]
    fn stale_sleep_is_dropped() {
        let mut st = MonitorState::default();
        decide(&mut st, &cfg(), Signal::DidWake { at: 100 });
        assert_eq!(
            decide(&mut st, &cfg(), Signal::WillSleep { sent_at: 50 }),
            None
        );
        assert_eq!(
            decide(&mut st, &cfg(), Signal::WillSleep { sent_at: 150 }),
            Some(Action::On(Source::Sleep))
        );
        let no = Cfg {
            sleep: false,
            ..cfg()
        };
        let mut st = MonitorState::default();
        assert_eq!(decide(&mut st, &no, Signal::WillSleep { sent_at: 1 }), None);
    }

    #[test]
    fn tick_order_is_lock_then_no_reply_then_idle() {
        let five = 5 * 60_000;
        let c = Cfg {
            unanswered_ms: Some(five),
            idle_ms: Some(five),
            ..cfg()
        };
        // Equal waits: the no-reply trigger wins over idle in all-chats mode.
        let mut st = MonitorState::default();
        assert_eq!(
            decide(&mut st, &c, tick(five, 300, Some(0))),
            Some(Action::On(Source::Unanswered))
        );
        let mut st = MonitorState::default();
        assert_eq!(
            decide(&mut st, &c, tick(five, 300, None)),
            Some(Action::On(Source::Idle))
        );
        let mut st = MonitorState::default();
        assert_eq!(decide(&mut st, &c, tick(five, 299, Some(1))), None);
    }

    #[test]
    fn no_reply_waits_from_the_last_span_end() {
        let mut st = MonitorState::default();
        st.closed(10 * 60_000);
        let c = cfg();
        assert_eq!(decide(&mut st, &c, tick(30 * 60_000, 0, Some(0))), None);
        assert_eq!(
            decide(&mut st, &c, tick(40 * 60_000, 0, Some(0))),
            Some(Action::On(Source::Unanswered))
        );
    }

    #[test]
    fn idle_span_ends_on_input_since_the_previous_tick() {
        let mut st = MonitorState::default();
        let c = cfg();
        st.opened(Source::Idle);
        assert_eq!(decide(&mut st, &c, tick(1_000_000, 2000, None)), None);
        // Still idle 30 s later.
        assert_eq!(decide(&mut st, &c, tick(1_030_000, 2030, None)), None);
        // Input 10 s ago, after the previous tick.
        assert_eq!(
            decide(&mut st, &c, tick(1_060_000, 10, None)),
            Some(Action::OffIf(Source::Idle))
        );
    }

    #[test]
    fn input_while_locked_is_not_a_return() {
        let mut st = MonitorState::default();
        let c = cfg();
        st.opened(Source::Idle);
        decide(&mut st, &c, tick(1_000_000, 2000, None));
        decide(&mut st, &c, Signal::Locked { at: 1_010_000 });
        assert_eq!(decide(&mut st, &c, tick(1_030_000, 1, None)), None);
    }

    #[test]
    fn manual_span_never_ends_automatically() {
        let mut st = MonitorState::default();
        let c = cfg();
        st.opened(Source::Manual);
        decide(&mut st, &c, tick(1_000_000, 2000, None));
        assert_eq!(decide(&mut st, &c, tick(1_030_000, 0, None)), None);
        assert_eq!(decide(&mut st, &c, Signal::Unlocked), None);
    }

    #[test]
    fn sleep_span_ends_on_input_after_wake() {
        let mut st = MonitorState::default();
        let c = cfg();
        st.opened(Source::Sleep);
        decide(&mut st, &c, Signal::DidWake { at: 500_000 });
        // Last input before the wake: no return.
        assert_eq!(decide(&mut st, &c, tick(600_000, 200, None)), None);
        assert_eq!(
            decide(&mut st, &c, tick(630_000, 5, None)),
            Some(Action::OffIf(Source::Sleep))
        );
    }

    #[test]
    fn unanswered_ends_on_focused_window_input() {
        let mut st = MonitorState::default();
        let c = cfg();
        st.opened(Source::Unanswered);
        decide(&mut st, &c, tick(100_000, 0, None));
        decide(&mut st, &c, Signal::WindowInput { at: 110_000 });
        // Not focused: no end.
        assert_eq!(decide(&mut st, &c, tick(130_000, 999, None)), None);
        decide(&mut st, &c, Signal::Focused(true));
        decide(&mut st, &c, Signal::WindowInput { at: 140_000 });
        assert_eq!(
            decide(&mut st, &c, tick(160_000, 999, None)),
            Some(Action::OffIf(Source::Unanswered))
        );
    }
}
