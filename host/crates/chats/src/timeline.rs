//! The send timeline: one `info` log line per turn-opening send, so a slow first
//! token can be split into Helm's share and Claude Code's without rebuilding it from transcripts.
//!
//! Times are epoch milliseconds (host and worker share the clock), printed relative to the moment
//! the send was saved. The worker's own marks come from the `timing` field of its `get_state` and
//! prompt responses (`worker/src/protocol/timing.ts`). The line is written at the first output
//! (a text or thinking delta, or a tool start), or when the turn ends without any. The same line
//! is appended to `<profile>/diagnostics/send-timeline.log` (kept under 256 KiB, one older file),
//! because a release build has no console for the log.
//!
//! ```text
//! chat c1: send 1b2c3d4e timeline (ms after saved): dispatched +2, spawn +3, worker +210,
//! claude +260, initialized +301, ready +305, written +307, acked +312, first output +318; cold
//! ```
//!
//! A steer (a message sent while a turn runs) that is still unread when the wait limit asks the
//! worker to deliver it gets lines of its own in the same file: one at the worker's first
//! `waiting` answer, then at most one every [`WAIT_LOG_EVERY_MS`], each naming what holds it
//! (a running call by its tool name and kind, an open card, or the model itself), and one when
//! Claude reads it.
//!
//! ```text
//! chat c1: steer 1b2c3d4e waiting +31004 (answer 1): helm mcp__helm__app_slow 31 s
//! chat c1: steer 1b2c3d4e read +32110 after 2 waiting answers
//! ```

use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// The file is moved to `send-timeline.old.log` once it passes this.
const FILE_CAP: u64 = 256 * 1024;
/// A steer that keeps waiting is logged again at most this often.
pub(crate) const WAIT_LOG_EVERY_MS: i64 = 30_000;
/// Steers tracked at once; one never read (withdrawn, or the worker went away) is dropped first.
const MAX_STEER_WAITS: usize = 64;
/// What holds a steer: at most this many calls per line.
const MAX_BLOCKERS: usize = 8;

/// One send, from the actor taking it to its first output.
#[derive(Debug, Clone, Default)]
pub(crate) struct SendTimeline {
    pub submission_id: String,
    /// The submission's `created_at`: when `chat_send` saved it.
    pub saved: i64,
    pub dispatched: i64,
    /// The worker was ready before the send (no start-up in front of it).
    pub warm: bool,
    /// The worker was started ahead of the send (pre-warm) but was not ready yet.
    pub warming: bool,
    /// The worker start was requested (the process start is the worker's `workerStart` mark).
    pub spawn: Option<i64>,
    pub ready: Option<i64>,
    /// The worker's launch marks (`get_state` response `timing`).
    pub launch: Option<Value>,
    /// The worker's prompt marks (`received`, `written`).
    pub prompt: Option<Value>,
    pub acked: Option<i64>,
    pub first_output: Option<i64>,
}

impl SendTimeline {
    pub fn new(submission_id: &str, saved: i64, dispatched: i64) -> Self {
        Self {
            submission_id: submission_id.to_owned(),
            saved,
            dispatched,
            ..Default::default()
        }
    }

    fn rel(&self, t: i64) -> String {
        let d = t - self.saved;
        if d >= 0 {
            format!("+{d}")
        } else {
            d.to_string()
        }
    }

    fn mark(v: Option<&Value>, k: &str) -> Option<i64> {
        v.and_then(|v| v.get(k)).and_then(Value::as_i64)
    }

    /// The log line (without the chat prefix).
    pub fn line(&self) -> String {
        let short: String = self.submission_id.chars().take(8).collect();
        let mut parts = vec![format!("dispatched {}", self.rel(self.dispatched))];
        if let Some(t) = self.spawn {
            parts.push(format!("spawn {}", self.rel(t)));
        }
        // Launch marks before the send (a pre-warmed or reused worker) are left out: they are
        // not part of this send's wait.
        let launch = self.launch.as_ref();
        for (k, label) in [
            ("workerStart", "worker"),
            ("claudeSpawned", "claude"),
            ("initialized", "initialized"),
        ] {
            if let Some(t) = Self::mark(launch, k).filter(|t| *t >= self.saved) {
                parts.push(format!("{label} {}", self.rel(t)));
            }
        }
        if let Some(t) = self.ready {
            parts.push(format!("ready {}", self.rel(t)));
        }
        if let Some(t) = Self::mark(self.prompt.as_ref(), "written") {
            parts.push(format!("written {}", self.rel(t)));
        }
        if let Some(t) = self.acked {
            parts.push(format!("acked {}", self.rel(t)));
        }
        match self.first_output {
            Some(t) => parts.push(format!("first output {}", self.rel(t))),
            None => parts.push("no output".into()),
        }
        let kind = if self.warm {
            "warm"
        } else if self.warming {
            "pre-warming"
        } else {
            "cold"
        };
        format!(
            "send {short} timeline (ms after saved): {}; {kind}",
            parts.join(", ")
        )
    }
}

