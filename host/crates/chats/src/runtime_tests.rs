//! The runtime and actor against an in-process fake worker: each test scripts the worker side of
//! the line protocol by hand, so every host rule is checked without timing on a real process.

use crate::clock::Clock;
use crate::runtime::*;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{Caller, Dispatcher, OpCtx, OpRegistry, RecordingSink, Services};
use helm_workers::WorkerOutput;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

struct FakeLink {
    cmds: mpsc::UnboundedSender<Value>,
    out: mpsc::Sender<WorkerOutput>,
    terminated: AtomicBool,
    /// How many processes this worker's group reports (the worker and the CLI make two).
    tree: Arc<AtomicU32>,
}

impl WorkerLink for FakeLink {
    fn tree_size(&self) -> Option<u32> {
        Some(self.tree.load(Ordering::SeqCst))
    }

    fn send(&self, command: &Value) -> bool {
        self.cmds.send(command.clone()).is_ok()
    }
    fn terminate(&self) {
        if !self.terminated.swap(true, Ordering::SeqCst) {
            let _ = self.out.try_send(WorkerOutput::Exited {
                code: Some(1),
                stderr: String::new(),
                by_host: true,
                job_active: Some(0),
                signal: None,
            });
        }
    }
}

pub struct FakeWorker {
    pub req: SpawnRequest,
    cmds: mpsc::UnboundedReceiver<Value>,
    out: mpsc::Sender<WorkerOutput>,
    /// The size of the group this worker reports; a test grows it to play a build starting.
    pub tree: Arc<AtomicU32>,
}

impl FakeWorker {
    /// The next command the host wrote. The wait is wide: under a whole workspace's test load
    /// the actor task can wait many seconds for CPU alone, and a command that follows from a
    /// tick or a database write waits with it. The window sizes to that scheduling, never to
    /// what the test asserts — a test that must prove a command does NOT come uses
    /// [`FakeWorker::no_command_for`], which stays short.
    pub async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(45), self.cmds.recv())
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

    /// Skip ahead to the next command of `kind`, waiting up to `d` in all: for a command that
    /// follows from work elsewhere in the runtime, which a loaded machine can be slow to finish.
    pub async fn expect_within(&mut self, kind: &str, d: Duration) -> Value {
        let deadline = tokio::time::Instant::now() + d;
        loop {
            let c = tokio::time::timeout_at(deadline, self.cmds.recv())
                .await
                .unwrap_or_else(|_| panic!("no {kind} command within {d:?}"))
                .expect("worker channel open");
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
                by_host: false,
                job_active: None,
                signal: None,
            })
            .await;
    }

    /// The worker's process ended from outside (a kill by name, say): exit code 1, only Node's
    /// start-up warning on stderr, and Claude Code still running in its job.
    pub async fn killed(&self) {
        let _ = self
            .out
            .send(WorkerOutput::Exited {
                code: Some(1),
                stderr: "(node:7) Warning: [MODULE_TYPELESS_PACKAGE_JSON] Warning: main.ts parsed as an ES module\n\
                         (Use `node --trace-warnings ...` to show where the warning was created)\n"
                    .into(),
                by_host: false,
                job_active: Some(1),
                signal: None,
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
    /// The triage helper's answer.
    pub triage: Mutex<Result<(bool, String), String>>,
    pub triage_jobs: Mutex<Vec<Value>>,
    /// The group size a worker reports when it starts.
    pub tree_at_launch: AtomicU32,
    /// The sign-in gate its Claude starts share; none unless a test sets one.
    pub sign_in: Mutex<Option<Arc<helm_core::signin_gate::SignInGate>>>,
}

impl Spawner for FakeSpawner {
    fn spawn(&self, req: &SpawnRequest) -> Result<Spawned, String> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        let (ctx, crx) = mpsc::unbounded_channel();
        let (otx, orx) = mpsc::channel(256);
        let tree = Arc::new(AtomicU32::new(self.tree_at_launch.load(Ordering::SeqCst)));
        let link = Arc::new(FakeLink {
            cmds: ctx,
            out: otx.clone(),
            terminated: AtomicBool::new(false),
            tree: tree.clone(),
        });
        // A test that has already finished no longer listens; that is not this test's failure.
        let _ = self.tx.send(FakeWorker {
            req: req.clone(),
            cmds: crx,
            out: otx,
            tree,
        });
        Ok((link, orx))
    }

    fn run_title(&self, job: &Value) -> Result<String, String> {
        self.title_jobs.lock().unwrap().push(job.clone());
        std::thread::sleep(*self.title_delay.lock().unwrap());
        self.title.lock().unwrap().clone()
    }

    /// The configured answer, one option per line.
    fn run_title_suggestions(&self, job: &Value) -> Result<Vec<String>, String> {
        self.run_title(job)
            .map(|t| t.lines().map(str::to_owned).collect())
    }

    fn run_triage(&self, job: &Value) -> Result<(bool, String), String> {
        self.triage_jobs.lock().unwrap().push(job.clone());
        self.triage.lock().unwrap().clone()
    }

    fn sign_in_gate(&self) -> Option<Arc<helm_core::signin_gate::SignInGate>> {
        self.sign_in.lock().unwrap().clone()
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
        // Generous: a test that needs a deadline to expire sets its own (see
        // ACK_TIMEOUT_TEST, and the gate table's `deadline_ms` rows). These are wall-clock
        // deadlines inside the actor, and on a saturated machine (a full lib suite next to
        // other heavy jobs) the scripted worker can be seconds old before its actor task is
        // even scheduled; a window shorter than that scheduling does not test the deadline,
        // it tests the CPU. They stay short enough that a test which deliberately waits for
        // one still finishes in seconds.
        ack: Duration::from_secs(30),
        stop: Duration::from_secs(1),
        archive_stop: Duration::from_secs(2),
        model_ack: Duration::from_secs(2),
        flush: Duration::from_millis(20),
        stream_db: Duration::ZERO,
        title_wait: Duration::ZERO,
        wait_retry: Duration::from_millis(50),
        max_active: 100,
        max_idle_workers: 20,
        // A `stallMinutes` minute is 100 ms here; the harness parks the window itself
        // (see `harness_with_services`), and the tests that study stalling set their own.
        tick: Duration::from_secs(5),
        stall_unit: Duration::from_millis(100),
        send_soon: Duration::from_secs(5),
        // Long enough that a test handing a verdict by hand always beats the hold ending;
        // the row that wants the hold to end shortens it.
        gate_deadline: Duration::from_secs(10),
        // As the gate's: a verdict handed by hand always beats the hold ending.
        stop_deadline: Duration::from_secs(10),
        // Tests start workers by the dozen and never wait on one another's spread.
        start_stagger: Duration::ZERO,
    }
}

pub fn harness_with(limits: Limits) -> H {
    harness_with_services(limits, |_| {})
}

/// A harness with more services (a project gate, say) registered before the ops are built.
pub fn harness_with_services(limits: Limits, extra: impl FnOnce(&mut Services)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::open(&dir.path().join("prof")).unwrap();
    let db =
        Arc::new(Database::open(&profile.db_path(), &crate::schema::tests::registry()).unwrap());
    // The maintenance tick flags a running chat as stalled after `stallMinutes` units of quiet,
    // and a unit here is 100 ms — the real default of five minutes would be half a second, which
    // a loaded machine crosses just scheduling the turn's own first output. Park the window above
    // every test's span so only a test that studies the stall flag arms it (each sets its own
    // `stallMinutes` in the db, as the stall tests do).
    db.write(|tx| helm_core::prefs::set_generic(tx, "stallMinutes", &json!(1440)))
        .unwrap();
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
        triage: Mutex::new(Err("no triage in this test".into())),
        triage_jobs: Mutex::new(Vec::new()),
        tree_at_launch: AtomicU32::new(2),
        sign_in: Mutex::new(None),
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
    extra(&mut s);
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

    /// The next worker the chat starts. The wait is wide on purpose: a full lib suite runs
    /// every test's actor task on its own two-thread runtime, and with the machine saturated
    /// (several test binaries at once, say) a task can wait tens of seconds for its first
    /// slice of CPU. The wait has to outlast that scheduling, since it is not the test's logic
    /// that is slow; a test that expects no worker checks for that itself.
    pub async fn worker(&mut self) -> FakeWorker {
        tokio::time::timeout(Duration::from_secs(60), self.workers.recv())
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

    /// Poll until `f` holds (the actor runs on its own task). The window is wide on purpose: a
    /// full lib suite on a loaded machine can starve this test's actor task for tens of seconds,
    /// and the state change itself is certain — the wait must outlast the scheduling, not fail
    /// with it. Only the absence of a change is ever asserted on a short timer.
    pub async fn until(&self, what: &str, f: impl Fn(&H) -> bool) {
        for _ in 0..6_000 {
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
    // Usage as the API spells it, which is what the worker passes on.
    let usage = json!({"input_tokens": 12, "output_tokens": 30, "cache_read_input_tokens": 400, "cache_creation_input_tokens": 50});
    w.emit(json!({"type": "message_end", "message": {"id": mid, "role": "assistant", "content": [{"type": "text", "text": text}], "usage": usage, "status": "complete"}}))
        .await;
}

pub async fn ack(w: &FakeWorker, prompt: &Value) {
    w.respond(
        prompt,
        json!({"submissionId": prompt["submissionId"], "state": "accepted"}),
    )
    .await;
}

pub async fn echo(w: &FakeWorker, sid: &str) {
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
    // The turn's result: its own tokens, and Claude Code's running totals (subagents included).
    w.emit(json!({"type": "usage", "inputTokens": 12, "outputTokens": 30, "cacheReadTokens": 400, "cacheWriteTokens": 50, "costUsd": 0.05,
        "running": {"inputTokens": 20, "outputTokens": 40, "cacheReadTokens": 500, "cacheWriteTokens": 60, "costUsd": 0.05}}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-1"}))
        .await;
    h.until("chat idle", |h| h.status(&chat) == "idle").await;
    assert_eq!(h.state(&sid), "complete");
    // The reply keeps its own request's tokens; the turn's tokens and cost are recorded once.
    assert_eq!(
        h.col("SELECT json_extract(details, '$.usage.inputTokens') || '/' || json_extract(details, '$.usage.cacheReadTokens') FROM messages WHERE role = 'assistant'"),
        ["12/400"]
    );
    assert_eq!(
        h.col("SELECT input_tokens || '/' || output_tokens || '/' || cache_read_tokens || '/' || cache_write_tokens || '/' || cost_usd || '/' || provider FROM chat_turn_usage"),
        ["20/40/500/60/0.05/claude"]
    );
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
    assert_eq!(log.entries.len(), 3, "user, reply and the turn's usage");
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

/// A message sent while Claude works goes straight to it, whichever way it is sent, and is read
/// at the next tool boundary (Pending until then). Nothing waits in Helm for the turn to end, and
/// the old "queue" action no longer exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_send_goes_to_claude_at_once() {
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
    let (second, sent) = h.send(&chat, "and this", "steer").await;
    assert_eq!(sent["submission"]["state"], "saved");
    let p2 = w.expect("prompt").await;
    assert_eq!(p2["submissionId"], second.as_str());
    assert_eq!(p2["streamingBehavior"], "steer");
    let gone = h
        .api
        .call(
            &main(),
            "chat_send",
            json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(),
                   "text": "x", "action": "queue", "draftRevision": 0}),
        )
        .await;
    assert!(gone.is_err(), "the queue action is gone");
    ack(&w, &p2).await;
    w.emit(json!({"type": "submission_delivered", "submissionId": second}))
        .await;
    h.until("delivered", |h| h.state(&second) == "delivered")
        .await;
    // A steer Claude drops on its own is not coming: it reads "not sent", not "accepted".
    let (dropped, _) = h.send(&chat, "and that", "steer").await;
    let p3 = w.expect("prompt").await;
    ack(&w, &p3).await;
    h.until("accepted", |h| h.state(&dropped) == "accepted")
        .await;
    w.emit(json!({"type": "submission_cancelled", "submissionId": dropped}))
        .await;
    h.until("uncertain", |h| h.state(&dropped) == "uncertain")
        .await;
    // A steer left "accepted" by an earlier run must not keep the chat running after this one.
    h.db.write(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, mode, state, created_at) VALUES ('stale', ?1, 't', 'steer', 'accepted', 1)",
            [&chat],
        )?;
        tx.execute(
            "INSERT INTO submission_meta(submission_id, run_id) VALUES ('stale', 'an-older-run')",
            [],
        )
    })
    .unwrap();
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    assert_eq!(h.state(&steer), "complete");
    assert!(w.no_command_for(Duration::from_millis(100)).await.is_none());
}

/// Stop sends a plain abort; a clean settle is `stopped`; everything Claude had not read — the
/// steers it cancelled and anything not yet handed over — goes back to it at once as the next
/// turn, oldest first: the first opens it and the rest are read at its tool boundaries. Nothing
/// is held and nothing is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_hands_the_unread_messages_to_the_next_turn_at_once() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let (first, _) = h.send(&chat, "one", "steer").await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    let (second, _) = h.send(&chat, "two", "steer").await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    h.until("accepted", |h| h.state(&second) == "accepted")
        .await;
    h.db.write(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES ('late', ?1, 't', 'saved', 9)",
            [&chat],
        )
    })
    .unwrap();
    let api = h.api.clone();
    let c2 = chat.clone();
    let stop =
        tokio::spawn(async move { api.call(&main(), "chat_stop", json!({"chatId": c2})).await });
    let abort = w.expect("abort").await;
    assert!(
        abort.get("continueQueue").is_none(),
        "Stop is one thing: it never asks whether to keep sending"
    );
    w.respond(
        &abort,
        json!({"state": "stopping", "cancelledSubmissionIds": [first, second]}),
    )
    .await;
    w.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
        .await;
    let r = stop.await.unwrap().unwrap();
    assert_eq!(r["status"], "stopped");
    assert_eq!(
        h.col(&format!(
            "SELECT status FROM runs WHERE chat_id = '{chat}' ORDER BY started_at, rowid"
        ))[0],
        "stopped",
        "the stopped run is closed as such"
    );
    // All three are unsent now; the settle sends them again in the order they were sent, which
    // opens the next turn at once.
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], "late");
    assert!(p.get("streamingBehavior").is_none(), "it opens the turn");
    ack(&w, &p).await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], first.as_str());
    assert_eq!(p["streamingBehavior"], "steer");
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], second.as_str());
    assert_eq!(p["streamingBehavior"], "steer");
}

/// A stop made on the app's own account (a roll over, the usage governor) sends nothing again:
/// what Claude had not read reads "not sent", and no turn opens by itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_that_asks_for_no_resend_leaves_the_unread_messages_unsent() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    h.db.write(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES ('late', ?1, 't', 'saved', 9)",
            [&chat],
        )
    })
    .unwrap();
    let api = h.api.clone();
    let c2 = chat.clone();
    let stop = tokio::spawn(async move {
        api.call(&main(), "chat_stop_no_resend", json!({"chatId": c2}))
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
    let r = stop.await.unwrap().unwrap();
    assert_eq!(r["status"], "stopped");
    assert_eq!(h.state("late"), "uncertain");
    assert!(
        w.no_command_for(Duration::from_millis(150)).await.is_none(),
        "nothing is sent again"
    );
}

/// A failed turn holds nothing back: the chat is simply interrupted, and the next message
/// starts a turn at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_turn_leaves_nothing_held() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    w.emit(json!({"type": "agent_settled", "outcome": "error", "error": "API error"}))
        .await;
    h.until("interrupted", |h| h.status(&chat) == "interrupted")
        .await;
    let (next, _) = h.send(&chat, "try again", "default").await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], next.as_str());
}

/// A worker that does not settle within the stop limit is closed, and the run is interrupted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_that_never_settles_closes_the_worker() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let api = h.api.clone();
    let c2 = chat.clone();
    let stop =
        tokio::spawn(async move { api.call(&main(), "chat_stop", json!({"chatId": c2})).await });
    let _abort = w.expect("abort").await;
    let r = stop.await.unwrap().unwrap();
    assert_eq!(r["status"], "interrupted");
    // The reply and the rows it describes are written a moment apart on a loaded machine: wait
    // for them against a deadline instead of reading them at once.
    let error_sql = format!("SELECT error FROM chats WHERE id = '{chat}'");
    h.until("the interruption is recorded", |h| {
        h.col(&error_sql) == [crate::actor::STOP_TIMEOUT_ERROR]
            && h.col("SELECT status FROM runs") == ["interrupted"]
    })
    .await;
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

/// While an AFK policy is installed it answers questions at once, leaves permission cards
/// waiting (with a transcript line), and puts its text on each new turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn afk_policy_answers_questions_and_defers_cards() {
    use crate::seams::{AfkAction, AfkPolicy};
    use crate::types::{Interaction, InteractionAttention};
    struct Policy;
    impl AfkPolicy for Policy {
        fn turn_text(&self, _: &str) -> Option<String> {
            Some("[AFK] away".into())
        }
        fn decide(&self, _: &str, i: &Interaction, _: bool) -> AfkAction {
            match i.attention {
                Some(InteractionAttention::Question) => AfkAction::Answer {
                    fields: serde_json::from_value(json!({"value": "go with the default"}))
                        .unwrap(),
                    marker: "AFK answered a question".into(),
                },
                Some(InteractionAttention::Approval) => AfkAction::Defer {
                    marker: "AFK left this for you".into(),
                },
                _ => AfkAction::Leave,
            }
        }
    }
    let mut h = harness();
    h.rt.set_afk_policy(Arc::new(Policy));
    let (chat, _, mut w) = running_turn(&mut h).await;
    w.emit(json!({"type": "extension_ui_request", "id": "q1", "method": "select", "attention": "question",
                  "title": "Which?", "options": ["a", "b"]}))
        .await;
    let ans = w.expect("extension_ui_response").await;
    assert_eq!(
        (ans["id"].as_str(), ans["value"].as_str()),
        (Some("q1"), Some("go with the default"))
    );
    w.emit(json!({"type": "extension_ui_request", "id": "a1", "method": "select", "attention": "approval",
                  "title": "Run ls?", "options": ["Allow once", "Deny"]}))
        .await;
    h.until("card waits", |h| {
        h.rt.live_thread(&chat)
            .is_some_and(|t| t.interactions.len() == 1)
    })
    .await;
    assert_eq!(h.rt.live_thread(&chat).unwrap().interactions[0].id, "a1");
    h.until("markers", |h| {
        h.col("SELECT text FROM messages WHERE role = 'context'")
            == ["AFK answered a question", "AFK left this for you"]
    })
    .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    // The turn has ended before the next message, so that one opens a turn of its own.
    h.until("the turn ends", |h| h.status(&chat) == "idle")
        .await;
    let (_, _) = h.send(&chat, "next", "default").await;
    let p = w.expect("prompt").await;
    assert_eq!(p["context"], "[AFK] away");
    // A note that itself says AFK is on does not carry the turn text a second time.
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    crate::submit::host_send_afk_note(
        &h.rt,
        crate::submit::HostSend {
            chat_id: &chat,
            submission_id: &uuid::Uuid::new_v4().to_string(),
            text: "AFK just turned on.",
            origin: crate::types::SubmissionOrigin::App,
        },
    )
    .unwrap();
    let p = w.expect("prompt").await;
    assert!(p.get("context").is_none(), "the note says it itself");
}

/// The question gate's hold, one table beside the AFK test. A fake gate only decides whether to
/// hold a card; the test plays the gate's late verdict by hand (`gate_verdict`), the user's
/// answer during the hold, and the hold deadline (shortened). Every row ends with the card in
/// the user's hands, the gate's hands, or answered — nothing is ever dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_question_gate_holds_cards_until_it_judges_them() {
    use crate::seams::{GateOffer, GateOutcome, QuestionGate};
    use crate::types::{
        ContextKind, GateBrief, GateBriefOption, GateStatus, Interaction, InteractionAttention,
    };

    struct FakeGate {
        mode: GateOffer,
        offered: Mutex<Vec<String>>,
    }
    impl QuestionGate for FakeGate {
        fn offer(&self, _: &str, i: &Interaction) -> GateOffer {
            self.offered.lock().unwrap().push(i.id.clone());
            self.mode
        }
    }

    struct Policy;
    impl crate::seams::AfkPolicy for Policy {
        fn turn_text(&self, _: &str) -> Option<String> {
            None
        }
        fn decide(&self, _: &str, i: &Interaction, _: bool) -> crate::seams::AfkAction {
            match i.attention {
                Some(InteractionAttention::Question) => crate::seams::AfkAction::Answer {
                    fields: serde_json::from_value(json!({"value": "AFK default"})).unwrap(),
                    marker: "AFK answered a question".into(),
                },
                _ => crate::seams::AfkAction::Leave,
            }
        }
    }

    fn question(id: &str) -> Value {
        json!({"type": "extension_ui_request", "id": id, "method": "select",
               "attention": "question", "title": "Which?", "options": ["ship it", "hold"]})
    }
    fn brief() -> GateBrief {
        GateBrief {
            problem: "It asks which route to take.".into(),
            options: vec![GateBriefOption {
                label: "ship it".into(),
                means: "carry on now".into(),
                pros: "fastest".into(),
                cons: "less checking".into(),
                cost: "none".into(),
            }],
            recommendation: "ship it".into(),
            why: "the cheap choice, easy to undo".into(),
        }
    }
    fn gate_state(h: &H, chat: &str, id: &str) -> Option<(GateStatus, Interaction)> {
        h.rt.live_thread(chat)
            .and_then(|t| t.interactions.into_iter().find(|i| i.id == id))
            .and_then(|i| {
                let s = i.gate.as_ref()?.state;
                Some((s, i))
            })
    }
    async fn needs_you_has(h: &H, chat: &str) -> bool {
        let a = h.call("attention_get", json!({})).await.unwrap();
        a["needsYou"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["chatId"] == chat)
    }

    enum Then {
        /// The gate passed it: a normal card, no gate mark.
        ShownPlain,
        /// The gate decided: answered, card gone, a transcript line, and a second verdict is false.
        Decided,
        /// The gate escalated: the card stays with the brief, the recommendation pre-filled,
        /// and it counts in the inbox.
        Escalated,
        /// The gate failed it: a normal card with the quiet line.
        FailedVerdict,
        /// The user answered during the hold; the late verdict comes back false.
        UserAnswered,
        /// The hold deadline passed with no verdict: the card becomes a normal failed card.
        TimedOut,
        /// The gate passed it and AFK answers, exactly as before the gate existed.
        AfkAnswered,
        /// The gate escalated it, then AFK turned on: the sweep leaves it for the user.
        EscalatedThenAfk,
    }
    struct Case {
        then: Then,
        mode: GateOffer,
        card: Value,
        afk: bool,
        deadline_ms: Option<u64>,
    }
    let cases = [
        Case {
            then: Then::ShownPlain,
            mode: GateOffer::Pass,
            card: question("q1"),
            afk: false,
            deadline_ms: None,
        },
        Case {
            then: Then::Decided,
            mode: GateOffer::Hold,
            card: question("q1"),
            afk: false,
            deadline_ms: None,
        },
        Case {
            then: Then::Escalated,
            mode: GateOffer::Hold,
            card: question("q1"),
            afk: false,
            deadline_ms: None,
        },
        Case {
            then: Then::FailedVerdict,
            mode: GateOffer::Hold,
            card: question("q1"),
            afk: false,
            deadline_ms: None,
        },
        Case {
            then: Then::UserAnswered,
            mode: GateOffer::Hold,
            card: question("q1"),
            afk: false,
            deadline_ms: None,
        },
        Case {
            then: Then::TimedOut,
            mode: GateOffer::Hold,
            card: question("q1"),
            afk: false,
            deadline_ms: Some(150),
        },
        Case {
            then: Then::ShownPlain,
            mode: GateOffer::Hold,
            // The safety line: an approval card is never offered, whatever the gate would say.
            card: json!({"type": "extension_ui_request", "id": "a1", "method": "select",
                         "attention": "approval", "title": "Run ls?", "options": ["Allow once", "Deny"]}),
            afk: false,
            deadline_ms: None,
        },
        Case {
            then: Then::ShownPlain,
            mode: GateOffer::Hold,
            // Nor a secret input.
            card: json!({"type": "extension_ui_request", "id": "s1", "method": "input",
                         "attention": "question", "title": "Token?", "secret": true}),
            afk: false,
            deadline_ms: None,
        },
        Case {
            then: Then::AfkAnswered,
            mode: GateOffer::Pass,
            card: question("q1"),
            afk: true,
            deadline_ms: None,
        },
        Case {
            then: Then::ShownPlain,
            mode: GateOffer::Hold,
            // A step the user went back to (the card's Back button) comes with their earlier
            // answer filled in: they are revising it themselves, so it is never offered.
            card: json!({"type": "extension_ui_request", "id": "q2", "method": "select",
                         "attention": "question", "title": "Which?", "options": ["ship it", "hold"],
                         "prefill": "hold"}),
            afk: false,
            deadline_ms: None,
        },
        Case {
            then: Then::EscalatedThenAfk,
            mode: GateOffer::Hold,
            card: question("q1"),
            afk: false,
            deadline_ms: None,
        },
    ];

    for c in cases {
        let mut limits = limits();
        if let Some(ms) = c.deadline_ms {
            limits.gate_deadline = Duration::from_millis(ms);
        }
        let mut h = harness_with(limits);
        let gate = Arc::new(FakeGate {
            mode: c.mode,
            offered: Mutex::new(Vec::new()),
        });
        h.rt.set_question_gate(gate.clone());
        if c.afk {
            h.rt.set_afk_policy(Arc::new(Policy));
        }
        let (chat, _, mut w) = running_turn(&mut h).await;
        w.emit(c.card.clone()).await;
        let id = c.card["id"].as_str().unwrap().to_owned();

        match c.then {
            Then::ShownPlain | Then::AfkAnswered => {
                if matches!(c.then, Then::AfkAnswered) {
                    let ans = w.expect("extension_ui_response").await;
                    assert_eq!(
                        ans["value"], "AFK default",
                        "AFK still answers a passed card"
                    );
                } else {
                    h.until("the card waits for the user", |h| {
                        h.rt.live_thread(&chat)
                            .is_some_and(|t| t.interactions.len() == 1)
                    })
                    .await;
                    assert_eq!(
                        h.rt.live_thread(&chat).unwrap().interactions[0].gate,
                        None,
                        "a passed card carries no gate mark"
                    );
                }
                let seen = gate.offered.lock().unwrap().clone();
                if c.card["attention"] == "question"
                    && c.card.get("secret").is_none()
                    && c.card.get("prefill").is_none()
                {
                    assert_eq!(
                        seen,
                        [id.as_str()],
                        "a question card is offered to the gate"
                    );
                } else {
                    assert!(
                        seen.is_empty(),
                        "an approval, a secret or a revisited step never reaches the gate"
                    );
                }
            }
            Then::Decided
            | Then::Escalated
            | Then::EscalatedThenAfk
            | Then::FailedVerdict
            | Then::UserAnswered
            | Then::TimedOut => {
                let applied = if matches!(c.then, Then::TimedOut) {
                    // No verdict ever comes: the shortened hold ends by itself and the card
                    // is the user's again, counted like any other.
                    h.until("the hold ends by itself", |h| {
                        h.rt.live_thread(&chat).is_some_and(|t| {
                            t.interactions.iter().any(|i| {
                                i.id == id
                                    && i.gate
                                        .as_ref()
                                        .is_some_and(|g| g.state == GateStatus::Failed)
                            })
                        })
                    })
                    .await;
                    assert!(
                        needs_you_has(&h, &chat).await,
                        "a timed-out card counts as normal"
                    );
                    assert!(
                        w.no_command_for(Duration::from_millis(120)).await.is_none(),
                        "nothing was answered for the user"
                    );
                    true
                } else {
                    h.until("the card is held", |h| {
                        gate_state(h, &chat, &id).is_some_and(|(s, _)| s == GateStatus::Checking)
                    })
                    .await;
                    // During the hold the badge stays quiet.
                    assert!(
                        !needs_you_has(&h, &chat).await,
                        "a held card is not counted"
                    );
                    match c.then {
                    Then::Decided => h.rt.gate_verdict(
                        &chat,
                        &id,
                        GateOutcome::Decide {
                            fields: serde_json::from_value(
                                json!({"value": "ship it (Helm chose this default for the user)"}),
                            )
                            .unwrap(),
                            marker: "Helm answered for you: ship it".into(),
                        },
                    ),
                    Then::Escalated | Then::EscalatedThenAfk => h
                        .rt
                        .gate_verdict(&chat, &id, GateOutcome::Escalate { brief: brief() }),
                    Then::FailedVerdict => h.rt.gate_verdict(
                        &chat,
                        &id,
                        GateOutcome::Failed {
                            note: "Helm couldn't check this one.".into(),
                        },
                    ),
                    Then::UserAnswered => {
                        h.call(
                            "interaction_answer",
                            json!({"chatId": chat, "requestId": id, "value": "hold"}),
                        )
                        .await
                        .unwrap();
                        let ans = w.expect("extension_ui_response").await;
                        assert_eq!((ans["id"].as_str(), ans["value"].as_str()), (Some(id.as_str()), Some("hold")));
                        // The verdict arrives after the user answered: it finds no card.
                        h.rt.gate_verdict(
                            &chat,
                            &id,
                            GateOutcome::Escalate { brief: brief() },
                        )
                    }
                        _ => unreachable!(),
                    }
                };
                let expect_applied = !matches!(c.then, Then::UserAnswered);
                assert_eq!(
                    applied, expect_applied,
                    "the verdict finds the card it is for"
                );
                match c.then {
                    Then::Decided => {
                        let ans = w.expect("extension_ui_response").await;
                        assert_eq!(
                            (ans["id"].as_str(), ans["value"].as_str()),
                            (
                                Some(id.as_str()),
                                Some("ship it (Helm chose this default for the user)")
                            )
                        );
                        h.until("the card is gone", |h| {
                            h.rt.live_thread(&chat)
                                .is_some_and(|t| t.interactions.iter().all(|i| i.id != id))
                        })
                        .await;
                        h.until("the transcript line", |h| {
                            h.col("SELECT text FROM messages WHERE role = 'context'")
                                == ["Helm answered for you: ship it"]
                                && h.col("SELECT json_extract(details, '$.contextKind') FROM messages WHERE role = 'context'")
                                    == [serde_json::to_value(ContextKind::Gatekeeper).unwrap().as_str().unwrap()]
                        })
                        .await;
                        assert!(
                            !h.rt.gate_verdict(
                                &chat,
                                &id,
                                GateOutcome::Failed {
                                    note: "late".into()
                                }
                            ),
                            "a second verdict on a gone card is false"
                        );
                    }
                    Then::Escalated | Then::EscalatedThenAfk => {
                        h.until("the brief sits on the card", |h| {
                            gate_state(h, &chat, &id).is_some_and(|(s, i)| {
                                s == GateStatus::Escalated
                                    && i.prefill.as_deref() == Some("ship it")
                                    && i.gate.as_ref().is_some_and(|g| {
                                        g.brief
                                            .as_ref()
                                            .is_some_and(|b| b.problem.starts_with("It asks"))
                                    })
                            })
                        })
                        .await;
                        assert!(
                            needs_you_has(&h, &chat).await,
                            "an escalated card counts from then on"
                        );
                        if matches!(c.then, Then::EscalatedThenAfk) {
                            // AFK turns on later: its sweep answers every plain question, but
                            // this one was judged to need the user, so it stays theirs.
                            h.rt.set_afk_policy(Arc::new(Policy));
                            h.rt.afk_sweep();
                            assert!(
                                w.no_command_for(Duration::from_millis(500)).await.is_none(),
                                "AFK never answers a card the gate handed to the user"
                            );
                            assert!(
                                gate_state(&h, &chat, &id)
                                    .is_some_and(|(s, _)| s == GateStatus::Escalated),
                                "the card is still there"
                            );
                        }
                    }
                    Then::FailedVerdict => {
                        h.until("the quiet line", |h| {
                            h.rt.live_thread(&chat).is_some_and(|t| {
                                t.interactions.iter().any(|i| {
                                    i.id == id
                                        && i.gate.as_ref().is_some_and(|g| {
                                            g.state == GateStatus::Failed
                                                && g.note.as_deref()
                                                    == Some("Helm couldn't check this one.")
                                        })
                                })
                            })
                        })
                        .await;
                        assert!(
                            needs_you_has(&h, &chat).await,
                            "a failed card counts as normal"
                        );
                    }
                    Then::UserAnswered => {
                        assert!(h
                            .rt
                            .live_thread(&chat)
                            .is_some_and(|t| t.interactions.iter().all(|i| i.id != id)));
                    }
                    _ => {}
                }
                let seen = gate.offered.lock().unwrap().clone();
                assert_eq!(
                    seen,
                    [id.as_str()],
                    "the gate was asked about the card once"
                );
            }
        }
    }
}

