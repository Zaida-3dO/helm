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
    /// The titles helper's answer, after `title_delay`.
    pub title: Mutex<Result<String, String>>,
    pub title_delay: Mutex<Duration>,
    pub title_jobs: Mutex<Vec<Value>>,
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
        // A test that has already finished no longer listens; that is not this test's failure.
        let _ = self.tx.send(FakeWorker {
            req: req.clone(),
            cmds: crx,
            out: otx,
        });
        Ok((link, orx))
    }

    fn run_title(&self, job: &Value) -> Result<String, String> {
        self.title_jobs.lock().unwrap().push(job.clone());
        std::thread::sleep(*self.title_delay.lock().unwrap());
        self.title.lock().unwrap().clone()
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
        // Generous: a test that needs the ack to time out sets its own (see ACK_TIMEOUT_TEST).
        // At 400 ms the prompt ack raced the fake worker's answer under full-suite load.
        ack: Duration::from_secs(3),
        stop: Duration::from_millis(400),
        model_ack: Duration::from_millis(300),
        flush: Duration::from_millis(20),
        stream_db: Duration::ZERO,
        title_wait: Duration::ZERO,
        wait_retry: Duration::from_millis(50),
        max_active: 100,
        max_idle_workers: 20,
        // Slow by default; the stall test speeds it up.
        tick: Duration::from_secs(5),
        stall_unit: Duration::from_millis(100),
        send_soon: Duration::from_secs(5),
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
        title: Mutex::new(Err("no titles in this test".into())),
        title_delay: Mutex::new(Duration::ZERO),
        title_jobs: Mutex::new(Vec::new()),
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

/// The ack limit for the test that waits for it to expire.
const ACK_TIMEOUT_TEST: Duration = Duration::from_millis(400);

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
    let mut h = harness_with(Limits {
        ack: ACK_TIMEOUT_TEST,
        ..limits()
    });
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
    // The hold is written just after the status (a loaded machine can see the gap).
    h.until("held", |h| {
        h.col(&format!(
            "SELECT chat_id FROM chat_queue_holds WHERE chat_id = '{chat}'"
        )) == std::slice::from_ref(&chat)
    })
    .await;
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
        fork_of: None,
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
    h.db.write(|tx| helm_core::prefs::set_generic(tx, "stallMinutes", &json!(10)))
        .unwrap();
    let (chat, _, w) = running_turn(&mut h).await;
    let flag = |h: &H| {
        h.col(&format!(
            "SELECT stalled FROM chat_activity WHERE chat_id = '{chat}'"
        ))
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
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

/// One finished turn with a reply, on the fake worker.
async fn finished_turn(h: &H, w: &mut FakeWorker, chat: &str, prompt: &str, mid: &str, text: &str) {
    h.send(chat, prompt, "default").await;
    let p = w.expect("prompt").await;
    ack(w, &p).await;
    echo(w, p["submissionId"].as_str().unwrap()).await;
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "message_start", "message": {"id": mid, "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_end", "message": {"id": mid, "role": "assistant", "content": [{"type": "text", "text": text}], "status": "complete"}}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(chat) == "idle").await;
}

fn texts(h: &H, chat: &str) -> Vec<String> {
    h.col(&format!(
        "SELECT role || ':' || text FROM messages WHERE chat_id = '{chat}' AND active = 1 ORDER BY timestamp, id"
    ))
}

async fn first_turn(h: &mut H, chat: &str) -> FakeWorker {
    h.send(chat, "first", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    echo(&w, p["submissionId"].as_str().unwrap()).await;
    w.emit(json!({"type": "agent_start"})).await;
    // Direct records (no global streaming poll), so several chats can each run a first turn.
    w.emit(json!({"type": "message_start", "message": {"id": "m1", "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_end", "message": {"id": "m1", "role": "assistant", "content": [{"type": "text", "text": "answer one"}], "status": "complete"}}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(chat) == "idle").await;
    w
}

/// A whole-chat fork copies the history, ends it with a boundary, is idempotent on the fork
/// id, and its first launch resumes the source's Claude session as a new one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_whole_chat_fork_copies_history_and_resumes_as_a_new_session() {
    let mut h = harness();
    let chat = h.create().await;
    let _w = first_turn(&mut h, &chat).await;
    let sessions = h.profile.path("sessions");
    crate::session_log::ClaudeBinding {
        native_session_id: Some("native-src".into()),
        cwd: h.work.to_string_lossy().into_owned(),
        last_leaf_id: None,
        harness_switch_id: None,
        updated_at: 1,
        fork_of: None,
    }
    .save(&sessions, &chat)
    .unwrap();
    let fid = uuid::Uuid::new_v4().to_string();
    let f = h
        .call("chat_fork", json!({"chatId": chat, "forkId": fid}))
        .await
        .unwrap();
    let fork = f["id"].as_str().unwrap().to_owned();
    assert_eq!(f["title"], "Fork · first");
    let again = h
        .call("chat_fork", json!({"chatId": chat, "forkId": fid}))
        .await
        .unwrap();
    assert_eq!(again["id"], fork.as_str(), "idempotent on the fork id");
    let clash = h
        .call(
            "chat_fork",
            json!({"chatId": chat, "forkId": fid, "messageId": 1}),
        )
        .await
        .unwrap_err();
    assert!(clash.message.contains("already in use"));
    let t = texts(&h, &fork);
    assert_eq!(
        t[..2],
        ["user:first".to_owned(), "assistant:answer one".to_owned()]
    );
    assert_eq!(t.last().unwrap(), "context:Forked from first");
    // The fork's first launch resumes the source's session with --fork-session.
    h.send(&fork, "carry on", "default").await;
    let w2 = h.worker().await;
    assert_eq!(w2.req.resume_session_id.as_deref(), Some("native-src"));
    assert!(w2.req.fork_session);
}

/// A message fork stops at the chosen reply, starts a fresh session, and its first prompt
/// carries the history up to there, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_fork_is_cut_at_the_reply_and_carries_it_once() {
    let mut h = harness();
    let chat = h.create().await;
    let mut w = first_turn(&mut h, &chat).await;
    finished_turn(&h, &mut w, &chat, "second", "m2", "answer two").await;
    let m1: i64 = h.col("SELECT id FROM messages WHERE source_key LIKE 'm:%:m1'")[0]
        .parse()
        .unwrap();
    let f = h
        .call(
            "chat_fork",
            json!({"chatId": chat, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": m1}),
        )
        .await
        .unwrap();
    let fork = f["id"].as_str().unwrap().to_owned();
    assert_eq!(f["draft"], "", "a message fork starts with an empty draft");
    let t = texts(&h, &fork);
    assert!(t.contains(&"assistant:answer one".to_owned()));
    assert!(
        !t.iter().any(|x| x.contains("two")),
        "nothing after the reply"
    );
    h.send(&fork, "go on", "default").await;
    let mut w2 = h.worker().await;
    assert_eq!(w2.req.resume_session_id, None, "a fresh session");
    w2.ready().await;
    let p = w2.expect("prompt").await;
    let carried = p["context"].as_str().unwrap();
    assert!(carried.contains("User: first") && carried.contains("Assistant: answer one"));
    assert!(!carried.contains("answer two"));
    ack(&w2, &p).await;
    w2.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("fork idle", |h| h.status(&fork) == "idle").await;
    h.send(&fork, "next", "default").await;
    let p = w2.expect("prompt").await;
    assert!(p.get("context").is_none(), "carried once");
    // A user row is not a fork point.
    let u: i64 = h.col("SELECT id FROM messages WHERE role = 'user' LIMIT 1")[0]
        .parse()
        .unwrap();
    let bad = h
        .call(
            "chat_fork",
            json!({"chatId": chat, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": u}),
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code, "refused");
}

/// Placement and creation: a fork lands right below its source and later rows shift; a
/// side chat quotes its excerpt, is hidden from the list and is never placed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forks_are_placed_below_the_source_and_side_chats_stay_hidden() {
    let mut h = harness();
    let a = h.create().await;
    let _w = first_turn(&mut h, &a).await;
    let b = h.create().await;
    h.db.write(|tx| {
        tx.execute("UPDATE chats SET position = 0 WHERE id = ?1", [&a])?;
        tx.execute("UPDATE chats SET position = 1 WHERE id = ?1", [&b])
    })
    .unwrap();
    let f = h
        .call(
            "chat_fork",
            json!({"chatId": a, "forkId": uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let fork = f["id"].as_str().unwrap().to_owned();
    assert_eq!(
        h.col("SELECT id FROM chats WHERE position IS NOT NULL ORDER BY position"),
        [a.clone(), fork.clone(), b.clone()]
    );
    let sidebar = h.events("sidebar");
    assert_eq!(sidebar.last().unwrap()["chatIds"], json!([b, fork]));
    let mid: i64 = h.col(&format!(
        "SELECT id FROM messages WHERE chat_id = '{a}' AND source_key LIKE 'm:%:m1'"
    ))[0]
        .parse()
        .unwrap();
    let long = "x".repeat(16_001);
    let bad = h
        .call(
            "chat_fork",
            json!({"chatId": a, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": mid, "side": true, "excerpt": long}),
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code, "bad_args");
    let side = h
        .call(
            "chat_fork",
            json!({"chatId": a, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": mid, "side": true, "excerpt": "answer one"}),
        )
        .await
        .unwrap();
    assert_eq!(side["sideSourceId"], a.as_str());
    assert_eq!(side["draft"], "Regarding this excerpt:\n\n> answer one\n\n");
    assert_eq!(side["title"], "Side chat · first");
    let list = h.call("chat_list", json!({})).await.unwrap();
    assert!(
        !list
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == side["id"]),
        "hidden until promoted"
    );
    let refused = h
        .call(
            "chat_fork",
            json!({"chatId": side["id"], "forkId": uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap_err();
    assert!(refused.message.contains("Promote"));
}

fn user_row(h: &H, chat: &str, text: &str) -> i64 {
    h.col(&format!(
        "SELECT id FROM messages WHERE chat_id = '{chat}' AND role = 'user' AND text = '{text}'"
    ))[0]
        .parse()
        .unwrap()
}

/// Resend replaces the message and everything after it; the old branch is kept inactive,
/// its submissions are superseded, the worker restarts fresh and gets the edited text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_replaces_the_branch_and_keeps_the_old_one_inactive() {
    let mut h = harness();
    let chat = h.create().await;
    let mut w = first_turn(&mut h, &chat).await;
    finished_turn(&h, &mut w, &chat, "second", "m2", "answer two").await;
    let second = user_row(&h, &chat, "second");
    let o = h
        .call(
            "message_edit_open",
            json!({"chatId": chat, "messageId": second}),
        )
        .await
        .unwrap();
    assert_eq!(o["text"], "second");
    let stale = h
        .call(
            "message_resend",
            json!({"chatId": chat, "messageId": second, "revision": o["revision"].as_i64().unwrap() + 1,
                   "submissionId": uuid::Uuid::new_v4().to_string(), "text": "x"}),
        )
        .await
        .unwrap_err();
    assert!(stale.message.contains("changed while you were editing"));
    h.call(
        "draft_save",
        json!({"chatId": chat, "text": "my draft", "revision": helm_core::now_ms() + 5}),
    )
    .await
    .unwrap();
    let sid = uuid::Uuid::new_v4().to_string();
    h.call(
        "message_resend",
        json!({"chatId": chat, "messageId": second, "revision": o["revision"],
               "submissionId": sid, "text": "second, edited"}),
    )
    .await
    .unwrap();
    // Phase two waits for the worker to close.
    w.expect("shutdown").await;
    w.exit().await;
    let mut w2 = h.worker().await;
    assert_eq!(
        w2.req.resume_session_id, None,
        "Claude's session cannot be cut, so it starts fresh"
    );
    w2.ready().await;
    let p = w2.expect("prompt").await;
    assert_eq!(p["text"], "second, edited");
    let carried = p["context"].as_str().unwrap();
    assert!(carried.contains("User: first") && !carried.contains("answer two"));
    let t = texts(&h, &chat);
    assert!(t.contains(&"user:first".to_owned()) && t.contains(&"assistant:answer one".to_owned()));
    assert!(
        !t.iter()
            .any(|x| x == "user:second" || x.contains("answer two")),
        "{t:?}"
    );
    assert!(t.iter().any(|x| x.starts_with("context:Message edited")));
    let inactive = h.col(&format!(
        "SELECT text FROM messages WHERE chat_id = '{chat}' AND active = 0 ORDER BY id"
    ));
    assert_eq!(inactive, ["second", "answer two"], "the old branch is kept");
    let old = h.col(&format!(
        "SELECT state FROM submissions WHERE chat_id = '{chat}' AND text = 'second'"
    ));
    assert_eq!(old, ["superseded"]);
    let draft = h.col(&format!("SELECT draft FROM chats WHERE id = '{chat}'"));
    assert_eq!(draft, ["my draft"], "the draft is left alone");
}

/// A crash between stage and apply is repaired at startup; the message waits for Recover.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_between_stage_and_apply_is_repaired_at_startup() {
    let mut h = harness();
    let chat = h.create().await;
    let mut w = first_turn(&mut h, &chat).await;
    finished_turn(&h, &mut w, &chat, "second", "m2", "answer two").await;
    let first = user_row(&h, &chat, "first");
    let o = crate::edit::open(&h.db, &chat, first).unwrap();
    let sid = uuid::Uuid::new_v4().to_string();
    crate::edit::stage(
        &h.db,
        &h.profile.path("sessions"),
        &crate::types::MessageResendArgs {
            chat_id: chat.clone(),
            message_id: first,
            revision: o.revision,
            submission_id: sid.clone(),
            text: "first, edited".into(),
        },
    )
    .unwrap();
    // "Crash": nothing applied it. Startup recovery does.
    crate::edit::recover_all(&h.db, &h.profile.path(""), 1);
    assert!(h.col("SELECT chat_id FROM pending_edits").is_empty());
    assert_eq!(
        h.state(&sid),
        "rejected",
        "kept for Recover, never sent unseen"
    );
    let active = h.col(&format!(
        "SELECT role || ':' || text FROM messages WHERE chat_id = '{chat}' AND active = 1 AND role != 'context'"
    ));
    assert!(
        active.is_empty(),
        "everything from the edited message on is inactive: {active:?}"
    );
    // Applying again changes nothing.
    crate::edit::recover_all(&h.db, &h.profile.path(""), 2);
    let log = crate::session_log::read(&crate::session_log::log_path(
        &h.profile.path("sessions"),
        &chat,
    ))
    .unwrap();
    let markers = log
        .entries
        .iter()
        .filter(|e| e["id"].as_str().is_some_and(|i| i.starts_with("c:edit:")))
        .count();
    assert_eq!(markers, 1);
    let marker = log
        .entries
        .iter()
        .find(|e| e["id"].as_str().is_some_and(|i| i.starts_with("c:edit:")))
        .unwrap();
    assert!(
        marker["parentId"].is_null(),
        "the branch starts where the first message did"
    );
}

/// Edits are refused while the chat works, and only for the user's sent messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_are_refused_while_busy_and_for_replies() {
    let mut h = harness();
    let chat = h.create().await;
    let mut w = first_turn(&mut h, &chat).await;
    let reply_id: i64 = h.col(&format!(
        "SELECT id FROM messages WHERE chat_id = '{chat}' AND role = 'assistant'"
    ))[0]
        .parse()
        .unwrap();
    let r = h
        .call(
            "message_edit_open",
            json!({"chatId": chat, "messageId": reply_id}),
        )
        .await
        .unwrap_err();
    assert!(r.message.contains("Only your own"));
    let first = user_row(&h, &chat, "first");
    h.send(&chat, "busy now", "default").await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    h.until("running", |h| h.status(&chat) == "running").await;
    let busy = h
        .call(
            "message_edit_open",
            json!({"chatId": chat, "messageId": first}),
        )
        .await
        .unwrap_err();
    assert!(busy.message.contains("Wait for the turn"));
}

fn title_of(h: &H, chat: &str) -> (String, String) {
    let t = h
        .col(&format!("SELECT title FROM chats WHERE id = '{chat}'"))
        .pop()
        .unwrap();
    let s = h
        .col(&format!("SELECT COALESCE(state, '') || '/' || COALESCE(job, '') FROM chat_titles WHERE chat_id = '{chat}'"))
        .pop()
        .unwrap_or_default();
    (t, s)
}

/// The first send writes the fallback at once, then the automatic title replaces it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_chat_gets_its_fallback_then_an_automatic_title() {
    let h = harness();
    *h.spawner.title.lock().unwrap() = Ok("Fix the login redirect".into());
    *h.spawner.title_delay.lock().unwrap() = Duration::from_millis(600);
    let chat = h.create().await;
    h.send(
        &chat,
        "
  the login page redirects twice after sign-in, please look
more detail",
        "default",
    )
    .await;
    assert_eq!(
        title_of(&h, &chat).0,
        "the login page redirects twice after sign-in, please look",
        "fallback: the first non-blank line"
    );
    h.until("generated", |h| {
        title_of(h, &chat) == ("Fix the login redirect".into(), "generated/".into())
    })
    .await;
    let jobs = h.spawner.title_jobs.lock().unwrap().clone();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["forked"], false);
    // Later sends start no new job.
    h.send(&chat, "and another", "queue").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.spawner.title_jobs.lock().unwrap().len(), 1);
}

/// A manual rename wins over a title that arrives later; a failed job keeps the fallback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manual_rename_wins_and_a_failure_keeps_the_fallback() {
    let h = harness();
    *h.spawner.title.lock().unwrap() = Ok("Generated title".into());
    *h.spawner.title_delay.lock().unwrap() = Duration::from_millis(200);
    let chat = h.create().await;
    h.send(&chat, "do the thing", "default").await;
    let bad = h
        .call("chat_rename", json!({"chatId": chat, "title": "  "}))
        .await
        .unwrap_err();
    assert_eq!(bad.code, "bad_args");
    let r = h
        .call("chat_rename", json!({"chatId": chat, "title": "My name"}))
        .await
        .unwrap();
    assert_eq!(r["titleState"], "manual");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        title_of(&h, &chat),
        ("My name".into(), "manual/".into()),
        "the rename wins"
    );
    // A failing helper leaves the fallback and marks the job failed.
    *h.spawner.title.lock().unwrap() = Err("no".into());
    *h.spawner.title_delay.lock().unwrap() = Duration::ZERO;
    let other = h.create().await;
    h.send(&other, "second chat", "default").await;
    h.until("failed", |h| {
        title_of(h, &other) == ("second chat".into(), "fallback/failed".into())
    })
    .await;
    // A suggestion is answered, not written.
    *h.spawner.title.lock().unwrap() = Ok("Suggested".into());
    h.until("the user row", |h| {
        !h.col(&format!(
            "SELECT id FROM messages WHERE chat_id = '{other}' AND role = 'user'"
        ))
        .is_empty()
    })
    .await;
    let s = h
        .call("title_suggest", json!({"chatId": other}))
        .await
        .unwrap();
    assert_eq!(s["title"], "Suggested");
    assert_eq!(title_of(&h, &other).0, "second chat");
}

/// An attached image rides the prompt as an image block; an image-only send is allowed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attached_images_ride_the_prompt() {
    use base64::Engine as _;
    let mut h = harness();
    let chat = h.create().await;
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(10u32.to_be_bytes());
    png.extend(10u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let put = h
        .call(
            "attachment_put",
            json!({"chatId": chat, "name": "shot.png", "mime": "image/png", "bytes": b64}),
        )
        .await
        .unwrap();
    let id = put["attachmentId"].as_str().unwrap().to_owned();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(), "text": "",
               "action": "default", "draftRevision": 0, "attachments": [id]}),
    )
    .await
    .unwrap();
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["images"][0]["mimeType"], "image/png");
    assert_eq!(p["images"][0]["data"], b64.as_str());
    // The user's row carries the image for the bubble, and attachment_get serves it back to its
    // own chat only.
    let row = h.col("SELECT details FROM messages WHERE role = 'user'");
    let d: serde_json::Value = serde_json::from_str(&row[0]).unwrap();
    assert_eq!(d["images"][0]["id"], id.as_str());
    assert_eq!(d["images"][0]["name"], "shot.png");
    assert_eq!(d["images"][0]["width"], 10);
    let got = h
        .call(
            "attachment_get",
            json!({"chatId": chat, "attachmentId": id}),
        )
        .await
        .unwrap();
    assert_eq!(got["mime"], "image/png");
    assert_eq!(got["bytes"], b64.as_str());
    let foreign = h
        .call(
            "attachment_get",
            json!({"chatId": "another-chat", "attachmentId": id}),
        )
        .await
        .unwrap_err();
    assert_eq!(foreign.code, "not_found");
    // The worker's echo rewrites the row as complete; the image stays on it.
    echo(&w, p["submissionId"].as_str().unwrap()).await;
    h.until("the echoed row", |h| {
        h.col("SELECT status FROM messages WHERE role = 'user'") == ["complete"]
    })
    .await;
    let row = h.col("SELECT details FROM messages WHERE role = 'user'");
    let d: serde_json::Value = serde_json::from_str(&row[0]).unwrap();
    assert_eq!(d["images"][0]["id"], id.as_str());
    // A second send with the same attachment is refused and saves nothing.
    let again = h
        .call(
            "chat_send",
            json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(), "text": "x",
                   "action": "queue", "draftRevision": 0, "attachments": [id]}),
        )
        .await
        .unwrap_err();
    assert_eq!(again.code, "bad_args");
    assert_eq!(h.col("SELECT count(*) FROM submissions"), ["1"]);
}