/// Write the line once and clear the slot: at the first output, or when the turn ends without one.
pub(crate) fn finish(
    slot: &mut Option<SendTimeline>,
    profile: &Path,
    chat_id: &str,
    first_output: Option<i64>,
) {
    if let Some(mut t) = slot.take() {
        t.first_output = first_output;
        log_line(profile, chat_id, t.saved, &t.line());
    }
}

/// Log one line for a chat and append it to the diagnostics file.
pub(crate) fn log_line(profile: &Path, chat_id: &str, saved: i64, line: &str) {
    let line = format!("chat {chat_id}: {line}");
    log::info!("{line}");
    if let Err(e) = append(&profile.join("diagnostics"), saved, &line) {
        log::warn!("send timeline not written: {e}");
    }
}

/// The steers a chat's worker has answered `waiting` for, until each is read.
#[derive(Debug, Default)]
pub(crate) struct SteerWaits {
    /// Submission id → (waiting answers so far, when its last line was written).
    seen: HashMap<String, (u32, i64)>,
}

impl SteerWaits {
    /// A `waiting` answer at `now`: the answer's number when it gets a line (the first, then one
    /// per [`WAIT_LOG_EVERY_MS`]), otherwise `None`.
    pub fn waiting(&mut self, sid: &str, now: i64) -> Option<u32> {
        if !self.seen.contains_key(sid) && self.seen.len() >= MAX_STEER_WAITS {
            let stale = self
                .seen
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone());
            if let Some(k) = stale {
                self.seen.remove(&k);
            }
        }
        let (answers, logged) = self.seen.entry(sid.to_owned()).or_insert((0, i64::MIN));
        *answers += 1;
        if *logged == i64::MIN || now - *logged >= WAIT_LOG_EVERY_MS {
            *logged = now;
            Some(*answers)
        } else {
            None
        }
    }

    /// The steer was read: how many `waiting` answers it had. It is forgotten.
    pub fn read(&mut self, sid: &str) -> u32 {
        self.seen.remove(sid).map_or(0, |(n, _)| n)
    }
}

fn short(sid: &str) -> String {
    sid.chars().take(8).collect()
}

fn rel(saved: i64, t: i64) -> String {
    let d = t - saved;
    if d >= 0 {
        format!("+{d}")
    } else {
        d.to_string()
    }
}

