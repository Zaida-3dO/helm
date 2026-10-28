//! The client: at most 4 requests in flight, the rate-limit pause, and turning GitHub's answers
//! into `{status, rateLimit, nextPage?, data, errors?}` or a classed error. Nothing is retried:
//! a read the user wants again is one click away, and a write is sent once.

use crate::error::{Class, GhError, GhResult};
use crate::transport::{Method, Request, Response, Transport, TransportError};
use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub const MAX_IN_FLIGHT: usize = 4;
pub const PAUSE_MIN_MS: i64 = 1_000;
pub const PAUSE_MAX_MS: i64 = 60 * 60 * 1_000;
const PAUSE_DEFAULT_MS: i64 = 60 * 1_000;
/// How long a request waits for one of the 4 slots before giving up.
const SLOT_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    pub limit: u64,
    pub remaining: u64,
    pub used: u64,
    /// Milliseconds since the epoch.
    pub reset_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub status: u16,
    pub rate_limit: Option<RateLimit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page: Option<u32>,
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Value>,
}

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct Client {
    transport: Arc<dyn Transport>,
    now: Clock,
    in_flight: Mutex<usize>,
    freed: Condvar,
    /// github.com is the only host, so one pause (ms since the epoch).
    paused_until: Mutex<Option<i64>>,
}

/// What a request is for, which decides how a failure is classed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Read,
    Write,
}

impl Client {
    pub fn new(transport: Arc<dyn Transport>, now: Clock) -> Self {
        Self {
            transport,
            now,
            in_flight: Mutex::new(0),
            freed: Condvar::new(),
            paused_until: Mutex::new(None),
        }
    }

    /// Forget the rate-limit pause (the view was turned off).
    pub fn clear_pause(&self) {
        *self.paused_until.lock().unwrap() = None;
    }

    pub fn paused_until(&self) -> Option<i64> {
        let now = (self.now)();
        self.paused_until
            .lock()
            .unwrap()
            .filter(|until| *until > now)
    }

    fn acquire(&self) -> GhResult<SlotGuard<'_>> {
        let mut n = self.in_flight.lock().unwrap();
        let start = std::time::Instant::now();
        while *n >= MAX_IN_FLIGHT {
            let left = SLOT_WAIT.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Err(GhError::new(
                    Class::Network,
                    "Too many GitHub requests are waiting; try again in a moment.",
                ));
            }
            n = self.freed.wait_timeout(n, left).unwrap().0;
        }
        *n += 1;
        Ok(SlotGuard(self))
    }

    fn rate_limited(&self, until: i64) -> GhError {
        GhError::new(
            Class::RateLimited,
            format!(
                "GitHub's rate limit is reached; try again {}.",
                wait_text(until - (self.now)())
            ),
        )
    }

    fn send(&self, req: &Request, kind: Kind) -> GhResult<Response> {
        if let Some(until) = self.paused_until() {
            return Err(self.rate_limited(until));
        }
        let _slot = self.acquire()?;
        let res = self.transport.send(req).map_err(|e| match e {
            TransportError::Missing(_) => GhError::new(
                Class::Unavailable,
                "The GitHub command-line tool (gh) is not installed or could not start. Install it, sign in to it in a terminal, then try again.",
            ),
            TransportError::SignedOut => GhError::new(
                Class::Unauthorized,
                "The GitHub command-line tool (gh) is not signed in to github.com. Sign in to it in a terminal, then try again.",
            ),
            TransportError::TooLarge if kind == Kind::Read => {
                GhError::new(Class::TooLarge, "GitHub sent more than Helm reads at once (10 MiB).")
            }
            TransportError::Timeout if kind == Kind::Read => {
                GhError::new(Class::Network, "GitHub did not answer within 30 seconds.")
            }
            TransportError::Failed(why) if kind == Kind::Read => {
                GhError::new(Class::Network, format!("GitHub could not be reached: {why}"))
            }
            _ => uncertain(),
        })?;
        self.note_rate_limit(&res)?;
        if res.status == 401 {
            return Err(GhError::new(
                Class::Unauthorized,
                "GitHub refused the GitHub command-line tool's sign-in. Sign in to it again in a terminal, then try again.",
            ));
        }
        Ok(res)
    }

    /// Set the pause on a rate-limit answer and fail it.
    fn note_rate_limit(&self, res: &Response) -> GhResult<()> {
        let limited_status = matches!(res.status, 403 | 429);
        let remaining_zero = res.header("x-ratelimit-remaining") == Some("0");
        let retry_after = res
            .header("retry-after")
            .and_then(|v| v.trim().parse::<i64>().ok());
        let graphql_limited =
            res.status == 200 && remaining_zero && res.body.contains("\"RATE_LIMITED\"");
        if !(limited_status && (remaining_zero || retry_after.is_some()) || graphql_limited) {
            return Ok(());
        }
        let now = (self.now)();
        let wait = match (retry_after, reset_ms(res)) {
            (Some(s), _) => s.saturating_mul(1000),
            (None, Some(reset)) if remaining_zero => reset - now,
            _ => PAUSE_DEFAULT_MS,
        }
        .clamp(PAUSE_MIN_MS, PAUSE_MAX_MS);
        let until = now + wait;
        *self.paused_until.lock().unwrap() = Some(until);
        Err(self.rate_limited(until))
    }

    /// A read: anything GitHub answered (other than a sign-in or rate-limit refusal) is returned
    /// with its status for the page to word.
    pub fn read(&self, req: &Request) -> GhResult<Answer> {
        let res = self.send(req, Kind::Read)?;
        let data = parse_body(&res.body).ok_or_else(|| {
            GhError::new(Class::GitHub, "GitHub sent an answer Helm could not read.")
        })?;
        let next_page =
            crate::rest::next_page(res.header("link"), &self.transport.origin(), &req.endpoint);
        let mut answer = Answer {
            status: res.status,
            rate_limit: rate_limit(&res),
            next_page,
            data,
            errors: None,
        };
        if req.endpoint == "graphql" {
            graphql_split(&mut answer)?;
        }
        Ok(answer)
    }

    /// A write, sent once. A 2xx is the answer; a 4xx is `[Refused]` (nothing written); anything
    /// else is `[Uncertain]`.
    pub fn write(&self, req: &Request) -> GhResult<Answer> {
        debug_assert_eq!(req.method, Method::Post);
        let res = self.send(req, Kind::Write)?;
        let data = parse_body(&res.body);
        if (400..500).contains(&res.status) {
            return Err(GhError::new(
                Class::Refused,
                format!(
                    "GitHub refused it ({}){}",
                    res.status,
                    data.as_ref()
                        .map(github_message)
                        .filter(|m| !m.is_empty())
                        .map(|m| format!(": {m}"))
                        .unwrap_or_else(|| ".".into())
                ),
            ));
        }
        if !(200..300).contains(&res.status) {
            return Err(uncertain());
        }
        Ok(Answer {
            status: res.status,
            rate_limit: rate_limit(&res),
            next_page: None,
            data: data.unwrap_or(Value::Null),
            errors: None,
        })
    }

    /// A GraphQL mutation, sent once. GraphQL errors with no result for `field` are `[Refused]`.
    pub fn mutate(&self, req: &Request, field: &str) -> GhResult<Answer> {
        let mut a = self.write(req)?;
        let errors = a.data.get("errors").filter(|e| !e.is_null()).cloned();
        let result = a
            .data
            .get("data")
            .and_then(|d| d.get(field))
            .filter(|r| !r.is_null())
            .cloned();
        match (result, errors) {
            (Some(r), errors) => {
                a.data = r;
                a.errors = errors;
                Ok(a)
            }
            (None, Some(e)) => Err(GhError::new(
                Class::Refused,
                format!("GitHub refused it: {}", graphql_messages(&e)),
            )),
            (None, None) => Err(uncertain()),
        }
    }
}

