//! The room speaker's lane over real sockets: a fake speaker (HTTP), a fake PC (`/relay`) and a
//! fake speech-to-text server, against the real router. These walk the routes, the statuses and
//! the frames of the protocol; the queue rules inside the bridge have their own small unit tests
//! in `src/speaker.rs`. Time-dependent steps (the wait for a line, the session closing, an
//! abandoned turn) are driven by ticking the bridge with a later `now`, not by waiting. All text
//! is synthetic.

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use futures_util::{SinkExt, StreamExt};
use helm_hub::lines::Lines;
use helm_hub::mcp::Options;
use helm_hub::registry::Registry;
use helm_hub::server::{router, AppState};
use helm_hub::speaker::{SpeakerBridge, SpeakerConfig, NEED_UPDATE};
use helm_hub::store::Store;
use helm_hub::stt::{Stt, SttConfig};
use helm_hub::telegram::now_ms;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const LINE: &str = "line-token-0123456789abcdefghij";
const BEDROOM: &str = "bedroom-token-0123456789abcdefgh";
const KITCHEN: &str = "kitchen-token-0123456789abcdefgh";
const NOTES: &str = "notes-token-0123456789abcdefghij";
const NOBODY: &str = "not-a-token-0123456789abcdefgh";

struct Opts {
    speaker: bool,
    stt: bool,
    idle: Duration,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            speaker: true,
            stt: true,
            idle: Duration::from_secs(10 * 60),
        }
    }
}

/// The fake speech-to-text server: answers what it is told to, and keeps what it was sent,
/// including the priority header the speaker lane asks for.
#[allow(clippy::type_complexity)]
struct FakeStt {
    url: String,
    answer: Arc<Mutex<(u16, String)>>,
    uploads: Arc<Mutex<Vec<Option<String>>>>,
}

