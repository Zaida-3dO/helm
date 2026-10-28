//! How a request reaches GitHub. Production runs the real `gh` executable (`gh api`) under the
//! user's own sign-in, so Helm never sees a token. Debug builds can instead point at a loopback
//! HTTP server (`HELM_TEST_GITHUB_URL`) for end-to-end checks: a fake `gh` would have to be a
//! script shim, which Helm never spawns.
//!
//! `gh` is always started the same way: the real executable, an explicit argument list (no
//! shell), `api --hostname github.com -i`, prompts and update notices off, no console window and a
//! 30 s deadline. A read gets no stdin at all; a write's JSON body is written to stdin, which is
//! then closed, so the command line never carries user text and never hits the Windows length
//! limit. Helm never asks `gh` for its token and never reads its configuration.

use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const DEADLINE: Duration = Duration::from_secs(30);
/// The most of one answer Helm reads.
pub const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;
/// Room for the status line and headers `gh -i` prints before the body.
const HEADER_ALLOWANCE: usize = 64 * 1024;
const MAX_STDERR: usize = 16 * 1024;
pub const API_ORIGIN: &str = "https://api.github.com";
pub const TEST_URL_ENV: &str = "HELM_TEST_GITHUB_URL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// One request: `endpoint` is a REST path without the leading slash (query included, already
/// validated) or `graphql`.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub method: Method,
    pub endpoint: String,
    pub body: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Response {
    pub status: u16,
    /// Lower-cased names.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// `gh` is not installed or could not start: nothing was sent.
    Missing(String),
    /// `gh` ran but is not signed in: nothing was sent.
    SignedOut,
    /// No answer within the deadline.
    Timeout,
    TooLarge,
    /// Anything else: the request may or may not have reached GitHub.
    Failed(String),
}

pub trait Transport: Send + Sync {
    fn send(&self, req: &Request) -> Result<Response, TransportError>;
    /// The origin `Link` paging URLs must point back at.
    fn origin(&self) -> String {
        API_ORIGIN.to_owned()
    }
}

// ---- gh ----------------------------------------------------------------------------------

/// The real `gh`.
pub struct GhCli;

/// The program name: the real executable on Windows, never a `.cmd` or `.bat` shim.
pub const GH_PROGRAM: &str = if cfg!(windows) { "gh.exe" } else { "gh" };

/// The argument list for one request. Nothing in it comes from user text: the endpoint is built
/// from the allowlist, and a body travels on stdin.
pub fn gh_argv(req: &Request) -> Vec<String> {
    let mut argv: Vec<String> = [
        "api",
        "--hostname",
        "github.com",
        "-i",
        "-X",
        req.method.as_str(),
        "-H",
        "Accept: application/vnd.github+json",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    argv.push(req.endpoint.clone());
    if req.body.is_some() {
        argv.push("--input".into());
        argv.push("-".into());
    }
    argv
}

impl Transport for GhCli {
    fn send(&self, req: &Request) -> Result<Response, TransportError> {
        let mut cmd = Command::new(GH_PROGRAM);
        cmd.args(gh_argv(req))
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
            .env("GH_SPINNER_DISABLED", "1")
            .env("NO_COLOR", "1")
            .current_dir(std::env::temp_dir())
            .stdin(if req.body.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // no console window
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| TransportError::Missing(e.to_string()))?;
        if let (Some(mut stdin), Some(body)) = (child.stdin.take(), req.body.as_ref()) {
            let bytes = serde_json::to_vec(body).unwrap_or_default();
            // On a thread, so a child that answers before reading cannot stall us; dropping
            // the pipe closes it.
            std::thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
            });
        }
        let mut so = child.stdout.take().expect("piped");
        let mut se = child.stderr.take().expect("piped");
        let cap = (MAX_RESPONSE_BYTES + HEADER_ALLOWANCE) as u64;
        let t_out = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = so.by_ref().take(cap + 1).read_to_end(&mut b);
            b
        });
        let t_err = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = se.by_ref().take(MAX_STDERR as u64).read_to_end(&mut b);
            b
        });
        let started = Instant::now();
        let mut out_done_at: Option<Instant> = None;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s.code(),
                Ok(None) if started.elapsed() >= DEADLINE => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(TransportError::Timeout);
                }
                Ok(None) => {
                    // stdout closed or over the cap but the child lingers: stop it.
                    if t_out.is_finished()
                        && out_done_at.get_or_insert_with(Instant::now).elapsed()
                            > Duration::from_secs(2)
                    {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    let _ = child.kill();
                    return Err(TransportError::Failed(e.to_string()));
                }
            }
        };
        let out = t_out.join().unwrap_or_default();
        let err = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned();
        if out.len() as u64 > cap {
            return Err(TransportError::TooLarge);
        }
        let out = String::from_utf8_lossy(&out);
        match parse_included(&out) {
            Some(r) => Ok(r),
            None => Err(classify_gh_failure(status, &err)),
        }
    }
}