/// The stop judge's hold, one table beside the question gate's. A fake judge only says whether to
/// hold a turn end and records what it was offered; the test plays the judge's verdict by hand
/// (`stop_verdict`), a message during the hold, the worker leaving, an archive, a Stop and the
/// deadline (shortened). Every wait is on a state the actor has already settled, never on a
/// sleep; every row ends with the turn finished exactly once, or dropped on purpose.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stop_judge_holds_a_turn_end_until_it_answers() {
    use crate::seams::{StopJudge, StopKind, StopOffer, StopOutcome, TurnEnd};
    use crate::types::ContextKind;

    struct FakeJudge {
        offer: StopOffer,
        ends: Mutex<Vec<TurnEnd>>,
        line: Option<String>,
    }
    impl StopJudge for FakeJudge {
        fn offer(&self, _: &str, end: &TurnEnd) -> StopOffer {
            self.ends.lock().unwrap().push(end.clone());
            self.offer
        }
        fn turn_line(&self, _: &str) -> Option<String> {
            self.line.clone()
        }
    }
    struct AfkText;
    impl crate::seams::AfkPolicy for AfkText {
        fn turn_text(&self, _: &str) -> Option<String> {
            Some("[AFK] away".into())
        }
        fn decide(
            &self,
            _: &str,
            _: &crate::types::Interaction,
            _: bool,
        ) -> crate::seams::AfkAction {
            crate::seams::AfkAction::Leave
        }
    }

    #[derive(Clone, Copy)]
    enum Then {
        /// No judge installed: the turn finishes at once.
        NoJudge,
        /// The judge passes: the same.
        Pass,
        Continue,
        Stop(StopKind, ContextKind),
        Done(Option<&'static str>),
        Failed,
        /// The hold ends by itself (shortened).
        Deadline,
        /// A message during the hold: the finish is announced first, the late verdict is false,
        /// and the next turn's end is a hold of its own.
        Send,
        WorkerExit,
        Archive,
        /// The user's Stop on the checked chat finishes it.
        StopWhileHeld,
        /// Never offered: an interrupted turn.
        Interrupted,
        /// Never offered: a Stop the user asked for while the turn ran.
        UserStopped,
        /// Never offered: a message waiting to go out when the turn ends.
        MessageWaiting,
        /// Never offered, and the chat keeps running: Claude has an unread steer.
        UnreadSteer,
        /// Never offered: a background agent is still running.
        AgentRunning,
    }
    use Then::*;
    let cases = [
        NoJudge,
        Pass,
        Continue,
        Stop(StopKind::Blocker, ContextKind::StopBlocker),
        Stop(StopKind::Limit, ContextKind::StopLimit),
        Stop(StopKind::Failure, ContextKind::StopFailure),
        Stop(StopKind::Asked, ContextKind::StopAsked),
        Done(Some("Run finished: 2 choices were made for you.")),
        Done(None),
        Failed,
        Deadline,
        Send,
        WorkerExit,
        Archive,
        StopWhileHeld,
        Interrupted,
        UserStopped,
        MessageWaiting,
        UnreadSteer,
        AgentRunning,
    ];

    fn completions(h: &H) -> usize {
        h.events("completion").len()
    }
    fn holds(h: &H) -> Vec<String> {
        h.col("SELECT chat_id FROM chat_stop_holds")
    }
    fn checking(h: &H, chat: &str) -> bool {
        h.rt.live_thread(chat)
            .is_some_and(|t| t.stop_check.is_some())
    }
    fn context_rows(h: &H) -> Vec<String> {
        h.col("SELECT json_extract(details, '$.contextKind') || ': ' || text FROM messages WHERE role = 'context'")
    }
    fn kind_name(k: ContextKind) -> String {
        serde_json::to_value(k)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }

    for then in cases {
        let mut lim = limits();
        if matches!(then, Deadline) {
            lim.stop_deadline = Duration::from_millis(150);
        }
        let mut h = harness_with(lim);
        let judge = Arc::new(FakeJudge {
            offer: if matches!(then, Pass) {
                StopOffer::Pass
            } else {
                StopOffer::Hold
            },
            ends: Mutex::new(Vec::new()),
            line: None,
        });
        if !matches!(then, NoJudge) {
            h.rt.set_stop_judge(judge.clone());
        }
        let (chat, _, mut w) = running_turn(&mut h).await;
        let ended = |j: &FakeJudge| j.ends.lock().unwrap().clone();

        // How the turn ends: most rows end it cleanly with a question in its last message.
        match then {
            Interrupted => {
                w.emit(json!({"type": "agent_settled", "outcome": "error", "error": "API error"}))
                    .await;
                h.until("the turn failed", |h| h.status(&chat) == "interrupted")
                    .await;
                assert!(
                    ended(&judge).is_empty(),
                    "an interrupted turn is never offered"
                );
                continue;
            }
            UserStopped => {
                let (api, c2) = (h.api.clone(), chat.clone());
                let stop = tokio::spawn(async move {
                    api.call(&main(), "chat_stop", json!({"chatId": c2})).await
                });
                w.expect("abort").await;
                w.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
                    .await;
                h.until("stopped", |h| h.status(&chat) == "stopped").await;
                stop.await.unwrap().ok();
                assert!(
                    ended(&judge).is_empty(),
                    "a Stop by the user is never offered"
                );
                assert!(holds(&h).is_empty());
                continue;
            }
            UnreadSteer => {
                let (steer, _) = h.send(&chat, "also this", "default").await;
                let p = w.expect("prompt").await;
                ack(&w, &p).await;
                h.until("the steer is accepted", |h| h.state(&steer) == "accepted")
                    .await;
                reply(&h, &w, "m1", "Shall I go ahead?").await;
                w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
                    .await;
                // The CLI opens its next turn on the steer by itself: the chat stays running.
                h.until("the settle was handled", |h| {
                    h.col("SELECT status FROM chats") == ["running"]
                })
                .await;
                assert!(
                    ended(&judge).is_empty(),
                    "a chat with an unread steer is not over"
                );
                assert_eq!(completions(&h), 0);
                continue;
            }
            MessageWaiting => {
                let (waiting, _) = h.send(&chat, "also this", "default").await;
                let _p = w.expect("prompt").await; // handed over, not yet read by Claude
                assert_ne!(h.state(&waiting), "accepted");
            }
            AgentRunning => {
                w.emit(json!({"type": "subagent_start", "toolCallId": "sa_1", "name": "background job"}))
                    .await;
                h.until("the agent is running", |h| {
                    h.rt.live_thread(&chat)
                        .is_some_and(|t| t.subagents_running == 1)
                })
                .await;
            }
            _ => {}
        }
        reply(&h, &w, "m1", "Shall I go ahead?").await;
        w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
            .await;

        match then {
            NoJudge | Pass | MessageWaiting | AgentRunning => {
                h.until("the turn finished", |h| completions(h) == 1).await;
                assert!(holds(&h).is_empty() && !checking(&h, &chat));
                let seen = ended(&judge);
                if matches!(then, Pass) {
                    assert_eq!(seen.len(), 1, "the judge was asked about the turn end once");
                } else {
                    assert!(seen.is_empty(), "this turn end is never offered");
                }
                continue;
            }
            _ => {}
        }

        // Everything else is held: the finish waits, the chat reads as being checked, and the
        // hold is on record for anything watching the chat.
        h.until("the turn end is held", |h| {
            ended(&judge).len() == 1 && checking(h, &chat)
        })
        .await;
        let end = ended(&judge).remove(0);
        assert_eq!(holds(&h), [chat.as_str()], "the hold is recorded");
        assert_eq!(h.status(&chat), "idle");
        assert_eq!(completions(&h), 0, "no finish while the judge checks");
        assert!(h.col("SELECT 1 FROM chat_completions").is_empty());

        match then {
            Continue => {
                assert!(h.rt.stop_verdict(&chat, &end.run_id, StopOutcome::Continue));
                assert!(holds(&h).is_empty() && !checking(&h, &chat));
                assert_eq!(completions(&h), 0, "carrying on drops the finish");
                assert!(h.col("SELECT 1 FROM chat_completions").is_empty());
            }
            Stop(kind, ck) => {
                let ok = h.rt.stop_verdict(
                    &chat,
                    &end.run_id,
                    StopOutcome::Stop {
                        kind,
                        line: "Blocker: needs a sign-in.".into(),
                    },
                );
                assert!(ok);
                h.until("the class row, then the finish", |h| completions(h) == 1)
                    .await;
                assert_eq!(
                    context_rows(&h),
                    [format!("{}: Blocker: needs a sign-in.", kind_name(ck))]
                );
                assert!(holds(&h).is_empty() && !checking(&h, &chat));
            }
            Done(line) => {
                let ok = h.rt.stop_verdict(
                    &chat,
                    &end.run_id,
                    StopOutcome::Done {
                        line: line.map(str::to_owned),
                    },
                );
                assert!(ok);
                h.until("the finish", |h| completions(h) == 1).await;
                let want: Vec<String> = line
                    .map(|l| format!("{}: {l}", kind_name(ContextKind::Gatekeeper)))
                    .into_iter()
                    .collect();
                assert_eq!(context_rows(&h), want);
            }
            Failed => {
                assert!(h.rt.stop_verdict(&chat, &end.run_id, StopOutcome::Failed));
                h.until("the finish", |h| completions(h) == 1).await;
                assert!(context_rows(&h).is_empty());
            }
            Deadline => {
                h.until("the hold ends by itself", |h| completions(h) == 1)
                    .await;
                assert!(holds(&h).is_empty() && !checking(&h, &chat));
            }
            Send => {
                let (nudge, _) = h.send(&chat, "go on", "default").await;
                let p = w.expect("prompt").await;
                assert_eq!(p["submissionId"], nudge.as_str());
                // The prompt left only after the earlier finish was written and announced.
                assert_eq!(completions(&h), 1);
                assert!(holds(&h).is_empty() && !checking(&h, &chat));
                assert!(
                    !h.rt.stop_verdict(&chat, &end.run_id, StopOutcome::Continue),
                    "the late verdict finds the hold gone"
                );
                // The next turn's end is held on its own, under its own run: the old verdict
                // still lands nowhere, the new one does.
                ack(&w, &p).await;
                w.emit(json!({"type": "agent_start"})).await;
                w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
                    .await;
                h.until("the next end is held", |h| {
                    ended(&judge).len() == 2 && checking(h, &chat)
                })
                .await;
                let second = ended(&judge).remove(1);
                assert_ne!(second.run_id, end.run_id);
                assert!(!h.rt.stop_verdict(&chat, &end.run_id, StopOutcome::Failed));
                assert!(
                    checking(&h, &chat),
                    "an earlier turn's verdict ends no later hold"
                );
                assert!(h
                    .rt
                    .stop_verdict(&chat, &second.run_id, StopOutcome::Continue));
                assert_eq!(
                    completions(&h),
                    1,
                    "one finish in all: the dropped one is silent"
                );
            }
            WorkerExit => {
                w.exit().await;
                h.until("the finish", |h| completions(h) == 1).await;
                assert!(holds(&h).is_empty() && !checking(&h, &chat));
                assert!(!h.rt.stop_verdict(&chat, &end.run_id, StopOutcome::Continue));
            }
            Archive => {
                h.call("chat_archive", json!({"chatId": chat}))
                    .await
                    .unwrap();
                h.until("the hold is gone", |h| {
                    holds(h).is_empty() && !checking(h, &chat)
                })
                .await;
                assert_eq!(completions(&h), 0, "an archived chat announces nothing");
                assert!(!h.rt.stop_verdict(&chat, &end.run_id, StopOutcome::Continue));
            }
            StopWhileHeld => {
                h.call("chat_stop", json!({"chatId": chat})).await.unwrap();
                h.until("the finish", |h| completions(h) == 1).await;
                assert!(holds(&h).is_empty() && !checking(&h, &chat));
                assert!(!h.rt.stop_verdict(&chat, &end.run_id, StopOutcome::Continue));
            }
            _ => unreachable!(),
        }
        if !matches!(then, Continue | Archive | Send) {
            // A verdict twice finishes nothing twice.
            assert!(!h.rt.stop_verdict(&chat, &end.run_id, StopOutcome::Failed));
            assert_eq!(completions(&h), 1);
        }
    }

    // The judge's own line rides each turn's context beside AFK's text.
    for (afk, line, want) in [
        (
            false,
            Some("[Helm] Mode changed."),
            Some("[Helm] Mode changed."),
        ),
        (
            true,
            Some("[Helm] Mode changed."),
            Some("[AFK] away\n\n[Helm] Mode changed."),
        ),
        (true, None, Some("[AFK] away")),
        (false, None, None),
    ] {
        let mut h = harness();
        if afk {
            h.rt.set_afk_policy(Arc::new(AfkText));
        }
        h.rt.set_stop_judge(Arc::new(FakeJudge {
            offer: StopOffer::Pass,
            ends: Mutex::new(Vec::new()),
            line: line.map(str::to_owned),
        }));
        let chat = h.create().await;
        h.send(&chat, "start", "default").await;
        let mut w = h.worker().await;
        w.ready().await;
        let p = w.expect("prompt").await;
        assert_eq!(p.get("context").and_then(Value::as_str), want);
    }
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
    assert_eq!(
        h.rt.live_thread(&chat).unwrap().live_tools[0]
            .summary
            .as_deref(),
        Some("Running a command"),
        "the live tool carries a few plain words about the call"
    );
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
    // A second call: the few words are stored with the row, so every later view of the call
    // shows the same phrase.
    w.emit(json!({"type": "tool_execution_start", "toolCallId": "tc3", "toolName": "Grep", "args": {"pattern": "HVAC.*filter"}, "stoppable": false}))
        .await;
    h.until("summary in the row", |h| {
        !h.col("SELECT json_extract(details, '$.toolSummary') FROM messages WHERE source_key = 't:tc3'")
            .is_empty()
    })
    .await;
    assert_eq!(
        h.col("SELECT json_extract(details, '$.toolSummary') FROM messages WHERE source_key = 't:tc3'"),
        ["Searching HVAC.*filter"]
    );
    w.emit(json!({"type": "tool_execution_end", "toolCallId": "tc3", "toolName": "Grep", "result": {"text": "2 files"}, "isError": false}))
        .await;
    // A picture a tool returned is kept as the chat's own image and listed on the tool's row; it is
    // linked to the call, so the composer never lists it as waiting. A bad picture is skipped.
    {
        use base64::Engine as _;
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(6u32.to_be_bytes());
        png.extend(4u32.to_be_bytes());
        png.extend([8, 6, 0, 0, 0]);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        w.emit(json!({"type": "tool_execution_start", "toolCallId": "tc5", "toolName": "Shot", "args": {}, "stoppable": false}))
            .await;
        w.emit(json!({"type": "tool_execution_end", "toolCallId": "tc5", "toolName": "Shot",
                      "result": {"text": "", "images": [{"mimeType": "image/png", "data": b64}, {"mimeType": "image/png", "data": "not base64!"}]},
                      "isError": false}))
            .await;
        h.until("tool picture", |h| {
            h.col("SELECT json_array_length(details, '$.images') FROM messages WHERE source_key = 't:tc5'") == ["1"]
        })
        .await;
        assert_eq!(
            h.col("SELECT json_extract(details, '$.images[0].width') FROM messages WHERE source_key = 't:tc5'"),
            ["6"]
        );
        assert_eq!(
            h.col("SELECT submission_id FROM chat_attachments WHERE name = 'Picture 1'"),
            ["tool:tc5"]
        );
    }
    // The files the command changed are kept as an entry of their own, with no row; a line with
    // no usable path is dropped.
    w.emit(json!({"type": "shell_changes", "toolCallId": "tc1", "paths": ["C:/p/a.txt", "", 5]}))
        .await;
    w.emit(json!({"type": "shell_changes", "toolCallId": "tc2", "paths": []}))
        .await;
    h.until("shell changes", |h| {
        !h.col("SELECT raw FROM entries WHERE entry_id = 's:tc1'")
            .is_empty()
    })
    .await;
    assert_eq!(
        h.col("SELECT json_extract(raw, '$.paths[0]') FROM entries WHERE entry_id = 's:tc1'"),
        ["C:/p/a.txt"]
    );
    assert_eq!(
        h.col("SELECT json_array_length(raw, '$.paths') FROM entries WHERE entry_id = 's:tc1'"),
        ["1"]
    );
    assert!(h
        .col("SELECT 1 FROM messages WHERE source_key LIKE 's:%'")
        .is_empty());
    assert!(h
        .col("SELECT 1 FROM entries WHERE entry_id = 's:tc2'")
        .is_empty());
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

/// A line added from another crate (the Orchestrator switch) lands in the transcript as a context
/// row, in order with the chat's own rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_from_another_crate_becomes_a_context_row() {
    let h = harness();
    let chat = h.create().await;
    h.rt.add_context_row(
        &chat,
        crate::types::ContextKind::Orchestrator,
        "You turned Orchestrator on for this chat.",
    );
    h.until("row", |h| {
        h.col("SELECT json_extract(details, '$.contextKind') FROM messages WHERE role = 'context'")
            == ["orchestrator"]
    })
    .await;
    assert_eq!(
        h.col("SELECT text FROM messages WHERE role = 'context'"),
        ["You turned Orchestrator on for this chat."]
    );
}

/// Three Claude chats start together while nothing is known of the sign-in: one opens and checks
/// it, the other two wait at their start with the reason `sign_in` (no worker starts for them),
/// and the first one's answer lets both go. A failed request on the way is no answer: it is
/// stored as an error row whose text is empty, with the error in its details. The answer is the
/// reply's first streamed words, not the end of the message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_start_checks_the_sign_in_and_its_answer_lets_the_waiting_ones_go() {
    let mut h = harness();
    *h.spawner.sign_in.lock().unwrap() = Some(Arc::new(helm_core::signin_gate::SignInGate::new()));
    let chats = [h.create().await, h.create().await, h.create().await];
    for c in &chats {
        h.send(c, "go", "default").await;
    }
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    let first = w.req.chat_id.clone();
    let others: Vec<String> = chats.iter().filter(|c| **c != first).cloned().collect();
    let waiting = |h: &H| {
        others.iter().all(|c| {
            h.status(c) == "starting"
                && h.col(&format!(
                    "SELECT COALESCE(queue_reason, 'x') FROM chat_activity WHERE chat_id = '{c}'"
                )) == ["sign_in"]
        })
    };
    h.until("the other two wait for the sign-in check", waiting)
        .await;
    let text = "API Error: 500 the request failed";
    w.emit(json!({"type": "message_end", "message": {"id": "err1", "role": "assistant", "content": [{"type": "text", "text": text}], "status": "error", "errorMessage": text}}))
        .await;
    h.until("the error row keeps its error in the details", |h| {
        h.col("SELECT text || '|' || json_extract(details, '$.errorMessage') FROM messages WHERE status = 'error'")
            == [format!("|{text}")]
    })
    .await;
    assert!(waiting(&h), "an error is no answer: they still wait");
    assert!(
        h.workers.try_recv().is_err(),
        "no worker for a waiting start"
    );
    w.emit(json!({"type": "message_start", "message": {"id": "m1", "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_update", "messageId": "m1", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}}))
        .await;
    let mut opened = Vec::new();
    for _ in 0..2 {
        let mut w = h.worker().await;
        w.ready().await;
        w.expect("prompt").await;
        opened.push(w.req.chat_id.clone());
    }
    opened.sort();
    let mut want = others.clone();
    want.sort();
    assert_eq!(opened, want, "the answer let both waiting starts go");
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
    // The maintenance tick hands every live chat the stored send behaviour, and a tick landing
    // mid-test would overwrite the short limit set straight on the live thread below with the
    // stored default. Park the tick; nothing else in this test is tick-driven.
    let mut h = harness_with(Limits {
        tick: Duration::from_secs(3600),
        ..limits()
    });
    let (chat, _, mut w) = running_turn(&mut h).await;
    // The setting itself floors the wait at 5 s; the test arms 1 s straight on the live thread
    // so the retry loop runs well inside the test's span.
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
    // The deliver follows a 1 s timer inside the starved actor task: wide window.
    let d1 = w
        .expect_within("deliver_now", Duration::from_secs(30))
        .await;
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

/// Beyond the active-chat cap a send is not refused: it is taken and waits at the chat's
/// start — visibly, with a record in the queue — and opens when a place frees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_beyond_the_active_cap_wait_in_the_queue_and_open_when_one_frees() {
    let mut l = limits();
    l.max_active = 1;
    let mut h = harness_with(l);
    let (busy, _, w) = running_turn(&mut h).await;
    let other = h.create().await;
    let (sid, _) = h.send(&other, "wait for me", "default").await;
    h.until(
        "the second start waits at its start, with its reason recorded",
        |h| {
            h.status(&other) == "starting"
                && h.state(&sid) == "saved"
                && h.col(&format!(
                "SELECT COALESCE(queue_reason, 'x') FROM chat_activity WHERE chat_id = '{other}'"
            )) == ["at_limit"]
                && h.col(&format!(
                    "SELECT reason FROM chat_dispatch_holds WHERE chat_id = '{other}'"
                )) == ["at_limit"]
        },
    )
    .await;
    // A second waiting chat must not count against the first: chats held at their start are
    // waiting, not working, so the freed place still goes to the oldest.
    let third = h.create().await;
    let (sid3, _) = h.send(&third, "me too", "default").await;
    h.until("the third start waits as well", |h| {
        h.status(&third) == "starting" && h.state(&sid3) == "saved"
    })
    .await;
    // The freed place takes it: no new send, the same submission opens the turn.
    w.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-q1"}))
        .await;
    let mut w2 = h.worker().await;
    w2.ready().await;
    let p = w2.expect("prompt").await;
    assert_eq!(p["submissionId"], sid.as_str());
    h.until("the hold is gone once it opens", |h| {
        h.col(&format!(
            "SELECT count(*) FROM chat_dispatch_holds WHERE chat_id = '{other}'"
        )) == ["0"]
            && h.col(&format!(
                "SELECT COALESCE(queue_reason, 'x') FROM chat_activity WHERE chat_id = '{other}'"
            )) == ["x"]
    })
    .await;
    // The chat that waited is working now and counts against the cap: the third stays waiting
    // behind it instead of opening beside it.
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert_eq!(
        h.col(&format!(
            "SELECT reason FROM chat_dispatch_holds WHERE chat_id = '{third}'"
        )),
        ["at_limit"],
        "a chat that waited and then opened still counts as working"
    );
    assert_eq!(h.status(&third), "starting");
    let _ = busy;
}

