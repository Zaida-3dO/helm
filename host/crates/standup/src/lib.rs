//! The read-only Agent Standup client.
//!
//! Helm reads board projects from Agent Standup over fixed GET routes and never writes to it in
//! v1. The bearer token Helm holds for it can write, so the guard is on the **method**: the one
//! transport seam, [`Transport::get`], is the only way a request leaves this crate, and a test scans
//! the crate's source for any other request builder. `GET /api/projects` shares its path with the
//! board's create route, so a path allowlist would not be enough.
//!
//! Rules every request follows:
//! - at most 3 attempts, 100 ms then 200 ms apart, retried only when the board is unreachable;
//! - a 15 s deadline across the attempts;
//! - at most 8 requests in flight across the process;
//! - redirects are never followed;
//! - responses are capped at 2 MiB;
//! - the token is never logged, never in an error, and scrubbed from anything the board echoes.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

pub const MAX_ATTEMPTS: usize = 3;
pub const BACKOFF: [Duration; 2] = [Duration::from_millis(100), Duration::from_millis(200)];
pub const DEADLINE: Duration = Duration::from_secs(15);
pub const MAX_IN_FLIGHT: usize = 8;
pub const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
/// Projects per page asked of the board (its maximum), and the most pages one sync reads.
pub const PAGE_LIMIT: usize = 200;
pub const MAX_PAGES: usize = 50;

// ---- token --------------------------------------------------------------------------------

/// The bearer token. It has no `Display`, and its `Debug` prints nothing of the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(s: impl Into<String>) -> Option<Self> {
        let s = s.into().trim().to_owned();
        (!s.is_empty()).then_some(Self(s))
    }

    fn scrub(&self, text: &str) -> String {
        if self.0.len() < 4 {
            return text.to_owned();
        }
        text.replace(&self.0, "[token]")
    }

    /// For the credential store only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(..)")
    }
}

// ---- errors -------------------------------------------------------------------------------

/// What can go wrong, each as a plain sentence for the person reading it (addendum item 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StandupError {
    /// No board address or token is set.
    NotConfigured,
    /// `attach_only`: no connection is up, so nothing was sent.
    NotStarted,
    BadUrl(String),
    Unreachable(String),
    Unauthorized,
    Redirected,
    TooLarge,
    Status(u16),
    BadResponse(String),
}

impl fmt::Display for StandupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("Agent Standup is not set up yet: add its address and token in Settings, Projects."),
            Self::NotStarted => f.write_str("Agent Standup is not connected."),
            Self::BadUrl(m) => write!(f, "That Agent Standup address is not usable: {m}"),
            Self::Unreachable(_) => f.write_str("Couldn't reach Agent Standup; showing the last sync."),
            Self::Unauthorized => f.write_str("Agent Standup refused the token; check it in Settings, Projects."),
            Self::Redirected => f.write_str("Agent Standup answered with a redirect, which Helm does not follow; check the address."),
            Self::TooLarge => f.write_str("Agent Standup sent more than Helm reads at once (2 MiB)."),
            Self::Status(s) => write!(f, "Agent Standup answered with an error ({s}); showing the last sync."),
            Self::BadResponse(m) => write!(f, "Agent Standup sent something Helm could not read: {m}"),
        }
    }
}

impl std::error::Error for StandupError {}

// ---- transport ----------------------------------------------------------------------------

pub struct RawResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug)]
pub enum TransportError {
    /// Connection refused, DNS failure, timeout: the only kind that is retried.
    Unreachable(String),
    TooLarge,
    Other(String),
}

/// The one way a request leaves this crate. There is deliberately no other method.
pub trait Transport: Send + Sync {
    fn get(
        &self,
        url: &str,
        bearer: &str,
        timeout: Duration,
    ) -> Result<RawResponse, TransportError>;
}

/// The real transport: ureq, redirects off, status codes returned rather than raised, body capped.
pub struct HttpTransport {
    agent: ureq::Agent,
}

impl Default for HttpTransport {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(DEADLINE))
            .user_agent("helm-home")
            .build()
            .new_agent();
        Self { agent }
    }
}

impl Transport for HttpTransport {
    fn get(
        &self,
        url: &str,
        bearer: &str,
        timeout: Duration,
    ) -> Result<RawResponse, TransportError> {
        let res = self
            .agent
            .get(url)
            .header("Authorization", &format!("Bearer {bearer}"))
            .header("Accept", "application/json")
            .config()
            .timeout_global(Some(timeout))
            .build()
            .call();
        let mut res = match res {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(s)) => {
                return Ok(RawResponse {
                    status: s,
                    body: String::new(),
                })
            }
            Err(
                e @ (ureq::Error::Io(_)
                | ureq::Error::Timeout(_)
                | ureq::Error::HostNotFound
                | ureq::Error::ConnectionFailed),
            ) => return Err(TransportError::Unreachable(e.to_string())),
            Err(e) => return Err(TransportError::Other(e.to_string())),
        };
        let status = res.status().as_u16();
        let body = res
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => TransportError::TooLarge,
                other => TransportError::Other(other.to_string()),
            })?;
        Ok(RawResponse { status, body })
    }
}

// ---- concurrency limit --------------------------------------------------------------------

struct Permits {
    used: Mutex<usize>,
    freed: Condvar,
}

static PERMITS: Permits = Permits {
    used: Mutex::new(0),
    freed: Condvar::new(),
};

struct Permit;

impl Permit {
    fn take() -> Self {
        let mut used = PERMITS.used.lock().unwrap_or_else(|e| e.into_inner());
        while *used >= MAX_IN_FLIGHT {
            used = PERMITS.freed.wait(used).unwrap_or_else(|e| e.into_inner());
        }
        *used += 1;
        Permit
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut used = PERMITS.used.lock().unwrap_or_else(|e| e.into_inner());
        *used -= 1;
        PERMITS.freed.notify_one();
    }
}

