//! The runtime and actor against an in-process fake worker: each test scripts the worker side of
//! the line protocol by hand, so every host rule is checked without timing on a real process.

use crate::clock::Clock;
use crate::runtime::*;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{Caller, Dispatcher, OpCtx, OpRegistry, RecordingSink, Services};
use helm_workers::WorkerOutput;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

struct FakeLink {
    cmds: mpsc::UnboundedSender<Value>,
    out: mpsc::Sender<WorkerOutput>,
    terminated: AtomicBool,
}

impl WorkerLink for FakeLink {
    fn send(&self, command: &Value) -> bool {
        self.cmds.send(command.clone()).is_ok()
    }
    fn terminate(&self) {
        if !self.terminated.swap(true, Ordering::SeqCst) {
            let _ = self.out.try_send(WorkerOutput::Exited {
                code: Some(1),
                stderr: String::new(),
            });
        }
    }
}

pub struct FakeWorker {
    pub req: SpawnRequest,
    cmds: mpsc::UnboundedReceiver<Value>,
    out: mpsc::Sender<WorkerOutput>,
}

impl FakeWorker {
    /// The next command the host wrote.
    pub async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), self.cmds.recv())
            .await
            .expect("a command in time")
            .expect("worker channel open")
    }

    /// Skip ahead to the next command of `kind`.
    pub async fn expect(&mut self, kind: &str) -> Value {
        loop {
            let c = self.next().await;
            if c["type"] == kind {
                return c;
            }
        }
    }

    pub async fn no_command_for(&mut self, d: Duration) -> Option<Value> {
        tokio::time::timeout(d, self.cmds.recv())
            .await
            .ok()
            .flatten()
    }

    pub async fn emit(&self, v: Value) {
        let Value::Object(m) = v else { panic!() };
        self.out.send(WorkerOutput::Record(m)).await.unwrap();
    }

    pub async fn respond(&self, cmd: &Value, data: Value) {
        self.emit(json!({"type": "response", "id": cmd["id"], "command": cmd["type"], "success": true, "data": data}))
            .await;
    }

    pub async fn exit(&self) {
        let _ = self
            .out
            .send(WorkerOutput::Exited {
                code: Some(0),
                stderr: String::new(),
            })
            .await;
    }

    /// Answer `get_state` with every capability on.
    pub async fn ready(&mut self) {
        let c = self.expect("get_state").await;
        self.respond(
            &c,
            json!({"capabilities": {"withdraw": true, "deliverNow": true, "stopTool": true, "compact": true},
                   "models": [{"id": "fake-model", "name": "Fake"}]}),
        )
        .await;
    }
}

pub struct FakeSpawner {
    tx: mpsc::UnboundedSender<FakeWorker>,
    pub fail: Mutex<Option<String>>,
}

impl Spawner for FakeSpawner {
    fn spawn(&self, req: &SpawnRequest) -> Result<Spawned, String> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        let (ctx, crx) = mpsc::unbounded_channel();
        let (otx, orx) = mpsc::channel(256);
        let link = Arc::new(FakeLink {
            cmds: ctx,
            out: otx.clone(),
            terminated: AtomicBool::new(false),
        });
        self.tx
            .send(FakeWorker {
                req: req.clone(),
                cmds: crx,
                out: otx,
            })
            .unwrap();
        Ok((link, orx))
    }
}

pub struct H {
    pub _dir: tempfile::TempDir,
    pub api: Arc<Dispatcher>,
    pub rt: ChatRuntime,
    pub sink: Arc<RecordingSink>,
    pub db: Arc<Database>,
    pub profile: Profile,
    pub workers: mpsc::UnboundedReceiver<FakeWorker>,
    pub work: std::path::PathBuf,
    pub spawner: Arc<FakeSpawner>,
}

pub fn limits() -> Limits {
    Limits {
        ack: Duration::from_millis(400),
        stop: Duration::from_millis(400),
        model_ack: Duration::from_millis(300),
        flush: Duration::from_millis(20),
        wait_retry: Duration::from_millis(50),
        max_active: 100,
        max_idle_workers: 20,
    }
}