/// The line for a `waiting` answer: how long the steer has waited and what holds it (the
/// answer's `blockers`; kinds `helm`, `mcp`, `shell`, `agent`, `native`, `model`, `user`).
pub(crate) fn steer_wait_line(
    sid: &str,
    saved: i64,
    now: i64,
    answer: u32,
    data: &Value,
) -> String {
    let blockers = data
        .get("blockers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut parts: Vec<String> = blockers
        .iter()
        .take(MAX_BLOCKERS)
        .map(|b| {
            let kind = b.get("kind").and_then(Value::as_str).unwrap_or("unknown");
            let mut p: String = kind.chars().take(16).collect();
            if let Some(name) = b.get("name").and_then(Value::as_str) {
                p.push(' ');
                p.extend(name.chars().take(80));
            }
            if kind != "user" {
                let secs = b.get("runningMs").and_then(Value::as_i64).unwrap_or(0) / 1000;
                p.push_str(&format!(" {secs} s"));
            }
            p
        })
        .collect();
    if blockers.len() > MAX_BLOCKERS {
        parts.push(format!("{} more", blockers.len() - MAX_BLOCKERS));
    }
    let held = if parts.is_empty() {
        "nothing named".to_owned()
    } else {
        parts.join(", ")
    };
    format!(
        "steer {} waiting {} (answer {answer}): {held}",
        short(sid),
        rel(saved, now)
    )
}

/// The line for a steer Claude has read.
pub(crate) fn steer_read_line(sid: &str, saved: i64, now: i64, answers: u32) -> String {
    let waits = match answers {
        0 => "with no waiting answer".to_owned(),
        1 => "after 1 waiting answer".to_owned(),
        n => format!("after {n} waiting answers"),
    };
    format!("steer {} read {} {waits}", short(sid), rel(saved, now))
}

/// Append `<saved> <line>` to `send-timeline.log`, rolling it over past [`FILE_CAP`].
fn append(dir: &Path, saved: i64, line: &str) -> std::io::Result<()> {
    helm_core::privfs::create_private_dir_all(dir)?;
    let path = dir.join("send-timeline.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > FILE_CAP) {
        std::fs::rename(&path, dir.join("send-timeline.old.log"))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    // One write per line, so lines from two chats never interleave.
    f.write_all(format!("{saved} {line}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_cold_send_lists_every_step_relative_to_saved() {
        let mut t = SendTimeline::new("1b2c3d4e-0000", 1_000, 1_002);
        t.spawn = Some(1_003);
        t.launch = Some(
            json!({"workerStart": 1_210, "claudeSpawned": 1_260, "initialized": 1_301, "ready": 1_305}),
        );
        t.ready = Some(1_305);
        t.prompt = Some(json!({"received": 1_306, "written": 1_307}));
        t.acked = Some(1_312);
        t.first_output = Some(1_318);
        assert_eq!(
            t.line(),
            "send 1b2c3d4e timeline (ms after saved): dispatched +2, spawn +3, worker +210, \
             claude +260, initialized +301, ready +305, written +307, acked +312, first output +318; cold"
        );
    }

    #[test]
    fn the_line_goes_to_the_diagnostics_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut slot = Some(SendTimeline::new("abcdef0123", 7, 9));
        finish(&mut slot, dir.path(), "c1", Some(20));
        finish(&mut slot, dir.path(), "c1", Some(30));
        assert!(slot.is_none());
        let text =
            std::fs::read_to_string(dir.path().join("diagnostics/send-timeline.log")).unwrap();
        assert_eq!(
            text,
            "7 chat c1: send abcdef01 timeline (ms after saved): dispatched +2, first output +13; cold\n"
        );
    }

    #[test]
    fn the_file_rolls_over_past_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("send-timeline.log");
        std::fs::write(&path, vec![b'x'; FILE_CAP as usize + 1]).unwrap();
        append(dir.path(), 1, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1 new\n");
        assert!(dir.path().join("send-timeline.old.log").is_file());
    }

    #[test]
    fn a_waiting_steer_is_logged_first_then_every_thirty_seconds_and_when_read() {
        let mut w = SteerWaits::default();
        // One answer a second from 10 s after saved: lines at the first answer and 30 s on.
        let logged: Vec<(i64, u32)> = (0..45)
            .filter_map(|i| {
                let now = 10_000 + i * 1_000;
                w.waiting("1b2c3d4e-9", now).map(|n| (now, n))
            })
            .collect();
        assert_eq!(logged, [(10_000, 1), (40_000, 31)]);
        for (data, want) in [
            (
                json!({"result": "waiting", "blockers": [
                    {"toolCallId": "tu_h", "name": "mcp__helm__app_slow", "kind": "helm", "runningMs": 31_400},
                    {"toolCallId": "tu_r", "name": "Read", "kind": "native", "runningMs": 900}
                ]}),
                "helm mcp__helm__app_slow 31 s, native Read 0 s",
            ),
            (
                json!({"blockers": [{"kind": "model", "runningMs": 64_000}]}),
                "model 64 s",
            ),
            (json!({"blockers": [{"kind": "user", "runningMs": 0}]}), "user"),
            (json!({"result": "waiting"}), "nothing named"),
            (
                json!({"blockers": (0..10).map(|_| json!({"kind": "native", "name": "Grep", "runningMs": 0})).collect::<Vec<_>>()}),
                "native Grep 0 s, native Grep 0 s, native Grep 0 s, native Grep 0 s, native Grep 0 s, \
                 native Grep 0 s, native Grep 0 s, native Grep 0 s, 2 more",
            ),
        ] {
            assert_eq!(
                steer_wait_line("1b2c3d4e-9", 0, 10_000, 1, &data),
                format!("steer 1b2c3d4e waiting +10000 (answer 1): {want}")
            );
        }
        assert_eq!(w.read("1b2c3d4e-9"), 45);
        assert_eq!(w.read("1b2c3d4e-9"), 0, "forgotten once read");
        assert_eq!(
            steer_read_line("1b2c3d4e-9", 1_000, 55_000, 45),
            "steer 1b2c3d4e read +54000 after 45 waiting answers"
        );
        // Never more than a bounded number tracked: the stalest goes first.
        for i in 0..(MAX_STEER_WAITS as i64 + 1) {
            w.waiting(&format!("s{i}"), i);
        }
        assert_eq!(w.seen.len(), MAX_STEER_WAITS);
        assert!(!w.seen.contains_key("s0"));
        // The lines reach the same file as the send timeline.
        let dir = tempfile::tempdir().unwrap();
        log_line(dir.path(), "c1", 5, &steer_read_line("abc", 5, 8, 0));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("diagnostics/send-timeline.log")).unwrap(),
            "5 chat c1: steer abc read +3 with no waiting answer\n"
        );
    }

    #[test]
    fn a_warm_send_leaves_out_launch_marks_from_before_it() {
        let mut t = SendTimeline::new("abc", 5_000, 5_001);
        t.warm = true;
        t.launch = Some(json!({"workerStart": 100, "claudeSpawned": 200}));
        t.acked = Some(5_040);
        assert_eq!(
            t.line(),
            "send abc timeline (ms after saved): dispatched +1, acked +40, no output; warm"
        );
    }
}
