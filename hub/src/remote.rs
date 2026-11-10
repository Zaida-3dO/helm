//! The Remote page: a small phone-and-laptop page served by the hub, and the one call it makes.
//!
//! - `GET /remote`, `/remote/app.js`, `/remote/app.css`: the page, three fixed files built into
//!   the hub. They hold no secret and are open, because a browser cannot send a header when it
//!   opens a page; the page asks for its token and keeps it only for the browser tab.
//! - `POST /remote/api/call` with `{"name": "...", "args": {...}, "machine": "..."}`: one call to
//!   Helm down a relay line of the token's own person, with a `remote`-scope token. Only the Helm
//!   tools the hub already forwards, plus the page's own two (`app_list_waiting`,
//!   `app_answer_waiting`), are accepted; Helm checks every call again on the PC, and the hub
//!   stamps each one `"scope": "remote"` so that the PC can tell the page from any other caller.
//!   `machine` picks one of the person's computers (default: the most recently active one); the
//!   answer lists the computers that are connected. The answer is always 200 with `ok`:
//!   `{ok: true, result}`, or `{ok: false, error, offline?, unknown?}`; `line` says whether the
//!   chosen line is up now.
//!
//! The hub keeps nothing about chats. A token is never put in a URL, a log line or a response, the
//! page's responses carry a strict content policy (no inline script, nothing from elsewhere), and
//! each token has its own allowance of calls a minute: [`ACTIONS_PER_MINUTE`] that change
//! something and a larger [`READS_PER_MINUTE`] for the page's reads, so the page looking at Helm
//! every few seconds never uses up what an answer or a message needs.
//!
//! The hub sits behind a reverse proxy in production, so nothing here depends on the address a
//! request came from: the token is the only credential.

use crate::apptools;
use crate::auth::Scope;
use crate::relay::{CallOutcome, OFFLINE};
use crate::server::{client_of, refused, unauthorized, AppState};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PAGE_HTML: &str = include_str!("assets/remote.html");
pub const PAGE_JS: &str = include_str!("assets/remote.js");
pub const PAGE_CSS: &str = include_str!("assets/remote.css");

/// Calls that change something (a message, a stop, an answer) one token may make in a minute.
pub const ACTIONS_PER_MINUTE: usize = 30;
/// Reads (the chat list, a transcript, what waits) one token may make in a minute. The page reads
/// a few times every few seconds, so this is well above what it needs.
pub const READS_PER_MINUTE: usize = 240;
/// The largest request body (a typed answer or message is far smaller).
pub const MAX_BODY: usize = 64 * 1024;

/// The calls that change something in Helm; every other tool the page may call is a read.
const ACTIONS: &[&str] = &["app_send_message", "app_stop_chat", "app_answer_waiting"];

/// Nothing in the page loads from elsewhere or runs inline, and it cannot be framed.
const POLICY: &str =
    "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
    img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Which allowance a call counts against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Action,
    Read,
}

impl Kind {
    pub fn of(tool: &str) -> Kind {
        if ACTIONS.contains(&tool) {
            Kind::Action
        } else {
            Kind::Read
        }
    }

    fn per_minute(self) -> usize {
        match self {
            Kind::Action => ACTIONS_PER_MINUTE,
            Kind::Read => READS_PER_MINUTE,
        }
    }
}

/// Calls made by each client (registry id) in the last minute.
#[derive(Default)]
pub struct Limiter(Mutex<HashMap<(i64, Kind), VecDeque<Instant>>>);

impl Limiter {
    /// Take one call, or the seconds to wait when the client already made its allowance of this
    /// kind in the last minute.
    pub fn take(&self, client: i64, kind: Kind, now: Instant) -> Result<(), u64> {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let calls = m.entry((client, kind)).or_default();
        while calls
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            calls.pop_front();
        }
        if calls.len() >= kind.per_minute() {
            let wait = calls
                .front()
                .map(|t| 60u64.saturating_sub(now.duration_since(*t).as_secs()))
                .unwrap_or(1);
            return Err(wait.max(1));
        }
        calls.push_back(now);
        Ok(())
    }
}

