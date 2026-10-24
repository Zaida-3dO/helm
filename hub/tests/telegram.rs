//! The Telegram bridge against a recording fake Bot API: the allow-list (trust-boundary item
//! a), the offline queue (f), and the live status message.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use helm_hub::auth::Tokens;
use helm_hub::mcp::Options;
use helm_hub::relay::{Line, Upstream, OFFLINE};
use helm_hub::server::{router, AppState};
use helm_hub::store::Store;
use helm_hub::telegram::{
    now_ms, parse_allowed, parse_receive, ApiError, ApiFuture, Bridge, Handled, Receive,
    TelegramApi, TelegramConfig, HELLO, QUEUED,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tower::ServiceExt;

const OWNER: i64 = 1001;
const STRANGER: i64 = 2002;
const T0: i64 = 1_800_000_000_000;
const MIN: i64 = 60_000;
const RW: &str = "rw-token-0123456789abcdefghij";
const LINE: &str = "line-token-0123456789abcdefghij";
const FORWARDER: &str = "forwarder-token-0123456789abcdef";

#[derive(Default)]
struct FakeApi {
    calls: Mutex<Vec<(String, Value)>>,
    next_id: Mutex<i64>,
    fail_edits: Mutex<bool>,
}

impl FakeApi {
    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
    fn sent_texts(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|(m, _)| m == "sendMessage")
            .map(|(_, b)| b["text"].as_str().unwrap().to_owned())
            .collect()
    }
    fn clear(&self) {
        self.calls.lock().unwrap().clear();
    }
}

impl TelegramApi for FakeApi {
    fn call<'a>(&'a self, method: &'a str, body: Value) -> ApiFuture<'a> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_owned(), body.clone()));
        let result = match method {
            "sendMessage" => {
                let mut id = self.next_id.lock().unwrap();
                *id += 1;
                Ok(json!({ "message_id": *id }))
            }
            "editMessageText" if *self.fail_edits.lock().unwrap() => Err(ApiError {
                code: Some(429),
                description: "Too Many Requests".into(),
            }),
            "getMe" => Ok(json!({ "username": "helm_test_bot" })),
            // A long poll that never returns: enough to see whether the bot polls at all.
            "getUpdates" => return Box::pin(std::future::pending()),
            _ => Ok(json!(true)),
        };
        Box::pin(async move { result })
    }
}

fn cfg() -> TelegramConfig {
    TelegramConfig {
        token: Some("t".into()),
        allowed: vec![OWNER],
        api_base: "http://fake".into(),
        queue_lifetime: Duration::from_secs(30 * 60),
        queue_max: 3,
        receive: Receive::Poll,
    }
}

struct Env {
    api: Arc<FakeApi>,
    line: Arc<Line>,
    bridge: Arc<Bridge>,
}

fn env() -> Env {
    let api = Arc::new(FakeApi::default());
    let (line, _up) = Line::new();
    let line = Arc::new(line);
    let bridge = Arc::new(Bridge::new(api.clone(), line.clone(), &cfg()));
    Env { api, line, bridge }
}

/// Every update gets its own `update_id`, as Telegram's do.
static NEXT_UPDATE: AtomicI64 = AtomicI64::new(1);

fn update(from: i64, text: &str, date_s: i64) -> Value {
    json!({ "update_id": NEXT_UPDATE.fetch_add(1, Ordering::Relaxed), "message": {
        "message_id": 9, "date": date_s, "text": text,
        "from": { "id": from, "is_bot": false, "first_name": "x" },
        "chat": { "id": from, "type": "private" } } })
}

fn line_up(e: &Env) -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    e.line.attach(tx);
    rx
}

fn frames(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    while let Ok(f) = rx.try_recv() {
        out.push(serde_json::from_str(&f).unwrap());
    }
    out.into_iter().filter(|f| f["type"] != "welcome").collect()
}

/// The hub's routes over this bridge (or with the bot off), with a memory token, Helm's line
/// token and a forwarder's telegram token.
fn hub(e: &Env, bot_on: bool) -> axum::Router {
    let tokens = Tokens::parse(&format!(
        "helm:{RW}, helm-line:{LINE}:relay, forwarder:{FORWARDER}:telegram"
    ))
    .unwrap();
    router(Arc::new(AppState {
        store: Mutex::new(Store::open_in_memory().unwrap()),
        tokens,
        options: Options {
            allow_mutate: false,
        },
        line: e.line.clone(),
        telegram: bot_on.then(|| e.bridge.clone()),
    }))
}

