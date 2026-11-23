//! The four HTTP routes a room speaker calls: `/speaker/status`, `/speaker/stt`,
//! `/speaker/turn` and `/speaker/turn/<id>`.
//!
//! They are the speaker's whole view of the hub, and the hub's whole view of a speaker: one
//! bearer token per speaker, of the `speaker` scope, which stands alone — it opens these routes
//! and nothing else, and no other token opens them. Everything they do goes to
//! [`crate::speaker::SpeakerBridge`]; this module only turns a call into a bridge call and the
//! answer back into JSON.
//!
//! The timing a caller is promised: a turn call waits `wait` seconds (clamped to 0..25) for an
//! answer and otherwise answers `202 {state: "working"}`, which the caller follows up with
//! `GET /speaker/turn/<id>`; an id the hub does not know is a 404, and never a resend. Bodies
//! are read to their own caps, like the phone's voice routes, so the hub's 4 MiB default does
//! not apply here. Nothing a speaker sends is ever logged beyond an id.

use crate::auth::Scope;
use crate::server::{client_of, refused, unauthorized, AppState};
use crate::speaker::{SpeakerBridge, SpeakerTurn, SttOutcome, TurnOutcome, VERSION};
use crate::voice::valid_id;
use crate::voice_http::{query_value, read_capped, ReadError};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A turn call's JSON body is small: the words plus a room snapshot.
pub const MAX_TURN_BODY: usize = 32 * 1024;
/// How long a turn call or poll may hold the connection open, and the longest wait asked for.
pub const MAX_WAIT_SECS: u64 = 25;
pub const DEFAULT_WAIT_SECS: u64 = 20;
/// How often a waiting call looks at the bridge.
const POLL: Duration = Duration::from_millis(250);

/// The routes under `/speaker`. Like `/voice` they are not behind the hub's 4 MiB body limit:
/// each reads its own body to its own cap.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/speaker/status", get(get_status))
        .route("/speaker/stt", post(post_stt))
        .route("/speaker/turn", post(post_turn))
        .route("/speaker/turn/{id}", get(get_turn))
}

/// Who is calling and the speaker line they speak to, or the answer to refuse them with: 401
/// for no or a bad token, 403 for a token of another scope, 404 when the speaker line is
/// switched off.
fn gate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(i64, i64, Arc<SpeakerBridge>), Box<Response>> {
    let Some(client) = client_of(state, headers) else {
        return Err(Box::new(unauthorized()));
    };
    if !client.has(Scope::Speaker) {
        return Err(Box::new(refused(
            StatusCode::FORBIDDEN,
            "only a speaker token may use /speaker",
        )));
    }
    match state.speaker.clone() {
        Some(bridge) => Ok((client.id, client.person_id, bridge)),
        None => Err(Box::new(refused(
            StatusCode::NOT_FOUND,
            "The speaker line is switched off on this hub.",
        ))),
    }
}

fn answer(body: Value) -> Response {
    (StatusCode::OK, Json(body)).into_response()
}

/// `GET /speaker/status`: whether a Helm takes this speaker's turns, whether speech can be
/// heard at all, and the talk in progress, if any.
async fn get_status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let (token, person, bridge) = match gate(&state, &headers) {
        Ok(g) => g,
        Err(r) => return *r,
    };
    let out = bridge.status(token, person);
    answer(json!({
        "ok": true,
        "version": VERSION,
        "helm": out.helm,
        "stt": out.stt,
        "session": out.session,
    }))
}

/// `POST /speaker/stt`: one wav capture in, what was heard out. A clip shorter than the
/// bridge's minimum, or one heard as blank, comes back `empty`; a clip too long is refused
/// before the server is asked, and no speech-to-text server at all is `offline`.
async fn post_stt(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Body,
) -> Response {
    let (token, person, bridge) = match gate(&state, &headers) {
        Ok(g) => g,
        Err(r) => return *r,
    };
    let _ = query; // a device and room named here are recorded with the turn that follows
    let audio = match read_capped(body, crate::speaker::MAX_STT_BODY).await {
        Ok(a) => a,
        Err(ReadError::TooBig) => {
            return refused(
                StatusCode::PAYLOAD_TOO_LARGE,
                "the recording is longer than the hub takes",
            )
        }
        Err(ReadError::Broken) => {
            return refused(StatusCode::BAD_REQUEST, "the upload was cut off")
        }
    };
    match bridge.stt_call(token, person, audio).await {
        SttOutcome::Text(text) => answer(json!({ "text": text })),
        SttOutcome::Empty => answer(json!({ "state": "empty" })),
        SttOutcome::Failed => answer(json!({ "state": "error" })),
        SttOutcome::NotConfigured => answer(json!({ "state": "offline" })),
        SttOutcome::TooLong => refused(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the recording is longer than the hub takes",
        ),
        SttOutcome::RateLimited => refused(
            StatusCode::TOO_MANY_REQUESTS,
            "this speaker is asking too often",
        ),
    }
}

/// What one turn's call carries, once checked. `hint` is kept for the speaker's own context
/// and unread for now; `presence` is a room snapshot recorded in the registry, never routed on.
struct TurnBody {
    wait: u64,
    speaker: SpeakerTurn,
}