struct SlotGuard<'a>(&'a Client);

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        let mut n = self.0.in_flight.lock().unwrap();
        *n -= 1;
        self.0.freed.notify_one();
    }
}

fn uncertain() -> GhError {
    GhError::new(
        Class::Uncertain,
        "GitHub did not confirm it; it may have been sent. Refresh to check.",
    )
}

/// An empty body reads as null.
fn parse_body(body: &str) -> Option<Value> {
    if body.trim().is_empty() {
        return Some(Value::Null);
    }
    serde_json::from_str(body).ok()
}

fn reset_ms(res: &Response) -> Option<i64> {
    res.header("x-ratelimit-reset")?
        .trim()
        .parse::<i64>()
        .ok()
        .map(|s| s.saturating_mul(1000))
}

fn rate_limit(res: &Response) -> Option<RateLimit> {
    let n = |h: &str| res.header(h)?.trim().parse::<u64>().ok();
    Some(RateLimit {
        limit: n("x-ratelimit-limit")?,
        remaining: n("x-ratelimit-remaining")?,
        used: n("x-ratelimit-used").unwrap_or(0),
        reset_at: reset_ms(res)?,
    })
}

/// GitHub's own `message` (and the first validation error), cleaned and bounded.
fn github_message(v: &Value) -> String {
    let mut m = v["message"].as_str().unwrap_or_default().to_owned();
    if let Some(e) = v["errors"].as_array().and_then(|a| a.first()) {
        let detail = e["message"]
            .as_str()
            .or_else(|| e.as_str())
            .unwrap_or_default();
        if !detail.is_empty() {
            m = format!("{m} ({detail})");
        }
    }
    crate::clean(&m, 300)
}

/// The first three GraphQL error messages.
fn graphql_messages(errors: &Value) -> String {
    let msgs: Vec<String> = errors
        .as_array()
        .map(|a| {
            a.iter()
                .take(3)
                .filter_map(|e| e["message"].as_str())
                .map(|m| crate::clean(m, 200))
                .collect()
        })
        .unwrap_or_default();
    if msgs.is_empty() {
        "an unreadable error".into()
    } else {
        msgs.join("; ")
    }
}

/// A GraphQL answer: `data` and `errors` side by side; errors with no data at all fail.
fn graphql_split(a: &mut Answer) -> GhResult<()> {
    if a.status != 200 {
        return Ok(());
    }
    let body = std::mem::take(&mut a.data);
    let errors = body.get("errors").filter(|e| !e.is_null()).cloned();
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    if data.is_null() {
        if let Some(e) = &errors {
            return Err(GhError::new(Class::GitHub, graphql_messages(e)));
        }
    }
    a.data = data;
    a.errors = errors;
    Ok(())
}

/// "in 40 seconds", "in 12 minutes".
fn wait_text(ms: i64) -> String {
    let secs = (ms + 999) / 1000;
    if secs < 90 {
        format!(
            "in {} second{}",
            secs.max(1),
            if secs == 1 { "" } else { "s" }
        )
    } else {
        format!("in {} minutes", (secs + 59) / 60)
    }
}