async fn post_update(app: &axum::Router, token: Option<&str>, body: String) -> (StatusCode, Value) {
    let mut req =
        Request::post("/telegram/update").header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[test]
fn the_bot_is_off_without_a_token_or_an_allow_list() {
    let mut c = cfg();
    assert_eq!(c.disabled_reason(), None);
    c.allowed.clear();
    assert!(c.disabled_reason().unwrap().contains("denies everyone"));
    c = cfg();
    c.token = None;
    assert!(c.disabled_reason().unwrap().contains("no bot token"));
    assert_eq!(parse_allowed(" 12, 34 ").unwrap(), vec![12, 34]);
    assert!(parse_allowed("@owner").is_err());
    assert!(parse_allowed("-5").is_err());
    assert!(parse_allowed("").unwrap().is_empty());
}

#[tokio::test]
async fn a_sender_not_on_the_allow_list_gets_no_forward_and_no_reply() {
    let e = env();
    let mut rx = line_up(&e);
    for text in ["hello", "/stop", "/cancel", "/start"] {
        assert_eq!(
            e.bridge
                .on_update(&update(STRANGER, text, T0 / 1000), T0)
                .await,
            Handled::Ignored
        );
    }
    // Not even when the stranger writes in a chat the allowed user is in.
    let mut group = update(STRANGER, "hi", T0 / 1000);
    group["message"]["chat"] = json!({ "id": -500, "type": "group" });
    assert_eq!(e.bridge.on_update(&group, T0).await, Handled::Ignored);
    // Nor the allowed user writing in a group: other members would read the replies.
    let mut own_group = update(OWNER, "hi", T0 / 1000);
    own_group["message"]["chat"] = json!({ "id": -500, "type": "group" });
    assert_eq!(e.bridge.on_update(&own_group, T0).await, Handled::Ignored);
    // A message with no sender at all.
    let mut anon = update(OWNER, "hi", T0 / 1000);
    anon["message"].as_object_mut().unwrap().remove("from");
    assert_eq!(e.bridge.on_update(&anon, T0).await, Handled::Ignored);
    assert!(
        e.api.calls().is_empty(),
        "nothing was sent back: {:?}",
        e.api.calls()
    );
    assert!(frames(&mut rx).is_empty(), "nothing went down the line");
    assert_eq!(e.bridge.queued_len(), 0);
}

#[tokio::test]
async fn the_allowed_user_is_forwarded_and_held_until_acknowledged() {
    let e = env();
    let mut rx = line_up(&e);
    assert_eq!(
        e.bridge
            .on_update(&update(OWNER, "status?", T0 / 1000), T0)
            .await,
        Handled::Forwarded
    );
    let f = frames(&mut rx);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0]["type"], "telegram");
    assert_eq!(f[0]["text"], "status?");
    assert_eq!(f[0]["ageMinutes"], Value::Null);
    assert!(
        e.api.sent_texts().is_empty(),
        "no offline reply while the line is up"
    );
    assert_eq!(e.bridge.queued_len(), 1, "held until Helm acknowledges");
    e.bridge.on_ack(f[0]["id"].as_str().unwrap());
    assert_eq!(e.bridge.queued_len(), 0);
}

