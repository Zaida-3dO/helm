//! More than one person on one hub: pairing, a line per machine, revoking, who sees which
//! memory, the household questions and Telegram routing per person.

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use helm_hub::auth::{Scopes, DEFAULT_PAIRED_SCOPES};
use helm_hub::lines::Lines;
use helm_hub::mcp::Options;
use helm_hub::registry::{display_code, Registry};
use helm_hub::relay::{Line, Upstream};
use helm_hub::server::{router, AppState, PAIR_MAX_FAILURES};
use helm_hub::store::Store;
use helm_hub::telegram::{
    now_ms, ApiError, ApiFuture, Bridge, Handled, Receive, Telegram, TelegramApi, TelegramConfig,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone)]
struct Hub {
    addr: String,
    lines: Arc<Lines>,
    registry: Arc<Mutex<Registry>>,
}

async fn start() -> Hub {
    let registry = Arc::new(Mutex::new(Registry::open_in_memory().unwrap()));
    let (lines, _upstream) = Lines::new(registry.clone());
    let lines = Arc::new(lines);
    let app = router(Arc::new(AppState::new(
        Store::open_in_memory().unwrap(),
        registry.clone(),
        Options { allow_mutate: true },
        lines.clone(),
        None,
    )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Hub {
        addr,
        lines,
        registry,
    }
}

/// A machine for `person` (made if new); returns its token.
fn add(hub: &Hub, person: &str, machine: &str, scopes: &str) -> String {
    let scopes = Scopes::parse_list(scopes).unwrap();
    let mut r = hub.registry.lock().unwrap();
    r.add_client(person, machine, &scopes).unwrap().token
}

/// A tiny HTTP/1.1 client over the socket, so the test needs no HTTP client crate.
async fn post(hub: &Hub, path: &str, token: Option<&str>, body: Value) -> (u16, Value) {
    post_with(hub, path, token, &[], body).await
}

/// [`post`] with more request headers.
async fn post_with(
    hub: &Hub,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Value,
) -> (u16, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = body.to_string();
    let mut auth = token
        .map(|t| format!("authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    for (k, v) in headers {
        auth.push_str(&format!("{k}: {v}\r\n"));
    }
    let mut s = tokio::net::TcpStream::connect(&hub.addr).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nhost: hub\r\n{auth}content-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status: u16 = out[9..12].parse().unwrap();
    let json = out
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("")
        .trim();
    (status, serde_json::from_str(json).unwrap_or(Value::Null))
}

async fn tool(hub: &Hub, token: &str, name: &str, args: Value) -> Value {
    let (status, v) = post(
        hub,
        "/mcp",
        Some(token),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": args } }),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    v["result"].clone()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(hub: &Hub, token: &str) -> Result<Ws, u16> {
    let mut req = format!("ws://{}/relay", hub.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => Err(resp.status().as_u16()),
        Err(e) => panic!("{e}"),
    }
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let f = next_any(ws).await;
        // The hub tells every line which devices are up whenever one joins or leaves; the
        // household checks below are about the frames that follow a call, not that list.
        if f["type"] != "devices" {
            return f;
        }
    }
}

async fn next_any(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a frame within 5 s")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

/// Connect, read the welcome and return the socket with its ids.
async fn join(hub: &Hub, token: &str) -> (Ws, i64, i64) {
    let mut ws = connect(hub, token).await.expect("the token opens the line");
    let welcome = next_json(&mut ws).await;
    assert_eq!(welcome["type"], "welcome");
    let ids = (
        welcome["personId"].as_i64().unwrap(),
        welcome["machineId"].as_i64().unwrap(),
    );
    (ws, ids.0, ids.1)
}

async fn closed(ws: &mut Ws, within: Duration) -> bool {
    let wait = async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    };
    tokio::time::timeout(within, wait).await.is_ok()
}

/// Nothing but `devices` lists arrives for 250 ms.
async fn quiet(ws: &mut Ws) -> bool {
    let until = tokio::time::Instant::now() + Duration::from_millis(250);
    loop {
        match tokio::time::timeout_at(until, ws.next()).await {
            Err(_) => return true,
            Ok(Some(Ok(Message::Text(t)))) => {
                let f: Value = serde_json::from_str(t.as_str()).unwrap();
                if f["type"] != "devices" {
                    return false;
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(_) => return false,
        }
    }
}

async fn send_json(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

#[tokio::test]
async fn pairing_connects_machines_side_by_side_and_a_revoke_drops_only_that_one() {
    let hub = start().await;
    let ann = add(&hub, "Ann", "desk", DEFAULT_PAIRED_SCOPES);
    let scopes = Scopes::parse_list(DEFAULT_PAIRED_SCOPES).unwrap();
    let new_code = |machine: &str| {
        let mut r = hub.registry.lock().unwrap();
        r.create_pairing("Bea", machine, &scopes, now_ms())
            .unwrap()
            .0
    };
    let code = new_code("laptop");

    // A wrong code is refused; the right one, typed any way, pairs once.
    let (status, _) = post(&hub, "/pair", None, json!({ "code": "AAAA-AAAA" })).await;
    assert_eq!(status, 403);
    let typed = display_code(&code).to_lowercase();
    let (status, paired) = post(&hub, "/pair", None, json!({ "code": typed })).await;
    assert_eq!(status, 200, "{paired}");
    assert_eq!(paired["personName"], "Bea");
    assert_eq!(paired["machineName"], "laptop");
    let bea = paired["token"].as_str().unwrap().to_owned();
    let (status, _) = post(&hub, "/pair", None, json!({ "code": code })).await;
    assert_eq!(status, 403, "a code works once");

    // Both machines are on at once; a reconnect replaces only that machine's own line.
    let (mut ann_ws, ann_person, ann_id) = join(&hub, &ann).await;
    let (mut bea_ws, bea_person, bea_id) = join(&hub, &bea).await;
    assert_ne!(ann_person, bea_person);
    assert_eq!(bea_id, paired["machineId"].as_i64().unwrap());
    let (_second_ws, _, again) = join(&hub, &ann).await;
    assert_eq!(again, ann_id);
    assert!(closed(&mut ann_ws, Duration::from_secs(5)).await);
    assert!(hub.lines.get(ann_id).unwrap().is_up());
    assert!(hub.lines.get(bea_id).unwrap().is_up());

    // Revoking her machine drops her line within seconds, with no restart, and leaves his.
    hub.registry.lock().unwrap().revoke("laptop", None).unwrap();
    assert!(
        closed(&mut bea_ws, Duration::from_secs(10)).await,
        "the revoked line stayed up"
    );
    assert_eq!(connect(&hub, &bea).await.err(), Some(401));
    let (status, _) = post(&hub, "/mcp", Some(&bea), json!({})).await;
    assert_eq!(status, 401);
    assert!(hub.lines.get(ann_id).unwrap().is_up());

    // Wrong codes run into a limit that even a good code does not get past.
    let mut seen = vec![];
    for _ in 0..PAIR_MAX_FAILURES {
        seen.push(
            post(&hub, "/pair", None, json!({ "code": "BBBB-BBBB" }))
                .await
                .0,
        );
    }
    assert_eq!((seen[0], seen[PAIR_MAX_FAILURES - 1]), (403, 429));
    let good = new_code("tablet");
    let (status, _) = post(&hub, "/pair", None, json!({ "code": good })).await;
    assert_eq!(status, 429);
}

#[tokio::test]
async fn memories_are_private_unless_shared_and_only_the_owner_changes_them() {
    let hub = start().await;
    let ann = add(&hub, "Ann", "desk", "memory:rw");
    let bea = add(&hub, "Bea", "laptop", "memory:rw");
    let id_of = |r: &Value| r["structuredContent"]["id"].as_i64().unwrap();
    let count = |r: &Value| r["structuredContent"]["count"].as_i64().unwrap();

    let secret = id_of(
        &tool(
            &hub,
            &ann,
            "memory_write",
            json!({ "title": "Gift plan", "body": "ring size secret" }),
        )
        .await,
    );
    let shared = id_of(
        &tool(
            &hub,
            &ann,
            "memory_write",
            json!({ "title": "Garden notes", "body": "tomatoes in May", "visibility": "shared" }),
        )
        .await,
    );
    let hers = id_of(
        &tool(
            &hub,
            &bea,
            "memory_write",
            json!({ "title": "Her list", "body": "tomatoes too" }),
        )
        .await,
    );

    // Her searches, lists and reads find her own and the shared one, never his private one.
    let found = tool(&hub, &bea, "memory_search", json!({ "query": "secret" })).await;
    assert_eq!(count(&found), 0);
    let found = tool(&hub, &bea, "memory_search", json!({ "query": "tomatoes" })).await;
    assert_eq!(count(&found), 2, "{found}");
    assert_eq!(count(&tool(&hub, &bea, "memory_list", json!({})).await), 2);
    assert_eq!(count(&tool(&hub, &ann, "memory_list", json!({})).await), 2);
    let read = tool(&hub, &bea, "memory_read", json!({ "id": secret })).await;
    assert_eq!(read["isError"], true);
    let read = tool(&hub, &bea, "memory_read", json!({ "id": shared })).await;
    assert_eq!(read["structuredContent"]["mine"], false);
    assert_eq!(read["structuredContent"]["visibility"], "shared");

    // Only the owner changes or deletes; a private memory looks absent to anyone else.
    for (name, id) in [
        ("memory_update", shared),
        ("memory_update", secret),
        ("memory_delete", shared),
        ("memory_delete", secret),
    ] {
        let r = tool(&hub, &bea, name, json!({ "id": id, "body": "mine now" })).await;
        assert_eq!(r["isError"], true, "{name} {id}: {r}");
    }
    let r = tool(
        &hub,
        &ann,
        "memory_update",
        json!({ "id": secret, "visibility": "shared" }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let read = tool(&hub, &bea, "memory_read", json!({ "id": secret })).await;
    assert_eq!(read["structuredContent"]["title"], "Gift plan");
    let read = tool(&hub, &ann, "memory_read", json!({ "id": hers })).await;
    assert_eq!(
        read["isError"], true,
        "his search never shows her private memory"
    );
}

#[tokio::test]
async fn app_calls_stay_with_the_callers_machines_and_household_questions_obey_the_switches() {
    let hub = start().await;
    let all = DEFAULT_PAIRED_SCOPES;
    let (ann_t, bea_t) = (
        add(&hub, "Ann", "desk", all),
        add(&hub, "Bea", "laptop", all),
    );
    let kit_t = add(&hub, "Kit", "pc", "relay");
    let (mut ann, ann_person, ann_id) = join(&hub, &ann_t).await;
    let (mut bea, bea_person, _) = join(&hub, &bea_t).await;
    let (mut kit, _, _) = join(&hub, &kit_t).await;

    // Her app call goes down her line, named as her machine, and never down his.
    let h = hub.clone();
    let caller = tokio::spawn(async move {
        tool(
            &h,
            &bea_t,
            "app_list_chats",
            json!({ "query": "x", "machine": "laptop" }),
        )
        .await
    });
    let frame = next_json(&mut bea).await;
    assert_eq!(
        (frame["type"].as_str(), frame["client"].as_str()),
        (Some("call"), Some("laptop"))
    );
    assert_eq!(
        frame["args"],
        json!({ "query": "x" }),
        "the machine name is not forwarded"
    );
    send_json(
        &mut bea,
        json!({ "type": "result", "id": frame["id"], "ok": true, "result": { "chats": [] } }),
    )
    .await;
    assert_eq!(caller.await.unwrap()["isError"], false);
    assert!(quiet(&mut ann).await);

    // The household: everyone is listed with who is online; nothing is shared yet.
    let ask = |id: &str, op: &str, args: Value| json!({ "type": "peer", "id": id, "op": op, "args": args });
    send_json(&mut bea, ask("q1", "people", json!({}))).await;
    let r = next_json(&mut bea).await;
    assert_eq!(
        (r["type"].as_str(), r["id"].as_str(), r["ok"].as_bool()),
        (Some("peer_result"), Some("q1"), Some(true))
    );
    let people = r["result"]["people"].as_array().unwrap();
    assert_eq!(people.len(), 3);
    let him = people.iter().find(|p| p["id"] == ann_person).unwrap();
    assert_eq!(him["machines"][0]["online"], true);
    assert_eq!(him["machines"][0]["sharing"]["chats"], false);
    assert_eq!(
        people.iter().find(|p| p["id"] == bea_person).unwrap()["you"],
        true
    );

    // Each op is refused, in words, until the switch it needs is on.
    let refusal = |r: &Value, needle: &str| {
        assert_eq!(r["ok"], false, "{r}");
        let e = r["error"].as_str().unwrap();
        assert!(e.contains(needle), "{e}");
    };
    send_json(
        &mut bea,
        ask("q2", "list_chats", json!({ "personId": ann_person })),
    )
    .await;
    refusal(&next_json(&mut bea).await, "not shared their chats");
    send_json(&mut kit, ask("k1", "people", json!({}))).await;
    refusal(&next_json(&mut kit).await, "cannot use the household");

    let flip = |chats, transcripts, messages| json!({ "type": "sharing", "chats": chats, "transcripts": transcripts, "messages": messages });
    send_json(&mut ann, flip(true, false, false)).await;
    for _ in 0..100 {
        if hub.lines.get(ann_id).unwrap().sharing().chats {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    send_json(
        &mut bea,
        ask(
            "q3",
            "read_chat",
            json!({ "personId": ann_person, "chatId": "c1" }),
        ),
    )
    .await;
    refusal(&next_json(&mut bea).await, "not shared their transcripts");
    send_json(
        &mut bea,
        ask(
            "q4",
            "send_message",
            json!({ "personId": ann_person, "chatId": "c1", "message": "hi" }),
        ),
    )
    .await;
    refusal(&next_json(&mut bea).await, "message their chats");

    // With the chat list shared, her question reaches his Helm as a peer_call, with only the
    // arguments the op knows, and his answer comes back to her unchanged.
    let (answer, _) = tokio::join!(
        async {
            send_json(
                &mut bea,
                ask(
                    "q5",
                    "list_chats",
                    json!({ "personId": ann_person, "limit": 5, "extra": "x" }),
                ),
            )
            .await;
            next_json(&mut bea).await
        },
        async {
            let f = next_json(&mut ann).await;
            assert_eq!(
                (f["type"].as_str(), f["person"].as_str(), f["op"].as_str()),
                (Some("peer_call"), Some("Bea"), Some("list_chats"))
            );
            assert_eq!(f["args"], json!({ "limit": 5 }));
            send_json(
                &mut ann,
                json!({ "type": "result", "id": f["id"], "ok": true, "result": { "chats": [] } }),
            )
            .await;
        }
    );
    assert_eq!(
        (answer["id"].as_str(), answer["ok"].as_bool()),
        (Some("q5"), Some(true))
    );
    assert_eq!(answer["result"], json!({ "chats": [] }));

    // Once his machine is gone she is told so.
    drop(ann);
    for _ in 0..100 {
        if !hub.lines.get(ann_id).unwrap().is_up() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    send_json(
        &mut bea,
        ask("q6", "list_chats", json!({ "personId": ann_person })),
    )
    .await;
    refusal(&next_json(&mut bea).await, "offline");
}

#[derive(Default)]
struct FakeApi {
    calls: Mutex<Vec<(String, Value)>>,
}

impl TelegramApi for FakeApi {
    fn call<'a>(&'a self, method: &'a str, body: Value) -> ApiFuture<'a> {
        self.calls.lock().unwrap().push((method.to_owned(), body));
        Box::pin(async { Ok::<Value, ApiError>(json!({ "message_id": 1 })) })
    }
}

fn message(from: i64, text: &str) -> Value {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    json!({ "update_id": NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), "message": {
        "message_id": 9, "date": now_ms() / 1000, "text": text,
        "from": { "id": from, "is_bot": false, "first_name": "x" },
        "chat": { "id": from, "type": "private" } } })
}

fn frames(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    while let Ok(f) = rx.try_recv() {
        out.push(serde_json::from_str(&f).unwrap());
    }
    out.into_iter().filter(|f| f["type"] != "welcome").collect()
}

#[tokio::test]
async fn telegram_goes_to_each_persons_own_helm_and_replies_come_back_to_them_only() {
    const ANN_TG: i64 = 11;
    const BEA_TG: i64 = 22;
    let registry = Arc::new(Mutex::new(Registry::open_in_memory().unwrap()));
    let scopes = Scopes::parse_list(DEFAULT_PAIRED_SCOPES).unwrap();
    let client = |person: &str, machine: &str| {
        let mut r = registry.lock().unwrap();
        let token = r.add_client(person, machine, &scopes).unwrap().token;
        r.authenticate(Some(&format!("Bearer {token}"))).unwrap()
    };
    let (ann, bea) = (client("Ann", "desk"), client("Bea", "laptop"));
    let (lines, mut upstream) = Lines::new(registry.clone());
    let lines = Arc::new(lines);
    let (ann_line, bea_line): (Arc<Line>, Arc<Line>) = (lines.line_for(&ann), lines.line_for(&bea));
    let (tx_o, mut rx_o) = mpsc::unbounded_channel();
    let (tx_t, mut rx_t) = mpsc::unbounded_channel();
    ann_line.attach(tx_o);
    let bea_gen = bea_line.attach(tx_t);

    // 11 is the owner's; 22 is named as Bea's.
    let cfg = TelegramConfig {
        token: Some("t".into()),
        allowed: vec![ANN_TG, BEA_TG],
        people: vec![(BEA_TG, "Bea".into())],
        api_base: "http://fake".into(),
        queue_lifetime: Duration::from_secs(1800),
        queue_max: 5,
        receive: Receive::Off,
        stt: None,
    };
    let api = Arc::new(FakeApi::default());
    let mut resolve = |name: &str| -> Result<i64, String> {
        let mut r = registry.lock().unwrap();
        r.ensure_person(name)
            .map(|p| p.id)
            .map_err(|e| e.to_string())
    };
    let tg = Telegram::build(
        api.clone(),
        &lines,
        &cfg,
        ann.person_id,
        &mut resolve,
        &|b: Bridge| b,
    )
    .unwrap();

    let sent = |rx: &mut mpsc::UnboundedReceiver<String>| -> Vec<String> {
        frames(rx)
            .iter()
            .filter(|f| f["type"] == "telegram")
            .map(|f| f["text"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        tg.on_update(&message(ANN_TG, "for ann"), now_ms()).await,
        Handled::Forwarded
    );
    assert_eq!(
        tg.on_update(&message(BEA_TG, "for bea"), now_ms()).await,
        Handled::Forwarded
    );
    assert_eq!(
        tg.on_update(&message(33, "stranger"), now_ms()).await,
        Handled::Ignored
    );
    assert_eq!(sent(&mut rx_o), ["for ann"]);
    assert_eq!(sent(&mut rx_t), ["for bea"]);

    // What her machine sends up reaches her Telegram user only.
    assert!(bea_line.receive(bea_gen, r#"{"type":"telegram_final","text":"done"}"#));
    let (person, frame) = upstream.recv().await.unwrap();
    assert_eq!(
        (person, &frame),
        (bea.person_id, &Upstream::TelegramFinal("done".into()))
    );
    tg.on_upstream(person, frame).await;
    let texts: Vec<(i64, String)> = api
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == "sendMessage")
        .map(|(_, b)| {
            (
                b["chat_id"].as_i64().unwrap(),
                b["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(texts, [(BEA_TG, "done".to_owned())]);
}

/// Read frames until one of type `ty` arrives; the others (welcome, device lists) are skipped.
async fn next_of(ws: &mut Ws, ty: &str) -> Value {
    loop {
        let f = next_any(ws).await;
        if f["type"] == ty {
            return f;
        }
    }
}

/// The first `devices` list `ok` accepts.
async fn devices_where(ws: &mut Ws, ok: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    loop {
        let list = next_of(ws, "devices").await["list"]
            .as_array()
            .unwrap()
            .clone();
        if ok(&list) {
            return list;
        }
    }
}

fn device<'a>(list: &'a [Value], uid: &str) -> &'a Value {
    list.iter().find(|d| d["uid"] == uid).unwrap()
}

#[tokio::test]
async fn devices_see_each_other_and_calls_pushes_and_refusals_pass_between_them() {
    const DESK: &str = "desk-0000-0001";
    const LAPTOP: &str = "laptop-0000-0002";
    const NOBODY: &str = "nobody-0000-0000";
    let hub = start().await;
    let all = DEFAULT_PAIRED_SCOPES;
    let (ann_t, bea_t, cy_t) = (
        add(&hub, "Ann", "desk", all),
        add(&hub, "Bea", "laptop", all),
        add(&hub, "Cy", "tablet", all),
    );
    let kit_t = add(&hub, "Kit", "pc", "relay");
    let (mut ann, ann_person, _) = join(&hub, &ann_t).await;
    let (mut bea, bea_person, _) = join(&hub, &bea_t).await;
    let (mut kit, _, _) = join(&hub, &kit_t).await;
    let (mut cy, _, _) = join(&hub, &cy_t).await;
    let announce = |uid: &str, name: &str, run: bool| json!({ "type": "device", "uid": uid, "name": name, "run": run });
    let call = |id: Value, to: &str, op: &str, args: Value| json!({ "type": "dev", "id": id, "to": to, "op": op, "args": args });
    let code = |r: &Value| r["error"]["code"].as_str().unwrap_or("none").to_owned();

    // Two people's devices appear on both lines, each marked on its own; a token without the
    // peer scope is not part of the household.
    send_json(&mut ann, announce(DESK, "Desk", true)).await;
    send_json(&mut bea, announce(LAPTOP, "Laptop", false)).await;
    send_json(&mut kit, announce("kit-0000-0003", "Kit PC", true)).await;
    let seen = devices_where(&mut ann, |l| l.len() == 2).await;
    let (desk, laptop) = (device(&seen, DESK), device(&seen, LAPTOP));
    assert_eq!(
        (
            desk["self"].as_bool(),
            desk["online"].as_bool(),
            desk["run"].as_bool(),
            desk["person"].as_str(),
            desk["personId"].as_str()
        ),
        (
            Some(true),
            Some(true),
            Some(true),
            Some("Ann"),
            Some(ann_person.to_string().as_str())
        )
    );
    assert_eq!(
        (
            laptop["self"].as_bool(),
            laptop["name"].as_str(),
            laptop["run"].as_bool(),
            laptop["personId"].as_str()
        ),
        (
            Some(false),
            Some("Laptop"),
            Some(false),
            Some(bea_person.to_string().as_str())
        )
    );
    let seen = devices_where(&mut bea, |l| l.len() == 2).await;
    assert_eq!(
        (
            device(&seen, LAPTOP)["self"].as_bool(),
            device(&seen, DESK)["self"].as_bool()
        ),
        (Some(true), Some(false))
    );

    // A call reaches the other device under the hub's id; the answer returns under the sender's.
    let (answer, _) = tokio::join!(
        async {
            let ask = call(json!("c1"), DESK, "hello", json!({ "x": 1 }));
            send_json(&mut bea, ask).await;
            next_of(&mut bea, "dev_result").await
        },
        async {
            let c = next_of(&mut ann, "dev_call").await;
            assert_eq!(
                (c["op"].as_str(), c["args"].clone()),
                (Some("hello"), json!({ "x": 1 }))
            );
            assert_eq!(
                c["from"],
                json!({ "uid": LAPTOP, "name": "Laptop", "personId": bea_person.to_string(),
                        "person": "Bea" })
            );
            assert_ne!(c["id"], "c1");
            let r = json!({ "type": "dev_result", "id": c["id"], "ok": true,
                            "result": { "device": "Desk" } });
            send_json(&mut ann, r).await;
        }
    );
    assert_eq!(
        (answer["id"].as_str(), answer["ok"].as_bool()),
        (Some("c1"), Some(true))
    );
    assert_eq!(answer["result"], json!({ "device": "Desk" }));

    // The other device's own refusal comes back as it was, under a numeric id too.
    let (answer, _) = tokio::join!(
        async {
            send_json(&mut bea, call(json!(7), DESK, "send", json!({}))).await;
            next_of(&mut bea, "dev_result").await
        },
        async {
            let c = next_of(&mut ann, "dev_call").await;
            let error = json!({ "code": "not_allowed", "message": "No." });
            let r = json!({ "type": "dev_result", "id": c["id"], "ok": false, "error": error });
            send_json(&mut ann, r).await;
        }
    );
    assert_eq!(
        (
            answer["id"].clone(),
            answer["ok"].as_bool(),
            answer["error"].clone()
        ),
        (
            json!(7),
            Some(false),
            json!({ "code": "not_allowed", "message": "No." })
        )
    );

    // The hub's own refusals: no such device, too large, no peer scope, device not announced.
    send_json(&mut bea, call(json!("e1"), NOBODY, "hello", json!({}))).await;
    let r = next_of(&mut bea, "dev_result").await;
    assert_eq!(
        (r["id"].as_str(), code(&r).as_str()),
        (Some("e1"), "offline")
    );
    let pad = "x".repeat(1024 * 1024 + 10);
    send_json(
        &mut bea,
        call(json!("e2"), DESK, "hello", json!({ "pad": pad })),
    )
    .await;
    assert_eq!(code(&next_of(&mut bea, "dev_result").await), "too_large");
    send_json(&mut kit, call(json!("k1"), DESK, "hello", json!({}))).await;
    assert_eq!(code(&next_of(&mut kit, "dev_result").await), "scope");
    send_json(&mut cy, call(json!("y1"), DESK, "hello", json!({}))).await;
    assert_eq!(code(&next_of(&mut cy, "dev_result").await), "bad_request");
    // A device id another person holds cannot be taken over.
    send_json(&mut cy, announce(DESK, "Stolen", true)).await;
    send_json(&mut cy, call(json!("y2"), LAPTOP, "hello", json!({}))).await;
    assert_eq!(code(&next_of(&mut cy, "dev_result").await), "bad_request");
    let holder = hub.registry.lock().unwrap().client_by_device(DESK).unwrap();
    assert_eq!(holder.unwrap().person, "Ann");

    // A push is stamped with the sender's device; one for a device that is not up is dropped.
    let push = |to: &str, seq: i64| {
        json!({ "type": "dev_push", "to": to, "sub": "chat:c1", "seq": seq,
                "event": { "kind": "message" } })
    };
    send_json(&mut ann, push(NOBODY, 1)).await;
    send_json(&mut ann, push(LAPTOP, 2)).await;
    let p = next_of(&mut bea, "dev_push").await;
    assert_eq!(
        (
            p["from"].as_str(),
            p["sub"].as_str(),
            p["seq"].as_i64(),
            p["event"].clone()
        ),
        (
            Some(DESK),
            Some("chat:c1"),
            Some(2),
            json!({ "kind": "message" })
        )
    );

    // A dropped line shows as offline with a last-seen time and calls to it say so; the hub
    // remembers the device, so it is back as itself when its Helm reconnects.
    drop(ann);
    let seen = devices_where(&mut bea, |l| device(l, DESK)["online"] == false).await;
    assert!(device(&seen, DESK)["lastSeenAt"].is_string());
    send_json(&mut bea, call(json!("e3"), DESK, "hello", json!({}))).await;
    assert_eq!(code(&next_of(&mut bea, "dev_result").await), "offline");
    let (mut ann, _, _) = join(&hub, &ann_t).await;
    let seen = devices_where(&mut ann, |l| device(l, DESK)["online"] == true).await;
    let desk = device(&seen, DESK);
    assert_eq!(
        (desk["self"].as_bool(), desk["run"].as_bool()),
        (Some(true), Some(true))
    );
    assert!(
        quiet(&mut kit).await,
        "a line without peer hears nothing of the household"
    );
}

#[tokio::test]
async fn knowledge_files_sync_through_the_hub_with_a_conflict_on_a_stale_base() {
    let hub = start().await;
    let all = DEFAULT_PAIRED_SCOPES;
    let (ann_t, bea_t) = (
        add(&hub, "Ann", "desk", all),
        add(&hub, "Bea", "laptop", all),
    );
    let kit_t = add(&hub, "Kit", "pc", "relay");
    let (mut ann, _, _) = join(&hub, &ann_t).await;
    let (mut bea, bea_person, _) = join(&hub, &bea_t).await;
    let (mut kit, _, _) = join(&hub, &kit_t).await;
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let sha = |s: &str| format!("{:x}", Sha256::digest(s.as_bytes()));
    let put = |id: &str, layer: &str, path: &str, base: i64, body: Option<&str>| {
        json!({
            "type": "kn_put", "id": id, "layer": layer, "path": path, "baseSeq": base,
            "sha256": body.map(sha).unwrap_or_default(),
            "deleted": body.is_none(), "body": body.map(b64),
        })
    };
    let pull = |id: &str, since: i64, limit: i64| json!({ "type": "kn_pull", "id": id, "since": since, "limit": limit });
    let result = |r: &Value| {
        (
            r["id"].as_str().map(str::to_owned),
            r["ok"].as_bool(),
            r["seq"].as_i64(),
        )
    };

    // A file put on one line is announced to every peer line and pulled on the other.
    send_json(
        &mut ann,
        put("p1", "everyone", "rules/style.md", 0, Some("be kind")),
    )
    .await;
    let r = next_of(&mut ann, "kn_put_result").await;
    assert_eq!(result(&r), (Some("p1".into()), Some(true), Some(1)));
    assert_eq!(next_of(&mut bea, "kn_changed").await["seq"], 1);
    assert_eq!(next_of(&mut ann, "kn_changed").await["seq"], 1);
    send_json(&mut bea, pull("q1", 0, 50)).await;
    let r = next_of(&mut bea, "kn_files").await;
    let f = &r["files"][0];
    assert_eq!(
        (
            r["files"].as_array().unwrap().len(),
            r["cursor"].as_i64(),
            r["more"].as_bool()
        ),
        (1, Some(1), Some(false))
    );
    assert_eq!(
        (
            f["layer"].as_str(),
            f["path"].as_str(),
            f["seq"].as_i64(),
            f["size"].as_i64()
        ),
        (Some("everyone"), Some("rules/style.md"), Some(1), Some(7))
    );
    assert_eq!(
        (f["sha256"].as_str(), f["body"].as_str()),
        (Some(sha("be kind").as_str()), Some(b64("be kind").as_str()))
    );

    // A put from a stale base conflicts and reports the stored copy; from the current one it lands.
    send_json(
        &mut bea,
        put("p2", "everyone", "rules/style.md", 0, Some("be bold")),
    )
    .await;
    let r = next_of(&mut bea, "kn_put_result").await;
    assert_eq!(
        (
            r["ok"].as_bool(),
            r["conflict"]["seq"].as_i64(),
            r["conflict"]["sha256"].as_str()
        ),
        (Some(false), Some(1), Some(sha("be kind").as_str()))
    );
    send_json(
        &mut bea,
        put("p3", "everyone", "rules/style.md", 1, Some("be bold")),
    )
    .await;
    let r = next_of(&mut bea, "kn_put_result").await;
    assert_eq!(result(&r), (Some("p3".into()), Some(true), Some(2)));
    assert_eq!(next_of(&mut ann, "kn_changed").await["seq"], 2);

    // A deletion travels as a file without a body.
    send_json(&mut ann, put("p4", "everyone", "rules/style.md", 2, None)).await;
    let r = next_of(&mut ann, "kn_put_result").await;
    assert_eq!(result(&r), (Some("p4".into()), Some(true), Some(3)));
    send_json(&mut bea, pull("q2", 2, 50)).await;
    let r = next_of(&mut bea, "kn_files").await;
    let f = &r["files"][0];
    assert_eq!(
        (
            f["deleted"].as_bool(),
            f.get("body").is_none(),
            r["cursor"].as_i64()
        ),
        (Some(true), true, Some(3))
    );

    // A person's layer needs a person who exists; bad layers, paths and checksums are refused in
    // words.
    let layer = format!("person:{bea_person}");
    let skill = "---\nname: tidy\n---";
    send_json(
        &mut ann,
        put("p5", &layer, "skills/tidy/SKILL.md", 0, Some(skill)),
    )
    .await;
    let r = next_of(&mut ann, "kn_put_result").await;
    assert_eq!(result(&r), (Some("p5".into()), Some(true), Some(4)));
    for (id, bad_layer, path, why) in [
        ("r1", "person:999", "rules/a.md", "no such person"),
        ("r2", "team", "rules/a.md", "knowledge layer"),
        ("r3", "everyone", "rules/../a.md", "cannot be stored"),
        ("r4", "everyone", "notes/a.md", "Files live under"),
    ] {
        send_json(&mut ann, put(id, bad_layer, path, 0, Some("x"))).await;
        let r = next_of(&mut ann, "kn_put_result").await;
        assert_eq!(r["ok"], false, "{id}: {r}");
        assert!(r["error"].as_str().unwrap().contains(why), "{id}: {r}");
    }
    let bad = json!({ "type": "kn_put", "id": "r5", "layer": "everyone", "path": "rules/b.md",
                      "baseSeq": 0, "sha256": "00", "deleted": false, "body": b64("x") });
    send_json(&mut ann, bad).await;
    let r = next_of(&mut ann, "kn_put_result").await;
    assert!(r["error"].as_str().unwrap().contains("sha256"), "{r}");

    // A pull pages by its cursor.
    send_json(&mut bea, pull("q3", 0, 1)).await;
    let r = next_of(&mut bea, "kn_files").await;
    assert_eq!(
        (
            r["files"].as_array().unwrap().len(),
            r["cursor"].as_i64(),
            r["more"].as_bool()
        ),
        (1, Some(3), Some(true))
    );
    send_json(&mut bea, pull("q4", 3, 1)).await;
    let r = next_of(&mut bea, "kn_files").await;
    assert_eq!(
        (
            r["files"][0]["layer"].as_str(),
            r["files"][0]["path"].as_str(),
            r["more"].as_bool()
        ),
        (
            Some(layer.as_str()),
            Some("skills/tidy/SKILL.md"),
            Some(false)
        )
    );

    // A line without the peer scope gets a sentence and none of the news.
    send_json(&mut kit, pull("k1", 0, 10)).await;
    let r = next_of(&mut kit, "kn_files").await;
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("cannot use the household"),
        "{r}"
    );
    assert!(quiet(&mut kit).await);
}

/// One memory call as `token`, acting as `who` (the `X-Helm-Person` header) when given.
async fn tool_as(
    hub: &Hub,
    token: &str,
    who: Option<&str>,
    name: &str,
    args: Value,
) -> (u16, Value) {
    let headers: Vec<(&str, &str)> = who.map(|w| ("x-helm-person", w)).into_iter().collect();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } });
    let (status, v) = post_with(hub, "/mcp", Some(token), &headers, body).await;
    (status, v["result"].clone())
}

#[tokio::test]
async fn a_helm_acts_as_the_person_it_names_and_memories_say_who_wrote_them() {
    let hub = start().await;
    let ann = add(&hub, "Ann", "desk", "memory:rw,peer");
    let kit = add(&hub, "Kit", "pc", "memory:rw");
    let bea_id = hub
        .registry
        .lock()
        .unwrap()
        .ensure_person("Bea")
        .unwrap()
        .id
        .to_string();
    let bea = bea_id.as_str();
    let count = |r: &Value| r["structuredContent"]["count"].as_i64().unwrap();
    let owners = |r: &Value| -> Vec<String> {
        let mut names: Vec<String> = r["structuredContent"]["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["owner"].as_str().unwrap_or("?").to_owned())
            .collect();
        names.sort();
        names
    };

    // What Ann's machine writes while acting as Bea is Bea's: private by default, hers to read.
    let write = json!({ "title": "Tea", "body": "likes green tea" });
    let (status, w) = tool_as(&hub, &ann, Some(bea), "memory_write", write).await;
    assert_eq!((status, w["isError"].clone()), (200, json!(false)));
    let tea = w["structuredContent"]["id"].as_i64().unwrap();
    let green = json!({ "query": "green" });
    let (_, r) = tool_as(&hub, &ann, None, "memory_search", green.clone()).await;
    assert_eq!(
        count(&r),
        0,
        "Ann herself does not see Bea's private memory"
    );
    let (_, r) = tool_as(&hub, &ann, Some(bea), "memory_search", green).await;
    assert_eq!((count(&r), owners(&r)), (1, vec!["Bea".to_string()]));

    // `scope` everyone (or its alias shared) makes a memory the household's; the person may be
    // named by name as well as by id.
    let rule = json!({ "title": "House rule", "body": "quokka shoes off", "scope": "everyone" });
    let (_, w) = tool_as(&hub, &ann, Some("Bea"), "memory_write", rule).await;
    assert_eq!(w["isError"], false, "{w}");
    let secret = json!({ "title": "Ann secret", "body": "quokka plan", "scope": "me" });
    tool_as(&hub, &ann, None, "memory_write", secret).await;
    let garden = json!({ "title": "Ann garden", "body": "tomatoes", "scope": "shared" });
    tool_as(&hub, &ann, None, "memory_write", garden).await;
    let quokka = json!({ "query": "quokka" });
    let (_, r) = tool_as(&hub, &ann, None, "memory_search", quokka).await;
    assert_eq!(
        (count(&r), owners(&r)),
        (2, vec!["Ann".to_string(), "Bea".to_string()])
    );

    // person: me and any are the memories the person may see; a name or id narrows them; someone
    // else's private memory never crosses.
    let (_, mine) = tool_as(&hub, &ann, Some(bea), "memory_list", json!({})).await;
    let any = json!({ "person": "any" });
    let (_, any) = tool_as(&hub, &ann, Some(bea), "memory_list", any).await;
    let by_name = json!({ "person": "Ann" });
    let (_, anns) = tool_as(&hub, &ann, Some(bea), "memory_list", by_name).await;
    let by_id = json!({ "person": bea });
    let (_, beas) = tool_as(&hub, &ann, Some(bea), "memory_list", by_id).await;
    assert_eq!((count(&mine), count(&any)), (3, 3));
    assert_eq!((count(&anns), owners(&anns)), (1, vec!["Ann".to_string()]));
    assert_eq!(count(&beas), 2);
    let across = json!({ "query": "quokka", "person": "any" });
    let (_, r) = tool_as(&hub, &ann, Some(bea), "memory_search", across).await;
    assert_eq!((count(&r), owners(&r)), (1, vec!["Bea".to_string()]));
    let nobody = json!({ "person": "Nobody" });
    let (_, r) = tool_as(&hub, &ann, Some(bea), "memory_list", nobody).await;
    assert_eq!(r["isError"], true);
    let (_, own) = tool_as(&hub, &ann, None, "memory_list", json!({})).await;
    assert_eq!(count(&own), 3, "Ann sees her two and Bea's shared one");

    // Only the owner changes a memory, and acting as the owner counts.
    let share = json!({ "id": tea, "scope": "everyone" });
    let (_, r) = tool_as(&hub, &ann, None, "memory_update", share.clone()).await;
    assert_eq!(r["isError"], true);
    let (_, r) = tool_as(&hub, &ann, Some(bea), "memory_update", share).await;
    assert_eq!(r["isError"], false, "{r}");
    let (_, r) = tool_as(&hub, &ann, None, "memory_read", json!({ "id": tea })).await;
    assert_eq!(
        (
            r["structuredContent"]["owner"].as_str(),
            r["structuredContent"]["visibility"].as_str()
        ),
        (Some("Bea"), Some("shared"))
    );
    let both = json!({ "title": "x", "body": "y", "scope": "me", "visibility": "shared" });
    let (_, r) = tool_as(&hub, &ann, Some(bea), "memory_write", both).await;
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("disagree"));

    // Naming nobody known is a 400, and a token without the peer scope may not act as anyone.
    for (token, who, status) in [(&ann, "999", 400), (&ann, "Nobody", 400), (&kit, bea, 403)] {
        let (s, _) = tool_as(&hub, token, Some(who), "memory_list", json!({})).await;
        assert_eq!(s, status, "{who}");
    }
    let (s, r) = tool_as(&hub, &kit, None, "memory_list", json!({})).await;
    assert_eq!((s, r["isError"].clone()), (200, json!(false)));
}
