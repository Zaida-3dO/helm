//! The phone's voice line over real sockets: a fake phone (HTTP and `/voice/live`), a fake PC
//! (`/relay`) and a fake speech-to-text server, against the real router. The rules inside the
//! bridge (queue limits, outbox caps, ordering) have their unit tests in `src/voice.rs`; these
//! walk the routes, the statuses and the frames of the protocol. Time-dependent steps (the 45 s
//! resend, the queue and outbox lifetimes, the 60 s attachment wait) are driven by ticking the
//! bridge with a later `now`, not by waiting. All text is synthetic.

use axum::body::Bytes;
use axum::http::{header, HeaderMap, StatusCode};
use futures_util::{SinkExt, StreamExt};
use helm_hub::lines::Lines;
use helm_hub::mcp::Options;
use helm_hub::registry::Registry;
use helm_hub::server::{router, AppState};
use helm_hub::store::Store;
use helm_hub::stt::{Stt, SttConfig};
use helm_hub::telegram::now_ms;
use helm_hub::voice::{VoiceBridge, VoiceConfig, EXPIRED, STT_FAILED};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const LINE: &str = "line-token-0123456789abcdefghij";
const PHONE: &str = "phone-token-0123456789abcdefghij";
const TABLET: &str = "tablet-token-0123456789abcdefghi";
const FORWARDER: &str = "forwarder-token-0123456789abcdef";
const NOTES: &str = "notes-token-0123456789abcdefghij";
const NOBODY: &str = "not-a-token-0123456789abcdefgh";

struct Opts {
    voice: bool,
    stt: bool,
    queue_max: usize,
    queue_lifetime: Duration,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            voice: true,
            stt: true,
            queue_max: 20,
            queue_lifetime: Duration::from_secs(30 * 60),
        }
    }
}

/// The fake speech-to-text server: answers what it is told to and keeps what it was sent.
#[allow(clippy::type_complexity)]
struct FakeStt {
    url: String,
    answer: Arc<Mutex<(u16, String)>>,
    uploads: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
}