#[tokio::test]
async fn offline_messages_queue_in_order_and_arrive_marked_with_their_age() {
    let e = env();
    assert_eq!(
        e.bridge
            .on_update(&update(OWNER, "one", T0 / 1000), T0)
            .await,
        Handled::Queued
    );
    assert_eq!(
        e.bridge
            .on_update(&update(OWNER, "two", (T0 + MIN) / 1000), T0 + MIN)
            .await,
        Handled::Queued
    );
    assert_eq!(
        e.api.sent_texts(),
        vec![QUEUED, QUEUED],
        "told at once, each time"
    );
    let mut rx = line_up(&e);
    assert_eq!(e.bridge.flush(T0 + 12 * MIN).await, 2);
    let f = frames(&mut rx);
    assert_eq!(
        f.iter()
            .map(|f| f["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(f[0]["ageMinutes"], 12);
    assert_eq!(f[1]["ageMinutes"], 11);
    assert_eq!(
        f[0]["bot"],
        Value::Null,
        "getMe was not called in this test"
    );
}

#[tokio::test]
async fn unacknowledged_messages_are_resent_on_the_next_line() {
    let e = env();
    let mut rx = line_up(&e);
    e.bridge
        .on_update(&update(OWNER, "one", T0 / 1000), T0)
        .await;
    let first = frames(&mut rx);
    assert_eq!(first.len(), 1);
    // The PC reconnects before acknowledging: the same message (same id) goes again.
    let mut rx2 = line_up(&e);
    assert_eq!(e.bridge.flush(T0 + 1000).await, 1);
    let again = frames(&mut rx2);
    assert_eq!(again[0]["id"], first[0]["id"]);
}

#[tokio::test]
async fn expired_messages_are_dropped_and_the_user_is_told() {
    let e = env();
    e.bridge
        .on_update(&update(OWNER, "old", T0 / 1000), T0)
        .await;
    e.bridge
        .on_update(&update(OWNER, "new", (T0 + 25 * MIN) / 1000), T0 + 25 * MIN)
        .await;
    e.api.clear();
    let mut rx = line_up(&e);
    assert_eq!(e.bridge.flush(T0 + 40 * MIN).await, 1);
    assert_eq!(frames(&mut rx)[0]["text"], "new");
    assert_eq!(
        e.api.sent_texts(),
        vec!["1 queued message expired before Helm reconnected and was dropped."]
    );
}

#[tokio::test]
async fn cancel_clears_the_queue_and_a_full_queue_refuses() {
    let e = env();
    for i in 0..3 {
        e.bridge
            .on_update(&update(OWNER, &format!("m{i}"), T0 / 1000), T0)
            .await;
    }
    e.api.clear();
    e.bridge
        .on_update(&update(OWNER, "m3", T0 / 1000), T0)
        .await;
    assert!(e.api.sent_texts()[0].contains("queue is full"));
    assert_eq!(e.bridge.queued_len(), 3);
    e.api.clear();
    assert_eq!(
        e.bridge
            .on_update(&update(OWNER, "/cancel", T0 / 1000), T0)
            .await,
        Handled::Command
    );
    assert_eq!(e.api.sent_texts(), vec!["Dropped 3 queued messages."]);
    let mut rx = line_up(&e);
    assert_eq!(e.bridge.flush(T0).await, 0);
    assert!(frames(&mut rx).is_empty(), "nothing cancelled is delivered");
}

#[tokio::test]
async fn stop_goes_down_the_line_or_answers_offline() {
    let e = env();
    e.bridge
        .on_update(&update(OWNER, "/stop", T0 / 1000), T0)
        .await;
    assert_eq!(e.api.sent_texts(), vec![OFFLINE]);
    let mut rx = line_up(&e);
    e.bridge
        .on_update(&update(OWNER, "/stop@helm_test_bot", T0 / 1000), T0)
        .await;
    assert_eq!(frames(&mut rx)[0]["type"], "telegram_stop");
}

#[tokio::test]
async fn one_status_message_is_edited_in_place_then_replaced_by_the_answer() {
    let e = env();
    e.bridge
        .on_update(&update(OWNER, "/start", T0 / 1000), T0)
        .await; // learns the chat
    e.api.clear();
    e.bridge
        .on_upstream(Upstream::TelegramStatus("Reading files".into()))
        .await;
    e.bridge.flush_status(T0).await;
    // Debounced: a second status inside the interval is held, not sent.
    e.bridge.on_status("Running tests");
    e.bridge.on_status("Running tests (2 of 5)");
    e.bridge.flush_status(T0 + 1000).await;
    e.bridge.flush_status(T0 + 2000).await;
    e.bridge.flush_status(T0 + 3500).await;
    let calls = e.api.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].0, "sendMessage");
    assert_eq!(calls[0].1["disable_notification"], true);
    assert_eq!(calls[1].0, "editMessageText");
    assert_eq!(
        calls[1].1["text"], "Running tests (2 of 5)",
        "only the latest text is sent"
    );
    let status_id = calls[1].1["message_id"].clone();
    e.api.clear();
    e.bridge.on_final("All 5 passed.").await;
    let calls = e.api.calls();
    assert_eq!(calls[0].0, "sendMessage", "the answer is a NEW message");
    assert_eq!(calls[0].1["text"], "All 5 passed.");
    assert_eq!(
        calls[0].1["disable_notification"], false,
        "so the phone notifies"
    );
    assert_eq!(calls[1].0, "deleteMessage");
    assert_eq!(calls[1].1["message_id"], status_id);
    assert_eq!(calls.len(), 2);
    // A pending status from before the answer is discarded with it.
    e.api.clear();
    e.bridge.flush_status(T0 + 99_000).await;
    assert!(e.api.calls().is_empty());
}

#[tokio::test]
async fn a_rate_limited_edit_is_retried_after_the_interval() {
    let e = env();
    e.bridge.on_status("a");
    e.bridge.flush_status(T0).await;
    *e.api.fail_edits.lock().unwrap() = true;
    e.bridge.on_status("b");
    e.bridge.flush_status(T0 + 3000).await;
    *e.api.fail_edits.lock().unwrap() = false;
    e.bridge.flush_status(T0 + 4000).await; // inside the interval after the failure
    e.bridge.flush_status(T0 + 6000).await;
    let edits: Vec<_> = e
        .api
        .calls()
        .into_iter()
        .filter(|(m, _)| m == "editMessageText")
        .collect();
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[1].1["text"], "b");
}