/// While the plan allowance reads used up, a new start waits at its start with that reason
/// recorded, and the window's own reset releases it — no new send. An interactive send instead
/// gets an answer in words and is never left waiting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_used_up_plan_allowance_holds_a_start_and_the_reset_releases_it() {
    use crate::submit::{host_send, HostSend};
    let mut h = harness();
    let seed = |h: &H, pct: f64, reset_in: i64| {
        let now = helm_core::now_ms();
        let body = json!({ "fiveHour": { "pct": pct, "at": now, "resetsAt": now + reset_in } });
        h.db.write(move |tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS plan_usage (\
                     id INTEGER PRIMARY KEY CHECK (id = 1), body TEXT NOT NULL, \
                     updated_at INTEGER NOT NULL);",
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO plan_usage(id, body, updated_at) VALUES (1, ?1, ?2)",
                rusqlite::params![body.to_string(), now],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
    };
    // A window that resets in four seconds: the wait is real but short.
    seed(&h, 96.0, 4_000);
    let chat = h.create().await;
    let (sid, _) = h.send(&chat, "hold me", "default").await;
    h.until("the start waits with the allowance as its reason", |h| {
        h.status(&chat) == "starting"
            && h.state(&sid) == "saved"
            && h.col(&format!(
                "SELECT reason FROM chat_dispatch_holds WHERE chat_id = '{chat}'"
            )) == ["claude_capacity"]
    })
    .await;
    h.until("the reset clears the hold", |h| {
        h.col(&format!(
            "SELECT count(*) FROM chat_dispatch_holds WHERE chat_id = '{chat}'"
        )) == ["0"]
    })
    .await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(
        p["submissionId"],
        sid.as_str(),
        "the same send opens the turn once the window reopens"
    );
    // A window that will not reopen for an hour: an interactive send is answered, not held.
    seed(&h, 96.0, 3_600_000);
    let remote = h.create().await;
    host_send(
        &h.rt,
        HostSend {
            chat_id: &remote,
            submission_id: "tg-1",
            text: "get the shopping list",
            origin: crate::types::SubmissionOrigin::Telegram,
        },
    )
    .unwrap();
    h.until("the interactive send is answered, not left waiting", |h| {
        h.state("tg-1") == "rejected" && h.status(&remote) == "idle"
    })
    .await;
    assert_eq!(
        h.col(&format!(
            "SELECT count(*) FROM chat_dispatch_holds WHERE chat_id = '{remote}'"
        )),
        ["0"],
        "an interactive start is never parked"
    );
}

/// The queue line, moved by hand: to the front, parked until released, and sent back. Against a
/// local server's cap of one, where every opening is a real turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_queue_line_can_be_reordered_parked_and_sent_back() {
    let mut h = harness_with(Limits {
        tick: Duration::from_millis(50),
        ..limits()
    });
    h.db.write(|tx| helm_core::prefs::set_generic(tx, "localChatCap", &json!(1)))
        .unwrap();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("m-one".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());
    let cwd = h.work.to_string_lossy().into_owned();
    let asked = |id: &str| -> bool { local.turns.lock().unwrap().iter().any(|t| t.chat_id == id) };
    let new_id = |r: Value| r["id"].as_str().unwrap().to_owned();
    let a = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    h.send(&a, "job a", "default").await;
    let mut wa = h.worker().await;
    wa.ready().await;
    let pa = wa.expect("prompt").await;
    ack(&wa, &pa).await;
    wa.emit(json!({"type": "agent_start"})).await;
    let b = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    let (sidb, _) = h.send(&b, "job b", "default").await;
    let c = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    let (sidc, _) = h.send(&c, "job c", "default").await;
    h.until("both waits are recorded", |h| {
        h.status(&b) == "starting"
            && h.state(&sidb) == "saved"
            && h.status(&c) == "starting"
            && h.state(&sidc) == "saved"
            && h.col("SELECT count(*) FROM chat_dispatch_holds") == ["2"]
    })
    .await;
    // C goes to the front of the line.
    crate::queue::action(&h.rt, &c, "top").unwrap();
    assert_eq!(
        h.col(&format!(
            "SELECT top FROM chat_dispatch_holds WHERE chat_id = '{c}'"
        )),
        ["1"]
    );
    // The freed place takes C, not the older B.
    wa.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-q-a"}))
        .await;
    h.until("the front of the line goes first", |_h| asked(&c))
        .await;
    assert!(
        !asked(&b),
        "the older send waits its turn behind the one at the front"
    );
    let mut wc = h.worker().await;
    wc.ready().await;
    let pc = wc.expect("prompt").await;
    assert_eq!(pc["submissionId"], sidc.as_str());
    ack(&wc, &pc).await;
    wc.emit(json!({"type": "agent_start"})).await;
    // Park B by hand: a freed place does not take it until it is released.
    crate::queue::action(&h.rt, &b, "hold").unwrap();
    wc.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-q-c"}))
        .await;
    h.until("C's turn ends", |h| h.status(&c) == "idle").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!asked(&b), "a hand-held chat waits past a free place");
    assert_eq!(
        h.col(&format!(
            "SELECT reason FROM chat_dispatch_holds WHERE chat_id = '{b}'"
        )),
        ["manual"]
    );
    crate::queue::action(&h.rt, &b, "release").unwrap();
    h.until("the released chat takes the free place", |_h| asked(&b))
        .await;
    let mut wb = h.worker().await;
    wb.ready().await;
    let pb = wb.expect("prompt").await;
    assert_eq!(pb["submissionId"], sidb.as_str());
    ack(&wb, &pb).await;
    wb.emit(json!({"type": "agent_start"})).await;
    // A running chat parked by hand has no wait to cancel: the cancel is refused and its turn
    // goes on; releasing the park is the way out.
    crate::queue::action(&h.rt, &b, "hold").unwrap();
    assert!(crate::queue::action(&h.rt, &b, "cancel").is_err());
    assert_ne!(
        h.status(&b),
        "idle",
        "a live turn is not ended by the queue"
    );
    assert_eq!(
        h.col(&format!(
            "SELECT count(*) FROM chat_dispatch_holds WHERE chat_id = '{b}'"
        )),
        ["1"]
    );
    crate::queue::action(&h.rt, &b, "release").unwrap();
    // Cancel: a waiting send goes back to its sender.
    let d = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    let (sidd, _) = h.send(&d, "job d", "default").await;
    h.until("D waits in the line", |h| {
        h.status(&d) == "starting"
            && h.state(&sidd) == "saved"
            && h.col(&format!(
                "SELECT count(*) FROM chat_dispatch_holds WHERE chat_id = '{d}'"
            )) == ["1"]
    })
    .await;
    crate::queue::action(&h.rt, &d, "cancel").unwrap();
    h.until("the cancelled send is given back", |h| {
        h.state(&sidd) == "rejected"
            && h.status(&d) == "idle"
            && h.col(&format!(
                "SELECT count(*) FROM chat_dispatch_holds WHERE chat_id = '{d}'"
            )) == ["0"]
    })
    .await;
}

/// Withdraw: the text comes back as a newer draft revision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withdrawn_steer_returns_as_a_newer_draft() {
    use base64::Engine as _;
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(10u32.to_be_bytes());
    png.extend(10u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let put = h
        .call(
            "attachment_put",
            json!({"chatId": chat, "name": "back.png", "mime": "image/png", "bytes": b64}),
        )
        .await
        .unwrap();
    let img = put["attachmentId"].as_str().unwrap().to_owned();
    // One the user takes out of the composer is gone for good, the stored file with it: it is not
    // staged again, and a second discard finds nothing.
    let extra = h
        .call(
            "attachment_put",
            json!({"chatId": chat, "name": "extra.png", "mime": "image/png", "bytes": b64}),
        )
        .await
        .unwrap()["attachmentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let note = h.work.join("discard-me.md");
    std::fs::write(&note, "x").unwrap();
    let by_ref = h
        .call(
            "attachment_ref",
            json!({"chatId": chat, "kind": "path", "target": note.to_string_lossy()}),
        )
        .await
        .unwrap()["attachmentId"]
        .as_str()
        .unwrap()
        .to_owned();
    // Pasted text is saved as a file under the chat's attachments and attached by reference.
    let pasted = h
        .call(
            "attachment_text_put",
            json!({"chatId": chat, "name": "pasted-2-lines.txt", "text": "one\ntwo"}),
        )
        .await
        .unwrap();
    assert_eq!(pasted["kind"], "file");
    assert_eq!(pasted["name"], "pasted-2-lines.txt");
    assert!(pasted["target"]
        .as_str()
        .unwrap()
        .ends_with("pasted-2-lines.txt"));
    let pasted = pasted["attachmentId"].as_str().unwrap().to_owned();
    assert_eq!(
        std::fs::read_to_string(
            h.profile
                .path("attachments")
                .join(&pasted)
                .join("pasted-2-lines.txt")
        )
        .unwrap(),
        "one\ntwo"
    );
    assert!(h.profile.path("attachments").join(&extra).is_file());
    for id in [&extra, &by_ref, &pasted] {
        let r = h
            .call(
                "attachment_discard",
                json!({"chatId": chat, "attachmentId": id}),
            )
            .await
            .unwrap();
        assert_eq!(r["discarded"], true);
        let again = h
            .call(
                "attachment_discard",
                json!({"chatId": chat, "attachmentId": id}),
            )
            .await
            .unwrap();
        assert_eq!(again["discarded"], false);
    }
    assert!(!h.profile.path("attachments").join(&extra).exists());
    let left = h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(left["images"].as_array().unwrap().len(), 1);
    assert!(left["refs"].as_array().unwrap().is_empty());
    // It waits in the chat's unstaged set until the steer takes it.
    let staged = h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(staged["images"][0]["id"], img.as_str());
    let steer = uuid::Uuid::new_v4().to_string();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": steer, "text": "take me back", "action": "steer",
               "draftRevision": 0, "attachments": [img.clone()]}),
    )
    .await
    .unwrap();
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    h.until("accepted", |h| h.state(&steer) == "accepted").await;
    // A waiting bubble shows its picture, and the accepted send has taken it out of the
    // unstaged set.
    let page = h
        .call("transcript_page", json!({"chatId": chat}))
        .await
        .unwrap();
    let entry = page["pending"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == steer.as_str())
        .expect("the steer waits in the list");
    assert_eq!(entry["images"][0]["id"], img.as_str());
    assert!(
        h.call("attachment_list", json!({"chatId": chat}))
            .await
            .unwrap()["images"]
            .as_array()
            .unwrap()
            .is_empty(),
        "sent: out of the unstaged set"
    );
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
    // The picture comes back with the text and waits unstaged for a new send.
    let list = h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(
        list["images"][0]["id"],
        img.as_str(),
        "withdrawn: the image returns with the draft"
    );
}

/// A send whose worker cannot start is `rejected` and stays in `pending` so the user can recover it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_that_could_not_start_stays_recoverable() {
    use base64::Engine as _;
    let h = harness();
    let chat = h.create().await;
    let note = h.work.join("lost-note.md");
    std::fs::write(&note, "x").unwrap();
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(10u32.to_be_bytes());
    png.extend(10u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let put = h
        .call(
            "attachment_put",
            json!({"chatId": chat, "name": "lost.png", "mime": "image/png",
                   "bytes": base64::engine::general_purpose::STANDARD.encode(&png)}),
        )
        .await
        .unwrap();
    let img = put["attachmentId"].as_str().unwrap().to_owned();
    let rf = h
        .call(
            "attachment_ref",
            json!({"chatId": chat, "kind": "path", "target": note.to_string_lossy()}),
        )
        .await
        .unwrap();
    let path_ref = rf["attachmentId"].as_str().unwrap().to_owned();
    // The fake spawner fails every start.
    *h.spawner.fail.lock().unwrap() = Some("Node.js was not found.".into());
    let sid = uuid::Uuid::new_v4().to_string();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": sid, "text": "lost?", "action": "default",
               "draftRevision": 0, "attachments": [img.clone(), path_ref.clone()]}),
    )
    .await
    .unwrap();
    h.until("rejected", |h| h.state(&sid) == "rejected").await;
    let page = h
        .call("transcript_page", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(page["pending"][0]["id"], sid.as_str());
    assert_eq!(page["pending"][0]["state"], "rejected");
    // The refused send's bubble still shows its picture and its file.
    assert_eq!(page["pending"][0]["images"][0]["id"], img.as_str());
    assert_eq!(page["pending"][0]["refs"][0]["name"], "lost-note.md");
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
    // Both attachments wait unstaged again, so the recovered draft can carry them.
    let list = h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(list["images"][0]["id"], img.as_str());
    assert_eq!(
        list["refs"][0]["target"],
        note.to_string_lossy().to_string().as_str()
    );
    // A send with the same ids is accepted again (the spawner still fails, so it is rejected
    // the same way, but the ids are not refused as already sent).
    let sid2 = uuid::Uuid::new_v4().to_string();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": sid2, "text": "lost?", "action": "default",
               "draftRevision": 0, "attachments": [img, path_ref]}),
    )
    .await
    .unwrap();
    h.until("rejected again", |h| h.state(&sid2) == "rejected")
        .await;
    assert!(
        h.call("attachment_list", json!({"chatId": chat}))
            .await
            .unwrap()["images"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the re-send took them out again"
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

/// The outage watch's probe, always answered.
struct Reached;

impl crate::outage::Probe for Reached {
    fn reach(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
        Box::pin(async { true })
    }
}

/// A worker ended from outside mid-turn: the chat says so (not Node's warning), the exit is
/// logged as a crash, and the outage watch asks the chat to carry on once the wait is over,
/// resuming the conversation with the fixed prompt rather than re-running anything. A chat the
/// user stopped, whose worker died the same way, is never carried on, and neither is one whose
/// worker died before Claude read the turn's prompt (there is nothing to continue).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_killed_mid_turn_is_carried_on_after_the_wait_but_a_stopped_one_is_not() {
    // Every end here is the test's own kill line, never Helm's stop deadline: push that deadline
    // out so the scripted kill is the only thing that ends a worker, whatever the machine does.
    let mut h = harness_with(Limits {
        stop: Duration::from_secs(30),
        ..limits()
    });
    let (chat, sid, w) = running_turn(&mut h).await;
    w.emit(json!({"type": "submission_delivered", "submissionId": sid}))
        .await;
    h.until("delivered", |h| h.state(&sid) == "delivered").await;
    w.killed().await;
    h.until("interrupted", |h| h.status(&chat) == "interrupted")
        .await;
    assert_eq!(
        h.col(&format!("SELECT error FROM chats WHERE id = '{chat}'")),
        ["The worker process was ended from outside Helm while Claude Code was still running (exit code 1)."]
    );
    assert_eq!(h.state(&sid), "delivered");
    let exit_line = |h: &H, chat: &str| -> Value {
        let log = std::fs::read_to_string(h.profile.path("diagnostics/worker-exits.log")).unwrap();
        log.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|l| l["kind"] == "exit" && l["chatId"] == chat)
            .unwrap()
    };
    let line = exit_line(&h, &chat);
    assert_eq!(
        (&line["crash"], &line["jobActive"], &line["hostAsked"]),
        (&json!(true), &json!(1), &json!(false))
    );

    // This chat's prompt was taken but never read: the message stays unsent, to recover.
    let (unread, unread_sid, w4) = running_turn(&mut h).await;
    w4.killed().await;
    h.until("unread chat interrupted", |h| {
        h.status(&unread) == "interrupted"
    })
    .await;
    assert_eq!(h.state(&unread_sid), "uncertain");
    let line = exit_line(&h, &unread);
    assert_eq!(
        (&line["crash"], &line["promptRead"], &line["hostAsked"]),
        (&json!(false), &json!(false), &json!(false))
    );

    // The second chat is stopped by the user, and its worker dies while it stops.
    let (stopped, _, mut w2) = running_turn(&mut h).await;
    let api = h.api.clone();
    let c2 = stopped.clone();
    let stop =
        tokio::spawn(async move { api.call(&main(), "chat_stop", json!({"chatId": c2})).await });
    w2.expect("abort").await;
    w2.killed().await;
    stop.await.unwrap().unwrap();
    h.until("stopped chat interrupted", |h| {
        h.status(&stopped) == "interrupted"
    })
    .await;
    assert_eq!(exit_line(&h, &stopped)["hostReason"], "stop");

    // A stop that settles, then a worker that ends itself: still an end Helm asked for.
    let (settled, _, mut w6) = running_turn(&mut h).await;
    let api = h.api.clone();
    let c6 = settled.clone();
    let stop =
        tokio::spawn(async move { api.call(&main(), "chat_stop", json!({"chatId": c6})).await });
    w6.expect("abort").await;
    w6.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
        .await;
    stop.await.unwrap().unwrap();
    w6.exit().await;
    h.until("settled chat's exit logged", |h| {
        std::fs::read_to_string(h.profile.path("diagnostics/worker-exits.log"))
            .is_ok_and(|l| l.contains(&format!("\"chatId\":\"{settled}\"")))
    })
    .await;
    assert_eq!(exit_line(&h, &settled)["hostReason"], "stop");

    // A crashed chat the user starts writing in is theirs: its crash is forgotten at the next
    // look, so clearing the draft later never brings a resume back.
    let (drafted, drafted_sid, w5) = running_turn(&mut h).await;
    w5.emit(json!({"type": "submission_delivered", "submissionId": drafted_sid}))
        .await;
    h.until("drafted delivered", |h| {
        h.state(&drafted_sid) == "delivered"
    })
    .await;
    w5.killed().await;
    h.until("drafted chat interrupted", |h| {
        h.status(&drafted) == "interrupted"
    })
    .await;
    assert_eq!(exit_line(&h, &drafted)["crash"], true);
    h.db.write(|tx| {
        tx.execute(
            "UPDATE chats SET draft = 'my own next step' WHERE id = ?1",
            [&drafted],
        )
    })
    .unwrap();

    let watch = crate::outage::OutageWatch::with_probe(h.rt.clone(), Arc::new(Reached));
    // Looks are timed from the crash itself, so a slow machine cannot make a wait look over.
    let now = h.rt.sh.exits.recent_crashes(helm_core::now_ms())[&chat].at_ms;
    let prompts = |h: &H, chat: &str| {
        h.col(&format!(
            "SELECT text FROM submissions WHERE chat_id = '{chat}' ORDER BY created_at, rowid"
        ))
    };
    watch.look_at(now).await;
    assert!(!h.rt.sh.exits.recent_crashes(now).contains_key(&drafted));
    h.db.write(|tx| tx.execute("UPDATE chats SET draft = '' WHERE id = ?1", [&drafted]))
        .unwrap();
    watch.look_at(now + 20_000).await;
    assert_eq!(prompts(&h, &chat), ["start"], "not before 30 s");
    watch.look_at(now + 31_000).await;
    let mut w3 = h.worker().await;
    w3.ready().await;
    let p = w3.expect("prompt").await;
    assert_eq!(p["text"], crate::outage::WORKER_EXIT_CONTINUATION_TEXT);
    assert_eq!(
        prompts(&h, &chat),
        ["start", crate::outage::WORKER_EXIT_CONTINUATION_TEXT]
    );
    // Long after: still nothing for the stopped or the unread chat, and no worker for either.
    watch.look_at(now + 30 * 60_000).await;
    assert_eq!(prompts(&h, &stopped), ["start"]);
    assert_eq!(prompts(&h, &unread), ["start"]);
    assert_eq!(prompts(&h, &drafted), ["start"]);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), h.workers.recv())
            .await
            .is_err(),
        "no worker for the stopped chat"
    );
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
    // The session's last recorded running totals: a resumed Claude Code starts from them.
    h.db.write(|tx| {
        tx.execute(
            "INSERT INTO chat_turn_usage(chat_id, entry_id, timestamp, native_session, input_tokens, output_tokens,
                 cache_read_tokens, cache_write_tokens, cost_usd, running)
             VALUES (?1, 's:old', 1, 'native-7', 0, 0, 0, 0, 1.0,
                 '{\"inputTokens\":100,\"outputTokens\":10,\"cacheReadTokens\":1000,\"cacheWriteTokens\":10,\"costUsd\":1.0}')",
            [&chat],
        )
    })
    .unwrap();
    let mut w = h.worker().await;
    assert_eq!(w.req.resume_session_id.as_deref(), Some("native-7"));
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["text"], crate::submit::CONTINUATION_TEXT);
    assert!(
        w.no_command_for(Duration::from_millis(200)).await.is_none(),
        "once"
    );
    // Turn shares: measured from the restored totals; then totals above the saved ones in every
    // field but grown by fewer tokens than the turn's own, so a fresh start (nothing was restored)
    // and all of them are new; then the totals drop (a /clear), so all of them are new; then a
    // result with no running totals counts its own tokens only.
    let main =
        json!({"inputTokens": 1, "outputTokens": 2, "cacheReadTokens": 3, "cacheWriteTokens": 4});
    let mut big = main.clone();
    big["inputTokens"] = json!(20);
    for (running, main) in [
        (
            json!({"inputTokens": 150, "outputTokens": 30, "cacheReadTokens": 1500, "cacheWriteTokens": 20, "costUsd": 1.25}),
            &main,
        ),
        (
            json!({"inputTokens": 160, "outputTokens": 32, "cacheReadTokens": 1505, "cacheWriteTokens": 22, "costUsd": 1.3}),
            &big,
        ),
        (
            json!({"inputTokens": 5, "outputTokens": 5, "cacheReadTokens": 5, "cacheWriteTokens": 5, "costUsd": 0.1}),
            &main,
        ),
        (json!(null), &main),
    ] {
        let mut e = main.clone();
        e["type"] = json!("usage");
        e["costUsd"] = json!(9.0);
        e["running"] = running;
        w.emit(e).await;
    }
    let shares = |h: &H| {
        h.col(&format!(
        "SELECT input_tokens || '/' || output_tokens || '/' || cache_read_tokens || '/' || cache_write_tokens || '/' || cost_usd
         FROM chat_turn_usage WHERE chat_id = '{chat}' AND entry_id != 's:old' ORDER BY timestamp, rowid"
    ))
    };
    h.until("four turns recorded", |h| shares(h).len() == 4)
        .await;
    assert_eq!(
        shares(&h),
        [
            "50/20/500/10/0.25",
            "160/32/1505/22/1.3",
            "5/5/5/5/0.1",
            "1/2/3/4/0.0"
        ]
    );
    let origin = h.col("SELECT origin FROM submission_meta");
    assert_eq!(
        origin,
        ["app"],
        "the restart continuation is the app's own send"
    );
}

/// Messages an interrupted or closed Helm never handed over are sent at once on the next start,
/// never held: in a chat with no continuation the first opens a turn and the rest are read as it
/// works; in a chat that gets a continuation they all follow it as steers. A row an older version
/// left in its retired waiting state reads the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_hands_over_what_was_never_sent() {
    let mut h = harness();
    let db = h.db.clone();
    let seed = move |chat: &str, ids: &[&str]| {
        db.write(|tx| {
            for (i, id) in ids.iter().enumerate() {
                tx.execute(
                    "INSERT INTO submissions(id, chat_id, text, mode, state, created_at)
                     VALUES (?1, ?2, ?1, 'steer', 'saved', ?3)",
                    rusqlite::params![id, chat, i as i64],
                )?;
                tx.execute(
                    "INSERT INTO submission_meta(submission_id) VALUES (?1)",
                    [id],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
    };
    // No continuation: the first message starts the turn, the second is read as it works.
    let plain = h.create().await;
    seed(&plain, &["p1", "p2"]);
    h.db.write(|tx| {
        tx.execute(
            "UPDATE submissions SET state = 'queued' WHERE id = 'p2'",
            [],
        )
    })
    .unwrap();
    h.rt.start_continuations();
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], "p1");
    assert!(p.get("streamingBehavior").is_none());
    ack(&w, &p).await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], "p2");
    assert_eq!(p["streamingBehavior"], "steer");
    // A continuation goes first; what was waiting follows it as steers.
    let resumed = h.create().await;
    seed(&resumed, &["r1"]);
    h.db.write(|tx| crate::schema::record_continuation(tx, &resumed, 1))
        .unwrap();
    h.rt.start_continuations();
    let mut w2 = h.worker().await;
    w2.ready().await;
    let p = w2.expect("prompt").await;
    assert_eq!(p["text"], crate::submit::CONTINUATION_TEXT);
    let p = w2.expect("prompt").await;
    assert_eq!(p["submissionId"], "r1");
    assert_eq!(p["streamingBehavior"], "steer");
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

/// A chat that has not started is offered its project's startup text with its first message: the
/// text rides the prompt ahead of the rest of the context, one visible line says so, a roll-over
/// successor's predecessor is passed on, and a chat that has run is never asked again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_chats_first_message_carries_the_startup_text_and_a_started_chat_is_not_asked() {
    use crate::types::SendOrigin;
    struct Seam(Mutex<Vec<(String, Option<String>)>>);
    impl crate::seams::StartupSeam for Seam {
        fn first_send(
            &self,
            _tx: &rusqlite::Transaction<'_>,
            _chat_id: &str,
            submission_id: &str,
            predecessor: Option<&str>,
        ) -> rusqlite::Result<Option<crate::seams::StartupInjection>> {
            self.0
                .lock()
                .unwrap()
                .push((submission_id.to_owned(), predecessor.map(Into::into)));
            Ok(Some(crate::seams::StartupInjection {
                context: "Startup: make a worktree.".into(),
                row: "Startup instructions were added to this chat".into(),
            }))
        }
    }
    let mut h = harness();
    let chat = h.create().await;
    let seam = Arc::new(Seam(Mutex::new(vec![])));
    h.rt.set_startup_seam(seam.clone());
    h.rt.db()
        .write(|tx| {
            tx.execute(
                "INSERT INTO chat_succession(chat_id, predecessor_id) VALUES (?1, 'old-chat')",
                [&chat],
            )
        })
        .unwrap();
    let builder = crate::seams::SendContext(Arc::new(Ctx(Mutex::new(vec![]))));
    let rt = h.rt.clone();
    let (c2, b2) = (chat.clone(), builder.clone());
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
    assert_eq!(
        p["context"], "Startup: make a worktree.\n\nLooking at: src/a.rs",
        "the startup text comes first"
    );
    assert_eq!(
        h.col("SELECT text FROM messages WHERE role = 'context' AND json_extract(details, '$.contextKind') = 'startup'"),
        ["Startup instructions were added to this chat"]
    );
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    let rt = h.rt.clone();
    let c2 = chat.clone();
    tokio::task::spawn_blocking(move || {
        crate::submit::chat_send(&rt, send_args(&c2, SendOrigin::Action, json!({})), None)
    })
    .await
    .unwrap()
    .unwrap();
    let p = w.expect("prompt").await;
    assert!(
        p.get("context").is_none(),
        "a chat that has run carries no startup text"
    );
    assert_eq!(
        seam.0
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.1.clone())
            .collect::<Vec<_>>(),
        [Some("old-chat".to_owned())],
        "asked once, with the predecessor"
    );
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
    // Wait for the approval's own row, not for any badge of 1: a badge can also rise from a
    // different group, and matching on the number alone let a stray event end the wait early.
    h.until("needs you published", |h| {
        h.events("attention").last().is_some_and(|e| {
            e["summary"]["needsYou"]
                .as_array()
                .is_some_and(|v| v.iter().any(|r| r["kind"] == "approval"))
        })
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
    // The approval leaves Needs you; the clean finish is held in the finished group instead, not
    // listed as needing attention.
    h.until("needs you cleared", |h| {
        h.events("attention").last().is_some_and(|e| {
            e["summary"]["busy"] == 0
                && e["summary"]["needsYou"]
                    .as_array()
                    .is_some_and(|v| v.is_empty())
                && e["summary"]["unread"]
                    .as_array()
                    .is_some_and(|v| v.is_empty())
                && e["summary"]["finished"][0]["reason"] == "Finished; not read yet"
        })
    })
    .await;
}

/// A clean finish writes one completion, announced once, and holds the chat in the finished
/// group without counting it; the next turn drops it.
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
    assert_eq!(a["badge"], 0, "a finished, unread chat is not counted");
    assert!(a["unread"].as_array().unwrap().is_empty(), "and not listed");
    assert_eq!(a["finished"][0]["chatId"], chat.as_str());
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

/// Acknowledging the completion takes the chat out of the finished group, also when a summary
/// read lands between the write and the writer's own publish (the read must publish what it
/// found, or the event-only listeners such as the toast withdrawals never hear of it).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledging_the_completion_clears_the_finished_row() {
    let mut h = harness();
    let (chat, _, mut w) = running_turn(&mut h).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("completion", |h| !h.events("completion").is_empty())
        .await;
    h.until("finished", |h| {
        h.events("attention").last().is_some_and(|e| {
            e["summary"]["finished"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
        })
    })
    .await;
    let k = h.events("completion")[0]["completion"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.call("completion_ack", json!({"chatId": chat, "completionId": k}))
        .await
        .unwrap();
    h.until("finished cleared", |h| {
        h.events("attention").last().is_some_and(|e| {
            e["summary"]["finished"]
                .as_array()
                .is_some_and(|v| v.is_empty())
        })
    })
    .await;
    // A second finish, marked read behind the publisher's back, then read through attention_get.
    h.send(&chat, "again", "default").await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("finished again", |h| {
        h.events("attention").last().is_some_and(|e| {
            e["summary"]["finished"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
        })
    })
    .await;
    h.db.write(|tx| tx.execute("UPDATE chat_completions SET read = 1", []))
        .unwrap();
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert!(
        a["finished"].as_array().unwrap().is_empty(),
        "a read completion is not held"
    );
    let last = h.events("attention").last().cloned().unwrap();
    assert_eq!(
        (&last["summary"]["badge"], &last["summary"]["revision"]),
        (&json!(0), &a["revision"]),
        "the read published the change it found"
    );
}

/// Archiving a running chat is immediate: the row is archived at once, the turn is stopped in the
/// background, a late event from it changes nothing the user sees, and nothing still waiting is
/// sent. An idle chat's worker is simply retired. Both stop deadlines are parked far out here:
/// every end in this scenario is the scripted worker's own settle line, never Helm's kill timer,
/// so no scheduling delay can turn one path of this test into the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_takes_a_running_chat_at_once_and_a_dying_worker_cannot_bring_it_back() {
    let mut h = harness_with(Limits {
        stop: Duration::from_secs(30),
        archive_stop: Duration::from_secs(30),
        ..limits()
    });
    // A worker that settles late, after the archive: its completion must not be announced.
    let (chat, _, mut w) = running_turn(&mut h).await;
    // A message Claude has not read yet (a steer waiting for the next boundary).
    let (unread, _) = h.send(&chat, "later", "default").await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    h.until("accepted", |h| h.state(&unread) == "accepted")
        .await;
    h.db.write(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES ('late', ?1, 't', 'saved', 9)",
            [&chat],
        )
    })
    .unwrap();
    let archived = h
        .call("chat_archive", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(archived["archived"], true, "no wait for the stop");
    // A Stop landing together with the archive must not send what is waiting: ask for it before
    // the worker settles, so both reach the actor while its stop is under way.
    let (api, c2) = (h.api.clone(), chat.clone());
    let esc =
        tokio::spawn(async move { api.call(&main(), "chat_stop", json!({"chatId": c2})).await });
    w.expect("abort").await;
    // Settle at once when the abort command reaches us: both reach the actor while its stop is
    // under way, and a sleep here would only race the scheduling of this test's own task.
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("stopped", |h| h.status(&chat) == "stopped").await;
    esc.await.unwrap().ok();
    w.expect("shutdown").await;
    assert_eq!(h.col("SELECT archived FROM chats"), ["1"]);
    assert_eq!(
        h.state("late"),
        "uncertain",
        "an archive voids a saved message too"
    );
    assert!(h.events("completion").is_empty(), "no notification");
    assert!(h.col("SELECT chat_id FROM chat_completions").is_empty());
    assert!(h.col("SELECT chat_id FROM unread_marks").is_empty());
    assert_eq!(
        h.state(&unread),
        "uncertain",
        "the unread message reads not delivered and is not sent"
    );
    assert!(
        h.rt.attention().unwrap().needs_you.is_empty(),
        "nothing to attend to"
    );
    // Undo restores the chat as it stands, not the stopped turn.
    h.call("chat_restore", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(h.col("SELECT archived FROM chats"), ["0"]);
    assert_eq!(h.status(&chat), "stopped");
    assert_eq!(
        h.state(&unread),
        "uncertain",
        "recoverable after the restore"
    );

    // An idle chat's worker is retired.
    let (chat3, _, mut w3) = running_turn(&mut h).await;
    w3.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat3) == "idle").await;
    h.call("chat_archive", json!({"chatId": chat3}))
        .await
        .unwrap();
    w3.expect("shutdown").await;
}

