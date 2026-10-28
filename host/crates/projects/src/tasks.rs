//! The Tasks view's board reads.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `board_read` | async | [`ReadArgs`] → `{action, data}` |
//! | `board_cancel` | blocking | `{requestId}` → `{cancelled}` |
//!
//! Every action is one GET on the board through `helm_standup` (the client has no way to write).
//! The renderer limits itself to four reads at a time and drops late answers; here each read has
//! a deadline of 100 ms to 15 s (15 s unless the caller asks for less) and can be cancelled by
//! its `requestId`. A cancelled or timed-out read stops being waited on at once; its HTTP call
//! ends on its own within the client's own 15 s limit.
//!
//! `attachOnly` reads use the connection already held (set at launch from the saved address, or
//! by a sync or Save); they never read the credential store, so typing in search cannot be the
//! thing that first reaches for the board. Without a saved connection they answer `not_started`.
//!
//! Error codes the UI shows by name: `not_configured`, `not_started`, `token_rejected`,
//! `unreachable`, `cancelled`, `timed_out`, `board_error`.

use crate::board::{ctx_credentials, resolve};
use crate::ops::{parse_args, to_value};
use helm_core::db::Database;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use helm_standup::{BoardLink, Client, StandupError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

pub const MIN_TIMEOUT_MS: u64 = 100;
pub const MAX_TIMEOUT_MS: u64 = 15_000;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.async_op("board_read", read)
        .blocking("board_cancel", OpClass::Blocking, cancel);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadArgs {
    /// `projects`, `board_page`, `item`, `detail`, `history`, `artifacts`, `loops`, `loop`,
    /// `fleet`, `ready` or `search`.
    pub action: String,
    pub request_id: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub attach_only: bool,
    #[serde(default)]
    pub column: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub loop_id: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub include_closed: bool,
    #[serde(default)]
    pub history_limit: Option<usize>,
    #[serde(default)]
    pub artifact_limit: Option<usize>,
}

fn need<'a>(v: &'a Option<String>, what: &str) -> Result<&'a str, StandupError> {
    v.as_deref()
        .ok_or_else(|| StandupError::Invalid(format!("{what} is missing")))
}

/// One read against `client`. The board's answer is passed through as JSON.
pub fn run(client: &Client, a: &ReadArgs) -> Result<Value, StandupError> {
    let limit = a.limit.unwrap_or(30);
    let cursor = a.cursor.as_deref();
    match a.action.as_str() {
        "projects" => {
            let (projects, complete) = client.list_projects()?;
            Ok(json!({ "projects": projects, "complete": complete }))
        }
        "board_page" => client.board_page(
            need(&a.column, "the column")?,
            a.project.as_deref(),
            limit,
            cursor,
        ),
        "item" => client.item(need(&a.id, "the item id")?),
        "detail" => client.item_detail(
            need(&a.id, "the item id")?,
            a.history_limit.unwrap_or(20),
            a.artifact_limit.unwrap_or(20),
        ),
        "history" => client.history(need(&a.id, "the item id")?, limit, cursor),
        "artifacts" => client.artifacts(need(&a.id, "the item id")?, limit, cursor),
        "loops" => client.loops(need(&a.id, "the item id")?, limit, cursor, a.include_closed),
        "loop" => client.loop_text(
            need(&a.id, "the item id")?,
            need(&a.loop_id, "the loop id")?,
        ),
        "fleet" => {
            let f = client.fleet()?;
            Ok(json!({ "assignments": f.assignments, "complete": f.complete }))
        }
        "ready" => client.ready(),
        "search" => client.search_items(need(&a.query, "the search text")?, limit),
        other => Err(StandupError::Invalid(format!("unknown read {other:?}"))),
    }
}

fn api_error(e: &StandupError) -> ApiError {
    let code = match e {
        StandupError::NotConfigured => "not_configured",
        StandupError::NotStarted => "not_started",
        StandupError::Unauthorized => "token_rejected",
        StandupError::Unreachable(_) => "unreachable",
        StandupError::Invalid(_) => "bad_args",
        _ => "board_error",
    };
    ApiError::new(code, e.to_string())
}