impl FakeStt {
    async fn start() -> FakeStt {
        let answer = Arc::new(Mutex::new((200u16, "Synthetic words.".to_owned())));
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let (a, u) = (answer.clone(), uploads.clone());
        let app = axum::Router::new().route(
            "/v1/audio/transcriptions",
            axum::routing::post(move |headers: HeaderMap, body: Bytes| async move {
                let priority = headers
                    .get("x-priority")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_owned());
                u.lock().unwrap().push(Some(format!(
                    "{}|{} bytes",
                    priority.unwrap_or_else(|| "none".into()),
                    body.len()
                )));
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

    /// What the n-th upload arrived with: the priority header and the size.
    fn upload(&self, n: usize) -> String {
        self.uploads.lock().unwrap()[n].clone().unwrap()
    }

    fn count(&self) -> usize {
        self.uploads.lock().unwrap().len()
    }
}

struct Hub {
    addr: String,
    bridge: Option<Arc<SpeakerBridge>>,
    stt: FakeStt,
}

impl Hub {
    fn bridge(&self) -> &SpeakerBridge {
        self.bridge.as_ref().expect("the speaker line is on")
    }
}

/// One hub with a relay token, two speaker tokens (of the same person) and a memory client.
async fn start(o: Opts) -> Hub {
    let registry = Registry::from_legacy_spec(
        &format!(
            "helm-line:{LINE}:relay+peer, bedroom:{BEDROOM}:speaker, kitchen:{KITCHEN}:speaker, \
             notes:{NOTES}:rw"
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
    if o.speaker {
        let mut b = SpeakerBridge::new(lines.clone(), SpeakerConfig { idle: o.idle });
        if o.stt {
            b = b.with_stt(Arc::new(
                Stt::new(&SttConfig {
                    url: stt.url.clone(),
                    model: "fake-model".into(),
                    language: "en".into(),
                    timeout: Duration::from_secs(30),
                })
                .with_headers(&[("X-Priority", "interactive")]),
            ));
        }
        let b = Arc::new(b);
        tokio::spawn(b.clone().run(lines.speaker_receiver()));
        state = state.with_speaker(b.clone());
        bridge = Some(b);
    }
    let app = router(Arc::new(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Hub { addr, bridge, stt }
}

// ----- HTTP and sockets -----

struct Reply {
    status: u16,
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
    let (_, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    Reply {
        status,
        body: serde_json::from_str(body.trim()).unwrap_or(Value::Null),
    }
}

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

/// Everything that arrives within `ms` milliseconds.
async fn drain(ws: &mut Ws, ms: u64) -> Vec<Value> {
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    let mut out = Vec::new();
    while let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout_at(end, ws.next()).await {
        out.push(serde_json::from_str(t.as_str()).unwrap());
    }
    out
}

/// Helm's relay line, up, and announced as taking this person's speaker turns (`claims`:
/// telegram, voice, speaker).
async fn pc(hub: &Hub, claims: (bool, bool, bool)) -> Ws {
    let mut ws = connect(hub, "/relay", Some(LINE)).await.expect("the line");
    next_where(&mut ws, "welcome", is("welcome")).await;
    send(
        &mut ws,
        json!({"type": "device", "uid": "uid-desk", "name": "Desk", "run": true,
               "people": [{"personId": "1",
                           "telegram": claims.0, "voice": claims.1, "speaker": claims.2}]}),
    )
    .await;
    // Give the announcement a moment to be recorded before the first turn is asked for.
    tokio::time::sleep(Duration::from_millis(200)).await;
    ws
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

/// Two seconds of it: long enough to be heard, short enough to be cheap.
fn wav_two_seconds() -> Vec<u8> {
    wav(16_000 * 2 * 2)
}

fn turn_body(turn: &str, text: &str, wait: u64, room: Option<&str>) -> Vec<u8> {
    let mut b = json!({"turn_id": turn, "text": text, "wait": wait, "device": "speaker-desk"});
    if let Some(r) = room {
        b["room"] = json!(r);
        b["area"] = json!(r);
        b["presence"] = json!({"people": ["one"], "occupied": true});
    }
    b.to_string().into_bytes()
}

/// A record body: one exchange the speaker answered itself.
fn record_body(id: &str) -> Value {
    json!({
        "turn_id": id, "device": "speaker-desk", "room": "bedroom", "area": "Bedroom",
        "started_at": now_ms() - 3_000, "text": "Lights on.", "answer": "Done.",
        "kind": "answer", "model": "small-model",
        "tool_calls": [{"name": "light_turn_on", "summary": "bedroom lamp", "ok": true}],
        "leftover": ["Buy a new bulb"], "timings": {"stt_ms": 40},
    })
}

fn final_answer(turn: &str, text: &str) -> Value {
    json!({
        "type": "voice_final", "id": format!("reply-{turn}"), "turn": turn,
        "lane": "speaker", "mode": "conversation",
        "sentences": [{"n": 1, "text": text}], "text": text, "truncated": false,
        "conversation": "c-synthetic", "at": now_ms(), "attachments": 0,
    })
}

// ----- the tests -----

/// Every speaker route wants a speaker token, and a speaker token opens nothing else. With the
/// line off the routes answer 404.
#[tokio::test]
async fn every_speaker_route_needs_a_speaker_token_and_other_routes_refuse_it() {
    let hub = start(Opts::default()).await;
    let stt_body = wav(2_000);
    let turn = turn_body("turn-0001", "Synthetic hello.", 0, None);
    // (method, target, content type, body, what a speaker token is allowed to get)
    let record = record_body("record-0001").to_string().into_bytes();
    let routes: [(&str, &str, &str, Vec<u8>, u16); 5] = [
        ("GET", "/speaker/status", "", vec![], 200),
        ("POST", "/speaker/stt", "audio/wav", stt_body.clone(), 200),
        (
            "POST",
            "/speaker/turn",
            "application/json",
            turn.clone(),
            202,
        ),
        ("GET", "/speaker/turn/turn-0001?wait=0", "", vec![], 202),
        (
            "POST",
            "/speaker/record",
            "application/json",
            record.clone(),
            200,
        ),
    ];
    for (method, target, ctype, body, allowed) in &routes {
        for (token, want) in [
            (None, 401),
            (Some(NOBODY), 401),
            (Some(LINE), 403),
            (Some(NOTES), 403),
            (Some(BEDROOM), *allowed),
        ] {
            let r = http(&hub, method, target, token, ctype, body).await;
            assert_eq!(r.status, want, "{method} {target} with {token:?}");
            if want == 403 {
                assert!(
                    r.body["error"]
                        .as_str()
                        .unwrap()
                        .contains("only a speaker token"),
                    "{target}"
                );
            }
        }
    }
    // A speaker token opens no other surface: not the line, not memory, not Telegram's endpoint.
    assert_eq!(
        connect(&hub, "/relay", Some(BEDROOM)).await.err(),
        Some(403)
    );
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
            Some(BEDROOM),
            "application/json",
            body.as_bytes(),
        )
        .await;
        assert_eq!(r.status, 403, "{target}");
        assert!(r.body["error"].as_str().unwrap().contains(want), "{target}");
    }
    // Off: the routes are gone, and the rest of the hub is unaffected.
    let off = start(Opts {
        speaker: false,
        ..Opts::default()
    })
    .await;
    for (method, target, ctype, body) in [
        ("GET", "/speaker/status", "", vec![] as Vec<u8>),
        ("POST", "/speaker/stt", "audio/wav", stt_body),
        ("POST", "/speaker/turn", "application/json", turn),
        ("POST", "/speaker/record", "application/json", record),
    ] {
        let r = http(&off, method, target, Some(BEDROOM), ctype, &body).await;
        assert_eq!(r.status, 404, "{method} {target}");
    }
}

/// The whole lane, once: a clip heard at the speech server, a turn down the line with the
/// speaker's fields, an ack, an answer back to the speaker, and the same turn id answered again
/// without a second frame.
#[tokio::test]
async fn a_spoken_turn_travels_the_lane_and_comes_back_once() {
    let hub = start(Opts::default()).await;
    let mut ws = pc(&hub, (true, true, true)).await;

    // Status before anyone spoke: no line yet claims the speaker, so offline; after the
    // announcement, online with a session once a turn has run.
    let r = http(&hub, "GET", "/speaker/status", Some(BEDROOM), "", &[]).await;
    assert_eq!((r.status, r.body["ok"].as_bool()), (200, Some(true)));
    assert_eq!(r.body["version"], json!(1));
    assert_eq!(r.body["stt"], json!(true));
    assert_eq!(r.body["helm"], json!("online"));
    assert!(r.body["session"].is_null());
    // The status says this hub takes an urgency on each turn.
    assert_eq!(r.body["urgency"], json!(true));

    // A clip is heard at the speech server, asked at the front of its queue.
    let r = http(
        &hub,
        "POST",
        "/speaker/stt",
        Some(BEDROOM),
        "audio/wav",
        &wav_two_seconds(),
    )
    .await;
    assert_eq!(
        (r.status, r.body["text"].as_str()),
        (200, Some("Synthetic words."))
    );
    assert_eq!(hub.stt.count(), 1);
    assert!(
        hub.stt.upload(0).starts_with("interactive|"),
        "{}",
        hub.stt.upload(0)
    );

    // A very short clip is heard as nothing, and the server is not asked at all.
    let r = http(
        &hub,
        "POST",
        "/speaker/stt?device=desk",
        Some(BEDROOM),
        "audio/wav",
        &wav(3_200),
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("empty")));
    assert_eq!(hub.stt.count(), 1);

    // The turn: the hub takes it and answers 202 while Helm works.
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &{
            let mut b: Value = serde_json::from_slice(&turn_body(
                "turn-0001",
                "What time is it?",
                0,
                Some("bedroom"),
            ))
            .unwrap();
            // The speaker's last exchanges ride along, it says it handed the turn over, and it
            // asks for the deep lane to answer it.
            b["context"] = json!("Turn the lamp on. Done.");
            b["origin"] = json!("escalation");
            b["urgency"] = json!("deep");
            b.to_string().into_bytes()
        },
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (202, Some("working")));
    // Even a turn still working carries the hub's own clock: taken, and not yet anything else.
    assert!(
        r.body["timing"]["received"].as_i64().unwrap() > 0,
        "{}",
        r.body
    );
    assert!(r.body["timing"]["answered"].is_null());

    // The frame the PC sees: the phone's voice frame with the speaker's fields beside it.
    let frame = next_where(&mut ws, "the speaker turn", |f| {
        f["type"] == "voice" && f["lane"] == "speaker"
    })
    .await;
    assert_eq!(frame["id"], json!("turn-0001"));
    assert_eq!(frame["personId"], json!("1"));
    assert_eq!(frame["mode"], json!("conversation"));
    assert_eq!(frame["source"], json!("speech"));
    assert_eq!(frame["text"], json!("What time is it?"));
    assert_eq!(frame["device"], json!("speaker-desk"));
    assert_eq!(frame["room"], json!("bedroom"));
    assert_eq!(frame["assurance"], json!("none"));
    assert_eq!(frame["person"], Value::Null);
    assert_eq!(frame["tier"], json!("do"));
    assert_eq!(frame["until"], Value::Null);
    assert_eq!(frame["context"], json!("Turn the lamp on. Done."));
    assert_eq!(frame["origin"], json!("escalation"));
    // The lane the speaker asked for rides down to Helm; the hub does not read it.
    assert_eq!(frame["urgency"], json!("deep"));
    let session = frame["session"].as_str().unwrap().to_owned();
    assert!(!session.is_empty());

    // A poll while Helm works says working; the ack is not an answer.
    let r = http(
        &hub,
        "GET",
        "/speaker/turn/turn-0001?wait=0",
        Some(BEDROOM),
        "",
        &[],
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (202, Some("working")));
    send(
        &mut ws,
        json!({"type": "voice_ack", "id": "turn-0001", "lane": "speaker", "personId": "1"}),
    )
    .await;
    let r = http(
        &hub,
        "GET",
        "/speaker/turn/turn-0001?wait=0",
        Some(BEDROOM),
        "",
        &[],
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (202, Some("working")));

    // The answer, once, with the session it belongs to.
    send(&mut ws, final_answer("turn-0001", "Half past seven.")).await;
    let r = http(
        &hub,
        "GET",
        "/speaker/turn/turn-0001?wait=2",
        Some(BEDROOM),
        "",
        &[],
    )
    .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["state"], json!("answer"));
    assert_eq!(r.body["text"], json!("Half past seven."));
    assert_eq!(r.body["session"]["id"], json!(session));
    assert_eq!(r.body["session"]["assurance"], json!("none"));
    assert_eq!(r.body["session"]["elevated"], json!(false));
    // The hub's own milliseconds for each hop, in order; the poll and the first call agree.
    let t = &r.body["timing"];
    let ms = |k: &str| {
        t[k].as_i64()
            .unwrap_or_else(|| panic!("{k} missing in {t}"))
    };
    assert!(ms("received") <= ms("delivered_to_pc"), "{t}");
    assert!(ms("delivered_to_pc") <= ms("acked"), "{t}");
    assert!(ms("acked") <= ms("answered"), "{t}");

    // The same turn id from the same speaker is answered from the record: no new frame, and the
    // status keeps the session.
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &turn_body("turn-0001", "What time is it?", 0, None),
    )
    .await;
    assert_eq!(
        (r.status, r.body["text"].as_str()),
        (200, Some("Half past seven."))
    );
    assert!(drain(&mut ws, 400).await.is_empty());
    let r = http(&hub, "GET", "/speaker/status", Some(BEDROOM), "", &[]).await;
    assert_eq!(r.body["session"]["id"], json!(session));

    // The same id from the other speaker's token is refused, and an id nobody knows is a 404.
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(KITCHEN),
        "application/json",
        &turn_body("turn-0001", "Other words.", 0, None),
    )
    .await;
    assert_eq!(r.status, 409);
    let r = http(
        &hub,
        "GET",
        "/speaker/turn/turn-9999?wait=0",
        Some(BEDROOM),
        "",
        &[],
    )
    .await;
    assert_eq!(r.status, 404);
    // And the other speaker's own turn is its own: the same words, a new id, a different session.
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(KITCHEN),
        "application/json",
        &turn_body("turn-0002", "Other words.", 0, None),
    )
    .await;
    assert_eq!(r.status, 202);
    let frame = next_where(&mut ws, "the second turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0002"
    })
    .await;
    assert_ne!(frame["session"].as_str().unwrap(), session);
    // A plain call names none of context, origin or urgency: all three go down the line null.
    assert!(frame["context"].is_null() && frame["origin"].is_null() && frame["urgency"].is_null());

