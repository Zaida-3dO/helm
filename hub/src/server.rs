//! HTTP routes: `/health` (open), `/mcp` (bearer token), `/relay` (a machine's line, a token with
//! the `relay` scope), `/pair` (a one-time code) and `/telegram/update` (Telegram updates posted
//! by another service that already runs the bot, a `telegram`-scope token). The household
//! questions travel over the relay lines (`peer.rs`), not over HTTP.

use crate::apptools;
use crate::auth::{Client, Scope};
use crate::lines::Lines;
use crate::mcp::{self, Options};
use crate::registry::{Registry, RegistryError};
use crate::relay::{Line, MAX_FRAME};
use crate::store::Store;
use crate::telegram::{self, Telegram};
use axum::body::Bytes;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct AppState {
    pub store: Mutex<Store>,
    /// Shared with [`Lines`], which answers the household questions from it.
    pub registry: Arc<Mutex<Registry>>,
    pub options: Options,
    /// Every connected machine's line.
    pub lines: Arc<Lines>,
    /// The Telegram bot, or `None` when it is off.
    pub telegram: Option<Arc<Telegram>>,
    pub pair_limit: PairLimit,
}

impl AppState {
    pub fn new(
        store: Store,
        registry: Arc<Mutex<Registry>>,
        options: Options,
        lines: Arc<Lines>,
        telegram: Option<Arc<Telegram>>,
    ) -> Self {
        AppState {
            store: Mutex::new(store),
            registry,
            options,
            lines,
            telegram,
            pair_limit: PairLimit::default(),
        }
    }
}

/// The hub pings the line this often; a line silent for three intervals is dropped.
pub const PING_EVERY: Duration = Duration::from_secs(30);

/// An open line is checked against the registry this often, so a revoked machine is dropped
/// within seconds and without a restart.
pub const REVALIDATE_EVERY: Duration = Duration::from_secs(5);

/// Request bodies above this are refused (a memory body is capped at 1 MB).
pub const MAX_REQUEST: usize = 4 * 1024 * 1024;

/// A pairing request body is a short JSON object.
const MAX_PAIR_BODY: usize = 4096;

/// After this many wrong pairing codes in [`PAIR_WINDOW`], every attempt answers 429 until the
/// window clears. Codes have 40 bits and live 15 minutes; this keeps guessing far out of reach.
pub const PAIR_MAX_FAILURES: usize = 10;
pub const PAIR_WINDOW: Duration = Duration::from_secs(60);

/// Recent wrong pairing codes, across all callers.
#[derive(Default)]
pub struct PairLimit {
    failures: Mutex<Vec<Instant>>,
}

impl PairLimit {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Instant>> {
        self.failures.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn blocked(&self) -> bool {
        let mut f = self.lock();
        f.retain(|t| t.elapsed() < PAIR_WINDOW);
        f.len() >= PAIR_MAX_FAILURES
    }

    pub fn record_failure(&self) {
        self.lock().push(Instant::now());
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/mcp", get(no_stream).post(post_mcp).delete(no_stream))
        .route("/relay", get(relay))
        .route("/pair", post(pair))
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

/// The client a request's bearer token belongs to. Read from the registry on every request, so
/// a revoke is felt at once.
fn client_of(state: &AppState, headers: &HeaderMap) -> Option<Client> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let registry = state.registry.lock().ok()?;
    registry.authenticate(auth)
}

async fn post_mcp(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(client) = client_of(&state, &headers) else {
        return unauthorized();
    };
    // Least privilege: only a token with a memory or app scope reaches the MCP surface. Helm's
    // line token opens the line and nothing else, and a forwarder's token posts Telegram
    // updates and nothing else.
    if !client.scopes.can_read_memory() && !client.has(Scope::App) {
        let what = if client.has(Scope::Relay) {
            "a relay token"
        } else if client.has(Scope::Telegram) {
            "a telegram token"
        } else {
            "this token"
        };
        return refused(
            StatusCode::FORBIDDEN,
            &format!("{what} cannot call the MCP surface"),
        );
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

/// One JSON-RPC message: forwarded Helm tools go down one of the caller's own person's lines,
/// without holding the store; everything else is the memory surface.
async fn answer(state: &AppState, client: &Client, m: &Value) -> Result<Option<Value>, StatusCode> {
    let method = m.get("method").and_then(Value::as_str);
    let tool = m.pointer("/params/name").and_then(Value::as_str);
    if client.has(Scope::App) && method == Some("tools/call") {
        if let (Some(name), Some(id)) = (tool, m.get("id")) {
            if apptools::is_forwarded(name) {
                let mut args = m
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                // `machine` picks one of the caller's computers; Helm never sees it.
                let machine = args
                    .as_object_mut()
                    .and_then(|o| o.remove("machine"))
                    .and_then(|v| v.as_str().map(str::to_owned));
                let result = match state.lines.pick(client.person_id, machine.as_deref()) {
                    Some(line) => apptools::call(&line, &client.name, name, &args).await,
                    None => apptools::offline_result(),
                };
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
    if client.has(Scope::App) && method == Some("tools/list") {
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

/// `/relay`: a machine's one outbound line. Upgraded only for a token with the `relay` scope.
async fn relay(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(client) = client_of(&state, &headers) else {
        return unauthorized();
    };
    if !client.has(Scope::Relay) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "only a token with the relay scope may open the line" })),
        )
            .into_response();
    }
    let line = state.lines.line_for(&client);
    ws.max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| run_line(socket, state, line, client))
}

async fn run_line(mut socket: WebSocket, state: Arc<AppState>, line: Arc<Line>, client: Client) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let generation = line.attach_as(
        tx,
        json!({
            "personId": client.person_id,
            "person": client.person,
            "machineId": client.id,
            "machine": client.machine,
        }),
    );
    if let Ok(r) = state.registry.lock() {
        r.touch(client.id);
    }
    eprintln!("relay: line up ({} / {})", client.person, client.machine);
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut revalidate = tokio::time::interval(REVALIDATE_EVERY);
    revalidate.tick().await;
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(frame) => {
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                // A newer line of this client replaced this one.
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
            _ = revalidate.tick() => {
                let active = state
                    .registry
                    .lock()
                    .map(|r| r.is_active(client.id))
                    .unwrap_or(false);
                if !active {
                    eprintln!("relay: {} / {} was revoked, dropping its line", client.person, client.machine);
                    break;
                }
            }
        }
    }
    line.detach(generation);
    if let Ok(r) = state.registry.lock() {
        r.touch(client.id);
    }
    eprintln!("relay: line down ({} / {})", client.person, client.machine);
}

/// Hosts a pairing request may come from: this machine, the home network, Tailscale. The hub
/// is not meant to face the internet; this keeps a mistaken port-forward from offering codes.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                // 100.64.0.0/10: Tailscale and other carrier-grade NAT.
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                // fc00::/7 (Tailscale's fd7a:... included) and fe80::/10.
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| is_private(IpAddr::V4(v4)))
        }
    }
}