#[tokio::test]
async fn a_long_answer_is_split_on_character_boundaries() {
    let e = env();
    let long = "é".repeat(4500);
    e.bridge.on_final(&long).await;
    let texts = e.api.sent_texts();
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[0].chars().count(), 4000);
    assert_eq!(texts[1].chars().count(), 500);
}

#[tokio::test]
async fn get_me_names_the_bot_for_helm() {
    let e = env();
    e.bridge.identify().await.unwrap();
    let mut rx = line_up(&e);
    e.bridge
        .on_update(&update(OWNER, "hi", T0 / 1000), T0)
        .await;
    assert_eq!(frames(&mut rx)[0]["bot"], "helm_test_bot");
}

#[tokio::test]
async fn receive_off_never_polls_but_still_names_the_bot() {
    assert_eq!(parse_receive(None), Ok(Receive::Poll));
    assert_eq!(parse_receive(Some("poll")), Ok(Receive::Poll));
    assert_eq!(parse_receive(Some(" off ")), Ok(Receive::Off));
    assert!(parse_receive(Some("webhook")).is_err());
    for (receive, polls) in [(Receive::Poll, true), (Receive::Off, false)] {
        let api = Arc::new(FakeApi::default());
        let (line, up) = Line::new();
        let c = TelegramConfig { receive, ..cfg() };
        let bridge = Arc::new(Bridge::new(api.clone(), Arc::new(line), &c));
        let run = tokio::spawn(bridge.run(up));
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        let methods: Vec<String> = api.calls().into_iter().map(|(m, _)| m).collect();
        assert!(methods.contains(&"getMe".to_owned()), "{methods:?}");
        assert_eq!(
            methods.contains(&"getUpdates".to_owned()),
            polls,
            "{receive:?}: {methods:?}"
        );
        run.abort();
    }
}

