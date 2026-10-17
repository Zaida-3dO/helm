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
        // Slow by default; the stall test speeds it up.
        tick: Duration::from_secs(5),
        stall_unit: Duration::from_millis(100),
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
    assert_eq!(
        origin,
        ["app"],
        "the restart continuation is the app's own send"
    );
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

// ---------------------------------------------------------------------------------------------
// Seams
// ---------------------------------------------------------------------------------------------

/// A send from a button or a notification leaves the draft alone; a composer send clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_action_send_leaves_the_draft_untouched() {
    let h = harness();
    let chat = h.create().await;
    h.call(
        "draft_save",
        json!({"chatId": chat, "text": "half typed", "revision": 100}),
    )
    .await
    .unwrap();
    let send = |origin: &'static str| {
        let chat = chat.clone();
        let h = &h;
        async move {
            h.call(
                "chat_send",
                json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(),
                       "text": "Update", "action": "default", "draftRevision": 100, "origin": origin}),
            )
            .await
            .unwrap()
        }
    };
    let r = send("action").await;
    assert_eq!(
        r["chat"]["draft"], "half typed",
        "an action send never touches the draft"
    );
    let r = send("notification").await;
    assert_eq!(r["chat"]["draft"], "half typed");
    let r = send("composer").await;
    assert_eq!(
        r["chat"]["draft"], "",
        "a composer send clears the draft it sent"
    );
    let bad = h
        .call(
            "chat_send",
            json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(),
                   "text": "x", "action": "default", "draftRevision": 0, "origin": "robot"}),
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code, "bad_args");
}

struct Ctx(Mutex<Vec<(String, u64)>>);
impl crate::seams::SendContextBuilder for Ctx {
    fn build(
        &self,
        _tx: &rusqlite::Transaction<'_>,
        chat_id: &str,
        payload: &Value,
        gen: crate::seams::ContextGen,
    ) -> Result<crate::seams::ContextBlock, crate::archive::Refusal> {
        self.0.lock().unwrap().push((chat_id.to_owned(), gen.0));
        if payload["refuse"] == true {
            return Err(crate::archive::Refusal::new(
                "refused",
                "no context for you",
            ));
        }
        Ok(crate::seams::ContextBlock {
            text: format!("Looking at: {}", payload["path"].as_str().unwrap_or("")),
        })
    }
}

fn send_args(
    chat: &str,
    origin: crate::types::SendOrigin,
    ctx: Value,
) -> crate::types::ChatSendArgs {
    crate::types::ChatSendArgs {
        chat_id: chat.into(),
        submission_id: uuid::Uuid::new_v4().to_string(),
        text: "look".into(),
        action: crate::types::SendAction::Default,
        draft_revision: 0,
        origin: Some(origin),
        attachments: None,
        context: Some(ctx),
    }
}