pub fn harness_with(limits: Limits) -> H {
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::open(&dir.path().join("prof")).unwrap();
    let db =
        Arc::new(Database::open(&profile.db_path(), &crate::schema::tests::registry()).unwrap());
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let clock = Arc::new(Clock::new(0));
    let sink = Arc::new(RecordingSink::default());
    let (tx, workers) = mpsc::unbounded_channel();
    let spawner = Arc::new(FakeSpawner {
        tx,
        fail: Mutex::new(None),
    });
    let rt = ChatRuntime::new(
        db.clone(),
        clock.clone(),
        sink.clone(),
        spawner.clone(),
        RuntimeConfig {
            profile_dir: profile.path(""),
            limits,
        },
        tokio::runtime::Handle::current(),
    );
    let mut s = Services::new();
    s.insert(Arc::new(profile.clone()));
    s.insert(db.clone());
    s.insert(clock);
    s.insert(Arc::new(rt.clone()));
    let mut ops = OpRegistry::new();
    crate::register_ops(&mut ops);
    let api = Arc::new(Dispatcher::new(
        ops,
        OpCtx {
            services: Arc::new(s),
            events: sink.clone(),
        },
        false,
    ));
    H {
        _dir: dir,
        api,
        rt,
        sink,
        db,
        profile,
        workers,
        work,
        spawner,
    }
}

pub fn harness() -> H {
    harness_with(limits())
}

fn main() -> Caller {
    Caller {
        window_label: "main".into(),
        url: "http://tauri.localhost/".into(),
    }
}

impl H {
    pub async fn call(&self, op: &str, args: Value) -> Result<Value, helm_ipc::ApiError> {
        self.api.call(&main(), op, args).await
    }

    pub async fn create(&self) -> String {
        let c = self
            .call("chat_create", json!({"cwd": self.work.to_string_lossy()}))
            .await
            .unwrap();
        c["id"].as_str().unwrap().to_owned()
    }

    pub async fn send(&self, chat: &str, text: &str, action: &str) -> (String, Value) {
        let sid = uuid::Uuid::new_v4().to_string();
        let r = self
            .call(
                "chat_send",
                json!({"chatId": chat, "submissionId": sid, "text": text, "action": action, "draftRevision": 0}),
            )
            .await
            .unwrap();
        (sid, r)
    }

    pub async fn worker(&mut self) -> FakeWorker {
        tokio::time::timeout(Duration::from_secs(5), self.workers.recv())
            .await
            .expect("a worker was spawned")
            .unwrap()
    }