// ---- cancellation -------------------------------------------------------------------------

/// The reads in flight, by request id.
static ACTIVE: Mutex<Option<HashMap<String, Arc<Notify>>>> = Mutex::new(None);

struct Registered(String);

impl Registered {
    fn new(id: &str) -> Result<(Self, Arc<Notify>), ApiError> {
        let mut g = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let map = g.get_or_insert_with(HashMap::new);
        if map.contains_key(id) {
            return Err(ApiError::conflict("That request id is already in use."));
        }
        let n = Arc::new(Notify::new());
        map.insert(id.to_owned(), n.clone());
        Ok((Self(id.to_owned()), n))
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        if let Some(map) = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            map.remove(&self.0);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelArgs {
    request_id: String,
}

fn cancel(_ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CancelArgs = parse_args(args)?;
    let found = ACTIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&a.request_id).cloned());
    if let Some(n) = &found {
        n.notify_one();
    }
    Ok(json!({ "cancelled": found.is_some() }))
}

// ---- the op -------------------------------------------------------------------------------

fn request_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// The connection to read with: the one already held, or (unless `attach_only`) the saved one.
fn client_for(ctx: &OpCtx, attach_only: bool) -> Result<Client, ApiError> {
    let link = ctx.services.get::<BoardLink>();
    if let Some(l) = &link {
        if let Ok(c) = l.attach_only() {
            return Ok(c);
        }
    }
    if attach_only {
        return Err(api_error(&StandupError::NotStarted));
    }
    let db = ctx.services.need::<Database>()?;
    let creds = ctx_credentials(ctx)?;
    let client = resolve(&db, &creds).map_err(|e| api_error(&e))?;
    if let Some(l) = &link {
        l.set(Some(client.clone()));
    }
    Ok(client)
}

