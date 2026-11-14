//! Worker exits: what the chat says when its worker ends, and the log that records every end.
//!
//! - **The chat's line.** A worker that dies leaves a stderr tail, and the last lines of it are
//!   often Node's start-up warnings rather than anything about the end. [`meaningful_line`] drops
//!   that noise (and stack frames), so the chat shows the last line that says something.
//! - **The log.** Every worker exit is one JSON line in `<profile>/diagnostics/worker-exits.log`
//!   (kept under 1 MiB, one older file): when, the exit code and signal, whether Helm asked for
//!   the end and why, what the worker last said, how many processes of its tree were still
//!   running, the stderr tail, the tool call in flight, and the chat's model and folder.
//! - **Incidents.** Exits within [`INCIDENT_GAP_MS`] of each other belong to one incident, so a
//!   wave of workers ended together reads as one event ("One of 7 chats that stopped at 06:38:42")
//!   rather than seven unrelated failures. A second exit in an incident writes an `incident` line.
//! - **Crashes.** An exit Helm did not ask for, while a turn was open, is a crash: the outage
//!   watch asks such a chat to carry on (see [`crate::outage`]). What is known about crashes is
//!   kept in memory only; after a restart of the app, its own restart continuation covers them.

use chrono::TimeZone;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Exits closer together than this belong to one incident.
pub const INCIDENT_GAP_MS: i64 = 5_000;
/// The log moves to `worker-exits.old.log` once it passes this.
const FILE_CAP: u64 = 1024 * 1024;
/// Characters of the in-flight tool's command kept in the log.
const COMMAND_KEEP: usize = 300;
/// Bytes of stderr kept (the end of it) for an exit Helm asked for. A crash, or an exit with no
/// known cause, keeps all of it, so ordinary ends never roll a crash's evidence out of the log.
const ASKED_STDERR_KEEP: usize = 2 * 1024;
/// Crashes older than this are forgotten (the outage watch no longer carries such a turn on).
const CRASH_KEEP_MS: i64 = 6 * 60 * 60 * 1000;
/// At most this many crashes are remembered at once; the oldest go first.
const MAX_CRASHES: usize = 500;

/// Lines in a worker's stderr that never explain an exit: Node's warning about a module type it
/// had to guess (with its follow-up advice), experimental-feature notices, and the worker's own
/// closing line.
const NOISE: &[&str] = &[
    "module_typeless_package_json",
    "reparsing as es module",
    "to eliminate this warning",
    "--trace-warnings",
    "experimentalwarning",
    "type stripping",
    GOODBYE,
];

/// The line of `stderr` that best explains an exit: the last one that looks like an error
/// (`TypeError: …`, `FATAL ERROR: …`, the worker's own `[helm-worker] …`), else the last line that
/// says anything at all. Blank lines, stack frames (`at …` and native `13: 00007FF6… …`), Node's
/// closing `Node.js v…` and known noise are skipped. `None` when nothing is left.
pub fn meaningful_line(stderr: &str) -> Option<String> {
    let mut said = stderr.lines().rev().map(str::trim).filter(|l| !is_noise(l));
    let last = said.clone().next()?;
    let line = said.find(|l| looks_like_error(l)).unwrap_or(last);
    Some(line.chars().take(500).collect())
}

fn is_noise(l: &str) -> bool {
    if l.is_empty()
        || l.starts_with("at ")
        || is_native_frame(l)
        || (l.starts_with("-----") && l.ends_with("-----"))
        || l.strip_prefix("Node.js v")
            .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
    {
        return true;
    }
    let lower = l.to_ascii_lowercase();
    NOISE.iter().any(|n| lower.contains(n))
}

/// A frame of Node's native stack trace: `13: 00007FF6A1B2C3D4 RtlUserThreadStart+40`.
fn is_native_frame(l: &str) -> bool {
    l.split_once(": ").is_some_and(|(n, rest)| {
        !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
            && rest.chars().take_while(char::is_ascii_hexdigit).count() >= 8
    })
}

fn looks_like_error(l: &str) -> bool {
    l.starts_with("[helm-worker]")
        || l.contains("FATAL ERROR")
        || l.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|w| w.ends_with("Error") || w.ends_with("Exception"))
}