    pub fn col(&self, sql: &str) -> Vec<String> {
        self.db
            .snapshot(|tx| {
                let mut st = tx.prepare(sql)?;
                let rows = st.query_map([], |r| {
                    Ok(match r.get_ref(0)? {
                        rusqlite::types::ValueRef::Null => "NULL".to_owned(),
                        rusqlite::types::ValueRef::Integer(i) => i.to_string(),
                        rusqlite::types::ValueRef::Real(f) => f.to_string(),
                        rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => {
                            String::from_utf8_lossy(t).into_owned()
                        }
                    })
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
    }

    pub fn state(&self, sid: &str) -> String {
        self.col(&format!("SELECT state FROM submissions WHERE id = '{sid}'"))
            .pop()
            .unwrap_or_default()
    }

    pub fn status(&self, chat: &str) -> String {
        self.col(&format!("SELECT status FROM chats WHERE id = '{chat}'"))
            .pop()
            .unwrap()
    }

    /// Poll until `f` holds (the actor runs on its own task).
    pub async fn until(&self, what: &str, f: impl Fn(&H) -> bool) {
        for _ in 0..200 {
            if f(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    pub fn events(&self, kind: &str) -> Vec<Map<String, Value>> {
        self.sink
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.fields.clone())
            .collect()
    }
}

/// Play a short assistant reply on the fake worker.
async fn reply(h: &H, w: &FakeWorker, mid: &str, text: &str) {
    w.emit(json!({"type": "message_start", "message": {"id": mid, "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_update", "messageId": mid, "index": 0, "delta": {"type": "text_delta", "text": &text[..text.len() / 2]}}))
        .await;
    let partial = text[..text.len() / 2].to_owned();
    h.until("a streaming flush", |h| {
        h.col("SELECT status || ':' || text FROM messages WHERE role = 'assistant'")
            == [format!("streaming:{partial}")]
    })
    .await;
    w.emit(json!({"type": "message_update", "messageId": mid, "index": 0, "delta": {"type": "text_delta", "text": &text[text.len() / 2..]}}))
        .await;
    w.emit(json!({"type": "message_end", "message": {"id": mid, "role": "assistant", "content": [{"type": "text", "text": text}], "status": "complete"}}))
        .await;
}

async fn ack(w: &FakeWorker, prompt: &Value) {
    w.respond(
        prompt,
        json!({"submissionId": prompt["submissionId"], "state": "accepted"}),
    )
    .await;
}

async fn echo(w: &FakeWorker, sid: &str) {
    w.emit(json!({"type": "message_end", "message": {"id": sid, "role": "user", "content": [], "status": "complete", "submissionId": sid}}))
        .await;
}

/// The usual shape plus the ordering rule and the flush: get_state precedes the prompt, streaming
/// rows are flushed, the reply completes, the submission and chat settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_streams_and_settles() {
    let mut h = harness();
    let chat = h.create().await;
    let (sid, sent) = h.send(&chat, "hello", "default").await;
    assert_eq!(
        sent["submission"]["mode"], "prompt",
        "idle send is a prompt"
    );
    let mut w = h.worker().await;
    let first = w.next().await;
    assert_eq!(first["type"], "get_state", "nothing before get_state");
    assert!(
        w.no_command_for(Duration::from_millis(100)).await.is_none(),
        "the prompt waits for the ready answer"
    );
    w.respond(&first, json!({"capabilities": {"withdraw": true, "deliverNow": true, "stopTool": true, "compact": true}})).await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], sid.as_str());
    assert!(p.get("streamingBehavior").is_none());
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    echo(&w, &sid).await;
    w.emit(json!({"type": "submission_delivered", "submissionId": sid}))
        .await;
    reply(&h, &w, "msg_1", "Hello there!").await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-1"}))
        .await;
    h.until("chat idle", |h| h.status(&chat) == "idle").await;
    assert_eq!(h.state(&sid), "complete");
    let streamed = h
        .events("message")
        .iter()
        .flat_map(|e| e["rows"].as_array().cloned().unwrap_or_default())
        .any(|r| r["status"] == "streaming" && r["text"] == "Hello ");
    assert!(streamed, "a flush carried the partial text");
    let rows = h.col(&format!(
        "SELECT role || ':' || status || ':' || text FROM messages WHERE chat_id = '{chat}' ORDER BY timestamp, id"
    ));
    assert_eq!(
        rows,
        ["user:complete:hello", "assistant:complete:Hello there!"]
    );
    // The session log holds both, and the binding has the native session.
    let sessions = h.profile.path("sessions");
    let log = crate::session_log::read(&crate::session_log::log_path(&sessions, &chat)).unwrap();
    assert_eq!(log.entries.len(), 2);
    h.until("binding saved", |_| {
        crate::session_log::ClaudeBinding::load(&sessions, &chat).is_some()
    })
    .await;
    let b = crate::session_log::ClaudeBinding::load(&sessions, &chat).unwrap();
    assert_eq!(b.native_session_id.as_deref(), Some("native-1"));
    // Statuses went starting -> running -> idle on the chat channel.
    let statuses: Vec<String> = h
        .events("chat")
        .iter()
        .map(|e| e["chat"]["status"].as_str().unwrap().to_owned())
        .collect();
    assert!(statuses.contains(&"starting".into()) && statuses.contains(&"running".into()));
    assert_eq!(statuses.last().unwrap(), "idle");
}

/// No answer in time makes the submission uncertain, and it is never written again; a repeat
/// of the same submission id sends nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_prompt_becomes_uncertain_and_is_not_resent() {
    let mut h = harness();
    let chat = h.create().await;
    let (sid, _) = h.send(&chat, "hi", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], sid.as_str());
    h.until("uncertain", |h| h.state(&sid) == "uncertain").await;
    h.until("the turn that never began is ended", |h| {
        h.status(&chat) == "interrupted"
    })
    .await;
    let again = h
        .call(
            "chat_send",
            json!({"chatId": chat, "submissionId": sid, "text": "hi", "action": "default", "draftRevision": 0}),
        )
        .await
        .unwrap();
    assert_eq!(again["submission"]["state"], "uncertain");
    assert!(
        w.no_command_for(Duration::from_millis(300)).await.is_none(),
        "never resent"
    );
}

async fn running_turn(h: &mut H) -> (String, String, FakeWorker) {
    let chat = h.create().await;
    let (sid, _) = h.send(&chat, "start", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    h.until("running", |h| h.status(&chat) == "running").await;
    (chat, sid, w)
}

/// With the default send behaviour a busy Send steers; it is Pending (accepted) until delivered.
/// The queue action queues instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_send_steers_by_default_and_queue_queues() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let (steer, sent) = h.send(&chat, "also this", "default").await;
    assert_eq!(sent["submission"]["mode"], "steer");
    let p = w.expect("prompt").await;
    assert_eq!(p["streamingBehavior"], "steer");
    ack(&w, &p).await;
    h.until("accepted", |h| h.state(&steer) == "accepted").await;
    w.emit(json!({"type": "submission_delivered", "submissionId": steer}))
        .await;
    h.until("delivered", |h| h.state(&steer) == "delivered")
        .await;
    let (queued, sent) = h.send(&chat, "later", "queue").await;
    assert_eq!(sent["submission"]["state"], "queued");
    assert!(
        w.no_command_for(Duration::from_millis(100)).await.is_none(),
        "queued stays host-side"
    );
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    // Settle: the queued follow-up opens the next turn as a plain prompt.
    let p2 = w.expect("prompt").await;
    assert_eq!(p2["submissionId"], queued.as_str());
    assert!(p2.get("streamingBehavior").is_none());
    assert_eq!(h.state(&steer), "complete");
}

/// Stop sends abort; a clean settle is `stopped`; queued messages become uncertain;
/// dropped steers come back to the queue (then uncertain too, since Stop pauses).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_pauses_the_queue_and_returns_dropped_steers() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let (queued, _) = h.send(&chat, "queued", "queue").await;
    let (steer, _) = h.send(&chat, "steer", "steer").await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    h.until("accepted", |h| h.state(&steer) == "accepted").await;
    let api = h.api.clone();
    let c2 = chat.clone();
    let stop = tokio::spawn(async move {
        api.call(
            &main(),
            "chat_stop",
            json!({"chatId": c2, "continueQueue": false}),
        )
        .await
    });
    let abort = w.expect("abort").await;
    assert_eq!(abort["continueQueue"], false);
    w.respond(
        &abort,
        json!({"state": "stopping", "cancelledSubmissionIds": [steer]}),
    )
    .await;
    w.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
        .await;
    let r = stop.await.unwrap().unwrap();
    assert_eq!(r["status"], "stopped");
    assert_eq!(h.status(&chat), "stopped");
    assert_eq!(h.state(&queued), "uncertain");
    assert_eq!(h.state(&steer), "uncertain");
    let order = h.col(&format!(
        "SELECT submission_id FROM queued_submissions WHERE chat_id = '{chat}' ORDER BY position"
    ));
    assert_eq!(
        order,
        [steer.clone(), queued.clone()],
        "the dropped steer is at the front"
    );
    assert!(
        w.no_command_for(Duration::from_millis(150)).await.is_none(),
        "nothing sent after Stop"
    );
}

/// Esc continues the queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_lets_the_queue_continue() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let (queued, _) = h.send(&chat, "next", "queue").await;
    let api = h.api.clone();
    let c2 = chat.clone();
    tokio::spawn(async move {
        api.call(
            &main(),
            "chat_stop",
            json!({"chatId": c2, "continueQueue": true}),
        )
        .await
    });
    let abort = w.expect("abort").await;
    w.respond(
        &abort,
        json!({"state": "stopping", "cancelledSubmissionIds": []}),
    )
    .await;
    w.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
        .await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], queued.as_str());
}

