//! The Agent Standup outbox against an in-process fake board (no network, no live item).

use crate::sa_outbox::{flush, marker, Flushed};
use crate::sa_writer::{Response, SendError, Target, WriteTransport};
use crate::types::*;
use crate::wftools::workflow_call;
use helm_consent::AllowKinds;
use helm_core::db::{Database, MigrationRegistry};
use serde_json::{json, Value};
use std::sync::Mutex;

/// What the fake does with the next POST.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ok,
    /// Store it, then drop the reply: the outcome is unknown to Helm.
    StoreThenDrop,
    /// Drop it before it is stored.
    DropBefore,
    Refuse,
    Unreachable,
}

struct FakeBoard {
    mode: Mutex<Mode>,
    stored: Mutex<Vec<(String, Value)>>,
    posts: Mutex<usize>,
    tokens: Mutex<Vec<String>>,
}

impl FakeBoard {
    fn new() -> Self {
        Self {
            mode: Mutex::new(Mode::Ok),
            stored: Mutex::new(vec![]),
            posts: Mutex::new(0),
            tokens: Mutex::new(vec![]),
        }
    }
    fn set(&self, m: Mode) {
        *self.mode.lock().unwrap() = m;
    }
    fn stored(&self) -> Vec<(String, Value)> {
        self.stored.lock().unwrap().clone()
    }
}

impl WriteTransport for FakeBoard {
    fn send(
        &self,
        method: &str,
        url: &str,
        bearer: &str,
        body: Option<&str>,
    ) -> Result<Response, SendError> {
        self.tokens.lock().unwrap().push(bearer.to_owned());
        let path = url
            .split_once("://")
            .map(|(_, r)| &r[r.find('/').unwrap_or(r.len())..])
            .unwrap_or(url)
            .to_owned();
        if method == "GET" {
            let stored = self.stored();
            let artifacts: Vec<Value> = stored
                .iter()
                .filter(|(p, _)| p.ends_with("/artifacts"))
                .map(|(_, v)| json!({"kind": v["artifactKind"], "bodyPreview": v["body"], "ref": v["ref"]}))
                .collect();
            return Ok(Response {
                status: 200,
                body: json!({"artifacts": artifacts, "item": {"state": "on_deck"}}).to_string(),
            });
        }
        *self.posts.lock().unwrap() += 1;
        let v: Value = serde_json::from_str(body.unwrap_or("{}")).unwrap();
        match *self.mode.lock().unwrap() {
            Mode::Ok => {
                self.stored.lock().unwrap().push((path, v));
                Ok(Response {
                    status: 201,
                    body: "{}".into(),
                })
            }
            Mode::StoreThenDrop => {
                self.stored.lock().unwrap().push((path, v));
                Err(SendError::Unknown("connection reset after send".into()))
            }
            Mode::DropBefore => Err(SendError::Unknown("timed out".into())),
            Mode::Refuse => Ok(Response {
                status: 422,
                body: "{\"error\":\"bad\"}".into(),
            }),
            Mode::Unreachable => Err(SendError::NotSent("connection refused".into())),
        }
    }
}

struct T {
    _d: tempfile::TempDir,
    db: Database,
    kinds: AllowKinds,
}

fn setup() -> T {
    let mut reg = MigrationRegistry::new();
    helm_consent::register_migrations(&mut reg);
    crate::register_migrations(&mut reg);
    let d = tempfile::tempdir().unwrap();
    let db = Database::open(&d.path().join("t.sqlite3"), &reg).unwrap();
    db.write::<_, rusqlite::Error>(|tx| {
        tx.execute("INSERT INTO projects(id, name, path, created_at) VALUES ('p', 'P', 'C:/w', 1)", [])?;
        tx.execute("INSERT INTO chats(id, project_id, title, created_at, updated_at) VALUES ('c', 'p', 'T', 1, 1)", [])?;
        crate::settings::store(tx, &WorkflowSettings { enabled: true, new_chats: NewChats::Always, ..Default::default() }, 1)
    })
    .unwrap();
    let mut kinds = AllowKinds::new();
    crate::kinds::register_allow_kinds(&mut kinds);
    T { _d: d, db, kinds }
}

impl T {
    fn wf(&self, a: Value) -> Value {
        workflow_call(&self.db, &self.kinds, "c", false, &a, 10).unwrap()
    }
    fn rows(&self, state: &str) -> i64 {
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT count(*) FROM workflow_standup_outbox WHERE state = ?1",
                    [state],
                    |r| r.get(0),
                )
            })
            .unwrap()
    }
}

const ITEM: &str = "11111111-2222-4333-8444-555555555555";

fn target() -> Target {
    Target::new("http://127.0.0.1:9", "test-token-not-real").unwrap()
}

