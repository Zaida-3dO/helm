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

/// A step where every answer it offered reached the phone — what the real mirror reports back
/// through [`Mirror::ack`]. Tests that watch an exchange close use it; a test of a failed send
/// calls `ack` itself.
fn step_ok(m: &mut Mirror, s: &Snap, now: i64) -> Vec<Up> {
    let out = m.step(s, now);
    for o in &out {
        if let Up::Final(t) = o {
            m.ack(t, true);
        }
    }
    out
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
    // The lists carry short phrases describing the calls, not bare tool names.
    let b = busy(
        &["Checking the nightly backup"],
        &["Reading winter bills.md"],
    );
    let mut m = Mirror::new(0, T0);
    m.step(&busy(&[], &[]), T0);
    assert!(m.step(&b, T0 + STATUS_GAP_MS - 1).is_empty());
    assert_eq!(
        m.step(&b, T0 + STATUS_GAP_MS),
        vec![Up::Status(
            "Working…\nRunning: Checking the nightly backup\nRecent: Reading winter bills.md"
                .into(),
        )]
    );
    // Idle once: wait for the last row to flush; idle twice: the final answer, and once the
    // phone has it, done.
    assert!(m.step(&idle(Some("The answer.")), T0 + 9_000).is_empty());
    assert_eq!(
        step_ok(&mut m, &idle(Some("The answer.")), T0 + 9_500),
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
fn a_second_message_in_the_same_exchange_is_mirrored_within_the_running_turn() {
    // A phone message that joins a running turn keeps it busy (Claude reads it at a tool
    // boundary), so one mirror carries both answers and ends when the exchange settles.
    let mut m = Mirror::new(0, T0);
    m.step(&busy(&[], &[]), T0);
    m.step(&busy(&[], &["Thinking."]), T0 + 1_000);
    assert_eq!(m.step(&idle(Some("First.")), T0 + 2_000), vec![]);
    assert!(!m.done, "the first idle sample only starts the settle");
    assert_eq!(
        step_ok(&mut m, &idle(Some("Second.")), T0 + 2_500),
        vec![Up::Final("Second.".into())]
    );
    assert!(
        m.done,
        "nothing waits behind the turn, so one Final closes the exchange"
    );
}

#[test]
fn an_answer_goes_up_the_moment_its_turn_closes_even_while_the_chat_stays_busy() {
    // After the earlier delivery fix a message the user queued is read at the turn's first
    // boundary and opens the next turn, so the chat may never show idle between the turns of an
    // exchange. The row that ended the turn is the sign the answer is ready: it goes up then,
    // once, and the next turn's answer goes up the same way.
    let mut m = Mirror::new(0, T0);
    let mut s = busy(&[], &[]);
    s.turn_done = Some((7, "First.".into()));
    assert_eq!(
        m.step(&s, T0),
        vec![Up::Final("First.".into())],
        "the answer waits for no idle sample"
    );
    // The line was down: the same answer is offered again, and goes when the line does.
    m.ack("First.", false);
    assert_eq!(m.step(&s, T0 + 1_000), vec![Up::Final("First.".into())]);
    m.ack("First.", true);
    assert!(!m.done, "the chat is still busy: the exchange is not over");
    // Confirmed sent: the same row is not sent up again, and the status line resumes.
    assert_eq!(m.step(&s, T0 + 2_000), vec![Up::Status("Working…".into())]);
    // The next turn's answer goes up when its row appears.
    s.turn_done = Some((11, "Second.".into()));
    assert_eq!(
        step_ok(&mut m, &s, T0 + 3_000),
        vec![Up::Final("Second.".into())]
    );
    assert!(!m.done);
    // When the exchange finally settles, the close says nothing a second time.
    let mut s = idle(Some("Second."));
    s.turn_done = Some((11, "Second.".into()));
    assert!(step_ok(&mut m, &s, T0 + 4_000).is_empty());
    assert!(step_ok(&mut m, &s, T0 + 4_500).is_empty());
    assert!(
        m.done,
        "the settled samples close an already-answered exchange"
    );
}

#[test]
fn an_owed_answer_is_offered_again_until_it_goes_up_and_the_close_completes_after_it() {
    // The line was down the moment the turn closed; the chat settles before it recovers. The
    // owed answer comes first on every step; the exchange closes after the send that carried
    // it, not before.
    let mut m = Mirror::new(0, T0);
    let mut s = busy(&[], &[]);
    s.turn_done = Some((5, "The answer.".into()));
    assert_eq!(m.step(&s, T0), vec![Up::Final("The answer.".into())]);
    m.ack("The answer.", false);
    let mut i = idle(Some("The answer."));
    i.turn_done = Some((5, "The answer.".into()));
    assert_eq!(
        m.step(&i, T0 + 1_000),
        vec![Up::Final("The answer.".into())],
        "the owed answer is offered again, not a second one"
    );
    m.ack("The answer.", false);
    assert_eq!(
        m.step(&i, T0 + 1_500),
        vec![Up::Final("The answer.".into())]
    );
    m.ack("The answer.", true);
    assert!(
        !m.done,
        "the answer reached the phone while the chat was idle, but the settled samples close it"
    );
    assert!(step_ok(&mut m, &i, T0 + 2_000).is_empty());
    assert!(step_ok(&mut m, &i, T0 + 2_500).is_empty());
    assert!(
        m.done,
        "settled samples after the send: nothing owed, so the exchange closes quiet"
    );
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
        sent_path: telegram::sent_path(&dir.path().join("prof")),
    };
    Env { dir, db, ops, ctx }
}

fn msg(id: &str, text: &str) -> TelegramMessage {
    TelegramMessage {
        images: vec![],
        files: vec![],
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

/// The recent line reads the few words stored with each call, keeps to the last three, and
/// falls back to the bare name when a call has none. (Invented calls; none is real.)
#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_lists_the_last_three_calls_in_their_own_words() {
    let e = env();
    let chat =
        e.db.write(|tx| helm_chats::store::create_chat(tx, "C:/work", None, None, 1))
            .unwrap();
    sql(
        &e.db,
        "UPDATE chats SET status = 'running' WHERE id = ?1",
        &[&chat],
    );
    let seed = |tx: &rusqlite::Transaction<'_>, key: &str, name: &str, details: Option<&str>| {
        tx.execute(
            "INSERT INTO messages(chat_id, source_key, role, tool_name, tool_call_id,
                                  status, timestamp, details)
             VALUES (?1, ?2, 'toolResult', ?3, ?4, 'complete', 1000, ?5)",
            rusqlite::params![&chat, key, name, key, details],
        )
        .map(|_| ())
    };
    e.db.write(|tx| {
        seed(tx, "t:a", "Read", None)?;
        seed(
            tx,
            "t:b",
            "Grep",
            Some(r#"{"toolSummary":"Searching HVAC filter"}"#),
        )?;
        seed(
            tx,
            "t:c",
            "Bash",
            Some(r#"{"toolSummary":"Checking the nightly backup"}"#),
        )?;
        seed(
            tx,
            "t:d",
            "Read",
            Some(r#"{"toolSummary":"Reading winter bills.md"}"#),
        )?;
        seed(tx, "t:e", "Grep", Some("{}"))
    })
    .unwrap();
    let snap = snapshot(&e.ctx.rt, &e.db, &chat, 0, false);
    assert!(snap.busy, "the chat is running");
    assert!(snap.running.is_empty(), "no worker is live in this test");
    assert_eq!(
        snap.recent,
        [
            "Checking the nightly backup",
            "Reading winter bills.md",
            "Grep"
        ],
        "the last three, in words, with the bare name as fallback"
    );
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
    // Pictures come in as the composer's do, stored and linked to the submission. One the chat
    // refuses (here an animated PNG) does not hold the message back: its text goes alone, with a
    // note saying why.
    use base64::Engine as _;
    let png = |animated: bool| {
        let chunk = |tag: &[u8], data: &[u8]| {
            let mut c = (data.len() as u32).to_be_bytes().to_vec();
            c.extend([tag, data, &[0; 4]].concat());
            c
        };
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.extend(chunk(
            b"IHDR",
            &[&[0, 0, 0, 10, 0, 0, 0, 10][..], &[8, 6, 0, 0, 0]].concat(),
        ));
        if animated {
            b.extend(chunk(b"acTL", &[0; 8]));
        }
        b.extend(chunk(b"IDAT", &[0; 4]));
        crate::client::TelegramFile {
            name: "photo.png".into(),
            media_type: "image/png".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b),
        }
    };
    for (id, animated) in [("hub-4", false), ("hub-5", true)] {
        let m = TelegramMessage {
            images: vec![png(animated)],
            ..msg(id, "what is this?")
        };
        place_async(&e.ctx, m).await.unwrap();
        let sid = telegram::submission_id(id);
        let (attached, text): (i64, String) =
            e.db.snapshot(|tx| {
                Ok::<_, rusqlite::Error>((
                    tx.query_row(
                        "SELECT count(*) FROM chat_attachments WHERE submission_id = ?1",
                        [&sid],
                        |r| r.get(0),
                    )?,
                    tx.query_row("SELECT text FROM submissions WHERE id = ?1", [&sid], |r| {
                        r.get(0)
                    })?,
                ))
            })
            .unwrap();
        assert_eq!(attached, i64::from(!animated), "{id}");
        assert_eq!(
            text.contains("could not be attached. Animated images"),
            animated,
            "{id}: {text}"
        );
    }
    // Any other file is saved where the agent can read it, under a name every Windows folder
    // takes, and listed for the agent beside the prompt.
    let file = |name: &str, bytes: &[u8]| crate::client::TelegramFile {
        name: name.into(),
        media_type: "application/octet-stream".into(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    };
    let long = format!("{}.tar.gz", "x".repeat(300));
    let dotted = format!("{}{}.pdf", "y".repeat(94), ".".repeat(6));
    let names = [
        ("report.pdf", "report.pdf".to_owned()),
        ("nul.txt", "_nul.txt".to_owned()),
        ("CON", "_CON".to_owned()),
        ("com1.log", "_com1.log".to_owned()),
        ("a/b\\c:d?.txt", "a_b_c_d_.txt".to_owned()),
        ("  trailing. . ", "trailing".to_owned()),
        ("..", "file".to_owned()),
        (long.as_str(), format!("{}.gz", "x".repeat(97))),
        // Cut inside a run of dots: the dots go too, never left before the extension.
        (dotted.as_str(), format!("{}.pdf", "y".repeat(94))),
    ];
    let m = TelegramMessage {
        files: names.iter().map(|(n, _)| file(n, n.as_bytes())).collect(),
        ..msg("hub-6", "[File] report.pdf, 4 bytes")
    };
    place_async(&e.ctx, m).await.unwrap();
    let sid = telegram::submission_id("hub-6");
    let (saved, context): (Vec<(String, String)>, String) =
        e.db.snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT name, target FROM chat_attachment_refs
                 WHERE submission_id = ?1 AND kind = 'file' ORDER BY rowid",
            )?;
            let rows = st
                .query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let context = tx.query_row(
                "SELECT text FROM submission_context WHERE submission_id = ?1",
                [&sid],
                |r| r.get(0),
            )?;
            Ok::<_, rusqlite::Error>((rows, context))
        })
        .unwrap();
    assert_eq!(
        saved.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        names.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>()
    );
    for ((sent, _), (name, target)) in names.iter().zip(&saved) {
        let path = std::path::Path::new(target);
        assert!(path.is_absolute(), "{target}");
        assert_eq!(path.file_name().unwrap().to_str(), Some(name.as_str()));
        assert_eq!(std::fs::read(path).unwrap(), sent.as_bytes(), "{target}");
        assert!(context.contains(target.as_str()), "{context}");
    }
    drop(e.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_chat_gets_a_steer_whatever_the_send_mode() {
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
    // An old stored "queue" mode changes nothing: it is still a steer, never a stop.
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
    assert_eq!(c.2.as_str(), "steer");
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
    // With the origin flipped the row is read at all: refused now only for the brief note it
    // carries (its length, or its extra line), and with the bare words it counts.
    let r = spend(&e.db).unwrap_err();
    assert!(
        r.contains("longer than") || r.contains("more than one line"),
        "{r}"
    );
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
