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
fn a_project_is_completed_only_when_it_has_work_and_all_of_it_is_finished() {
    let read = |extra: Value| -> BoardProject {
        let mut v = serde_json::json!({"id": "p", "title": "T"});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    };
    let cases = [
        (serde_json::json!({"total": 3, "finished": 3}), true),
        (serde_json::json!({"total": 1, "finished": 1}), true),
        (serde_json::json!({"total": 3, "finished": 2}), false),
        // Childless: nothing under it, so nothing is finished.
        (serde_json::json!({"total": 0, "finished": 0}), false),
        // An older board sends no counts.
        (serde_json::json!({}), false),
        (serde_json::json!({"total": 2}), false),
        (serde_json::json!({"finished": 2}), false),
        (serde_json::json!({"total": null, "finished": null}), false),
    ];
    for (extra, want) in cases {
        let p = read(extra.clone());
        assert_eq!(p.completed(), want, "{extra}");
    }
    // The counts come through a real page read, beside the fields the sidebar already used.
    let server = FakeServer::start(Arc::new(|_, _, _| {
        let body = serde_json::json!({"projects": [
            {"id": "done", "title": "Done", "total": 4, "finished": 4, "merged": 4, "childless": false},
            {"id": "live", "title": "Live", "total": 4, "finished": 1},
            {"id": "old", "title": "Old"},
        ], "nextCursor": null});
        (200, vec![], body.to_string())
    }));
    let (got, _) = Client::new(&server.base, token())
        .unwrap()
        .list_projects()
        .unwrap();
    assert_eq!(
        got.iter()
            .map(|p| (p.id.as_str(), p.completed()))
            .collect::<Vec<_>>(),
        [("done", true), ("live", false), ("old", false)]
    );
    assert_eq!((got[1].total, got[1].finished), (Some(4), Some(1)));
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
    let reads = include_str!("reads.rs");
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
        // Only this test's own list may mention them. The reads module is held to the same rule.
        for (name, text) in [("lib.rs", lib), ("reads.rs", reads)] {
            assert_eq!(
                text.matches(forbidden).count(),
                0,
                "{name} must not contain {forbidden:?}"
            );
        }
    }
    // The reads module builds paths and calls `get_json`; it never makes a request itself.
    for own in ["ureq", "agent", "ransport"] {
        assert_eq!(
            reads.matches(own).count(),
            0,
            "reads.rs must not touch {own:?}"
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

/// The Tasks view's reads: each one fixed GET route, parameters checked before anything is sent.
#[test]
fn the_tasks_reads_use_fixed_routes_and_check_their_parameters() {
    let server = FakeServer::start(Arc::new(|_, path, _| match path {
        p if p.starts_with("/api/fleet") && p.contains("cursor=a2") => (
            200,
            vec![],
            r#"{"assignments":[{"id":"a3"}],"nextCursor":null}"#.into(),
        ),
        p if p.starts_with("/api/fleet") => (
            200,
            vec![],
            r#"{"assignments":[{"id":"a1"},{"id":"a2"}],"nextCursor":"a2"}"#.into(),
        ),
        "/api/ready" => (503, vec![], String::new()),
        _ => (200, vec![], r#"{"ok":true}"#.into()),
    }));
    let c = Client::new(&server.base, token()).unwrap();
    let id = "5c9e2a41-0f0b-4d6e-9d1f-0a7c1e3b9a10";
    c.board_page("in_progress", None, 30, None).unwrap();
    c.board_page("completed", Some(id), 999, Some("cur sor"))
        .unwrap();
    c.item("5c9e2a41").unwrap();
    c.item_detail(id, 20, 20).unwrap();
    c.history(id, 20, Some("1234")).unwrap();
    c.artifacts(id, 20, None).unwrap();
    c.loops(id, 20, Some("loop-1"), true).unwrap();
    c.loop_text(id, "loop-1").unwrap();
    c.search_items("  two   words ", 10).unwrap();
    let paths: Vec<String> = server.requests().into_iter().map(|r| r.1).collect();
    assert_eq!(
        paths,
        [
            "/api/board?column=in_progress&level=exclude%3A0&limit=30".to_owned(),
            format!("/api/board?column=completed&level=exclude%3A0&limit=200&project={id}&cursor=cur%20sor"),
            "/api/items/5c9e2a41".to_owned(),
            format!("/api/items/{id}/detail?historyLimit=20&artifactLimit=20"),
            format!("/api/items/{id}/history?full=true&limit=20&cursor=1234"),
            format!("/api/items/{id}/artifacts?full=true&limit=20"),
            format!("/api/items/{id}/loops?limit=20&includeClosed=true&cursor=loop-1"),
            format!("/api/items/{id}/loops/loop-1"),
            "/api/search?q=two%20words&limit=10".to_owned(),
        ]
    );

    // Refused before sending: nothing reaches the board.
    let before = server.requests().len();
    let long = "a".repeat(513);
    let refused = [
        c.board_page("done", None, 30, None),
        c.board_page("backlog", Some("../x"), 30, None),
        c.item(""),
        c.item(".."),
        c.item("a/b"),
        c.item(&long),
        c.item_detail("a b", 1, 1),
        c.history(id, 20, Some("12a")),
        c.artifacts(id, 20, Some("")),
        c.loops(id, 20, Some("bad\ncursor"), false),
        c.loop_text(id, "x/y"),
        c.search_items(" a ", 5),
    ];
    for r in refused {
        assert!(matches!(r, Err(StandupError::Invalid(_))), "{r:?}");
    }
    assert_eq!(server.requests().len(), before);

    // The fleet drains page by page and says so when it is whole; a board that is not ready is
    // an answer.
    let fleet = c.fleet().unwrap();
    assert!(fleet.complete);
    assert_eq!(fleet.assignments.len(), 3);
    assert_eq!(c.ready().unwrap(), serde_json::json!({"ready": false}));

    // A board that repeats a cursor ends the drain as partial instead of looping.
    let looping = FakeServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![],
            r#"{"assignments":[{"id":"a"}],"nextCursor":"same"}"#.into(),
        )
    }));
    let f = Client::new(&looping.base, token())
        .unwrap()
        .fleet()
        .unwrap();
    assert!(!f.complete);
    assert_eq!(f.assignments.len(), 2);
}