#[tokio::test]
async fn only_a_telegram_token_may_post_updates() {
    let e = env();
    let (on, off) = (hub(&e, true), hub(&e, false));
    let start = update(OWNER, "/start", now_ms() / 1000).to_string();
    for (app, token, body, want) in [
        (&on, None, start.as_str(), StatusCode::UNAUTHORIZED),
        (
            &on,
            Some("not-a-real-token-at-all-xxxx"),
            &start,
            StatusCode::UNAUTHORIZED,
        ),
        (&on, Some(RW), &start, StatusCode::FORBIDDEN),
        (&on, Some(LINE), &start, StatusCode::FORBIDDEN),
        (
            &off,
            Some(FORWARDER),
            &start,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (&on, Some(FORWARDER), "{not json", StatusCode::BAD_REQUEST),
        (&on, Some(FORWARDER), "\"hello\"", StatusCode::BAD_REQUEST),
        (&on, Some(FORWARDER), &start, StatusCode::OK),
    ] {
        let (status, v) = post_update(app, token, body.to_owned()).await;
        assert_eq!(status, want, "{token:?} {body}: {v}");
        if status == StatusCode::OK {
            assert_eq!(v, json!({ "ok": true, "handled": ["command"] }));
        } else {
            assert!(v["error"].is_string(), "{v}");
        }
    }
    assert_eq!(
        e.api.sent_texts(),
        vec![HELLO],
        "only the accepted post was handled"
    );
}

#[tokio::test]
async fn a_posted_update_is_forwarded_like_a_polled_one_and_handled_once() {
    let e = env();
    let app = hub(&e, true);
    let mut rx = line_up(&e);
    let now = now_ms();
    let posted = update(OWNER, "posted", now / 1000);
    let (status, v) = post_update(&app, Some(FORWARDER), posted.to_string()).await;
    assert_eq!(
        (status, &v["handled"]),
        (StatusCode::OK, &json!(["forwarded"]))
    );
    let f = frames(&mut rx);
    assert_eq!(
        (f.len(), &f[0]["type"], &f[0]["text"]),
        (1, &json!("telegram"), &json!("posted"))
    );
    // The forwarder retries, and the same update also arrives by polling: handled once.
    let (_, v) = post_update(&app, Some(FORWARDER), posted.to_string()).await;
    assert_eq!(v["handled"], json!(["duplicate"]));
    assert_eq!(e.bridge.on_update(&posted, now).await, Handled::Duplicate);
    // An array is handled in order, each update on its own.
    let fresh = update(OWNER, "fresh", now / 1000);
    let (_, v) = post_update(&app, Some(FORWARDER), json!([posted, fresh]).to_string()).await;
    assert_eq!(v["handled"], json!(["duplicate", "forwarded"]));
    let f = frames(&mut rx);
    assert_eq!((f.len(), &f[0]["text"]), (1, &json!("fresh")));
    // The allow-list holds for posted updates too.
    let stranger = update(STRANGER, "hi", now / 1000);
    let (_, v) = post_update(&app, Some(FORWARDER), stranger.to_string()).await;
    assert_eq!(v["handled"], json!(["ignored"]));
    assert!(frames(&mut rx).is_empty());
    assert!(e.api.sent_texts().is_empty(), "nothing was sent back");
}

#[tokio::test]
async fn the_helm_prefix_is_read_as_the_text_after_it() {
    let e = env();
    let mut rx = line_up(&e);
    // (typed, what goes down the line, what the bot replies)
    for (typed, down, reply) in [
        ("/helm status?", Some("status?"), None),
        ("/helm@helm_test_bot  two\nlines", Some("two\nlines"), None),
        ("/helmet on", Some("/helmet on"), None),
        (
            "/helm /stop",
            Some("telegram_stop"),
            Some("Stopping the running turn."),
        ),
        ("/helm", None, Some(HELLO)),
        ("/helm@helm_test_bot", None, Some(HELLO)),
    ] {
        e.api.clear();
        e.bridge
            .on_update(&update(OWNER, typed, T0 / 1000), T0)
            .await;
        let got: Vec<String> = frames(&mut rx)
            .iter()
            .map(|f| match f["type"].as_str() {
                Some("telegram") => f["text"].as_str().unwrap().to_owned(),
                other => other.unwrap().to_owned(),
            })
            .collect();
        assert_eq!(got, Vec::from_iter(down), "{typed:?}");
        assert_eq!(e.api.sent_texts(), Vec::from_iter(reply), "{typed:?}");
    }
}

#[tokio::test]
async fn an_unacknowledged_message_is_resent_on_the_same_line_until_it_expires() {
    let e = env();
    let mut rx = line_up(&e);
    e.bridge
        .on_update(&update(OWNER, "one", T0 / 1000), T0)
        .await;
    let id = frames(&mut rx)[0]["id"].clone();
    // (seconds after the first send, how many times it goes down again)
    for (at_s, again) in [(44, 0), (45, 1), (60, 0), (89, 0), (90, 1)] {
        assert_eq!(e.bridge.flush(T0 + at_s * 1000).await, again, "at {at_s} s");
        let f = frames(&mut rx);
        assert_eq!(f.len(), again, "at {at_s} s");
        assert!(f.iter().all(|f| f["id"] == id), "the same message, same id");
    }
    // Still unacknowledged past the queue lifetime (30 min here): dropped, and the user is told.
    e.api.clear();
    assert_eq!(e.bridge.flush(T0 + 31 * MIN).await, 0);
    assert!(frames(&mut rx).is_empty());
    assert_eq!(e.bridge.queued_len(), 0);
    assert_eq!(
        e.api.sent_texts(),
        vec!["1 message could not be delivered to Helm in time and was dropped."]
    );
    // An acknowledged message is never sent again.
    e.bridge
        .on_update(&update(OWNER, "two", (T0 + 40 * MIN) / 1000), T0 + 40 * MIN)
        .await;
    e.bridge.on_ack(frames(&mut rx)[0]["id"].as_str().unwrap());
    assert_eq!(e.bridge.flush(T0 + 42 * MIN).await, 0);
}
