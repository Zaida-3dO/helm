//! HTTP routes: `/health` (open), `/mcp` (bearer token), `/relay` (Helm's line, a
//! `relay`-scope token) and `/telegram/update` (Telegram updates posted by another service
//! that already runs the bot, a `telegram`-scope token).

use crate::apptools;
use crate::auth::{Client, Scope, Tokens};
use crate::mcp::{self, Options};
use crate::relay::{Line, MAX_FRAME};
use crate::store::Store;
use crate::telegram::{self, Bridge};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct AppState {
    pub store: Mutex<Store>,
    pub tokens: Tokens,
    pub options: Options,
    pub line: Arc<Line>,
    /// The Telegram bot, or `None` when it is off.
    pub telegram: Option<Arc<Bridge>>,
}

/// The hub pings the line this often; a line silent for three intervals is dropped.
pub const PING_EVERY: Duration = Duration::from_secs(30);

/// Request bodies above this are refused (a memory body is capped at 1 MB).
pub const MAX_REQUEST: usize = 4 * 1024 * 1024;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/mcp", get(no_stream).post(post_mcp).delete(no_stream))
        .route("/relay", get(relay))
        .route("/telegram/update", post(telegram_update))
        .layer(DefaultBodyLimit::max(MAX_REQUEST))
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> Response {
    // Touch the database so "ok" means the store answers, not just the socket.
    let ok = state
        .store
        .lock()
        .map(|s| s.count().is_ok())
        .unwrap_or(false);
    let body = json!({
        "status": if ok { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
    });
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body)).into_response()
}

/// The hub sends no server-initiated messages, so there is no GET stream,
/// and sessions hold no state, so there is nothing to DELETE.
async fn no_stream() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(json!({ "error": "a valid bearer token is required" })),
    )
        .into_response()
}

fn refused(code: StatusCode, why: &str) -> Response {
    (code, Json(json!({ "error": why }))).into_response()
}

async fn post_mcp(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let Some(client) = state.tokens.check(auth).cloned() else {
        return unauthorized();
    };
    // Least privilege: Helm's line token opens the line and nothing else, and a forwarder's
    // token posts Telegram updates and nothing else.
    match client.scope {
        Scope::Relay => {
            return refused(
                StatusCode::FORBIDDEN,
                "a relay token cannot call the MCP surface",
            )
        }
        Scope::Telegram => {
            return refused(
                StatusCode::FORBIDDEN,
                "a telegram token cannot call the MCP surface",
            )
        }
        Scope::ReadOnly | Scope::ReadWrite => {}
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            let e = json!({ "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": "request body is not JSON" } });
            return (StatusCode::BAD_REQUEST, Json(e)).into_response();
        }
    };
    let started = Instant::now();
    let is_init = msg.get("method").and_then(Value::as_str) == Some("initialize");
    let label = describe(&msg);
    let reply = match &msg {
        // Older clients may batch; answer each message in order.
        Value::Array(batch) => {
            let mut replies = vec![];
            for m in batch {
                match answer(&state, &client, m).await {
                    Ok(Some(r)) => replies.push(r),
                    Ok(None) => {}
                    Err(code) => return code.into_response(),
                }
            }
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        m => match answer(&state, &client, m).await {
            Ok(r) => r,
            Err(code) => return code.into_response(),
        },
    };
    eprintln!(
        "{} {} {}ms",
        client.name,
        label,
        started.elapsed().as_millis()
    );
    let Some(reply) = reply else {
        return StatusCode::ACCEPTED.into_response();
    };
    let mut resp = Json(reply).into_response();
    if is_init {
        if let Ok(v) = HeaderValue::from_str(&uuid::Uuid::new_v4().to_string()) {
            resp.headers_mut().insert("mcp-session-id", v);
        }
    }
    resp
}

/// One JSON-RPC message: forwarded Helm tools go down the line, without holding the
/// store; everything else is the memory surface.
async fn answer(state: &AppState, client: &Client, m: &Value) -> Result<Option<Value>, StatusCode> {
    let method = m.get("method").and_then(Value::as_str);
    let tool = m.pointer("/params/name").and_then(Value::as_str);
    if client.app && method == Some("tools/call") {
        if let (Some(name), Some(id)) = (tool, m.get("id")) {
            if apptools::is_forwarded(name) {
                let args = m
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = apptools::call(&state.line, &client.name, name, &args).await;
                return Ok(Some(
                    json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                ));
            }
        }
    }
    let reply = {
        let Ok(mut store) = state.store.lock() else {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        };
        mcp::handle(&mut store, client, &state.options, m)
    };
    let mut reply = reply;
    if client.app && method == Some("tools/list") {
        if let Some(tools) = reply
            .as_mut()
            .and_then(|r| r.pointer_mut("/result/tools"))
            .and_then(Value::as_array_mut)
        {
            tools.extend(apptools::tool_list());
        }
    }
    Ok(reply)
}

/// `/relay`: Helm's one outbound line. Upgraded only for a `relay`-scope token.
async fn relay(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let Some(client) = state.tokens.check(auth).cloned() else {
        return unauthorized();
    };
    if client.scope != Scope::Relay {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "only a relay token may open the line" })),
        )
            .into_response();
    }
    let line = state.line.clone();
    ws.max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| run_line(socket, line, client.name))
}

async fn run_line(mut socket: WebSocket, line: Arc<Line>, client: String) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let generation = line.attach(tx);
    eprintln!("relay: line up ({client})");
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(frame) => {
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                // A newer line replaced this one.
                None => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(t))) => {
                    last_heard = Instant::now();
                    if !line.receive(generation, t.as_str()) {
                        eprintln!("relay: dropped a malformed frame");
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => last_heard = Instant::now(),
            },
            _ = ping.tick() => {
                if last_heard.elapsed() > PING_EVERY * 3 {
                    eprintln!("relay: line silent, dropping it");
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    line.detach(generation);
    eprintln!("relay: line down ({client})");
}

/// `/telegram/update`: for a bot that another service already runs (only one process may
/// consume a bot's updates), that service posts each Telegram Update it received, or an array
/// of them. They take the same path as polled ones: allow-list, private chats, commands, and
/// one handling per `update_id`.
async fn telegram_update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let Some(client) = state.tokens.check(auth) else {
        return unauthorized();
    };
    if client.scope != Scope::Telegram {
        return refused(
            StatusCode::FORBIDDEN,
            "only a telegram token may post updates",
        );
    }
    let Some(bridge) = state.telegram.clone() else {
        return refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "the Telegram bot is off on this hub",
        );
    };
    let updates = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Array(updates)) => updates,
        Ok(update @ Value::Object(_)) => vec![update],
        _ => {
            return refused(
                StatusCode::BAD_REQUEST,
                "the body must be a Telegram Update object or an array of them",
            )
        }
    };
    let mut handled = Vec::with_capacity(updates.len());
    for u in &updates {
        handled.push(bridge.on_update(u, telegram::now_ms()).await.as_str());
    }
    eprintln!("{} telegram/update {}", client.name, handled.join(","));
    Json(json!({ "ok": true, "handled": handled })).into_response()
}

/// A log label: the method, plus the tool name for tool calls. Never arguments.
fn describe(msg: &Value) -> String {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("batch");
    match msg.pointer("/params/name").and_then(Value::as_str) {
        Some(tool) if method == "tools/call" => format!("{method} {tool}"),
        _ => method.to_owned(),
    }
}