// ---- the client ---------------------------------------------------------------------------

/// The board address in the form Helm stores and compares: `http(s)://host[:port][/base]`, no
/// trailing slash, no credentials, no query.
pub fn normalize_url(raw: &str) -> Result<String, StandupError> {
    let s = raw.trim().trim_end_matches('/');
    let lower = s.to_ascii_lowercase();
    let scheme = if lower.starts_with("https://") {
        "https://"
    } else if lower.starts_with("http://") {
        "http://"
    } else {
        return Err(StandupError::BadUrl(
            "it must start with https:// or http://".into(),
        ));
    };
    let rest = &s[scheme.len()..];
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err(StandupError::BadUrl("it has no host".into()));
    }
    if authority.contains('@') {
        return Err(StandupError::BadUrl(
            "it must not carry a user name or password".into(),
        ));
    }
    if s.contains('?') || s.contains('#') || s.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(StandupError::BadUrl("it must be a plain address".into()));
    }
    Ok(format!("{scheme}{rest}"))
}

/// A board project as `GET /api/projects` returns it (only the fields the sidebar uses).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardProject {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub headline: Option<String>,
    #[serde(default)]
    pub area: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub last_activity: Option<String>,
    /// How many items sit under the project, and how many of those are in a terminal state. A
    /// board that predates these counts leaves both out.
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub finished: Option<u64>,
}

impl BoardProject {
    /// Every piece of work under the project is done: it has at least one item and all of them
    /// are terminal. A project with nothing under it, or a board that sends no counts, is not.
    pub fn completed(&self) -> bool {
        matches!((self.total, self.finished), (Some(t), Some(f)) if t > 0 && f == t)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectsPage {
    projects: Vec<BoardProject>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Token,
    transport: Arc<dyn Transport>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &self.token)
            .finish()
    }
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Client {
    pub fn new(base: &str, token: Token) -> Result<Self, StandupError> {
        Self::with_transport(base, token, Arc::new(HttpTransport::default()))
    }

    pub fn with_transport(
        base: &str,
        token: Token,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, StandupError> {
        Ok(Self {
            base: normalize_url(base)?,
            token,
            transport,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// One GET of `path` (which starts with `/`), under every rule in the module header.
    fn get_json(&self, path: &str) -> Result<Value, StandupError> {
        let url = format!("{}{}", self.base, path);
        let started = Instant::now();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let left = DEADLINE.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(StandupError::Unreachable("deadline".into()));
            }
            let res = {
                let _permit = Permit::take();
                self.transport.get(&url, self.token.expose(), left)
            };
            match res {
                Err(TransportError::Unreachable(m)) => {
                    let m = self.token.scrub(&m);
                    if attempt >= MAX_ATTEMPTS
                        || started.elapsed() + BACKOFF[attempt - 1] >= DEADLINE
                    {
                        log::warn!("Agent Standup unreachable after {attempt} attempts: {m}");
                        return Err(StandupError::Unreachable(m));
                    }
                    std::thread::sleep(BACKOFF[attempt - 1]);
                }
                Err(TransportError::TooLarge) => return Err(StandupError::TooLarge),
                Err(TransportError::Other(m)) => {
                    return Err(StandupError::BadResponse(self.token.scrub(&m)))
                }
                Ok(r) => {
                    return match r.status {
                        200..=299 => serde_json::from_str(&r.body).map_err(|e| {
                            StandupError::BadResponse(self.token.scrub(&e.to_string()))
                        }),
                        300..=399 => Err(StandupError::Redirected),
                        401 | 403 => Err(StandupError::Unauthorized),
                        s => Err(StandupError::Status(s)),
                    }
                }
            }
        }
    }

    /// Every board project, every page, including those whose work is all finished:
    /// `GET /api/projects?includeCompleted=true&limit=200[&cursor=…]`. The flag is false when the
    /// read stopped at [`MAX_PAGES`] with more to come, so the caller must not treat what is
    /// missing as gone.
    pub fn list_projects(&self) -> Result<(Vec<BoardProject>, bool), StandupError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut path = format!("/api/projects?includeCompleted=true&limit={PAGE_LIMIT}");
            if let Some(c) = &cursor {
                path.push_str(&format!("&cursor={}", encode(c)));
            }
            let page: ProjectsPage = serde_json::from_value(self.get_json(&path)?)
                .map_err(|e| StandupError::BadResponse(e.to_string()))?;
            out.extend(page.projects);
            match page.next_cursor {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => return Ok((out, true)),
            }
        }
        Ok((out, false))
    }

    /// Board search for Mod+K: `GET /api/search?q=…&limit=…`.
    /// Returns the board's JSON as is.
    pub fn search(&self, query: &str, limit: usize) -> Result<Value, StandupError> {
        self.get_json(&format!(
            "/api/search?q={}&limit={}",
            encode(query),
            limit.clamp(1, 50)
        ))
    }
}

/// The process's current connection, shared as a service. `attach_only` never starts anything:
/// with no connection up it answers [`StandupError::NotStarted`].
#[derive(Default)]
pub struct BoardLink {
    client: RwLock<Option<Client>>,
}

impl BoardLink {
    pub fn set(&self, client: Option<Client>) {
        *self.client.write().unwrap_or_else(|e| e.into_inner()) = client;
    }

    pub fn attach_only(&self) -> Result<Client, StandupError> {
        self.client
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(StandupError::NotStarted)
    }
}

#[cfg(test)]
mod tests;