/// A message sent while a turn runs has no row until the worker echoes it: the echo carries the
/// image for the bubble.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_with_an_image_gets_it_from_the_echo() {
    use base64::Engine as _;
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(12u32.to_be_bytes());
    png.extend(7u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let put = h
        .call(
            "attachment_put",
            json!({"chatId": chat, "name": "late.png", "mime": "image/png", "bytes": b64}),
        )
        .await
        .unwrap();
    let id = put["attachmentId"].as_str().unwrap().to_owned();
    let sid = uuid::Uuid::new_v4().to_string();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": sid, "text": "look", "action": "default",
               "draftRevision": 0, "attachments": [id]}),
    )
    .await
    .unwrap();
    let p = w.expect("prompt").await;
    assert_eq!(p["streamingBehavior"], "steer");
    let rows = "SELECT details FROM messages WHERE role = 'user' AND text = 'look'";
    assert!(h.col(rows).is_empty(), "no row until the echo");
    ack(&w, &p).await;
    echo(&w, &sid).await;
    h.until("the echoed row", |h| !h.col(rows).is_empty()).await;
    let d: serde_json::Value = serde_json::from_str(&h.col(rows)[0]).unwrap();
    assert_eq!(d["images"][0]["id"], id.as_str());
    assert_eq!(d["images"][0]["height"], 7);
}

