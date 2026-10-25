//! Looking at the things a wait watches. The watcher only ever reads: it never changes a chat, a
//! process or a file, and it never calls a model.
//!
//! Pull requests are read through the GitHub command-line tool (`gh pr view`), under whatever
//! sign-in the user already gave it; Helm holds no GitHub credential of its own. If `gh` is not
//! installed a PR wait is refused when it is declared.

use crate::model::{ChatObs, Check, FileObs, PrObs, ProcessObs};
use helm_core::db::Database;
use rusqlite::OptionalExtension;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long one `gh` call may take.
const GH_TIMEOUT: Duration = Duration::from_secs(20);

/// What the watcher can look at. A test supplies its own.
pub trait Probes: Send + Sync {
    /// `Ok(None)` is a chat that does not exist; an `Err` means "could not look", which is
    /// never treated as the chat being gone.
    fn chat(&self, chat_id: &str) -> Result<Option<ChatObs>, String>;
    fn process(&self, pid: u32) -> ProcessObs;
    fn file(&self, path: &str) -> FileObs;
    /// Blocking; runs on the blocking pool.
    fn pr(&self, url: &str) -> Result<PrObs, String>;
}

pub struct RealProbes {
    pub db: Arc<Database>,
}

impl Probes for RealProbes {
    fn chat(&self, chat_id: &str) -> Result<Option<ChatObs>, String> {
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT archived, status, error,
                            EXISTS(SELECT 1 FROM queued_submissions q WHERE q.chat_id = chats.id)
                         OR EXISTS(SELECT 1 FROM slot_waits s WHERE s.chat_id = chats.id)
                     FROM chats WHERE id = ?1",
                    [chat_id],
                    |r| {
                        let status: String = r.get(1)?;
                        Ok(ChatObs {
                            archived: r.get(0)?,
                            busy: matches!(status.as_str(), "starting" | "running" | "stopping"),
                            stopped: status == "stopped",
                            error: r.get(2)?,
                            queued: r.get(3)?,
                        })
                    },
                )
                .optional()
            })
            .map_err(|e| e.to_string())
    }

    fn process(&self, pid: u32) -> ProcessObs {
        process_state(pid)
    }

    fn file(&self, path: &str) -> FileObs {
        match std::fs::metadata(path) {
            Ok(m) => FileObs {
                exists: true,
                len: m.len(),
                modified_ms: m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            },
            Err(_) => FileObs {
                exists: false,
                len: 0,
                modified_ms: 0,
            },
        }
    }

    fn pr(&self, url: &str) -> Result<PrObs, String> {
        gh_pr_view(url)
    }
}

#[cfg(windows)]
fn process_state(pid: u32) -> ProcessObs {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FILETIME, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: a query-only handle, closed before returning.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            // A process we may not query is still there.
            return if GetLastError() == ERROR_ACCESS_DENIED {
                ProcessObs::Alive(None)
            } else {
                ProcessObs::Gone(None)
            };
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code);
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut x, mut y, mut z) = (zero, zero, zero, zero);
        let timed = GetProcessTimes(h, &mut created, &mut x, &mut y, &mut z) != 0;
        CloseHandle(h);
        if ok == 0 || code == STILL_ACTIVE as u32 {
            let started = timed
                .then_some((created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64);
            ProcessObs::Alive(started)
        } else {
            ProcessObs::Gone(Some(code as i64))
        }
    }
}

#[cfg(not(windows))]
fn process_state(pid: u32) -> ProcessObs {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The start time is field 22; the fields after the parenthesised name begin at field 3.
        Ok(stat) => ProcessObs::Alive(
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(19))
                .and_then(|v| v.parse().ok()),
        ),
        Err(_) => ProcessObs::Gone(None),
    }
}

/// `https://github.com/<owner>/<repo>/pull/<n>`, and nothing else.
pub fn canonical_pr_url(host: &str, repo: &str, number: u64) -> Result<String, String> {
    if !host.eq_ignore_ascii_case("github.com") {
        return Err("Only github.com pull requests can be watched.".into());
    }
    let ok_part = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let mut it = repo.split('/');
    match (it.next(), it.next(), it.next()) {
        (Some(o), Some(n), None) if ok_part(o) && ok_part(n) && number > 0 => {
            Ok(format!("https://github.com/{o}/{n}/pull/{number}"))
        }
        _ => {
            Err("repo must look like owner/name, and number must be a pull request number.".into())
        }
    }
}

/// Read one pull request through `gh`.
fn gh_pr_view(url: &str) -> Result<PrObs, String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args([
        "pr",
        "view",
        url,
        "--json",
        "state,reviewDecision,reviews,statusCheckRollup",
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // no console window
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "The GitHub command-line tool (gh) is not installed.".to_owned()
        } else {
            format!("gh could not start: {e}")
        }
    })?;
    // Read both pipes on threads so a chatty child cannot fill one and stall.
    let read = |p: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = p {
                let _ = std::io::Read::read_to_string(&mut p, &mut s);
            }
            s
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as _));
    let err = read(child.stderr.take().map(|p| Box::new(p) as _));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() > GH_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("gh took too long to answer.".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("gh failed: {e}")),
        }
    };
    let (out, err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    if !status.success() {
        let why = err.lines().next().unwrap_or("gh failed").trim();
        return Err(format!(
            "gh could not read the pull request: {}",
            crate::model::clean(why, 200)
        ));
    }
    parse_pr(&out)
}

/// Read `gh pr view --json state,reviewDecision,reviews,statusCheckRollup`.
pub fn parse_pr(json: &str) -> Result<PrObs, String> {
    let v: Value =
        serde_json::from_str(json).map_err(|_| "gh gave an unreadable answer.".to_owned())?;
    let s = |x: &Value| x.as_str().unwrap_or_default().to_owned();
    let reviews = v["reviews"].as_array().cloned().unwrap_or_default();
    let latest_review = reviews.last().map(|r| {
        (
            s(&r["author"]["login"]),
            s(&r["state"]).to_lowercase().replace('_', " "),
        )
    });
    let checks = v["statusCheckRollup"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| {
                    // A check run has status and conclusion; a commit status has only a state.
                    let (name, done, failed) = if c.get("state").is_some() {
                        let st = s(&c["state"]);
                        (
                            s(&c["context"]),
                            !matches!(st.as_str(), "PENDING" | "EXPECTED" | ""),
                            matches!(st.as_str(), "FAILURE" | "ERROR"),
                        )
                    } else {
                        let conclusion = s(&c["conclusion"]);
                        (
                            s(&c["name"]),
                            s(&c["status"]) == "COMPLETED",
                            matches!(
                                conclusion.as_str(),
                                "FAILURE"
                                    | "TIMED_OUT"
                                    | "CANCELLED"
                                    | "ACTION_REQUIRED"
                                    | "STARTUP_FAILURE"
                            ),
                        )
                    };
                    Check {
                        name: crate::model::clean(&name, 80),
                        done,
                        failed,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(PrObs {
        state: s(&v["state"]),
        review_decision: s(&v["reviewDecision"]),
        review_count: reviews.len(),
        latest_review: latest_review.map(|(a, st)| (crate::model::clean(&a, 60), st)),
        checks,
    })
}
