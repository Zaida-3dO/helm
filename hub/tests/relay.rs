//! The relay line over a real socket: who may open it, forwarding through it, and the
//! offline answer when it is down (trust-boundary items b, d and f).

use futures_util::{SinkExt, StreamExt};
use helm_hub::lines::Lines;
use helm_hub::mcp::Options;
use helm_hub::registry::Registry;
use helm_hub::relay::{Line, Upstream, MAX_FRAME, OFFLINE};
use helm_hub::server::{router, AppState};
use helm_hub::store::Store;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const LINE: &str = "line-token-0123456789abcdefghij";
const REMOTE: &str = "remote-token-0123456789abcdefghi";
const MEMORY_ONLY: &str = "memory-token-0123456789abcdefgh";
const FORWARDER: &str = "forwarder-token-0123456789abcdef";
const PHONE: &str = "phone-token-0123456789abcdefghij";
const PAGE: &str = "page-token-0123456789abcdefghijkl";
const INDEXER: &str = "indexer-token-0123456789abcdefgh";

/// The person who owns every client the legacy token list makes (the first person made).
const OWNER: i64 = 1;

struct Hub {
    addr: String,
    lines: Arc<Lines>,
}

async fn start() -> Hub {
    let registry = Registry::from_legacy_spec(
        &format!(
            "helm-line:{LINE}:relay, remote:{REMOTE}:ro+app, notes:{MEMORY_ONLY}:rw, \
             forwarder:{FORWARDER}:telegram, phone:{PHONE}:voice, page:{PAGE}:remote, indexer:{INDEXER}:relay+peer+app+rw"
        ),
        "owner",
    )
    .unwrap();
    let registry = Arc::new(Mutex::new(registry));
    let (lines, _upstream) = Lines::new(registry.clone());
    let lines = Arc::new(lines);
    let app = router(Arc::new(AppState::new(
        Store::open_in_memory().unwrap(),
        registry,
        Options {
            allow_mutate: false,
        },
        lines.clone(),
        None,
    )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Hub { addr, lines }
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

async fn mcp(hub: &Hub, token: &str, body: Value) -> (u16, Value) {
    // A tiny HTTP/1.1 client over the socket, so the test needs no HTTP client crate.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = body.to_string();
    let mut s = tokio::net::TcpStream::connect(&hub.addr).await.unwrap();
    let req = format!(
        "POST /mcp HTTP/1.1\r\nhost: hub\r\nauthorization: Bearer {token}\r\n\
         content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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

fn call(tool: &str, args: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": tool, "arguments": args } })
}

async fn wait_up(hub: &Hub, up: bool) {
    for _ in 0..100 {
        if hub.lines.best_up(OWNER).is_some() == up {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the line never became {}", if up { "up" } else { "down" });
}

#[tokio::test]
async fn only_a_relay_token_opens_the_line() {
    let hub = start().await;
    assert_eq!(
        connect(&hub, "not-a-token-at-all-0123456789").await.err(),
        Some(401)
    );
    assert_eq!(
        connect(&hub, REMOTE).await.err(),
        Some(403),
        "an MCP token cannot open it"
    );
    assert_eq!(connect(&hub, MEMORY_ONLY).await.err(), Some(403));
    assert_eq!(
        connect(&hub, FORWARDER).await.err(),
        Some(403),
        "a telegram token cannot open it"
    );
    assert_eq!(
        connect(&hub, PHONE).await.err(),
        Some(403),
        "a voice token cannot open it"
    );
    assert_eq!(
        connect(&hub, PAGE).await.err(),
        Some(403),
        "a Remote-page token cannot open it"
    );
    let mut ws = connect(&hub, LINE).await.expect("the relay token opens it");
    assert_eq!(next_json(&mut ws).await["type"], "welcome");
    // ...and neither the relay token, a telegram token, a voice token nor a Remote-page token
    // reaches the MCP surface.
    for token in [LINE, FORWARDER, PHONE, PAGE] {
        let (status, _) = mcp(
            &hub,
            token,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;
        assert_eq!(status, 403);
    }
}

#[tokio::test]
async fn app_tools_are_listed_only_for_app_clients() {
    let hub = start().await;
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let names = |v: &Value| -> Vec<String> {
        v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, v) = mcp(&hub, REMOTE, list.clone()).await;
    assert!(names(&v).contains(&"app_send_message".to_owned()));
    assert!(names(&v).contains(&"memory_search".to_owned()));
    let (_, v) = mcp(&hub, MEMORY_ONLY, list).await;
    assert!(!names(&v).iter().any(|n| n.starts_with("app_")));
    // A client without +app cannot call one either, even with the line up: nothing is
    // forwarded, and the answer is the memory surface's own "unknown tool".
    let mut ws = connect(&hub, LINE).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "welcome");
    wait_up(&hub, true).await;
    let addr = Hub {
        addr: hub.addr.clone(),
        lines: hub.lines.clone(),
    };
    let caller =
        tokio::spawn(
            async move { mcp(&addr, MEMORY_ONLY, call("app_list_chats", json!({}))).await },
        );
    let quiet = tokio::time::timeout(Duration::from_millis(500), ws.next()).await;
    assert!(
        quiet.is_err(),
        "a non-app client's call was forwarded: {quiet:?}"
    );
    let (_, v) = caller.await.unwrap();
    assert_eq!(v["result"]["isError"], true);
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("unknown tool"), "{text}");
}

#[tokio::test]
async fn line_down_answers_offline_and_sends_nothing() {
    let hub = start().await;
    let (status, v) = mcp(
        &hub,
        REMOTE,
        call(
            "app_send_message",
            json!({"chatId": "c1", "message": "hi", "mode": "queue"}),
        ),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(v["result"]["isError"], true);
    assert_eq!(v["result"]["content"][0]["text"], OFFLINE);
}

#[tokio::test]
async fn a_forwarded_call_goes_down_the_line_and_its_answer_comes_back() {
    let hub = start().await;
    let mut ws = connect(&hub, LINE).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "welcome");
    wait_up(&hub, true).await;
    let addr = Hub {
        addr: hub.addr.clone(),
        lines: hub.lines.clone(),
    };
    let caller = tokio::spawn(async move {
        mcp(
            &addr,
            REMOTE,
            call("app_list_chats", json!({"query": "hub"})),
        )
        .await
    });
    let frame = next_json(&mut ws).await;
    assert_eq!(frame["type"], "call");
    assert_eq!(frame["tool"], "app_list_chats");
    assert_eq!(
        frame["client"], "remote",
        "the hub names the caller from its token"
    );
    assert_eq!(frame["args"], json!({"query": "hub"}));
    ws.send(Message::Text(
        json!({"type": "result", "id": frame["id"], "ok": true, "result": {"chats": []}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let (_, v) = caller.await.unwrap();
    assert_eq!(v["result"]["isError"], false);
    assert_eq!(v["result"]["structuredContent"], json!({"chats": []}));
}

#[tokio::test]
async fn names_outside_the_vetted_set_are_never_forwarded() {
    let hub = start().await;
    let mut ws = connect(&hub, LINE).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "welcome");
    wait_up(&hub, true).await;
    for tool in [
        "app_update_settings",
        "app_add_mcp_server",
        "app_mcp_call",
        "app_create_project",
    ] {
        let (_, v) = mcp(&hub, REMOTE, call(tool, json!({}))).await;
        assert_eq!(v["result"]["isError"], true, "{tool}");
    }
    // Nothing reached the line.
    let quiet = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    assert!(quiet.is_err(), "a frame was forwarded: {quiet:?}");
}

#[tokio::test]
async fn a_line_that_drops_mid_call_answers_may_or_may_not_have_run() {
    let hub = start().await;
    let mut ws = connect(&hub, LINE).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "welcome");
    wait_up(&hub, true).await;
    let addr = Hub {
        addr: hub.addr.clone(),
        lines: hub.lines.clone(),
    };
    let caller = tokio::spawn(async move {
        mcp(
            &addr,
            REMOTE,
            call("app_stop_chat", json!({"chatId": "c1"})),
        )
        .await
    });
    assert_eq!(next_json(&mut ws).await["type"], "call");
    drop(ws);
    let (_, v) = caller.await.unwrap();
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(v["result"]["isError"], true);
    assert!(text.contains("may or may not have run"), "{text}");
    wait_up(&hub, false).await;
}

#[tokio::test]
async fn the_hub_list_is_checked_again_at_the_call() {
    // Defence in depth: even called directly, a name off the hub's list is not forwarded.
    let (line, _up) = Line::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    line.attach(tx);
    let _welcome = rx.try_recv().unwrap();
    let v =
        helm_hub::apptools::call(&line, "remote", "app_update_settings", &json!({}), None).await;
    assert_eq!(v["isError"], true);
    assert!(rx.try_recv().is_err(), "nothing went down the line");
    let v = helm_hub::apptools::call(&line, "remote", "app_list_chats", &json!([1]), None).await;
    assert_eq!(v["isError"], true, "arguments must be an object");
    assert!(rx.try_recv().is_err());

    // What the hub advertises matches what the PC takes: the chat list filters by person and
    // machine, and the only send mode it names is the one the PC still takes.
    let list = helm_hub::apptools::tool_list();
    let spec = |name: &str| {
        list.iter()
            .find(|t| t["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} is not advertised"))
    };
    let props = spec("app_list_chats")["inputSchema"]["properties"].clone();
    for arg in ["query", "person", "device", "all"] {
        assert!(props.get(arg).is_some(), "app_list_chats hides {arg}");
    }
    assert_eq!(
        spec("app_send_message")["inputSchema"]["properties"]["mode"]["enum"],
        json!(["steer"]),
        "the advertised send modes are not what the PC takes"
    );
}

#[tokio::test]
async fn an_upstream_frame_under_the_limit_is_accepted_and_a_bigger_one_ends_the_line() {
    let hub = start().await;
    let mut voice = hub.lines.voice_receiver();
    let mut ws = connect(&hub, LINE).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "welcome");
    // One picture of 1 MiB is about 1.4 MB as base64: it fits in a single frame.
    let data = "A".repeat(1_398_104);
    let frame = json!({"type": "voice_attachment", "reply": "synthetic-0001", "n": 1,
        "kind": "image", "title": "A picture", "mediaType": "image/png", "data": data});
    ws.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
    let (person, _client, up) = tokio::time::timeout(Duration::from_secs(5), voice.recv())
        .await
        .expect("the frame within 5 s")
        .unwrap();
    assert_eq!(person, OWNER);
    let Upstream::VoiceAttachment(a) = up else {
        panic!("not an attachment: {up:?}");
    };
    assert_eq!(a.data.map(|d| d.len()), Some(1_398_104));

    // A frame over the line's limit is refused and the line is closed.
    let big = json!({"type": "telegram_status", "text": "x".repeat(MAX_FRAME + 1)});
    ws.send(Message::Text(big.to_string().into()))
        .await
        .unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(
        ended.is_ok(),
        "the hub kept a line that sent a frame over the limit"
    );
    wait_up(&hub, false).await;
}

async fn send(ws: &mut Ws, frame: Value) {
    ws.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}

/// One request frame and its answer.
async fn ask(ws: &mut Ws, frame: Value) -> Value {
    send(ws, frame).await;
    loop {
        let f = next_json(ws).await;
        if f["type"] != "devices" {
            return f;
        }
    }
}

fn int8(v: &[f32]) -> (String, f32) {
    use base64::Engine as _;
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    let scale = max / 127.0;
    let q: Vec<u8> = v
        .iter()
        .map(|x| ((x / scale).round() as i8) as u8)
        .collect();
    (base64::engine::general_purpose::STANDARD.encode(q), scale)
}

#[tokio::test]
async fn a_machine_indexes_its_notes_and_they_are_found_by_words_and_by_meaning() {
    let hub = start().await;
    let mut ws = connect(&hub, INDEXER).await.unwrap();
    let welcome = next_json(&mut ws).await;
    assert_eq!(welcome["features"], json!(["index"]));
    // Only a token with the peer scope may use the index frames.
    let mut plain = connect(&hub, LINE).await.unwrap();
    next_json(&mut plain).await;
    let r = ask(
        &mut plain,
        json!({ "type": "ix_hello", "id": 1, "sources": [] }),
    )
    .await;
    assert_eq!(
        (r["type"].as_str(), r["ok"].as_bool()),
        (Some("ix_state"), Some(false))
    );

    let state = ask(
        &mut ws,
        json!({ "type": "ix_hello", "id": 2, "sources": [{ "id": "pc:notes", "kind": "folder", "name": "Notes" }] }),
    )
    .await;
    assert_eq!(state["sources"][0]["docs"], 0);

    let (b64, scale) = int8(&[1.0, 0.0, 0.0, 0.2]);
    let doc = |hash: &str, owner: &str| {
        json!({ "key": "garden/beds.md", "hash": hash, "title": "Garden beds", "locator": "garden/beds.md",
                "owner": owner, "scope": "me", "tier": "note", "occurredAt": 1_700_000_000_000i64,
                "chunks": [
                    { "ord": 0, "text": "Raised beds hold the tomatoes", "vec": b64, "scale": scale,
                      "model": "nomic-embed-text:latest" },
                    { "ord": 1, "text": "Compost goes in each autumn" },
                ] })
    };
    let put =
        |docs: Vec<Value>| json!({ "type": "ix_put", "id": 3, "source": "pc:notes", "docs": docs });
    let status = |r: &Value| r["results"][0]["status"].as_str().unwrap().to_owned();
    assert_eq!(
        status(&ask(&mut ws, put(vec![doc("h1", "1")])).await),
        "inserted"
    );
    assert_eq!(
        status(&ask(&mut ws, put(vec![doc("h1", "1")])).await),
        "unchanged"
    );
    assert_eq!(
        status(&ask(&mut ws, put(vec![doc("h2", "1")])).await),
        "replaced"
    );
    // A person this machine does not act for (or that does not exist) is refused.
    let refused = ask(&mut ws, put(vec![doc("h3", "2")])).await;
    assert_eq!(status(&refused), "refused", "{refused}");

    let page = ask(
        &mut ws,
        json!({ "type": "ix_manifest", "id": 4, "source": "pc:notes", "afterKey": "" }),
    )
    .await;
    assert_eq!(
        page["items"],
        json!([{ "key": "garden/beds.md", "hash": "h2" }])
    );
    assert_eq!(page["more"], false);
    let again = ask(
        &mut ws,
        json!({ "type": "ix_hello", "id": 5, "sources": [{ "id": "pc:notes", "kind": "folder" }] }),
    )
    .await;
    assert_eq!(again["sources"][0]["docs"], 1);
    use sha2::{Digest, Sha256};
    assert_eq!(
        again["sources"][0]["digest"],
        format!("{:x}", Sha256::digest(b"garden/beds.md:h2"))
    );

    // A source belongs to the machine that named it: no other machine may read or change it.
    let other = ask(
        &mut plain,
        json!({ "type": "ix_manifest", "id": 11, "source": "pc:notes" }),
    )
    .await;
    assert_eq!(other["ok"], false);
    // Close that line, so the query below is embedded by the indexer's PC (the only one up).
    drop(plain);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Chunk 1 has no vector yet: it is what `ix_pending` lists, until `ix_vectors` fills it.
    let waiting = ask(
        &mut ws,
        json!({ "type": "ix_pending", "id": 6, "limit": 50 }),
    )
    .await;
    let chunks = waiting["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0]["text"], "Compost goes in each autumn");
    let (b2, s2) = int8(&[0.0, 1.0, 0.0, 0.0]);
    let stored = ask(
        &mut ws,
        json!({ "type": "ix_vectors", "id": 7, "model": "nomic-embed-text",
                "items": [{ "chunk": chunks[0]["chunk"], "vec": b2, "scale": s2 }] }),
    )
    .await;
    assert_eq!(stored["stored"], 1);
    let waiting = ask(&mut ws, json!({ "type": "ix_pending", "id": 8 })).await;
    assert_eq!(waiting["chunks"], json!([]));

    // Found by words (the PC is not asked to embed: nobody answers within the test's patience
    // would also do, so the search falls back and says why).
    let search = |query: &str| {
        call(
            "memory_search",
            json!({ "query": query, "parseDates": false }),
        )
    };
    let (_, found) = tokio::join!(async {}, async {
        let pc = async {
            // The hub asks the PC to embed the query; answer with the first chunk's direction.
            let f = next_json(&mut ws).await;
            assert_eq!(f["type"], "ix_embed");
            assert_eq!(f["text"], "search_query: vegetable patches location");
            send(
                &mut ws,
                json!({ "type": "ix_embedded", "id": f["id"], "vec": [0.9, 0.0, 0.0, 0.1],
                "model": "nomic-embed-text:latest" }),
            )
            .await;
        };
        let (r, ()) = tokio::join!(mcp(&hub, INDEXER, search("vegetable patches location")), pc);
        r
    });
    let result = &found.1["result"]["structuredContent"];
    assert_eq!(result["searched"]["meaning"], "pc", "{result}");
    assert_eq!(result["mode"], "hybrid");
    let top = &result["results"][0];
    assert_eq!(
        (top["kind"].as_str(), top["title"].as_str()),
        (Some("note"), Some("Garden beds"))
    );
    assert_eq!(top["matched"], json!(["vector"]));
    let reference = top["ref"].as_str().unwrap().to_owned();
    assert!(
        reference.starts_with("d:") && reference.ends_with("#0"),
        "{reference}"
    );

    // A PC that does not answer in time leaves the search on keywords, with the reason.
    let (_, kw) = tokio::join!(async {}, mcp(&hub, INDEXER, search("compost")));
    let result = &kw.1["result"]["structuredContent"];
    assert!(
        result["searched"]["meaning"]
            .as_str()
            .unwrap()
            .starts_with("off:"),
        "{result}"
    );
    assert_eq!(result["results"][0]["ref"], reference.replace("#0", "#1"));
    // Drain the embed request that went unanswered.
    let _ = next_json(&mut ws).await;

    // Reading a hit gives its chunk and the ones beside it.
    let (_, read) = mcp(
        &hub,
        INDEXER,
        call("memory_read", json!({ "ref": reference })),
    )
    .await;
    let chunks = read["result"]["structuredContent"]["chunks"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0]["text"], "Raised beds hold the tomatoes");

    let gone = ask(&mut ws, json!({ "type": "ix_delete", "id": 9, "source": "pc:notes", "keys": ["garden/beds.md", "nope.md"] })).await;
    assert_eq!(gone["deleted"], 1);
    let page = ask(
        &mut ws,
        json!({ "type": "ix_manifest", "id": 10, "source": "pc:notes" }),
    )
    .await;
    assert_eq!(page["items"], json!([]));
}