/// The skills a worker reports at get_state are what skills_list answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skills_come_from_the_workers_get_state() {
    let mut h = harness();
    let chat = h.create().await;
    assert_eq!(
        h.call("skills_list", json!({"chatId": chat}))
            .await
            .unwrap(),
        json!([])
    );
    h.send(&chat, "hi", "default").await;
    let mut w = h.worker().await;
    let c = w.expect("get_state").await;
    w.respond(
        &c,
        json!({"capabilities": {}, "skills": [{"name": "review", "description": "Review the diff"}]}),
    )
    .await;
    h.until("skills", |h| !h.rt.skills(&chat).is_empty()).await;
    let s = h
        .call("skills_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(
        s,
        json!([{"name": "review", "description": "Review the diff"}])
    );
}

async fn side_fork(h: &H, a: &str, mid: i64) -> String {
    h.call(
        "chat_fork",
        json!({"chatId": a, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": mid, "side": true, "excerpt": "answer one"}),
    )
    .await
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A side chat is promoted into the list, or discarded (confirm needed once it sent).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn side_chats_are_promoted_or_discarded() {
    let mut h = harness();
    let a = h.create().await;
    let _w = first_turn(&mut h, &a).await;
    let mid: i64 = h.col(&format!(
        "SELECT id FROM messages WHERE chat_id = '{a}' AND role = 'assistant'"
    ))[0]
        .parse()
        .unwrap();
    let s1 = side_fork(&h, &a, mid).await;
    let p = h
        .call(
            "side_chat_resolve",
            json!({"chatId": s1, "action": "promote"}),
        )
        .await
        .unwrap();
    assert!(p["sideSourceId"].is_null());
    let list = h.call("chat_list", json!({})).await.unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == s1.as_str()),
        "promoted chats are listed"
    );
    let again = h
        .call(
            "side_chat_resolve",
            json!({"chatId": s1, "action": "discard"}),
        )
        .await
        .unwrap_err();
    assert_eq!(again.code, "refused", "a promoted chat is not discarded");
    let s2 = side_fork(&h, &a, mid).await;
    h.db.write(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES ('x1', ?1, 't', 'complete', 1)",
            [&s2],
        )
    })
    .unwrap();
    let need = h
        .call(
            "side_chat_resolve",
            json!({"chatId": s2, "action": "discard"}),
        )
        .await
        .unwrap_err();
    assert_eq!(need.code, "confirm");
    h.call(
        "side_chat_resolve",
        json!({"chatId": s2, "action": "discard", "confirmed": true}),
    )
    .await
    .unwrap();
    assert!(h
        .col(&format!("SELECT id FROM chats WHERE id = '{s2}'"))
        .is_empty());
    assert!(h
        .events("chat_removed")
        .iter()
        .any(|e| e["chatId"] == s2.as_str()));
}

