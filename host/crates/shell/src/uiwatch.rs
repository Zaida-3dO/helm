//! The UI content-process watchdog.
//!
//! The app probes the main window's page every [`PROBE_EVERY`] with a one-line script whose answer
//! carries [`PROBE_TOKEN`]. A page that answered before and then leaves a probe unanswered for
//! [`DEAD_AFTER`] while the window is shown is treated as a dead content process: the app writes a
//! diagnostic line ([`write_diagnostic`]) and reloads the page. Drafts survive because the composer
//! saves them on a debounce; nothing here touches the database.
//!
//! WebView2's own process-failed event is the fast path ([`CrashGuard`]): a renderer that exits is
//! reloaded at once, a browser process that exits gets a new window, and a window that keeps
//! failing is left alone until the user says to reload it. The probe above is the backstop for a
//! page that hangs without any process dying.
//!
//! Decisions:
//! - Probing, not page timers, so a hidden or occluded window whose timers are throttled is never
//!   mistaken for a dead one; a hidden or minimised window is not judged at all.
//! - A page that has not answered since it (re)loaded is never reloaded, so a slow first load or a
//!   page that crashes at once cannot cause a reload loop.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

pub const PROBE_EVERY: Duration = Duration::from_secs(10);
pub const DEAD_AFTER: Duration = Duration::from_secs(30);
/// The probe script evaluates to this string; any other answer (or none) is not a live page.
pub const PROBE_TOKEN: &str = "helm-ui-alive";
pub const PROBE_SCRIPT: &str = "'helm-ui-alive'";
/// The diagnostic file is cut back to its last half once it passes this size.
const DIAGNOSTIC_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Send one probe now.
    Probe,
    /// The page has not answered for this long: write the diagnostic and reload.
    Reload {
        silent_for: Duration,
    },
}

#[derive(Debug, Default)]
pub struct UiWatchdog {
    /// The page answered at least once since it last loaded.
    answered_since_load: bool,
    /// The oldest probe still waiting for an answer.
    outstanding: Option<Instant>,
}

impl UiWatchdog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called every [`PROBE_EVERY`] with whether the main window is shown and not minimised.
    pub fn tick(&mut self, now: Instant, shown: bool) -> Action {
        if !shown {
            // Not judged while hidden; a probe sent before hiding does not count against it.
            self.outstanding = None;
            return Action::Nothing;
        }
        match self.outstanding {
            Some(sent) if self.answered_since_load && now.duration_since(sent) >= DEAD_AFTER => {
                self.answered_since_load = false;
                self.outstanding = None;
                Action::Reload {
                    silent_for: now.duration_since(sent),
                }
            }
            Some(_) => Action::Nothing,
            None => {
                self.outstanding = Some(now);
                Action::Probe
            }
        }
    }

    /// A probe's answer arrived (the raw string the webview handed back).
    pub fn answered(&mut self, answer: &str) {
        if answer.contains(PROBE_TOKEN) {
            self.answered_since_load = true;
            self.outstanding = None;
        }
    }
}

/// Append one line to `<dir>/ui-process.log` (created owner-only), keeping the file small.
pub fn write_diagnostic(dir: &Path, unix_ms: i64, silent_for: Duration) -> std::io::Result<()> {
    append_line(
        dir,
        &format!(
            "{unix_ms} the main window's page did not answer for {} s; reloaded it",
            silent_for.as_secs()
        ),
    )
}

/// The same file, for a process WebView2 reported as failed.
pub fn write_crash(
    dir: &Path,
    unix_ms: i64,
    failure: Failure,
    recovery: Recovery,
) -> std::io::Result<()> {
    append_line(
        dir,
        &format!(
            "{unix_ms} the web view reported {}; {}",
            failure.describe(),
            recovery.describe()
        ),
    )
}

fn append_line(dir: &Path, line: &str) -> std::io::Result<()> {
    helm_core::privfs::create_private_dir_all(dir)?;
    let path = dir.join("ui-process.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > DIAGNOSTIC_MAX_BYTES {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let keep = text.len() / 2;
            let cut = text[keep..]
                .find('\n')
                .map(|i| keep + i + 1)
                .unwrap_or(text.len());
            std::fs::write(&path, &text[cut..])?;
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{line}")
}

/// A process WebView2 reported as failed (its process-failed event, by kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The browser process that hosts everything: the web view is gone.
    BrowserExited,
    /// The page's renderer process exited or crashed.
    RendererExited,
    /// The renderer stopped answering but is still alive.
    RendererUnresponsive,
    /// A renderer for a frame inside the page exited (a sealed page in the side panel, say).
    FrameRendererExited,
    /// Anything else (GPU, utility and helper processes): WebView2 restarts those itself.
    Other,
}