async fn read(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ReadArgs = parse_args(args)?;
    if !request_id_ok(&a.request_id) {
        return Err(ApiError::bad_args(
            "requestId must be 1 to 64 letters, digits, - or _.",
        ));
    }
    let client = client_for(&ctx, a.attach_only)?;
    let timeout = Duration::from_millis(
        a.timeout_ms
            .unwrap_or(MAX_TIMEOUT_MS)
            .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS),
    );
    let (_registered, cancelled) = Registered::new(&a.request_id)?;
    let action = a.action.clone();
    let job = tokio::task::spawn_blocking(move || run(&client, &a));
    tokio::select! {
        done = job => {
            let data = done
                .map_err(|_| ApiError::internal("The read stopped."))?
                .map_err(|e| api_error(&e))?;
            to_value(&json!({ "action": action, "data": data }))
        }
        _ = cancelled.notified() => Err(ApiError::new("cancelled", "The read was cancelled.")),
        _ = tokio::time::sleep(timeout) => {
            Err(ApiError::new("timed_out", "Agent Standup didn't answer in time."))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_standup::{RawResponse, Token, Transport, TransportError};

    /// Answers every path with a small JSON body and records what was asked.
    struct Script {
        seen: Mutex<Vec<String>>,
    }

    impl Transport for Script {
        fn get(&self, url: &str, _b: &str, _t: Duration) -> Result<RawResponse, TransportError> {
            self.seen.lock().unwrap().push(url.to_owned());
            let body = if url.contains("/api/projects") {
                r#"{"projects":[{"id":"p1","title":"One"}],"nextCursor":null}"#
            } else if url.contains("/api/fleet") {
                r#"{"assignments":[{"id":"a1","itemId":"i1"}],"nextCursor":null}"#
            } else {
                r#"{"ok":true}"#
            };
            Ok(RawResponse {
                status: 200,
                body: body.into(),
            })
        }
    }

    fn args(mut v: Value) -> ReadArgs {
        v["requestId"] = json!("r1");
        serde_json::from_value(v).unwrap()
    }

    /// Every action maps to its one route; a missing or unknown piece is refused; errors carry the
    /// codes the Tasks view switches on.
    #[test]
    fn each_action_is_one_get_and_errors_have_names() {
        let t = Arc::new(Script {
            seen: Mutex::new(Vec::new()),
        });
        let c = Client::with_transport(
            "https://board.example",
            Token::new("tok-12345").unwrap(),
            t.clone(),
        )
        .unwrap();
        let cases = [
            (
                json!({"action": "projects"}),
                "/api/projects?includeCompleted=true",
            ),
            (
                json!({"action": "board_page", "column": "waiting", "project": "p1", "limit": 30}),
                "/api/board?column=waiting&level=exclude%3A0&limit=30&project=p1",
            ),
            (
                json!({"action": "item", "id": "5c9e2a41"}),
                "/api/items/5c9e2a41",
            ),
            (
                json!({"action": "detail", "id": "i1"}),
                "/api/items/i1/detail?historyLimit=20&artifactLimit=20",
            ),
            (
                json!({"action": "history", "id": "i1", "limit": 20, "cursor": "9"}),
                "/api/items/i1/history?full=true&limit=20&cursor=9",
            ),
            (
                json!({"action": "artifacts", "id": "i1", "limit": 20}),
                "/api/items/i1/artifacts?full=true&limit=20",
            ),
            (
                json!({"action": "loops", "id": "i1", "limit": 20, "includeClosed": true}),
                "/api/items/i1/loops?limit=20&includeClosed=true",
            ),
            (
                json!({"action": "loop", "id": "i1", "loopId": "l1"}),
                "/api/items/i1/loops/l1",
            ),
            (json!({"action": "fleet"}), "/api/fleet?limit=500"),
            (json!({"action": "ready"}), "/api/ready"),
            (
                json!({"action": "search", "query": "merge queue", "limit": 10}),
                "/api/search?q=merge%20queue&limit=10",
            ),
        ];
        for (a, want) in cases {
            let before = t.seen.lock().unwrap().len();
            let out = run(&c, &args(a.clone())).unwrap_or_else(|e| panic!("{a}: {e}"));
            let seen = t.seen.lock().unwrap();
            assert_eq!(seen.len(), before + 1, "{a}: one request");
            assert!(
                seen.last().unwrap().ends_with(want) || seen.last().unwrap().contains(want),
                "{a}: {}",
                seen.last().unwrap()
            );
            if a["action"] == "projects" {
                assert_eq!(out["projects"][0]["id"], "p1");
                assert_eq!(out["complete"], true);
            }
            if a["action"] == "fleet" {
                assert_eq!(out["assignments"][0]["itemId"], "i1");
            }
        }
        let n = t.seen.lock().unwrap().len();
        for bad in [
            json!({"action": "item"}),
            json!({"action": "board_page"}),
            json!({"action": "write_item", "id": "i1"}),
            json!({"action": "item", "id": "../x"}),
        ] {
            let e = run(&c, &args(bad.clone())).unwrap_err();
            assert_eq!(api_error(&e).code, "bad_args", "{bad}");
        }
        assert_eq!(t.seen.lock().unwrap().len(), n, "nothing refused was sent");

        for (e, code) in [
            (StandupError::NotConfigured, "not_configured"),
            (StandupError::NotStarted, "not_started"),
            (StandupError::Unauthorized, "token_rejected"),
            (StandupError::Unreachable("x".into()), "unreachable"),
            (StandupError::Status(500), "board_error"),
            (StandupError::TooLarge, "board_error"),
        ] {
            assert_eq!(api_error(&e).code, code);
        }
    }

    /// A read can be cancelled by its id, one id cannot be in flight twice, and the registry
    /// forgets a read when it ends.
    #[test]
    fn a_read_is_cancelled_by_its_request_id() {
        let (guard, note) = Registered::new("req-1").unwrap();
        assert_eq!(Registered::new("req-1").err().unwrap().code, "conflict");
        let found = ACTIVE
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|m| m.get("req-1").cloned());
        assert!(found.is_some());
        found.unwrap().notify_one();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // The stored permit wakes the waiter at once.
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(1), note.notified())
                .await
                .unwrap()
        });
        drop(guard);
        assert!(
            Registered::new("req-1").is_ok(),
            "the id is free again once the read ends"
        );
        assert!(request_id_ok("0a1b-2c_3d"));
        assert!(!request_id_ok("") && !request_id_ok("a b") && !request_id_ok(&"a".repeat(65)));
    }
}
