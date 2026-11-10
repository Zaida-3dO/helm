//! The phone's side of the voice line: the HTTP routes and the live socket.
//!
//! `POST /voice/turn`, `POST /voice/heard`, `GET /voice/status`, `POST /voice/fresh` and the
//! WebSocket `GET /voice/live`. All of them need a token with the `voice` scope and speak for that
//! token's person. The state and the rules live in `voice.rs` ([`VoiceBridge`]); this file reads
//! requests, answers with the statuses the protocol names, and runs the socket loop (the hello,
//! the pings and the silence timeout). The protocol is section 4 of the voice build plan.

use crate::auth::{Client, Scope};
use crate::server::{client_of, refused, unauthorized, AppState, REVALIDATE_EVERY};
use crate::telegram::now_ms;
use crate::voice::{
    self, Begin, Hello, Mode, NewTurn, Out, PhoneFrame, Reject, Report, VoiceBridge,
};
use axum::body::{Body, HttpBody};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A recording: the hub's global 4 MiB limit is lifted for this route only.
pub const MAX_TURN_BODY: usize = 8 * 1024 * 1024;
/// A typed turn is at most 8000 characters; 4 bytes each plus the JSON around them fits.
pub const MAX_TYPED_BODY: usize = 64 * 1024;
/// A batch of heard reports.
pub const MAX_HEARD_BODY: usize = 64 * 1024;
/// A body over its limit is still read this far past it and thrown away, so the client finishes
/// sending and gets its answer instead of a reset connection.
const DRAIN_EXTRA: usize = 16 * 1024 * 1024;
/// How long a new socket may take to send its `hello`.
pub const HELLO_WAIT: Duration = Duration::from_secs(10);
/// The hub pings the phone this often. In production a reverse proxy closes a WebSocket that is
/// idle for 60 s, so this stays well under that (the protocol's 30 s was too slow for it).
pub const LIVE_PING_EVERY: Duration = Duration::from_secs(20);
/// A socket that sends nothing (not even a pong) for this long is dropped.
pub const LIVE_SILENCE: Duration = Duration::from_secs(90);

/// The routes under `/voice`. They are not behind the hub's 4 MiB limit: each reads its own body
/// to its own limit.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/voice/turn", post(post_turn))
        .route("/voice/heard", post(post_heard))
        .route("/voice/status", get(get_status))
        .route("/voice/fresh", post(post_fresh))
        .route("/voice/live", get(live))
}

/// Who is calling and the voice line they speak to, or the answer to refuse them with: 401 for no
/// or a bad token, 403 for a token of another scope, 404 when the voice line is switched off.
fn gate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Client, Arc<VoiceBridge>), Box<Response>> {
    let Some(client) = client_of(state, headers) else {
        return Err(Box::new(unauthorized()));
    };
    if !client.has(Scope::Voice) {
        return Err(Box::new(refused(
            StatusCode::FORBIDDEN,
            "only a voice token may use /voice",
        )));
    }
    match state.voice.clone() {
        Some(bridge) => Ok((client, bridge)),
        None => Err(Box::new(refused(
            StatusCode::NOT_FOUND,
            "The voice line is switched off on this hub.",
        ))),
    }
}

enum ReadError {
    TooBig,
    Broken,
}

/// The body, up to `max` bytes. See [`DRAIN_EXTRA`] for what happens to one that is longer.
async fn read_capped(body: Body, max: usize) -> Result<Vec<u8>, ReadError> {
    let mut body = std::pin::pin!(body);
    let mut out: Vec<u8> = Vec::new();
    let mut total = 0usize;
    while let Some(frame) = std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)).await {
        let Ok(frame) = frame else {
            return Err(ReadError::Broken);
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        total += data.len();
        if total <= max {
            out.extend_from_slice(&data);
        } else if total > max.saturating_add(DRAIN_EXTRA) {
            return Err(ReadError::TooBig);
        } else {
            out = Vec::new();
        }
    }
    if total > max {
        Err(ReadError::TooBig)
    } else {
        Ok(out)
    }
}

/// The value of `key` in a query string. Ids and modes need no percent-decoding.
fn query_value<'a>(query: Option<&'a str>, key: &str) -> Option<&'a str> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

fn bad_request(why: &str) -> Response {
    refused(StatusCode::BAD_REQUEST, why)
}

/// A recording over the body limit, or over 130 seconds.
fn too_long() -> Response {
    refused(StatusCode::PAYLOAD_TOO_LARGE, "That recording is too long.")
}