impl FakeStt {
    async fn start() -> FakeStt {
        let answer = Arc::new(Mutex::new((200u16, "Synthetic words.".to_owned())));
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let (a, u) = (answer.clone(), uploads.clone());
        let app = axum::Router::new().route(
            "/v1/audio/transcriptions",
            axum::routing::post(move |headers: HeaderMap, body: Bytes| async move {
                let kind = headers
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                u.lock().unwrap().push((kind, body.to_vec()));
                let (status, text) = a.lock().unwrap().clone();
                (StatusCode::from_u16(status).unwrap(), text)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1/audio/transcriptions",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        FakeStt {
            url,
            answer,
            uploads,
        }
    }

    fn says(&self, status: u16, text: &str) {
        *self.answer.lock().unwrap() = (status, text.to_owned());
    }

    /// The multipart form of the n-th upload as text (the recording's bytes are in it too).
    fn upload(&self, n: usize) -> (String, String) {
        let (kind, body) = self.uploads.lock().unwrap()[n].clone();
        (kind, String::from_utf8_lossy(&body).into_owned())
    }
}

struct Hub {
    addr: String,
    bridge: Option<Arc<VoiceBridge>>,
    stt: FakeStt,
}

impl Hub {
    fn bridge(&self) -> &VoiceBridge {
        self.bridge.as_ref().expect("the voice line is on")
    }
}

/// One hub with a relay token, two phones (voice tokens of the same person), a telegram
/// forwarder and a memory client.
async fn start(o: Opts) -> Hub {
    let registry = Registry::from_legacy_spec(
        &format!(
            "helm-line:{LINE}:relay, phone:{PHONE}:voice, tablet:{TABLET}:voice, \
             forwarder:{FORWARDER}:telegram, notes:{NOTES}:rw"
        ),
        "owner",
    )
    .unwrap();
    let registry = Arc::new(Mutex::new(registry));
    let (lines, _upstream) = Lines::new(registry.clone());
    let lines = Arc::new(lines);
    let stt = FakeStt::start().await;
    let mut state = AppState::new(
        Store::open_in_memory().unwrap(),
        registry,
        Options {
            allow_mutate: false,
        },
        lines.clone(),
        None,
    );
    let mut bridge = None;
    if o.voice {
        let mut b = VoiceBridge::new(
            lines.clone(),
            VoiceConfig {
                queue_max: o.queue_max,
                queue_lifetime: o.queue_lifetime,
                ..VoiceConfig::default()
            },
        );
        if o.stt {
            b = b.with_stt(Arc::new(Stt::new(&SttConfig {
                url: stt.url.clone(),
                model: "fake-model".into(),
                language: "en".into(),
                timeout: Duration::from_secs(30),
            })));
        }
        // The test's Helm line never answers the call that frees the graphics memory.
        let b = Arc::new(b.with_room_wait(Duration::from_millis(300)));
        tokio::spawn(b.clone().run(lines.voice_receiver()));
        state = state.with_voice(b.clone());
        bridge = Some(b);
    }
    let app = router(Arc::new(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Hub { addr, bridge, stt }
}

// ----- HTTP -----

struct Reply {
    status: u16,
    head: String,
    body: Value,
}

/// A tiny HTTP/1.1 client over the socket, so the test needs no HTTP client crate. An empty
/// `ctype` sends none.
async fn http(
    hub: &Hub,
    method: &str,
    target: &str,
    token: Option<&str>,
    ctype: &str,
    body: &[u8],
) -> Reply {
    let mut req = format!(
        "{method} {target} HTTP/1.1\r\nhost: hub\r\nconnection: close\r\ncontent-length: {}\r\n",
        body.len()
    );
    if let Some(t) = token {
        req.push_str(&format!("authorization: Bearer {t}\r\n"));
    }
    if !ctype.is_empty() {
        req.push_str(&format!("content-type: {ctype}\r\n"));
    }
    req.push_str("\r\n");
    let mut bytes = req.into_bytes();
    bytes.extend_from_slice(body);
    let mut s = tokio::net::TcpStream::connect(&hub.addr).await.unwrap();
    s.write_all(&bytes).await.unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    let text = String::from_utf8_lossy(&out).into_owned();
    let status: u16 = text[9..12].parse().unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    Reply {
        status,
        head: head.to_owned(),
        body: serde_json::from_str(body.trim()).unwrap_or(Value::Null),
    }
}

/// Wait until the fake speech-to-text server has been sent `n` recordings.
async fn uploads_reach(hub: &Hub, n: usize) {
    for _ in 0..50 {
        if hub.stt.uploads.lock().unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the speech-to-text server was not sent {n} recording(s)");
}

fn typed(text: &str) -> Vec<u8> {
    json!({ "text": text }).to_string().into_bytes()
}

fn id(kind: &str, n: u32) -> String {
    format!("{kind}-{n:04}")
}

/// `POST /voice/turn` as the phone makes it; `extra` is more query (`&continues=...`).
async fn post_turn(hub: &Hub, turn: &str, extra: &str, ctype: &str, body: &[u8]) -> Reply {
    let target = format!("/voice/turn?turn={turn}&mode=conversation{extra}");
    http(hub, "POST", &target, Some(PHONE), ctype, body).await
}

/// An OGG file that says it is `seconds` long (the page position at 48 kHz).
fn ogg(seconds: u32) -> Vec<u8> {
    let mut b = b"OggS\0\0".to_vec();
    b.extend_from_slice(&(i64::from(seconds) * 48_000).to_le_bytes());
    b.extend_from_slice(&[0; 20]);
    b
}

/// A 16 kHz mono 16-bit WAV with `data` bytes of silence.
fn wav(data: usize) -> Vec<u8> {
    let mut w = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&[1, 0, 1, 0]);
    w.extend_from_slice(&16_000u32.to_le_bytes());
    w.extend_from_slice(&32_000u32.to_le_bytes());
    w.extend_from_slice(&[2, 0, 16, 0]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data as u32).to_le_bytes());
    w.extend(std::iter::repeat_n(0u8, data));
    w
}

// ----- WebSockets -----

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(hub: &Hub, path: &str, token: Option<&str>) -> Result<Ws, u16> {
    let mut req = format!("ws://{}{path}", hub.addr)
        .into_client_request()
        .unwrap();
    if let Some(t) = token {
        req.headers_mut()
            .insert("authorization", format!("Bearer {t}").parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => Err(resp.status().as_u16()),
        Err(e) => panic!("{e}"),
    }
}

async fn send(ws: &mut Ws, frame: Value) {
    ws.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}

/// The next text frame the test is waiting for, skipping the others.
async fn next_where(ws: &mut Ws, what: &str, want: impl Fn(&Value) -> bool) -> Value {
    let mut skipped: Vec<String> = Vec::new();
    loop {
        let got = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
        match got {
            Err(_) => panic!("no {what} within 5 s; skipped {skipped:?}"),
            Ok(Some(Ok(Message::Text(t)))) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                if want(&v) {
                    return v;
                }
                skipped.push(v.to_string());
            }
            Ok(Some(Ok(_))) => {}
            Ok(other) => panic!("the socket ended waiting for {what}: {other:?}; {skipped:?}"),
        }
    }
}

fn is(kind: &'static str) -> impl Fn(&Value) -> bool {
    move |f| f["type"] == kind
}

fn turn_is(turn: String, state: &'static str) -> impl Fn(&Value) -> bool {
    move |f| f["type"] == "turn" && f["turn"] == turn.as_str() && f["state"] == state
}

/// Everything that arrives within `ms` milliseconds.
async fn drain(ws: &mut Ws, ms: u64) -> Vec<Message> {
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    let mut out = Vec::new();
    while let Ok(Some(Ok(m))) = tokio::time::timeout_at(end, ws.next()).await {
        out.push(m);
    }
    out
}

fn frames(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
            _ => None,
        })
        .collect()
}

/// The code and reason of a close frame among `messages`.
fn closed_with(messages: &[Message]) -> Option<(u16, String)> {
    messages.iter().find_map(|m| match m {
        Message::Close(Some(f)) => Some((u16::from(f.code), f.reason.as_str().to_owned())),
        _ => None,
    })
}

/// Helm's relay line, up.
async fn pc(hub: &Hub) -> Ws {
    let mut ws = connect(hub, "/relay", Some(LINE)).await.expect("the line");
    next_where(&mut ws, "welcome", is("welcome")).await;
    ws
}

/// A phone's live socket, with its `hello` sent and the `welcome` read.
async fn phone(hub: &Hub, token: &str, last_seq: u64, live: bool) -> (Ws, Value) {
    let mut ws = connect(hub, "/voice/live", Some(token))
        .await
        .expect("the live socket");
    let hello = json!({ "type": "hello", "lastSeq": last_seq, "live": live,
                        "mode": "conversation", "app": "test/1" });
    send(&mut ws, hello).await;
    let welcome = next_where(&mut ws, "welcome", is("welcome")).await;
    (ws, welcome)
}

/// A `voice_final` (a reply to `turn`) or, with no turn, a `voice_notify`.
fn answer(reply: &str, turn: Option<&str>, text: &str, attachments: u32) -> Value {
    json!({
        "type": if turn.is_some() { "voice_final" } else { "voice_notify" },
        "id": reply, "turn": turn, "mode": "conversation",
        "sentences": [{"n": 1, "text": text}, {"n": 2, "text": "Want the details?"}],
        "text": format!("{text} Want the details?"), "truncated": false,
        "conversation": "c-synthetic", "at": 1_800_000_000_000i64, "attachments": attachments,
        "about": if turn.is_some() { Value::Null } else { json!({"chat": "Build"}) },
    })
}

fn link(reply: &str, n: u32) -> Value {
    json!({ "type": "voice_attachment", "reply": reply, "n": n, "kind": "link",
            "title": "The page", "url": "https://example.test/page" })
}

// ----- the tests -----

#[tokio::test]
async fn every_voice_route_needs_a_voice_token_and_other_routes_refuse_it() {
    let hub = start(Opts::default()).await;
    let body_heard = br#"{"reports":[{"reply":"reply-0001","state":"all"}]}"#.to_vec();
    // (method, target, content type, body, what a voice token gets)
    let routes: [(&str, &str, &str, Vec<u8>, u16); 4] = [
        (
            "POST",
            "/voice/turn?turn=turn-0001&mode=chat",
            "application/json",
            typed("Synthetic hello."),
            202,
        ),
        ("POST", "/voice/heard", "application/json", body_heard, 200),
        ("GET", "/voice/status", "", vec![], 200),
        ("POST", "/voice/fresh", "", vec![], 409),
    ];
    for (method, target, ctype, body, allowed) in &routes {
        for (token, want) in [
            (None, 401),
            (Some(NOBODY), 401),
            (Some(LINE), 403),
            (Some(NOTES), 403),
            (Some(FORWARDER), 403),
            (Some(PHONE), *allowed),
        ] {
            let r = http(&hub, method, target, token, ctype, body).await;
            assert_eq!(r.status, want, "{method} {target} with {token:?}");
            if want == 401 {
                let head = r.head.to_ascii_lowercase();
                assert!(head.contains("www-authenticate: bearer"), "{head}");
            }
            if want == 403 {
                assert_eq!(r.body["error"], "only a voice token may use /voice");
            }
        }
    }
    // The live socket: the same refusals before the upgrade.
    for (token, want) in [
        (None, 401),
        (Some(NOBODY), 401),
        (Some(LINE), 403),
        (Some(FORWARDER), 403),
    ] {
        assert_eq!(connect(&hub, "/voice/live", token).await.err(), Some(want));
    }
    assert!(connect(&hub, "/voice/live", Some(PHONE)).await.is_ok());

    // And a voice token opens nothing else: not the line, not the memory surface, not the
    // Telegram forwarder's endpoint.
    assert_eq!(connect(&hub, "/relay", Some(PHONE)).await.err(), Some(403));
    for (target, body, want) in [
        (
            "/mcp",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            "cannot call the MCP surface",
        ),
        (
            "/telegram/update",
            "[]",
            "only a telegram token may post updates",
        ),
    ] {
        let r = http(
            &hub,
            "POST",
            target,
            Some(PHONE),
            "application/json",
            body.as_bytes(),
        )
        .await;
        assert_eq!(r.status, 403, "{target}");
        assert!(r.body["error"].as_str().unwrap().contains(want), "{target}");
    }

    // With the voice line switched off the routes answer 404, and Telegram is unaffected.
    let off = start(Opts {
        voice: false,
        ..Opts::default()
    })
    .await;
    let r = http(&off, "GET", "/voice/status", Some(PHONE), "", &[]).await;
    assert_eq!(r.status, 404);
    assert_eq!(
        connect(&off, "/voice/live", Some(PHONE)).await.err(),
        Some(404)
    );
    let r = post_turn(&off, "turn-0001", "", "application/json", &typed("hi")).await;
    assert_eq!(r.status, 404);
}

#[tokio::test]
async fn a_turn_is_accepted_or_refused_with_the_statuses_of_the_protocol() {
    let hub = start(Opts {
        queue_max: 2,
        ..Opts::default()
    })
    .await;
    hub.stt.says(200, "Synthetic words.");
    let app_json = "application/json";
    let long_text = "x".repeat(8001);
    let too_big = vec![0u8; 8 * 1024 * 1024 + 1024];
    let status_ok = json!({"ok": true, "turn": "turn-0101", "already": false,
                           "helm": "offline", "queued": true});
    // (name, target, content type, body, status, a piece of the answer or the whole answer)
    #[allow(clippy::type_complexity)]
    let rows: Vec<(&str, String, &str, Vec<u8>, u16, Value)> = vec![
        (
            "a short turn id",
            "/voice/turn?turn=short&mode=chat".into(),
            app_json,
            typed("hi"),
            400,
            json!("turn id"),
        ),
        (
            "no mode",
            "/voice/turn?turn=turn-0100".into(),
            app_json,
            typed("hi"),
            400,
            json!("mode"),
        ),
        (
            "a mode that does not exist",
            "/voice/turn?turn=turn-0100&mode=loud".into(),
            app_json,
            typed("hi"),
            400,
            json!("mode"),
        ),
        (
            "text/plain",
            "/voice/turn?turn=turn-0100&mode=chat".into(),
            "text/plain",
            b"hi".to_vec(),
            415,
            json!("audio/ogg, audio/wav or JSON"),
        ),
        (
            "an empty recording",
            "/voice/turn?turn=turn-0100&mode=conversation".into(),
            "audio/ogg",
            vec![],
            400,
            json!("empty"),
        ),
        (
            "blank typed text",
            "/voice/turn?turn=turn-0100&mode=chat".into(),
            app_json,
            typed("   "),
            400,
            json!("nothing to send"),
        ),
        (
            "typed text over 8000 characters",
            "/voice/turn?turn=turn-0100&mode=chat".into(),
            app_json,
            typed(&long_text),
            400,
            json!("too long"),
        ),
        (
            "a body that is not JSON",
            "/voice/turn?turn=turn-0100&mode=chat".into(),
            app_json,
            b"nope".to_vec(),
            400,
            json!("text"),
        ),
        (
            "a turn that continues an unknown one",
            "/voice/turn?turn=turn-0100&mode=chat&continues=turn-9999".into(),
            app_json,
            typed("hi"),
            400,
            json!("carries on"),
        ),
        (
            "131 seconds of audio",
            "/voice/turn?turn=turn-0100&mode=conversation".into(),
            "audio/ogg",
            ogg(131),
            413,
            json!("That recording is too long."),
        ),
        (
            "a body over 8 MiB",
            "/voice/turn?turn=turn-0100&mode=conversation".into(),
            "audio/ogg",
            too_big,
            413,
            json!("That recording is too long."),
        ),
        (
            "a typed turn with Helm offline",
            "/voice/turn?turn=turn-0101&mode=conversation".into(),
            app_json,
            typed("Synthetic question."),
            202,
            status_ok,
        ),
        (
            "the same turn again",
            "/voice/turn?turn=turn-0101&mode=conversation".into(),
            app_json,
            typed("Synthetic question."),
            200,
            json!({"ok": true, "turn": "turn-0101", "already": true, "state": "queued"}),
        ),
        (
            "a recording, 119 seconds, with a content type that has a parameter",
            "/voice/turn?turn=turn-0102&mode=conversation".into(),
            "audio/ogg; codecs=opus",
            ogg(119),
            202,
            json!({"ok": true, "turn": "turn-0102", "already": false,
                   "helm": "offline", "queued": true}),
        ),
        (
            "a third turn with the queue full",
            "/voice/turn?turn=turn-0103&mode=conversation".into(),
            app_json,
            typed("Synthetic question."),
            429,
            json!("Helm is offline and the queue is full, so this wasn't kept."),
        ),
    ];
    for (name, target, ctype, body, status, want) in rows {
        let r = http(&hub, "POST", &target, Some(PHONE), ctype, &body).await;
        assert_eq!(r.status, status, "{name}: {}", r.body);
        match want {
            Value::String(piece) => {
                let said = r.body["error"].as_str().unwrap_or("");
                assert!(said.contains(&piece), "{name}: {said:?} lacks {piece:?}");
            }
            whole => assert_eq!(r.body, whole, "{name}"),
        }
    }

    // The recording reached the speech-to-text server once, as a multipart form with the clip
    // under the name the format gives it.
    uploads_reach(&hub, 1).await;
    let (kind, form) = hub.stt.upload(0);
    assert!(kind.starts_with("multipart/form-data; boundary="), "{kind}");
    for part in [
        "filename=\"turn.ogg\"",
        "Content-Type: audio/ogg",
        "name=\"model\"\r\n\r\nfake-model",
        "name=\"language\"\r\n\r\nen",
    ] {
        assert!(form.contains(part), "{part:?} in {form:?}");
    }
    assert_eq!(
        hub.stt.uploads.lock().unwrap().len(),
        1,
        "nothing else got through"
    );

    // A WAV goes by its own name, and a hub with no speech-to-text refuses a recording and
    // still takes typed turns.
    let hub = start(Opts::default()).await;
    let r = post_turn(&hub, "turn-0104", "", "audio/wav", &wav(3200)).await;
    assert_eq!(r.status, 202);
    uploads_reach(&hub, 1).await;
    let (_, form) = hub.stt.upload(0);
    assert!(form.contains("filename=\"turn.wav\"") && form.contains("Content-Type: audio/wav"));

    let bare = start(Opts {
        stt: false,
        ..Opts::default()
    })
    .await;
    let r = post_turn(&bare, "turn-0105", "", "audio/ogg", &ogg(2)).await;
    assert_eq!(r.status, 503);
    assert_eq!(r.body["error"], "Speech-to-text isn't set up on the hub.");
    let r = post_turn(&bare, "turn-0106", "", "application/json", &typed("hi")).await;
    assert_eq!(r.status, 202);
    let r = http(&bare, "GET", "/voice/status", Some(PHONE), "", &[]).await;
    assert_eq!(r.body["stt"], false);
}

#[tokio::test]
async fn a_spoken_turn_goes_to_helm_and_the_answer_comes_back() {
    let hub = start(Opts::default()).await;
    let mut helm = pc(&hub).await;
    let (mut me, welcome) = phone(&hub, PHONE, 0, true).await;
    assert_eq!(
        (
            welcome["helm"].as_str(),
            welcome["seq"].as_u64(),
            welcome["version"].as_u64()
        ),
        (Some("online"), Some(0), Some(1))
    );
    // Helm is told the talk screen is open.
    let presence = next_where(&mut helm, "presence", is("voice_presence")).await;
    assert_eq!(
        (presence["live"].as_bool(), presence["mode"].as_str()),
        (Some(true), Some("conversation"))
    );

    // The recording is transcribed, the transcript told to the phone and the turn sent to Helm.
    hub.stt.says(200, "Synthetic question one.");
    let t1 = id("turn", 1);
    let r = post_turn(&hub, &t1, "", "audio/ogg", &ogg(3)).await;
    assert_eq!(r.status, 202);
    assert_eq!(
        r.body,
        json!({"ok": true, "turn": t1, "already": false, "helm": "online", "queued": true})
    );
    next_where(&mut me, "transcribing", turn_is(t1.clone(), "transcribing")).await;
    let heard = next_where(&mut me, "heard", turn_is(t1.clone(), "heard")).await;
    assert_eq!(heard["transcript"], "Synthetic question one.");
    assert_eq!(heard["seq"], 1);
    // Before the recording went to the speech-to-text server, the person's Helm was asked to free
    // the PC's graphics memory (the test line never answers, so the hub moved on after its wait).
    let room = next_where(&mut helm, "the room call", is("call")).await;
    assert_eq!(
        (
            room["tool"].as_str(),
            room["client"].as_str(),
            &room["args"]
        ),
        (
            Some("app_make_room_for_speech"),
            Some("hub-speech"),
            &json!({})
        )
    );
    let v = next_where(&mut helm, "the voice frame", is("voice")).await;
    assert_eq!(
        (
            v["id"].as_str(),
            v["mode"].as_str(),
            v["text"].as_str(),
            v["source"].as_str()
        ),
        (
            Some(t1.as_str()),
            Some("conversation"),
            Some("Synthetic question one."),
            Some("speech")
        )
    );
    assert_eq!(
        (&v["ageMinutes"], &v["continues"]),
        (&Value::Null, &Value::Null)
    );
    assert!(v["sentAt"].as_i64().unwrap() > 0);

    // Helm takes it and shows its working line.
    send(&mut helm, json!({"type": "voice_ack", "id": t1})).await;
    next_where(&mut me, "delivered", turn_is(t1.clone(), "delivered")).await;
    send(
        &mut helm,
        json!({"type": "voice_status", "turn": t1, "text": "Thinking"}),
    )
    .await;
    let status = next_where(&mut me, "status", is("status")).await;
    assert_eq!(
        (status["turn"].as_str(), status["text"].as_str()),
        (Some(t1.as_str()), Some("Thinking"))
    );

    // The answer, with one attachment sent before it, is stored under the next seq.
    let r1 = id("reply", 1);
    send(&mut helm, link(&r1, 1)).await;
    send(
        &mut helm,
        answer(&r1, Some(t1.as_str()), "Synthetic answer one.", 1),
    )
    .await;
    let reply = next_where(&mut me, "reply", is("reply")).await;
    assert_eq!(
        (
            reply["seq"].as_u64(),
            reply["kind"].as_str(),
            reply["mode"].as_str()
        ),
        (Some(2), Some("reply"), Some("conversation"))
    );
    assert_eq!(
        (reply["id"].as_str(), reply["turn"].as_str()),
        (Some(r1.as_str()), Some(t1.as_str()))
    );
    assert_eq!(
        reply["sentences"][1],
        json!({"n": 2, "text": "Want the details?"})
    );
    assert_eq!(reply["text"], "Synthetic answer one. Want the details?");
    assert_eq!(
        reply["attachments"],
        json!([{"kind": "link", "title": "The page", "url": "https://example.test/page"}])
    );
    assert_eq!(
        (&reply["about"], reply["conversation"].as_str()),
        (&Value::Null, Some("c-synthetic"))
    );
    send(&mut me, json!({"type": "ack", "seq": 2})).await;

    // What the phone played goes to Helm.
    let body = json!({"reports": [{"reply": r1, "state": "partial", "sentence": 2,
                                   "charOffset": 5, "at": 1_800_000_000_000i64}]});
    let r = http(
        &hub,
        "POST",
        "/voice/heard",
        Some(PHONE),
        "application/json",
        body.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        (r.status, &r.body),
        (200, &json!({"ok": true, "forwarded": 1}))
    );
    let heard = next_where(&mut helm, "voice_heard", is("voice_heard")).await;
    assert_eq!(heard["reports"][0]["state"], "partial");
    assert_eq!(heard["reports"][0]["sentence"], 2);

    // The screen changes reach Helm too.
    send(
        &mut me,
        json!({"type": "presence", "live": false, "mode": "chat"}),
    )
    .await;
    let presence = next_where(&mut helm, "presence", |f| {
        f["type"] == "voice_presence" && f["live"] == false
    })
    .await;
    assert_eq!(presence["mode"], "chat");

    // Helm speaks first: news carries no turn and says which chat it is about.
    let r2 = id("reply", 2);
    send(&mut helm, answer(&r2, None, "The build finished.", 0)).await;
    let news = next_where(&mut me, "news", |f| {
        f["type"] == "reply" && f["kind"] == "notify"
    })
    .await;
    assert_eq!(
        (
            news["seq"].as_u64(),
            news["about"].clone(),
            news["turn"].clone()
        ),
        (Some(3), json!({"chat": "Build"}), Value::Null)
    );

    // Cancelling a turn Helm took tells Helm to stop it.
    send(&mut me, json!({"type": "cancel", "turn": t1})).await;
    let stop = next_where(&mut helm, "voice_stop", is("voice_stop")).await;
    assert_eq!(stop["turn"], t1.as_str());

    // Cancelling one Helm has not acknowledged drops it and tells Helm in case it was taken.
    let t2 = id("turn", 2);
    let r = post_turn(
        &hub,
        &t2,
        "",
        "application/json",
        &typed("Synthetic second."),
    )
    .await;
    assert_eq!(r.status, 202);
    next_where(&mut helm, "turn 2", |f| {
        f["type"] == "voice" && f["id"] == t2.as_str()
    })
    .await;
    send(&mut me, json!({"type": "cancel", "turn": t2})).await;
    let cancelled = next_where(&mut me, "cancelled", turn_is(t2.clone(), "cancelled")).await;
    assert!(cancelled["seq"].as_u64().is_some(), "kept for the phone");
    let stop = next_where(&mut helm, "voice_stop", is("voice_stop")).await;
    assert_eq!(stop["turn"], t2.as_str());

    // A turn Helm does not acknowledge is sent again 45 s later, and a turn that carries on
    // another says so.
    let t3 = id("turn", 3);
    post_turn(
        &hub,
        &t3,
        "",
        "application/json",
        &typed("Synthetic third."),
    )
    .await;
    let is_t3 = |f: &Value| f["type"] == "voice" && f["id"] == t3.as_str();
    next_where(&mut helm, "turn 3", is_t3).await;
    hub.bridge().tick(now_ms() + 46_000);
    next_where(&mut helm, "turn 3 again", is_t3).await;
    send(&mut helm, json!({"type": "voice_ack", "id": t3})).await;
    next_where(&mut me, "delivered", turn_is(t3.clone(), "delivered")).await;
    let t4 = id("turn", 4);
    let extra = format!("&continues={t3}");
    let r = post_turn(
        &hub,
        &t4,
        &extra,
        "application/json",
        &typed("Synthetic fourth."),
    )
    .await;
    assert_eq!(r.status, 202);
    let v = next_where(&mut helm, "turn 4", |f| {
        f["type"] == "voice" && f["id"] == t4.as_str()
    })
    .await;
    assert_eq!(
        (v["continues"].as_str(), v["source"].as_str()),
        (Some(t3.as_str()), Some("typed"))
    );

    // What the speech-to-text server answers decides the turn's state, and its own words never
    // reach the phone. A failed turn can be sent again; one with nothing heard cannot.
    for (n, status, text, state, message) in [
        (10, 200u16, "Synthetic words.", "heard", None),
        (11, 500, "model not loaded", "failed", Some(STT_FAILED)),
        (12, 200, "{\"text\": \"  \"}", "empty", None),
    ] {
        hub.stt.says(status, text);
        let turn = id("turn", n);
        let r = post_turn(&hub, &turn, "", "audio/ogg", &ogg(1)).await;
        assert_eq!(r.status, 202, "{state}");
        let f = next_where(&mut me, state, |f| {
            f["type"] == "turn" && f["turn"] == turn.as_str() && (f["state"] == state)
        })
        .await;
        assert_eq!(f["message"].as_str(), message, "{state}");
    }
    hub.stt.says(200, "Synthetic words.");
    let retry = post_turn(&hub, &id("turn", 11), "", "audio/ogg", &ogg(1)).await;
    assert_eq!(
        (retry.status, retry.body["already"].clone()),
        (202, json!(false))
    );
    let again = post_turn(&hub, &id("turn", 12), "", "audio/ogg", &ogg(1)).await;
    assert_eq!(
        (again.status, again.body["state"].clone()),
        (200, json!("empty"))
    );
}

#[tokio::test]
async fn turns_wait_while_helm_is_offline_and_a_turn_helm_never_takes_expires() {
    let hub = start(Opts {
        queue_lifetime: Duration::from_secs(60),
        ..Opts::default()
    })
    .await;
    let (mut me, welcome) = phone(&hub, PHONE, 0, true).await;
    assert_eq!(welcome["helm"], "offline");

    let t1 = id("turn", 1);
    let r = post_turn(
        &hub,
        &t1,
        "",
        "application/json",
        &typed("Synthetic question."),
    )
    .await;
    assert_eq!(
        (r.status, r.body["helm"].clone(), r.body["queued"].clone()),
        (202, json!("offline"), json!(true))
    );
    next_where(&mut me, "queued", turn_is(t1.clone(), "queued")).await;
    let again = post_turn(
        &hub,
        &t1,
        "",
        "application/json",
        &typed("Synthetic question."),
    )
    .await;
    assert_eq!(
        (again.status, again.body["state"].clone()),
        (200, json!("queued"))
    );

    // Helm comes up: the phone hears of it, Helm gets the turn fresh, and its ack reaches the phone.
    let mut helm = pc(&hub).await;
    next_where(&mut me, "helm online", |f| {
        f["type"] == "helm" && f["state"] == "online"
    })
    .await;
    let v = next_where(&mut helm, "the turn", is("voice")).await;
    assert_eq!(
        (v["id"].as_str(), v["source"].as_str(), v["text"].as_str()),
        (
            Some(t1.as_str()),
            Some("typed"),
            Some("Synthetic question.")
        )
    );
    assert_eq!(v["ageMinutes"], Value::Null);
    send(&mut helm, json!({"type": "voice_ack", "id": t1})).await;
    next_where(&mut me, "delivered", turn_is(t1.clone(), "delivered")).await;

    // A turn Helm never acknowledges expires after the queue time. The phone is told, the state
    // is kept for a phone that reconnects, and the same id may be tried again.
    let t2 = id("turn", 2);
    post_turn(
        &hub,
        &t2,
        "",
        "application/json",
        &typed("Synthetic unanswered."),
    )
    .await;
    next_where(&mut helm, "turn 2", |f| {
        f["type"] == "voice" && f["id"] == t2.as_str()
    })
    .await;
    hub.bridge().tick(now_ms() + 61_000);
    let expired = next_where(&mut me, "expired", turn_is(t2.clone(), "expired")).await;
    assert_eq!(expired["message"], EXPIRED);
    let (mut me2, _) = phone(&hub, PHONE, 0, false).await;
    next_where(&mut me2, "expired again", turn_is(t2.clone(), "expired")).await;
    let r = post_turn(
        &hub,
        &t2,
        "",
        "application/json",
        &typed("Synthetic unanswered."),
    )
    .await;
    assert_eq!((r.status, r.body["already"].clone()), (202, json!(false)));

    // The line drops: the phone hears that too.
    drop(helm);
    next_where(&mut me2, "helm offline", |f| {
        f["type"] == "helm" && f["state"] == "offline"
    })
    .await;
    let status = http(&hub, "GET", "/voice/status", Some(PHONE), "", &[]).await;
    assert_eq!(
        status.body,
        json!({"ok": true, "helm": "offline", "version": 1, "stt": true})
    );
}

#[tokio::test]
async fn replies_wait_for_the_phone_replay_in_order_and_acks_free_them() {
    let hub = start(Opts::default()).await;
    let mut helm = pc(&hub).await;
    // Three pieces of news while no phone is connected. The first has its attachment in time; the
    // second's waits past 60 s for a reply that comes later and is left out.
    let (r1, r2, r3) = (id("reply", 1), id("reply", 2), id("reply", 3));
    send(&mut helm, link(&r1, 1)).await;
    send(&mut helm, answer(&r1, None, "Synthetic one.", 1)).await;
    send(&mut helm, link(&r2, 1)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    hub.bridge().tick(now_ms() + 61_000);
    send(&mut helm, answer(&r2, None, "Synthetic two.", 1)).await;
    send(&mut helm, answer(&r3, None, "Synthetic three.", 0)).await;
    send(
        &mut helm,
        answer(&r3, None, "Synthetic three, sent again.", 0),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The phone connects with nothing: it is told the newest seq and sent everything in order.
    let (mut me, welcome) = phone(&hub, PHONE, 0, false).await;
    assert_eq!(welcome["seq"], 3);
    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(next_where(&mut me, "a reply", is("reply")).await);
    }
    let shape: Vec<(u64, &str, usize)> = got
        .iter()
        .map(|f| {
            (
                f["seq"].as_u64().unwrap(),
                f["id"].as_str().unwrap(),
                f["attachments"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            (1, r1.as_str(), 1),
            (2, r2.as_str(), 0),
            (3, r3.as_str(), 0)
        ]
    );
    assert_eq!(
        got[2]["text"], "Synthetic three. Want the details?",
        "a reply sent again is kept once"
    );
    assert!(drain(&mut me, 300).await.is_empty(), "nothing more");

    // An ack is cumulative; a reconnect names what it holds and the older socket is closed.
    send(&mut me, json!({"type": "ack", "seq": 2})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mut again, welcome) = phone(&hub, PHONE, 2, false).await;
    assert_eq!(welcome["seq"], 3);
    let replay = frames(&drain(&mut again, 300).await);
    assert_eq!(
        replay
            .iter()
            .map(|f| f["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [3]
    );
    assert_eq!(
        closed_with(&drain(&mut me, 300).await),
        Some((4000, "replaced".to_owned()))
    );
    send(&mut again, json!({"type": "ack", "seq": 3})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mut empty, _) = phone(&hub, PHONE, 0, false).await;
    assert!(
        frames(&drain(&mut empty, 300).await).is_empty(),
        "everything was acknowledged"
    );

    // A hub that restarted starts its seq over: its welcome is below the phone's lastSeq, which
    // is how the phone knows to start over, and the next reply arrives under seq 1.
    let fresh = start(Opts::default()).await;
    let mut helm = pc(&fresh).await;
    let (mut me, welcome) = phone(&fresh, PHONE, 41, true).await;
    assert_eq!(welcome["seq"], 0);
    send(
        &mut helm,
        answer(&id("reply", 7), None, "Synthetic after a restart.", 0),
    )
    .await;
    let reply = next_where(&mut me, "reply", is("reply")).await;
    assert_eq!(reply["seq"], 1);
    // An entry the phone never acknowledges is forgotten after the outbox time (2 hours).
    fresh.bridge().tick(now_ms() + 121 * 60_000);
    let (mut late, _) = phone(&fresh, PHONE, 0, false).await;
    assert!(frames(&drain(&mut late, 300).await).is_empty());
}

#[tokio::test]
async fn two_phones_of_one_person_keep_their_own_socket_acks_and_screen() {
    let hub = start(Opts::default()).await;
    let mut helm = pc(&hub).await;
    let (mut phone_a, _) = phone(&hub, PHONE, 0, true).await;
    let (mut tablet, _) = phone(&hub, TABLET, 0, true).await;
    let quiet = drain(&mut phone_a, 300).await;
    assert!(
        quiet.is_empty(),
        "a second token did not push the first off: {quiet:?}"
    );

    // News reaches both. The phone acking it does not free it for the tablet.
    let r1 = id("reply", 1);
    send(&mut helm, answer(&r1, None, "Synthetic news.", 0)).await;
    next_where(&mut phone_a, "news", is("reply")).await;
    next_where(&mut tablet, "news", is("reply")).await;
    send(&mut phone_a, json!({"type": "ack", "seq": 1})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mut tablet2, _) = phone(&hub, TABLET, 0, true).await;
    assert_eq!(
        closed_with(&drain(&mut tablet, 300).await),
        Some((4000, "replaced".to_owned()))
    );
    next_where(&mut tablet2, "the replay", is("reply")).await;
    send(&mut tablet2, json!({"type": "ack", "seq": 1})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mut phone_b, _) = phone(&hub, PHONE, 0, true).await;
    assert!(
        frames(&drain(&mut phone_b, 300).await).is_empty(),
        "both have it now"
    );

    // Helm hears "not live" only when the last screen in front goes.
    drain(&mut helm, 300).await;
    send(
        &mut phone_b,
        json!({"type": "presence", "live": false, "mode": "chat"}),
    )
    .await;
    let seen = frames(&drain(&mut helm, 400).await);
    assert!(
        seen.iter().all(|f| f["type"] != "voice_presence"),
        "the tablet is still in front: {seen:?}"
    );
    send(
        &mut tablet2,
        json!({"type": "presence", "live": false, "mode": "chat"}),
    )
    .await;
    let presence = next_where(&mut helm, "presence", |f| {
        f["type"] == "voice_presence" && f["live"] == false
    })
    .await;
    assert_eq!(presence["mode"], "chat");
}

#[tokio::test]
async fn status_fresh_and_heard_reports() {
    let hub = start(Opts::default()).await;
    let get = |target: &'static str| http(&hub, "GET", target, Some(PHONE), "", &[]);
    assert_eq!(
        get("/voice/status").await.body,
        json!({"ok": true, "helm": "offline", "version": 1, "stt": true})
    );
    let fresh = http(&hub, "POST", "/voice/fresh", Some(PHONE), "", &[]).await;
    assert_eq!(
        (fresh.status, fresh.body["error"].clone()),
        (409, json!("Helm is offline."))
    );

    // Reports are checked, and with Helm offline kept: the latest per reply wins (a later "all"
    // replaces an earlier "partial", an older report does not replace a newer one).
    let heard = |reports: Value| {
        let body = json!({ "reports": reports }).to_string().into_bytes();
        let hub = &hub;
        async move {
            http(
                hub,
                "POST",
                "/voice/heard",
                Some(PHONE),
                "application/json",
                &body,
            )
            .await
        }
    };
    let many: Vec<Value> = (0..51)
        .map(|_| json!({"reply": "reply-0001", "state": "all"}))
        .collect();
    for (name, bad) in [
        ("none", json!([])),
        (
            "a state that does not exist",
            json!([{"reply": "reply-0001", "state": "most"}]),
        ),
        ("a short reply id", json!([{"reply": "x", "state": "all"}])),
        ("51 of them", json!(many)),
    ] {
        assert_eq!(heard(bad).await.status, 400, "{name}");
    }
    let not_json = http(
        &hub,
        "POST",
        "/voice/heard",
        Some(PHONE),
        "application/json",
        b"nope",
    )
    .await;
    assert_eq!(not_json.status, 400);
    let huge = vec![b' '; 65 * 1024];
    let r = http(
        &hub,
        "POST",
        "/voice/heard",
        Some(PHONE),
        "application/json",
        &huge,
    )
    .await;
    assert_eq!(r.status, 413);
    for report in [
        json!({"reply": "reply-0001", "state": "partial", "sentence": 2, "at": 10}),
        json!({"reply": "reply-0001", "state": "all", "at": 20}),
        json!({"reply": "reply-0001", "state": "none", "at": 5}),
        json!({"reply": "reply-0002", "state": "none", "at": 6}),
    ] {
        let r = heard(json!([report])).await;
        assert_eq!((r.status, r.body["forwarded"].clone()), (200, json!(0)));
    }

    // The line comes up: Helm gets the two that are left, once.
    let mut helm = pc(&hub).await;
    let sent = next_where(&mut helm, "voice_heard", is("voice_heard")).await;
    let states: Vec<(&str, &str)> = sent["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["reply"].as_str().unwrap(), r["state"].as_str().unwrap()))
        .collect();
    assert_eq!(states, [("reply-0001", "all"), ("reply-0002", "none")]);
    assert!(frames(&drain(&mut helm, 700).await)
        .iter()
        .all(|f| f["type"] != "voice_heard"));
    let r = heard(json!([{"reply": "reply-0003", "state": "partial", "sentence": 1}])).await;
    assert_eq!(r.body["forwarded"], 1);
    next_where(&mut helm, "voice_heard", is("voice_heard")).await;

    // Now Helm is online, and "Start fresh" reaches it.
    assert_eq!(get("/voice/status").await.body["helm"], "online");
    let fresh = http(&hub, "POST", "/voice/fresh", Some(PHONE), "", &[]).await;
    assert_eq!((fresh.status, fresh.body), (202, json!({"ok": true})));
    next_where(&mut helm, "voice_fresh", is("voice_fresh")).await;
}

#[tokio::test]
async fn the_live_socket_wants_a_hello_first() {
    let hub = start(Opts::default()).await;
    // A first frame that is not a hello ends the socket, and a hello is checked.
    for first in [
        json!({"type": "ack", "seq": 1}),
        json!({"type": "hello", "live": true, "mode": "loud"}),
    ] {
        let mut ws = connect(&hub, "/voice/live", Some(PHONE)).await.unwrap();
        send(&mut ws, first).await;
        let seen = drain(&mut ws, 1000).await;
        assert!(
            closed_with(&seen).is_some_and(|(code, _)| code == 1008),
            "{seen:?}"
        );
    }
    // Frames the hub does not know are dropped quietly; a good socket stays open.
    let (mut me, _) = phone(&hub, PHONE, 0, true).await;
    send(&mut me, json!({"type": "sing"})).await;
    send(&mut me, json!({"type": "ack", "seq": -1})).await;
    assert!(drain(&mut me, 300).await.is_empty());
}