/// Why Helm itself ended (or was ending) a worker. `None` in [`ExitFacts::host_reason`] means
/// Helm had not asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostReason {
    /// The user stopped the turn.
    Stop,
    /// The chat was archived.
    Archive,
    /// The app is quitting.
    Quit,
    /// Helm closed the worker to start a fresh one (a changed setting, an edit, a retirement).
    Close,
    /// Helm ended the worker's process tree itself (a broken record, a stop or a change that
    /// timed out).
    Ended,
}

impl HostReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Archive => "archive",
            Self::Quit => "quit",
            Self::Close => "close",
            Self::Ended => "ended",
        }
    }
}

/// Everything known about one worker exit, as the chat saw it.
#[derive(Clone, Debug, Default)]
pub struct ExitFacts {
    pub chat_id: String,
    pub at_ms: i64,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub host_reason: Option<HostReason>,
    pub worker_error: Option<String>,
    /// Processes of the worker's tree still running when its exit was seen.
    pub job_active: Option<u32>,
    pub stderr: String,
    /// The tool call in flight: its name and command (or arguments).
    pub last_tool: Option<(String, String)>,
    pub uptime_ms: Option<i64>,
    pub model: Option<String>,
    /// The local model server, for a chat on one.
    pub provider: Option<String>,
    /// A turn was open: the user (or an agent) was waiting on this worker.
    pub turn_open: bool,
    /// Claude had read the prompt that opened the turn. A worker that dies before that leaves
    /// nothing to continue: the message stays unsent, for the user to recover.
    pub prompt_read: bool,
    /// The turn's run, which a continuation must still find to be the chat's latest.
    pub run_id: Option<String>,
    pub launch_path: Option<PathBuf>,
    pub cwd: Option<String>,
}

impl ExitFacts {
    /// A crash: an end Helm did not ask for, while a turn Claude had started on was open.
    pub fn is_crash(&self) -> bool {
        self.host_reason.is_none() && self.turn_open && self.prompt_read && self.run_id.is_some()
    }
}

/// What the chat should say about an exit, from what is known.
pub fn exit_reason(f: &ExitFacts) -> String {
    if let Some(e) = &f.worker_error {
        return e.clone();
    }
    let code = f
        .code
        .map(|c| format!("exit code {c}"))
        .or(f.signal.map(|s| format!("signal {s}")))
        .unwrap_or_else(|| "no exit code".to_owned());
    if let Some(line) = meaningful_line(&f.stderr) {
        return format!("The worker stopped ({code}): {line}");
    }
    if said_goodbye(&f.stderr) {
        // It ended itself; what is left of its tree may still be closing.
        format!("The worker process ended without saying why ({code}).")
    } else if f.job_active.is_some_and(|n| n > 0) {
        format!(
            "The worker process was ended from outside Helm while Claude Code was still running ({code})."
        )
    } else {
        // Every way the worker ends itself writes its closing line; only a process ended from
        // outside (or one that crashed outright) leaves none.
        format!(
            "The worker process was ended from outside Helm, or crashed without a word ({code})."
        )
    }
}

/// The worker wrote its closing line ("exiting with code N"): it ended itself.
pub fn said_goodbye(stderr: &str) -> bool {
    stderr.contains(GOODBYE)
}

const GOODBYE: &str = "[helm-worker] exiting with code";

/// A crash the outage watch may carry on from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crash {
    pub run_id: String,
    pub at_ms: i64,
    /// Its place among the crashes of its incident (0 for the first), so continuations to the
    /// chats of one incident are spread out rather than sent all at once.
    pub order: u32,
}

/// What recording an exit found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    /// Exits in this exit's incident so far, this one included.
    pub incident_size: usize,
    /// Crashes among them.
    pub incident_crashes: u32,
    /// When the incident's first exit was seen.
    pub incident_at_ms: i64,
    pub crash: bool,
}

#[derive(Default)]
struct Incident {
    first_ms: i64,
    last_ms: i64,
    exits: usize,
    crashes: u32,
}

#[derive(Default)]
struct State {
    incident: Option<Incident>,
    crashes: HashMap<String, Crash>,
}

/// The exit log and what is remembered about recent crashes. One per runtime.
pub struct ExitLog {
    dir: PathBuf,
    state: Mutex<State>,
}