/// A turn that ends abnormally (an error, not a user
/// Stop) holds the queue, shown with Resume, instead of advancing into the next queued message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abnormal_end_holds_the_queue_until_resume() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let (queued, _) = h.send(&chat, "next", "queue").await;
    w.emit(json!({"type": "agent_settled", "outcome": "error", "error": "API error"}))
        .await;
    h.until("interrupted", |h| h.status(&chat) == "interrupted")
        .await;
    assert!(
        w.no_command_for(Duration::from_millis(200)).await.is_none(),
        "the queued message is not sent"
    );
    assert_eq!(h.state(&queued), "queued");
    assert_eq!(
        h.col(&format!(
            "SELECT chat_id FROM chat_queue_holds WHERE chat_id = '{chat}'"
        )),
        std::slice::from_ref(&chat)
    );
    let got = h
        .api
        .call(&main(), "chat_get", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(got["thread"]["queueHeld"], true);
    h.api
        .call(&main(), "queue_resume", json!({"chatId": chat}))
        .await
        .unwrap();
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], queued.as_str());
}

/// A worker that does not settle within the stop limit is closed, and the run is interrupted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_that_never_settles_closes_the_worker() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let api = h.api.clone();
    let c2 = chat.clone();
    let stop = tokio::spawn(async move {
        api.call(
            &main(),
            "chat_stop",
            json!({"chatId": c2, "continueQueue": false}),
        )
        .await
    });
    let _abort = w.expect("abort").await;
    let r = stop.await.unwrap().unwrap();
    assert_eq!(r["status"], "interrupted");
    let err = h.col(&format!("SELECT error FROM chats WHERE id = '{chat}'"));
    assert_eq!(err, [crate::actor::STOP_TIMEOUT_ERROR]);
    assert_eq!(h.col("SELECT status FROM runs"), ["interrupted"]);
}

