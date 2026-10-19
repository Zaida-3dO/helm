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
//! chat c1: send 1b2c3d4e timeline (ms after saved): dispatched +2, spawn +3..+9, worker +210,
//! claude +260, initialized +301, ready +305, written +307, acked +312, first output +318; cold
//! ```

use serde_json::Value;
use std::io::Write;
use std::path::Path;

/// The file is moved to `send-timeline.old.log` once it passes this.
const FILE_CAP: u64 = 256 * 1024;

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
    /// Spawn requested, and the worker process started.
    pub spawn: Option<(i64, i64)>,
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
        if let Some((a, b)) = self.spawn {
            parts.push(format!("spawn {}..{}", self.rel(a), self.rel(b)));
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
        let line = format!("chat {chat_id}: {}", t.line());
        log::info!("{line}");
        if let Err(e) = append(&profile.join("diagnostics"), t.saved, &line) {
            log::warn!("send timeline not written: {e}");
        }
    }
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
    writeln!(f, "{saved} {line}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_cold_send_lists_every_step_relative_to_saved() {
        let mut t = SendTimeline::new("1b2c3d4e-0000", 1_000, 1_002);
        t.spawn = Some((1_003, 1_009));
        t.launch = Some(
            json!({"workerStart": 1_210, "claudeSpawned": 1_260, "initialized": 1_301, "ready": 1_305}),
        );
        t.ready = Some(1_305);
        t.prompt = Some(json!({"received": 1_306, "written": 1_307}));
        t.acked = Some(1_312);
        t.first_output = Some(1_318);
        assert_eq!(
            t.line(),
            "send 1b2c3d4e timeline (ms after saved): dispatched +2, spawn +3..+9, worker +210, \
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
            "7 chat c1: send abcdef01 timeline (ms after saved): dispatched +2, first output +13; cold
"
        );
    }

    #[test]
    fn the_file_rolls_over_past_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("send-timeline.log");
        std::fs::write(&path, vec![b'x'; FILE_CAP as usize + 1]).unwrap();
        append(dir.path(), 1, "new").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "1 new
"
        );
        assert!(dir.path().join("send-timeline.old.log").is_file());
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