/// A new long-command setting relaunches an idle worker at once and a
/// running one at its next idle point.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_command_change_relaunches_workers_when_idle() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    h.db.write(|tx| {
        helm_core::prefs::set_generic(tx, "sendBehavior", &json!({"longCommands": "off"}))
    })
    .unwrap();
    h.rt.apply_send_behavior();
    assert!(
        w.no_command_for(Duration::from_millis(200)).await.is_none(),
        "a running worker is not interrupted"
    );
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    w.expect("shutdown").await;
    w.exit().await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    // The next send launches with the new setting.
    h.send(&chat, "again", "default").await;
    let w2 = h.worker().await;
    assert_eq!(
        w2.req.send_behavior.long_commands,
        crate::types::LongCommands::Off
    );
    // An idle worker relaunches at once.
    let mut w2 = w2;
    w2.ready().await;
    let p = w2.expect("prompt").await;
    ack(&w2, &p).await;
    w2.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle again", |h| h.status(&chat) == "idle").await;
    h.db.write(|tx| {
        helm_core::prefs::set_generic(tx, "sendBehavior", &json!({"longCommands": "warn"}))
    })
    .unwrap();
    h.rt.apply_send_behavior();
    w2.expect("shutdown").await;
}

/// Past the idle-worker cap the least recently used idle worker is retired.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_least_recently_used_idle_worker_is_retired_past_the_cap() {
    let mut h = harness_with(Limits {
        max_idle_workers: 1,
        ..limits()
    });
    let a = h.create().await;
    let mut wa = first_turn(&mut h, &a).await;
    let b = h.create().await;
    let wb = first_turn(&mut h, &b).await;
    // Two idle workers, cap one: the older (a) goes; b stays.
    wa.expect("shutdown").await;
    let mut wb = wb;
    assert!(
        wb.no_command_for(Duration::from_millis(200))
            .await
            .is_none(),
        "the newer stays"
    );
}