/// Interactions are held per chat, answered through the op, and dismissed on settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactions_pass_through() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    w.emit(json!({"type": "extension_ui_request", "id": "ui-1", "method": "select", "attention": "approval",
                  "title": "Run ls?", "options": ["Allow once", "Deny", "Stop turn"]}))
        .await;
    h.until("interaction shown", |h| {
        h.rt.live_thread(&chat)
            .is_some_and(|t| t.interactions.len() == 1)
    })
    .await;
    let got = h.call("chat_get", json!({"chatId": chat})).await.unwrap();
    assert_eq!(got["thread"]["interactions"][0]["attention"], "approval");
    assert_eq!(got["thread"]["interactions"][0]["chatId"], chat.as_str());
    h.call(
        "interaction_answer",
        json!({"chatId": chat, "requestId": "ui-1", "value": "Allow once"}),
    )
    .await
    .unwrap();
    let ans = w.expect("extension_ui_response").await;
    assert_eq!(ans["id"], "ui-1");
    assert_eq!(ans["value"], "Allow once");
    w.emit(json!({"type": "extension_ui_request", "id": "ui-2", "method": "confirm", "attention": "plan"}))
        .await;
    h.until("second", |h| {
        h.rt.live_thread(&chat)
            .is_some_and(|t| t.interactions.len() == 1)
    })
    .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("dismissed", |h| {
        h.rt.live_thread(&chat)
            .is_some_and(|t| t.interactions.is_empty())
    })
    .await;
}

/// Tool start and end are written as they happen, with the live tool on the thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_are_written_at_start_and_end() {
    let mut h = harness();
    let (chat, _, w) = running_turn(&mut h).await;
    w.emit(json!({"type": "tool_execution_start", "toolCallId": "tc1", "toolName": "Bash", "args": {"command": "ls"}, "stoppable": false}))
        .await;
    h.until("tool row", |h| {
        !h.col("SELECT status FROM messages WHERE source_key = 't:tc1'")
            .is_empty()
    })
    .await;
    assert_eq!(
        h.col("SELECT status FROM messages WHERE source_key = 't:tc1'"),
        ["streaming"]
    );
    w.emit(json!({"type": "tool_execution_update", "toolCallId": "tc1", "stoppable": true}))
        .await;
    h.until("stoppable", |h| {
        h.rt.live_thread(&chat)
            .is_some_and(|t| t.live_tools.first().is_some_and(|x| x.stoppable))
    })
    .await;
    w.emit(json!({"type": "tool_execution_end", "toolCallId": "tc1", "toolName": "Bash", "result": {"text": "a.txt"}, "isError": false}))
        .await;
    h.until("tool done", |h| {
        h.col("SELECT status FROM messages WHERE source_key = 't:tc1'") == ["complete"]
    })
    .await;
    assert_eq!(
        h.col("SELECT text FROM messages WHERE source_key = 't:tc1'"),
        ["a.txt"]
    );
    let args = h
        .col("SELECT json_extract(details, '$.toolArgs') FROM messages WHERE source_key = 't:tc1'");
    assert_eq!(args, [r#"{"command":"ls"}"#], "toolArgs is JSON text");
    assert!(h.rt.live_thread(&chat).unwrap().live_tools.is_empty());
}

/// A model change not acknowledged within the limit fails the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unacknowledged_model_change_fails_the_run() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let rec = h
        .call("chat_set_model", json!({"chatId": chat, "model": "opus"}))
        .await
        .unwrap();
    assert_eq!(rec["model"], "opus");
    let sm = w.expect("set_model").await;
    assert_eq!(sm["model"], "opus");
    h.until("failed", |h| h.status(&chat) == "interrupted")
        .await;
    let err = h.col(&format!("SELECT error FROM chats WHERE id = '{chat}'"));
    assert_eq!(err, [crate::actor::MODEL_ACK_ERROR]);
    let ctx =
        h.col("SELECT json_extract(details, '$.contextKind') FROM messages WHERE role = 'context'");
    assert_eq!(ctx, ["modelChange"]);
}

