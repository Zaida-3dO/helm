//! Is Helm running, and should a Helm that ended be started again?
//!
//! Helm leaves `diagnostics/run.json` in its profile: its process id when it starts, and an
//! `exit` field when it ends in an orderly way (a quit, a sign-out, a shutdown). A record with a
//! process id that is gone and no `exit` is a run that was cut short: a crash.
//!
//! The launcher restarts Helm only after it has watched that process end, never because of a
//! record it finds when it starts itself: the restart rule never opens Helm on its own. Opening
//! Helm at sign-in is the separate switch in [`crate::daemon::opens_helm_at_logon`]. [`decide`]
//! is the whole restart rule.
//!
//! An update is the exception, and [`decide_update`] its whole rule: a run that quit to run a
//! setup file leaves `diagnostics/update-pending.json` behind (written by the app through
//! [`helm_core::trail::note_update_pending`]), so the launcher can still bring Helm back once
//! that setup has ended — whatever the setup did, even fail. Without the marker the rule never
//! opens Helm on its own, exactly as before.

use serde::Deserialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long after Helm ends the launcher looks again before restarting it.
pub const RESTART_DELAY: Duration = Duration::from_secs(10);
/// A Helm program file modified more recently than this is being replaced by an update: no
/// restart.
pub const FRESH_EXE: Duration = Duration::from_secs(60);
/// At most this many restarts in [`RESTART_WINDOW`]; past that it is a crash loop.
pub const MAX_RESTARTS: usize = 3;
pub const RESTART_WINDOW: Duration = Duration::from_secs(15 * 60);
/// While nothing is running, how often the record is read again.
pub const LOOK_EVERY: Duration = Duration::from_secs(10);

/// What `run.json` says, as far as the launcher needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRecord {
    pub pid: Option<u32>,
    /// The run ended in an orderly way.
    pub exited: bool,
}

#[derive(Deserialize)]
struct RawRecord {
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    exit: Option<serde_json::Value>,
}

pub fn run_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("diagnostics").join("run.json")
}

/// The record, or `None` when there is none (Helm never ran in this profile). A file that does
/// not parse reads as a record with no process id.
pub fn read_run(profile_dir: &Path) -> Option<RunRecord> {
    let path = run_path(profile_dir);
    let bytes = helm_core::privfs::read_capped(&path, 16 * 1024).ok()?;
    Some(match serde_json::from_slice::<RawRecord>(&bytes) {
        Ok(r) => RunRecord {
            pid: r.pid,
            exited: r.exit.is_some_and(|e| !e.is_null()),
        },
        Err(_) => RunRecord {
            pid: None,
            exited: false,
        },
    })
}

/// The process the record names, if that run is still going.
pub fn running_pid(rec: Option<&RunRecord>, is_helm: impl Fn(u32) -> bool) -> Option<u32> {
    let rec = rec?;
    if rec.exited {
        return None;
    }
    rec.pid.filter(|p| is_helm(*p))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Nothing,
    Relaunch,
    /// Too many restarts lately: stop, and say so to the hub.
    GiveUp,
}

/// After a watched Helm ended and [`RESTART_DELAY`] passed: the record as it reads now, whether
/// the process it names is alive, how many restarts the window already holds, and how long ago
/// the Helm program file was modified (`None` when it is not there).
pub fn decide(
    rec: Option<&RunRecord>,
    pid_alive: bool,
    restarts_in_window: usize,
    exe_age: Option<Duration>,
) -> Decision {
    let Some(rec) = rec else {
        return Decision::Nothing;
    };
    // An orderly end, or a Helm that is running again (started by the person, or a new run).
    if rec.exited || pid_alive {
        return Decision::Nothing;
    }
    match exe_age {
        // Uninstalled or moved: nothing to start.
        None => Decision::Nothing,
        // An update is writing it.
        Some(age) if age < FRESH_EXE => Decision::Nothing,
        Some(_) if restarts_in_window >= MAX_RESTARTS => Decision::GiveUp,
        Some(_) => Decision::Relaunch,
    }
}

/// What the update marker asks for, with nothing else in the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateDecision {
    /// The setup is still running: its files are being replaced, and starting anything now
    /// would get in its way.
    Wait,
    /// The setup has ended and Helm is down: start whatever Helm this profile points at now.
    Relaunch,
    /// Helm runs again: the update's promise is kept and the marker can go.
    Done,
}

/// A watched Helm just ended: is an update mid-flight? True when the marker is present and its
/// setup process is still running. Then the update rule owns the restart and the crash rule must
/// keep its hands off — starting Helm now would collide with the setup replacing its files,
/// which is the locked-file failure the update rule exists to avoid.
pub fn update_waits(diag: &Path, setup_alive: impl FnOnce(u32) -> bool) -> bool {
    helm_core::trail::read_update_pending(diag).is_some_and(|m| setup_alive(m.setup_pid))
}

/// The update rule. The marker is present, Helm is not running as far as `run.json` says, and
/// `setup_alive` is whether the marker's setup process is still running. A dead pid that was
/// reused reads as alive, which is the safe way to be wrong: a setup is only ever minutes long,
/// and a person watching an update would notice one that never ends.
pub fn decide_update(setup_alive: bool, helm_running: bool) -> UpdateDecision {
    if helm_running {
        return UpdateDecision::Done;
    }
    if setup_alive {
        return UpdateDecision::Wait;
    }
    UpdateDecision::Relaunch
}

/// When the launcher restarted Helm lately.
#[derive(Default)]
pub struct Restarts {
    at: VecDeque<Instant>,
}

impl Restarts {
    pub fn in_window(&mut self, now: Instant) -> usize {
        while self
            .at
            .front()
            .is_some_and(|t| now.duration_since(*t) > RESTART_WINDOW)
        {
            self.at.pop_front();
        }
        self.at.len()
    }