/// Model_catalogue {refresh: true} re-reads the catalogue from a live worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_catalogue_refresh_reads_a_live_get_state() {
    let mut h = harness();
    let chat = h.create().await;
    let mut w = first_turn(&mut h, &chat).await;
    let api = h.api.clone();
    let call = tokio::spawn(async move {
        api.call(
            &main(),
            "model_catalogue",
            json!({"harness": "claude", "refresh": true}),
        )
        .await
        .unwrap()
    });
    let g = w.expect("get_state").await;
    w.respond(
        &g,
        json!({"models": [{"id": "fresh-model", "name": "Fresh"}]}),
    )
    .await;
    let c = call.await.unwrap();
    assert!(c["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "fresh-model"));
}

// ---- Failure modes ----

/// A full worker channel: with every slot but one taken, ordinary commands do not fit and
/// an interrupt still does, without waiting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_channel_keeps_its_last_slot_for_interrupts() {
    use crate::runtime::enqueue;
    let (tx, mut rx) = mpsc::channel::<crate::actor::Cmd>(3);
    assert!(enqueue(&tx, crate::actor::Cmd::Retire).is_ok());
    assert!(enqueue(&tx, crate::actor::Cmd::Retire).is_ok());
    assert!(
        enqueue(&tx, crate::actor::Cmd::Retire).is_err(),
        "a best-effort command never takes the reserved slot"
    );
    assert!(
        enqueue(&tx, crate::actor::Cmd::Kick).is_err(),
        "nor does a send soon"
    );
    assert!(
        enqueue(&tx, crate::actor::Cmd::Terminate).is_ok(),
        "an interrupt takes it"
    );
    assert!(enqueue(&tx, crate::actor::Cmd::Terminate).is_err());
    for _ in 0..3 {
        rx.recv().await.unwrap();
    }
    assert!(rx.try_recv().is_err(), "nothing extra was queued");
}