/// Retry states land on the failed message's details.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_states_land_on_the_failed_message() {
    let mut h = harness();
    let (_chat, _, w) = running_turn(&mut h).await;
    w.emit(json!({"type": "message_end", "message": {"id": "err1", "role": "assistant", "content": [], "status": "error", "errorMessage": "overloaded"}}))
        .await;
    w.emit(json!({"type": "auto_retry_start", "attempt": 1, "waiting": false}))
        .await;
    h.until("retrying", |h| {
        h.col("SELECT json_extract(details, '$.retry.state') FROM messages WHERE status = 'error'")
            == ["retrying"]
    })
    .await;
    h.until("marker set", |h| {
        h.col("SELECT chat_id FROM chat_retry_markers").len() == 1
    })
    .await;
    w.emit(json!({"type": "auto_retry_end", "state": "recovered", "attempt": 1}))
        .await;
    h.until("recovered", |h| {
        h.col("SELECT json_extract(details, '$.retry.state') FROM messages WHERE status = 'error'")
            == ["recovered"]
    })
    .await;
    h.until("marker cleared", |h| {
        h.col("SELECT chat_id FROM chat_retry_markers").is_empty()
    })
    .await;
}

/// The wait limit arms only for the user's accepted steer, sends deliver_now with
/// minRunMs = min(limit, 10 s), retries every interval while the answer is `waiting`, and stops
/// after any other answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wait_limit_retries_while_waiting_then_stops() {
    let mut h = harness();
    // A 5 s limit through the generic preference op (the e2e path).
    h.call(
        "preference",
        json!({"key": "sendBehavior", "value": {"waitLimitSeconds": 5}}),
    )
    .await
    .ok();
    let (chat, _, mut w) = running_turn(&mut h).await;
    // The limit applies to chats as they start; this one started with the default 30 s, so set
    // a short one straight on the live thread for the test.
    {
        let map = h.rt.sh.actors.lock().unwrap();
        map.get(&chat)
            .unwrap()
            .live
            .lock()
            .unwrap()
            .thread
            .send_behavior
            .wait_limit_seconds = Some(1);
    }
    let (steer, _) = h.send(&chat, "now please", "steer").await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    let d1 = tokio::time::timeout(Duration::from_secs(3), w.expect("deliver_now"))
        .await
        .unwrap();
    assert_eq!(d1["submissionId"], steer.as_str());
    assert_eq!(d1["reason"], "wait_limit");
    assert_eq!(d1["minRunMs"], 1000);
    w.respond(&d1, json!({"result": "waiting"})).await;
    let d2 = w.expect("deliver_now").await;
    w.respond(&d2, json!({"result": "unblocked", "moved": ["tc1"]}))
        .await;
    assert!(
        w.no_command_for(Duration::from_millis(300)).await.is_none(),
        "no retry after a real answer"
    );
}

/// Beyond the active-chat cap a send is refused and the draft is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_beyond_the_active_cap_are_refused_with_the_draft_kept() {
    let mut l = limits();
    l.max_active = 1;
    let mut h = harness_with(l);
    let (_busy, _, _w) = running_turn(&mut h).await;
    let other = h.create().await;
    h.call(
        "draft_save",
        json!({"chatId": other, "text": "keep me", "revision": 10}),
    )
    .await
    .unwrap();
    let e = h
        .call(
            "chat_send",
            json!({"chatId": other, "submissionId": uuid::Uuid::new_v4().to_string(), "text": "keep me", "action": "default", "draftRevision": 10}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "capacity");
    assert_eq!(
        h.col(&format!("SELECT draft FROM chats WHERE id = '{other}'")),
        ["keep me"]
    );
}

