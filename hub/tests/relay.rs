//! The relay line over a real socket: who may open it, forwarding through it, and the
//! offline answer when it is down (trust-boundary items b, d and f).

use futures_util::{SinkExt, StreamExt};
use helm_hub::auth::Tokens;
use helm_hub::mcp::Options;
use helm_hub::relay::{Line, OFFLINE};
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

struct Hub {
    addr: String,
    line: Arc<Line>,
}

async fn start() -> Hub {
    let tokens = Tokens::parse(&format!(
        "helm-line:{LINE}:relay, remote:{REMOTE}:ro+app, notes:{MEMORY_ONLY}:rw, \
         forwarder:{FORWARDER}:telegram"
    ))
    .unwrap();
    let (line, _upstream) = Line::new();
    let line = Arc::new(line);
    let app = router(Arc::new(AppState {
        store: Mutex::new(Store::open_in_memory().unwrap()),
        tokens,
        options: Options {
            allow_mutate: false,
        },
        line: line.clone(),
        telegram: None,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Hub { addr, line }
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
        if hub.line.is_up() == up {
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
    let mut ws = connect(&hub, LINE).await.expect("the relay token opens it");
    assert_eq!(next_json(&mut ws).await["type"], "welcome");
    // ...and neither the relay token nor a telegram token reaches the MCP surface.
    for token in [LINE, FORWARDER] {
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
        line: hub.line.clone(),
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
        line: hub.line.clone(),
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
        line: hub.line.clone(),
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
    let v = helm_hub::apptools::call(&line, "remote", "app_update_settings", &json!({})).await;
    assert_eq!(v["isError"], true);
    assert!(rx.try_recv().is_err(), "nothing went down the line");
    let v = helm_hub::apptools::call(&line, "remote", "app_list_chats", &json!([1])).await;
    assert_eq!(v["isError"], true, "arguments must be an object");
    assert!(rx.try_recv().is_err());
}