    // An origin other than escalation, an urgency that is neither quick nor deep, or a context
    // that is too long (the end of it is kept) is handled before anything runs.
    for (field_name, bad_value, want) in [
        ("origin", json!("shouting"), "escalation"),
        ("urgency", json!("hasty"), "quick or deep"),
    ] {
        let mut bad: Value =
            serde_json::from_slice(&turn_body("turn-0003", "Words.", 0, None)).unwrap();
        bad[field_name] = bad_value.clone();
        let r = http(
            &hub,
            "POST",
            "/speaker/turn",
            Some(BEDROOM),
            "application/json",
            bad.to_string().as_bytes(),
        )
        .await;
        assert_eq!(r.status, 400, "{field_name}={bad_value}");
        let said = r.body["error"].as_str().unwrap_or("");
        assert!(said.contains(want), "{field_name}: {said:?} lacks {want:?}");
    }
    let mut long: Value =
        serde_json::from_slice(&turn_body("turn-0004", "Words.", 0, None)).unwrap();
    long["context"] = json!(format!("{}END", "x".repeat(5_000)));
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        long.to_string().as_bytes(),
    )
    .await;
    assert_eq!(r.status, 202);
    let frame = next_where(&mut ws, "the long-context turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0004"
    })
    .await;
    let kept = frame["context"].as_str().unwrap();
    assert!(
        kept.len() <= 2_048 && kept.ends_with("END"),
        "{}",
        kept.len()
    );
}