/// Undo inside the archive window: the short kill deadline is dropped and the ordinary stop
/// deadline swapped in, so the chat is not ended with the timeout note while its worker is still
/// winding down. The two limits are set so the reading is exact: at the check the archive
/// deadline (8 s) has certainly passed, while the swapped-in stop deadline (60 s from the
/// restore) certainly has not — the only way the chat survives untouched is the deadline being
/// dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undo_inside_the_archive_window_drops_the_short_kill_deadline() {
    let mut h = harness_with(Limits {
        stop: Duration::from_secs(60),
        archive_stop: Duration::from_secs(8),
        ..limits()
    });
    let (chat, _, mut w) = running_turn(&mut h).await;
    let archived_at = std::time::Instant::now();
    h.call("chat_archive", json!({"chatId": chat}))
        .await
        .unwrap();
    w.expect("abort").await;
    h.call("chat_restore", json!({"chatId": chat}))
        .await
        .unwrap();
    // The restore only says something if it was asked for while the window was still open; on a
    // machine so loaded that even this lands late, the scenario proves nothing and is skipped.
    let in_window = archived_at.elapsed() < Duration::from_millis(4_000);
    tokio::time::sleep(Duration::from_millis(8_200)).await; // past archive_stop
    let status = h.status(&chat);
    if in_window {
        assert_ne!(
            status, "interrupted",
            "the archive deadline does not fire after a restore"
        );
    }
    w.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
        .await;
    if in_window {
        h.until("stopped", |h| h.status(&chat) == "stopped").await;
    }
}

/// A worker that never answers the stop is killed after the archive stop limit — and the kill is
/// not an error. The window is deliberately tiny: the scripted worker stays silent, so there is
/// nothing for the deadline to race with, however late the machine schedules.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_that_never_answers_the_stop_is_killed_at_the_archive_limit() {
    let mut h = harness_with(Limits {
        archive_stop: Duration::from_millis(300),
        ..limits()
    });
    let (chat, _, mut w) = running_turn(&mut h).await;
    h.call("chat_archive", json!({"chatId": chat}))
        .await
        .unwrap();
    w.expect("abort").await;
    h.until("killed", |h| h.status(&chat) == "stopped").await;
    assert_eq!(
        h.col(&format!("SELECT error FROM chats WHERE id = '{chat}'")),
        ["NULL"],
        "an archive-time stop is not an error"
    );
    assert!(h.events("completion").is_empty());
}

/// A busy chat reports a live state (generating, tool, waitingUser, silent) that the tick
/// maintains without asking any model. Only the silent one is flagged, and when it is, the flag,
/// the attention row and a note to a busy parent chat all say the same thing (at most one note
/// per child per hour, and none to an idle parent: a timer starts no model call); a running tool
/// call and a card waiting on the person never raise it, and the next output ends the whole
/// thing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_running_chat_shows_as_stalled_until_output() {
    let mut h = harness_with(Limits {
        tick: Duration::from_millis(50),
        ..limits()
    });
    // The parent is mid-turn on a tool call, which is never silent, so only the child is flagged.
    let (parent, _, pw) = running_turn(&mut h).await;
    pw.emit(json!({"type": "tool_execution_start", "toolCallId": "pt", "toolName": "Bash", "args": {"command": "sleep"}, "stoppable": true}))
        .await;
    // The silent window starts parked (the harness default): on a loaded machine the start-up
    // itself can outlast a short window, which would let the first observed state be `silent`
    // rather than `generating`. The window is shortened once the turn is demonstrably open.
    let (chat, _, w) = running_turn(&mut h).await;
    h.db.write(|tx| {
        tx.execute(
            "UPDATE chats SET parent_chat_id = ?1 WHERE id = ?2",
            [&parent, &chat],
        )
    })
    .unwrap();
    let flag = |h: &H| {
        h.col(&format!(
            "SELECT stalled FROM chat_activity WHERE chat_id = '{chat}'"
        ))
    };
    let state = |h: &H| -> String {
        serde_json::from_str::<Value>(
            &h.col(&format!(
                "SELECT COALESCE(state, 'null') FROM chat_activity WHERE chat_id = '{chat}'"
            ))[0],
        )
        .unwrap()["kind"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(flag(&h), ["0"], "not flagged while the window is parked");
    h.until("generating", |h| state(h) == "generating").await;
    // One minute is 100 ms in the test limits: from here the silent window is a second.
    h.db.write(|tx| helm_core::prefs::set_generic(tx, "stallMinutes", &json!(10)))
        .unwrap();

    // A tool call genuinely running: the state names it, and it is never silent no matter how
    // long it runs.
    w.emit(json!({"type": "tool_execution_start", "toolCallId": "tc1", "toolName": "Bash", "args": {"command": "cargo build"}, "stoppable": true}))
        .await;
    h.until("tool", |h| state(h) == "tool").await;
    tokio::time::sleep(Duration::from_millis(1150)).await;
    assert_eq!(flag(&h), ["0"], "a running tool call is not a silent");
    assert_eq!(state(&h), "tool", "and it keeps its state");
    w.emit(json!({"type": "tool_execution_end", "toolCallId": "tc1", "toolName": "Bash", "result": {"text": "done"}, "isError": false}))
        .await;

    // A card waiting on the person: a different state, and equally not a silent.
    w.emit(json!({"type": "extension_ui_request", "id": "ui-1", "method": "select", "attention": "approval",
                  "title": "Run ls?", "options": ["Allow once", "Deny"]}))
        .await;
    h.until("waitingUser", |h| state(h) == "waitingUser").await;
    tokio::time::sleep(Duration::from_millis(1150)).await;
    assert_eq!(flag(&h), ["0"], "waiting on the person is not a silent");
    h.call(
        "interaction_answer",
        json!({"chatId": chat, "requestId": "ui-1", "value": "Allow once"}),
    )
    .await
    .unwrap();

    // Now nothing is running and nothing answers: the silent arrives, and it is the one state
    // that raises a voice.
    h.until("stalled", |h| flag(h) == ["1"]).await;
    assert_eq!(state(&h), "silent", "the state says which one");
    let a = h.call("attention_get", json!({})).await.unwrap();
    assert_eq!(a["stalled"][0]["chatId"], chat.as_str());
    assert_eq!(a["badge"], 1);
    let got = h.call("chat_get", json!({"chatId": chat})).await.unwrap();
    assert_eq!(got["chat"]["stalled"], true);
    assert_eq!(got["chat"]["state"]["kind"], "silent");
    // The parent hears about it once, as a message it can act on.
    h.until("the parent was told", |h| {
        h.col(&format!(
            "SELECT m.origin FROM submissions s \
             JOIN submission_meta m ON m.submission_id = s.id \
             WHERE s.chat_id = '{parent}' AND m.origin = 'app'"
        )) == ["app"]
    })
    .await;
    assert!(
        h.col(&format!(
            "SELECT text FROM submissions WHERE chat_id = '{parent}' AND text LIKE 'Helm noticed%'"
        ))[0]
            .contains("silent for"),
        "the note says what and how long"
    );
    w.emit(json!({"type": "message_start", "message": {"id": "m9", "role": "assistant", "content": []}}))
        .await;
    h.until("unstalled", |h| flag(h) == ["0"]).await;
    h.until("generating again", |h| state(h) == "generating")
        .await;
    // The note goes once per hour per child: this chat recovered and went silent again, and the
    // parent is not told a second time.
    let notes = |h: &H| {
        h.col(&format!(
            "SELECT count(*) FROM submissions WHERE chat_id = '{parent}' AND text LIKE 'Helm noticed%'"
        ))
    };
    tokio::time::sleep(Duration::from_millis(1150)).await;
    h.until("stalled again", |h| flag(h) == ["1"]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        notes(&h),
        ["1"],
        "a second spell adds nothing within the hour"
    );

    // A parent that is idle is left idle: its silent child raises the flag and nothing else.
    let idle = h.create().await;
    let (child2, _, _w2) = running_turn(&mut h).await;
    h.db.write(|tx| {
        tx.execute(
            "UPDATE chats SET parent_chat_id = ?1 WHERE id = ?2",
            [&idle, &child2],
        )
    })
    .unwrap();
    h.until("the second child is flagged", |h| {
        h.col(&format!(
            "SELECT stalled FROM chat_activity WHERE chat_id = '{child2}'"
        )) == ["1"]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        h.col(&format!(
            "SELECT count(*) FROM submissions WHERE chat_id = '{idle}'"
        )),
        ["0"],
        "no turn is opened in an idle parent"
    );

    // A chat whose own helpers (tool servers, say) keep its process group at three from the
    // start still goes silent at the ordinary limit; only a group that grows past what it
    // began with (a build left running) stretches the limit and clears the flag.
    h.spawner.tree_at_launch.store(3, Ordering::SeqCst);
    let (c3, _, w3) = running_turn(&mut h).await;
    let flag3 = |h: &H| {
        h.col(&format!(
            "SELECT stalled FROM chat_activity WHERE chat_id = '{c3}'"
        ))
    };
    h.until("silent at the ordinary limit", |h| flag3(h) == ["1"])
        .await;
    w3.tree.store(4, Ordering::SeqCst);
    h.until("a grown group stretches the limit", |h| flag3(h) == ["0"])
        .await;
}

/// A child that ends a turn without having reported (its last reply is a tool call printed as
/// text) tells its parent once, even when the parent is idle: an idle parent is the one that
/// would otherwise wait for ever. A turn that closes normally says nothing, and the next
/// unfinished turn is a new spell with a new note.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_that_ends_its_turn_unfinished_tells_its_parent_once_per_spell() {
    let mut h = harness();
    let parent = h.create().await;
    let (child, _, mut w) = running_turn(&mut h).await;
    h.db.write(|tx| {
        tx.execute(
            "UPDATE chats SET parent_chat_id = ?1 WHERE id = ?2",
            [&parent, &child],
        )
    })
    .unwrap();
    let notes = |h: &H| {
        h.col(&format!(
            "SELECT text FROM submissions WHERE chat_id = '{parent}'              AND text LIKE 'Helm noticed:%ended its turn without reporting%'"
        ))
    };
    // The first turn ends on a printed tool call: the idle parent is told, once.
    w.emit(json!({"type": "message_start", "message": {"id": "m1", "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_end", "message": {"id": "m1", "role": "assistant", "content": [{"type": "text", "text": "<tool_call>{\"name\": \"Bash\"}</tool_call>"}], "status": "complete"}}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("the parent was told", |h| notes(h).len() == 1)
        .await;
    assert!(notes(&h)[0].contains("printed as text"), "{:?}", notes(&h));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(notes(&h).len(), 1, "once per spell");
    // A turn that closes with words says nothing.
    finished_turn(&h, &mut w, &child, "again", "m2", "Done: all twelve pass.").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(notes(&h).len(), 1, "a normal close is not reported");
    // The next unfinished turn is a new spell.
    finished_turn(&h, &mut w, &child, "once more", "m3", "<function=Read>").await;
    h.until("a second note", |h| notes(h).len() == 2).await;
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
    // The user's own message is a checkpoint: the fork is cut just before it, its draft holds
    // that message's text, and the source keeps everything.
    let second = user_row(&h, &chat, "second");
    let back = h
        .call(
            "chat_fork",
            json!({"chatId": chat, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": second}),
        )
        .await
        .unwrap();
    assert_eq!(back["draft"], "second");
    // What going back means, as the confirmation shows it: only the conversation comes back.
    let info = h
        .call(
            "checkpoint_info",
            json!({"chatId": chat, "messageId": second}),
        )
        .await
        .unwrap();
    assert_eq!(info["prompt"], "second");
    assert_eq!(info["messagesAfter"], 1);
    assert_eq!(info["filesRestored"], false);
    let t = texts(&h, back["id"].as_str().unwrap());
    assert!(t.contains(&"assistant:answer one".to_owned()));
    assert!(
        !t.iter().any(|x| x.contains("two")),
        "nothing from the message on"
    );
    assert_eq!(
        t.last().unwrap(),
        "context:Went back to before a message in first"
    );
    assert!(texts(&h, &chat).contains(&"assistant:answer two".to_owned()));
    // Before the very first message the fork starts empty.
    let first = user_row(&h, &chat, "first");
    let start = h
        .call(
            "chat_fork",
            json!({"chatId": chat, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": first}),
        )
        .await
        .unwrap();
    assert_eq!(start["draft"], "first");
    assert!(!texts(&h, start["id"].as_str().unwrap())
        .iter()
        .any(|x| x.starts_with("user:") || x.starts_with("assistant:")));
    // A row that is not part of the chat is refused.
    let bad = h
        .call(
            "chat_fork",
            json!({"chatId": chat, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": 999_999}),
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code, "refused");
    // A user row Helm wrote itself (a continuation, another chat's message) is no place to go
    // back to.
    h.db.write(|tx| {
        tx.execute(
            "UPDATE messages SET details = json_set(COALESCE(details, '{}'), '$.origin', 'app')
             WHERE id = ?1",
            [second],
        )
    })
    .unwrap();
    for (op, args) in [
        (
            "chat_fork",
            json!({"chatId": chat, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": second}),
        ),
        (
            "checkpoint_info",
            json!({"chatId": chat, "messageId": second}),
        ),
    ] {
        assert_eq!(h.call(op, args).await.unwrap_err().code, "refused", "{op}");
    }
    // A reply that did not finish is no fork point either.
    h.db.write(|tx| {
        tx.execute(
            "UPDATE messages SET status = 'interrupted' WHERE chat_id = ?1 AND role = 'assistant' AND text = 'answer two'",
            [&chat],
        )
    })
    .unwrap();
    let cut_short: i64 = h.col(&format!(
        "SELECT id FROM messages WHERE chat_id = '{chat}' AND text = 'answer two'"
    ))[0]
        .parse()
        .unwrap();
    let refused = h
        .call(
            "chat_fork",
            json!({"chatId": chat, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": cut_short}),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, "refused");
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
    // Phase two waits for the worker to close; a message that arrives meanwhile (a phone message,
    // a schedule) is not part of the replaced branch and still goes out after the edit.
    let (held, _) = h.send(&chat, "arrived while closing", "default").await;
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
    let p2 = w2.expect("prompt").await;
    assert_eq!(p2["submissionId"], held.as_str());
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
        &h.profile.path("attachments"),
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
    // A staged edit that cannot be applied (its session log is shorter than when it was staged)
    // is rejected and its staging row removed, not left to fail at every start.
    let broken = h.create().await;
    let _w2 = first_turn(&mut h, &broken).await;
    let first = user_row(&h, &broken, "first");
    let o = crate::edit::open(&h.db, &broken, first).unwrap();
    let sid = uuid::Uuid::new_v4().to_string();
    crate::edit::stage(
        &h.db,
        &h.profile.path("sessions"),
        &h.profile.path("attachments"),
        &crate::types::MessageResendArgs {
            chat_id: broken.clone(),
            message_id: first,
            revision: o.revision,
            submission_id: sid.clone(),
            text: "first, edited".into(),
        },
    )
    .unwrap();
    std::fs::remove_file(crate::session_log::log_path(
        &h.profile.path("sessions"),
        &broken,
    ))
    .unwrap();
    crate::edit::recover_all(&h.db, &h.profile.path(""), 3);
    assert!(h.col("SELECT chat_id FROM pending_edits").is_empty());
    assert_eq!(h.state(&sid), "rejected");
    // The repair leaves the chat editable: the same message can be staged again, and that is
    // not refused as a second pending edit.
    let o = crate::edit::open(&h.db, &broken, first).unwrap();
    let again = uuid::Uuid::new_v4().to_string();
    crate::edit::stage(
        &h.db,
        &h.profile.path("sessions"),
        &h.profile.path("attachments"),
        &crate::types::MessageResendArgs {
            chat_id: broken.clone(),
            message_id: first,
            revision: o.revision,
            submission_id: again.clone(),
            text: "first, edited again".into(),
        },
    )
    .unwrap();
    assert_eq!(
        h.col("SELECT chat_id FROM pending_edits"),
        std::slice::from_ref(&broken)
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
    h.send(&chat, "and another", "steer").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.spawner.title_jobs.lock().unwrap().len(), 1);
}

/// A background model set in preferences rides on the helper job: the `claude` name for a Claude
/// chat, and for a chat on a local model server the `local` name resolved through that chat's own
/// server (address and window on the job; the key travels too, and the process spawner moves it
/// into the helper's environment). Nothing set leaves the job as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_background_model_rides_on_the_helper_job() {
    let h = harness();
    *h.spawner.title.lock().unwrap() = Ok("Fix the login redirect".into());
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("qwen3-8b-helm-4k".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());

    // Nothing configured: the job is exactly what it was before this setting existed.
    let plain = h.create().await;
    h.send(&plain, "write the report on the q3 spend", "default")
        .await;
    h.until("plain title job", |h| {
        !h.spawner.title_jobs.lock().unwrap().is_empty()
    })
    .await;
    assert!(
        h.spawner.title_jobs.lock().unwrap()[0]
            .get("model")
            .is_none(),
        "an unset preference changes nothing"
    );

    // A local chat with no local model chosen never hands its words to Claude: no job runs and
    // the fallback title stays.
    let made = h
        .call(
            "chat_create",
            json!({"cwd": h.work.to_string_lossy(), "model": "local/ollama/qwen3-8b"}),
        )
        .await
        .unwrap();
    let unset_local = made["id"].as_str().unwrap().to_owned();
    h.send(&unset_local, "private words for the local model", "default")
        .await;
    h.until("local fallback", |h| {
        title_of(h, &unset_local).1 == "fallback/failed"
    })
    .await;
    assert_eq!(
        h.spawner.title_jobs.lock().unwrap().len(),
        1,
        "no title job ran for the local chat"
    );

    // The generic writer behind the `preference` op (the op itself lives in core, outside this
    // crate's harness; the writer is the part that classifies and validates).
    h.db.write(|tx| {
        helm_core::prefs::set_generic(
            tx,
            "backgroundModel",
            &json!({"claude": "qwen-turbo", "local": "qwen3-8b-helm-4k"}),
        )
    })
    .unwrap();

    let claude_chat = h.create().await;
    h.send(&claude_chat, "rename the deploy script safely", "default")
        .await;
    let made = h
        .call(
            "chat_create",
            json!({"cwd": h.work.to_string_lossy(), "model": "local/ollama/qwen3-8b"}),
        )
        .await
        .unwrap();
    let local_chat = made["id"].as_str().unwrap().to_owned();
    h.send(&local_chat, "summarise the notes on backups", "default")
        .await;

    h.until("three title jobs", |h| {
        h.spawner.title_jobs.lock().unwrap().len() >= 3
    })
    .await;
    let jobs = h.spawner.title_jobs.lock().unwrap().clone();
    let job_for = |needle: &str| {
        jobs.iter()
            .find(|j| {
                j["firstRequest"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(needle)
            })
            .cloned()
            .unwrap_or_else(|| panic!("no job for {needle}: {jobs:?}"))
    };
    let on_claude = job_for("rename the deploy script");
    assert_eq!(on_claude["model"], "qwen-turbo");
    assert!(
        on_claude.get("localBaseUrl").is_none(),
        "a Claude chat never gets the local route"
    );
    let on_local = job_for("summarise the notes on backups");
    assert_eq!(on_local["model"], "qwen3-8b-helm-4k");
    assert_eq!(on_local["localBaseUrl"], "http://127.0.0.1:9");
    assert_eq!(on_local["localKey"], "sk-fake-local-key");
    assert_eq!(on_local["localContextTokens"], 65_536);
    assert_eq!(on_local["localMaxOutputTokens"], 16_384);
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
    // A direction override and a zero-width space in the title are taken out.
    let r = h
        .call(
            "chat_rename",
            json!({"chatId": chat, "title": "My \u{202E}na\u{200B}me"}),
        )
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
    assert_eq!(s["titles"], json!(["Suggested"]));
    assert_eq!(title_of(&h, &other).0, "second chat");
    // Several options come back cleaned: no repeat (ignoring case), nothing equal to the current
    // title, at most three. The job carries the titles (only) of the siblings, archived ones
    // included, and none of their messages.
    h.db.write(|tx| {
        tx.execute(
            "UPDATE chats SET title = 'Helm 2.0 · crew X: fix', archived = 1 WHERE id = ?1",
            [&chat],
        )
    })
    .unwrap();
    *h.spawner.title.lock().unwrap() = Ok(
        "second chat\nHelm 2.0 · crew Y: add\nhelm 2.0 · crew y: add\n \nThird\nFourth\nFifth"
            .into(),
    );
    let s = h
        .call("title_suggest", json!({"chatId": other}))
        .await
        .unwrap();
    assert_eq!(
        s["titles"],
        json!(["Helm 2.0 · crew Y: add", "Third", "Fourth"])
    );
    assert_eq!(s["title"], "Helm 2.0 · crew Y: add");
    let jobs = h.spawner.title_jobs.lock().unwrap().clone();
    let job = jobs.last().unwrap();
    assert_eq!(job["suggest"], 3);
    assert_eq!(job["currentTitle"], "second chat");
    assert_eq!(job["siblingTitles"], json!(["Helm 2.0 · crew X: fix"]));
    assert_eq!(job["profileTitles"], json!([]));
    assert!(!job.to_string().contains("do the thing"));
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
    // It waits in the chat's unstaged set until a send takes it.
    let staged = h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(staged["images"][0]["id"], id.as_str());
    assert_eq!(staged["images"][0]["name"], "shot.png");
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(), "text": "",
               "action": "default", "draftRevision": 0, "attachments": [id.clone()]}),
    )
    .await
    .unwrap();
    // A waiting bubble shows the picture from the moment the send is accepted, and the image
    // has left the unstaged set.
    let page = h
        .call("transcript_page", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(page["pending"][0]["images"][0]["id"], id.as_str());
    assert!(h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap()["images"]
        .as_array()
        .unwrap()
        .is_empty());
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
                   "action": "steer", "draftRevision": 0, "attachments": [id]}),
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

/// The bubble's thumbnail is a small cached copy; the full image stays available.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bubble_thumbnail_is_small_and_cached() {
    use base64::Engine as _;
    let h = harness();
    let chat = h.create().await;
    let big = {
        let img = image::ImageBuffer::from_fn(1600, 1200, |x, y| {
            let n = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503).rotate_left(7);
            image::Rgb([(n >> 3) as u8, (n >> 11) as u8, (n >> 19) as u8])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    };
    let put = h
        .call(
            "attachment_put",
            json!({"chatId": chat, "name": "big.png", "mime": "image/png",
                   "bytes": base64::engine::general_purpose::STANDARD.encode(&big)}),
        )
        .await
        .unwrap();
    let id = put["attachmentId"].as_str().unwrap().to_owned();
    let ask = |thumb: bool| json!({"chatId": chat, "attachmentId": id, "thumb": thumb});
    // Staged but not sent yet: its own chat already reads it in both forms, so a composer
    // reopening over a restart can show it again. A foreign chat cannot, sent or staged.
    let staged = h.call("attachment_get", ask(false)).await.unwrap();
    assert_eq!(staged["mime"], "image/png");
    assert_eq!(
        staged["bytes"],
        base64::engine::general_purpose::STANDARD
            .encode(&big)
            .as_str()
    );
    assert_eq!(
        h.call(
            "attachment_get",
            json!({"chatId": "another-chat", "attachmentId": id})
        )
        .await
        .unwrap_err()
        .code,
        "not_found"
    );
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(), "text": "see",
               "action": "default", "draftRevision": 0, "attachments": [id]}),
    )
    .await
    .unwrap();
    let thumb_file = h.rt.sh.cfg.profile_dir.join("thumbs").join(&id);
    assert!(!thumb_file.exists(), "made on the first request");
    let small = h.call("attachment_get", ask(true)).await.unwrap();
    assert_eq!(small["mime"], "image/jpeg");
    let small_bytes = base64::engine::general_purpose::STANDARD
        .decode(small["bytes"].as_str().unwrap())
        .unwrap();
    assert!(small_bytes.len() < big.len() / 4);
    assert_eq!(std::fs::read(&thumb_file).unwrap(), small_bytes, "cached");
    let again = h.call("attachment_get", ask(true)).await.unwrap();
    assert_eq!(again, small);
    // The full image is still the stored file.
    let full = h.call("attachment_get", ask(false)).await.unwrap();
    assert_eq!(full["mime"], "image/png");
    assert_eq!(
        full["bytes"],
        base64::engine::general_purpose::STANDARD
            .encode(&big)
            .as_str()
    );
    // Another chat gets neither.
    let foreign = json!({"chatId": "another-chat", "attachmentId": id, "thumb": true});
    assert_eq!(
        h.call("attachment_get", foreign).await.unwrap_err().code,
        "not_found"
    );
}

/// A first turn whose message carries one image: the worker, the image's id and its bytes.
async fn first_turn_with_image(h: &mut H, chat: &str) -> (FakeWorker, String, Vec<u8>) {
    use base64::Engine as _;
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(12u32.to_be_bytes());
    png.extend(7u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let put = h
        .call(
            "attachment_put",
            json!({"chatId": chat, "name": "shot.png", "mime": "image/png",
                   "bytes": base64::engine::general_purpose::STANDARD.encode(&png)}),
        )
        .await
        .unwrap();
    let id = put["attachmentId"].as_str().unwrap().to_owned();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(), "text": "first",
               "action": "default", "draftRevision": 0, "attachments": [id]}),
    )
    .await
    .unwrap();
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    echo(&w, p["submissionId"].as_str().unwrap()).await;
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "message_start", "message": {"id": "m1", "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_end", "message": {"id": "m1", "role": "assistant", "content": [{"type": "text", "text": "answer one"}], "status": "complete"}}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(chat) == "idle").await;
    (w, id, png)
}

/// The references on a chat's user row.
async fn user_refs(h: &H, chat: &str) -> Value {
    let page = h
        .call("transcript_page", json!({"chatId": chat}))
        .await
        .unwrap();
    let user = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap_or_else(|| panic!("no user row in {page}"));
    user["details"]["refs"].clone()
}

/// The image ids on a chat's user rows, read through the transcript op a reopened chat uses.
async fn user_image_ids(h: &H, chat: &str) -> Vec<String> {
    let page = h
        .call("transcript_page", json!({"chatId": chat}))
        .await
        .unwrap();
    page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .flat_map(|m| {
            m["details"]["images"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|i| i["id"].as_str().unwrap().to_owned())
        })
        .collect()
}

async fn image_bytes(h: &H, chat: &str, id: &str) -> Result<String, String> {
    h.call(
        "attachment_get",
        json!({"chatId": chat, "attachmentId": id}),
    )
    .await
    .map(|v| v["bytes"].as_str().unwrap().to_owned())
    .map_err(|e| e.code.to_string())
}

/// A chat reopened from its history still shows the images: the rows come back from the session log
/// with the images in them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_rebuilt_from_its_log_still_has_its_images() {
    use base64::Engine as _;
    let mut h = harness();
    let chat = h.create().await;
    let (_w, id, png) = first_turn_with_image(&mut h, &chat).await;
    assert_eq!(user_image_ids(&h, &chat).await, std::slice::from_ref(&id));
    // Drop the projection and rebuild it from the log, as startup repair does.
    h.db.write(|tx| {
        tx.execute("DELETE FROM messages WHERE chat_id = ?1", [&chat])?;
        tx.execute("DELETE FROM entries WHERE chat_id = ?1", [&chat])
    })
    .unwrap();
    assert!(user_image_ids(&h, &chat).await.is_empty());
    crate::session_log::reconcile(
        &h.db,
        &h.profile.path("sessions"),
        &h.profile.path("artifacts"),
        &chat,
        99,
    )
    .unwrap();
    assert_eq!(user_image_ids(&h, &chat).await, std::slice::from_ref(&id));
    assert_eq!(
        image_bytes(&h, &chat, &id).await.unwrap(),
        base64::engine::general_purpose::STANDARD.encode(&png)
    );
}

/// A fork, whole or cut at a reply, shows the images sent in its copied history, and keeps them
/// when the source chat is deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forked_chats_have_their_own_copies_of_the_images() {
    use base64::Engine as _;
    let mut h = harness();
    let chat = h.create().await;
    let (w, id, png) = first_turn_with_image(&mut h, &chat).await;
    let want = base64::engine::general_purpose::STANDARD.encode(&png);
    // A picture a tool returned is a copy-carried image too.
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "tool_execution_start", "toolCallId": "tcp", "toolName": "Shot", "args": {}, "stoppable": false}))
        .await;
    w.emit(json!({"type": "tool_execution_end", "toolCallId": "tcp", "toolName": "Shot",
                  "result": {"text": "", "images": [{"mimeType": "image/png", "data": want}]}, "isError": false}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("tool picture", |h| {
        h.col(&format!(
            "SELECT json_array_length(details, '$.images') FROM messages WHERE chat_id = '{chat}' AND source_key = 't:tcp'"
        )) == ["1"]
    })
    .await;
    h.until("idle again", |h| h.status(&chat) == "idle").await;
    let tool_picture = h.col(&format!(
        "SELECT json_extract(details, '$.images[0].id') FROM messages WHERE chat_id = '{chat}' AND source_key = 't:tcp'"
    ))[0]
        .clone();
    let reply: i64 = h.col(&format!(
        "SELECT id FROM messages WHERE chat_id = '{chat}' AND role = 'assistant'"
    ))[0]
        .parse()
        .unwrap();
    let whole = h
        .call(
            "chat_fork",
            json!({"chatId": chat, "forkId": uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cut = h
        .call(
            "chat_fork",
            json!({"chatId": chat, "forkId": uuid::Uuid::new_v4().to_string(), "messageId": reply}),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let kept = h.col(&format!(
        "SELECT json_extract(details, '$.images[0].id') FROM messages WHERE chat_id = '{whole}' AND source_key = 't:tcp'"
    ));
    assert_eq!(kept.len(), 1, "the whole fork shows the tool's picture");
    assert_ne!(kept[0], tool_picture, "it is the fork's own copy");
    assert_eq!(image_bytes(&h, &whole, &kept[0]).await.unwrap(), want);
    let mut copies = Vec::new();
    for fork in [&whole, &cut] {
        let ids = user_image_ids(&h, fork).await;
        assert_eq!(ids.len(), 1, "{fork} shows the image");
        assert_ne!(ids[0], id, "it is the fork's own copy");
        assert_eq!(image_bytes(&h, fork, &ids[0]).await.unwrap(), want);
        // The thumbnail path serves it too.
        let t = h
            .call(
                "attachment_get",
                json!({"chatId": fork, "attachmentId": ids[0], "thumb": true}),
            )
            .await
            .unwrap();
        assert_eq!(t["bytes"], want.as_str());
        // A copy is the fork's alone.
        assert_eq!(
            image_bytes(&h, &chat, &ids[0]).await,
            Err("not_found".into())
        );
        assert_eq!(image_bytes(&h, fork, &id).await, Err("not_found".into()));
        copies.push(ids[0].clone());
    }
    assert_eq!(image_bytes(&h, &chat, &id).await.unwrap(), want);
    // The fork's log names the copy, so it survives a rebuild from the log.
    let log = std::fs::read_to_string(crate::session_log::log_path(
        &h.profile.path("sessions"),
        &whole,
    ))
    .unwrap();
    assert!(log.contains(&copies[0]) && !log.contains(&id));
    // Deleting the source leaves the forks' images.
    h.call("chat_archive", json!({"chatId": chat}))
        .await
        .unwrap();
    h.call("chat_delete", json!({"chatId": chat}))
        .await
        .unwrap();
    crate::delete::run_cleanup(&h.db, &h.profile.path(""));
    assert!(!h.profile.path("attachments").join(&id).exists());
    for (fork, copy) in [(&whole, &copies[0]), (&cut, &copies[1])] {
        assert_eq!(image_bytes(&h, fork, copy).await.unwrap(), want);
    }
    // A fork of a fork carries them on.
    let again = h
        .call(
            "chat_fork",
            json!({"chatId": whole, "forkId": uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ids = user_image_ids(&h, &again).await;
    assert_eq!(image_bytes(&h, &again, &ids[0]).await.unwrap(), want);
}

/// An edited message sent again keeps its images: the new bubble shows them and they ride the
/// prompt; the old message keeps its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edited_message_keeps_its_images() {
    use base64::Engine as _;
    let mut h = harness();
    let chat = h.create().await;
    let (mut w, id, png) = first_turn_with_image(&mut h, &chat).await;
    let want = base64::engine::general_purpose::STANDARD.encode(&png);
    let first = user_row(&h, &chat, "first");
    let o = h
        .call(
            "message_edit_open",
            json!({"chatId": chat, "messageId": first}),
        )
        .await
        .unwrap();
    let sid = uuid::Uuid::new_v4().to_string();
    h.call(
        "message_resend",
        json!({"chatId": chat, "messageId": first, "revision": o["revision"],
               "submissionId": sid, "text": "first, edited"}),
    )
    .await
    .unwrap();
    w.expect("shutdown").await;
    w.exit().await;
    let mut w2 = h.worker().await;
    w2.ready().await;
    let p = w2.expect("prompt").await;
    assert_eq!(p["text"], "first, edited");
    assert_eq!(p["images"][0]["mimeType"], "image/png");
    assert_eq!(p["images"][0]["data"], want.as_str());
    ack(&w2, &p).await;
    echo(&w2, &sid).await;
    h.until("the edited row", |h| {
        h.col(&format!(
            "SELECT status FROM messages WHERE chat_id = '{chat}' AND text = 'first, edited'"
        )) == ["complete"]
    })
    .await;
    let ids = user_image_ids(&h, &chat).await;
    assert_eq!(ids.len(), 1, "one active user message with one image");
    assert_ne!(ids[0], id, "the new message has its own copy");
    assert_eq!(image_bytes(&h, &chat, &ids[0]).await.unwrap(), want);
    // The first version still has its image (inactive, so out of the transcript).
    assert_eq!(image_bytes(&h, &chat, &id).await.unwrap(), want);
}

/// An edited message sent again keeps its files, folders and chat references too: the agent
/// hears about them again, and the new bubble shows the chips.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edited_message_keeps_its_references_too() {
    let mut h = harness();
    let chat = h.create().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "x").unwrap();
    let r = h
        .call(
            "attachment_ref",
            json!({"chatId": chat, "kind": "path", "target": file.to_string_lossy()}),
        )
        .await
        .unwrap();
    let ref_id = r["attachmentId"].as_str().unwrap().to_owned();
    let sid1 = uuid::Uuid::new_v4().to_string();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": sid1, "text": "first", "action": "default",
               "draftRevision": 0, "attachments": [ref_id]}),
    )
    .await
    .unwrap();
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert!(
        p["context"]
            .as_str()
            .unwrap()
            .contains(&format!("- file `notes.md`: {}", file.to_string_lossy())),
        "the reference rides the first prompt"
    );
    ack(&w, &p).await;
    echo(&w, &sid1).await;
    h.until("the first row", |h| {
        h.col(&format!(
            "SELECT status FROM messages WHERE chat_id = '{chat}' AND role = 'user'"
        )) == ["complete"]
    })
    .await;
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    let first = user_row(&h, &chat, "first");
    let o = h
        .call(
            "message_edit_open",
            json!({"chatId": chat, "messageId": first}),
        )
        .await
        .unwrap();
    let sid2 = uuid::Uuid::new_v4().to_string();
    h.call(
        "message_resend",
        json!({"chatId": chat, "messageId": first, "revision": o["revision"],
               "submissionId": sid2, "text": "first, edited"}),
    )
    .await
    .unwrap();
    w.expect("shutdown").await;
    w.exit().await;
    let mut w2 = h.worker().await;
    w2.ready().await;
    let p2 = w2.expect("prompt").await;
    assert_eq!(p2["text"], "first, edited");
    assert!(
        p2["context"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!("- file `notes.md`: {}", file.to_string_lossy())),
        "the carried reference rides the new prompt"
    );
    ack(&w2, &p2).await;
    echo(&w2, &sid2).await;
    h.until("the edited row", |h| {
        h.col(&format!(
            "SELECT status FROM messages WHERE chat_id = '{chat}' AND text = 'first, edited'"
        )) == ["complete"]
    })
    .await;
    assert_eq!(
        user_refs(&h, &chat).await,
        json!([{"kind": "file", "name": "notes.md", "target": file.to_string_lossy()}]),
        "the new bubble shows the carried reference"
    );
    assert_eq!(
        h.col("SELECT count(*) FROM chat_attachment_refs WHERE submission_id IS NOT NULL"),
        ["2"],
        "the old message keeps its own reference and the new one has its own"
    );
}

/// A recovered draft can carry its message's pictures and files again: the sent message keeps
/// its own, and new copies wait in the unstaged set for the next send.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recovered_draft_gets_fresh_copies_of_its_attachments() {
    use base64::Engine as _;
    let mut h = harness();
    let chat = h.create().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "x").unwrap();
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(10u32.to_be_bytes());
    png.extend(10u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let want = base64::engine::general_purpose::STANDARD.encode(&png);
    let put = h
        .call(
            "attachment_put",
            json!({"chatId": chat, "name": "shot.png", "mime": "image/png", "bytes": want.clone()}),
        )
        .await
        .unwrap();
    let img = put["attachmentId"].as_str().unwrap().to_owned();
    let rf = h
        .call(
            "attachment_ref",
            json!({"chatId": chat, "kind": "path", "target": file.to_string_lossy()}),
        )
        .await
        .unwrap();
    let path_ref = rf["attachmentId"].as_str().unwrap().to_owned();
    let sid1 = uuid::Uuid::new_v4().to_string();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": sid1, "text": "first", "action": "default",
               "draftRevision": 0, "attachments": [img.clone(), path_ref]}),
    )
    .await
    .unwrap();
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    echo(&w, &sid1).await;
    h.until("the first row", |h| {
        h.col(&format!(
            "SELECT status FROM messages WHERE chat_id = '{chat}' AND role = 'user'"
        )) == ["complete"]
    })
    .await;
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    let first = user_row(&h, &chat, "first");
    let r = h
        .call(
            "attachment_recover",
            json!({"chatId": chat, "messageId": first}),
        )
        .await
        .unwrap();
    assert_eq!(
        (r["images"].as_i64(), r["refs"].as_i64()),
        (Some(1), Some(1))
    );
    // Asking again copies the picture once more but not the file: an unstaged reference to
    // the same target already waits.
    let r = h
        .call(
            "attachment_recover",
            json!({"chatId": chat, "messageId": first}),
        )
        .await
        .unwrap();
    assert_eq!(
        (r["images"].as_i64(), r["refs"].as_i64()),
        (Some(1), Some(0))
    );
    let list = h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap();
    let copies: Vec<String> = list["images"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        copies.len(),
        2,
        "two fresh copies, the sent one is not in the pool"
    );
    assert!(!copies.contains(&img));
    assert_eq!(
        list["refs"].as_array().unwrap().len(),
        1,
        "one unstaged reference"
    );
    assert_eq!(
        list["refs"][0]["target"],
        file.to_string_lossy().to_string()
    );
    // The copies ride a new send: the prompt carries the picture bytes and names the file.
    let sid2 = uuid::Uuid::new_v4().to_string();
    let copy_ref = list["refs"][0]["id"].as_str().unwrap().to_owned();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": sid2, "text": "again", "action": "default",
               "draftRevision": 0, "attachments": [copies[0].clone(), copies[1].clone(), copy_ref]}),
    )
    .await
    .unwrap();
    // The chat's worker is still up: the prompt goes straight to it.
    let p2 = w.expect("prompt").await;
    assert_eq!(p2["images"].as_array().unwrap().len(), 2);
    assert_eq!(p2["images"][0]["data"], want.as_str());
    assert!(
        p2["context"]
            .as_str()
            .unwrap()
            .contains(&format!("- file `notes.md`: {}", file.to_string_lossy())),
        "the copied reference rides the prompt"
    );
    ack(&w, &p2).await;
    echo(&w, &sid2).await;
    h.until("the second row", |h| {
        h.col(&format!(
            "SELECT status FROM messages WHERE chat_id = '{chat}' AND text = 'again'"
        )) == ["complete"]
    })
    .await;
    let d: serde_json::Value = serde_json::from_str(
        &h.col(&format!(
            "SELECT details FROM messages WHERE chat_id = '{chat}' AND text = 'again'"
        ))[0],
    )
    .unwrap();
    assert_eq!(
        d["images"].as_array().unwrap().len(),
        2,
        "the new bubble shows the copies"
    );
    assert_eq!(d["refs"][0]["name"], "notes.md");
    let list = h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert!(
        list["images"].as_array().unwrap().is_empty()
            && list["refs"].as_array().unwrap().is_empty()
    );
    // The sent message keeps reading its own image, and a message that is not there is refused.
    assert_eq!(image_bytes(&h, &chat, &img).await.unwrap(), want);
    assert_eq!(
        h.call(
            "attachment_recover",
            json!({"chatId": chat, "messageId": 999999})
        )
        .await
        .unwrap_err()
        .code,
        "not_found"
    );
}