/// Headers every Remote response carries.
fn harden(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    for (name, value) in [
        (header::CONTENT_SECURITY_POLICY, POLICY),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CACHE_CONTROL, "no-store"),
        (HeaderName::from_static("x-frame-options"), "DENY"),
    ] {
        h.insert(name, HeaderValue::from_static(value));
    }
    resp
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    harden(([(header::CONTENT_TYPE, content_type)], body).into_response())
}

pub async fn page() -> Response {
    asset(PAGE_HTML, "text/html; charset=utf-8")
}

pub async fn script() -> Response {
    asset(PAGE_JS, "text/javascript; charset=utf-8")
}

pub async fn style() -> Response {
    asset(PAGE_CSS, "text/css; charset=utf-8")
}

fn answer(body: Value) -> Response {
    harden((StatusCode::OK, axum::Json(body)).into_response())
}

/// What a call asks for: the tool, its arguments and, optionally, which computer.
struct Asked {
    name: String,
    args: Value,
    machine: Option<String>,
}

fn parse(body: &[u8]) -> Option<Asked> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let name = v.get("name")?.as_str()?.to_owned();
    let args = v.get("args").cloned().unwrap_or_else(|| json!({}));
    let machine = match v.get("machine") {
        None | Some(Value::Null) => None,
        Some(m) => Some(
            m.as_str()
                .filter(|m| !m.is_empty() && m.len() <= 128)?
                .to_owned(),
        ),
    };
    Some(Asked {
        name,
        args,
        machine,
    })
}