#[test]
fn only_a_linked_task_queues_records_and_they_carry_no_token() {
    let t = setup();
    t.wf(json!({"action": "start", "title": "Unlinked"}));
    assert_eq!(t.rows("pending"), 0, "no item, nothing for the board");
    t.wf(json!({"action": "park", "reason": "x"}));
    t.wf(json!({"action": "start", "title": "Linked", "itemId": ITEM, "new": true}));
    t.wf(json!({"action": "plan", "text": "the plan"}));
    assert_eq!(
        t.rows("pending"),
        2,
        "a transition to executing and a plan artifact"
    );
    let queued = |chat: &str| {
        t.db.snapshot(|tx| crate::sa_outbox::waiting(tx, chat))
            .unwrap()
    };
    assert_eq!(
        (queued("c"), queued("other")),
        (2, 0),
        "the panel's count of records waiting"
    );
    let board = FakeBoard::new();
    assert_eq!(
        flush(&t.db, &target(), &board, 100, 50).unwrap(),
        Flushed {
            sent: 2,
            ..Default::default()
        }
    );
    assert_eq!(queued("c"), 0, "nothing waits once the board has them");
    let stored = board.stored();
    assert!(stored[0]
        .0
        .ends_with(&format!("/api/items/{ITEM}/transition")));
    assert_eq!(stored[0].1["to"], "executing");
    assert_eq!(stored[1].1["artifactKind"], "plan");
    assert!(stored[1].1["body"].as_str().unwrap().starts_with("[helm "));
    assert!(stored[1].1["body"].as_str().unwrap().contains("the plan"));
    assert_eq!(stored[1].1["createdById"], "helm");
    for (_, v) in &stored {
        assert!(
            !v.to_string().contains("test-token-not-real"),
            "the token never goes into a body"
        );
    }
    assert!(board
        .tokens
        .lock()
        .unwrap()
        .iter()
        .all(|b| b == "test-token-not-real"));
    // Sent rows are not sent again.
    assert_eq!(
        flush(&t.db, &target(), &board, 200, 50).unwrap(),
        Flushed::default()
    );
}

#[test]
fn an_unknown_outcome_is_read_back_before_any_resend() {
    let t = setup();
    t.wf(json!({"action": "start", "title": "Linked", "itemId": ITEM}));
    t.wf(json!({"action": "plan", "text": "p"}));
    let board = FakeBoard::new();
    // The board stores both but the replies are lost.
    board.set(Mode::StoreThenDrop);
    assert_eq!(flush(&t.db, &target(), &board, 100, 50).unwrap().unknown, 2);
    let posts = *board.posts.lock().unwrap();
    board.set(Mode::Ok);
    // Not due yet: nothing happens.
    assert_eq!(
        flush(&t.db, &target(), &board, 101, 50).unwrap(),
        Flushed::default()
    );
    // Due: the artifact is found by its marker, so it is NOT posted again. The transition reads
    // the item back, finds it still on deck (the fake does not move states), and is resent.
    let f = flush(&t.db, &target(), &board, 100 + 3_600_000, 50).unwrap();
    assert_eq!(f.sent, 2);
    assert_eq!(
        *board.posts.lock().unwrap(),
        posts + 1,
        "only the transition was resent"
    );
    let plans = board
        .stored()
        .iter()
        .filter(|(_, v)| v["artifactKind"] == "plan")
        .count();
    assert_eq!(plans, 1, "the plan artifact exists once");
}

#[test]
fn a_lost_request_is_resent_and_a_refusal_is_final() {
    let t = setup();
    t.wf(json!({"action": "start", "title": "Linked", "itemId": ITEM}));
    let board = FakeBoard::new();
    board.set(Mode::DropBefore);
    assert_eq!(flush(&t.db, &target(), &board, 100, 50).unwrap().unknown, 1);
    board.set(Mode::Ok);
    assert_eq!(
        flush(&t.db, &target(), &board, 100 + 3_600_000, 50)
            .unwrap()
            .sent,
        1
    );
    assert_eq!(board.stored().len(), 1);
    // A refusal is never retried.
    t.wf(json!({"action": "plan", "text": "p"}));
    board.set(Mode::Refuse);
    assert_eq!(flush(&t.db, &target(), &board, 200, 50).unwrap().failed, 1);
    let err: String =
        t.db.snapshot(|tx| {
            tx.query_row(
                "SELECT last_error FROM workflow_standup_outbox WHERE state = 'failed'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert!(err.starts_with("422"), "{err}");
    // Unreachable: nothing was sent, it waits and stays pending.
    t.wf(json!({"action": "park", "reason": "x"}));
    t.wf(json!({"action": "start", "title": "Second", "itemId": ITEM, "new": true}));
    board.set(Mode::Unreachable);
    assert_eq!(flush(&t.db, &target(), &board, 300, 50).unwrap().waiting, 1);
    assert_eq!(t.rows("pending"), 1);
}

#[test]
fn the_marker_names_the_row() {
    assert_eq!(marker("r-1"), "[helm r-1]");
}