/// Read and check a turn body. A body that is not a JSON object, or without a usable id or
/// text, is a 400.
fn turn_body(raw: &[u8]) -> Result<TurnBody, Box<Response>> {
    let Ok(value) = serde_json::from_slice::<Value>(raw) else {
        return Err(Box::new(refused(
            StatusCode::BAD_REQUEST,
            "the turn is not a JSON object",
        )));
    };
    let field = |k: &str| value.get(k).and_then(Value::as_str).map(|s| s.to_owned());
    let small = |k: &str| {
        field(k)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(120).collect())
    };
    let Some(turn_id) = field("turn_id") else {
        return Err(Box::new(refused(
            StatusCode::BAD_REQUEST,
            "the turn needs a turn_id",
        )));
    };
    if !valid_id(&turn_id) {
        return Err(Box::new(refused(
            StatusCode::BAD_REQUEST,
            "the turn_id is not usable",
        )));
    }
    let Some(text) = field("text") else {
        return Err(Box::new(refused(
            StatusCode::BAD_REQUEST,
            "the turn needs text",
        )));
    };
    let wait = value
        .get("wait")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(DEFAULT_WAIT_SECS)
        .min(MAX_WAIT_SECS);
    Ok(TurnBody {
        wait,
        speaker: SpeakerTurn {
            id: turn_id,
            text,
            device: small("device"),
            room: small("room"),
            area: small("area"),
            presence: value.get("presence").cloned().filter(Value::is_object),
        },
    })
}

/// `POST /speaker/turn`: the words go into the token's queue. The call waits up to `wait`
/// seconds for the answer; past that it answers 202 and the caller polls.
async fn post_turn(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let (token, person, bridge) = match gate(&state, &headers) {
        Ok(g) => g,
        Err(r) => return *r,
    };
    let raw = match read_capped(body, MAX_TURN_BODY).await {
        Ok(b) => b,
        Err(ReadError::TooBig) => {
            return refused(StatusCode::PAYLOAD_TOO_LARGE, "the turn is too big")
        }
        Err(ReadError::Broken) => {
            return refused(StatusCode::BAD_REQUEST, "the upload was cut off")
        }
    };
    let parsed = match turn_body(&raw) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let turn_id = parsed.speaker.id.clone();
    match bridge.begin_turn(token, person, parsed.speaker, started) {
        TurnOutcome::Conflict => refused(
            StatusCode::CONFLICT,
            "that turn id belongs to another speaker",
        ),
        TurnOutcome::Rejected => refused(
            StatusCode::TOO_MANY_REQUESTS,
            "this speaker has more turns than the hub takes",
        ),
        TurnOutcome::Invalid => {
            refused(StatusCode::BAD_REQUEST, "the turn id or text is not usable")
        }
        other => wait_for(bridge, token, &turn_id, other, parsed.wait).await,
    }
}

/// `GET /speaker/turn/<id>?wait=N`: the same answer, for a caller following a 202. An id the
/// hub does not know is 404; a hub that forgot a turn never runs it again.
async fn get_turn(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let (token, _person, bridge) = match gate(&state, &headers) {
        Ok(g) => g,
        Err(r) => return *r,
    };
    if !valid_id(&id) {
        return refused(StatusCode::NOT_FOUND, "no such turn");
    }
    let wait = query_value(query.as_deref(), "wait")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_WAIT_SECS)
        .min(MAX_WAIT_SECS);
    let known = bridge.outcome(token, &id);
    if matches!(known, TurnOutcome::Unknown) {
        return refused(StatusCode::NOT_FOUND, "no such turn");
    }
    wait_for(bridge, token, &id, known, wait).await
}

/// Hold the call open while the turn is working, up to `wait` seconds, and answer what the
/// bridge says. The bridge's own clocks (resend, abandon, the wait for a line) keep running
/// while this waits; a caller that gives up simply stops polling.
async fn wait_for(
    bridge: Arc<SpeakerBridge>,
    token: i64,
    id: &str,
    first: TurnOutcome,
    wait: u64,
) -> Response {
    let mut outcome = first;
    let until = Instant::now() + Duration::from_secs(wait);
    while matches!(outcome, TurnOutcome::Working) && Instant::now() < until {
        tokio::time::sleep(POLL).await;
        outcome = bridge.outcome(token, id);
    }
    match outcome {
        TurnOutcome::Answer { text, session } => answer(json!({
            "state": "answer",
            "text": text,
            "session": session,
        })),
        TurnOutcome::Working => {
            (StatusCode::ACCEPTED, Json(json!({ "state": "working" }))).into_response()
        }
        TurnOutcome::Offline => answer(json!({ "state": "offline" })),
        // The speaker side has its own fixed sentence for an error; the hub adds no words.
        TurnOutcome::Error => answer(json!({ "state": "error" })),
        TurnOutcome::Rejected => refused(
            StatusCode::TOO_MANY_REQUESTS,
            "this speaker has more turns than the hub takes",
        ),
        TurnOutcome::Invalid => {
            refused(StatusCode::BAD_REQUEST, "the turn id or text is not usable")
        }
        TurnOutcome::Conflict => refused(StatusCode::CONFLICT, "that turn id is in use"),
        TurnOutcome::Unknown => refused(StatusCode::NOT_FOUND, "no such turn"),
    }
}