fn attachment_files(h: &H) -> usize {
    std::fs::read_dir(h.profile.path("attachments"))
        .map(|d| d.count())
        .unwrap_or(0)
}

/// A fork whose source image vanishes after it was checked fails cleanly: no fork chat, no log, no
/// copied file, and no log naming a copy that has no row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_fails_cleanly_when_a_source_image_vanishes() {
    let mut h = harness();
    let chat = h.create().await;
    let (_w, _id, _png) = first_turn_with_image(&mut h, &chat).await;
    let files = attachment_files(&h);
    let chats = h.col("SELECT id FROM chats");
    let fork_id = uuid::Uuid::new_v4().to_string();
    let refused = crate::fork::fork(
        &h.db,
        &h.profile.path(""),
        &crate::fork::ForkRequest {
            source: &chat,
            fork_id: &fork_id,
            message_id: None,
            side: false,
            excerpt: None,
        },
        // Runs at the start of the fork's transaction: the image row goes away just before it is
        // copied.
        &|tx, _| {
            tx.execute("DELETE FROM chat_attachments", [])?;
            Ok(false)
        },
    )
    .map(|f| f.chat_id)
    .unwrap_err();
    assert!(
        refused.message.contains("no longer available"),
        "{refused:?}"
    );
    assert_eq!(h.col("SELECT id FROM chats"), chats, "no fork chat");
    assert!(h.col("SELECT fork_id FROM chat_forks").is_empty());
    assert_eq!(attachment_files(&h), files, "the copied file was removed");
    let logs = std::fs::read_dir(h.profile.path("sessions"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".jsonl")
        })
        .count();
    assert_eq!(logs, 1, "only the source's log remains");
}

/// An edit that fails after its images were linked leaves no stray file behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_edit_leaves_no_stray_image_file() {
    let mut h = harness();
    let chat = h.create().await;
    let (_w, _id, _png) = first_turn_with_image(&mut h, &chat).await;
    let first = user_row(&h, &chat, "first");
    let o = crate::edit::open(&h.db, &chat, first).unwrap();
    let files = attachment_files(&h);
    // Make the staging's last insert fail, after the images were copied.
    h.db.write(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER no_pending_edits BEFORE INSERT ON pending_edits
             BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;",
        )
    })
    .unwrap();
    let sid = uuid::Uuid::new_v4().to_string();
    let err = crate::edit::stage(
        &h.db,
        &h.profile.path("sessions"),
        &h.profile.path("attachments"),
        &crate::types::MessageResendArgs {
            chat_id: chat.clone(),
            message_id: first,
            revision: o.revision,
            submission_id: sid.clone(),
            text: "first, edited".into(),
        },
    )
    .unwrap_err();
    assert!(err.message.contains("refused for the test"), "{err:?}");
    assert_eq!(attachment_files(&h), files, "the linked copy was removed");
    assert!(h
        .col(&format!("SELECT id FROM submissions WHERE id = '{sid}'"))
        .is_empty());
    assert_eq!(
        h.col("SELECT count(*) FROM chat_attachments"),
        ["1"],
        "and no row was kept"
    );
}

/// The skills a worker reports at get_state are what skills_list answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skills_come_from_the_workers_get_state() {
    let mut h = harness();
    // One of the reported skills is one of Helm's own: the host marks it so the `/` menu can say
    // so, even though the CLI shows it under its plugin prefix.
    std::fs::create_dir_all(h.profile.path("knowledge/everyone/skills/review")).unwrap();
    std::fs::write(
        h.profile.path("knowledge/everyone/skills/review/SKILL.md"),
        "---\ndescription: Review the diff\n---\n",
    )
    .unwrap();
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
        json!({"capabilities": {}, "skills": [
            {"name": "helm:review", "description": "Review the diff"},
            {"name": "deploy", "description": "Ship it"},
            {"name": "Personal:notes", "description": "Mine"},
            {"name": "project:lint", "description": "This repo's"}
        ]}),
    )
    .await;
    h.until("skills", |h| !h.rt.skills(&chat).is_empty()).await;
    let s = h
        .call("skills_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(
        s,
        json!([
            {"name": "helm:review", "description": "Review the diff", "scope": "helm-skill"},
            {"name": "deploy", "description": "Ship it", "scope": ""},
            {"name": "Personal:notes", "description": "Mine", "scope": "personal"},
            {"name": "project:lint", "description": "This repo's", "scope": "project"}
        ])
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

/// Entering the web search key, flipping "Offer in Claude chats" or forgetting the key reaches a
/// chat that is already running at its next turn: an idle worker closes when the next send opens
/// a turn and the fresh worker lists the tool again, resuming the same session; a working chat is
/// never interrupted — its worker closes once the turn settles, and the turn after sees the tool
/// gone again after a second flip. MCP servers need none of this: they reach a running worker
/// over its live config line (helm_mcp's lines tests).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_web_search_change_restarts_a_running_chat_at_its_next_turn() {
    struct Flip(AtomicBool);
    impl helm_hosttools::WebToolsSource for Flip {
        fn offered(&self, _local_chat: bool) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    let mut h = harness();
    let mut reg = helm_hosttools::HostToolRegistry::new();
    reg.blocking(
        helm_hosttools::ToolSpec::read("app_list_chats", "List chats.", json!({"type": "object"})),
        |_, _| Ok(json!({})),
    )
    .blocking(
        helm_hosttools::ToolSpec::read("app_web_search", "Search.", json!({"type": "object"}))
            .when(helm_hosttools::Condition::WebTools),
        |_, _| Ok(json!({})),
    );
    let flip = Arc::new(Flip(AtomicBool::new(false)));
    let mut services = Services::new();
    services.insert(Arc::new(helm_hosttools::WebToolsGate(flip.clone())));
    h.rt.attach_host(crate::hostcalls::HostSeam {
        ctx: OpCtx {
            services: Arc::new(services),
            events: h.sink.clone(),
        },
        tools: Arc::new(helm_hosttools::HostTools::new(Arc::new(reg))),
        lines: Arc::new(helm_hosttools::WorkerLines::new()),
    });
    let lists_search = |w: &FakeWorker| {
        w.req.host.as_ref().is_some_and(|host| {
            host.tools.as_array().is_some_and(|a| {
                a.iter()
                    .any(|t| t["name"].as_str() == Some("app_web_search"))
            })
        })
    };
    // Not offered: the first worker lists no search tool and settles with a session.
    let chat = h.create().await;
    h.send(&chat, "first", "default").await;
    let mut w = h.worker().await;
    assert!(!lists_search(&w));
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-w1"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    // Offered now: the send that opens the next turn closes the idle worker first, and the fresh
    // worker — which lists the tool and resumes the same session — gets the prompt.
    flip.0.store(true, Ordering::SeqCst);
    h.send(&chat, "again", "default").await;
    w.expect("shutdown").await;
    let mut w2 = h.worker().await;
    assert!(lists_search(&w2), "the fresh worker lists the tool");
    assert_eq!(
        w2.req.resume_session_id.as_deref(),
        Some("native-w1"),
        "the conversation carries over"
    );
    w2.ready().await;
    let p2 = w2.expect("prompt").await;
    ack(&w2, &p2).await;
    w2.emit(json!({"type": "agent_start"})).await;
    h.until("running", |h| h.status(&chat) == "running").await;
    // Off again mid-turn: the running worker is not interrupted; the turn finishes on it, the
    // worker closes at the settle, and the turn after that lists the tool no more.
    flip.0.store(false, Ordering::SeqCst);
    h.send(&chat, "steer", "default").await;
    let p3 = w2.expect("prompt").await;
    assert_eq!(p3["streamingBehavior"], "steer");
    ack(&w2, &p3).await;
    assert!(
        w2.no_command_for(Duration::from_millis(200))
            .await
            .is_none(),
        "no shutdown mid-turn"
    );
    w2.emit(json!({"type": "submission_delivered", "submissionId": p3["submissionId"]}))
        .await;
    w2.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-w2"}))
        .await;
    w2.expect("shutdown").await;
    w2.exit().await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    h.send(&chat, "after", "default").await;
    let w3 = h.worker().await;
    assert!(!lists_search(&w3), "the tool is gone again");
    assert_eq!(w3.req.resume_session_id.as_deref(), Some("native-w2"));
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
    // Two idle workers, cap one: the older (a) goes; b stays. The retirement follows b's turn
    // settling, so it is waited for as an event, with room for a loaded machine.
    wa.expect_within("shutdown", Duration::from_secs(30)).await;
    let mut wb = wb;
    let got = wb.no_command_for(Duration::from_millis(200)).await;
    assert!(got.is_none(), "the newer stays; it got {got:?}");
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
        enqueue(&tx, crate::actor::Cmd::Dispatch(String::new())).is_err(),
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
        send_soon(
            &sender,
            crate::actor::Cmd::Dispatch(String::new()),
            Duration::from_secs(5),
        )
        .await
        .is_ok()
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!waiting.is_finished(), "it waits while the channel is full");
    assert!(matches!(rx.recv().await, Some(crate::actor::Cmd::Retire)));
    assert!(waiting.await.unwrap(), "delivered once a slot freed");
    assert!(matches!(
        rx.recv().await,
        Some(crate::actor::Cmd::Dispatch(_))
    ));

    // The archive's stop is retried in the background when the channel is full, not dropped.
    assert!(enqueue(&tx, crate::actor::Cmd::Retire).is_ok());
    assert!(enqueue(&tx, crate::actor::Cmd::Terminate).is_ok());
    crate::runtime::enqueue_or_retry(
        &tokio::runtime::Handle::current(),
        tx.clone(),
        crate::actor::Cmd::Archived,
        Duration::from_secs(5),
    );
    assert!(matches!(rx.recv().await, Some(crate::actor::Cmd::Retire)));
    assert!(matches!(
        rx.recv().await,
        Some(crate::actor::Cmd::Terminate)
    ));
    assert!(matches!(rx.recv().await, Some(crate::actor::Cmd::Archived)));

    assert!(enqueue(&tx, crate::actor::Cmd::Retire).is_ok());
    let started = std::time::Instant::now();
    let r = send_soon(
        &tx,
        crate::actor::Cmd::Dispatch(String::new()),
        Duration::from_millis(200),
    )
    .await;
    assert!(
        matches!(r, Err(crate::actor::Cmd::Dispatch(_))),
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
    use crate::submit::{host_send, HostSend};
    let mut h = harness();
    let chat = h.create().await;
    let send = |h: &H| {
        host_send(
            &h.rt,
            HostSend {
                chat_id: &chat,
                submission_id: "decision-1",
                text: "[Helm] The user allowed it.",
                origin: crate::types::SubmissionOrigin::App,
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

/// A saved submission the hand-off never ran for (the host stopped first): a retry under the
/// same id hands it to the chat then, instead of reporting "saved" forever and leaving the
/// caller to retry an impossible send.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_send_retry_hands_over_a_saved_row_the_first_hand_off_missed() {
    use crate::submit::{host_send, HostSend};
    let mut h = harness();
    let chat = h.create().await;
    h.db.write(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, mode, state, created_at)
                 VALUES ('heal-1', ?1, '[Helm] A wait finished.', 'prompt', 'saved', 9)",
            [&chat],
        )
    })
    .unwrap();
    let sent = host_send(
        &h.rt,
        HostSend {
            chat_id: &chat,
            submission_id: "heal-1",
            text: "[Helm] A wait finished.",
            origin: crate::types::SubmissionOrigin::App,
        },
    )
    .unwrap();
    assert!(
        sent.delivered,
        "the chat takes the row it had already saved"
    );
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], "heal-1", "the saved row reaches Claude");
    assert_eq!(p["text"], "[Helm] A wait finished.");
    // A second retry while it is already on its way must not send it again.
    ack(&w, &p).await;
    let again = host_send(
        &h.rt,
        HostSend {
            chat_id: &chat,
            submission_id: "heal-1",
            text: "[Helm] A wait finished.",
            origin: crate::types::SubmissionOrigin::App,
        },
    )
    .unwrap();
    assert!(again.delivered);
    assert!(
        w.no_command_for(Duration::from_millis(300))
            .await
            .filter(|c| c["type"] == "prompt")
            .is_none(),
        "a row further than saved is never sent twice"
    );
}