/// `POST /voice/turn?turn=<id>&mode=conversation|chat[&continues=<id>]`: a new turn, as audio
/// (`audio/ogg` or `audio/wav`) or as typed text (`application/json`, `{"text": "..."}`).
async fn post_turn(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let (client, bridge) = match gate(&state, &parts.headers) {
        Ok(x) => x,
        Err(refusal) => return *refusal,
    };
    let query = parts.uri.query();
    let Some(turn) = query_value(query, "turn").filter(|t| voice::valid_id(t)) else {
        return bad_request("That turn id isn't valid.");
    };
    let Some(mode) = query_value(query, "mode").and_then(Mode::parse) else {
        return bad_request("The mode must be conversation or chat.");
    };
    let continues = query_value(query, "continues")
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    // `Some(type)` for a recording, `None` for a typed turn.
    let audio_type = match content_type.as_str() {
        "audio/ogg" => Some("audio/ogg"),
        "audio/wav" => Some("audio/wav"),
        "application/json" => None,
        _ => {
            return refused(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Send audio/ogg, audio/wav or JSON.",
            )
        }
    };
    if audio_type.is_some() && !bridge.has_stt() {
        return refused(StatusCode::SERVICE_UNAVAILABLE, voice::STT_OFF);
    }

    let limit = if audio_type.is_some() {
        MAX_TURN_BODY
    } else {
        MAX_TYPED_BODY
    };
    let bytes = match read_capped(body, limit).await {
        Ok(b) => b,
        Err(ReadError::TooBig) if audio_type.is_some() => return too_long(),
        Err(ReadError::TooBig) => return bad_request("That message is too long."),
        Err(ReadError::Broken) => return bad_request("The upload was cut off."),
    };

    let (text, audio) = if audio_type.is_some() {
        if bytes.is_empty() {
            return bad_request("That recording is empty.");
        }
        if voice::audio_seconds(&bytes).is_some_and(|s| s > voice::MAX_AUDIO_SECONDS) {
            return too_long();
        }
        (None, Some(bytes))
    } else {
        let typed = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("text").and_then(Value::as_str).map(str::to_owned));
        let Some(typed) = typed else {
            return bad_request("Send {\"text\": \"...\"}.");
        };
        (Some(typed), None)
    };
    let empty_text = text.as_deref().is_some_and(|t| t.trim().is_empty());

    let new = NewTurn {
        id: turn.to_owned(),
        mode,
        continues,
        text,
    };
    match bridge.begin_turn(client.person_id, new, now_ms()) {
        Begin::New { helm_online } => {
            if let (Some(audio), Some(mime)) = (audio, audio_type) {
                let (bridge, person, id) = (bridge.clone(), client.person_id, turn.to_owned());
                tokio::spawn(async move {
                    bridge.transcribe_turn(person, &id, audio, mime).await;
                });
            }
            eprintln!(
                "voice: {} sent a {} turn ({})",
                client.name,
                if audio_type.is_some() {
                    "spoken"
                } else {
                    "typed"
                },
                mode.as_str()
            );
            (
                StatusCode::ACCEPTED,
                Json(json!({
                    "ok": true,
                    "turn": turn,
                    "already": false,
                    "helm": if helm_online { "online" } else { "offline" },
                    "queued": true,
                })),
            )
                .into_response()
        }
        Begin::Already(now_state) => Json(json!({
            "ok": true,
            "turn": turn,
            "already": true,
            "state": now_state,
        }))
        .into_response(),
        Begin::Rejected(Reject::BadId) => bad_request("That turn id isn't valid."),
        Begin::Rejected(Reject::BadText) if empty_text => bad_request("There is nothing to send."),
        Begin::Rejected(Reject::BadText) => bad_request("That message is too long."),
        Begin::Rejected(Reject::UnknownContinues) => {
            bad_request("The turn this one carries on isn't known.")
        }
        Begin::Rejected(Reject::QueueFull) => refused(
            StatusCode::TOO_MANY_REQUESTS,
            "Helm is offline and the queue is full, so this wasn't kept.",
        ),
    }
}

/// `POST /voice/heard {"reports": [...]}`: what the phone did and didn't play.
async fn post_heard(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let (client, bridge) = match gate(&state, &parts.headers) {
        Ok(x) => x,
        Err(refusal) => return *refusal,
    };
    let bytes = match read_capped(body, MAX_HEARD_BODY).await {
        Ok(b) => b,
        Err(ReadError::TooBig) => {
            return refused(StatusCode::PAYLOAD_TOO_LARGE, "That report is too big.")
        }
        Err(ReadError::Broken) => return bad_request("The upload was cut off."),
    };
    let now = now_ms();
    let reports = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| Report::parse_all(&v, now));
    let Some(reports) = reports else {
        return bad_request("Send {\"reports\": [...]} with 1 to 50 reports.");
    };
    let forwarded = bridge.record_heard(client.person_id, reports, now);
    Json(json!({ "ok": true, "forwarded": forwarded })).into_response()
}