/// A turn with no Helm to take it: a person with no line at all waits the longer wait and is
/// answered `offline`; a person whose Helm is up but never claimed the speaker is answered
/// within the short wait with the sentence only a new enough Helm can give. Both waits end the
/// turn, not run it later.
#[tokio::test]
async fn a_turn_finds_no_line_or_an_old_helm_and_answers_itself() {
    let hub = start(Opts::default()).await;
    // No line up at all.
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &turn_body("turn-0001", "Hello there.", 0, None),
    )
    .await;
    assert_eq!(r.status, 202);
    hub.bridge().tick(now_ms() + 29_000);
    let r = http(
        &hub,
        "GET",
        "/speaker/turn/turn-0001?wait=0",
        Some(BEDROOM),
        "",
        &[],
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (202, Some("working")));
    hub.bridge().tick(now_ms() + 31_000);
    let r = http(
        &hub,
        "GET",
        "/speaker/turn/turn-0001?wait=0",
        Some(BEDROOM),
        "",
        &[],
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("offline")));

    // A Helm that is up but claimed only telegram and voice: the short wait, then its own words.
    let hub = start(Opts::default()).await;
    let mut ws = pc(&hub, (true, true, false)).await;
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &turn_body("turn-0002", "Hello there.", 0, None),
    )
    .await;
    assert_eq!(r.status, 202);
    hub.bridge().tick(now_ms() + 2_000);
    let r = http(
        &hub,
        "GET",
        "/speaker/turn/turn-0002?wait=0",
        Some(BEDROOM),
        "",
        &[],
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (202, Some("working")));
    hub.bridge().tick(now_ms() + 4_000);
    let r = http(
        &hub,
        "GET",
        "/speaker/turn/turn-0002?wait=0",
        Some(BEDROOM),
        "",
        &[],
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("answer")));
    assert_eq!(r.body["text"], json!(NEED_UPDATE));
    // Nothing went down the line for it.
    let extra: Vec<Value> = drain(&mut ws, 400)
        .await
        .into_iter()
        .filter(|f| f["type"] == "voice")
        .collect();
    assert!(
        extra.is_empty(),
        "no speaker turn went down the line: {extra:?}"
    );
    // A status call now says so too: no candidate takes the speaker.
    let r = http(&hub, "GET", "/speaker/status", Some(BEDROOM), "", &[]).await;
    assert_eq!(r.body["helm"], json!("offline"));
}