impl Failure {
    /// From the event's kind number: 0 browser, 1 renderer, 2 renderer unresponsive, 3 frame
    /// renderer; the rest are processes WebView2 recovers on its own.
    pub fn from_kind(kind: i32) -> Self {
        match kind {
            0 => Self::BrowserExited,
            1 => Self::RendererExited,
            2 => Self::RendererUnresponsive,
            3 => Self::FrameRendererExited,
            _ => Self::Other,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::BrowserExited => "the browser process exited",
            Self::RendererExited => "the page's renderer exited",
            Self::RendererUnresponsive => "the page's renderer was unresponsive",
            Self::FrameRendererExited => "a frame's renderer exited",
            Self::Other => "another web view process failed",
        }
    }
}

/// What the app does about a [`Failure`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Nothing: the failure is not the main page's, or it may clear by itself.
    Ignore,
    /// Reload the page in the same window.
    Reload,
    /// The web view is gone: build the window again.
    Rebuild,
    /// It keeps failing: ask the user before trying again.
    Offer,
}

impl Recovery {
    fn describe(self) -> &'static str {
        match self {
            Self::Ignore => "left it alone",
            Self::Reload => "reloaded the page",
            Self::Rebuild => "built the window again",
            Self::Offer => "asked whether to reload",
        }
    }
}

/// The repair that would be the `CRASH_LIMIT`th within `CRASH_WINDOW` is offered instead.
pub const CRASH_WINDOW: Duration = Duration::from_secs(120);
pub const CRASH_LIMIT: usize = 3;

/// Decides what to do about each reported failure, and stops a crash loop.
#[derive(Debug, Default)]
pub struct CrashGuard {
    repairs: Vec<Instant>,
}

