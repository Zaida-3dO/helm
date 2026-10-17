//! SB3: the client is read-only, keeps the token out of errors, and follows the request rules.

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};

const SENTINEL: &str = "sentinel-token-8c1f2e";

fn token() -> Token {
    Token::new(SENTINEL).unwrap()
}

/// A tiny HTTP/1.1 server on a loopback port. `respond(method, path, auth)` gives (status,
/// extra headers, body). Every request's method and path is recorded.
struct FakeServer {
    base: String,
    log: Arc<Mutex<Vec<(String, String, String)>>>,
}

type Responder = dyn Fn(&str, &str, &str) -> (u16, Vec<(String, String)>, String) + Send + Sync;

impl FakeServer {
    fn start(respond: Arc<Responder>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("").to_owned();
                let mut auth = String::new();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                    let lower = h.to_ascii_lowercase();
                    if lower.starts_with("authorization:") {
                        auth = h[14..].trim().to_owned();
                    }
                    if lower.starts_with("content-length:") {
                        len = h[15..].trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                let _ = reader.read_exact(&mut body);
                log2.lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), auth.clone()));
                let (status, headers, body) = respond(&method, &path, &auth);
                let mut out = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (k, v) in headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let _ = stream.write_all(out.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        Self { base, log }
    }

    fn requests(&self) -> Vec<(String, String, String)> {
        self.log.lock().unwrap().clone()
    }
}

fn projects_page(ids: &[&str], next: Option<&str>) -> String {
    let projects: Vec<Value> = ids
        .iter()
        .map(|id| serde_json::json!({"id": id, "title": format!("Title {id}"), "headline": null, "area": "a", "repo": null, "priority": "p2", "lastActivity": "2026-10-17T10:00:00.000Z", "counts": {}}))
        .collect();
    serde_json::json!({"projects": projects, "childlessCount": 0, "nextCursor": next}).to_string()
}

#[test]
fn list_projects_pages_with_get_only_and_sends_the_bearer_token() {
    let server = FakeServer::start(Arc::new(|method, path, _| {
        if method != "GET" {
            return (405, vec![], String::new());
        }
        if path.contains("cursor=p2") {
            (200, vec![], projects_page(&["p3"], None))
        } else {
            (200, vec![], projects_page(&["p1", "p2"], Some("p2")))
        }
    }));
    let c = Client::new(&server.base, token()).unwrap();
    let (got, complete) = c.list_projects().unwrap();
    assert!(complete);
    assert_eq!(
        got.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["p1", "p2", "p3"]
    );
    assert_eq!(
        got[0].last_activity.as_deref(),
        Some("2026-10-17T10:00:00.000Z")
    );
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    for (method, path, auth) in &reqs {
        assert_eq!(method, "GET", "only GET may ever be sent ({path})");
        assert!(
            path.starts_with("/api/projects?includeCompleted=true&limit=200"),
            "{path}"
        );
        assert_eq!(auth, &format!("Bearer {SENTINEL}"));
    }
}

#[test]
fn the_token_never_appears_in_errors_debug_or_logs() {
    // The board echoes the token back in a 500 body and in an unreadable 200 body.
    let server = FakeServer::start(Arc::new(|_, path, auth| {
        if path.contains("search") {
            (200, vec![], format!("not json {auth}"))
        } else {
            (401, vec![], format!("bad token {auth}"))
        }
    }));
    let c = Client::new(&server.base, token()).unwrap();
    let e1 = c.list_projects().unwrap_err();
    let e2 = c.search("x", 5).unwrap_err();
    assert_eq!(e1, StandupError::Unauthorized);
    for text in [
        e1.to_string(),
        format!("{e1:?}"),
        e2.to_string(),
        format!("{e2:?}"),
        format!("{c:?}"),
        format!("{:?}", token()),
    ] {
        assert!(!text.contains(SENTINEL), "token leaked into: {text}");
    }
    // A transport error that quotes the token is scrubbed too.
    struct Echo;
    impl Transport for Echo {
        fn get(&self, _u: &str, bearer: &str, _t: Duration) -> Result<RawResponse, TransportError> {
            Err(TransportError::Other(format!(
                "rejected header Bearer {bearer}"
            )))
        }
    }
    let c = Client::with_transport("https://board.example", token(), Arc::new(Echo)).unwrap();
    let e = c.list_projects().unwrap_err();
    assert!(!format!("{e:?}").contains(SENTINEL), "{e:?}");
}

struct Counting {
    calls: AtomicUsize,
    unreachable: bool,
}

impl Transport for Counting {
    fn get(&self, _u: &str, _b: &str, _t: Duration) -> Result<RawResponse, TransportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.unreachable {
            Err(TransportError::Unreachable("connection refused".into()))
        } else {
            Ok(RawResponse {
                status: 500,
                body: String::new(),
            })
        }
    }
}