/// A "send soon" retries a full channel until a slot frees, and gives the command back
/// once its window has passed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_soon_waits_for_a_slot_then_gives_up_after_its_window() {
    use crate::runtime::{enqueue, send_soon};
    let (tx, mut rx) = mpsc::channel::<crate::actor::Cmd>(2);
    assert!(enqueue(&tx, crate::actor::Cmd::Retire).is_ok());
    let sender = tx.clone();
    let waiting = tokio::spawn(async move {
        send_soon(&sender, crate::actor::Cmd::Kick, Duration::from_secs(5))
            .await
            .is_ok()
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!waiting.is_finished(), "it waits while the channel is full");
    assert!(matches!(rx.recv().await, Some(crate::actor::Cmd::Retire)));
    assert!(waiting.await.unwrap(), "delivered once a slot freed");
    assert!(matches!(rx.recv().await, Some(crate::actor::Cmd::Kick)));

    assert!(enqueue(&tx, crate::actor::Cmd::Retire).is_ok());
    let started = std::time::Instant::now();
    let r = send_soon(&tx, crate::actor::Cmd::Kick, Duration::from_millis(200)).await;
    assert!(
        matches!(r, Err(crate::actor::Cmd::Kick)),
        "given back after the window"
    );
    assert!(started.elapsed() >= Duration::from_millis(200));
}

