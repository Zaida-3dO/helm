//! HTTP routes: `/health` (open) and `/mcp` (bearer token).

use crate::auth::Tokens;
use crate::mcp::{self, Options};
use crate::store::Store;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct AppState {
    pub store: Mutex<Store>,
    pub tokens: Tokens,
    pub options: Options,
}

/// Request bodies above this are refused (a memory body is capped at 1 MB).
pub const MAX_REQUEST: usize = 4 * 1024 * 1024;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/mcp", get(no_stream).post(post_mcp).delete(no_stream))
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

async fn post_mcp(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let Some(client) = state.tokens.check(auth).cloned() else {
        return unauthorized();
    };
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
    let reply = {
        let Ok(mut store) = state.store.lock() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        match &msg {
            // Older clients may batch; answer each message in order.
            Value::Array(batch) => {
                let replies: Vec<Value> = batch
                    .iter()
                    .filter_map(|m| mcp::handle(&mut store, &client, &state.options, m))
                    .collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            m => mcp::handle(&mut store, &client, &state.options, m),
        }
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

/// A log label: the method, plus the tool name for tool calls. Never arguments.
fn describe(msg: &Value) -> String {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("batch");
    match msg.pointer("/params/name").and_then(Value::as_str) {
        Some(tool) if method == "tools/call" => format!("{method} {tool}"),
        _ => method.to_owned(),
    }
}
