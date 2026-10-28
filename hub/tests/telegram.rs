//! The Telegram bridge against a recording fake Bot API: the allow-list (trust-boundary item
//! a), the offline queue (f), the live status message, files, pictures and recordings, quoted
//! replies and the approval buttons.

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, Request, StatusCode};
use helm_hub::auth::Tokens;
use helm_hub::mcp::Options;
use helm_hub::relay::{Line, Upstream, OFFLINE};
use helm_hub::server::{router, AppState};
use helm_hub::store::Store;
use helm_hub::telegram::{
    now_ms, parse_allowed, parse_receive, size_text, ApiError, ApiFuture, Bridge, Files, Handled,
    HttpFiles, HttpVoice, Receive, SttConfig, TelegramApi, TelegramConfig, Voice, VoiceFuture,
    FILES_OFF, HELLO, IMAGE_FAILED, POLL_SECONDS, QUEUED, STT_DOWN, STT_DOWN_AFTER, TAP_OFFLINE,
    TAP_SENT, TAP_STALE, TEXT_ONLY, VOICE_OFF,
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
            "getFile" => Ok(match body["file_id"].as_str() {
                Some("huge") => json!({ "file_id": "huge", "file_size": 21 * 1024 * 1024,
                                        "file_path": "voice/huge.oga" }),
                Some("img-huge") => json!({ "file_id": "img-huge", "file_size": 21 * 1024 * 1024,
                                           "file_path": "photos/img-huge" }),
                Some(id) if id.starts_with("img-") => {
                    json!({ "file_id": id, "file_size": 1234, "file_path": format!("photos/{id}") })
                }
                // A file id with a dot in it is served under that name.
                Some(id) if id.contains('.') => {
                    json!({ "file_id": id, "file_size": 1234, "file_path": format!("files/{id}") })
                }
                _ => json!({ "file_id": body["file_id"], "file_size": 1234,
                             "file_path": "voice/file_7.oga" }),
            }),
            // A long poll that never returns: enough to see whether the bot polls at all.
            "getUpdates" => return Box::pin(std::future::pending()),
            _ => Ok(json!(true)),
        };
        Box::pin(async move { result })
    }
}

/// Downloads a few bytes and answers `heard`, once the gate lets it (open unless `held`).
struct FakeVoice {
    heard: Result<String, String>,
    gate: tokio::sync::Semaphore,
    seen: Mutex<Vec<String>>,
}

impl FakeVoice {
    fn new(heard: Result<&str, &str>) -> Arc<Self> {
        Self::gated(heard, 100)
    }
    fn held(heard: Result<&str, &str>) -> Arc<Self> {
        Self::gated(heard, 0)
    }
    fn gated(heard: Result<&str, &str>, permits: usize) -> Arc<Self> {
        Arc::new(FakeVoice {
            heard: heard.map(str::to_owned).map_err(str::to_owned),
            gate: tokio::sync::Semaphore::new(permits),
            seen: Mutex::new(vec![]),
        })
    }
}

impl Voice for FakeVoice {
    fn download<'a>(&'a self, file_path: &'a str) -> VoiceFuture<'a, Vec<u8>> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("download {file_path}"));
        Box::pin(async { Ok(b"OggS".to_vec()) })
    }
    fn transcribe<'a>(
        &'a self,
        audio: Vec<u8>,
        name: &'a str,
        mime: &'a str,
    ) -> VoiceFuture<'a, String> {
        let seen = format!("transcribe {name} {mime} {}", audio.len());
        self.seen.lock().unwrap().push(seen);
        Box::pin(async move {
            self.gate.acquire().await.unwrap().forget();
            self.heard.clone()
        })
    }
}

/// Hands out the file stored under a file id (`photos/<id>` or `files/<id>`), once the gate lets
/// it.
struct FakeFiles {
    files: Vec<(&'static str, Vec<u8>)>,
    gate: tokio::sync::Semaphore,
    /// How many downloads have started.
    started: AtomicI64,
}

impl Files for FakeFiles {
    fn download<'a>(&'a self, file_path: &'a str) -> VoiceFuture<'a, Vec<u8>> {
        self.started.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            self.gate.acquire().await.unwrap().forget();
            let id = file_path.rsplit('/').next().unwrap_or(file_path);
            match self.files.iter().find(|(f, _)| *f == id) {
                Some((_, bytes)) => Ok(bytes.clone()),
                None => Err("the download from Telegram failed (HTTP 404)".into()),
            }
        })
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
        stt: None,
    }
}

/// A PNG header-only picture of `w` x `h`, with an animation chunk when `animated`.
fn png(w: u32, h: u32, animated: bool) -> Vec<u8> {
    let chunk = |tag: &[u8], data: &[u8]| {
        let mut c = (data.len() as u32).to_be_bytes().to_vec();
        c.extend([tag, data, &[0; 4]].concat());
        c
    };
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    let size = [w.to_be_bytes(), h.to_be_bytes()].concat();
    b.extend(chunk(b"IHDR", &[size, vec![8, 6, 0, 0, 0]].concat()));
    if animated {
        b.extend(chunk(b"acTL", &[0; 8]));
    }
    b.extend(chunk(b"IDAT", &[0; 4]));
    b
}