impl CrashGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_failure(&mut self, now: Instant, failure: Failure) -> Recovery {
        let wanted = match failure {
            Failure::RendererExited => Recovery::Reload,
            Failure::BrowserExited => Recovery::Rebuild,
            // A frame's renderer is not the main page; an unresponsive one is the probe's to judge.
            Failure::FrameRendererExited | Failure::RendererUnresponsive | Failure::Other => {
                return Recovery::Ignore
            }
        };
        self.repairs
            .retain(|t| now.saturating_duration_since(*t) < CRASH_WINDOW);
        if self.repairs.len() + 1 >= CRASH_LIMIT {
            return Recovery::Offer;
        }
        self.repairs.push(now);
        wanted
    }

    /// The user said to repair after an offer: the count starts again, with this repair in it.
    pub fn offered_and_accepted(&mut self, now: Instant) {
        self.repairs.clear();
        self.repairs.push(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    #[test]
    fn a_page_that_stops_answering_is_reloaded_once_after_30_s() {
        let t0 = Instant::now();
        let mut w = UiWatchdog::new();
        assert_eq!(w.tick(t0, true), Action::Probe);
        w.answered("\"helm-ui-alive\"");
        assert_eq!(w.tick(at(t0, 10), true), Action::Probe);
        // No answer from here on.
        assert_eq!(w.tick(at(t0, 20), true), Action::Nothing);
        assert_eq!(w.tick(at(t0, 30), true), Action::Nothing);
        assert_eq!(
            w.tick(at(t0, 40), true),
            Action::Reload {
                silent_for: Duration::from_secs(30)
            }
        );
        // The reloaded page has not answered yet: probed, never reloaded again.
        assert_eq!(w.tick(at(t0, 50), true), Action::Probe);
        for s in [60, 90, 120, 300] {
            assert_eq!(w.tick(at(t0, s), true), Action::Nothing);
        }
    }

    #[test]
    fn a_live_page_is_never_reloaded() {
        let t0 = Instant::now();
        let mut w = UiWatchdog::new();
        for i in 0..100 {
            let a = w.tick(at(t0, i * 10), true);
            assert_eq!(a, Action::Probe, "tick {i}");
            w.answered("\"helm-ui-alive\"");
        }
    }

    #[test]
    fn a_hidden_or_minimised_window_is_not_judged() {
        let t0 = Instant::now();
        let mut w = UiWatchdog::new();
        assert_eq!(w.tick(t0, true), Action::Probe);
        w.answered("\"helm-ui-alive\"");
        assert_eq!(w.tick(at(t0, 10), true), Action::Probe);
        // Hidden to the tray for ten minutes with that probe unanswered.
        for s in (20..620).step_by(10) {
            assert_eq!(w.tick(at(t0, s), false), Action::Nothing);
        }
        // Shown again: a fresh probe, judged from now.
        assert_eq!(w.tick(at(t0, 620), true), Action::Probe);
        w.answered("\"helm-ui-alive\"");
        assert_eq!(w.tick(at(t0, 630), true), Action::Probe);
    }

    #[test]
    fn only_the_probe_token_counts_as_an_answer() {
        let t0 = Instant::now();
        let mut w = UiWatchdog::new();
        assert_eq!(w.tick(t0, true), Action::Probe);
        w.answered("\"helm-ui-alive\"");
        assert_eq!(w.tick(at(t0, 10), true), Action::Probe);
        w.answered("null");
        w.answered("");
        assert_eq!(w.tick(at(t0, 20), true), Action::Nothing);
        assert_eq!(w.tick(at(t0, 30), true), Action::Nothing);
        assert!(matches!(w.tick(at(t0, 40), true), Action::Reload { .. }));
    }

    #[test]
    fn a_crash_loop_ends_in_an_offer_and_other_failures_are_left_alone() {
        let t0 = Instant::now();
        for (kind, want) in [
            (0, Failure::BrowserExited),
            (1, Failure::RendererExited),
            (2, Failure::RendererUnresponsive),
            (3, Failure::FrameRendererExited),
            (6, Failure::Other),
            (99, Failure::Other),
        ] {
            assert_eq!(Failure::from_kind(kind), want, "kind {kind}");
        }
        let mut g = CrashGuard::new();
        for f in [
            Failure::FrameRendererExited,
            Failure::RendererUnresponsive,
            Failure::Other,
        ] {
            assert_eq!(g.on_failure(t0, f), Recovery::Ignore, "{f:?}");
        }
        // Two quick repairs are made; the third within two minutes is offered instead.
        let r = Failure::RendererExited;
        assert_eq!(g.on_failure(at(t0, 0), r), Recovery::Reload);
        assert_eq!(
            g.on_failure(at(t0, 30), Failure::BrowserExited),
            Recovery::Rebuild
        );
        assert_eq!(g.on_failure(at(t0, 60), r), Recovery::Offer);
        assert_eq!(g.on_failure(at(t0, 61), r), Recovery::Offer);
        // Failures spread out in time are each repaired.
        assert_eq!(g.on_failure(at(t0, 400), r), Recovery::Reload);
        assert_eq!(g.on_failure(at(t0, 900), r), Recovery::Reload);
        // After the user accepts an offer, the count starts again.
        g.offered_and_accepted(at(t0, 901));
        assert_eq!(g.on_failure(at(t0, 902), r), Recovery::Reload);
        assert_eq!(g.on_failure(at(t0, 903), r), Recovery::Offer);
    }

    #[test]
    fn the_diagnostic_appends_a_line_and_stays_small() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("diagnostics");
        write_diagnostic(&dir, 1, Duration::from_secs(31)).unwrap();
        write_diagnostic(&dir, 2, Duration::from_secs(45)).unwrap();
        let text = std::fs::read_to_string(dir.join("ui-process.log")).unwrap();
        assert_eq!(
            text,
            "1 the main window's page did not answer for 31 s; reloaded it\n\
             2 the main window's page did not answer for 45 s; reloaded it\n"
        );
        for i in 0..2000 {
            write_diagnostic(&dir, i, Duration::from_secs(30)).unwrap();
        }
        write_crash(&dir, 7, Failure::RendererExited, Recovery::Reload).unwrap();
        let len = std::fs::metadata(dir.join("ui-process.log")).unwrap().len();
        assert!(len <= DIAGNOSTIC_MAX_BYTES + 200, "{len}");
        let text = std::fs::read_to_string(dir.join("ui-process.log")).unwrap();
        assert!(
            text.ends_with(
                "7 the web view reported the page's renderer exited; reloaded the page\n"
            ),
            "{text:?}"
        );
        assert!(
            text.lines().all(|l| l.contains("reloaded")),
            "cut on a line boundary"
        );
    }
}