impl ExitLog {
    /// Logging to `<profile>/diagnostics`.
    pub fn new(profile: &Path) -> Self {
        Self {
            dir: profile.join("diagnostics"),
            state: Mutex::new(State::default()),
        }
    }

    /// Record one exit: group it into its incident, remember it if it was a crash, and append
    /// its line (and an `incident` line from the second exit of an incident on).
    pub fn record(&self, f: &ExitFacts, shown: &str) -> Recorded {
        let crash = f.is_crash();
        let (rec, incident_line) = {
            let mut st = self.state.lock().unwrap();
            let joins = st
                .incident
                .as_ref()
                .is_some_and(|i| f.at_ms - i.last_ms <= INCIDENT_GAP_MS);
            if !joins {
                st.incident = Some(Incident {
                    first_ms: f.at_ms,
                    ..Default::default()
                });
            }
            let inc = st.incident.as_mut().expect("set above");
            inc.last_ms = f.at_ms.max(inc.last_ms);
            inc.exits += 1;
            let order = inc.crashes;
            if crash {
                inc.crashes += 1;
            }
            let rec = Recorded {
                incident_size: inc.exits,
                incident_crashes: inc.crashes,
                incident_at_ms: inc.first_ms,
                crash,
            };
            let incident_line = (inc.exits >= 2).then(|| {
                json!({
                    "kind": "incident",
                    "incident": incident_id(inc.first_ms),
                    "time": local_time(f.at_ms, "%Y-%m-%dT%H:%M:%S%.3f%:z"),
                    "exits": inc.exits,
                    "crashes": inc.crashes,
                    "spanMs": inc.last_ms - inc.first_ms,
                })
            });
            match (crash, f.run_id.as_ref()) {
                (true, Some(run)) => {
                    st.crashes.insert(
                        f.chat_id.clone(),
                        Crash {
                            run_id: run.clone(),
                            at_ms: f.at_ms,
                            order,
                        },
                    );
                    prune(&mut st.crashes, f.at_ms);
                }
                // A later end the chat chose (or an idle one) supersedes an earlier crash.
                _ => {
                    st.crashes.remove(&f.chat_id);
                }
            }
            (rec, incident_line)
        };
        let mut line = exit_line(f, shown, &rec);
        line.push('\n');
        if let Some(i) = incident_line {
            line.push_str(&i.to_string());
            line.push('\n');
        }
        if let Err(e) = append(&self.dir, &line) {
            log::warn!("worker exit not logged: {e}");
        }
        log::warn!(
            "chat {}: worker exited ({}): {shown}",
            f.chat_id,
            f.code.map_or("no code".to_owned(), |c| c.to_string())
        );
        rec
    }

    /// The crashes remembered now, by chat.
    pub fn recent_crashes(&self, now_ms: i64) -> HashMap<String, Crash> {
        let mut st = self.state.lock().unwrap();
        prune(&mut st.crashes, now_ms);
        st.crashes.clone()
    }

    /// The chat has been dealt with (carried on, given up on, or the user took over): its crash
    /// is no longer remembered.
    pub fn forget(&self, chat_id: &str) {
        self.state.lock().unwrap().crashes.remove(chat_id);
    }
}

fn prune(crashes: &mut HashMap<String, Crash>, now_ms: i64) {
    crashes.retain(|_, c| now_ms - c.at_ms <= CRASH_KEEP_MS);
    while crashes.len() > MAX_CRASHES {
        let oldest = crashes
            .iter()
            .min_by_key(|(_, c)| c.at_ms)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => crashes.remove(&k),
            None => break,
        };
    }
}

fn incident_id(first_ms: i64) -> String {
    format!("i{first_ms}")
}

fn local_time(ms: i64, fmt: &str) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format(fmt).to_string())
        .unwrap_or_default()
}

/// "One of 4 chats that stopped at 06:38:42", for a crash that is one of several in its incident.
pub fn incident_note(rec: &Recorded) -> Option<String> {
    (rec.crash && rec.incident_crashes >= 2).then(|| {
        format!(
            "One of {} chats that stopped at {}.",
            rec.incident_crashes,
            local_time(rec.incident_at_ms, "%H:%M:%S")
        )
    })
}