/// One talk on one speaker is one session: it stays open while a turn runs, closes when it has
/// idled out (and every line that carried it is told, with the time it ran out), and the next
/// utterance starts a new one.
#[tokio::test]
async fn a_talk_stays_one_session_until_it_idles_out() {
    let idle = Duration::from_secs(600);
    let hub = start(Opts {
        idle,
        ..Opts::default()
    })
    .await;
    let mut ws = pc(&hub, (true, true, true)).await;
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &turn_body("turn-0001", "First words.", 0, Some("bedroom")),
    )
    .await;
    assert_eq!(r.status, 202);
    let first = next_where(&mut ws, "the first turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0001"
    })
    .await;
    let session = first["session"].as_str().unwrap().to_owned();
    // Helm takes it and answers it: only one turn is ever in flight.
    send(
        &mut ws,
        json!({"type": "voice_ack", "id": "turn-0001", "lane": "speaker", "personId": "1"}),
    )
    .await;
    send(&mut ws, final_answer("turn-0001", "First answer.")).await;

    // A second turn in the same talk keeps the same session.
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &turn_body("turn-0002", "More words.", 0, None),
    )
    .await;
    assert_eq!(r.status, 202);
    let again = next_where(&mut ws, "the second turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0002"
    })
    .await;
    assert_eq!(again["session"], json!(session));

    // Idle, but a turn is still in flight: the talk stays open.
    hub.bridge()
        .tick(now_ms() + idle.as_millis() as i64 + 1_000);
    let closed: Vec<Value> = drain(&mut ws, 300)
        .await
        .into_iter()
        .filter(|f| f["type"] == "speaker_session_closed")
        .collect();
    assert!(closed.is_empty(), "a running turn holds its session open");

    // Finish it, and the next tick closes the talk with the time it ran out.
    send(&mut ws, final_answer("turn-0002", "Done.")).await;
    let started = {
        let r = http(&hub, "GET", "/speaker/status", Some(BEDROOM), "", &[]).await;
        r.body["session"]["lastActiveAt"].as_i64().unwrap()
    };
    hub.bridge()
        .tick(now_ms() + idle.as_millis() as i64 + 1_000);
    let closed = next_where(&mut ws, "the session close", |f| {
        f["type"] == "speaker_session_closed"
    })
    .await;
    assert_eq!(closed["sessionId"], json!(session));
    assert_eq!(closed["closedAt"], json!(started + idle.as_millis() as i64));

    // The next utterance is a new talk, and the old id is still answered from its record.
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &turn_body("turn-0003", "Back again.", 0, None),
    )
    .await;
    assert_eq!(r.status, 202);
    let fresh = next_where(&mut ws, "the third turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0003"
    })
    .await;
    assert_ne!(fresh["session"].as_str().unwrap(), session);
}