/// A retry under one id is answered exactly as a new send to that chat would be when the chat
/// cannot take it: "busy" while it is stopping, so a retrier like the consent upkeep sends the
/// decision again at the next tick (and the stop's own carry hands the saved row over); and
/// "refused" once the chat is archived, so the upkeep stops retrying a chat that is gone and the
/// row is never carried to a worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_answers_like_a_new_send_when_the_chat_cannot_take_it() {
    use crate::submit::{host_send, HostSend};
    // This scenario is a stop that settles: its worker answers and finishes. The stop's kill
    // deadline is deliberately far out — nothing here tests the kill, and a scheduled-out
    // machine could fire it between the scripted worker's answer and its settle line.
    let mut h = harness_with(Limits {
        stop: Duration::from_secs(30),
        ..limits()
    });
    let (chat, _, mut w) = running_turn(&mut h).await;
    let decision = |id: &'static str| {
        host_send(
            &h.rt,
            HostSend {
                chat_id: &chat,
                submission_id: id,
                text: "[Helm] The user allowed it.",
                origin: crate::types::SubmissionOrigin::App,
            },
        )
    };
    // A saved row whose hand-off never ran, as an upkeep tick leaves it when the host stopped
    // first: the chat now stops with the row unsent.
    h.db.write(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, mode, state, created_at)
                 VALUES ('decision-1', ?1, '[Helm] The user allowed it.', 'prompt', 'saved', 9)",
            [&chat],
        )
    })
    .unwrap();
    let api = h.api.clone();
    let c2 = chat.clone();
    let stop =
        tokio::spawn(async move { api.call(&main(), "chat_stop", json!({"chatId": c2})).await });
    let abort = w.expect("abort").await;
    h.until("stopping", |h| h.status(&chat) == "stopping").await;
    let err = decision("decision-1").unwrap_err();
    assert_eq!(
        err.code, "busy",
        "a stopping chat is answered like a new send, not as a delivery: the upkeep retries"
    );
    assert!(
        err.message.contains("stopping"),
        "and says why: {}",
        err.message
    );
    // The stop's own carry takes the unsent row as the next turn; a later retry of the same id
    // then reads it as already on its way instead of sending it twice.
    w.respond(
        &abort,
        json!({"state": "stopping", "cancelledSubmissionIds": []}),
    )
    .await;
    w.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
        .await;
    stop.await.unwrap().unwrap();
    let p = w.expect("prompt").await;
    assert_eq!(
        p["submissionId"], "decision-1",
        "the carry hands the decision over"
    );
    ack(&w, &p).await;
    let sent = decision("decision-1").unwrap();
    assert!(
        sent.delivered,
        "a retry of a row further than saved is a delivery"
    );
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    // Archived: the permanent answer, and the row is never carried anywhere.
    h.db.write(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, mode, state, created_at)
                 VALUES ('decision-2', ?1, '[Helm] The user allowed it.', 'prompt', 'saved', 9)",
            [&chat],
        )
    })
    .unwrap();
    h.call("chat_archive", json!({"chatId": chat}))
        .await
        .unwrap();
    let err = decision("decision-2").unwrap_err();
    assert_eq!(err.code, "refused", "an archived chat never takes the note");
    assert_eq!(
        h.state("decision-2"),
        "uncertain",
        "archiving marks the unsent row uncertain — never carried, never resent"
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
    // The stall window is parked above this test's whole span (see `harness_with_services`), so
    // no upkeep tick can re-flag the chat while it runs. The flag still reading 0 says recovery
    // cleared it and the new turn left it clear; the old 100 ms sleep here only raced the
    // actor's scheduling and could prove nothing either way.
    assert_eq!(
        flag(&h),
        ["0"],
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
    // The last pick (plan mode aside) is where a chat the user starts from the new-chat page
    // begins, and Settings shows it; a chat started any other way (an agent, a crew, another
    // device, Telegram) begins in the launch mode.
    let s = h.call("runtime_settings_get", json!({})).await.unwrap();
    assert_eq!(s["permissionMode"], "acceptEdits");
    let cwd = h.work.to_string_lossy().into_owned();
    let made = |last: Option<bool>| {
        let mut args = json!({"cwd": cwd});
        if let Some(l) = last {
            args["startInLastMode"] = json!(l);
        }
        h.call("chat_create", args)
    };
    let mine = made(Some(true)).await.unwrap();
    assert_eq!(start(mine["id"].as_str().unwrap()), line("acceptEdits"));
    let theirs = made(None).await.unwrap();
    assert_eq!(start(theirs["id"].as_str().unwrap()), None);
    // A pick from another device is for that chat only: the new-chat default stays put.
    h.call(
        "chat_set_permission_mode",
        json!({"chatId": chat, "mode": "plan", "chatOnly": true}),
    )
    .await
    .unwrap();
    let s = h.call("runtime_settings_get", json!({})).await.unwrap();
    assert_eq!(s["permissionMode"], "acceptEdits");
    // No chat can move the stored pick: the preference is the user's alone.
    assert!(helm_hosttools::settings_allow::refusal("newChatPermissionMode", true).is_some());
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
        ("chat", chat.clone(), "bad_args"),
    ] {
        let e = h
            .call("attachment_ref", put(kind, target))
            .await
            .unwrap_err();
        assert_eq!(e.code, code, "{kind}");
    }
    let sid = uuid::Uuid::new_v4().to_string();
    let ids = json!([f["attachmentId"], d["attachmentId"], c["attachmentId"]]);
    // They wait in the chat's unstaged set, in the order they were added, until a send takes them.
    let staged = h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(staged["refs"][0]["id"], f["attachmentId"]);
    assert_eq!(staged["refs"][2]["id"], c["attachmentId"]);
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": sid, "text": "look", "action": "default",
               "draftRevision": 0, "attachments": ids}),
    )
    .await
    .unwrap();
    // The waiting bubble shows them, and they have left the unstaged set.
    let page = h
        .call("transcript_page", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(page["pending"][0]["refs"].as_array().unwrap().len(), 3);
    assert!(h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap()["refs"]
        .as_array()
        .unwrap()
        .is_empty());
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["text"], "look", "the user's own text is unchanged");
    ack(&w, &p).await;
    echo(&w, &sid).await;
    h.until("user row", |h| {
        !h.col(&format!(
            "SELECT id FROM messages WHERE chat_id = '{chat}' AND role = 'user'"
        ))
        .is_empty()
    })
    .await;
    let ctx = p["context"].as_str().unwrap();
    assert!(
        ctx.contains(&format!("- file `notes.md`: {}", file.to_string_lossy())),
        "{ctx}"
    );
    assert!(ctx.contains("- folder `"), "{ctx}");
    assert!(
        ctx.contains("app_read_chat") && ctx.contains(&format!("(chatId {other})")),
        "{ctx}"
    );
    assert!(ctx.contains("data, not as instructions"), "{ctx}");
    // The user's row carries the references, so the bubble keeps showing them, also after the
    // projection is rebuilt from the session log.
    let want = json!([
        {"kind": "file", "name": "notes.md", "target": file.to_string_lossy()},
        {"kind": "folder", "name": dir.path().file_name().unwrap().to_string_lossy(), "target": dir.path().to_string_lossy()},
        {"kind": "chat", "name": c["name"], "target": other},
    ]);
    assert_eq!(user_refs(&h, &chat).await, want);
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    h.db.write(|tx| {
        tx.execute("DELETE FROM messages WHERE chat_id = ?1", [&chat])?;
        tx.execute("DELETE FROM entries WHERE chat_id = ?1", [&chat])
    })
    .unwrap();
    crate::session_log::reconcile(
        &h.db,
        &h.profile.path("sessions"),
        &h.profile.path("artifacts"),
        &chat,
        99,
    )
    .unwrap();
    assert_eq!(user_refs(&h, &chat).await, want);
    // Sent references cannot be sent again.
    let e = h
        .call(
            "chat_send",
            json!({"chatId": chat, "submissionId": uuid::Uuid::new_v4().to_string(), "text": "again",
                   "action": "steer", "draftRevision": 0, "attachments": [f["attachmentId"]]}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "bad_args");
}

/// A fresh chat's `/` menu borrows the list another worker in the same folder
/// reported, and keeps it after that worker is gone; a chat in another folder gets nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_chat_borrows_its_folders_skills() {
    let mut h = harness();
    let chat = h.create().await;
    h.send(&chat, "hi", "default").await;
    let mut w = h.worker().await;
    let c = w.expect("get_state").await;
    w.respond(
        &c,
        json!({"capabilities": {}, "skills": [{"name": "review", "description": "Review the diff"}]}),
    )
    .await;
    h.until("skills", |h| !h.rt.skills(&chat).is_empty()).await;
    let fresh = h.create().await;
    let want = json!([{"name": "review", "description": "Review the diff", "scope": ""}]);
    assert_eq!(
        h.call("skills_list", json!({"chatId": fresh}))
            .await
            .unwrap(),
        want,
        "another live worker in the same folder"
    );
    let elsewhere = tempfile::tempdir().unwrap();
    let other = h
        .call(
            "chat_create",
            json!({"cwd": elsewhere.path().to_string_lossy()}),
        )
        .await
        .unwrap();
    let other = other["id"].as_str().unwrap().to_owned();
    assert_eq!(
        h.call("skills_list", json!({"chatId": other}))
            .await
            .unwrap(),
        json!([])
    );
    // Remembered for the folder, for chats after this worker is gone (and after a restart).
    let cached =
        crate::catalogue::cached_skills(&h.rt.sh.cfg.profile_dir, &h.work.to_string_lossy());
    assert_eq!(serde_json::to_value(cached).unwrap(), want);
}