/// `POST /pair {"code": "ABCD-EFGH"}`: spend a one-time code and get this machine's token, once.
async fn pair(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    if let Some(ConnectInfo(addr)) = parts.extensions.get::<ConnectInfo<SocketAddr>>() {
        if !is_private(addr.ip()) {
            return refused(
                StatusCode::FORBIDDEN,
                "pairing is only offered to the home network",
            );
        }
    }
    if state.pair_limit.blocked() {
        return refused(
            StatusCode::TOO_MANY_REQUESTS,
            "too many wrong codes; wait a minute and try again",
        );
    }
    let Ok(bytes) = axum::body::to_bytes(body, MAX_PAIR_BODY).await else {
        return refused(StatusCode::PAYLOAD_TOO_LARGE, "the request is too large");
    };
    let code = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| v.get("code").and_then(Value::as_str).map(str::to_owned));
    let Some(code) = code else {
        return refused(
            StatusCode::BAD_REQUEST,
            "the body must be {\"code\": \"...\"}",
        );
    };
    let redeemed = match state.registry.lock() {
        Ok(mut r) => r.redeem_pairing(&code, telegram::now_ms()),
        Err(_) => {
            return refused(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the registry is unavailable",
            )
        }
    };
    match redeemed {
        Ok(p) => {
            eprintln!("pair: {} / {} paired", p.client.person, p.client.machine);
            let scopes: Vec<&str> = p.client.scopes.iter().map(Scope::label).collect();
            Json(json!({
                "token": p.token,
                "personId": p.client.person_id,
                "personName": p.client.person,
                "machineId": p.client.id,
                "machineName": p.client.machine,
                "scopes": scopes,
            }))
            .into_response()
        }
        Err(RegistryError::BadCode) => {
            state.pair_limit.record_failure();
            refused(StatusCode::FORBIDDEN, &RegistryError::BadCode.to_string())
        }
        Err(RegistryError::Invalid(why)) => refused(StatusCode::CONFLICT, &why),
        Err(e) => refused(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
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
    let Some(client) = client_of(&state, &headers) else {
        return unauthorized();
    };
    if !client.has(Scope::Telegram) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_is_only_offered_to_private_addresses() {
        for (ip, private) in [
            ("127.0.0.1", true),
            ("192.168.1.20", true),
            ("10.0.0.5", true),
            ("172.17.0.1", true),
            ("100.101.102.103", true),
            ("169.254.1.1", true),
            ("::1", true),
            ("fd7a:115c:a1e0::1", true),
            ("fe80::1", true),
            ("::ffff:192.168.1.5", true),
            ("8.8.8.8", false),
            ("100.128.0.1", false),
            ("2001:db8::1", false),
            ("::ffff:8.8.8.8", false),
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert_eq!(is_private(ip), private, "{ip}");
        }
    }

    #[test]
    fn wrong_codes_run_into_a_limit() {
        let limit = PairLimit::default();
        for _ in 0..PAIR_MAX_FAILURES - 1 {
            limit.record_failure();
        }
        assert!(!limit.blocked());
        limit.record_failure();
        assert!(limit.blocked());
    }
}
