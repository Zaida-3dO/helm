//! End-to-end over the HTTP router: auth, MCP handshake, and every tool.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use helm_hub::auth::Tokens;
use helm_hub::mcp::Options;
use helm_hub::server::{router, AppState};
use helm_hub::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const RW: &str = "rw-token-0123456789abcdefghij";
const RO: &str = "ro-token-0123456789abcdefghij";

fn app(allow_mutate: bool) -> axum::Router {
    let tokens = Tokens::parse(&format!("helm:{RW},remote:{RO}:ro")).unwrap();
    router(Arc::new(AppState {
        store: Mutex::new(Store::open_in_memory().unwrap()),
        tokens,
        options: Options { allow_mutate },
        line: Arc::new(helm_hub::relay::Line::new().0),
        telegram: None,
    }))
}

async fn post(app: &axum::Router, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = Request::post("/mcp").header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

async fn call(app: &axum::Router, token: &str, tool: &str, args: Value) -> Value {
    let (status, v) = post(
        app,
        Some(token),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": tool, "arguments": args } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    v["result"].clone()
}

fn tool_names(v: &Value) -> Vec<String> {
    v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn health_is_open_and_mcp_is_not() {
    let app = app(false);
    let resp = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let (status, _) = post(&app, None, list.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = post(&app, Some("not-a-real-token-at-all-xxxx"), list.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, v) = post(&app, Some(RW), list).await;
    assert_eq!(status, StatusCode::OK);
    assert!(tool_names(&v).contains(&"memory_search".to_owned()));
}

#[tokio::test]
async fn initialize_negotiates_and_notifications_get_202() {
    let app = app(false);
    let req = Request::post("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {RW}"))
        .body(Body::from(
            json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize",
                    "params": { "protocolVersion": "2025-03-26", "capabilities": {},
                                "clientInfo": { "name": "t", "version": "0" } } })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("mcp-session-id"));
    let v: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(v["result"]["serverInfo"]["name"], "helm-hub");

    let (status, body) = post(
        &app,
        Some(RW),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, Value::Null);

    let (_, v) = post(
        &app,
        Some(RW),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "nope" }),
    )
    .await;
    assert_eq!(v["error"]["code"], -32601);
}

#[tokio::test]
async fn write_search_read_list_roundtrip() {
    let app = app(false);
    let w = call(
        &app,
        RW,
        "memory_write",
        json!({ "title": "Backup sync rule", "body": "Use git reset --mixed on synced repos. See [[ops]].",
                "tags": ["Ops"], "vector": [1.0, 0.0, 0.0], "model": "m1" }),
    )
    .await;
    assert_eq!(w["isError"], false, "{w}");
    let id = w["structuredContent"]["id"].as_i64().unwrap();
    assert_eq!(w["structuredContent"]["has_vector"], true);
    call(
        &app,
        RW,
        "memory_write",
        json!({ "title": "Plex", "body": "media", "vector": [0.0, 1.0, 0.0], "model": "m1" }),
    )
    .await;

    // Keyword only (what a read-only caller gets).
    let s = call(&app, RO, "memory_search", json!({ "query": "backup" })).await;
    assert_eq!(s["structuredContent"]["mode"], "keyword");
    assert_eq!(s["structuredContent"]["results"][0]["id"], id);

    // Hybrid: the vector alone is enough to find it.
    let s = call(
        &app,
        RW,
        "memory_search",
        json!({ "query": "", "vector": [0.9, 0.1, 0.0] }),
    )
    .await;
    assert_eq!(s["structuredContent"]["mode"], "hybrid");
    assert_eq!(s["structuredContent"]["results"][0]["id"], id);

    let r = call(&app, RO, "memory_read", json!({ "id": id })).await;
    assert_eq!(r["structuredContent"]["title"], "Backup sync rule");
    assert_eq!(r["structuredContent"]["tags"], json!(["ops"]));
    assert_eq!(r["structuredContent"]["links"], json!(["ops"]));

    let l = call(&app, RO, "memory_list", json!({ "tag": "ops" })).await;
    assert_eq!(l["structuredContent"]["count"], 1);

    let missing = call(&app, RO, "memory_read", json!({ "id": 9999 })).await;
    assert_eq!(missing["isError"], true);
}

#[tokio::test]
async fn read_only_tokens_cannot_write() {
    let app = app(true);
    let (_, v) = post(
        &app,
        Some(RO),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )
    .await;
    assert_eq!(
        tool_names(&v),
        vec!["memory_search", "memory_read", "memory_list"]
    );
    let w = call(
        &app,
        RO,
        "memory_write",
        json!({ "title": "t", "body": "b" }),
    )
    .await;
    assert_eq!(w["isError"], true);
    let l = call(&app, RW, "memory_list", json!({})).await;
    assert_eq!(l["structuredContent"]["count"], 0);
}

#[tokio::test]
async fn update_and_delete_need_the_flag() {
    let off = app(false);
    let (_, v) = post(
        &off,
        Some(RW),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )
    .await;
    assert!(!tool_names(&v).contains(&"memory_delete".to_owned()));
    let id = call(
        &off,
        RW,
        "memory_write",
        json!({ "title": "t", "body": "b" }),
    )
    .await["structuredContent"]["id"]
        .as_i64()
        .unwrap();
    let d = call(&off, RW, "memory_delete", json!({ "id": id })).await;
    assert_eq!(d["isError"], true);
    assert_eq!(
        call(&off, RW, "memory_read", json!({ "id": id })).await["isError"],
        false
    );

    let on = app(true);
    let (_, v) = post(
        &on,
        Some(RW),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )
    .await;
    assert!(tool_names(&v).contains(&"memory_update".to_owned()));
    let id = call(
        &on,
        RW,
        "memory_write",
        json!({ "title": "t", "body": "old" }),
    )
    .await["structuredContent"]["id"]
        .as_i64()
        .unwrap();
    let u = call(
        &on,
        RW,
        "memory_update",
        json!({ "id": id, "body": "fresh" }),
    )
    .await;
    assert_eq!(u["isError"], false, "{u}");
    let s = call(&on, RW, "memory_search", json!({ "query": "fresh" })).await;
    assert_eq!(s["structuredContent"]["count"], 1);
    assert_eq!(
        call(&on, RW, "memory_delete", json!({ "id": id })).await["isError"],
        false
    );
    assert_eq!(
        call(&on, RW, "memory_read", json!({ "id": id })).await["isError"],
        true
    );
}

#[tokio::test]
async fn bad_input_is_a_tool_error_not_a_crash() {
    let app = app(false);
    let v = call(
        &app,
        RW,
        "memory_write",
        json!({ "title": "t", "body": "b", "vector": [1.0] }),
    )
    .await;
    assert_eq!(v["isError"], true, "a vector without a model is refused");
    let v = call(
        &app,
        RW,
        "memory_search",
        json!({ "query": "x", "vector": "nope" }),
    )
    .await;
    assert_eq!(v["isError"], true);
    let (status, _) = post(&app, Some(RW), json!("not an object")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let req = Request::post("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {RW}"))
        .body(Body::from("{not json"))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}