/// `GET /voice/status`: proves the address and the token work. Never touches speech-to-text.
async fn get_status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let (client, bridge) = match gate(&state, &headers) {
        Ok(x) => x,
        Err(refusal) => return *refusal,
    };
    let status = bridge.status(client.person_id);
    Json(json!({
        "ok": true,
        "helm": if status.helm_online { "online" } else { "offline" },
        "version": voice::VERSION,
        "stt": status.stt,
    }))
    .into_response()
}

/// `POST /voice/fresh`: start a new conversation in Helm's voice chat. Not queued: with no line
/// up it answers 409.
async fn post_fresh(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let (client, bridge) = match gate(&state, &headers) {
        Ok(x) => x,
        Err(refusal) => return *refusal,
    };
    if bridge.fresh(client.person_id) {
        (StatusCode::ACCEPTED, Json(json!({ "ok": true }))).into_response()
    } else {
        refused(StatusCode::CONFLICT, "Helm is offline.")
    }
}

/// `GET /voice/live`: the WebSocket the phone's replies and turn states arrive on.
async fn live(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let (client, bridge) = match gate(&state, &headers) {
        Ok(x) => x,
        Err(refusal) => return *refusal,
    };
    ws.max_message_size(voice::MAX_PHONE_FRAME)
        .max_frame_size(voice::MAX_PHONE_FRAME)
        .on_upgrade(move |socket| run_live(socket, state, bridge, client))
}

/// The first frame must be a well-formed `hello`.
async fn first_hello(socket: &mut WebSocket) -> Option<Hello> {
    loop {
        match socket.recv().await? {
            Ok(Message::Text(t)) => {
                return match PhoneFrame::parse(t.as_str()) {
                    Some(PhoneFrame::Hello(h)) => Some(h),
                    _ => None,
                }
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            _ => return None,
        }
    }
}

async fn close_with(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

/// One phone's socket. The hub pings every 20 s and drops a socket that has been silent for 90 s;
/// a revoked token drops it within seconds. A newer socket of the same token replaces it (the
/// bridge asks it to close with code 4000).
async fn run_live(
    mut socket: WebSocket,
    state: Arc<AppState>,
    bridge: Arc<VoiceBridge>,
    client: Client,
) {
    let hello = match tokio::time::timeout(HELLO_WAIT, first_hello(&mut socket)).await {
        Ok(Some(h)) => h,
        _ => {
            close_with(&mut socket, 1008, "send hello first").await;
            return;
        }
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
    let (person, phone) = (client.person_id, client.id);
    let gen = bridge.attach_live(person, phone, &hello, tx, now_ms());
    if let Ok(r) = state.registry.lock() {
        r.touch(client.id);
    }
    eprintln!(
        "voice: {} is live ({})",
        client.name,
        if hello.live { "talk screen" } else { "sync" }
    );
    let mut ping = tokio::time::interval(LIVE_PING_EVERY);
    ping.tick().await;
    let mut revalidate = tokio::time::interval(REVALIDATE_EVERY);
    revalidate.tick().await;
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(Out::Frame(frame)) => {
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                Some(Out::Close(code, reason)) => {
                    close_with(&mut socket, code, reason).await;
                    break;
                }
                None => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(t))) => {
                    last_heard = Instant::now();
                    let now = now_ms();
                    match PhoneFrame::parse(t.as_str()) {
                        Some(PhoneFrame::Ack(seq)) => bridge.live_ack(person, phone, seq, now),
                        Some(PhoneFrame::Presence { live, mode }) => {
                            bridge.live_presence(person, phone, gen, live, mode, now);
                        }
                        Some(PhoneFrame::Cancel(turn)) => {
                            bridge.cancel(person, &turn, now);
                        }
                        // Only the first frame is a hello, and unknown frames are dropped.
                        Some(PhoneFrame::Hello(_)) | None => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => last_heard = Instant::now(),
            },
            _ = ping.tick() => {
                if last_heard.elapsed() > LIVE_SILENCE {
                    eprintln!("voice: {} was silent, dropping its socket", client.name);
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
                    eprintln!("voice: {} was revoked, dropping its socket", client.name);
                    break;
                }
            }
        }
    }
    bridge.detach_live(person, phone, gen, now_ms());
    if let Ok(r) = state.registry.lock() {
        r.touch(client.id);
    }
}