#[test]
fn only_unreachable_is_retried_and_at_most_three_times() {
    let t = Arc::new(Counting {
        calls: AtomicUsize::new(0),
        unreachable: true,
    });
    let c = Client::with_transport("https://board.example", token(), t.clone()).unwrap();
    let started = Instant::now();
    let e = c.list_projects().unwrap_err();
    assert!(matches!(e, StandupError::Unreachable(_)));
    assert_eq!(
        e.to_string(),
        "Couldn't reach Agent Standup; showing the last sync."
    );
    assert_eq!(t.calls.load(Ordering::SeqCst), 3);
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "100 ms then 200 ms backoff"
    );
    let t = Arc::new(Counting {
        calls: AtomicUsize::new(0),
        unreachable: false,
    });
    let c = Client::with_transport("https://board.example", token(), t.clone()).unwrap();
    assert_eq!(c.list_projects().unwrap_err(), StandupError::Status(500));
    assert_eq!(
        t.calls.load(Ordering::SeqCst),
        1,
        "a server error is not retried"
    );
}

#[test]
fn redirects_are_never_followed() {
    let server = FakeServer::start(Arc::new(|_, path, _| {
        if path.starts_with("/elsewhere") {
            (200, vec![], projects_page(&["x"], None))
        } else {
            (
                302,
                vec![("Location".into(), "/elsewhere".into())],
                String::new(),
            )
        }
    }));
    let c = Client::new(&server.base, token()).unwrap();
    assert_eq!(c.list_projects().unwrap_err(), StandupError::Redirected);
    assert_eq!(
        server.requests().len(),
        1,
        "the redirect target was never requested"
    );
}

#[test]
fn responses_are_capped_at_2_mib() {
    let big = "x".repeat(MAX_RESPONSE_BYTES as usize + 10);
    let server = FakeServer::start(Arc::new(move |_, _, _| (200, vec![], big.clone())));
    let c = Client::new(&server.base, token()).unwrap();
    assert_eq!(c.list_projects().unwrap_err(), StandupError::TooLarge);
}

#[test]
fn addresses_are_normalised_and_credentials_in_them_refused() {
    assert_eq!(
        normalize_url(" HTTPS://board.example/ ").unwrap(),
        "https://board.example"
    );
    assert_eq!(
        normalize_url("http://127.0.0.1:4100/base/").unwrap(),
        "http://127.0.0.1:4100/base"
    );
    for bad in [
        "",
        "board.example",
        "ftp://board.example",
        "https://",
        "https://u:p@board.example",
        "https://board.example/?x=1",
    ] {
        assert!(normalize_url(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn attach_only_answers_not_started_without_a_connection() {
    let link = BoardLink::default();
    assert_eq!(link.attach_only().unwrap_err(), StandupError::NotStarted);
    link.set(Some(Client::new("https://board.example", token()).unwrap()));
    assert_eq!(link.attach_only().unwrap().base(), "https://board.example");
}

/// SB3 "no write method exists": the transport trait has one method, `get`, and no source file
/// in this crate builds a request any other way. The token can write, so this is load-bearing.
#[test]
fn the_client_has_no_write_method() {
    let src = concat!(include_str!("lib.rs"), include_str!("tests.rs"));
    let lib = include_str!("lib.rs");
    // The one request builder in the crate.
    assert_eq!(
        lib.matches(".agent\n            .get(").count() + lib.matches("self.agent.get(").count(),
        1
    );
    for forbidden in [
        ".post(",
        ".put(",
        ".patch(",
        ".delete(",
        ".head(",
        ".options(",
        ".request(",
        "ureq::post",
        "ureq::put",
        "ureq::patch",
        "ureq::delete",
        "ureq::request",
        "Method::",
        "fn post",
        "fn put",
        "fn patch",
        "fn delete",
        "fn create",
        "fn update",
        "fn write",
    ] {
        // Only this test's own list may mention them.
        assert_eq!(
            lib.matches(forbidden).count(),
            0,
            "lib.rs must not contain {forbidden:?}"
        );
    }
    assert!(src.contains("fn get(&self, url: &str, bearer: &str, timeout: Duration)"));
}

static CAPTURED: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        CAPTURED.lock().unwrap().push(record.args().to_string());
    }
    fn flush(&self) {}
}

#[test]
fn the_captured_log_never_holds_the_token() {
    let _ = log::set_logger(&Capture).map(|()| log::set_max_level(log::LevelFilter::Trace));
    struct EchoUnreachable;
    impl Transport for EchoUnreachable {
        fn get(&self, _u: &str, bearer: &str, _t: Duration) -> Result<RawResponse, TransportError> {
            Err(TransportError::Unreachable(format!(
                "proxy refused Bearer {bearer}"
            )))
        }
    }
    let c = Client::with_transport("https://board.example", token(), Arc::new(EchoUnreachable))
        .unwrap();
    let e = c.list_projects().unwrap_err();
    let logs = CAPTURED.lock().unwrap().join("\n");
    assert!(
        logs.contains("unreachable"),
        "the failure was logged: {logs}"
    );
    assert!(!logs.contains(SENTINEL), "token in the log: {logs}");
    assert!(!format!("{e:?}").contains(SENTINEL));
}