/// Two speakers talk through one hub interleaved, on the five-minute idle default: each speaker's
/// second message within the window carries on its own talk (not the other's), each talk has a
/// session of its own, and after the idle both close — after which every new utterance starts a
/// fresh talk.
#[tokio::test]
async fn two_speakers_interleave_their_talks_and_each_idles_on_its_own() {
    let idle = Duration::from_secs(300);
    let hub = start(Opts {
        idle,
        ..Opts::default()
    })
    .await;
    let mut ws = pc(&hub, (true, true, true)).await;
    let ask = |id: &'static str, text: &'static str, token: &'static str| {
        let hub = &hub;
        async move {
            http(
                hub,
                "POST",
                "/speaker/turn",
                Some(token),
                "application/json",
                &turn_body(id, text, 0, Some("kitchen")),
            )
            .await
        }
    };

    // Speaker 1 (bedroom token) opens a talk.
    assert_eq!(ask("turn-0101", "First from one.", BEDROOM).await.status, 202);
    let first = next_where(&mut ws, "speaker 1's first turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0101"
    })
    .await;
    let s1 = first["session"].as_str().unwrap().to_owned();
    send(
        &mut ws,
        json!({"type": "voice_ack", "id": "turn-0101", "lane": "speaker", "personId": "1"}),
    )
    .await;
    send(&mut ws, final_answer("turn-0101", "One answered.")).await;

    // Speaker 2 (kitchen token, same room name) starts its own talk in the middle of speaker 1's.
    assert_eq!(ask("turn-0102", "First from two.", KITCHEN).await.status, 202);
    let second = next_where(&mut ws, "speaker 2's turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0102"
    })
    .await;
    let s2 = second["session"].as_str().unwrap().to_owned();
    assert_ne!(s2, s1, "a second speaker has a talk of its own");
    send(
        &mut ws,
        json!({"type": "voice_ack", "id": "turn-0102", "lane": "speaker", "personId": "1"}),
    )
    .await;
    send(&mut ws, final_answer("turn-0102", "Two answered.")).await;

    // Speaker 1 again well inside five minutes: the same talk carries on.
    assert_eq!(ask("turn-0103", "Back to one.", BEDROOM).await.status, 202);
    let again = next_where(&mut ws, "speaker 1's second turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0103"
    })
    .await;
    assert_eq!(again["session"], json!(s1), "its second message continues its talk");
    send(
        &mut ws,
        json!({"type": "voice_ack", "id": "turn-0103", "lane": "speaker", "personId": "1"}),
    )
    .await;
    send(&mut ws, final_answer("turn-0103", "One again.")).await;

    // Idle past five minutes: both talks close, each with its own id. A last answer can still be
    // in flight when the sweep first runs, and a session with a live turn is held back, so tick
    // again on every pass until both closes are seen; check nothing else closes twice or late.
    let mut closed: Vec<String> = Vec::new();
    for _ in 0..15 {
        hub.bridge()
            .tick(now_ms() + idle.as_millis() as i64 + 1_000);
        closed.extend(
            drain(&mut ws, 300)
                .await
                .into_iter()
                .filter(|f| f["type"] == "speaker_session_closed")
                .map(|f| f["sessionId"].as_str().unwrap_or_default().to_owned()),
        );
        if closed.len() >= 2 {
            break;
        }
    }
    let late = drain(&mut ws, 200)
        .await
        .into_iter()
        .filter(|f| f["type"] == "speaker_session_closed")
        .count();
    closed.sort();
    closed.dedup();
    let mut want = vec![s1.clone(), s2.clone()];
    want.sort();
    assert_eq!(closed, want, "both talks idle out on their own");
    assert_eq!(late, 0, "each talk closes exactly once");

    // The next utterance after the idle starts a fresh talk for that speaker.
    assert_eq!(ask("turn-0104", "Fresh start.", BEDROOM).await.status, 202);
    let fresh = next_where(&mut ws, "the new turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0104"
    })
    .await;
    assert_ne!(fresh["session"].as_str().unwrap(), s1);
}