fn jpeg(w: u16, h: u16) -> Vec<u8> {
    let mut b = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 17, 8];
    b.extend(h.to_be_bytes());
    b.extend(w.to_be_bytes());
    b.extend([3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    b
}

struct Env {
    api: Arc<FakeApi>,
    line: Arc<Line>,
    bridge: Arc<Bridge>,
}

fn env() -> Env {
    env_with(None)
}

fn env_with(voice: Option<Arc<FakeVoice>>) -> Env {
    env_of(voice, None)
}

fn env_of(voice: Option<Arc<FakeVoice>>, files: Option<Arc<FakeFiles>>) -> Env {
    let api = Arc::new(FakeApi::default());
    let (line, _up) = Line::new();
    let line = Arc::new(line);
    let mut bridge = Bridge::new(api.clone(), line.clone(), &cfg());
    if let Some(v) = voice {
        bridge = bridge.with_voice(v);
    }
    if let Some(f) = files {
        bridge = bridge.with_files(f);
    }
    let bridge = Arc::new(bridge);
    Env { api, line, bridge }
}

fn fake_files(files: Vec<(&'static str, Vec<u8>)>, open: bool) -> Arc<FakeFiles> {
    Arc::new(FakeFiles {
        files,
        gate: tokio::sync::Semaphore::new(if open { 100 } else { 0 }),
        started: AtomicI64::new(0),
    })
}

fn env_with_files(files: Vec<(&'static str, Vec<u8>)>, open: bool) -> (Env, Arc<FakeFiles>) {
    let files = fake_files(files, open);
    (env_of(None, Some(files.clone())), files)
}

/// Let spawned work (a file's fetch, a recording's transcription) run to its end, including the
/// encoding on the blocking pool.
async fn settle() {
    for _ in 0..3 {
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

/// Every update gets its own `update_id`, as Telegram's do.
static NEXT_UPDATE: AtomicI64 = AtomicI64::new(1);

fn update(from: i64, text: &str, date_s: i64) -> Value {
    json!({ "update_id": NEXT_UPDATE.fetch_add(1, Ordering::Relaxed), "message": {
        "message_id": 9, "date": date_s, "text": text,
        "from": { "id": from, "is_bot": false, "first_name": "x" },
        "chat": { "id": from, "type": "private" } } })
}

/// `u` with `fields` set on its message (a voice note, a reply, ...).
fn with_fields(mut u: Value, fields: Value) -> Value {
    let m = u["message"].as_object_mut().unwrap();
    m.extend(fields.as_object().unwrap().clone());
    u
}

/// A message from the owner, sent now, with no text and `fields` set.
fn owner_message(fields: Value) -> Value {
    let mut u = update(OWNER, "", now_ms() / 1000);
    u["message"].as_object_mut().unwrap().remove("text");
    with_fields(u, fields)
}

/// What went down the line: a forwarded message's text, else the frame's type.
fn down(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<String> {
    frames(rx)
        .iter()
        .map(|f| match f["type"].as_str() {
            Some("telegram") => f["text"].as_str().unwrap().to_owned(),
            other => other.unwrap().to_owned(),
        })
        .collect()
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
    // An alert Helm starts mid-turn is a new, notifying message, and the status stays.
    e.api.clear();
    e.bridge
        .on_upstream(Upstream::TelegramNotify("Docs chat: finished".into()))
        .await;
    let calls = e.api.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, "sendMessage");
    assert_eq!(calls[0].1["text"], "Docs chat: finished");
    assert_eq!(calls[0].1["disable_notification"], false);
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
        let calls = api.calls();
        if let Some((_, body)) = calls.iter().find(|(m, _)| m == "getUpdates") {
            // A short long poll, so a connection a router drops while idle is replaced quickly.
            assert_eq!(body["timeout"], POLL_SECONDS);
            assert_eq!(
                body["allowed_updates"],
                json!(["message", "callback_query"])
            );
            const { assert!(POLL_SECONDS <= 30) };
        }
        let methods: Vec<String> = calls.into_iter().map(|(m, _)| m).collect();
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

/// A file as it went down the line: (name, type, bytes).
type Down = (String, String, Vec<u8>);

/// The `telegram` frames that went down: (text, its pictures, its plain files).
fn attached_down(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<(String, Vec<Down>, Vec<Down>)> {
    use base64::Engine as _;
    let list = |v: &Value| -> Vec<Down> {
        let items = v.as_array().cloned().unwrap_or_default();
        items
            .iter()
            .map(|i| {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(i["data"].as_str().unwrap())
                    .unwrap();
                (
                    i["name"].as_str().unwrap().to_owned(),
                    i["mediaType"].as_str().unwrap().to_owned(),
                    data,
                )
            })
            .collect()
    };
    frames(rx)
        .iter()
        .filter(|f| f["type"] == "telegram")
        .map(|f| {
            (
                f["text"].as_str().unwrap().to_owned(),
                list(&f["images"]),
                list(&f["files"]),
            )
        })
        .collect()
}

/// A file as the tests expect it down the line.
fn file(name: &str, mime: &str, bytes: &[u8]) -> Down {
    (name.to_owned(), mime.to_owned(), bytes.to_vec())
}

const TOO_LARGE: &str = "it is larger than 20 MB, the most a Telegram bot can download";

#[tokio::test]
async fn recordings_go_with_their_transcript_and_the_recording_attached() {
    let note = json!({ "file_id": "f1", "mime_type": "audio/ogg", "file_size": 1234 });
    let big = json!({ "file_id": "f2", "mime_type": "audio/ogg", "file_size": 21 * 1024 * 1024 });
    // Refused at once: nothing fetches files on this hub, or a file above the Bot API's limit.
    let too_big = format!("That recording was not sent to Helm: {TOO_LARGE}");
    for (on, note, said) in [(false, &note, FILES_OFF), (true, &big, too_big.as_str())] {
        let e = env_with(on.then(|| FakeVoice::new(Ok("hi"))));
        let mut rx = line_up(&e);
        let m = owner_message(json!({ "voice": note }));
        assert_eq!(e.bridge.on_update(&m, now_ms()).await, Handled::Command);
        assert_eq!(
            (e.api.sent_texts(), down(&mut rx)),
            (vec![said.to_owned()], vec![])
        );
    }
    // Transcribed or not, the recording goes as a file beside its text; a server that is down or
    // errs gets a plain reply. (fields, what the server hears, the text, the reply)
    const HTTP_500: &str = "the speech-to-text server answered HTTP 500: model not loaded";
    const DOWN: &str = "the speech-to-text server could not be reached: Connection refused";
    let unheard = "[Voice note] (not transcribed: transcription was unavailable)";
    let down_reply = |why: &str| format!("{STT_DOWN}{why}{STT_DOWN_AFTER}");
    let memo = json!({ "file_id": "f3", "file_name": "memo.mp3", "mime_type": "audio/mpeg" });
    let reply_to = json!({ "audio": memo, "reply_to_message": { "message_id": 3, "text": "Go?" } });
    let markers = "part one [gap: 00:10-00:12] part two [truncated: past 60 min]";
    let ogg = file("file_7.oga", "audio/ogg", b"OggS");
    for (fields, heard, text, reply, attached) in [
        (
            json!({ "voice": { "file_id": "huge" } }),
            Ok("hi"),
            None,
            Some(too_big.clone()),
            None,
        ),
        (
            json!({ "voice": note }),
            Ok(" hello there "),
            Some("[Voice note] hello there".to_owned()),
            None,
            Some(ogg.clone()),
        ),
        (
            json!({ "voice": note }),
            Ok(markers),
            Some(format!("[Voice note] {markers}")),
            None,
            Some(ogg.clone()),
        ),
        (
            json!({ "voice": note }),
            Err(DOWN),
            Some(unheard.to_owned()),
            Some(down_reply(DOWN)),
            Some(ogg.clone()),
        ),
        (
            json!({ "voice": note }),
            Err(HTTP_500),
            Some(unheard.to_owned()),
            Some(down_reply(HTTP_500)),
            Some(ogg.clone()),
        ),
        (
            json!({ "voice": note }),
            Ok(" "),
            Some("[Voice note] (no speech was heard)".to_owned()),
            None,
            Some(ogg.clone()),
        ),
        (
            reply_to,
            Ok("yes"),
            Some("Replying to: \"Go?\"\n[Audio] memo.mp3, 4 bytes\nTranscript: yes".to_owned()),
            None,
            Some(file("memo.mp3", "audio/mpeg", b"OggS")),
        ),
        // A video note the server cannot transcribe just goes as a file, without a reply.
        (
            json!({ "video_note": { "file_id": "round.mp4", "file_size": 4 } }),
            Ok("hi"),
            Some("[Video note] round.mp4, 4 bytes\nTranscript: hi".to_owned()),
            None,
            Some(file("round.mp4", "application/octet-stream", b"OggS")),
        ),
        (
            json!({ "video_note": { "file_id": "round.mp4", "mime_type": "video/mp4" } }),
            Err(HTTP_500),
            Some("[Video note] round.mp4, 4 bytes".to_owned()),
            None,
            Some(file("round.mp4", "video/mp4", b"OggS")),
        ),
    ] {
        let voice = FakeVoice::new(heard);
        let e = env_with(Some(voice.clone()));
        let mut rx = line_up(&e);
        let m = owner_message(fields);
        assert_eq!(e.bridge.on_update(&m, now_ms()).await, Handled::Voice);
        settle().await;
        let got = attached_down(&mut rx);
        let want: Vec<_> = text
            .iter()
            .map(|t| (t.clone(), vec![], Vec::from_iter(attached.clone())))
            .collect();
        assert_eq!(got, want, "{m}");
        assert_eq!(e.api.sent_texts(), Vec::from_iter(reply), "{m}");
        assert_eq!(e.bridge.queued_len(), want.len(), "held until acknowledged");
        if m.to_string().contains("hello there") {
            let seen = voice.seen.lock().unwrap().clone();
            assert_eq!(
                seen,
                [
                    "download voice/file_7.oga",
                    "transcribe file_7.oga audio/ogg 4"
                ]
            );
        }
    }
    // No speech-to-text server: the recording still goes, and the user is told it has no text.
    let e = env_of(
        None,
        Some(fake_files(vec![("rec.ogg", b"OggS".to_vec())], true)),
    );
    let mut rx = line_up(&e);
    let m = owner_message(json!({ "voice": { "file_id": "rec.ogg", "mime_type": "audio/ogg" } }));
    assert_eq!(e.bridge.on_update(&m, now_ms()).await, Handled::Voice);
    settle().await;
    assert_eq!(
        attached_down(&mut rx),
        [(
            "[Voice note] (not transcribed: this hub has no speech-to-text server)".to_owned(),
            vec![],
            vec![file("rec.ogg", "audio/ogg", b"OggS")]
        )]
    );
    assert_eq!(e.api.sent_texts(), [VOICE_OFF]);
    // Offline: transcribed anyway, queued with the usual reply, delivered on reconnect.
    let e = env_with(Some(FakeVoice::new(Ok("later"))));
    let m = owner_message(json!({ "voice": note }));
    assert_eq!(e.bridge.on_update(&m, now_ms()).await, Handled::Voice);
    settle().await;
    assert_eq!(e.api.sent_texts(), [QUEUED]);
    let mut rx = line_up(&e);
    e.bridge.flush(now_ms()).await;
    assert_eq!(down(&mut rx), ["[Voice note] later"]);

    // The transcription runs beside the poll loop, and the note keeps its place: a message sent
    // after it waits for it (with no "offline" reply), then both go, in order.
    let voice = FakeVoice::held(Ok("first"));
    let e = env_with(Some(voice.clone()));
    let mut rx = line_up(&e);
    let now = now_ms();
    let first = owner_message(json!({ "voice": note }));
    assert_eq!(e.bridge.on_update(&first, now).await, Handled::Voice);
    let second = update(OWNER, "second", now / 1000);
    assert_eq!(e.bridge.on_update(&second, now).await, Handled::Queued);
    settle().await;
    assert!(down(&mut rx).is_empty(), "nothing overtakes the voice note");
    assert!(e.api.sent_texts().is_empty(), "{:?}", e.api.sent_texts());
    voice.gate.add_permits(1);
    settle().await;
    assert_eq!(down(&mut rx), ["[Voice note] first", "second"]);
    // A note whose transcription fails still goes first, marked, and the user is told.
    let voice = FakeVoice::held(Err(DOWN));
    let e = env_with(Some(voice.clone()));
    let mut rx = line_up(&e);
    e.bridge
        .on_update(&owner_message(json!({ "voice": note })), now)
        .await;
    e.bridge
        .on_update(&update(OWNER, "after", now / 1000), now)
        .await;
    voice.gate.add_permits(1);
    settle().await;
    assert_eq!(down(&mut rx), [unheard, "after"]);
    assert_eq!(e.api.sent_texts(), [down_reply(DOWN)]);
}

#[tokio::test]
async fn pictures_reach_helm_as_images_and_the_ones_it_cannot_show_as_plain_files() {
    let photo = |id: &str, w: u32, h: u32| json!({ "file_id": id, "width": w, "height": h });
    let sizes = json!([
        photo("img-s", 90, 90),
        photo("img-l", 1280, 1280),
        photo("img-xl", 6000, 6000)
    ]);
    let doc = |mime: &str, id: &str, name: &str| json!({ "file_id": id, "mime_type": mime, "file_name": name, "file_size": 1234 });
    let (still, moving) = (png(64, 48, false), png(64, 48, true));
    let gif = b"GIF89a\x40\x00\x30\x00".to_vec();
    let heic = b"\0\0\0\x18ftypheic".to_vec();
    let wide = png(8000, 4000, false);
    let junk = b"this is not a picture at all".to_vec();
    let files = vec![
        ("img-l", jpeg(1280, 960)),
        ("img-xl", jpeg(6000, 6000)),
        ("img-png", still.clone()),
        ("img-apng", moving.clone()),
        ("img-gif", gif.clone()),
        ("img-heic", heic.clone()),
        ("img-wide", wide.clone()),
        ("img-junk", junk.clone()),
    ];
    let (e, _files) = env_with_files(files.clone(), true);
    let mut rx = line_up(&e);
    let reply_to = json!({ "message_id": 3, "text": "Is this right?" });

    // Shown in the chat: the largest size Helm can show (the 36 MP one is over the limit), its
    // caption as the text, "[Photo]" without one, a file's own name, and a quoted reply. Not
    // shown, but passed on as a plain file: a GIF, HEIC, an animated or too large picture, or
    // bytes that are not a picture at all, whatever the message claims.
    let image = |name: &str, mime: &str, bytes: &[u8]| (vec![file(name, mime, bytes)], vec![]);
    let plain = |name: &str, mime: &str, bytes: &[u8]| (vec![], vec![file(name, mime, bytes)]);
    for (fields, text, (images, files)) in [
        (
            json!({ "photo": sizes, "caption": "what is this?" }),
            "what is this?".to_owned(),
            image("photo.jpg", "image/jpeg", &jpeg(1280, 960)),
        ),
        (
            json!({ "photo": sizes }),
            "[Photo]".to_owned(),
            image("photo.jpg", "image/jpeg", &jpeg(1280, 960)),
        ),
        (
            json!({ "document": doc("image/png", "img-png", "shot.png"), "caption": "/helm look" }),
            "look".to_owned(),
            image("shot.png", "image/png", &still),
        ),
        (
            json!({ "document": doc("image/png", "img-png", "shot.png"), "reply_to_message": reply_to }),
            "Replying to: \"Is this right?\"\n[Image]".to_owned(),
            image("shot.png", "image/png", &still),
        ),
        (
            json!({ "document": doc("image/gif", "img-gif", "dance.gif") }),
            format!("[File] dance.gif, {}", size_text(gif.len())),
            plain("dance.gif", "image/gif", &gif),
        ),
        (
            json!({ "document": doc("image/heic", "img-heic", "IMG_1.HEIC"), "caption": "from my phone" }),
            format!(
                "from my phone\n[File] IMG_1.HEIC, {}",
                size_text(heic.len())
            ),
            plain("IMG_1.HEIC", "image/heic", &heic),
        ),
        (
            json!({ "document": doc("image/png", "img-apng", "moving.png") }),
            format!("[File] moving.png, {}", size_text(moving.len())),
            plain("moving.png", "image/png", &moving),
        ),
        (
            json!({ "document": doc("image/png", "img-wide", "wide.png") }),
            format!("[File] wide.png, {}", size_text(wide.len())),
            plain("wide.png", "image/png", &wide),
        ),
        (
            json!({ "document": doc("image/png", "img-junk", "junk.png") }),
            format!("[File] junk.png, {}", size_text(junk.len())),
            plain("junk.png", "image/png", &junk),
        ),
        (
            json!({ "photo": [photo("img-xl", 6000, 6000)] }),
            format!("[File] img-xl, {}", size_text(jpeg(6000, 6000).len())),
            plain("img-xl", "image/jpeg", &jpeg(6000, 6000)),
        ),
    ] {
        let m = owner_message(fields);
        assert_eq!(
            e.bridge.on_update(&m, now_ms()).await,
            Handled::Image,
            "{m}"
        );
        settle().await;
        assert_eq!(attached_down(&mut rx), [(text, images, files)], "{m}");
        assert!(e.api.sent_texts().is_empty(), "{:?}", e.api.sent_texts());
    }
    assert_eq!(e.bridge.queued_len(), 10, "held until acknowledged");

    // Refused with a reason and nothing sent to Helm: at once from what the message says, or
    // after the fetch.
    for (fields, at_once, why) in [
        (
            json!({ "document": { "file_id": "x", "mime_type": "image/png", "file_size": 21 << 20 } }),
            true,
            TOO_LARGE,
        ),
        (
            json!({ "photo": [photo("img-huge", 640, 480)] }),
            false,
            TOO_LARGE,
        ),
        (
            json!({ "document": doc("image/png", "img-gone", "gone.png") }),
            false,
            "the download from Telegram failed (HTTP 404)",
        ),
    ] {
        e.api.clear();
        let m = owner_message(fields);
        let want = if at_once {
            Handled::Command
        } else {
            Handled::Image
        };
        assert_eq!(e.bridge.on_update(&m, now_ms()).await, want, "{m}");
        settle().await;
        assert_eq!(e.api.sent_texts(), [format!("{IMAGE_FAILED}{why}")], "{m}");
        assert!(attached_down(&mut rx).is_empty(), "{m}");
    }
    // A message with no file and no text (a location) is not passed on, and files can be off.
    e.api.clear();
    let here = owner_message(json!({ "location": { "latitude": 51.5, "longitude": 0.1 } }));
    assert_eq!(e.bridge.on_update(&here, now_ms()).await, Handled::Command);
    assert_eq!(e.api.sent_texts(), [TEXT_ONLY]);
    let off = env();
    let m = owner_message(json!({ "photo": sizes }));
    off.bridge.on_update(&m, now_ms()).await;
    assert_eq!(off.api.sent_texts(), [FILES_OFF]);

    // The picture keeps its place: a message sent after it waits, then both go in order. Offline,
    // it is queued with the usual reply and goes with its picture on reconnect.
    let (e, files) = env_with_files(files, false);
    let now = now_ms();
    let first = owner_message(json!({ "photo": sizes, "caption": "first" }));
    assert_eq!(e.bridge.on_update(&first, now).await, Handled::Image);
    let second = update(OWNER, "second", now / 1000);
    assert_eq!(e.bridge.on_update(&second, now).await, Handled::Queued);
    settle().await;
    assert_eq!(e.api.sent_texts(), [QUEUED]);
    files.gate.add_permits(1);
    settle().await;
    let mut rx = line_up(&e);
    e.bridge.flush(now_ms()).await;
    let got = attached_down(&mut rx);
    assert_eq!(
        got.iter()
            .map(|(t, i, _)| (t.as_str(), i.len()))
            .collect::<Vec<_>>(),
        [("first", 1), ("second", 0)]
    );
}

#[tokio::test]
async fn every_file_reaches_helm_and_an_album_goes_as_one_message() {
    let report = vec![7u8; 1_258_291];
    let small = |n: u8| vec![n; 5];
    let files = vec![
        ("report.pdf", report.clone()),
        ("notes.txt", small(1)),
        ("clip.mp4", small(2)),
        ("anim.mp4", small(3)),
        ("song.mp3", small(4)),
        ("img-l", jpeg(1280, 960)),
    ];
    let (e, _files) = env_with_files(files.clone(), true);
    let mut rx = line_up(&e);
    let doc = |id: &str, mime: &str| json!({ "file_id": id, "file_name": id, "mime_type": mime });
    let pdf = doc("report.pdf", "application/pdf");
    // (fields, how it is handled, the text, the file)
    for (fields, handled, text, sent) in [
        (
            json!({ "document": pdf, "caption": "please summarise" }),
            Handled::File,
            "please summarise\n[File] report.pdf, 1.2 MB",
            file("report.pdf", "application/pdf", &report),
        ),
        (
            json!({ "document": doc("notes.txt", "text/plain") }),
            Handled::File,
            "[File] notes.txt, 5 bytes",
            file("notes.txt", "text/plain", &small(1)),
        ),
        (
            json!({ "video": { "file_id": "clip.mp4", "mime_type": "video/mp4" }, "caption": "look" }),
            Handled::File,
            "look\n[Video] clip.mp4, 5 bytes",
            file("clip.mp4", "video/mp4", &small(2)),
        ),
        (
            json!({ "animation": { "file_id": "anim.mp4", "mime_type": "video/mp4" },
                    "document": { "file_id": "anim.mp4", "mime_type": "video/mp4" } }),
            Handled::File,
            "[Animation] anim.mp4, 5 bytes",
            file("anim.mp4", "video/mp4", &small(3)),
        ),
        // With no speech-to-text server an audio file is just a file, without a reply.
        (
            json!({ "audio": doc("song.mp3", "audio/mpeg") }),
            Handled::Voice,
            "[Audio] song.mp3, 5 bytes",
            file("song.mp3", "audio/mpeg", &small(4)),
        ),
    ] {
        let m = owner_message(fields);
        assert_eq!(e.bridge.on_update(&m, now_ms()).await, handled, "{m}");
        settle().await;
        assert_eq!(
            attached_down(&mut rx),
            [(text.to_owned(), vec![], vec![sent])],
            "{m}"
        );
        assert!(e.api.sent_texts().is_empty(), "{:?}", e.api.sent_texts());
    }
    // Too large to download: a plain reply naming the file, and nothing sent.
    let big = json!({ "document": { "file_id": "big.zip", "file_name": "big.zip", "file_size": 21 << 20 } });
    assert_eq!(
        e.bridge.on_update(&owner_message(big), now_ms()).await,
        Handled::Command
    );
    assert_eq!(
        e.api.sent_texts(),
        [format!("big.zip was not sent to Helm: {TOO_LARGE}")]
    );

    // An album: the files sent together go as one message once the window after the last one
    // has passed, in the order sent, with the caption one of them carried.
    e.api.clear();
    let now = now_ms();
    let album = |fields: Value, group: &str| {
        let mut m = owner_message(fields);
        m["message"]["media_group_id"] = json!(group);
        m
    };
    let photo = json!([{ "file_id": "img-l", "width": 1280, "height": 960 }]);
    for fields in [
        json!({ "photo": photo }),
        json!({ "document": pdf, "caption": "the trip" }),
        json!({ "video": { "file_id": "clip.mp4", "mime_type": "video/mp4" } }),
    ] {
        e.bridge.on_update(&album(fields, "g1"), now).await;
    }
    settle().await;
    assert!(
        attached_down(&mut rx).is_empty(),
        "the album window is open"
    );
    e.bridge.flush(now + 1000).await;
    assert!(attached_down(&mut rx).is_empty(), "still open");
    e.bridge.flush(now + 2000).await;
    assert_eq!(
        attached_down(&mut rx),
        [(
            "the trip\n[File] report.pdf, 1.2 MB\n[Video] clip.mp4, 5 bytes".to_owned(),
            vec![file("photo.jpg", "image/jpeg", &jpeg(1280, 960))],
            vec![
                file("report.pdf", "application/pdf", &report),
                file("clip.mp4", "video/mp4", &small(2))
            ]
        )]
    );
    assert!(e.api.sent_texts().is_empty(), "{:?}", e.api.sent_texts());
    // At most ten files in one message: the eleventh is left out, and the user is told.
    for _ in 0..11 {
        let m = album(json!({ "document": doc("notes.txt", "text/plain") }), "g2");
        e.bridge.on_update(&m, now).await;
    }
    settle().await;
    e.bridge.flush(now + 2000).await;
    let got = attached_down(&mut rx);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].2.len(), 10);
    assert_eq!(got[0].0.lines().count(), 10, "{}", got[0].0);
    assert_eq!(
        e.api.sent_texts(),
        ["notes.txt was left out: a message to Helm carries at most 10 files."]
    );
    // The sizes the message gives are judged before anything is downloaded: a third file said to
    // be 20 MB is left out at once.
    e.api.clear();
    for want in [Handled::File, Handled::File, Handled::Command] {
        let doc =
            json!({ "file_id": "notes.txt", "file_name": "notes.txt", "file_size": 20 << 20 });
        let m = album(json!({ "document": doc }), "g4");
        assert_eq!(e.bridge.on_update(&m, now).await, want);
    }
    settle().await;
    e.bridge.flush(now + 2000).await;
    assert_eq!(attached_down(&mut rx)[0].2.len(), 2);
    assert_eq!(
        e.api.sent_texts(),
        ["notes.txt was left out: a message to Helm carries at most 50.0 MB of files."]
    );
    // Only two files are fetched at once; the rest wait their turn.
    let (e, files) = env_with_files(vec![("notes.txt", small(1))], false);
    let mut rx = line_up(&e);
    for _ in 0..5 {
        let m = album(json!({ "document": doc("notes.txt", "text/plain") }), "g5");
        e.bridge.on_update(&m, now).await;
    }
    settle().await;
    assert_eq!(files.started.load(Ordering::Relaxed), 2);
    files.gate.add_permits(100);
    settle().await;
    assert_eq!(files.started.load(Ordering::Relaxed), 5);
    e.bridge.flush(now + 2000).await;
    assert_eq!(attached_down(&mut rx)[0].2.len(), 5);
    // And at most 50 MB of what arrives, when the message gives no sizes: the third 20 MB file
    // is left out.
    let (e, _files) = env_with_files(vec![("big.bin", vec![0u8; 20 << 20])], true);
    let mut rx = line_up(&e);
    for _ in 0..3 {
        let fields = json!({ "document": { "file_id": "big.bin", "file_name": "big.bin" } });
        e.bridge.on_update(&album(fields, "g3"), now).await;
    }
    let mut got = vec![];
    for _ in 0..500 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        e.bridge.flush(now + 2000).await;
        got = attached_down(&mut rx);
        if !got.is_empty() {
            break;
        }
    }
    assert_eq!(got.iter().map(|g| g.2.len()).collect::<Vec<_>>(), [2]);
    assert_eq!(
        e.api.sent_texts(),
        ["big.bin was left out: a message to Helm carries at most 50.0 MB of files."]
    );
}

#[tokio::test]
async fn a_reply_forwards_what_it_quotes_on_its_own_first_line() {
    let e = env();
    let mut rx = line_up(&e);
    let whole = json!({ "message_id": 3, "text": "the whole message" });
    let long = "x".repeat(400);
    for (typed, fields, want) in [
        (
            "yes",
            json!({ "quote": { "text": "part", "position": 4 }, "reply_to_message": whole }),
            "Replying to: \"part\"\nyes".to_owned(),
        ),
        (
            "yes",
            json!({ "reply_to_message": whole }),
            "Replying to: \"the whole message\"\nyes".into(),
        ),
        (
            "yes",
            json!({ "reply_to_message": { "message_id": 3, "text": "two\n  lines" } }),
            "Replying to: \"two lines\"\nyes".into(),
        ),
        (
            "yes",
            json!({ "reply_to_message": { "message_id": 3, "caption": "a photo" } }),
            "Replying to: \"a photo\"\nyes".into(),
        ),
        (
            "yes",
            json!({ "reply_to_message": { "message_id": 3, "text": long } }),
            format!("Replying to: \"{}…\"\nyes", "x".repeat(299)),
        ),
        (
            "yes",
            json!({ "reply_to_message": { "message_id": 3, "photo": [] } }),
            "yes".into(),
        ),
        (
            "/helm yes",
            json!({ "reply_to_message": whole }),
            "Replying to: \"the whole message\"\nyes".into(),
        ),
        // A command stays a command.
        (
            "/stop",
            json!({ "reply_to_message": whole }),
            "telegram_stop".into(),
        ),
    ] {
        let u = with_fields(update(OWNER, typed, T0 / 1000), fields);
        e.bridge.on_update(&u, T0).await;
        assert_eq!(down(&mut rx), [want.as_str()], "{u}");
    }
}

#[tokio::test]
async fn approval_buttons_answer_helm_once_and_only_for_the_allowed_user() {
    let e = env();
    let app = hub(&e, true);
    const ASK: &str = "0123456789abcdef0123456789abcdef";
    let ask = |id: &str| Upstream::TelegramAsk {
        id: id.into(),
        text: "Run the migration?".into(),
        options: vec!["Allow".into(), "Deny".into(), "z".repeat(50)],
    };
    e.bridge.on_upstream(ask(ASK)).await;
    e.bridge.on_upstream(ask(ASK)).await; // the same id again: already on the phone
    let calls = e.api.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let (method, sent) = &calls[0];
    assert_eq!(
        (method.as_str(), &sent["disable_notification"]),
        ("sendMessage", &json!(false))
    );
    let rows = sent["reply_markup"]["inline_keyboard"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "one button per row");
    assert_eq!(
        rows[1],
        json!([{ "text": "Deny", "callback_data": format!("a:{ASK}:1") }])
    );
    assert_eq!(rows[2][0]["text"].as_str().unwrap().chars().count(), 40);
    let mid = *e.api.next_id.lock().unwrap();
    let tap = |from: i64, chat_type: &str, mid: i64, data: &str| {
        let n = NEXT_UPDATE.fetch_add(1, Ordering::Relaxed);
        json!({ "update_id": n, "callback_query": {
            "id": format!("q{n}"), "chat_instance": "c", "data": data,
            "from": { "id": from, "is_bot": false, "first_name": "x" },
            "message": { "message_id": mid, "date": 0,
                         "chat": { "id": from, "type": chat_type } } } })
    };
    let methods = |e: &Env| {
        e.api
            .calls()
            .into_iter()
            .map(|(m, b)| (m, b.get("text").cloned()))
            .collect::<Vec<_>>()
    };
    let answer = |text: Option<&str>| ("answerCallbackQuery".to_owned(), text.map(|t| json!(t)));
    let data = format!("a:{ASK}:1");

    // A stranger's tap, or the owner's in a group: nothing at all.
    e.api.clear();
    for t in [
        tap(STRANGER, "private", mid, &data),
        tap(OWNER, "group", mid, &data),
    ] {
        assert_eq!(e.bridge.on_update(&t, T0).await, Handled::Ignored);
    }
    assert!(e.api.calls().is_empty());
    // The line is down: Helm hears nothing and the buttons stay.
    assert_eq!(
        e.bridge
            .on_update(&tap(OWNER, "private", mid, &data), T0)
            .await,
        Handled::Button
    );
    assert_eq!(methods(&e), [answer(Some(TAP_OFFLINE))]);
    // Data that is no button of this ask: answered with nothing.
    let mut rx = line_up(&e);
    for junk in [
        "nope".to_owned(),
        format!("a:{ASK}:7"),
        "a:XYZ:1".to_owned(),
    ] {
        e.api.clear();
        assert_eq!(
            e.bridge
                .on_update(&tap(OWNER, "private", mid, &junk), T0)
                .await,
            Handled::Button
        );
        assert_eq!(methods(&e), [answer(None)], "{junk}");
    }
    assert!(down(&mut rx).is_empty());
    // The tap, posted by a forwarder: Helm hears it once and the message shows the choice.
    e.api.clear();
    let tapped = tap(OWNER, "private", mid, &data);
    let (_, v) = post_update(&app, Some(FORWARDER), tapped.to_string()).await;
    assert_eq!(v["handled"], json!(["button"]));
    assert_eq!(
        frames(&mut rx),
        [json!({ "type": "telegram_answer", "id": ASK, "index": 1 })]
    );
    let calls = e.api.calls();
    assert_eq!(methods(&e)[0], answer(Some(TAP_SENT)));
    assert_eq!(
        calls[1],
        (
            "editMessageText".to_owned(),
            json!({ "chat_id": OWNER, "message_id": mid, "text": "Run the migration?\n\n→ Deny" })
        )
    );
    // The same update again is a duplicate; a fresh tap finds the ask gone.
    let (_, v) = post_update(&app, Some(FORWARDER), tapped.to_string()).await;
    assert_eq!(v["handled"], json!(["duplicate"]));
    e.api.clear();
    e.bridge
        .on_update(&tap(OWNER, "private", mid, &data), T0)
        .await;
    assert_eq!(methods(&e)[0], answer(Some(TAP_STALE)));
    assert_eq!(
        e.api.calls()[1],
        (
            "editMessageReplyMarkup".to_owned(),
            json!({ "chat_id": OWNER, "message_id": mid })
        )
    );
    assert!(down(&mut rx).is_empty(), "sent once");
    // Text never answers an ask.
    e.bridge
        .on_update(&update(OWNER, "Allow", T0 / 1000), T0)
        .await;
    assert_eq!(down(&mut rx), ["Allow"]);

    // Settled in Helm: the message gains the outcome and loses its buttons; a later tap is stale.
    const ASK2: &str = "fedcba9876543210";
    e.bridge.on_upstream(ask(ASK2)).await;
    let mid2 = *e.api.next_id.lock().unwrap();
    e.api.clear();
    for text in ["Allowed in Helm", "again"] {
        let done = Upstream::TelegramAskDone {
            id: ASK2.into(),
            text: text.into(),
        };
        e.bridge.on_upstream(done).await;
    }
    assert_eq!(
        e.api.calls(),
        [(
            "editMessageText".to_owned(),
            json!({ "chat_id": OWNER, "message_id": mid2,
                    "text": "Run the migration?\n\nAllowed in Helm" })
        )]
    );
    e.api.clear();
    e.bridge
        .on_update(&tap(OWNER, "private", mid2, &format!("a:{ASK2}:0")), T0)
        .await;
    assert_eq!(methods(&e)[0], answer(Some(TAP_STALE)));
    // At most 50 asks wait: the oldest is forgotten.
    let ids: Vec<String> = (0..51).map(|i| format!("{i:016x}")).collect();
    for id in &ids {
        e.bridge.on_upstream(ask(id)).await;
    }
    let first_mid = *e.api.next_id.lock().unwrap() - 50;
    e.api.clear();
    e.bridge
        .on_update(
            &tap(OWNER, "private", first_mid, &format!("a:{}:0", ids[0])),
            T0,
        )
        .await;
    assert_eq!(methods(&e)[0], answer(Some(TAP_STALE)));
    e.bridge
        .on_update(
            &tap(OWNER, "private", first_mid + 1, &format!("a:{}:0", ids[1])),
            T0,
        )
        .await;
    assert_eq!(down(&mut rx), ["telegram_answer"]);
}

#[tokio::test]
async fn http_voice_downloads_the_file_and_posts_a_multipart_form() {
    const TOKEN: &str = "123456:fake-bot-token-for-tests";
    let answer = Arc::new(Mutex::new((200u16, String::new())));
    let uploads = Arc::new(Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let (a, u) = (answer.clone(), uploads.clone());
    let app = axum::Router::new()
        .route(
            "/file/{*path}",
            axum::routing::get(
                |axum::extract::Path(path): axum::extract::Path<String>| async move {
                    match path == format!("bot{TOKEN}/voice/file_1.oga") {
                        true => (StatusCode::OK, b"OggS fake audio".to_vec()),
                        false => (StatusCode::NOT_FOUND, b"Not Found".to_vec()),
                    }
                },
            ),
        )
        .route(
            "/slow",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                "too late"
            }),
        )
        .route(
            "/stt",
            axum::routing::post(move |headers: HeaderMap, body: Bytes| async move {
                let kind = headers[header::CONTENT_TYPE].to_str().unwrap().to_owned();
                u.lock().unwrap().push((kind, body.to_vec()));
                let (status, text) = a.lock().unwrap().clone();
                (StatusCode::from_u16(status).unwrap(), text)
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let stt = |url: String| SttConfig {
        url,
        model: "large-v3-turbo".into(),
        language: "en".into(),
        timeout: Duration::from_secs(60),
    };
    let voice = HttpVoice::new(&base, TOKEN, &stt(format!("{base}/stt")));

    assert_eq!(
        voice.download("voice/file_1.oga").await.unwrap(),
        b"OggS fake audio"
    );
    let missing = voice.download("voice/missing.oga").await.unwrap_err();
    assert!(
        missing.contains("HTTP 404") && !missing.contains(TOKEN),
        "{missing}"
    );

    let long = "e".repeat(1000);
    for (status, body, want) in [
        (200, "hello there\n", Ok("hello there")),
        (200, "{\"text\": \" hi \"}", Ok("hi")),
        // The server's own markers in a long recording are passed on as written.
        (
            200,
            "one [gap: 00:10-00:12] two [truncated: past 60 min]\n",
            Ok("one [gap: 00:10-00:12] two [truncated: past 60 min]"),
        ),
        (
            500,
            "model not loaded",
            Err("the speech-to-text server answered HTTP 500: model not loaded"),
        ),
        (
            503,
            long.as_str(),
            Err("the speech-to-text server answered HTTP 503: eee"),
        ),
        (
            200,
            " \n",
            Err("the speech-to-text server returned no text"),
        ),
        (
            200,
            "{\"error\": {\"message\": \"bad audio\"}}",
            Err("the speech-to-text server reported an error"),
        ),
    ] {
        *answer.lock().unwrap() = (status, body.to_owned());
        let got = voice
            .transcribe(b"OggS fake audio".to_vec(), "file_1.oga", "audio/ogg")
            .await;
        match (got, want) {
            (Ok(t), Ok(w)) => assert_eq!(t, w),
            (Err(e), Err(w)) => assert!(e.starts_with(w) && e.chars().count() < 400, "{e}"),
            (got, want) => panic!("{status} {body:?}: {got:?}, wanted {want:?}"),
        }
    }
    let (kind, body) = uploads.lock().unwrap()[0].clone();
    let boundary = kind
        .strip_prefix("multipart/form-data; boundary=")
        .expect(&kind)
        .to_owned();
    let body = String::from_utf8(body).unwrap();
    for part in [
        "Content-Disposition: form-data; name=\"file\"; filename=\"file_1.oga\"\r\n\
         Content-Type: audio/ogg\r\n\r\nOggS fake audio\r\n",
        "Content-Disposition: form-data; name=\"model\"\r\n\r\nlarge-v3-turbo\r\n",
        "Content-Disposition: form-data; name=\"language\"\r\n\r\nen\r\n",
        "Content-Disposition: form-data; name=\"response_format\"\r\n\r\ntext\r\n",
    ] {
        assert!(
            body.contains(&format!("--{boundary}\r\n{part}")),
            "{part:?} in {body:?}"
        );
    }
    assert!(body.ends_with(&format!("--{boundary}--\r\n")), "{body:?}");
    // A server that takes longer than its timeout is said to have run out of time.
    let slow = SttConfig {
        timeout: Duration::from_secs(1),
        ..stt(format!("{base}/slow"))
    };
    let late = HttpVoice::new(&base, TOKEN, &slow)
        .transcribe(vec![1], "file_1.oga", "audio/ogg")
        .await;
    assert_eq!(
        late,
        Err("the speech-to-text server did not finish within 1 minute".into())
    );

    let files = HttpFiles::new(&base, TOKEN);
    assert_eq!(
        files.download("voice/file_1.oga").await.unwrap(),
        b"OggS fake audio"
    );

    // Nothing listening: a plain reason, and never the token.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let gone = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let refused = HttpFiles::new(&gone, TOKEN).download("photos/x.jpg").await;
    assert!(!refused.unwrap_err().contains(TOKEN));
    let voice = HttpVoice::new(&gone, TOKEN, &stt(format!("{gone}/stt")));
    let (download, heard) = tokio::join!(
        voice.download("voice/file_1.oga"),
        voice.transcribe(vec![1], "file_1.oga", "audio/ogg")
    );
    let download = download.unwrap_err();
    assert!(
        download.starts_with("the download from Telegram failed") && !download.contains(TOKEN),
        "{download}"
    );
    let heard = heard.unwrap_err();
    assert!(
        heard.starts_with("the speech-to-text server could not be reached"),
        "{heard}"
    );
}