/// Withdraw: the text comes back as a newer draft revision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withdrawn_steer_returns_as_a_newer_draft() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let (steer, _) = h.send(&chat, "take me back", "steer").await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    h.until("accepted", |h| h.state(&steer) == "accepted").await;
    let before: i64 =
        h.db.snapshot(|t| {
            t.query_row(
                &format!("SELECT draft_revision FROM chats WHERE id = '{chat}'"),
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    let api = h.api.clone();
    let s2 = steer.clone();
    let action = tokio::spawn(async move {
        api.call(
            &main(),
            "submission_action",
            json!({"submissionId": s2, "action": "withdraw"}),
        )
        .await
    });
    let wd = w.expect("withdraw").await;
    w.respond(&wd, json!({"withdrawn": true})).await;
    let r = action.await.unwrap().unwrap();
    assert_eq!(r["submission"]["state"], "withdrawn");
    let (draft, rev): (String, i64) =
        h.db.snapshot(|t| {
            t.query_row(
                &format!("SELECT draft, draft_revision FROM chats WHERE id = '{chat}'"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!(draft, "take me back");
    assert!(rev > before);
}

/// A send whose worker cannot start is `rejected` and stays in `pending` so the user can recover it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_that_could_not_start_stays_recoverable() {
    let h = harness();
    let chat = h.create().await;
    // The fake spawner fails every start.
    *h.spawner.fail.lock().unwrap() = Some("Node.js was not found.".into());
    let (sid, _) = h.send(&chat, "lost?", "default").await;
    h.until("rejected", |h| h.state(&sid) == "rejected").await;
    let page = h
        .call("transcript_page", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(page["pending"][0]["id"], sid.as_str());
    assert_eq!(page["pending"][0]["state"], "rejected");
    let r = h
        .call(
            "submission_action",
            json!({"submissionId": sid, "action": "recover"}),
        )
        .await
        .unwrap();
    assert_eq!(r["submission"]["state"], "withdrawn");
    assert_eq!(
        h.col(&format!("SELECT draft FROM chats WHERE id = '{chat}'")),
        ["lost?"]
    );
}

/// The worker dying mid-turn interrupts the run and makes in-flight submissions uncertain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_crash_interrupts_the_run() {
    let mut h = harness();
    let (chat, sid, w) = running_turn(&mut h).await;
    w.emit(json!({"type": "worker_error", "error": "claude crashed"}))
        .await;
    w.exit().await;
    h.until("interrupted", |h| h.status(&chat) == "interrupted")
        .await;
    assert_eq!(
        h.col(&format!("SELECT error FROM chats WHERE id = '{chat}'")),
        ["claude crashed"]
    );
    assert_eq!(h.state(&sid), "uncertain");
}

/// A chat owed a continuation gets one prompt, of harness origin, resuming the native session;
/// once only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_continuation_is_sent_once_with_the_native_session() {
    let mut h = harness();
    let chat = h.create().await;
    let sessions = h.profile.path("sessions");
    let cwd = h.col(&format!("SELECT p.path FROM chats c JOIN projects p ON p.id = c.project_id WHERE c.id = '{chat}'")).pop().unwrap();
    crate::session_log::ClaudeBinding {
        native_session_id: Some("native-7".into()),
        cwd,
        last_leaf_id: None,
        harness_switch_id: None,
        updated_at: 1,
    }
    .save(&sessions, &chat)
    .unwrap();
    h.db.write(|tx| crate::schema::record_continuation(tx, &chat, 1))
        .unwrap();
    h.rt.start_continuations();
    h.rt.start_continuations();
    let mut w = h.worker().await;
    assert_eq!(w.req.resume_session_id.as_deref(), Some("native-7"));
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["text"], crate::submit::CONTINUATION_TEXT);
    assert!(
        w.no_command_for(Duration::from_millis(200)).await.is_none(),
        "once"
    );
    let origin = h.col("SELECT origin FROM submission_meta");
    assert_eq!(origin, ["harness"]);
}

/// The drain records a continuation for the running chat and sends shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_quit_drain_records_continuations_and_shuts_workers_down() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let running = h.rt.running_titles();
    assert_eq!(running.len(), 1);
    let rt = h.rt.clone();
    let drain = tokio::spawn(async move { rt.drain().await });
    let s = w.expect("shutdown").await;
    assert_eq!(s["type"], "shutdown");
    w.exit().await;
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h.col("SELECT chat_id FROM restart_continuations"), [chat]);
}