    pub fn record(&mut self, now: Instant) {
        self.at.push_back(now);
    }

    pub fn clear(&mut self) {
        self.at.clear();
    }
}

/// How long ago a file was last modified; `None` when it cannot be read.
pub fn age_of(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        std::time::SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision::*;

    #[test]
    fn decide_restarts_only_a_crash_and_never_during_an_update_or_a_loop() {
        let crashed = RunRecord {
            pid: Some(42),
            exited: false,
        };
        let quit = RunRecord {
            pid: Some(42),
            exited: true,
        };
        let unreadable = RunRecord {
            pid: None,
            exited: false,
        };
        let old = Some(Duration::from_secs(3600));
        let fresh = Some(Duration::from_secs(5));
        // (why, record, its process alive, restarts in the window, program file's age, decision)
        type Row<'a> = (
            &'a str,
            Option<&'a RunRecord>,
            bool,
            usize,
            Option<Duration>,
            Decision,
        );
        #[rustfmt::skip]
        let rows: Vec<Row> = vec![
            ("no record: Helm never ran here",         None,              false, 0, old,   Nothing),
            ("an orderly quit stays quit",             Some(&quit),       false, 0, old,   Nothing),
            ("a quit record is final even if alive",   Some(&quit),       true,  0, old,   Nothing),
            ("running again (a new run, or a person)", Some(&crashed),    true,  0, old,   Nothing),
            ("a crash is restarted",                   Some(&crashed),    false, 0, old,   Relaunch),
            ("a second crash in the window",           Some(&crashed),    false, 2, old,   Relaunch),
            ("the fourth crash is a loop",             Some(&crashed),    false, 3, old,   GiveUp),
            ("well past the limit still gives up",     Some(&crashed),    false, 9, old,   GiveUp),
            ("an update is writing the program file",  Some(&crashed),    false, 0, fresh, Nothing),
            ("an update wins over a loop",             Some(&crashed),    false, 3, fresh, Nothing),
            ("the program file is gone",               Some(&crashed),    false, 0, None,  Nothing),
            ("an unreadable record left by a crash",   Some(&unreadable), false, 0, old,   Relaunch),
            ("just past the update guard",             Some(&crashed),    false, 0, Some(FRESH_EXE), Relaunch),
        ];
        for (why, rec, alive, restarts, age, want) in rows {
            assert_eq!(decide(rec, alive, restarts, age), want, "{why}");
        }
    }

    #[test]
    fn an_update_is_watched_until_its_setup_ends_then_helm_returns() {
        use UpdateDecision::*;
        // (why, the setup process, Helm, the decision)
        let rows: &[(&str, bool, bool, UpdateDecision)] = &[
            ("the setup is still replacing files", true, false, Wait),
            ("the setup ended and Helm is down", false, false, Relaunch),
            (
                "Helm runs again while the setup winds down",
                true,
                true,
                Done,
            ),
            ("Helm runs again after the setup", false, true, Done),
        ];
        for (why, setup_alive, helm_running, want) in rows {
            assert_eq!(decide_update(*setup_alive, *helm_running), *want, "{why}");
        }
        // The marker round-trips exactly as the app writes it; gone means gone.
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("diagnostics");
        assert_eq!(
            helm_core::trail::read_update_pending(&d),
            None,
            "no marker yet"
        );
        helm_core::trail::note_update_pending(&d, 4242, 6_000);
        assert_eq!(
            helm_core::trail::read_update_pending(&d),
            Some(helm_core::trail::UpdatePending {
                setup_pid: 4242,
                at_ms: 6_000
            })
        );
        // The crash rule's guard: while the marker's setup is still running the restart belongs
        // to the update rule, and only once that process has ended may the crash rule relaunch.
        assert!(
            update_waits(&d, |_| true),
            "the setup is still replacing files"
        );
        assert!(!update_waits(&d, |_| false), "the setup has ended");
        helm_core::trail::clear_update_pending(&d);
        assert_eq!(helm_core::trail::read_update_pending(&d), None);
        assert!(!update_waits(&d, |_| true), "no marker, no guard");
    }

    #[test]
    fn the_run_record_and_the_restart_window_read_as_the_launcher_needs() {
        let dir = tempfile::tempdir().unwrap();
        let diag = dir.path().join("diagnostics");
        assert_eq!(read_run(dir.path()), None, "no record yet");
        // The record Helm itself writes, then the field an orderly end adds.
        helm_core::trail::start(&diag, 1_000, "9.9.9");
        let rec = read_run(dir.path()).unwrap();
        assert_eq!(rec.pid, Some(std::process::id()));
        assert!(!rec.exited);
        assert_eq!(running_pid(Some(&rec), |_| true), rec.pid);
        assert_eq!(running_pid(Some(&rec), |_| false), None, "not Helm");
        helm_core::trail::record_exit(&diag, 2_000);
        let rec = read_run(dir.path()).unwrap();
        assert!(rec.exited);
        assert_eq!(running_pid(Some(&rec), |_| true), None);
        std::fs::write(run_path(dir.path()), b"{not json").unwrap();
        assert_eq!(
            read_run(dir.path()),
            Some(RunRecord {
                pid: None,
                exited: false
            })
        );

        let mut r = Restarts::default();
        let t0 = Instant::now();
        r.record(t0);
        r.record(t0 + Duration::from_secs(60));
        assert_eq!(r.in_window(t0 + Duration::from_secs(120)), 2);
        assert_eq!(
            r.in_window(t0 + RESTART_WINDOW + Duration::from_secs(30)),
            1,
            "the first fell out of the window"
        );
        r.clear();
        assert_eq!(r.in_window(t0), 0);
    }
}
