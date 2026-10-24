use super::*;
use helm_core::db::MigrationRegistry;
use helm_ipc::ApiError;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

const T0: i64 = 1_800_000_000_000; // 2027-01-15

// ------------------------------------------------------------------ the mirror state machine

fn busy(running: &[&str], recent: &[&str]) -> Snap {
    Snap {
        busy: true,
        running: running.iter().map(|s| s.to_string()).collect(),
        recent: recent.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn idle(text: Option<&str>) -> Snap {
    Snap {
        final_text: text.map(str::to_owned),
        max_id: 9,
        ..Default::default()
    }
}

#[test]
fn one_status_while_running_debounced_then_the_final_as_a_new_message() {
    let mut m = Mirror::new(0, T0);
    assert_eq!(
        m.step(&busy(&[], &[]), T0),
        vec![Up::Status("Working…".into())]
    );
    // Unchanged: nothing sent.
    assert!(m.step(&busy(&[], &[]), T0 + 5_000).is_empty());
    // Changed, but within the gap of the last update: held back, then sent once due.
    let b = busy(&["Bash"], &["Read"]);
    let mut m = Mirror::new(0, T0);
    m.step(&busy(&[], &[]), T0);
    assert!(m.step(&b, T0 + STATUS_GAP_MS - 1).is_empty());
    assert_eq!(
        m.step(&b, T0 + STATUS_GAP_MS),
        vec![Up::Status("Working…\nRunning: Bash\nRecent: Read".into())]
    );
    // Idle once: wait for the last row to flush; idle twice: the final answer, and done.
    assert!(m.step(&idle(Some("The answer.")), T0 + 9_000).is_empty());
    assert_eq!(
        m.step(&idle(Some("The answer.")), T0 + 9_500),
        vec![Up::Final("The answer.".into())]
    );
    assert!(m.done);
    assert!(m.step(&busy(&[], &[]), T0 + 10_000).is_empty());
}

#[test]
fn a_question_goes_up_once_with_numbered_options_and_no_status_meanwhile() {
    let mut m = Mirror::new(0, T0);
    let mut s = busy(&[], &[]);
    s.question = Some(("q1".into(), "Colour?\n1. Red\n2. Blue".into()));
    assert_eq!(
        m.step(&s, T0),
        vec![Up::Final("Colour?\n1. Red\n2. Blue".into())]
    );
    assert!(m.step(&s, T0 + 10_000).is_empty(), "asked once");
    // Answered: the status comes back (the hub posts a new status message).
    s.question = None;
    assert_eq!(m.step(&s, T0 + 20_000), vec![Up::Status("Working…".into())]);
    // An approval Telegram may not give: said once, then quiet until it is answered in Helm.
    s.blocked = Some(("a1".into(), "Waiting for you in Helm: Allow Bash?".into()));
    assert_eq!(
        m.step(&s, T0 + 30_000),
        vec![Up::Final("Waiting for you in Helm: Allow Bash?".into())]
    );
    assert!(m.step(&s, T0 + 40_000).is_empty(), "said once");
    s.blocked = None;
    assert_eq!(m.step(&s, T0 + 50_000), vec![Up::Status("Working…".into())]);
    assert!(!m.done);
}

#[test]
fn a_queued_telegram_message_keeps_the_mirror_for_its_own_turn() {
    let mut m = Mirror::new(0, T0);
    m.step(&busy(&[], &[]), T0);
    let mut s = idle(Some("First."));
    s.queued = true;
    m.step(&s, T0 + 1_000);
    assert_eq!(m.step(&s, T0 + 1_500), vec![Up::Final("First.".into())]);
    assert!(!m.done);
    assert_eq!(m.baseline, 9, "the next answer is read after this one");
    m.step(&busy(&[], &[]), T0 + 2_000);
    m.step(&idle(Some("Second.")), T0 + 3_000);
    assert_eq!(
        m.step(&idle(Some("Second.")), T0 + 3_500),
        vec![Up::Final("Second.".into())]
    );
    assert!(m.done);
}

#[test]
fn a_turn_that_never_starts_is_not_left_silent() {
    let mut m = Mirror::new(0, T0);
    assert!(m.step(&idle(None), T0 + START_WAIT_MS - 1).is_empty());
    let out = m.step(&idle(None), T0 + START_WAIT_MS);
    assert!(
        matches!(&out[..], [Up::Final(t)] if t.contains("not started")),
        "{out:?}"
    );
    assert!(m.done);
}

#[test]
fn the_send_mode_never_aborts() {
    assert_eq!(host_mode(SendMode::Queue), HostSendMode::Queue);
    assert_eq!(host_mode(SendMode::Asap), HostSendMode::Steer);
    assert_eq!(host_mode(SendMode::Interrupt), HostSendMode::Steer);
}

#[test]
fn verbatim_prefixes_are_dropped() {
    assert_eq!(
        plain_path(std::path::Path::new(r"\\?\C:\p\telegram")),
        r"C:\p\telegram"
    );
    assert_eq!(plain_path(std::path::Path::new(r"C:\p")), r"C:\p");
}

// ------------------------------------------------------------------ the real send path

struct NoWorker;
impl helm_chats::runtime::Spawner for NoWorker {
    fn spawn(
        &self,
        _req: &helm_chats::runtime::SpawnRequest,
    ) -> Result<helm_chats::runtime::Spawned, String> {
        Err("no worker in this test".into())
    }
}

/// The ops the sink calls: chat_create writes a real chat; the rest are recorded.
struct FakeOps {
    db: Arc<Database>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl OpInvoker for FakeOps {
    fn invoke<'a>(
        &'a self,
        op: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ApiError>> + Send + 'a>> {
        self.calls
            .lock()
            .unwrap()
            .push((op.to_owned(), args.clone()));
        let r = match op {
            "chat_create" => {
                let cwd = args["cwd"].as_str().unwrap_or_default().to_owned();
                self.db
                    .write(|tx| helm_chats::store::create_chat(tx, &cwd, None, None, 1))
                    .map(|id| json!({ "id": id }))
                    .map_err(|e| ApiError::internal(e.to_string()))
            }
            _ => Ok(Value::Null),
        };
        Box::pin(async move { r })
    }
}

struct Env {
    dir: tempfile::TempDir,
    db: Arc<Database>,
    ops: Arc<FakeOps>,
    ctx: Ctx,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    helm_chats::register_migrations(&mut reg);
    helm_consent::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
    let rt = ChatRuntime::new(
        db.clone(),
        Arc::new(helm_chats::clock::Clock::new(0)),
        Arc::new(helm_ipc::NullSink),
        Arc::new(NoWorker),
        helm_chats::RuntimeConfig {
            profile_dir: dir.path().join("prof"),
            limits: helm_chats::runtime::Limits::default(),
        },
        tokio::runtime::Handle::current(),
    );
    let ops = Arc::new(FakeOps {
        db: db.clone(),
        calls: Mutex::new(vec![]),
    });
    let ctx = Ctx {
        rt,
        db: db.clone(),
        ops: ops.clone(),
        handle: tokio::runtime::Handle::current(),
        tag_path: telegram::tag_path(&dir.path().join("prof")),
        folder: plain_path(&dir.path().join("prof").join("telegram")),
        policy: TagPolicy::default(),
        answered: Arc::new(Mutex::new(VecDeque::new())),
        answered_path: telegram::answered_path(&dir.path().join("prof")),
    };
    Env { dir, db, ops, ctx }
}

fn msg(id: &str, text: &str) -> TelegramMessage {
    TelegramMessage {
        id: id.into(),
        text: text.into(),
        sent_at: 0,
        age_minutes: None,
        bot: Some("helm_bot".into()),
    }
}

async fn place_async(c: &Ctx, m: TelegramMessage) -> Result<Placed, String> {
    let c = c.clone();
    tokio::task::spawn_blocking(move || place(&c, &m, helm_core::now_ms()))
        .await
        .unwrap()
}

/// (id, origin, mode, state, text) of every submission in the chat.
fn subs(db: &Database, chat: &str) -> Vec<(String, String, String, String, String)> {
    db.snapshot(|tx| {
        let mut st = tx.prepare(
            "SELECT s.id, COALESCE(m.origin, ''), s.mode, s.state, s.text FROM submissions s
             LEFT JOIN submission_meta m ON m.submission_id = s.id WHERE s.chat_id = ?1
             ORDER BY s.created_at, s.id",
        )?;
        let rows = st
            .query_map([chat], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>(rows)
    })
    .unwrap()
}

fn sql(db: &Database, q: &str, p: &[&dyn rusqlite::ToSql]) {
    db.write(|tx| tx.execute(q, p).map(|_| ())).unwrap();
}

/// Let the chat's actor finish reacting to the last send (it has no worker in these tests).
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}

fn chat_of(p: &Placed) -> String {
    match p {
        Placed::Sent { chat, .. } | Placed::Answered { chat, .. } => chat.clone(),
        Placed::Already => String::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_starts_the_telegram_chat_with_origin_telegram_and_is_stored_once() {
    let e = env();
    let p = place_async(&e.ctx, msg("hub-1", "hello from the phone"))
        .await
        .unwrap();
    assert!(matches!(p, Placed::Sent { .. }));
    let chat = chat_of(&p);
    let title: String =
        e.db.snapshot(|tx| {
            tx.query_row("SELECT title FROM chats WHERE id = ?1", [&chat], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert!(title.starts_with("Telegram helm_bot "), "{title}");
    let calls = e.ops.calls.lock().unwrap().clone();
    assert_eq!(calls[0].0, "chat_create");
    assert!(
        calls[0].1["cwd"].as_str().unwrap().ends_with("telegram"),
        "{calls:?}"
    );
    let s = subs(&e.db, &chat);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].0, telegram::submission_id("hub-1"));
    assert_eq!(s[0].1, "telegram", "never the user's typed words");
    assert!(s[0].4.starts_with(telegram::BRIEF_NOTE));
    assert!(s[0].4.ends_with("hello from the phone"));
    // The hub resends it (no ack reached it): the same submission, nothing new.
    sql(
        &e.db,
        "UPDATE chats SET status = 'idle' WHERE id = ?1",
        &[&chat],
    );
    let again = place_async(&e.ctx, msg("hub-1", "hello from the phone"))
        .await
        .unwrap();
    assert_eq!(
        again,
        Placed::Already,
        "never stored twice, nor taken as an answer"
    );
    assert_eq!(subs(&e.db, &chat).len(), 1);
    // Resent while its turn runs (Helm restarted before the ack): still stored once, and the
    // turn is watched so the phone hears how it goes.
    sql(
        &e.db,
        "UPDATE chats SET status = 'running' WHERE id = ?1",
        &[&chat],
    );
    let running = place_async(&e.ctx, msg("hub-1", "hello from the phone"))
        .await
        .unwrap();
    assert!(
        matches!(&running, Placed::Sent { chat: c, .. } if *c == chat),
        "{running:?}"
    );
    assert_eq!(subs(&e.db, &chat).len(), 1);
    sql(
        &e.db,
        "UPDATE chats SET status = 'idle' WHERE id = ?1",
        &[&chat],
    );
    // The next message attaches to the tagged chat.
    let next = place_async(&e.ctx, msg("hub-2", "and again"))
        .await
        .unwrap();
    assert_eq!(chat_of(&next), chat);
    assert_eq!(
        e.ops
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(o, _)| o == "chat_create")
            .count(),
        1
    );
    // A message that already answered a question (remembered across a restart) is resent:
    // it is acknowledged and never becomes a prompt.
    let path = e.ctx.answered_path.clone();
    telegram::remember_answered(&path, &mut VecDeque::new(), "hub-3").unwrap();
    let restarted = Ctx {
        answered: Arc::new(Mutex::new(telegram::load_answered(&path))),
        ..e.ctx.clone()
    };
    let dup = place_async(&restarted, msg("hub-3", "2")).await.unwrap();
    assert_eq!(dup, Placed::Already);
    assert_eq!(subs(&e.db, &chat).len(), 2);
    drop(e.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_chat_gets_a_steer_by_default_and_a_follow_up_in_queue_mode() {
    let e = env();
    let chat = chat_of(&place_async(&e.ctx, msg("a", "first")).await.unwrap());
    settle().await;
    sql(
        &e.db,
        "UPDATE chats SET status = 'running' WHERE id = ?1",
        &[&chat],
    );
    place_async(&e.ctx, msg("b", "steer me")).await.unwrap();
    let s = subs(&e.db, &chat);
    let b = s
        .iter()
        .find(|r| r.0 == telegram::submission_id("b"))
        .unwrap();
    assert_eq!(
        (b.1.as_str(), b.2.as_str()),
        ("telegram", "steer"),
        "asap: a steer, never a stop"
    );
    // The user's send behaviour says queue: it waits behind the turn.
    settle().await;
    sql(
        &e.db,
        "UPDATE chats SET status = 'running' WHERE id = ?1",
        &[&chat],
    );
    e.db.write(|tx| {
        helm_core::prefs::set(
            tx,
            helm_core::prefs::SEND_BEHAVIOR,
            &json!({"mode": "queue"}),
        )
        .map_err(|_| rusqlite::Error::InvalidQuery)
    })
    .unwrap();
    place_async(&e.ctx, msg("c", "later please")).await.unwrap();
    let s = subs(&e.db, &chat);
    let c = s
        .iter()
        .find(|r| r.0 == telegram::submission_id("c"))
        .unwrap();
    assert_eq!((c.2.as_str(), c.3.as_str()), ("followUp", "queued"));
}

/// Trust boundary: a Telegram message never counts as the user's consent, through the real send
/// path. The refusal is "not answered": the row is never even read as the user's words. The
/// control flips the same row's stored origin to `typed` (and drops the brief note), and then
/// the same words do count, so the origin is what refuses it.
#[tokio::test(flavor = "multi_thread")]
async fn a_telegram_message_through_the_real_send_path_never_counts_as_consent() {
    let e = env();
    let chat = chat_of(&place_async(&e.ctx, msg("warm", "hi")).await.unwrap());
    let mut kinds = helm_consent::AllowKinds::new();
    kinds.register(helm_consent::kinds::test_kind());
    let target = json!({ "name": "alpha" });
    let asked = helm_core::now_ms() - 1_000;
    e.db.write::<_, helm_consent::Refusal>(|tx| {
        helm_consent::store::record_ask(tx, &kinds, &chat, "test_action", &target, asked)
    })
    .unwrap();
    sql(
        &e.db,
        "UPDATE chats SET status = 'idle' WHERE id = ?1",
        &[&chat],
    );
    place_async(&e.ctx, msg("yes", "Yes. Run the test action alpha."))
        .await
        .unwrap();
    let sid = telegram::submission_id("yes");
    let spend = |db: &Database| {
        db.write::<_, helm_consent::Refusal>(|tx| {
            helm_consent::store::spend_words(
                tx,
                &kinds,
                &chat,
                "test_action",
                &target,
                "t",
                helm_core::now_ms() + 1_000,
            )
        })
        .map(|_| ())
        .map_err(|r| r.0)
    };
    let refused = spend(&e.db).unwrap_err();
    assert!(refused.contains("not answered"), "{refused}");
    sql(
        &e.db,
        "UPDATE submission_meta SET origin = 'typed' WHERE submission_id = ?1",
        &[&sid],
    );
    // With the origin flipped the row is read at all: refused now only for the brief note's
    // extra line, and with the bare words it counts.
    let r = spend(&e.db).unwrap_err();
    assert!(r.contains("more than one line"), "{r}");
    sql(
        &e.db,
        "UPDATE submissions SET text = 'Yes. Run the test action alpha.' WHERE id = ?1",
        &[&sid],
    );
    let r = spend(&e.db);
    assert!(r.is_ok(), "control: the same words typed do count: {r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relayed_outside_send_is_stored_as_another_chat_never_as_typed() {
    let e = env();
    let chat =
        e.db.write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
    let mut services = helm_ipc::Services::new();
    services.insert(e.db.clone());
    services.insert(Arc::new(e.ctx.rt.clone()));
    let exec = AppExecutor::default();
    exec.wire(crate::exec::Wiring {
        services: Arc::new(services),
        events: Arc::new(helm_ipc::NullSink),
        tools: Arc::new(helm_hosttools::HostTools::new(Arc::new(
            helm_hosttools::HostToolRegistry::new(),
        ))),
    });
    let call = crate::gate::admit(
        "app_send_message",
        &json!({"chatId": chat, "message": "from remote", "mode": "queue"}),
    )
    .unwrap();
    use crate::client::Executor;
    let r = exec.run("remote", call).await.unwrap();
    assert_eq!(r["chatId"], json!(chat));
    let s = subs(&e.db, &chat);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].1, "other_chat", "stored as another agent's message");
    assert!(s[0].4.contains("from remote"));
}

fn wired_sink(e: &Env) -> ChatSink {
    let mut services = helm_ipc::Services::new();
    services.insert(e.db.clone());
    services.insert(Arc::new(e.ctx.rt.clone()));
    let exec = Arc::new(AppExecutor::default());
    exec.wire(crate::exec::Wiring {
        services: Arc::new(services),
        events: Arc::new(helm_ipc::NullSink),
        tools: Arc::new(helm_hosttools::HostTools::new(Arc::new(
            helm_hosttools::HostToolRegistry::new(),
        ))),
    });
    ChatSink::new(exec, e.dir.path().join("prof"))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sink_delivers_only_once_wired_and_stop_reaches_the_tagged_chat() {
    let e = env();
    // Unwired: a delivery is refused (so it stays queued at the hub) and stop does nothing.
    let bare = ChatSink::new(Arc::new(AppExecutor::default()), e.dir.path().join("prof"));
    assert!(bare.deliver(msg("x", "y")).await.is_err());
    bare.stop().await;
    let sink = wired_sink(&e);
    assert!(
        sink.deliver(msg("x", "y")).await.is_err(),
        "no op dispatcher yet"
    );
    sink.attach_ops(e.ops.clone());
    sink.deliver(msg("m1", "hello")).await.unwrap();
    let tag = telegram::load_tag(&e.ctx.tag_path).unwrap();
    assert_eq!(subs(&e.db, &tag.chat_id)[0].1, "telegram");
    sink.stop().await;
    let calls = e.ops.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|(o, a)| o == "chat_stop" && a["chatId"] == json!(tag.chat_id)),
        "{calls:?}"
    );
}