/// `gh` printed no HTTP answer: say why from its exit code and first stderr line.
pub fn classify_gh_failure(code: Option<i32>, stderr: &str) -> TransportError {
    // gh exits with 4 when it has no sign-in for the host.
    if code == Some(4) || stderr.contains("gh auth login") || stderr.contains("not logged") {
        return TransportError::SignedOut;
    }
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("gh gave no answer");
    TransportError::Failed(crate::clean(first, 200))
}

/// Parse `gh api -i` output: a status line, headers, a blank line, then the body.
pub fn parse_included(out: &str) -> Option<Response> {
    let mut rest = out;
    let mut next_line = || -> Option<&str> {
        if rest.is_empty() {
            return None;
        }
        let (line, tail) = match rest.find('\n') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        rest = tail;
        Some(line.trim_end_matches('\r'))
    };
    let status_line = next_line()?;
    let mut parts = status_line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let status: u16 = parts.next()?.parse().ok()?;
    let mut headers = Vec::new();
    loop {
        match next_line() {
            None => break,
            Some("") => break,
            Some(l) => {
                if let Some((n, v)) = l.split_once(':') {
                    headers.push((n.trim().to_ascii_lowercase(), v.trim().to_owned()));
                }
            }
        }
    }
    Some(Response {
        status,
        headers,
        body: rest.to_owned(),
    })
}

// ---- loopback (debug builds, end-to-end checks) -----------------------------------------------

/// Plain HTTP to a loopback test server: REST at `<base>/<path>`, GraphQL at `POST <base>/graphql`.
/// Built only in debug builds and only for a loopback address.
pub struct Loopback {
    base: String,
    agent: ureq::Agent,
}

impl Loopback {
    /// `None` unless this is a debug build and `url` is `http://` on a loopback address.
    pub fn new(url: &str) -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let base = url.trim().trim_end_matches('/');
        let host = base.strip_prefix("http://")?;
        let host = host.split('/').next()?;
        let name = match host.rsplit_once(':') {
            Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) && !port.is_empty() => h,
            _ => return None,
        };
        if !matches!(name, "127.0.0.1" | "localhost" | "[::1]") || base.len() != 7 + host.len() {
            return None;
        }
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(DEADLINE))
            .user_agent("helm-home")
            .build()
            .new_agent();
        Some(Self {
            base: base.to_owned(),
            agent,
        })
    }

    /// From `HELM_TEST_GITHUB_URL`, in debug builds only.
    pub fn from_env() -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let url = std::env::var(TEST_URL_ENV).ok()?;
        let t = Self::new(&url);
        if t.is_none() {
            log::warn!("{TEST_URL_ENV} is not a loopback http:// address; using gh");
        }
        t
    }
}

impl Transport for Loopback {
    fn send(&self, req: &Request) -> Result<Response, TransportError> {
        let url = format!("{}/{}", self.base, req.endpoint);
        let res = match (&req.method, &req.body) {
            (Method::Get, _) => self
                .agent
                .get(&url)
                .header("Accept", "application/vnd.github+json")
                .call(),
            (Method::Post, body) => {
                let bytes = body
                    .as_ref()
                    .map(|b| serde_json::to_vec(b).unwrap_or_default())
                    .unwrap_or_default();
                self.agent
                    .post(&url)
                    .header("Accept", "application/vnd.github+json")
                    .header("Content-Type", "application/json")
                    .send(&bytes[..])
            }
        };
        let mut res = match res {
            Ok(r) => r,
            Err(ureq::Error::Timeout(_)) => return Err(TransportError::Timeout),
            Err(ureq::Error::ConnectionFailed | ureq::Error::HostNotFound) => {
                return Err(TransportError::Missing(
                    "the test server is not running".into(),
                ))
            }
            Err(e) => return Err(TransportError::Failed(e.to_string())),
        };
        let status = res.status().as_u16();
        let headers = res
            .headers()
            .iter()
            .map(|(n, v)| {
                (
                    n.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let body = res
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES as u64)
            .read_to_string()
            .map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => TransportError::TooLarge,
                other => TransportError::Failed(other.to_string()),
            })?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    fn origin(&self) -> String {
        self.base.clone()
    }
}