/// A record travels the lane like a turn but wants no answer: it is taken at once, sent again
/// until Helm acknowledges it, waits for a line when none is up, keeps its talk open until it is
/// taken, and the same id is never stored twice.
#[tokio::test]
async fn a_record_is_taken_at_once_and_reaches_helm_once() {
    let idle = Duration::from_secs(600);
    let hub = start(Opts {
        idle,
        ..Opts::default()
    })
    .await;
    let post = |token: &'static str, body: Value| {
        let hub = &hub;
        async move {
            http(
                hub,
                "POST",
                "/speaker/record",
                Some(token),
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
    };

    // No Helm yet: the hub still says yes, and holds the record for a line that takes the speaker.
    let r = post(BEDROOM, record_body("record-0001")).await;
    assert_eq!((r.status, r.body["ok"].as_bool()), (200, Some(true)));
    let mut ws = pc(&hub, (true, true, true)).await;
    hub.bridge().tick(now_ms());
    let frame = next_where(&mut ws, "the record", is("speaker_record")).await;
    assert_eq!(
        (frame["id"].clone(), frame["lane"].clone()),
        (json!("record-0001"), json!("speaker"))
    );
    assert_eq!(frame["personId"], json!("1"));
    assert_eq!(frame["device"], json!("speaker-desk"));
    assert_eq!(frame["room"], json!("bedroom"));
    assert_eq!(frame["text"], json!("Lights on."));
    assert_eq!(frame["answer"], json!("Done."));
    assert_eq!(frame["kind"], json!("answer"));
    assert_eq!(frame["model"], json!("small-model"));
    assert_eq!(frame["toolCalls"][0]["name"], json!("light_turn_on"));
    assert_eq!(frame["toolCalls"][0]["ok"], json!(true));
    assert_eq!(frame["leftover"], json!(["Buy a new bulb"]));
    assert_eq!(frame["timings"]["stt_ms"], json!(40));
    let session = frame["session"].as_str().unwrap().to_owned();

    // Unacknowledged, it goes again on the resend schedule; once acknowledged it never does.
    hub.bridge().tick(now_ms() + 11_000);
    let again = next_where(&mut ws, "the resent record", is("speaker_record")).await;
    assert_eq!(again["id"], json!("record-0001"));
    send(
        &mut ws,
        json!({"type": "voice_ack", "id": "record-0001", "lane": "speaker", "personId": "1"}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    hub.bridge().tick(now_ms() + 25_000);
    assert!(
        drain(&mut ws, 300)
            .await
            .iter()
            .all(|f| f["type"] != "speaker_record"),
        "an acknowledged record is not sent again"
    );

    // The same id again is taken, not stored twice; another speaker's use of it, or a turn's, is a 409.
    assert_eq!(post(BEDROOM, record_body("record-0001")).await.status, 200);
    assert_eq!(post(KITCHEN, record_body("record-0001")).await.status, 409);
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &turn_body("record-0001", "Words.", 0, None),
    )
    .await;
    assert_eq!(r.status, 409);
    hub.bridge().tick(now_ms() + 26_000);
    assert!(drain(&mut ws, 300)
        .await
        .iter()
        .all(|f| f["type"] != "speaker_record"));

    // What a record must carry, and its limits: a missing id, words or kind is a 400, a body over
    // 32 KiB a 413; long lists are cut, not refused.
    for (what, patch) in [
        ("no id", json!({"turn_id": null})),
        ("a bad id", json!({"turn_id": "a b"})),
        ("no words", json!({"text": "  "})),
        ("a kind of its own", json!({"kind": "chat"})),
    ] {
        let mut b = record_body("record-0002");
        for (k, v) in patch.as_object().unwrap() {
            b[k] = v.clone();
        }
        assert_eq!(post(BEDROOM, b).await.status, 400, "{what}");
    }
    let mut big = record_body("record-0002");
    big["answer"] = json!("x".repeat(33 * 1024));
    assert_eq!(post(BEDROOM, big).await.status, 413);
    let mut long = record_body("record-0003");
    long["leftover"] = json!((0..40).map(|n| format!("note {n}")).collect::<Vec<_>>());
    long["tool_calls"] = json!((0..80)
        .map(|n| json!({"name": format!("t{n}")}))
        .collect::<Vec<_>>());
    assert_eq!(post(BEDROOM, long).await.status, 200);
    let cut = next_where(&mut ws, "the long record", |f| f["id"] == "record-0003").await;
    assert_eq!(cut["leftover"].as_array().unwrap().len(), 20);
    assert_eq!(cut["toolCalls"].as_array().unwrap().len(), 50);
    assert_eq!(
        cut["toolCalls"][0]["ok"],
        json!(true),
        "ok defaults to true"
    );
    send(
        &mut ws,
        json!({"type": "voice_ack", "id": "record-0003", "lane": "speaker", "personId": "1"}),
    )
    .await;

    // The talk the records opened closes with the idle, and Helm is told which one it was.
    tokio::time::sleep(Duration::from_millis(200)).await;
    hub.bridge()
        .tick(now_ms() + idle.as_millis() as i64 + 1_000);
    let closed = next_where(&mut ws, "the session close", is("speaker_session_closed")).await;
    assert_eq!(closed["sessionId"], json!(session));
}

/// More than four live turns is refused, and a turn Helm never takes is finished off as an
/// error once its span passes - after which the same id is answered as an error, not sent again.
#[tokio::test]
async fn the_queue_holds_four_and_an_unanswered_turn_ends() {
    let hub = start(Opts::default()).await;
    let mut ws = pc(&hub, (true, true, true)).await;
    let mut sent = 0;
    for n in 1..=5 {
        let r = http(
            &hub,
            "POST",
            "/speaker/turn",
            Some(BEDROOM),
            "application/json",
            &turn_body(&format!("turn-{n:04}"), &format!("Words {n}."), 0, None),
        )
        .await;
        if r.status == 202 {
            sent += 1;
        } else {
            assert_eq!((n, r.status), (5, 429));
        }
    }
    assert_eq!(sent, 4);
    // Only the head of the queue goes down the line.
    let first = next_where(&mut ws, "the first turn", |f| {
        f["type"] == "voice" && f["lane"] == "speaker"
    })
    .await;
    assert_eq!(first["id"], json!("turn-0001"));
    let rest = drain(&mut ws, 400);
    assert!(rest.await.is_empty(), "one turn at a time goes down");

    // It goes again on its schedule, to the same connection.
    hub.bridge().tick(now_ms() + 11_000);
    let again = next_where(&mut ws, "the resend", |f| {
        f["type"] == "voice" && f["id"] == "turn-0001"
    })
    .await;
    assert_eq!(again["session"], first["session"]);

    // Helm never acknowledges it: past its span the turn ends as an error, and the queue moves
    // on to the next.
    hub.bridge().tick(now_ms() + 121_000);
    let r = http(
        &hub,
        "GET",
        "/speaker/turn/turn-0001?wait=0",
        Some(BEDROOM),
        "",
        &[],
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("error")));
    // The turns that never went out are abandoned from when they were made, so the whole queue
    // is finished off and the speaker is free to ask again in the same talk.
    for n in 2..=4 {
        let r = http(
            &hub,
            "GET",
            &format!("/speaker/turn/turn-{n:04}?wait=0"),
            Some(BEDROOM),
            "",
            &[],
        )
        .await;
        assert_eq!(
            (n, r.status, r.body["state"].as_str()),
            (n, 200, Some("error"))
        );
    }
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &turn_body("turn-0005", "Words again.", 0, None),
    )
    .await;
    assert_eq!(r.status, 202);
    let second = next_where(&mut ws, "the fresh turn", |f| {
        f["type"] == "voice" && f["id"] == "turn-0005"
    })
    .await;
    assert_eq!(second["lane"], json!("speaker"));
    assert_eq!(second["session"], first["session"]);
    // The abandoned id is answered as an error again, not re-sent.
    let r = http(
        &hub,
        "POST",
        "/speaker/turn",
        Some(BEDROOM),
        "application/json",
        &turn_body("turn-0001", "Words 1.", 0, None),
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("error")));
}