/// The context builder runs for composer sends only; its block rides the prompt as
/// `context`; a refusal refuses the send; the generation counts worker launches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn composer_sends_carry_the_built_context_block() {
    use crate::types::SendOrigin;
    let mut h = harness();
    let chat = h.create().await;
    let ctx = Arc::new(Ctx(Mutex::new(vec![])));
    let builder = crate::seams::SendContext(ctx.clone());
    let rt = h.rt.clone();
    let c2 = chat.clone();
    let b2 = builder.clone();
    let refused = tokio::task::spawn_blocking(move || {
        crate::submit::chat_send(
            &rt,
            send_args(&c2, SendOrigin::Composer, json!({"refuse": true})),
            Some(&b2),
        )
    })
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(refused.message, "no context for you");
    assert!(
        h.col("SELECT id FROM submissions").is_empty(),
        "a refused send saves nothing"
    );
    let rt = h.rt.clone();
    let c2 = chat.clone();
    let b2 = builder.clone();
    tokio::task::spawn_blocking(move || {
        crate::submit::chat_send(
            &rt,
            send_args(&c2, SendOrigin::Composer, json!({"path": "src/a.rs"})),
            Some(&b2),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["text"], "look");
    assert_eq!(p["context"], "Looking at: src/a.rs");
    assert_eq!(h.rt.context_gen(&chat).0, 1, "one worker launch so far");
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    let rt = h.rt.clone();
    let c2 = chat.clone();
    let b2 = builder.clone();
    tokio::task::spawn_blocking(move || {
        crate::submit::chat_send(
            &rt,
            send_args(&c2, SendOrigin::Action, json!({"path": "x"})),
            Some(&b2),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let p = w.expect("prompt").await;
    assert!(
        p.get("context").is_none(),
        "only the composer's sends carry context"
    );
    let calls = ctx.0.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "the builder is not called for an action send"
    );
    assert_eq!(calls[1].1, 0, "before the first launch the generation is 0");
}

/// An approval puts the chat in Needs you and on the badge; answering it takes it off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attention_follows_the_waiting_tracker() {
    let mut h = harness();
    let (chat, _, w) = running_turn(&mut h).await;
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(
        (a["busy"].as_u64(), a["badge"].as_u64()),
        (Some(1), Some(0))
    );
    assert_eq!(a["running"][0]["chatId"], chat.as_str());
    w.emit(json!({"type": "extension_ui_request", "id": "ui-1", "method": "select", "attention": "approval",
                  "title": "Run ls?", "options": ["Allow once", "Deny"]}))
        .await;
    h.until("needs you published", |h| {
        h.events("attention")
            .last()
            .is_some_and(|e| e["summary"]["badge"] == 1)
    })
    .await;
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(a["needsYou"][0]["kind"], "approval");
    let rev = a["revision"].as_i64().unwrap();
    let again = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(
        again["revision"].as_i64().unwrap(),
        rev,
        "no change, same revision"
    );
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    // The approval leaves Needs you; the clean finish leaves an unread completion instead.
    h.until("needs you cleared", |h| {
        h.events("attention").last().is_some_and(|e| {
            e["summary"]["busy"] == 0
                && e["summary"]["needsYou"]
                    .as_array()
                    .is_some_and(|v| v.is_empty())
                && e["summary"]["unread"][0]["reason"] == "Finished; not read yet"
        })
    })
    .await;
}

/// A clean finish writes one completion, announced once, counted as unread; the next turn
/// drops it; acknowledging it clears the badge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clean_turn_leaves_one_unread_completion() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    reply(&h, &w, "m1", "All done: the tests pass.").await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("completion announced", |h| {
        !h.events("completion").is_empty()
    })
    .await;
    let ev = h.events("completion");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["completion"]["preview"], "All done: the tests pass.");
    let k = ev[0]["completion"]["id"].as_str().unwrap().to_owned();
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(a["badge"], 1);
    assert_eq!(a["unread"][0]["chatId"], chat.as_str());
    // A new turn drops the completion.
    h.send(&chat, "again", "default").await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    h.until("dropped", |h| {
        h.col("SELECT id FROM chat_completions").is_empty()
    })
    .await;
    w.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
        .await;
    h.until("stopped", |h| h.status(&chat) == "stopped").await;
    assert!(
        h.col("SELECT id FROM chat_completions").is_empty(),
        "a stopped turn leaves none"
    );
    let _ = k;
}

/// Acknowledging the completion takes the chat off the badge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledging_the_completion_clears_the_badge() {
    let mut h = harness();
    let (chat, _, w) = running_turn(&mut h).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("completion", |h| !h.events("completion").is_empty())
        .await;
    let k = h.events("completion")[0]["completion"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.call("completion_ack", json!({"chatId": chat, "completionId": k}))
        .await
        .unwrap();
    h.until("badge 0", |h| {
        h.events("attention")
            .last()
            .is_some_and(|e| e["summary"]["badge"] == 0)
    })
    .await;
}

/// Archive retires the idle worker; archiving a running chat is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_is_refused_while_busy_and_retires_the_idle_worker() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let busy = h
        .call("chat_archive", json!({"chatId": chat}))
        .await
        .unwrap_err();
    assert_eq!(busy.message, crate::archive::BUSY_MESSAGE);
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    h.call("chat_archive", json!({"chatId": chat}))
        .await
        .unwrap();
    w.expect("shutdown").await;
}

