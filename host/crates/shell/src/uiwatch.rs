//! The UI content-process watchdog.
//!
//! The app probes the main window's page every [`PROBE_EVERY`] with a one-line script whose answer
//! carries [`PROBE_TOKEN`]. A page that answered before and then leaves a probe unanswered for
//! [`DEAD_AFTER`] while the window is shown is treated as a dead content process: the app writes a
//! diagnostic line ([`write_diagnostic`]) and reloads the page. Drafts survive because the composer
//! saves them on a debounce; nothing here touches the database.
//!
//! Decisions (Tauri does not surface WebView2's process-failed event):
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
    writeln!(
        f,
        "{unix_ms} the main window's page did not answer for {} s; reloaded it",
        silent_for.as_secs()
    )
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
        let len = std::fs::metadata(dir.join("ui-process.log")).unwrap().len();
        assert!(len <= DIAGNOSTIC_MAX_BYTES + 100, "{len}");
        let text = std::fs::read_to_string(dir.join("ui-process.log")).unwrap();
        assert!(
            text.ends_with("1999 the main window's page did not answer for 30 s; reloaded it\n")
        );
        assert!(
            text.lines().all(|l| l.ends_with("reloaded it")),
            "cut on a line boundary"
        );
    }
}