/// A worker exiting unexpectedly: after a crash the next send relaunches the worker, and
/// the uncertain submission is not replayed into it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_a_worker_crash_the_next_send_relaunches_without_replaying() {
    let mut h = harness();
    let (chat, lost, w) = running_turn(&mut h).await;
    w.exit().await;
    h.until("interrupted", |h| h.status(&chat) == "interrupted")
        .await;
    assert_eq!(h.state(&lost), "uncertain");
    let (next, _) = h.send(&chat, "again", "default").await;
    let mut w2 = h.worker().await;
    w2.ready().await;
    let p = w2.expect("prompt").await;
    assert_eq!(
        p["submissionId"],
        next.as_str(),
        "only the new message is sent"
    );
    assert_eq!(p["text"], "again");
    ack(&w2, &p).await;
    assert!(
        w2.no_command_for(Duration::from_millis(300))
            .await
            .filter(|c| c["type"] == "prompt")
            .is_none(),
        "the uncertain submission is never resent"
    );
    assert_eq!(h.state(&lost), "uncertain");
}

/// An undelivered consent decision: the upkeep redelivers under the same submission id,
/// and a repeat of that id never reaches the agent a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_send_repeated_under_one_submission_id_is_delivered_once() {
    use crate::submit::{host_send, HostSend, HostSendMode};
    let mut h = harness();
    let chat = h.create().await;
    let send = |h: &H| {
        host_send(
            &h.rt,
            HostSend {
                chat_id: &chat,
                submission_id: "decision-1",
                text: "[Helm] The user allowed it.",
                mode: HostSendMode::Queue,
                origin: crate::types::SubmissionOrigin::App,
                gated: false,
            },
        )
    };
    send(&h).unwrap();
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], "decision-1");
    ack(&w, &p).await;
    send(&h).unwrap();
    send(&h).unwrap();
    assert!(
        w.no_command_for(Duration::from_millis(300))
            .await
            .filter(|c| c["type"] == "prompt")
            .is_none(),
        "a retry under the same id is not sent again"
    );
    assert_eq!(
        h.col("SELECT count(*) FROM submissions WHERE id = 'decision-1'"),
        ["1"]
    );
}

// ---- Unattended reliability ----