/// A running chat with no output for the stall limit is flagged and counted; the
/// next output clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_running_chat_shows_as_stalled_until_output() {
    let mut h = harness_with(Limits {
        tick: Duration::from_millis(50),
        ..limits()
    });
    // One minute is 100 ms in the test limits.
    h.db.write(|tx| helm_core::prefs::set_generic(tx, "stallMinutes", &json!(6)))
        .unwrap();
    let (chat, _, w) = running_turn(&mut h).await;
    let flag = |h: &H| {
        h.col(&format!(
            "SELECT stalled FROM chat_activity WHERE chat_id = '{chat}'"
        ))
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_ne!(flag(&h), ["1"], "not before the limit");
    h.until("stalled", |h| flag(h) == ["1"]).await;
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(a["stalled"][0]["chatId"], chat.as_str());
    assert_eq!(a["badge"], 1);
    let got = h.call("chat_get", json!({"chatId": chat})).await.unwrap();
    assert_eq!(got["chat"]["stalled"], true);
    w.emit(json!({"type": "message_start", "message": {"id": "m9", "role": "assistant", "content": []}}))
        .await;
    h.until("unstalled", |h| flag(h) == ["0"]).await;
}

/// A muted chat leaves the badge and every group, but keep-awake still counts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_muted_chat_is_out_of_the_badge_but_still_busy() {
    let mut h = harness();
    let (chat, _, w) = running_turn(&mut h).await;
    w.emit(json!({"type": "extension_ui_request", "id": "ui-1", "method": "select", "attention": "approval",
                  "title": "Run ls?", "options": ["Allow once", "Deny"]}))
        .await;
    h.until("needs you", |h| {
        h.events("attention")
            .last()
            .is_some_and(|e| e["summary"]["badge"] == 1)
    })
    .await;
    let m = h
        .call("chat_mute", json!({"chatId": chat, "muted": true}))
        .await
        .unwrap();
    assert_eq!(m["muted"], true);
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(
        (a["badge"].as_u64(), a["busy"].as_u64()),
        (Some(0), Some(1))
    );
    assert!(a["needsYou"].as_array().unwrap().is_empty());
    assert!(
        a["running"].as_array().unwrap().is_empty(),
        "the tray leaves it out"
    );
    h.call("chat_mute", json!({"chatId": chat, "muted": false}))
        .await
        .unwrap();
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(a["badge"], 1);
}

async fn badge_of(h: &H) -> Value {
    h.call("attention_get", json!({})).await.unwrap()["badge"].clone()
}

/// Addressing or snoozing takes a chat out of the groups until new activity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addressed_and_snoozed_chats_leave_the_groups() {
    let h = harness();
    let chat = h.create().await;
    h.call("unread_mark", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(badge_of(&h).await, 1);
    let soon = helm_core::now_ms() + 60_000;
    h.call("attention_snooze", json!({"chatId": chat, "until": soon}))
        .await
        .unwrap();
    assert_eq!(badge_of(&h).await, 0, "snoozed");
    let bad = h
        .call("attention_snooze", json!({"chatId": chat, "until": 5}))
        .await
        .unwrap_err();
    assert_eq!(bad.code, "bad_args");
    h.call("attention_snooze", json!({"chatId": chat, "until": null}))
        .await
        .unwrap();
    assert_eq!(badge_of(&h).await, 1, "unsnoozed");
    tokio::time::sleep(Duration::from_millis(5)).await;
    h.call("attention_address", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(badge_of(&h).await, 0, "addressed");
    // New activity (a draft saved is a chat change) brings it back.
    tokio::time::sleep(Duration::from_millis(5)).await;
    h.call(
        "draft_save",
        json!({"chatId": chat, "text": "x", "revision": helm_core::now_ms() + 10}),
    )
    .await
    .unwrap();
    assert_eq!(badge_of(&h).await, 1, "activity resurfaces it");
}

/// Archiving, or changing the model, drops a pending restart continuation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_chat_loses_its_pending_continuation() {
    let h = harness();
    let a = h.create().await;
    let b = h.create().await;
    h.db.write(|tx| {
        crate::schema::record_continuation(tx, &a, 1)?;
        crate::schema::record_continuation(tx, &b, 1)
    })
    .unwrap();
    h.call("chat_archive", json!({"chatId": a})).await.unwrap();
    h.call("chat_set_model", json!({"chatId": b, "effort": "low"}))
        .await
        .unwrap();
    assert!(h
        .col("SELECT chat_id FROM restart_continuations")
        .is_empty());
}