fn exit_line(f: &ExitFacts, shown: &str, rec: &Recorded) -> String {
    let tool: Value = f.last_tool.as_ref().map_or(Value::Null, |(name, cmd)| {
        json!({"name": name, "command": cmd.chars().take(COMMAND_KEEP).collect::<String>()})
    });
    json!({
        "kind": "exit",
        "time": local_time(f.at_ms, "%Y-%m-%dT%H:%M:%S%.3f%:z"),
        "at": f.at_ms,
        "incident": incident_id(rec.incident_at_ms),
        "chatId": f.chat_id,
        "code": f.code,
        "signal": f.signal,
        "hostAsked": f.host_reason.is_some(),
        "hostReason": f.host_reason.map(HostReason::as_str),
        "crash": rec.crash,
        "turnOpen": f.turn_open,
        "promptRead": f.prompt_read,
        "workerError": f.worker_error,
        "jobActive": f.job_active,
        "saidGoodbye": said_goodbye(&f.stderr),
        "shown": shown,
        "lastTool": tool,
        "uptimeMs": f.uptime_ms,
        "model": f.model,
        "provider": f.provider,
        "launch": f.launch_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "cwd": f.cwd,
        "stderr": if f.host_reason.is_some() { tail(&f.stderr, ASKED_STDERR_KEEP) } else { &f.stderr },
    })
    .to_string()
}

/// The last `max` bytes of `s`, cut at a character boundary.
fn tail(s: &str, max: usize) -> &str {
    let mut from = s.len().saturating_sub(max);
    while !s.is_char_boundary(from) {
        from += 1;
    }
    &s[from..]
}