/// A wav the hub will not take is refused before the speech server sees it, and a server that
/// is not configured answers `offline` rather than guessing.
#[tokio::test]
async fn a_clip_too_long_or_a_server_missing_is_answered_before_any_model() {
    let hub = start(Opts::default()).await;
    // 31 seconds at 16 kHz 16-bit mono, inside the body cap: too long by the clock.
    let long = wav(16_000 * 2 * 31);
    let r = http(
        &hub,
        "POST",
        "/speaker/stt",
        Some(BEDROOM),
        "audio/wav",
        &long,
    )
    .await;
    assert_eq!(r.status, 413);
    assert_eq!(hub.stt.count(), 0);
    // A body over the cap is refused as well, and never reaches the server.
    let huge = wav(16_000 * 2 * 60);
    let r = http(
        &hub,
        "POST",
        "/speaker/stt",
        Some(BEDROOM),
        "audio/wav",
        &huge,
    )
    .await;
    assert_eq!(r.status, 413);
    assert_eq!(hub.stt.count(), 0);
    // A server that answers with nothing usable is a failed server, as on the phone's line.
    // (The hub's own blank-transcript rule stays as a guard for a server that sends one.)
    hub.stt.says(200, "   ");
    let r = http(
        &hub,
        "POST",
        "/speaker/stt",
        Some(BEDROOM),
        "audio/wav",
        &wav_two_seconds(),
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("error")));
    // A failing server is `error` too, and no speech server at all is `offline`.
    hub.stt.says(500, "broken");
    let r = http(
        &hub,
        "POST",
        "/speaker/stt",
        Some(BEDROOM),
        "audio/wav",
        &wav_two_seconds(),
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("error")));
    let no_stt = start(Opts {
        stt: false,
        ..Opts::default()
    })
    .await;
    let r = http(
        &no_stt,
        "POST",
        "/speaker/stt",
        Some(BEDROOM),
        "audio/wav",
        &wav_two_seconds(),
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("offline")));
    assert_eq!(
        http(&no_stt, "GET", "/speaker/status", Some(BEDROOM), "", &[])
            .await
            .body["stt"],
        json!(false)
    );
}
