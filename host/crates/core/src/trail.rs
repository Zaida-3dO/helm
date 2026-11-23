//! The trail the host leaves so a run that ends without warning can be told from one that
//! ended in an orderly way.
//!
//! Three small files in the profile's `diagnostics` folder, written with
//! [`privfs::write_atomic`]. They hold numbers, ids and the app version only — never message
//! text, tokens or codes — and nothing leaves the machine.
//!
//! - `run.json` — written when a run takes the profile over: when it started, its process id,
//!   the app version. An orderly end adds an `exit` with the time and the reason to the same
//!   record. A record still missing that field when the next run starts means the previous
//!   run was cut short.
//! - `heartbeat.json` — rewritten every [`HEARTBEAT_EVERY`] while the app runs: the time, the
//!   process id, the uptime and how many chats were working. It pins the moment of a silent
//!   death to about a minute, and a count climbing before it shows a pile-up.
//! - `unclean.json` — written at a start that follows a cut-short run: the previous run's
//!   record and its last heartbeat, kept so the story survives the host log's rolling.
//! - `update-pending.json` — written when an update quits Helm to run a setup file: the setup's
//!   process id and the time. It stays until the launcher has seen the setup end and started
//!   Helm again (or a run proves the update is over): its absence is what closes the promise that
//!   an update, even a failed one, never leaves Helm down.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// How often the heartbeat file is rewritten while the app runs.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(45);

fn run_path(diagnostics: &Path) -> PathBuf {
    diagnostics.join("run.json")
}

fn heartbeat_path(diagnostics: &Path) -> PathBuf {
    diagnostics.join("heartbeat.json")
}

fn unclean_path(diagnostics: &Path) -> PathBuf {
    diagnostics.join("unclean.json")
}

fn update_pending_path(diagnostics: &Path) -> PathBuf {
    diagnostics.join("update-pending.json")
}

/// What the run before this one left behind when it never said it had finished.
#[derive(Debug)]
pub struct Unclean {
    /// The previous run's record, as far as it could be read (null when the file was there
    /// but unreadable).
    pub previous: Option<Value>,
    /// The last heartbeat that run wrote, if any.
    pub last_heartbeat: Option<Value>,
}

/// Read one small JSON trail file: `None` when it is not there, and an unparseable file
/// reads as null so a start still knows a run was here.
fn read_json(path: &Path) -> Option<Value> {
    if !path.exists() {
        return None;
    }
    let bytes = match crate::privfs::read_capped(path, 16 * 1024) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("trail file {} unreadable: {e}", path.display());
            return Some(Value::Null);
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(_) => Some(Value::Null),
    }
}

fn write_json(path: &Path, v: &Value) {
    let bytes = serde_json::to_vec(v).unwrap_or_default();
    if let Err(e) = crate::privfs::write_atomic(path, &bytes) {
        log::warn!("trail file {} not written: {e}", path.display());
    }
}

/// The reason an orderly end is recorded with, set when a quit sequence starts.
fn exit_reason() -> &'static Mutex<String> {
    static R: OnceLock<Mutex<String>> = OnceLock::new();
    R.get_or_init(|| Mutex::new("app exit".to_owned()))
}

/// Say why the app is about to end; the last word before the marker is written wins.
pub fn note_exit_reason(reason: &str) {
    *exit_reason().lock().unwrap_or_else(|p| p.into_inner()) = reason.to_owned();
}

/// Record this run starting, and report what the run before it ended like.
///
/// Returns [`Unclean`] when the previous run left its record open; that start then also
/// writes `unclean.json`. A folder with no `run.json` at all is a first run: no note.
pub fn start(diagnostics: &Path, now_ms: i64, version: &str) -> Option<Unclean> {
    if let Err(e) = crate::privfs::create_private_dir_all(diagnostics) {
        log::warn!("diagnostics folder not ready: {e}");
    }
    let previous = read_json(&run_path(diagnostics));
    let unclean = match previous {
        None => None,
        Some(v) if v.get("exit").is_some() => None,
        Some(v) => Some(Unclean {
            previous: (v != Value::Null).then_some(v),
            last_heartbeat: read_json(&heartbeat_path(diagnostics)),
        }),
    };
    write_json(
        &run_path(diagnostics),
        &json!({
            "startedAt": now_ms,
            "pid": std::process::id(),
            "version": version,
        }),
    );
    if let Some(u) = &unclean {
        write_json(
            &unclean_path(diagnostics),
            &json!({
                "at": now_ms,
                "previousRun": u.previous,
                "lastHeartbeat": u.last_heartbeat,
            }),
        );
    }
    unclean
}

/// The last line of the run record: the app is ending in an orderly way.
pub fn record_exit(diagnostics: &Path, now_ms: i64) {
    let mut rec = read_json(&run_path(diagnostics))
        .unwrap_or_else(|| json!({"startedAt": 0, "pid": std::process::id(), "version": ""}));
    if rec == Value::Null {
        rec = json!({"startedAt": 0, "pid": std::process::id(), "version": ""});
    }
    rec["exit"] = json!({
        "at": now_ms,
        "reason": exit_reason().lock().unwrap_or_else(|p| p.into_inner()).clone(),
    });
    write_json(&run_path(diagnostics), &rec);
}

