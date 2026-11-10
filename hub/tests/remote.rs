//! The Remote page over the HTTP router: who may use it, which tools it may call, which computers
//! it reaches, what it says when Helm is offline, and what its responses never carry.

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use helm_hub::auth::Scopes;
use helm_hub::lines::Lines;
use helm_hub::mcp::Options;
use helm_hub::registry::Registry;
use helm_hub::relay::{CallOutcome, Line, OFFLINE};
use helm_hub::remote::{ACTIONS_PER_MINUTE, READS_PER_MINUTE};
use helm_hub::server::{router, AppState};
use helm_hub::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tower::ServiceExt;

/// A fake Helm on a person's computer: its line, and the frames the hub sent down it.
struct Pc {
    line: Arc<Line>,
    generation: u64,
    frames: mpsc::UnboundedReceiver<String>,
}

struct Hub {
    app: axum::Router,
    lines: Arc<Lines>,
    registry: Arc<Mutex<Registry>>,
    tokens: Vec<String>,
}

fn hub() -> Hub {
    let registry = Arc::new(Mutex::new(Registry::open_in_memory().unwrap()));
    let (lines, _upstream) = Lines::new(registry.clone());
    let lines = Arc::new(lines);
    let app = router(Arc::new(AppState::new(
        Store::open_in_memory().unwrap(),
        registry.clone(),
        Options {
            allow_mutate: false,
        },
        lines.clone(),
        None,
    )));
    Hub {
        app,
        lines,
        registry,
        tokens: vec![],
    }
}

impl Hub {
    /// A machine for `person` (made if new) with `scopes`; returns its token.
    fn add(&mut self, person: &str, machine: &str, scopes: &str) -> String {
        let scopes = Scopes::parse_list(scopes).unwrap();
        let token = self
            .registry
            .lock()
            .unwrap()
            .add_client(person, machine, &scopes)
            .unwrap()
            .token;
        self.tokens.push(token.clone());
        token
    }