/// A stalled flag the host died with is cleared by startup recovery, so the next turn's
/// output leaves it clear; an idle chat never reads as stalled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_flag_left_by_a_crash_is_cleared_and_never_shows_on_an_idle_chat() {
    let mut h = harness();
    let chat = h.create().await;
    let seed = |h: &H| {
        h.db.write(|tx| {
            tx.execute(
                "INSERT INTO chat_activity(chat_id, last_activity_at, stalled) VALUES (?1, 0, 1)
                 ON CONFLICT(chat_id) DO UPDATE SET stalled = 1",
                [&chat],
            )
        })
        .unwrap();
    };
    seed(&h);
    let got = h.call("chat_get", json!({"chatId": chat})).await.unwrap();
    assert_eq!(
        got["chat"]["stalled"], false,
        "an idle chat never reads stalled"
    );
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(a["stalled"], json!([]));

    // "Restart": startup recovery runs before any actor exists.
    helm_core::recovery::run(&h.db, &crate::schema::tests::registry()).unwrap();
    let flag = |h: &H| {
        h.col(&format!(
            "SELECT stalled FROM chat_activity WHERE chat_id = '{chat}'"
        ))
    };
    assert_eq!(flag(&h), ["0"]);

    h.send(&chat, "go", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    h.until("running", |h| h.status(&chat) == "running").await;
    w.emit(json!({"type": "message_start", "message": {"id": "m1", "role": "assistant", "content": []}}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_ne!(
        flag(&h),
        ["1"],
        "output during the next turn leaves it clear"
    );
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(
        a["stalled"],
        json!([]),
        "not counted as stalled from the start"
    );
}

/// The composer's permission-mode pick reaches the live worker as a
/// `set_permission_mode` line and is the next worker's start line. A worker's
/// `permission_mode_kept` line (Claude Code left plan mode, or refused the pick) moves the kept mode,
/// and a refused first pick is dropped; a pick no worker has had yet is not overridden. An unknown
/// mode is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_picked_permission_mode_reaches_the_worker_and_its_successors() {
    use crate::submit::{note_kept_mode as kept, permission_start_line as start};
    let line = |m: &str| Some(json!({"type": "set_permission_mode", "mode": m}));
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let r = h
        .call(
            "chat_set_permission_mode",
            json!({"chatId": chat, "mode": "plan"}),
        )
        .await
        .unwrap();
    assert_eq!(r["mode"], "plan");
    let sent = w.expect("set_permission_mode").await;
    assert_eq!(sent["mode"], "plan");
    assert_eq!(start(&chat), line("plan"));
    kept(&chat, Some("default"));
    assert_eq!(
        start(&chat),
        line("default"),
        "the CLI left plan mode: the next worker resumes there"
    );
    kept(&chat, None);
    assert_eq!(start(&chat), None, "a refused pick is forgotten");
    // A pick no worker has had yet is not moved by an older worker's report.
    let other = h.create().await;
    assert_eq!(start(&other), None, "no pick");
    h.call(
        "chat_set_permission_mode",
        json!({"chatId": other, "mode": "acceptEdits"}),
    )
    .await
    .unwrap();
    kept(&other, Some("bypassPermissions"));
    assert_eq!(start(&other), line("acceptEdits"));
    let err = h
        .call(
            "chat_set_permission_mode",
            json!({"chatId": chat, "mode": "yolo"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad_args");
    let mut l = helm_hosttools::WorkerLines::new();
    crate::submit::register_worker_lines(&mut l);
    assert!(l.kinds().contains(&"permission_mode_kept"));
}

/// Files, folders and chats attached by reference. The host checks
/// the path (absolute, existing; file or folder read from the disk), keeps one reference per target
/// in a draft, links them with the send, and the prompt carries a section naming them as data.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn references_ride_the_prompt_as_a_data_section() {
    let mut h = harness();
    let chat = h.create().await;
    let other = h.create().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "x").unwrap();
    let put = |kind: &str, target: String| json!({"chatId": chat, "kind": kind, "target": target});
    let f = h
        .call(
            "attachment_ref",
            put("path", file.to_string_lossy().into_owned()),
        )
        .await
        .unwrap();
    assert_eq!(
        (f["kind"].as_str(), f["name"].as_str()),
        (Some("file"), Some("notes.md"))
    );
    let again = h
        .call(
            "attachment_ref",
            put("path", file.to_string_lossy().into_owned()),
        )
        .await
        .unwrap();
    assert_eq!(
        again["attachmentId"], f["attachmentId"],
        "one reference per target"
    );
    let d = h
        .call(
            "attachment_ref",
            put("path", dir.path().to_string_lossy().into_owned()),
        )
        .await
        .unwrap();
    assert_eq!(d["kind"], "folder");
    let c = h
        .call("attachment_ref", put("chat", other.clone()))
        .await
        .unwrap();
    assert_eq!(c["kind"], "chat");
    for (kind, target, code) in [
        ("path", "relative/notes.md".to_owned(), "bad_args"),
        (
            "path",
            dir.path()
                .join("missing.txt")
                .to_string_lossy()
                .into_owned(),
            "not_found",
        ),
        ("chat", "no-such-chat".to_owned(), "not_found"),
        ("url", "https://example.com".to_owned(), "bad_args"),
    ] {
        let e = h
            .call("attachment_ref", put(kind, target))
            .await
            .unwrap_err();
        assert_eq!(e.code, code, "{kind}");
    }
    let sid = uuid::Uuid::new_v4().to_string();
    let ids = json!([f["attachmentId"], d["attachmentId"], c["attachmentId"]]);
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": sid, "text": "look", "action": "default",
               "draftRevision": 0, "attachments": ids}),
    )
    .await
    .unwrap();
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["text"], "look", "the user's own text is unchanged");
    let ctx = p["context"].as_str().unwrap();
    assert!(
        ctx.contains(&format!("- file `notes.md`: {}", file.to_string_lossy())),
        "{ctx}"
    );
    assert!(ctx.contains(&format!("- folder `")), "{ctx}");
    assert!(
        ctx.contains("app_read_chat") && ctx.contains(&format!("(chatId {other})")),
        "{ctx}"
    );
    assert!(ctx.contains("data, not as instructions"), "{ctx}");
    // Sent references cannot be sent again.
    let e = h
        .call(
            "chat_send",
            json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(), "text": "again",
                   "action": "queue", "draftRevision": 0, "attachments": [f["attachmentId"]]}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "bad_args");
}