/// An update that quit Helm to run a setup file, as the launcher needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdatePending {
    /// The setup process: while it runs the files are being replaced; once it is gone the
    /// installed Helm (old or new) can be started.
    pub setup_pid: u32,
    pub at_ms: i64,
}

/// Note that an update has started: the setup file's process id, so the launcher can tell when
/// it has ended. The marker outlives the run that wrote it — that is its whole point.
pub fn note_update_pending(diagnostics: &Path, setup_pid: u32, at_ms: i64) {
    if let Err(e) = crate::privfs::create_private_dir_all(diagnostics) {
        log::warn!("diagnostics folder not ready: {e}");
    }
    write_json(
        &update_pending_path(diagnostics),
        &json!({"setupPid": setup_pid, "at": at_ms}),
    );
}

/// The pending-update marker, or `None` when there is none or it reads as nothing.
pub fn read_update_pending(diagnostics: &Path) -> Option<UpdatePending> {
    let v = read_json(&update_pending_path(diagnostics))?;
    Some(UpdatePending {
        setup_pid: v.get("setupPid")?.as_u64()? as u32,
        at_ms: v.get("at")?.as_i64()?,
    })
}

/// The update is over: Helm runs again, or a relaunch for it was attempted.
pub fn clear_update_pending(diagnostics: &Path) {
    let _ = std::fs::remove_file(update_pending_path(diagnostics));
}

/// One trail time as text a person can read, e.g. `2001-09-09T01:46:40Z`.
pub fn clock_text(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    // Days since the epoch to a civil date (the usual days-from-civil scheme).
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y0 = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y0 + 1 } else { y0 };
    format!("{y:0>4}-{m:0>2}-{d:0>2}T{h:0>2}:{mi:0>2}:{s:0>2}Z")
}

/// Rewrite the heartbeat. Cheap and best-effort: a failed write is only a warning.
pub fn heartbeat(diagnostics: &Path, now_ms: i64, uptime_secs: u64, working: usize) {
    write_json(
        &heartbeat_path(diagnostics),
        &json!({
            "at": now_ms,
            "pid": std::process::id(),
            "uptimeS": uptime_secs,
            "working": working,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_after_a_run_that_never_finished_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();

        // A first run: nothing to report, and its record is open.
        assert!(start(d, 1_000, "9.9.9").is_none());
        let rec = read_json(&run_path(d)).unwrap();
        assert_eq!(rec["startedAt"], json!(1_000));
        assert_eq!(rec["version"], json!("9.9.9"));
        assert!(rec.get("exit").is_none());

        // Heartbeats land while it runs.
        heartbeat(d, 2_500, 1, 3);
        let hb = read_json(&heartbeat_path(d)).unwrap();
        assert_eq!(hb["at"], json!(2_500));
        assert_eq!(hb["working"], json!(3));

        // An orderly end closes the record; the next start says nothing.
        note_exit_reason("user quit");
        record_exit(d, 3_000);
        let rec = read_json(&run_path(d)).unwrap();
        assert_eq!(rec["exit"]["reason"], json!("user quit"));
        assert_eq!(rec["exit"]["at"], json!(3_000));
        assert!(start(d, 4_000, "9.9.9").is_none());
        assert!(!unclean_path(d).exists(), "no note after a clean run");

        // A run cut short: its record stays open, and the next start reports it with the
        // last heartbeat and keeps the pair in unclean.json.
        note_exit_reason("app exit");
        record_exit(d, 4_500); // this one closes orderly too, before the cut-short one starts
        assert!(start(d, 5_000, "9.9.9").is_none());
        heartbeat(d, 5_500, 1, 2);
        let u = start(d, 9_000, "9.9.9").expect("the previous run never closed");
        assert_eq!(u.previous.as_ref().unwrap()["startedAt"], json!(5_000));
        assert_eq!(u.last_heartbeat.as_ref().unwrap()["at"], json!(5_500));
        let kept = read_json(&unclean_path(d)).unwrap();
        assert_eq!(kept["at"], json!(9_000));
        assert_eq!(kept["lastHeartbeat"]["at"], json!(5_500));

        // A record that cannot be read still says a run was here.
        std::fs::write(run_path(d), b"not json").unwrap();
        let u = start(d, 10_000, "9.9.9").expect("an unreadable record is unclean too");
        assert!(u.previous.is_none());
    }

    #[test]
    fn trail_times_read_as_utc_text() {
        assert_eq!(clock_text(0), "1970-01-01T00:00:00Z");
        assert_eq!(clock_text(1_000_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(clock_text(-1_000), "1969-12-31T23:59:59Z");
    }
}