    /// Helm on `machine` connects its line.
    fn connect(&self, token: &str) -> Pc {
        let client = self
            .registry
            .lock()
            .unwrap()
            .authenticate(Some(&format!("Bearer {token}")))
            .unwrap();
        let line = self.lines.line_for(&client);
        let (tx, mut frames) = mpsc::unbounded_channel();
        let generation = line.attach(tx);
        assert_eq!(
            json_of(&frames.try_recv().unwrap())["type"],
            "welcome",
            "{}",
            client.machine
        );
        Pc {
            line,
            generation,
            frames,
        }
    }
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn api(token: Option<&str>, body: &str) -> Request<Body> {
    let mut r = Request::post("/remote/api/call").header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    r.body(Body::from(body.to_owned())).unwrap()
}

fn call_body(name: &str, args: Value) -> String {
    json!({ "name": name, "args": args }).to_string()
}

fn on_machine(name: &str, args: Value, machine: &str) -> String {
    json!({ "name": name, "args": args, "machine": machine }).to_string()
}

fn json_of(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or(Value::Null)
}

/// What Helm answers on its line to the call `frame`.
fn answer(pc: &Pc, frame: &Value, mut says: Value) {
    says["type"] = json!("result");
    says["id"] = frame["id"].clone();
    assert!(pc.line.receive(pc.generation, &says.to_string()));
}

#[tokio::test]
async fn only_a_remote_token_uses_the_page_and_it_reaches_nothing_else() {
    let mut h = hub();
    let phone = h.add("Sam", "phone", "remote");
    let laptop = h.add("Sam", "laptop", "remote");
    let notes = h.add("Sam", "notes", "memory:rw");
    let agent = h.add("Sam", "agent", "memory:ro,app");
    let line = h.add("Sam", "desk", "relay");
    let fwd = h.add("Sam", "fwd", "telegram");
    let voice = h.add("Sam", "voicephone", "voice");
    // The page itself holds no secret, so it opens without a token; every response is hardened.
    for path in ["/remote", "/remote/app.js", "/remote/app.css"] {
        let (status, headers, body) =
            send(&h.app, Request::get(path).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(!body.is_empty(), "{path}");
        let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(
            csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"),
            "{csp}"
        );
        assert!(
            !csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"),
            "{csp}"
        );
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer", "{path}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{path}");
        assert!(
            !headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            "no CORS on {path}"
        );
    }
    // The calls: (token, expected status). With no line up, a remote token reaches Helm's
    // offline answer, and nobody else gets that far.
    let body = call_body("app_list_chats", json!({}));
    for (token, want) in [
        (None, StatusCode::UNAUTHORIZED),
        (
            Some("not-a-token-at-all-0123456789"),
            StatusCode::UNAUTHORIZED,
        ),
        (Some(notes.as_str()), StatusCode::FORBIDDEN),
        (Some(agent.as_str()), StatusCode::FORBIDDEN),
        (Some(line.as_str()), StatusCode::FORBIDDEN),
        (Some(fwd.as_str()), StatusCode::FORBIDDEN),
        (Some(voice.as_str()), StatusCode::FORBIDDEN),
        (Some(phone.as_str()), StatusCode::OK),
        (Some(laptop.as_str()), StatusCode::OK),
    ] {
        let (status, _, text) = send(&h.app, api(token, &body)).await;
        assert_eq!(status, want, "{token:?}");
        for t in &h.tokens {
            assert!(!text.contains(t.as_str()), "a token came back in {text}");
        }
    }
    // A remote token reaches nothing else the hub serves with a token (opening the line takes a
    // real socket: tests/relay.rs).
    let mcp = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    for (path, body) in [
        ("/mcp", mcp.as_str()),
        ("/telegram/update", "{}"),
        ("/voice/status", ""),
        ("/voice/fresh", ""),
    ] {
        let method = if path == "/voice/status" {
            "GET"
        } else {
            "POST"
        };
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {phone}"))
            .body(Body::from(body.to_owned()))
            .unwrap();
        let (status, _, _) = send(&h.app, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
}

#[tokio::test]
async fn only_the_vetted_tools_are_called_and_bad_requests_send_nothing() {
    let mut h = hub();
    let phone = h.add("Sam", "phone", "remote");
    let desk = h.add("Sam", "desk", "relay");
    let mut pc = h.connect(&desk);
    let long = "x".repeat(70 * 1024);
    // (body, expected status)
    let cases: Vec<(String, StatusCode)> = vec![
        // Tools the page may never call, consent ones included.
        (
            call_body("app_update_settings", json!({})),
            StatusCode::FORBIDDEN,
        ),
        (
            call_body("app_request_allow", json!({})),
            StatusCode::FORBIDDEN,
        ),
        (
            call_body("app_workflow_gate", json!({})),
            StatusCode::FORBIDDEN,
        ),
        (
            call_body("app_orchestrator_on", json!({})),
            StatusCode::FORBIDDEN,
        ),
        (call_body("app_mcp_call", json!({})), StatusCode::FORBIDDEN),
        (
            call_body("app_create_chat", json!({})),
            StatusCode::FORBIDDEN,
        ),
        (call_body("memory_search", json!({})), StatusCode::FORBIDDEN),
        // Not a call at all.
        ("not json".into(), StatusCode::BAD_REQUEST),
        ("{}".into(), StatusCode::BAD_REQUEST),
        (json!({"name": 7}).to_string(), StatusCode::BAD_REQUEST),
        (
            json!({"name": "app_list_chats", "machine": 3}).to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            call_body("app_list_chats", json!([1])),
            StatusCode::BAD_REQUEST,
        ),
        (
            call_body("app_list_chats", json!({"query": long})),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ];
    for (body, want) in cases {
        let (status, _, text) = send(&h.app, api(Some(&phone), &body)).await;
        assert_eq!(status, want, "{}", &body[..body.len().min(60)]);
        assert!(!text.contains(&phone));
    }
    assert!(pc.frames.try_recv().is_err(), "nothing reached the line");
}

#[tokio::test]
async fn with_no_computer_connected_every_allowed_tool_says_nothing_was_sent() {
    let mut h = hub();
    let phone = h.add("Sam", "phone", "remote");
    for name in [
        "app_list_chats",
        "app_read_chat",
        "app_list_projects",
        "app_list_attention",
        "app_send_message",
        "app_stop_chat",
        "app_list_waiting",
        "app_answer_waiting",
    ] {
        let (status, _, text) = send(&h.app, api(Some(&phone), &call_body(name, json!({})))).await;
        assert_eq!(status, StatusCode::OK, "{name}");
        let v = json_of(&text);
        assert_eq!(
            (v["ok"].clone(), v["error"].clone()),
            (json!(false), json!(OFFLINE)),
            "{name}"
        );
        assert_eq!(
            (
                v["offline"].clone(),
                v["line"].clone(),
                v["machines"].clone()
            ),
            (json!(true), json!(false), json!([])),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_call_reaches_only_its_own_persons_computers_stamped_as_from_the_page() {
    let mut h = hub();
    let phone = h.add("Sam", "phone", "remote");
    let tablet = h.add("Pat", "tablet", "remote");
    let (desk, laptop, pat_pc) = (
        h.add("Sam", "desk", "relay"),
        h.add("Sam", "laptop", "relay"),
        h.add("Pat", "pc", "relay"),
    );
    let (mut desk, mut laptop, mut pat) =
        (h.connect(&desk), h.connect(&laptop), h.connect(&pat_pc));
    // Sam's page: (machine asked for, which computer's line the call goes down, the machines the
    // answer lists, whether the line is up)
    let sam = ["desk", "laptop"];
    for (machine, lands, line_up) in [
        // No choice: the computer used most recently (the laptop connected last).
        (None, Some("laptop"), true),
        (Some("desk"), Some("desk"), true),
        (Some("LAPTOP"), Some("laptop"), true),
        // A computer that is not Sam's, or does not exist: offline, and nothing is sent anywhere.
        (Some("pc"), None, false),
        (Some("nowhere"), None, false),
    ] {
        let args = json!({"chatId": "c1"});
        let body = match machine {
            Some(m) => on_machine("app_stop_chat", args.clone(), m),
            None => call_body("app_stop_chat", args.clone()),
        };
        let app = h.app.clone();
        let caller = tokio::spawn({
            let phone = phone.clone();
            async move { send(&app, api(Some(&phone), &body)).await }
        });
        let frame = match lands {
            Some("desk") => json_of(&desk.frames.recv().await.unwrap()),
            Some(_) => json_of(&laptop.frames.recv().await.unwrap()),
            None => Value::Null,
        };
        if lands.is_some() {
            assert_eq!(frame["type"], "call");
            assert_eq!(frame["tool"], "app_stop_chat");
            assert_eq!(frame["args"], args, "the machine choice never reaches Helm");
            assert_eq!(frame["client"], "phone");
            assert_eq!(frame["scope"], "remote", "the PC can tell the page apart");
            let pc = if lands == Some("desk") {
                &desk
            } else {
                &laptop
            };
            answer(pc, &frame, json!({"ok": true, "result": {"stopped": true}}));
        }
        let (status, _, text) = caller.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        let v = json_of(&text);
        assert_eq!(v["line"], json!(line_up), "{machine:?}");
        assert_eq!(v["machines"], json!(sam), "only Sam's own: {machine:?}");
        if lands.is_some() {
            assert_eq!(v["result"], json!({"stopped": true}), "{machine:?}");
        } else {
            assert_eq!(
                (v["ok"].clone(), v["offline"].clone()),
                (json!(false), json!(true))
            );
        }
        assert!(!text.contains(&phone));
    }
    for pc in [&mut desk, &mut laptop, &mut pat] {
        assert!(pc.frames.try_recv().is_err(), "no call went anywhere else");
    }
    // Pat's page reaches Pat's computer, and cannot name Sam's.
    for (machine, lands) in [(None, true), (Some("desk"), false)] {
        let body = match machine {
            Some(m) => on_machine("app_list_attention", json!({}), m),
            None => call_body("app_list_attention", json!({})),
        };
        let app = h.app.clone();
        let tablet = tablet.clone();
        let caller = tokio::spawn(async move { send(&app, api(Some(&tablet), &body)).await });
        if lands {
            let frame = json_of(&pat.frames.recv().await.unwrap());
            assert_eq!(
                (frame["client"].clone(), frame["scope"].clone()),
                (json!("tablet"), json!("remote"))
            );
            answer(&pat, &frame, json!({"ok": true, "result": {"rows": []}}));
        }
        let (_, _, text) = caller.await.unwrap();
        let v = json_of(&text);
        assert_eq!(v["ok"], json!(lands), "{machine:?}: {text}");
        assert_eq!(v["machines"], json!(["pc"]));
    }
    assert!(desk.frames.try_recv().is_err() && laptop.frames.try_recv().is_err());
}

#[tokio::test]
async fn what_helm_answers_comes_back_and_a_dropped_line_may_or_may_not_have_run() {
    let mut h = hub();
    let phone = h.add("Sam", "phone", "remote");
    let desk = h.add("Sam", "desk", "relay");
    let mut pc = h.connect(&desk);
    // (tool, arguments, what Helm answers on the line, expected body fields)
    let cases = [
        (
            "app_list_waiting",
            json!({}),
            json!({"ok": true, "result": {"answering": false, "waiting": []}}),
            json!({"ok": true, "result": {"answering": false, "waiting": []}, "line": true}),
        ),
        (
            "app_answer_waiting",
            json!({"chatId": "c1", "requestId": "r1", "index": 0, "label": "Deny"}),
            json!({"ok": false, "error": "Answering from the Remote page is off.", "changed": "no"}),
            json!({"ok": false, "error": "Answering from the Remote page is off. Nothing was changed.", "line": true}),
        ),
        (
            "app_send_message",
            json!({"chatId": "c1", "message": "hi", "mode": "steer"}),
            json!({"ok": false, "error": "The chat is busy.", "changed": "maybe"}),
            json!({"ok": false, "error": "The chat is busy.", "line": true}),
        ),
    ];
    for (tool, args, helm_says, want) in cases {
        let app = h.app.clone();
        let body = call_body(tool, args.clone());
        let token = phone.clone();
        let caller = tokio::spawn(async move { send(&app, api(Some(&token), &body)).await });
        let frame = json_of(&pc.frames.recv().await.unwrap());
        assert_eq!(frame["tool"], tool);
        assert_eq!(frame["args"], args);
        answer(&pc, &frame, helm_says);
        let (status, _, text) = caller.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        let v = json_of(&text);
        for (k, expected) in want.as_object().unwrap() {
            assert_eq!(&v[k], expected, "{tool} {k}");
        }
        assert!(!text.contains(&phone));
    }
    // The line drops while a call waits.
    let app = h.app.clone();
    let body = call_body("app_stop_chat", json!({"chatId": "c1"}));
    let caller = {
        let phone = phone.clone();
        tokio::spawn(async move { send(&app, api(Some(&phone), &body)).await })
    };
    assert_eq!(json_of(&pc.frames.recv().await.unwrap())["type"], "call");
    pc.line.detach(pc.generation);
    let (status, _, text) = caller.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    let v = json_of(&text);
    assert_eq!(
        (v["ok"].clone(), v["unknown"].clone()),
        (json!(false), json!(true))
    );
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("may or may not have run"),
        "{text}"
    );
    // No answer in time: the same words.
    let (tx, _rx) = mpsc::unbounded_channel();
    pc.line.attach(tx);
    let late = pc
        .line
        .call_with_timeout(
            "phone",
            "app_stop_chat",
            &json!({}),
            Duration::from_millis(50),
        )
        .await;
    match late {
        CallOutcome::Unknown(t) => assert!(t.contains("may or may not have run"), "{t}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn polling_never_eats_what_an_answer_needs_and_each_token_has_its_own_allowance() {
    let mut h = hub();
    let phone = h.add("Sam", "phone", "remote");
    let laptop = h.add("Sam", "laptop", "remote");
    let reads = call_body("app_list_waiting", json!({}));
    let act = call_body("app_stop_chat", json!({"chatId": "c1"}));
    // A hundred reads (a long while of the page looking) leave every action available...
    for _ in 0..100 {
        let (status, _, _) = send(&h.app, api(Some(&phone), &reads)).await;
        assert_eq!(status, StatusCode::OK);
    }
    for _ in 0..ACTIONS_PER_MINUTE {
        let (status, _, _) = send(&h.app, api(Some(&phone), &act)).await;
        assert_eq!(status, StatusCode::OK);
    }
    // ...until the actions themselves run out; reads then still go.
    let (status, headers, text) = send(&h.app, api(Some(&phone), &act)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
    assert!(!text.contains(&phone));
    let (status, _, _) = send(&h.app, api(Some(&phone), &reads)).await;
    assert_eq!(status, StatusCode::OK);
    // Reads have their own, larger allowance.
    for _ in 0..READS_PER_MINUTE - 101 {
        send(&h.app, api(Some(&phone), &reads)).await;
    }
    let (status, _, _) = send(&h.app, api(Some(&phone), &reads)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let (status, _, _) = send(&h.app, api(Some(&laptop), &act)).await;
    assert_eq!(status, StatusCode::OK, "the limit is per token");
}

#[tokio::test]
async fn an_agents_mcp_surface_never_offers_or_forwards_the_pages_two_tools() {
    let mut h = hub();
    let agent = h.add("Sam", "agent", "memory:rw,app");
    let desk = h.add("Sam", "desk", "relay");
    let mut pc = h.connect(&desk);
    let mcp = |body: Value| {
        Request::post("/mcp")
            .header(header::AUTHORIZATION, format!("Bearer {agent}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (_, _, text) = send(
        &h.app,
        mcp(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})),
    )
    .await;
    let names: Vec<String> = json_of(&text)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"app_stop_chat".to_owned()), "{names:?}");
    for page_only in ["app_list_waiting", "app_answer_waiting"] {
        assert!(!names.contains(&page_only.to_owned()), "{page_only}");
        let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": page_only, "arguments": {"chatId": "c", "requestId": "r", "index": 0, "label": "Allow"}}});
        let (_, _, text) = send(&h.app, mcp(call)).await;
        assert!(!text.contains("\"isError\":false"), "{page_only}: {text}");
    }
    assert!(pc.frames.try_recv().is_err(), "nothing went down the line");
}