/// Find in chat over the whole stored history: counts per row, the same searchable text as the
/// find bar, the active branch only, and paging across scan steps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_find_searches_the_whole_history_in_steps() {
    let h = harness();
    let chat = h.create().await;
    h.db.write(|tx| {
        let ins = "INSERT INTO messages(chat_id, source_key, role, text, thinking, tool_name, timestamp, active, details)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
        tx.execute(ins, rusqlite::params![chat, "a", "user", "Needle and NEEDLE", "", None::<String>, 1, 1, None::<String>])?;
        tx.execute(ins, rusqlite::params![chat, "b", "user", "needle on an abandoned branch", "", None::<String>, 2, 0, None::<String>])?;
        tx.execute(ins, rusqlite::params![chat, "c", "assistant", "plain", "needle in thought", None::<String>, 3, 1, None::<String>])?;
        tx.execute(ins, rusqlite::params![chat, "d", "toolResult", "output", "", "Needler", 4, 1, r#"{"toolArgs":"x"}"#])?;
        // Past one scan step, so the match at the end needs a second step.
        for i in 0..crate::store::FIND_STEP {
            tx.execute(ins, rusqlite::params![chat, format!("f{i}"), "assistant", "filler", "", None::<String>, 10 + i, 1, None::<String>])?;
        }
        tx.execute(ins, rusqlite::params![chat, "z", "assistant", "a needle at the end", "", None::<String>, 100_000, 1, None::<String>])?;
        Ok::<_, rusqlite::Error>(())
    })
    .unwrap();
    let find = |activity: bool, after: Option<String>| {
        let h = &h;
        let chat = chat.clone();
        async move {
            let r = h
                .call("chat_find", json!({"chatId": chat, "query": "needle", "includeActivity": activity, "after": after}))
                .await
                .unwrap();
            let rows: Vec<(i64, i64)> = r["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| (x["id"].as_i64().unwrap(), x["count"].as_i64().unwrap()))
                .collect();
            (rows, r["nextCursor"].as_str().map(str::to_owned))
        }
    };
    let ids = |sel: &str| -> Vec<i64> {
        h.col(&format!(
            "SELECT id FROM messages WHERE source_key {sel} ORDER BY id"
        ))
        .iter()
        .map(|s| s.parse().unwrap())
        .collect()
    };
    let a = ids("= 'a'")[0];
    let z = ids("= 'z'")[0];
    // Messages only: step one holds the first row (both occurrences), step two the last.
    let (r1, next) = find(false, None).await;
    assert_eq!(r1, [(a, 2)]);
    let (r2, next2) = find(false, next).await;
    assert_eq!(r2, [(z, 1)]);
    assert!(next2.is_none(), "the history is scanned");
    // With activity: thinking and tool names count too.
    let (r1, _) = find(true, None).await;
    let c = ids("= 'c'")[0];
    let d = ids("= 'd'")[0];
    assert_eq!(r1, [(a, 2), (c, 1), (d, 1)]);
    // An unknown chat is not found.
    assert!(h
        .call("chat_find", json!({"chatId": "nope", "query": "x"}))
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_idle_check_flags_a_quiet_chat_once_and_new_activity_clears_it() {
    let h = harness();
    let chat = h.create().await;
    let quiet_since = helm_core::now_ms() - 20 * 60_000;
    h.db.write(|tx| {
        tx.execute("UPDATE chats SET updated_at = ?1 WHERE id = ?2", rusqlite::params![quiet_since, chat])?;
        tx.execute(
            "INSERT INTO messages(chat_id, source_key, role, text, timestamp) VALUES (?1, 'u1', 'user', 'finish the migration', ?2)",
            rusqlite::params![chat, quiet_since],
        )
    })
    .unwrap();
    *h.spawner.triage.lock().unwrap() =
        Ok((true, "The migration still fails on the last step.".into()));
    let jobs = || h.spawner.triage_jobs.lock().unwrap().len();
    let flagged = || h.rt.attention().unwrap().flagged;
    // Wide like `H::until`: the verdict rides a helper thread and an actor write, both of which
    // a saturated machine can delay by tens of seconds.
    let wait_for = |want: usize| async move {
        for _ in 0..2_000 {
            if flagged().len() == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        panic!("expected {want} flagged chats");
    };

    crate::triage::pass(&h.rt.sh);
    wait_for(1).await;
    let s = h.rt.attention().unwrap();
    assert_eq!(
        (s.badge, s.flagged[0].kind),
        (1, crate::types::AttentionKind::Triage)
    );
    assert_eq!(
        s.flagged[0].reason,
        "The migration still fails on the last step."
    );
    let job = h.spawner.triage_jobs.lock().unwrap()[0].clone();
    assert_eq!(job["turns"][0]["text"], "finish the migration");
    assert_eq!(job["status"], "idle");

    // Once per quiet stretch (the 30 s gap and the checked stamp both say no).
    crate::triage::pass(&h.rt.sh);
    assert_eq!(jobs(), 1);

    // Triage off hides the flag; on again, it is back; new activity clears it for good.
    let set = |v: Value| {
        h.db.write(|tx| helm_core::prefs::set_generic(tx, "taskTriage", &v))
            .unwrap()
    };
    set(json!({"enabled": false, "delayMinutes": 15, "prompt": null}));
    assert!(flagged().is_empty());
    set(json!({"enabled": true, "delayMinutes": 15, "prompt": null}));
    assert_eq!(flagged().len(), 1);
    h.db.write(|tx| {
        tx.execute(
            "UPDATE chats SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![helm_core::now_ms(), chat],
        )
    })
    .unwrap();
    assert!(
        flagged().is_empty(),
        "the verdict was for the earlier activity"
    );
}

/// Roll over asks the agent for a hand-off, starts a successor in the same folder whose first
/// message carries it, archives the old chat and opens the new one. A chat with no reply, or one
/// that is working, is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roll_over_hands_the_work_to_a_successor_and_archives_the_old_chat() {
    let mut h = harness();
    let chat = h.create().await;
    let none = h
        .call("chat_roll_over", json!({"chatId": chat}))
        .await
        .unwrap_err();
    assert!(none.message.contains("no replies"), "{}", none.message);
    let (sid, _) = h.send(&chat, "build the thing", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    echo(&w, &sid).await;
    reply(&h, &w, "msg_1", "Built part one.").await;
    let busy = h
        .call("chat_roll_over", json!({"chatId": chat}))
        .await
        .unwrap_err();
    assert!(busy.message.contains("working"), "{}", busy.message);
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    // The chat belongs to someone other than the home person, and is shown to a second profile.
    h.db.write(|tx| {
        tx.execute(
            "UPDATE chats SET profile_id = 'p-sam' WHERE id = ?1",
            [&chat],
        )?;
        tx.execute(
            "INSERT INTO chat_shares(chat_id, profile_id) VALUES (?1, 'p-ann')",
            [&chat],
        )
    })
    .unwrap();

    let started = h
        .call("chat_roll_over", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(started["started"], true);
    let p = w.expect("prompt").await;
    assert!(p["text"].as_str().unwrap().contains("rolled over"));
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    let text = "Goal: finish it. Left to do: part two.";
    w.emit(json!({"type": "message_start", "message": {"id": "msg_2", "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_end", "message": {"id": "msg_2", "role": "assistant", "content": [{"type": "text", "text": text}], "status": "complete"}}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;

    let mut w2 = h.worker().await;
    w2.ready().await;
    let first = w2.expect("prompt").await;
    let text = first["text"].as_str().unwrap();
    assert!(
        text.contains(&chat) && text.contains("Left to do: part two."),
        "{text}"
    );
    // The successor's first turn is already under the old chat's person, not the home profile.
    let succ = h.col(&format!(
        "SELECT chat_id FROM chat_succession WHERE predecessor_id = '{chat}'"
    ))[0]
        .clone();
    assert_eq!(
        h.col(&format!(
            "SELECT profile_id || '/' || (SELECT group_concat(profile_id) FROM chat_shares WHERE chat_id = '{succ}') FROM chats WHERE id = '{succ}'"
        )),
        ["p-sam/p-ann"],
        "the owner and shares are set before the first turn"
    );
    h.until("old chat archived", |h| {
        h.col(&format!("SELECT archived FROM chats WHERE id = '{chat}'")) == ["1"]
    })
    .await;
    h.until("new chat opened", |h| {
        !h.events("shell_open_chat").is_empty()
    })
    .await;
    let opened = h.events("shell_open_chat").remove(0);
    let new = opened["chatId"].as_str().unwrap().to_owned();
    assert_ne!(new, chat);
    assert_eq!(
        opened["ifViewing"].as_str(),
        Some(chat.as_str()),
        "the window follows only while the old chat is still on screen"
    );
    // The successor keeps the old chat's title exactly: same work, same name, no marker.
    let old_title = h.col(&format!("SELECT title FROM chats WHERE id = '{chat}'"))[0].clone();
    assert!(!old_title.is_empty());
    assert_eq!(
        h.col(&format!("SELECT title FROM chats WHERE id = '{new}'")),
        [old_title.as_str()],
        "the successor carries the old chat's title as it is"
    );
    assert_eq!(
        opened["title"].as_str(),
        Some(old_title.as_str()),
        "the notice names the successor by that title: {opened:?}"
    );
    assert_eq!(
        h.col(&format!("SELECT project_id = (SELECT project_id FROM chats WHERE id = '{chat}') FROM chats WHERE id = '{new}'")),
        ["1"]
    );
    // The roll over ended in one write: the old chat archived and both sessions linked into
    // one stack, named for the oldest of them.
    assert_eq!(
        h.col(&format!(
            "SELECT COALESCE(next_session_id = '{new}', 0) FROM chats WHERE id = '{chat}'"
        )),
        ["1"],
        "the archived chat's row points at its successor"
    );
    assert_eq!(
        h.col(&format!(
            "SELECT COALESCE(prev_session_id = '{chat}', 0) || '/' || COALESCE(next_session_id, 'x') FROM chats WHERE id = '{new}'"
        )),
        ["1/x"],
        "the successor points back and is the stack's newest session"
    );
    assert_eq!(
        h.col(&format!(
            "SELECT stack_id FROM chats WHERE id IN ('{chat}', '{new}') ORDER BY created_at"
        )),
        [chat.as_str(), chat.as_str()],
        "both sessions carry one stack id, named for the oldest"
    );
    // One call from either session gives the whole stack, oldest first, with the newest named.
    let stack = h.call("chat_stack", json!({"chatId": chat})).await.unwrap();
    assert_eq!(stack["stackId"].as_str(), Some(chat.as_str()));
    assert_eq!(stack["latestId"].as_str(), Some(new.as_str()));
    let listed: Vec<&str> = stack["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, [chat.as_str(), new.as_str()]);
}

/// Attach a host seam carrying the harness's own services, so a flow started from inside the
/// runtime (the automatic roll-over trigger) can run its ops.
pub fn attach_ops_host(h: &H) {
    let mut s = Services::new();
    s.insert(Arc::new(h.profile.clone()));
    s.insert(h.db.clone());
    s.insert(Arc::new(Clock::new(0)));
    s.insert(Arc::new(h.rt.clone()));
    h.rt.attach_host(crate::hostcalls::HostSeam {
        ctx: OpCtx {
            services: Arc::new(s),
            events: h.sink.clone(),
        },
        tools: Arc::new(helm_hosttools::HostTools::new(Arc::new(
            helm_hosttools::HostToolRegistry::new(),
        ))),
        lines: Arc::new(helm_hosttools::WorkerLines::new()),
    });
}

/// A quiet chat grown past the roll-over setting runs the same flow the button runs, at the end
/// of its turn: the hand-off request goes out while the chat is idle, and the turn's end inside
/// that roll over is not itself a roll-over trigger (the in-flight guard holds).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_grown_past_the_setting_rolls_itself_over_at_its_turn_end() {
    let mut h = harness();
    attach_ops_host(&h);
    h.db.write(|tx| helm_core::prefs::set_generic(tx, "rolloverTokens", &json!(100)))
        .unwrap();
    let chat = h.create().await;
    let (sid, _) = h.send(&chat, "build the thing", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    echo(&w, &sid).await;
    // The reply's usage puts the chat at 462 tokens of context — over the setting.
    reply(&h, &w, "msg_1", "Built part one.").await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    let p = w.expect("prompt").await;
    assert!(
        p["text"].as_str().unwrap().contains("rolled over"),
        "the trigger asked for a hand-off at the turn's end"
    );
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    w.emit(json!({"type": "message_end", "message": {"id": "msg_2", "role": "assistant", "content": [{"type": "text", "text": "Goal: finish it. Left to do: part two."}], "status": "complete"}}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    let mut w2 = h.worker().await;
    w2.ready().await;
    let first = w2.expect("prompt").await;
    let text = first["text"].as_str().unwrap();
    assert!(
        text.contains(&chat) && text.contains("Left to do: part two."),
        "{text}"
    );
    h.until("old chat archived", |h| {
        h.col(&format!("SELECT archived FROM chats WHERE id = '{chat}'")) == ["1"]
    })
    .await;
}

/// The automatic trigger, one pass over every case that leaves a chat alone: the setting off,
/// context below it, no usage recorded, a side chat, an archived chat, a busy chat — and the
/// one row that fires. The asked-again case rides the in-flight guard in the firing row: the
/// roll over it starts is still running when its turn ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_auto_roll_over_trigger_checks_every_case_in_one_pass() {
    let h = harness();
    attach_ops_host(&h);
    let asked = |chat: &str| {
        h.col(&format!(
            "SELECT COUNT(*) FROM submissions WHERE chat_id = '{chat}' \
             AND text LIKE 'This chat is being rolled over%'"
        )) == ["1"]
    };
    for (label, pref, tokens, extra, expect) in [
        ("off", 0, 9_999, "", false),
        ("below", 100, 99, "", false),
        ("no usage", 100, 0, "", false),
        (
            "side chat",
            100,
            999,
            "UPDATE chats SET side_source_id = 'src' WHERE id = '{c}';",
            false,
        ),
        (
            "archived",
            100,
            999,
            "UPDATE chats SET archived = 1 WHERE id = '{c}';",
            false,
        ),
        (
            "busy",
            100,
            999,
            "UPDATE chats SET status = 'running' WHERE id = '{c}';",
            false,
        ),
        ("over the line", 100, 999, "", true),
    ] {
        let chat = h.create().await;
        if tokens > 0 {
            h.db
                .write(|tx| {
                    tx.execute(
                        "INSERT INTO messages(chat_id, source_key, role, text, timestamp, details)
                         VALUES (?1, 'x1', 'assistant', 'done', ?2, ?3)",
                        rusqlite::params![
                            &chat,
                            helm_core::now_ms(),
                            format!(
                                r#"{{"usage": {{"inputTokens": {tokens}, "cacheReadTokens": 0, "cacheWriteTokens": 0}}}}"#
                            )
                        ],
                    )
                })
                .unwrap();
        }
        if !extra.is_empty() {
            h.db.write(|tx| tx.execute_batch(&extra.replace("{c}", &chat)))
                .unwrap();
        }
        h.db.write(|tx| helm_core::prefs::set_generic(tx, "rolloverTokens", &json!(pref)))
            .unwrap();
        crate::rollover::on_turn_end(&h.rt.sh, &chat);
        assert_eq!(asked(&chat), expect, "{label}");
    }
    // The chat the trigger fired for is not asked a second time at the hand-off turn's end:
    // its roll over is still in flight.
    let last = h
        .col("SELECT id FROM chats WHERE archived = 0 ORDER BY rowid DESC LIMIT 1")
        .remove(0);
    crate::rollover::on_turn_end(&h.rt.sh, &last);
    assert_eq!(
        h.col(&format!(
            "SELECT COUNT(*) FROM submissions WHERE chat_id = '{last}' \
             AND text LIKE 'This chat is being rolled over%'"
        )),
        ["1"],
        "still exactly one ask"
    );
}

/// A local model server that records how it was used and can hold a preparation until released.
struct FakeLocal {
    turns: Mutex<Vec<crate::local::LocalTurn>>,
    outcome: Mutex<Result<String, String>>,
    gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    /// What the one preparation that waited at the gate answers, whatever `outcome` says by then.
    held_outcome: Mutex<Option<Result<String, String>>>,
    finished: Mutex<Vec<(String, String)>>,
    refreshed: Mutex<Vec<bool>>,
    queue: Mutex<Option<crate::local::QueueInfo>>,
}

impl crate::local::LocalModels for FakeLocal {
    fn knows(&self, server_id: &str) -> bool {
        matches!(server_id, "ollama" | "strata")
    }

    fn server(&self, _server_id: &str, _model: &str) -> Result<crate::local::LocalServer, String> {
        Ok(crate::local::LocalServer {
            base_url: "http://127.0.0.1:9".into(),
            key: "sk-fake-local-key".into(),
            context_tokens: Some(65_536),
            max_output_tokens: Some(16_384),
            lean: true,
        })
    }

    fn prepare(
        &self,
        turn: &crate::local::LocalTurn,
        progress: &dyn Fn(&str),
    ) -> Result<String, String> {
        self.turns.lock().unwrap().push(turn.clone());
        progress(&format!("Loading {}…", turn.model));
        let gate = self.gate.lock().unwrap().take();
        if let Some(g) = gate {
            let _ = g.recv_timeout(Duration::from_secs(5));
            if let Some(held) = self.held_outcome.lock().unwrap().take() {
                return held;
            }
        }
        self.outcome.lock().unwrap().clone()
    }

    fn finished(&self, server_id: &str, model: &str) {
        self.finished
            .lock()
            .unwrap()
            .push((server_id.into(), model.into()));
    }

    fn queue(&self, _server_id: &str) -> Option<crate::local::QueueInfo> {
        *self.queue.lock().unwrap()
    }

    fn catalogue(&self, refresh: bool) -> Vec<crate::types::LocalModelInfo> {
        self.refreshed.lock().unwrap().push(refresh);
        vec![crate::types::LocalModelInfo {
            id: "local/ollama/qwen3:8b".into(),
            name: "qwen3:8b".into(),
            model: "qwen3:8b".into(),
            server: "Ollama".into(),
            kind: crate::types::LocalServerKind::Ollama,
            context_tokens: Some(65_536),
            size_bytes: None,
            loaded: Some(false),
            reachable: true,
            startable: false,
        }]
    }
}

/// The live thread's loading line, if it shows one.
fn loading_text(h: &H) -> Option<String> {
    h.events("thread")
        .last()
        .and_then(|e| e["thread"]["loading"]["text"].as_str().map(str::to_owned))
}

/// A chat on a local model: the id maps to the chat's two columns and back, its model is made
/// ready before the worker starts (with the loading line up until the first words), a refusal
/// gives the text back, its turns cost nothing, and another model on its server starts a new
/// worker. The cross-provider moves have their own tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_chat_is_prepared_launched_and_costs_nothing() {
    let mut h = harness();
    let (open, gate) = std::sync::mpsc::channel();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("qwen3:8b-helm-64k".into())),
        gate: Mutex::new(Some(gate)),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());
    let cwd = h.work.to_string_lossy().into_owned();

    // The one id maps to provider + model on create, and comes back whole.
    for (model, why) in [
        ("local/nope/x", "not set up"),
        ("local/ollama", "not valid"),
    ] {
        let e = h
            .call("chat_create", json!({"cwd": cwd, "model": model}))
            .await
            .unwrap_err();
        assert!(e.message.contains(why), "{model}: {}", e.message);
    }
    let made = h
        .call(
            "chat_create",
            json!({"cwd": cwd, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap();
    let chat = made["id"].as_str().unwrap().to_owned();
    assert_eq!(made["model"], "local/ollama/qwen3:8b");
    assert_eq!(made["started"], false, "nothing has run yet");
    assert_eq!(
        h.col(&format!(
            "SELECT provider || '|' || model FROM chats WHERE id = '{chat}'"
        )),
        ["ollama|qwen3:8b"]
    );
    // A new-chat default that names a server which is gone starts Claude's own default instead of
    // refusing every chat that asked for no model; an explicit local pick is still refused.
    h.call("runtime_settings_save", json!({"model": "local/gone/x"}))
        .await
        .unwrap();
    let fallback = h.call("chat_create", json!({"cwd": cwd})).await.unwrap();
    assert_eq!(fallback["model"], Value::Null);
    h.call(
        "runtime_settings_save",
        json!({"model": "local/ollama/qwen3:8b"}),
    )
    .await
    .unwrap();
    let default_local = h.call("chat_create", json!({"cwd": cwd})).await.unwrap();
    assert_eq!(default_local["model"], "local/ollama/qwen3:8b");
    h.call("runtime_settings_save", json!({"model": ""}))
        .await
        .unwrap();
    let cat = h
        .call("model_catalogue", json!({"harness": "claude"}))
        .await
        .unwrap();
    assert_eq!(cat["local"][0]["id"], "local/ollama/qwen3:8b");
    h.call(
        "model_catalogue",
        json!({"harness": "claude", "refresh": true}),
    )
    .await
    .unwrap();
    assert_eq!(
        *local.refreshed.lock().unwrap(),
        [false, true],
        "only a refresh asks the servers"
    );

    // Typing warms the model: the same preparation the send will use, but no turn opens. The
    // loading line shows while it runs, and one typing burst prepares only once.
    h.call("chat_warm", json!({"chatId": chat})).await.unwrap();
    h.until("the warm-up's loading line", |h| {
        loading_text(h) == Some("Loading qwen3:8b…".into())
    })
    .await;
    assert_eq!(h.status(&chat), "idle", "a warm-up opens no turn");
    assert_eq!(local.turns.lock().unwrap().len(), 1);
    h.call("chat_warm", json!({"chatId": chat})).await.unwrap();
    assert_eq!(
        local.turns.lock().unwrap().len(),
        1,
        "once until a turn starts"
    );

    // The send joins the warm-up that is still running behind the gate: it shows as starting
    // from now, a second send waits behind the same preparation, and no worker exists yet.
    let (sid, _) = h.send(&chat, "hello", "default").await;
    h.until("the send takes the warm-up over", |h| {
        h.status(&chat) == "starting"
    })
    .await;
    let (second, _) = h.send(&chat, "and this", "default").await;
    assert!(
        h.workers.try_recv().is_err(),
        "no worker before the model is ready"
    );
    open.send(()).unwrap();
    let mut w = h.worker().await;
    let l = w.req.local.clone().expect("a local launch");
    assert_eq!(
        (l.server_id.as_str(), l.model.as_str(), l.thinking_tokens),
        ("ollama", "qwen3:8b-helm-64k", 16_384),
        "the worker asks for the name preparation answered, with the effort as a thinking budget"
    );
    let server = l.server.expect("the server was looked up before the start");
    assert_eq!(
        (server.key.as_str(), server.lean),
        ("sk-fake-local-key", true)
    );
    assert!(
        !format!("{:?}", w.req).contains("sk-fake-local-key"),
        "a request never prints the key"
    );
    // What the real spawner writes: the chat's effort stays in launch.json (the worker maps it to
    // the thinking budget), the model and server are set, and the key goes to the environment only.
    assert_eq!(w.req.effort.as_deref(), Some("xhigh"));
    let rt = helm_workers::launch::ClaudeRuntime::default();
    let mut claude = helm_workers::launch::claude_launch(
        &rt,
        None,
        w.req.model.clone(),
        w.req.effort.clone(),
        None,
    );
    let mut env = helm_workers::env::EnvMap::default();
    apply_local_launch(&mut claude, &mut env, w.req.local.as_ref().unwrap()).unwrap();
    assert_eq!(claude.effort.as_deref(), Some("xhigh"));
    assert_eq!(claude.model.as_deref(), Some("qwen3:8b-helm-64k"));
    assert_eq!(
        env.get(helm_workers::launch::LOCAL_KEY_ENV),
        Some("sk-fake-local-key")
    );
    let file = serde_json::to_string(&claude).unwrap();
    assert!(file.contains(r#""effort":"xhigh""#) && !file.contains("sk-fake-local-key"));
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], sid.as_str());
    let steer = w.expect("prompt").await;
    assert_eq!(
        (
            steer["submissionId"].as_str(),
            steer["streamingBehavior"].as_str()
        ),
        (Some(second.as_str()), Some("steer"))
    );
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    echo(&w, &sid).await;
    assert!(
        loading_text(&h).is_some(),
        "the line stays up until the model's first words"
    );
    reply(&h, &w, "msg_1", "Hi from the local model").await;
    h.until("the line is gone", |h| loading_text(h).is_none())
        .await;

    // Whatever cost Claude Code worked out is not real: tokens are kept, the cost is nothing.
    w.emit(json!({"type": "usage", "inputTokens": 12, "outputTokens": 30, "cacheReadTokens": 0, "cacheWriteTokens": 0, "costUsd": 0.05,
        "running": {"inputTokens": 12, "outputTokens": 30, "cacheReadTokens": 0, "cacheWriteTokens": 0, "costUsd": 0.05}}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-local"}))
        .await;
    h.until("chat idle", |h| h.status(&chat) == "idle").await;
    assert_eq!(
        h.col(
            "SELECT input_tokens || '/' || output_tokens || '/' || cost_usd || '/' || provider FROM chat_turn_usage"
        ),
        ["12/30/0.0/local"]
    );
    h.until("the keep-alive is set again", |_| {
        !local.finished.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        *local.finished.lock().unwrap(),
        [("ollama".to_owned(), "qwen3:8b-helm-64k".to_owned())]
    );
    let turns = local.turns.lock().unwrap().clone();
    assert_eq!(
        turns.len(),
        1,
        "one preparation covered the warm-up and both sends"
    );
    assert_eq!(
        (turns[0].server_id.as_str(), turns[0].launched.clone()),
        ("ollama", None)
    );
    h.until("session saved", |h| {
        crate::session_log::ClaudeBinding::load(&h.profile.path("sessions"), &chat)
            .is_some_and(|b| b.native_session_id.is_some())
    })
    .await;
    let got = h.call("chat_get", json!({"chatId": chat})).await.unwrap();
    assert_eq!(got["chat"]["started"], true, "its first turn ran");

    // A started chat can change its model. Another model on the same server starts a new worker;
    // a move across providers (to Claude, or to another server) has its own tests below.
    let moved = h
        .call(
            "chat_set_model",
            json!({"chatId": chat, "model": "local/ollama/llama3:8b"}),
        )
        .await
        .unwrap();
    assert_eq!(moved["model"], "local/ollama/llama3:8b");
    w.expect("shutdown").await;
    w.exit().await;
    h.until("the old worker is gone", |h| {
        h.events("thread")
            .last()
            .is_some_and(|e| e["thread"]["status"] == "idle")
    })
    .await;

    // A refusal starts no turn: the chat is idle, says why in the transcript and the text is
    // back in the draft.
    *local.outcome.lock().unwrap() =
        Err("llama3:8b can't load while Strata is answering in Notes.".into());
    let (refused, _) = h.send(&chat, "try again please", "default").await;
    h.until("the refusal", |h| h.state(&refused) == "rejected")
        .await;
    h.until("idle again", |h| h.status(&chat) == "idle").await;
    assert_eq!(
        h.col(&format!("SELECT draft FROM chats WHERE id = '{chat}'")),
        ["try again please"]
    );
    assert!(texts(&h, &chat)
        .iter()
        .any(|t| t == "context:llama3:8b can't load while Strata is answering in Notes."));

    // A message the app itself sent (a wait firing, a schedule) is refused the same way but is
    // not the user's text: it reads "not delivered" and the draft is left alone.
    let app_sid = uuid::Uuid::new_v4().to_string();
    crate::submit::host_send(
        &h.rt,
        crate::submit::HostSend {
            chat_id: &chat,
            submission_id: &app_sid,
            text: "a wait fired",
            origin: crate::types::SubmissionOrigin::App,
        },
    )
    .unwrap();
    h.until("the app message is refused", |h| {
        h.state(&app_sid) == "rejected"
    })
    .await;
    assert_eq!(
        h.col(&format!("SELECT draft FROM chats WHERE id = '{chat}'")),
        ["try again please"]
    );

    // Stop while the model is being made ready: nothing was sent, the text goes back to the draft.
    let (open, gate) = std::sync::mpsc::channel();
    *local.gate.lock().unwrap() = Some(gate);
    *local.outcome.lock().unwrap() = Ok("llama3:8b".into());
    let (held, _) = h.send(&chat, "never mind", "default").await;
    h.until("the line", |h| {
        loading_text(h) == Some("Loading llama3:8b…".into())
    })
    .await;
    h.call("chat_stop", json!({"chatId": chat})).await.unwrap();
    open.send(()).unwrap();
    h.until("stopped", |h| h.state(&held) == "rejected").await;
    assert_eq!(
        h.col(&format!("SELECT draft FROM chats WHERE id = '{chat}'")),
        ["try again please\n\nnever mind"]
    );
    assert_eq!(h.status(&chat), "stopped");

    // The once-between-turns rule does not lean on a preparation still being in flight: a finished
    // warm-up is still not followed by another one until a turn starts.
    let n = |id: &str| {
        local
            .turns
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.chat_id == id)
            .count()
    };
    let created = h
        .call(
            "chat_create",
            json!({"cwd": cwd, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap();
    let warmed_chat = created["id"].as_str().unwrap().to_owned();
    h.call("chat_warm", json!({"chatId": &warmed_chat}))
        .await
        .unwrap();
    h.until("the warm-up ran and answered", |_| n(&warmed_chat) == 1)
        .await;
    h.until("its line is gone with it", |h| loading_text(h).is_none())
        .await;
    h.call("chat_warm", json!({"chatId": &warmed_chat}))
        .await
        .unwrap();
    assert_eq!(
        n(&warmed_chat),
        1,
        "typing does not prepare twice between turns"
    );
    assert_eq!(
        h.status(&warmed_chat),
        "idle",
        "and a warm-up opens no turn"
    );

    // Type, pick another model, then send: a warm-up no send has taken over does not answer for
    // the new model — the send prepares that one instead.
    *local.outcome.lock().unwrap() = Ok("llama3:8b".into());
    let (open2, gate2) = std::sync::mpsc::channel();
    *local.gate.lock().unwrap() = Some(gate2);
    let changed = h
        .call(
            "chat_create",
            json!({"cwd": cwd, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap();
    let changed_chat = changed["id"].as_str().unwrap().to_owned();
    h.call("chat_warm", json!({"chatId": &changed_chat}))
        .await
        .unwrap();
    h.until("the first warm-up waits behind the gate", |h| {
        loading_text(h) == Some("Loading qwen3:8b…".into())
    })
    .await;
    h.call(
        "chat_set_model",
        json!({"chatId": &changed_chat, "model": "local/ollama/llama3:8b"}),
    )
    .await
    .unwrap();
    h.until("the model change took the warm-up down with it", |h| {
        loading_text(h).is_none()
    })
    .await;
    let (moved_sid, _) = h.send(&changed_chat, "hello", "default").await;
    h.until("the new model gets its own preparation", |_| {
        n(&changed_chat) == 2
    })
    .await;
    open2.send(()).unwrap();
    let w2 = h.worker().await;
    assert_eq!(
        w2.req
            .local
            .as_ref()
            .expect("a local launch")
            .model
            .as_str(),
        "llama3:8b",
        "the worker is launched for the model chosen now, not the one first warmed"
    );
    assert_eq!(
        local
            .turns
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.chat_id == changed_chat)
            .map(|t| t.model.as_str())
            .collect::<Vec<_>>(),
        ["qwen3:8b", "llama3:8b"],
        "the dropped warm-up finished, but never answered for the send"
    );

    // A message waiting on the old model's load goes out on the model chosen meanwhile: to
    // another local model it is prepared afresh (the old load's late answer is ignored), and to
    // a Claude model it needs no load at all.
    for (target, expect_local) in [
        ("local/ollama/llama3:8b", Some("llama3:8b")),
        ("opus", None),
    ] {
        *local.outcome.lock().unwrap() = Ok("llama3:8b".into());
        // The first model's load is held at the gate and answers with a name nothing else uses,
        // so its late answer can be told apart from the new model's own.
        *local.held_outcome.lock().unwrap() = Some(Ok("stale-name".into()));
        let (open3, gate3) = std::sync::mpsc::channel();
        *local.gate.lock().unwrap() = Some(gate3);
        let racing = h
            .call(
                "chat_create",
                json!({"cwd": cwd, "model": "local/ollama/qwen3:8b"}),
            )
            .await
            .unwrap();
        let racing_chat = racing["id"].as_str().unwrap().to_owned();
        let before = n(&racing_chat);
        let (racing_sid, _) = h.send(&racing_chat, "hello", "default").await;
        h.until("the first model's load is waiting", |_| {
            n(&racing_chat) == before + 1
        })
        .await;
        h.call(
            "chat_set_model",
            json!({"chatId": &racing_chat, "model": target}),
        )
        .await
        .unwrap();
        if expect_local.is_some() {
            h.until("the new model is prepared too", |_| {
                n(&racing_chat) == before + 2
            })
            .await;
        }
        let w3 = h.worker().await; // arrives without the old load being released
        assert_eq!(
            w3.req.local.as_ref().map(|l| l.model.as_str()),
            expect_local,
            "the turn starts on the model chosen while it waited ({target})"
        );
        open3.send(()).unwrap();
        assert_ne!(h.state(&racing_sid), "rejected");
    }

    // A later turn waiting on its load, with the chat's idle local worker still up, is moved to
    // another model: the worker is replaced for the new model, and since no agents were running
    // no notice says any were stopped.
    *local.outcome.lock().unwrap() = Ok("qwen3:8b".into());
    let later = h
        .call(
            "chat_create",
            json!({"cwd": cwd, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap();
    let later_chat = later["id"].as_str().unwrap().to_owned();
    let (first_sid, _) = h.send(&later_chat, "hello", "default").await;
    let mut w4 = h.worker().await;
    w4.ready().await;
    let p4 = w4.expect("prompt").await;
    ack(&w4, &p4).await;
    w4.emit(json!({"type": "agent_start"})).await;
    echo(&w4, &first_sid).await;
    w4.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-later"}))
        .await;
    h.until("the first turn is over", |h| {
        h.status(&later_chat) == "idle"
    })
    .await;
    *local.outcome.lock().unwrap() = Ok("llama3:8b".into());
    *local.held_outcome.lock().unwrap() = Some(Ok("stale-name".into()));
    let (open4, gate4) = std::sync::mpsc::channel();
    *local.gate.lock().unwrap() = Some(gate4);
    let before = n(&later_chat);
    h.send(&later_chat, "again", "default").await;
    h.until("the second turn's load is waiting", |_| {
        n(&later_chat) == before + 1
    })
    .await;
    h.call(
        "chat_set_model",
        json!({"chatId": &later_chat, "model": "local/ollama/llama3:8b"}),
    )
    .await
    .unwrap();
    w4.expect("shutdown").await;
    let w5 = h.worker().await; // arrives without the old load being released
    assert_eq!(
        w5.req.local.as_ref().map(|l| l.model.as_str()),
        Some("llama3:8b"),
        "the next turn starts on the model chosen while it waited"
    );
    open4.send(()).unwrap();
    assert!(
        !texts(&h, &later_chat)
            .iter()
            .any(|t| t.contains("Background agents were stopped")),
        "nothing was running, so nothing is said to have been stopped"
    );

    // A warm-up that fails stays quiet: no refusal, no turn — and no line left up in an idle chat.
    *local.outcome.lock().unwrap() = Err("no room on the server for that".into());
    let failed = h
        .call(
            "chat_create",
            json!({"cwd": cwd, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap();
    let failed_chat = failed["id"].as_str().unwrap().to_owned();
    h.call("chat_warm", json!({"chatId": &failed_chat}))
        .await
        .unwrap();
    h.until("the attempt ran", |_| n(&failed_chat) == 1).await;
    h.until("nothing is left showing", |h| loading_text(h).is_none())
        .await;
    assert_eq!(
        h.status(&failed_chat),
        "idle",
        "a failed warm-up starts nothing"
    );
    assert_ne!(
        h.state(&moved_sid),
        "rejected",
        "the send that replaced the warm-up was never refused"
    );

    // A chat that has started may still move from Claude to a local model: the next worker
    // resumes the same native session, so the conversation carries across the switch.
    let claude = h.create().await;
    h.call(
        "chat_set_model",
        json!({"chatId": claude, "model": "local/ollama/qwen3:8b"}),
    )
    .await
    .unwrap();
    h.call("chat_set_model", json!({"chatId": claude, "model": "opus"}))
        .await
        .unwrap();
    h.send(&claude, "first", "default").await;
    let mut cw = h.worker().await;
    cw.ready().await;
    let p = cw.expect("prompt").await;
    ack(&cw, &p).await;
    echo(&cw, p["submissionId"].as_str().unwrap()).await;
    cw.emit(json!({"type": "agent_start"})).await;
    cw.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-claude"}))
        .await;
    h.until("session saved", |h| {
        crate::session_log::ClaudeBinding::load(&h.profile.path("sessions"), &claude)
            .is_some_and(|b| b.native_session_id.is_some())
    })
    .await;
    let moved = h
        .call(
            "chat_set_model",
            json!({"chatId": claude, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap();
    assert_eq!(moved["model"], "local/ollama/qwen3:8b");
    cw.expect("shutdown").await;
    cw.exit().await;
    *local.outcome.lock().unwrap() = Ok("qwen3:8b-helm-64k".into());
    h.send(&claude, "carry on", "default").await;
    let wl = h.worker().await;
    assert_eq!(
        wl.req.resume_session_id.as_deref(),
        Some("native-claude"),
        "the local worker resumes the chat's Claude session: the conversation carries over"
    );
    assert!(wl.req.local.is_some(), "launched against the local server");
}

/// A provider change while the turn is open but nothing is in flight — the wait in a local
/// model server's queue is a wait, not work — stops the turn at once and opens it again on
/// the new model, instead of letting the queued request be answered on the old one. The same
/// in the other direction. A tool call genuinely running is held to the idle point as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_change_while_only_a_queue_wait_runs_restarts_the_turn_now() {
    let mut h = harness();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("qwen3:8b-helm-64k".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());

    // Local chat to Claude: the turn is open, no tool and no output — a queue wait.
    let lc = h.create().await;
    h.call(
        "chat_set_model",
        json!({"chatId": &lc, "model": "local/ollama/qwen3:8b"}),
    )
    .await
    .unwrap();
    let (sid, _) = h.send(&lc, "carry the plan", "default").await;
    let mut lw = h.worker().await;
    assert!(
        lw.req.local.is_some(),
        "the first worker is on the local server"
    );
    lw.ready().await;
    let p = lw.expect("prompt").await;
    ack(&lw, &p).await;
    lw.emit(json!({"type": "agent_start"})).await;
    h.until("running", |h| h.status(&lc) == "running").await;
    h.call("chat_set_model", json!({"chatId": &lc, "model": "opus"}))
        .await
        .unwrap();
    let abort = lw.expect("abort").await;
    lw.respond(
        &abort,
        json!({"state": "stopping", "cancelledSubmissionIds": []}),
    )
    .await;
    lw.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
        .await;
    lw.expect("shutdown").await;
    let mut cw2 = h.worker().await;
    assert!(
        cw2.req.local.is_none(),
        "the restarted turn's worker runs the chat's new model"
    );
    cw2.ready().await;
    let p = cw2.expect("prompt").await;
    assert_eq!(
        p["submissionId"],
        sid.as_str(),
        "the turn's opening message is sent again, not a fresh one"
    );
    assert!(
        p.get("streamingBehavior").is_none(),
        "it opens the restarted turn"
    );
    assert!(
        texts(&h, &lc)
            .iter()
            .any(|t| t.contains("The model was changed")),
        "the transcript says the turn restarts on the new model"
    );
    // Claude chat to local, same shape: the open turn stops and reopens on the local server.
    let cc = h.create().await;
    let (sid2, _) = h.send(&cc, "run it", "default").await;
    let mut cw = h.worker().await;
    cw.ready().await;
    let p = cw.expect("prompt").await;
    ack(&cw, &p).await;
    cw.emit(json!({"type": "agent_start"})).await;
    h.until("running", |h| h.status(&cc) == "running").await;
    h.call(
        "chat_set_model",
        json!({"chatId": &cc, "model": "local/ollama/qwen3:8b"}),
    )
    .await
    .unwrap();
    let abort = cw.expect("abort").await;
    cw.respond(
        &abort,
        json!({"state": "stopping", "cancelledSubmissionIds": []}),
    )
    .await;
    cw.emit(json!({"type": "agent_settled", "outcome": "stopped"}))
        .await;
    cw.expect("shutdown").await;
    let mut lw2 = h.worker().await;
    assert_eq!(
        lw2.req.local.as_ref().map(|l| l.model.as_str()),
        Some("qwen3:8b-helm-64k"),
        "the reopened turn's worker is launched for the local model it was made ready for"
    );
    lw2.ready().await;
    let p = lw2.expect("prompt").await;
    assert_eq!(
        p["submissionId"],
        sid2.as_str(),
        "and its message goes out again"
    );

    // A tool call genuinely running: nothing is stopped now; the change lands at the idle
    // point after the turn, as it always did.
    let hc = h.create().await;
    h.send(&hc, "work", "default").await;
    let mut hw = h.worker().await;
    hw.ready().await;
    let p = hw.expect("prompt").await;
    ack(&hw, &p).await;
    hw.emit(json!({"type": "agent_start"})).await;
    hw.emit(json!({"type": "tool_execution_start", "toolCallId": "tc1", "toolName": "Bash", "args": {"command": "cargo build"}, "stoppable": true}))
        .await;
    h.until("the actor has the tool in hand", |h| {
        !h.col(&format!(
            "SELECT id FROM messages WHERE chat_id = '{hc}' AND tool_call_id = 'tc1'"
        ))
        .is_empty()
    })
    .await;
    h.call(
        "chat_set_model",
        json!({"chatId": &hc, "model": "local/ollama/qwen3:8b"}),
    )
    .await
    .unwrap();
    assert!(
        hw.no_command_for(Duration::from_millis(200))
            .await
            .is_none(),
        "a running tool call is left alone"
    );
    hw.emit(json!({"type": "tool_execution_end", "toolCallId": "tc1", "toolName": "Bash", "result": {"text": "done"}, "isError": false}))
        .await;
    hw.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-held"}))
        .await;
    hw.expect("shutdown").await;

    // Between steps of a turn that has already replied and run a tool: nothing is in flight,
    // but starting the turn again would repeat what it did. The change waits for the idle point
    // and the opening message is not sent a second time.
    let bc = h.create().await;
    h.send(&bc, "ship it", "default").await;
    let mut bw = h.worker().await;
    bw.ready().await;
    let p = bw.expect("prompt").await;
    ack(&bw, &p).await;
    bw.emit(json!({"type": "agent_start"})).await;
    reply(&h, &bw, "mb1", "Pushing now.").await;
    bw.emit(json!({"type": "tool_execution_start", "toolCallId": "tb1", "toolName": "Bash", "args": {"command": "git push"}, "stoppable": true}))
        .await;
    bw.emit(json!({"type": "tool_execution_end", "toolCallId": "tb1", "toolName": "Bash", "result": {"text": "pushed"}, "isError": false}))
        .await;
    h.until("the tool call finished", |h| {
        h.col(&format!(
            "SELECT status FROM messages WHERE chat_id = '{bc}' AND tool_call_id = 'tb1'"
        )) == ["complete"]
    })
    .await;
    h.call(
        "chat_set_model",
        json!({"chatId": &bc, "model": "local/ollama/qwen3:8b"}),
    )
    .await
    .unwrap();
    assert!(
        bw.no_command_for(Duration::from_millis(200))
            .await
            .is_none(),
        "a turn that has acted is not stopped and opened again"
    );
    bw.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-b"}))
        .await;
    bw.expect("shutdown").await;
}

/// A send that lands while the previous worker is still winding down (a relaunch from any
/// cause) is never handed to the closing worker: the closing link is set aside and a fresh
/// worker starts, so the prompt reaches a worker that can answer and is not left uncertain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_during_a_worker_shutdown_starts_a_fresh_worker() {
    let mut h = harness();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("qwen3:8b".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());
    let claude = h.create().await;
    let mut cw = first_turn(&mut h, &claude).await; // live idle worker, nothing has exited
    h.call(
        "chat_set_model",
        json!({"chatId": claude, "model": "local/ollama/qwen3:8b"}),
    )
    .await
    .unwrap();
    cw.expect("shutdown").await;
    // The old worker has not exited yet when the next send arrives, right after the switch.
    let (sid, _) = h.send(&claude, "carry on", "default").await;
    let mut wl = h.worker().await;
    assert!(
        wl.req.local.is_some(),
        "the fresh worker launches on the local server"
    );
    wl.ready().await;
    let p = wl.expect("prompt").await;
    assert_eq!(p["submissionId"], sid.as_str(), "the prompt reaches it");
    ack(&wl, &p).await;
    h.until("accepted", |h| h.state(&sid) == "accepted").await;
    assert!(
        cw.no_command_for(Duration::from_millis(150))
            .await
            .is_none(),
        "the closing worker is not handed the send"
    );
}

/// An effort-only change on a local chat is retold to the live worker (which maps the effort
/// to the local model's thinking budget) instead of respawning it: no shutdown goes out and
/// no second worker starts. A model change still relaunches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_effort_change_on_a_local_chat_reaches_the_live_worker() {
    let mut h = harness();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("qwen3:8b".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());
    let cwd = h.work.to_string_lossy().into_owned();
    let chat = h
        .call(
            "chat_create",
            json!({"cwd": cwd, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.send(&chat, "first", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    echo(&w, p["submissionId"].as_str().unwrap()).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-lm"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    let rec = h
        .call("chat_set_model", json!({"chatId": &chat, "effort": "low"}))
        .await
        .unwrap();
    assert_eq!(rec["effort"], "low");
    let st = w.expect("set_thinking_level").await;
    assert_eq!(st["level"], "low", "the effort goes to the live worker");
    w.respond(&st, json!({})).await;
    assert!(
        w.no_command_for(Duration::from_millis(150)).await.is_none(),
        "no shutdown: the worker is kept, not respawned"
    );
    assert!(
        h.workers.try_recv().is_err(),
        "an effort-only change does not start a second worker"
    );
    assert_eq!(h.status(&chat), "idle");
    // A model change on the same local chat still relaunches: the model is part of the launch.
    h.call(
        "chat_set_model",
        json!({"chatId": &chat, "model": "local/ollama/llama3:8b"}),
    )
    .await
    .unwrap();
    w.expect("shutdown").await;
}

/// A worker that exits while the chat's model is still being made ready ends the run
/// interrupted and gives the typed text back: the preparation's later answer must not open a
/// turn on the run that has already ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_exit_during_preparation_does_not_reopen_the_turn() {
    let mut h = harness();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("qwen3:8b".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());
    let cwd = h.work.to_string_lossy().into_owned();
    let chat = h
        .call(
            "chat_create",
            json!({"cwd": cwd, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.send(&chat, "first", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    ack(&w, &p).await;
    echo(&w, p["submissionId"].as_str().unwrap()).await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-lm"}))
        .await;
    h.until("idle", |h| h.status(&chat) == "idle").await;
    // The next send is making its model ready when the worker exits behind it.
    let (open, gate) = std::sync::mpsc::channel();
    *local.gate.lock().unwrap() = Some(gate);
    // The second send carries a picture: it must come back to the composer with the text.
    use base64::Engine as _;
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(10u32.to_be_bytes());
    png.extend(10u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let put = h
        .call(
            "attachment_put",
            json!({"chatId": chat, "name": "gone.png", "mime": "image/png",
                   "bytes": base64::engine::general_purpose::STANDARD.encode(&png)}),
        )
        .await
        .unwrap();
    let img = put["attachmentId"].as_str().unwrap().to_owned();
    let sid = uuid::Uuid::new_v4().to_string();
    h.call(
        "chat_send",
        json!({"chatId": chat, "submissionId": sid, "text": "second", "action": "default",
               "draftRevision": 0, "attachments": [img.clone()]}),
    )
    .await
    .unwrap();
    h.until("the preparation", |h| h.status(&chat) == "starting")
        .await;
    w.exit().await;
    h.until("interrupted", |h| h.status(&chat) == "interrupted")
        .await;
    assert_eq!(h.state(&sid), "rejected", "nothing reached the model");
    assert_eq!(
        h.col(&format!("SELECT draft FROM chats WHERE id = '{chat}'")),
        ["second"],
        "the unsent text goes back to the draft"
    );
    let list = h
        .call("attachment_list", json!({"chatId": chat}))
        .await
        .unwrap();
    assert_eq!(
        list["images"][0]["id"],
        img.as_str(),
        "the picture waits in the unstaged set with the draft"
    );
    // The preparation now answers: no turn reopens on the ended run, and no worker starts.
    open.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.status(&chat), "interrupted", "the ended run stays ended");
    assert!(
        h.workers.try_recv().is_err(),
        "no worker is started for the send that ended"
    );
}

/// One whole turn played out to a settled end without the streaming and ordering checks the
/// dedicated tests make: the echo, one reply written whole, the running totals, and the settle
/// naming `session` (which is what the next worker resumes from). Answers with the worker, still
/// up and now idle.
async fn settled_turn(
    h: &mut H,
    chat: &str,
    text: &str,
    mid: &str,
    answer: &str,
    running: Value,
    session: &str,
) -> FakeWorker {
    let (sid, _) = h.send(chat, text, "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], sid.as_str());
    ack(&w, &p).await;
    echo(&w, sid.as_str()).await;
    w.emit(json!({"type": "agent_start"})).await;
    // Claude Code names its session as it starts, before anything is used.
    w.emit(json!({"type": "native_session", "sessionId": session}))
        .await;
    w.emit(json!({"type": "message_start", "message": {"id": mid, "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_update", "messageId": mid, "index": 0, "delta": {"type": "text_delta", "text": answer}}))
        .await;
    w.emit(json!({"type": "message_end", "message": {"id": mid, "role": "assistant", "content": [{"type": "text", "text": answer}], "usage": {"input_tokens": 12, "output_tokens": 30, "cache_read_input_tokens": 400, "cache_creation_input_tokens": 50}, "status": "complete"}}))
        .await;
    w.emit(json!({
        "type": "usage", "inputTokens": 12, "outputTokens": 30,
        "cacheReadTokens": 400, "cacheWriteTokens": 50, "costUsd": 0.05, "running": running
    }))
    .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": session}))
        .await;
    h.until("the turn settled", |h| h.status(chat) == "idle")
        .await;
    // The binding is on disk before anything resumes from it.
    h.until("the session is saved", |h| {
        crate::session_log::ClaudeBinding::load(&h.profile.path("sessions"), chat)
            .is_some_and(|b| b.native_session_id.as_deref() == Some(session))
    })
    .await;
    w
}

/// A chat that has started can change its model in either direction between a local model server
/// and Claude: the next worker resumes the chat's own Claude Code session, so the conversation
/// carries across the switch, and the transcript's notice row carries the changer's reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_started_chat_moves_between_local_and_claude_in_both_directions() {
    let mut h = harness();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("alpha-model".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());
    let chat = h.create().await;
    h.call("chat_set_model", json!({"chatId": &chat, "model": "opus"}))
        .await
        .unwrap();
    // The session's running totals after its k-th turn: each turn adds the same amount.
    let totals = |k: i64| {
        json!({"inputTokens": 20 * k, "outputTokens": 40 * k, "cacheReadTokens": 500 * k,
               "cacheWriteTokens": 60 * k, "costUsd": 0.5 * k as f64})
    };
    let mut cw = settled_turn(
        &mut h,
        &chat,
        "first on Claude",
        "m-1",
        "working on it",
        totals(1),
        "native-0",
    )
    .await;
    // (move to, the notice row it must write, the worker launches on the local server, the
    //  session the next worker resumes, and the session that turn settles as)
    for (k, (model, notice, on_local, resumes, session)) in [
        (
            "local/strata/alpha",
            "Model changed to alpha (local) — other provider",
            true,
            "native-0",
            "native-1",
        ),
        (
            "opus[1m]",
            "Model changed to opus[1m] — other provider",
            false,
            "native-1",
            "native-2",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let moved = h
            .call(
                "chat_set_model",
                json!({"chatId": &chat, "model": model, "reason": "other provider"}),
            )
            .await
            .unwrap();
        assert_eq!(moved["model"], model, "the chat moved to {model}");
        // The chat is idle, so the change takes its worker down now; the next send starts a new one.
        cw.expect("shutdown").await;
        cw.exit().await;
        cw = settled_turn(
            &mut h,
            &chat,
            &format!("a turn on {model}"),
            &format!("m-{session}"),
            "carried on from the other provider",
            totals(k as i64 + 2),
            session,
        )
        .await;
        assert_eq!(
            cw.req.local.is_some(),
            on_local,
            "{model}: the worker launches against the local server, not on Claude"
        );
        assert_eq!(
            cw.req.resume_session_id.as_deref(),
            Some(resumes),
            "{model}: the next worker resumes the conversation carried from the other provider"
        );
        let notes = h.col(&format!(
            "SELECT text FROM messages WHERE chat_id = '{chat}' AND role = 'context' ORDER BY id"
        ));
        assert!(
            notes.iter().any(|t| t == notice),
            "{model}: the transcript notice row is missing; rows are {notes:?}"
        );
    }
    // Each turn records only its own share of the running totals, whichever side it ran on; the
    // local turn costs nothing, and the Claude turn after it is not charged for the local one.
    assert_eq!(
        h.col("SELECT input_tokens || '/' || cache_read_tokens || '/' || cost_usd FROM chat_turn_usage ORDER BY timestamp, rowid"),
        ["20/500/0.5", "20/500/0.0", "20/500/0.5"]
    );
    cw.exit().await;
}

/// A switch to a local model waits for the chat's background agents, but a message that would open
/// a turn must not go to the old worker meanwhile: the agents are stopped, with a notice, and the
/// message goes to a worker on the new model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_after_a_provider_switch_does_not_reach_the_old_worker_behind_its_agents() {
    let mut h = harness();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("alpha-model".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());
    let chat = h.create().await;
    h.call("chat_set_model", json!({"chatId": &chat, "model": "opus"}))
        .await
        .unwrap();
    let small = json!({"inputTokens": 20, "outputTokens": 40, "cacheReadTokens": 500,
                       "cacheWriteTokens": 60, "costUsd": 0.5});
    let mut old = settled_turn(&mut h, &chat, "first", "m-1", "done", small, "native-0").await;
    old.emit(json!({"type": "subagent_start", "toolCallId": "sa_1", "name": "background job"}))
        .await;
    h.until("the agent is running", |h| {
        h.rt.live_thread(&chat)
            .is_some_and(|t| t.subagents_running == 1)
    })
    .await;
    h.call(
        "chat_set_model",
        json!({"chatId": &chat, "model": "local/strata/alpha"}),
    )
    .await
    .unwrap();
    assert!(
        old.no_command_for(Duration::from_millis(200))
            .await
            .is_none(),
        "the old worker stays while its agent runs"
    );
    h.send(&chat, "something private", "default").await;
    old.expect("shutdown").await;
    let mut fresh = h.worker().await;
    assert!(
        fresh.req.local.is_some(),
        "the message goes to the local model's worker"
    );
    fresh.ready().await;
    let p = fresh.expect("prompt").await;
    assert_eq!(p["text"], "something private");
    let notes = h.col(&format!(
        "SELECT text FROM messages WHERE chat_id = '{chat}' AND role = 'context' ORDER BY id"
    ));
    assert!(
        notes.iter().any(|t| t.contains("agents were stopped")),
        "{notes:?}"
    );
    old.exit().await;
    fresh.exit().await;
}

/// A model change whose conversation plainly will not fit the target's window is refused with a
/// roll over offered instead of a turn that would be cut off; taking the offer rolls the chat over
/// into a successor that runs on that model, carrying the hand-off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_change_too_large_for_the_target_offers_a_roll_over() {
    let mut h = harness();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("qwen3:8b-helm-64k".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());
    let chat = h.create().await;
    h.call("chat_set_model", json!({"chatId": &chat, "model": "opus"}))
        .await
        .unwrap();
    // The session's running totals after the turn add every turn's reading together; what
    // counts is the newest turn's own request, set below.
    let big = json!({"inputTokens": 100, "outputTokens": 900, "cacheReadTokens": 150_000,
                     "cacheWriteTokens": 2_000, "costUsd": 0.5});
    let mut w = settled_turn(
        &mut h,
        &chat,
        "a long job",
        "m-1",
        "deep into it",
        big,
        "native-1",
    )
    .await;
    // The turn's share above is huge, but the context is the newest reply's own request (462).
    let held = |h: &H| {
        h.db.snapshot::<_, rusqlite::Error>(|t| {
            Ok(crate::turn_usage::context_reported_at(t, &chat).map(|(n, _)| n))
        })
        .unwrap()
    };
    assert_eq!(held(&h), Some(462));
    // The newest reply's own request: far more than the small local window.
    h.db.write(|tx| {
        tx.execute(
            "UPDATE messages SET details = json_set(details, '$.usage.cacheReadTokens', 150000)
             WHERE chat_id = ?1 AND role = 'assistant'",
            [&chat],
        )
    })
    .unwrap();
    assert_eq!(held(&h), Some(150_062));
    let e = h
        .call(
            "chat_set_model",
            json!({"chatId": &chat, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "too_large");
    assert!(
        e.message.contains("65536") && e.message.contains("Roll this chat over instead"),
        "{}",
        e.message
    );
    // A move to a model the conversation still fits goes through: the guard only speaks where
    // the numbers say it must.
    h.call(
        "chat_set_model",
        json!({"chatId": &chat, "model": "opus[1m]"}),
    )
    .await
    .unwrap();

    // Take the offer. A move between two Claude models is told to the running worker live; it
    // then goes away here, so the hand-off the roll over asks for runs on the worker the next
    // send starts.
    let sm = w.expect("set_model").await;
    w.respond(&sm, json!({})).await;
    w.exit().await;
    let started = h
        .call(
            "chat_roll_over",
            json!({"chatId": &chat, "model": "local/ollama/qwen3:8b"}),
        )
        .await
        .unwrap();
    assert_eq!(started["started"], true);
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    assert!(p["text"].as_str().unwrap().contains("rolled over"));
    ack(&w, &p).await;
    w.emit(json!({"type": "agent_start"})).await;
    let handoff = "Goal: finish it. Left to do: part two.";
    w.emit(json!({"type": "message_start", "message": {"id": "m-2", "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_end", "message": {"id": "m-2", "role": "assistant", "content": [{"type": "text", "text": handoff}], "status": "complete"}}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-2"}))
        .await;
    w.exit().await;

    let mut w2 = h.worker().await;
    assert!(
        w2.req.local.is_some(),
        "the successor runs against the local server"
    );
    w2.ready().await;
    let first = w2.expect("prompt").await;
    let text = first["text"].as_str().unwrap();
    assert!(
        text.contains(&chat) && text.contains(handoff),
        "the successor opens with the hand-off: {text}"
    );
    h.until("old chat archived", |h| {
        h.col(&format!("SELECT archived FROM chats WHERE id = '{chat}'")) == ["1"]
    })
    .await;
    h.until("new chat opened", |h| {
        !h.events("shell_open_chat").is_empty()
    })
    .await;
    let opened = h.events("shell_open_chat").remove(0);
    let new = opened["chatId"].as_str().unwrap().to_owned();
    assert_eq!(
        opened["ifViewing"].as_str(),
        Some(chat.as_str()),
        "the window follows only while the old chat is still on screen"
    );
    assert!(
        opened["title"].as_str().is_some_and(|t| !t.is_empty()),
        "the notice names the successor: {opened:?}"
    );
    assert_eq!(
        h.col(&format!(
            "SELECT provider || '|' || model FROM chats WHERE id = '{new}'"
        )),
        ["ollama|qwen3:8b"],
        "the successor runs on the model the old chat wanted"
    );
}

/// The model server answers one request at a time. Two of Helm's chats open turns on it in turn:
/// the later one says how many would be answered before it, the first one says the server is busy
/// without a number, and neither counts as stalled while it waits. Its first words end the wait
/// (silence starts it again); a turn that ends takes the flag with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_chats_say_who_the_model_server_is_busy_with() {
    // The wait check runs on the maintenance tick, so the test speeds the tick up.
    let mut h = harness_with(Limits {
        tick: Duration::from_millis(50),
        ..limits()
    });
    // And it wants the ordinary stall rule live: five 100 ms units, so the quiet span below
    // crosses a full window and a wrongly-flagged waiting chat would show. (The harness parks
    // the window for every other test.)
    h.db.write(|tx| helm_core::prefs::set_generic(tx, "stallMinutes", &json!(5)))
        .unwrap();
    // The server says it is working on a request. How many of Helm's own chats would be answered
    // first is Helm's own count: the server cannot tell whose request is whose.
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("alpha-model".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(Some(crate::local::QueueInfo {
            busy: true,
            queued: None,
        })),
    });
    h.rt.set_local_models(local.clone());
    let cwd = h.work.to_string_lossy().into_owned();
    let ahead = |h: &H, id: &str| {
        h.col(&format!(
            "SELECT local_ahead FROM chat_activity WHERE chat_id = '{id}'"
        ))
    };
    // The chat record's own reading of it (what the header, sidebar and dock show), read the
    // way the chat ops read it; polled, since the tick that sets it runs on its own task.
    let record = |h: &H, id: &str| -> Option<i64> {
        h.db.snapshot(|tx| crate::store::get_chat(tx, id, 0))
            .unwrap()
            .unwrap()
            .local_ahead
    };

    let one = h
        .call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/alpha"}),
        )
        .await
        .unwrap();
    let first = one["id"].as_str().unwrap().to_owned();
    let two = h
        .call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/beta"}),
        )
        .await
        .unwrap();
    let second = two["id"].as_str().unwrap().to_owned();

    // The first chat's turn opens and stays quiet while the server works. Nothing of Helm's is
    // ahead of it, so the wait shows without a count — which also covers a request from outside
    // Helm holding the server, the shape of the real complaint.
    let _ = h.send(&first, "first job", "default").await;
    let mut w1 = h.worker().await;
    w1.ready().await;
    let p1 = w1.expect("prompt").await;
    ack(&w1, &p1).await;
    w1.emit(json!({"type": "agent_start"})).await;
    h.until("the quiet chat shows the wait", |h| {
        ahead(h, &first) == ["0"]
    })
    .await;
    h.until("the record carries it", |h| record(h, &first) == Some(0))
        .await;

    // The second chat's turn opens afterwards on the same server: one of ours stands ahead of it.
    let _ = h.send(&second, "second job", "default").await;
    let mut w2 = h.worker().await;
    w2.ready().await;
    let p2 = w2.expect("prompt").await;
    ack(&w2, &p2).await;
    w2.emit(json!({"type": "agent_start"})).await;
    h.until("the later chat counts one ahead", |h| {
        ahead(h, &second) == ["1"]
    })
    .await;
    assert_eq!(ahead(&h, &first), ["0"], "the first stays without a count");
    h.until("the later chat's record counts one ahead", |h| {
        record(h, &second) == Some(1)
    })
    .await;

    // Neither is a stall: standing at the model server has an explanation, so the "may be stuck"
    // flag (five units of quiet) is not set for a waiting chat.
    tokio::time::sleep(Duration::from_millis(650)).await;
    for id in [&first, &second] {
        assert_eq!(
            h.col(&format!(
                "SELECT stalled FROM chat_activity WHERE chat_id = '{id}'"
            )),
            ["0"],
            "the wait is not a stall"
        );
    }

    // The state both chats show is `queued`, with the queue position, and the server summary
    // counts them as waiting rather than working — what an orchestrator reads before starting
    // yet another local chat.
    let state_json = |h: &H, id: &str| -> Value {
        serde_json::from_str(
            &h.col(&format!(
                "SELECT COALESCE(state, 'null') FROM chat_activity WHERE chat_id = '{id}'"
            ))[0],
        )
        .unwrap()
    };
    assert_eq!(state_json(&h, &second)["kind"], "queued");
    assert_eq!(
        state_json(&h, &second)["ahead"],
        1,
        "the later chat's queued state names its place in the queue"
    );
    assert_eq!(state_json(&h, &first)["kind"], "queued");
    let loads = h.rt.local_load();
    assert_eq!(loads.len(), 1, "one server carries both");
    assert_eq!(loads[0].server_id, "strata");
    assert_eq!(loads[0].queued, 2, "both turns are waiting, not working");
    assert_eq!(loads[0].working, 0);

    // The model's first words end the wait for that chat…
    w2.emit(json!({"type": "message_start", "message": {"id": "m1", "role": "assistant", "content": []}}))
        .await;
    h.until("the wait ends at the first words", |h| {
        ahead(h, &second) == ["NULL"]
    })
    .await;
    // …and a unit of quiet starts it again: the answer stopped with the first chat still
    // unanswered ahead of it.
    h.until("a fresh wait counts the first chat again", |h| {
        ahead(h, &second) == ["1"]
    })
    .await;

    // A turn that ends takes the flag with it.
    w1.emit(json!({"type": "message_start", "message": {"id": "m0", "role": "assistant", "content": []}}))
        .await;
    w1.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-a"}))
        .await;
    h.until("the first chat is idle and not waiting", |h| {
        h.status(&first) == "idle" && ahead(h, &first) == ["NULL"]
    })
    .await;
    // The second chat: nothing of Helm's is ahead now, but the server's own answer (read at most
    // every ten seconds, so still the last one) says it is working: a wait without a count.
    h.until("the second chat waits without a count", |h| {
        ahead(h, &second) == ["0"]
    })
    .await;
    w2.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-b"}))
        .await;
    h.until("the second turn ends", |h| {
        h.status(&second) == "idle" && ahead(h, &second) == ["NULL"]
    })
    .await;
    assert_eq!(record(&h, &second), None, "an idle chat carries no flag");
    w1.exit().await;
    w2.exit().await;
}

/// A local model server carries a soft cap of our chats. A new local turn whose server already
/// carries the cap waits at its start — visibly queued, with its place in line, and nothing
/// asked of the server — and opens by itself when a place frees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_local_turns_wait_when_the_server_carries_its_cap() {
    let mut h = harness_with(Limits {
        tick: Duration::from_millis(50),
        ..limits()
    });
    h.db.write(|tx| helm_core::prefs::set_generic(tx, "localChatCap", &json!(2)))
        .unwrap();
    let local = Arc::new(FakeLocal {
        turns: Mutex::new(Vec::new()),
        outcome: Mutex::new(Ok("m-one".into())),
        gate: Mutex::new(None),
        finished: Mutex::new(Vec::new()),
        refreshed: Mutex::new(Vec::new()),
        held_outcome: Mutex::new(None),
        queue: Mutex::new(None),
    });
    h.rt.set_local_models(local.clone());
    let cwd = h.work.to_string_lossy().into_owned();
    let asked =
        |_: &H, id: &str| -> bool { local.turns.lock().unwrap().iter().any(|t| t.chat_id == id) };

    let new_id = |r: Value| r["id"].as_str().unwrap().to_owned();
    let first = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    h.send(&first, "job one", "default").await;
    let mut w1 = h.worker().await;
    w1.ready().await;
    let p1 = w1.expect("prompt").await;
    ack(&w1, &p1).await;
    w1.emit(json!({"type": "agent_start"})).await;
    let second = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    h.send(&second, "job two", "default").await;
    let mut w2 = h.worker().await;
    w2.ready().await;
    let p2 = w2.expect("prompt").await;
    ack(&w2, &p2).await;
    w2.emit(json!({"type": "agent_start"})).await;

    // The cap (two) stands: the third chat's turn waits at its start.
    let third = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    let (sid3, _) = h.send(&third, "job three", "default").await;
    h.until("the third waits at its start", |h| {
        h.status(&third) == "starting" && h.state(&sid3) == "saved" && !asked(h, &third)
    })
    .await;
    h.until("and shows its place in line", |h| {
        h.col(&format!(
            "SELECT local_ahead FROM chat_activity WHERE chat_id = '{third}'"
        )) == ["1"]
            && serde_json::from_str::<Value>(
                &h.col(&format!(
                    "SELECT COALESCE(state, 'null') FROM chat_activity WHERE chat_id = '{third}'"
                ))[0],
            )
            .unwrap()["kind"]
                == "queued"
    })
    .await;
    assert_eq!(h.state(&sid3), "saved", "nothing was sent to the server");
    assert!(!asked(&h, &third), "and its model was never asked to load");
    let loads = h.rt.local_load();
    assert_eq!(
        loads[0].working + loads[0].queued,
        3,
        "the summary counts the wait with the work"
    );
    assert!(
        loads[0].queued >= 1,
        "the waiting chat shows as queued, not working"
    );

    // A place frees when the first turn ends: the waiting one goes out on its own, no new send.
    w1.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-c1"}))
        .await;
    h.until("the freed place takes the third turn", |h| asked(h, &third))
        .await;
    let mut w3 = h.worker().await;
    assert_eq!(
        w3.req.local.as_ref().map(|l| l.model.as_str()),
        Some("m-one"),
        "launched on the server, not piled onto it"
    );
    w3.ready().await;
    let p3 = w3.expect("prompt").await;
    assert_eq!(
        p3["submissionId"],
        sid3.as_str(),
        "the same message opens the turn"
    );

    // Cap one, one chat working and two waiting: the waiters do not count each other as busy,
    // so the oldest starts the moment the place frees, and the next only after it.
    ack(&w3, &p3).await;
    w3.emit(json!({"type": "agent_start"})).await;
    w2.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-c2"}))
        .await;
    w3.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-c3"}))
        .await;
    h.until("the first three chats are idle", |h| {
        [&first, &second, &third]
            .iter()
            .all(|c| h.status(c) == "idle")
    })
    .await;
    h.db.write(|tx| helm_core::prefs::set_generic(tx, "localChatCap", &json!(1)))
        .unwrap();
    let c4 = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    h.send(&c4, "job four", "default").await;
    let mut w4 = h.worker().await;
    w4.ready().await;
    let p4 = w4.expect("prompt").await;
    ack(&w4, &p4).await;
    w4.emit(json!({"type": "agent_start"})).await;
    let c5 = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    let (sid5, _) = h.send(&c5, "job five", "default").await;
    h.until("the fifth waits for the place", |h| {
        h.status(&c5) == "starting" && !asked(h, &c5)
    })
    .await;
    let c6 = new_id(
        h.call(
            "chat_create",
            json!({"cwd": &cwd, "model": "local/strata/x"}),
        )
        .await
        .unwrap(),
    );
    let (sid6, _) = h.send(&c6, "job six", "default").await;
    let ahead_of = |h: &H, id: &str| {
        h.col(&format!(
            "SELECT COALESCE(local_ahead, 'NULL') FROM chat_activity WHERE chat_id = '{id}'"
        ))
    };
    h.until("the sixth waits behind the fifth", |h| {
        ahead_of(h, &c5) == ["1"] && ahead_of(h, &c6) == ["2"] && !asked(h, &c6)
    })
    .await;
    w4.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-c4"}))
        .await;
    h.until("the oldest waiter takes the place", |h| asked(h, &c5))
        .await;
    let mut w5 = h.worker().await;
    w5.ready().await;
    let p5 = w5.expect("prompt").await;
    assert_eq!(p5["submissionId"], sid5.as_str(), "in the order they sent");
    ack(&w5, &p5).await;
    w5.emit(json!({"type": "agent_start"})).await;
    h.until("the sixth keeps waiting while the fifth works", |h| {
        ahead_of(h, &c6) == ["1"] && !asked(h, &c6)
    })
    .await;
    w5.emit(json!({"type": "agent_settled", "outcome": "complete", "sessionId": "native-c5"}))
        .await;
    h.until("and the sixth goes when it ends", |h| asked(h, &c6))
        .await;
    let mut w6 = h.worker().await;
    w6.ready().await;
    let p6 = w6.expect("prompt").await;
    assert_eq!(p6["submissionId"], sid6.as_str());
}

/// The saved Claude token reaches a Claude worker or helper only as the handover variable. A
/// local-model start never gets it, a stray value from the surrounding environment never
/// survives, it is not part of the launch file, and the usage meter's helper does not read it.
#[test]
fn the_saved_claude_token_is_handed_over_only_to_claude_starts() {
    use helm_workers::env::EnvMap;
    use helm_workers::launch::CLAUDE_TOKEN_ENV;
    const TOKEN: &str = "sk-ant-oat01-fake-chat-token-for-tests";
    let reads = Arc::new(AtomicU32::new(0));
    let source: ChatTokenSource = {
        let reads = reads.clone();
        Arc::new(move || {
            reads.fetch_add(1, Ordering::SeqCst);
            Some(TOKEN.to_owned())
        })
    };
    let none: ChatTokenSource = Arc::new(|| None);

    // A Claude worker or helper with a saved token: the variable, nothing else changes.
    let mut env = EnvMap::default();
    apply_chat_token(&mut env, Some(&source), false);
    assert_eq!(env.get(CLAUDE_TOKEN_ENV), Some(TOKEN));
    // Without a source or a token, the start is as it always was, and an inherited value (say
    // from the person's own environment) is dropped rather than passed on.
    for source in [None, Some(&none)] {
        let mut env = EnvMap::default();
        env.set(CLAUDE_TOKEN_ENV, "inherited");
        apply_chat_token(&mut env, source, false);
        assert_eq!(env.get(CLAUDE_TOKEN_ENV), None);
    }
    // A local-model start never gets it, and the store is not even read for one.
    let before = reads.load(Ordering::SeqCst);
    let mut env = EnvMap::default();
    env.set(CLAUDE_TOKEN_ENV, "inherited");
    apply_chat_token(&mut env, Some(&source), true);
    assert_eq!(env.get(CLAUDE_TOKEN_ENV), None);
    assert_eq!(reads.load(Ordering::SeqCst), before);

    // Never in the launch file (which the worker reads from disk, and logs can show).
    let rt = helm_workers::launch::ClaudeRuntime::default();
    let claude = helm_workers::launch::claude_launch(&rt, None, None, None, None);
    assert!(!serde_json::to_string(&claude).unwrap().contains(TOKEN));
    // Nor in what an env map prints about itself.
    assert!(!format!("{:?}", claude).contains(TOKEN));

    // The usage meter keeps the normal sign-in: its helper's source never names the variable.
    let usage = include_str!("../../usage/src/helper.rs");
    assert!(!usage.contains("CLAUDE_TOKEN") && !usage.contains("chat_token"));
}