/// `POST /remote/api/call`.
pub async fn call(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(client) = client_of(&state, &headers) else {
        return harden(unauthorized());
    };
    if !client.has(Scope::Remote) {
        return harden(refused(
            StatusCode::FORBIDDEN,
            "only a remote token may use the Remote page",
        ));
    }
    if body.len() > MAX_BODY {
        return harden(refused(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the request is too large",
        ));
    }
    let Some(asked) = parse(&body) else {
        return harden(refused(
            StatusCode::BAD_REQUEST,
            "the body must be {\"name\": a tool name, \"args\": an object, \"machine\": optional}",
        ));
    };
    if !apptools::is_remote_callable(&asked.name) {
        return harden(refused(
            StatusCode::FORBIDDEN,
            "that tool is not available from the Remote page",
        ));
    }
    if !(asked.args.is_object() || asked.args.is_null()) {
        return harden(refused(StatusCode::BAD_REQUEST, "args must be an object"));
    }
    if let Err(wait) = state
        .remote
        .take(client.id, Kind::of(&asked.name), Instant::now())
    {
        let mut resp = refused(
            StatusCode::TOO_MANY_REQUESTS,
            "too many calls; wait a moment",
        );
        if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return harden(resp);
    }
    let args = if asked.args.is_null() {
        json!({})
    } else {
        asked.args
    };
    // Only the token's own person's computers: the same rule as every other route.
    let mut machines: Vec<String> = state
        .lines
        .up_lines_of(client.person_id)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    machines.sort();
    let started = Instant::now();
    let (line, outcome) = match state.lines.pick(client.person_id, asked.machine.as_deref()) {
        Some(line) => {
            let outcome = line.call_remote(&client.name, &asked.name, &args).await;
            (line.is_up(), outcome)
        }
        None => (false, CallOutcome::Offline),
    };
    // The tool name and the time, never the arguments (a message or an answer may be in them).
    eprintln!(
        "{} remote/call {} {}ms",
        client.name,
        asked.name,
        started.elapsed().as_millis()
    );
    let mut out = match outcome {
        CallOutcome::Ok(result) => json!({ "ok": true, "result": result }),
        CallOutcome::Err { text, changed } => json!({
            "ok": false,
            "error": if changed { text } else { format!("{text} Nothing was changed.") },
        }),
        CallOutcome::Offline => json!({ "ok": false, "error": OFFLINE, "offline": true }),
        CallOutcome::Unknown(text) => json!({ "ok": false, "error": text, "unknown": true }),
    };
    out["line"] = json!(line);
    out["machines"] = json!(machines);
    answer(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_and_reads_have_their_own_allowances_per_token() {
        let l = Limiter::default();
        let t0 = Instant::now();
        // Reads never use up what an answer or a message needs.
        for _ in 0..READS_PER_MINUTE {
            assert!(l.take(1, Kind::Read, t0).is_ok());
        }
        assert!((1..=60).contains(&l.take(1, Kind::Read, t0).unwrap_err()));
        for _ in 0..ACTIONS_PER_MINUTE {
            assert!(l.take(1, Kind::Action, t0).is_ok());
        }
        let wait = l.take(1, Kind::Action, t0).unwrap_err();
        assert!((1..=60).contains(&wait), "{wait}");
        assert!(l.take(2, Kind::Action, t0).is_ok(), "per token");
        assert!(l
            .take(1, Kind::Action, t0 + Duration::from_secs(61))
            .is_ok());
        // Five minutes of the page's polling (two reads every five seconds, a minute at a time)
        // is far below the allowance.
        let l = Limiter::default();
        for step in 0..60 {
            let at = t0 + Duration::from_secs(step * 5);
            assert!(l.take(1, Kind::Read, at).is_ok());
            assert!(l.take(1, Kind::Read, at).is_ok());
        }
        assert!(l
            .take(1, Kind::Action, t0 + Duration::from_secs(300))
            .is_ok());
    }

    #[test]
    fn only_the_calls_that_change_something_are_actions() {
        for (tool, kind) in [
            ("app_send_message", Kind::Action),
            ("app_stop_chat", Kind::Action),
            ("app_answer_waiting", Kind::Action),
            ("app_list_chats", Kind::Read),
            ("app_read_chat", Kind::Read),
            ("app_list_waiting", Kind::Read),
            ("app_list_attention", Kind::Read),
            ("app_list_projects", Kind::Read),
        ] {
            assert_eq!(Kind::of(tool), kind, "{tool}");
        }
        for t in ACTIONS {
            assert!(apptools::is_remote_callable(t), "{t}");
        }
    }

    #[test]
    fn a_call_is_read_from_its_name_arguments_and_machine() {
        let ok = |b: &str| parse(b.as_bytes()).map(|a| (a.name, a.args, a.machine));
        assert_eq!(
            ok(r#"{"name":"app_list_chats"}"#),
            Some(("app_list_chats".into(), json!({}), None))
        );
        assert_eq!(
            ok(r#"{"name":"app_stop_chat","args":{"chatId":"c"},"machine":"Desk"}"#),
            Some((
                "app_stop_chat".into(),
                json!({"chatId": "c"}),
                Some("Desk".into())
            ))
        );
        for bad in [
            "",
            "[]",
            r#"{"args":{}}"#,
            r#"{"name":3}"#,
            r#"{"name":"x","machine":""}"#,
            r#"{"name":"x","machine":7}"#,
        ] {
            assert!(ok(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_page_loads_nothing_from_elsewhere_and_runs_nothing_inline() {
        // The policy forbids inline script and style, so the page must not need either.
        assert!(!PAGE_HTML.contains("<script>") && !PAGE_HTML.contains(" style="));
        assert!(PAGE_HTML.contains("src=\"/remote/app.js\""));
        for (name, text) in [("html", PAGE_HTML), ("js", PAGE_JS), ("css", PAGE_CSS)] {
            assert!(
                !text.contains("http://") && !text.contains("https://"),
                "{name}"
            );
            assert!(!text.contains("@import"), "{name}");
        }
        // Chat text is only ever shown as text.
        assert!(!PAGE_JS.contains("innerHTML") && !PAGE_JS.contains("insertAdjacentHTML"));
        assert!(
            !PAGE_JS.contains("localStorage"),
            "the token stays in the tab"
        );
    }
}