fn append(dir: &Path, text: &str) -> std::io::Result<()> {
    helm_core::privfs::create_private_dir_all(dir)?;
    let path = dir.join("worker-exits.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > FILE_CAP) {
        std::fs::rename(&path, dir.join("worker-exits.old.log"))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    // One write, so lines from two chats never interleave.
    f.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shown_line_skips_node_noise_frames_and_blanks() {
        let typeless = "(node:1234) Warning: [MODULE_TYPELESS_PACKAGE_JSON] Warning: file:///C:/p/src/main.ts parsed as an ES module because module syntax was detected; to avoid the performance penalty of syntax detection, add \"type\": \"module\" to C:\\Users\\x\\package.json.\n\
Reparsing as ES module because module syntax was detected. This incurs a performance penalty.\n\
To eliminate this warning, add \"type\": \"module\" to C:\\Users\\x\\package.json.\n\
(Use `node --trace-warnings ...` to show where the warning was created)\n";
        for (stderr, want) in [
            // Only start-up noise: nothing to show.
            (typeless.to_owned(), None),
            ("".to_owned(), None),
            ("\n  \n".to_owned(), None),
            (
                "(node:9) ExperimentalWarning: Type Stripping is an experimental feature\n".to_owned(),
                None,
            ),
            // A real error after the noise wins, its stack frames skipped.
            (
                format!("{typeless}[helm-worker] uncaught exception: TypeError: x is not a function\n    at run (main.ts:3:1)\n    at main.ts:9:1\n[helm-worker] exiting with code 1\n"),
                Some("[helm-worker] uncaught exception: TypeError: x is not a function"),
            ),
            // An error written before the noise is still found.
            (
                format!("FATAL ERROR: Reached heap limit Allocation failed\n{typeless}"),
                Some("FATAL ERROR: Reached heap limit Allocation failed"),
            ),
            ("[helm-worker] claude stdin: write EPIPE\n".to_owned(), Some("[helm-worker] claude stdin: write EPIPE")),
            // An import that fails before the worker's handlers exist: Node's error, an object
            // dump and its version line. The error wins.
            (
                "node:internal/modules/esm/resolve:275\n    throw new ERR_MODULE_NOT_FOUND(\n          ^\n\nError [ERR_MODULE_NOT_FOUND]: Cannot find module 'C:\\p\\x.ts' imported from C:\\p\\main.ts\n    at finalizeResolution (node:internal:1:1)\n{\n  code: 'ERR_MODULE_NOT_FOUND',\n  url: 'file:///C:/p/x.ts'\n}\n\nNode.js v24.14.0\n".to_owned(),
                Some("Error [ERR_MODULE_NOT_FOUND]: Cannot find module 'C:\\p\\x.ts' imported from C:\\p\\main.ts"),
            ),
            // Out of memory: the fatal line, not the native frames after it.
            (
                "<--- JS stacktrace --->\n\nFATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory\n----- Native stack trace -----\n\n 1: 00007FF6A1B2C3D4 node::SetCppgcReference+17979\n13: 00007FF6A1B2C3D4 RtlUserThreadStart+40\n".to_owned(),
                Some("FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory"),
            ),
            // Only Node's own closing lines: nothing to show.
            ("----- Native stack trace -----\n 1: 00007FF6A1B2C3D4 x+1\nNode.js v24.14.0\n".to_owned(), None),
            // An error-looking line beats a plainer one written after it.
            ("RangeError: Invalid array length\ncleaning up\n".to_owned(), Some("RangeError: Invalid array length")),
            // No error-looking line: the last line that says something.
            ("first\nlast\n".to_owned(), Some("last")),
        ] {
            assert_eq!(meaningful_line(&stderr).as_deref(), want, "{stderr}");
        }
    }

    fn facts(chat: &str, at: i64) -> ExitFacts {
        ExitFacts {
            chat_id: chat.into(),
            at_ms: at,
            code: Some(1),
            turn_open: true,
            prompt_read: true,
            run_id: Some(format!("run-{chat}")),
            ..Default::default()
        }
    }

    #[test]
    fn the_reason_says_who_ended_the_worker() {
        let noise = "(node:1) Warning: [MODULE_TYPELESS_PACKAGE_JSON] Warning: x\n";
        let mut f = facts("a", 0);
        f.stderr = noise.into();
        f.job_active = Some(2);
        assert_eq!(
            exit_reason(&f),
            "The worker process was ended from outside Helm while Claude Code was still running (exit code 1)."
        );
        // No closing line from the worker: something ended it.
        f.job_active = Some(0);
        assert_eq!(
            exit_reason(&f),
            "The worker process was ended from outside Helm, or crashed without a word (exit code 1)."
        );
        f.stderr = format!("{noise}[helm-worker] exiting with code 1\n");
        assert_eq!(
            exit_reason(&f),
            "The worker process ended without saying why (exit code 1)."
        );
        // It ended itself while the rest of its tree was still closing: not an outside end.
        f.job_active = Some(2);
        assert_eq!(
            exit_reason(&f),
            "The worker process ended without saying why (exit code 1)."
        );
        f.job_active = None;
        f.code = None;
        f.signal = Some(9);
        assert_eq!(
            exit_reason(&f),
            "The worker process ended without saying why (signal 9)."
        );
        f.stderr = format!("{noise}boom\n");
        assert_eq!(exit_reason(&f), "The worker stopped (signal 9): boom");
        // What the worker said itself comes first.
        f.worker_error = Some("claude exited unexpectedly (code 3)".into());
        assert_eq!(exit_reason(&f), "claude exited unexpectedly (code 3)");
    }

    /// Which exits are crashes the watch may carry on from, and how exits group into incidents.
    #[test]
    fn crashes_and_incidents() {
        let dir = tempfile::tempdir().unwrap();
        let log = ExitLog::new(dir.path());
        let t0 = 1_700_000_000_000;
        // [chat, ms after t0, how it ended, crash?, incident size]
        type Row = (&'static str, i64, fn(&mut ExitFacts), bool, usize);
        let rows: &[Row] = &[
            ("crashed", 0, |_| {}, true, 1),
            (
                "stopped",
                1_000,
                |f| f.host_reason = Some(HostReason::Stop),
                false,
                2,
            ),
            (
                "archived",
                2_000,
                |f| f.host_reason = Some(HostReason::Archive),
                false,
                3,
            ),
            (
                "quit",
                3_000,
                |f| f.host_reason = Some(HostReason::Quit),
                false,
                4,
            ),
            (
                "ended",
                4_000,
                |f| f.host_reason = Some(HostReason::Ended),
                false,
                5,
            ),
            (
                "closed",
                5_000,
                |f| f.host_reason = Some(HostReason::Close),
                false,
                6,
            ),
            ("idle", 9_000, |f| f.turn_open = false, false, 7),
            // Claude had not read the turn's prompt yet: nothing to carry on.
            ("unread", 9_500, |f| f.prompt_read = false, false, 8),
            ("second", 13_000, |_| {}, true, 9),
            // More than 5 s after the last exit: a new incident.
            ("alone", 30_000, |_| {}, true, 1),
        ];
        for (chat, after, how, crash, size) in rows {
            let mut f = facts(chat, t0 + after);
            how(&mut f);
            let rec = log.record(&f, "x");
            assert_eq!((rec.crash, rec.incident_size), (*crash, *size), "{chat}");
        }
        let crashes = log.recent_crashes(t0 + 30_000);
        let mut got: Vec<_> = crashes
            .iter()
            .map(|(k, c)| (k.as_str(), c.order, c.run_id.as_str()))
            .collect();
        got.sort();
        // The second crash of an incident is second in line; a new incident starts over.
        assert_eq!(
            got,
            [
                ("alone", 0, "run-alone"),
                ("crashed", 0, "run-crashed"),
                ("second", 1, "run-second")
            ]
        );
        // A later end the chat chose replaces its crash; forgetting and age clear the rest.
        let mut f = facts("crashed", t0 + 40_000);
        f.host_reason = Some(HostReason::Stop);
        log.record(&f, "x");
        log.forget("second");
        assert_eq!(
            log.recent_crashes(t0 + 40_000).keys().collect::<Vec<_>>(),
            ["alone"]
        );
        assert!(log.recent_crashes(t0 + CRASH_KEEP_MS + 40_000).is_empty());
        // One line per exit, and an incident line from each incident's second exit on.
        let text =
            std::fs::read_to_string(dir.path().join("diagnostics/worker-exits.log")).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let count = |k: &str| lines.iter().filter(|l| l["kind"] == k).count();
        assert_eq!((count("exit"), count("incident")), (11, 8));
        let first = &lines[0];
        assert_eq!(first["chatId"], "crashed");
        assert_eq!(first["crash"], true);
        assert_eq!(first["hostAsked"], false);
        assert_eq!(first["incident"], format!("i{t0}"));
        let last_incident = lines
            .iter()
            .rev()
            .find(|l| l["kind"] == "incident")
            .unwrap();
        assert_eq!(last_incident["exits"], 9);
        assert_eq!(last_incident["crashes"], 2);
        // The chat's note counts the crashes of its incident, and only a crash gets one.
        let rec = |crash, crashes| Recorded {
            incident_size: 9,
            incident_crashes: crashes,
            incident_at_ms: t0,
            crash,
        };
        let note = incident_note(&rec(true, 4)).unwrap();
        assert!(
            note.starts_with("One of 4 chats that stopped at "),
            "{note}"
        );
        assert_eq!(incident_note(&rec(true, 1)), None);
        assert_eq!(incident_note(&rec(false, 4)), None);
    }

    #[test]
    fn the_log_keeps_the_tool_in_flight_short_and_rolls_over() {
        let dir = tempfile::tempdir().unwrap();
        let log = ExitLog::new(dir.path());
        let mut f = facts("a", 0);
        f.last_tool = Some(("Bash".into(), "x".repeat(1000)));
        f.stderr = "e".repeat(16 * 1024);
        log.record(&f, "shown");
        let path = dir.path().join("diagnostics/worker-exits.log");
        let line: Value = serde_json::from_str(
            std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(line["lastTool"]["name"], "Bash");
        assert_eq!(line["lastTool"]["command"].as_str().unwrap().len(), 300);
        // A crash keeps all of its stderr; an exit Helm asked for keeps only the end of it.
        assert_eq!(line["stderr"].as_str().unwrap().len(), 16 * 1024);
        let mut f = facts("b", 50_000);
        f.host_reason = Some(HostReason::Close);
        f.stderr = format!("{}é{}the end", "e".repeat(16 * 1024), "x".repeat(2040));
        log.record(&f, "shown");
        let text = std::fs::read_to_string(&path).unwrap();
        let asked: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        let kept = asked["stderr"].as_str().unwrap();
        assert!(
            kept.ends_with("the end") && kept.len() <= ASKED_STDERR_KEEP,
            "{}",
            kept.len()
        );
        for i in 0..70 {
            let mut f = facts("a", 100_000 * (i + 1));
            f.stderr = "e".repeat(16 * 1024);
            log.record(&f, "shown");
        }
        assert!(dir
            .path()
            .join("diagnostics/worker-exits.old.log")
            .is_file());
        assert!(std::fs::metadata(&path).unwrap().len() <= FILE_CAP + 32 * 1024);
    }
}
