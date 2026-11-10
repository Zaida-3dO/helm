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
    /// When set, a `chat_create` that names a model or effort is refused with a bad-args
    /// error, the way the chats side answers for a local server that is no longer set up —
    /// so the retry without the names can be proven to succeed.
    refuse_named: std::sync::atomic::AtomicBool,
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
            "chat_create"
                if self.refuse_named.load(std::sync::atomic::Ordering::SeqCst)
                    && (args.get("model").is_some() || args.get("effort").is_some()) =>
            {
                Err(ApiError::bad_args(
                    "that model's local server is not set up on this machine",
                ))
            }
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
        refuse_named: std::sync::atomic::AtomicBool::new(false),
    });
    let ctx = Ctx {
        rt,
        db: db.clone(),
        ops: ops.clone(),
        handle: tokio::runtime::Handle::current(),
        tag_path: telegram::tag_path(&dir.path().join("prof")),
        folder: plain_path(&dir.path().join("prof").join("telegram")),
        policy: TagPolicy::default(),
        new_chat_model: None,
        new_chat_effort: None,
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

/// The model and effort saved for a new Telegram chat are passed to `chat_create`; when they are
/// not saved the call omits them, and the chat opens on the new-chat default as before.
#[tokio::test(flavor = "multi_thread")]
async fn a_saved_model_and_effort_reach_the_new_telegram_chat() {
    for (model, effort) in [
        (Some("model-a"), Some("medium")),
        (Some("model-b"), None),
        (None, Some("high")),
        (None, None),
    ] {
        let e = env();
        let ctx = Ctx {
            new_chat_model: model.map(str::to_owned),
            new_chat_effort: effort.map(str::to_owned),
            ..e.ctx.clone()
        };
        let p = place_async(&ctx, msg("hub-m", "which model?"))
            .await
            .unwrap();
        assert!(matches!(p, Placed::Sent { .. }));
        let calls = e.ops.calls.lock().unwrap().clone();
        let (_, args) = calls.iter().find(|(o, _)| o == "chat_create").unwrap();
        assert_eq!(args.get("model").and_then(Value::as_str), model);
        assert_eq!(args.get("effort").and_then(Value::as_str), effort);
    }
}

/// A saved model the chats side refuses — its local server was removed from this machine, say —
/// must not leave the channel dark: `create` retries once without the model and effort, so the
/// chat opens on the app default and the message gets an answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_model_opens_the_chat_on_the_app_default() {
    let e = env();
    e.ops
        .refuse_named
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let ctx = Ctx {
        new_chat_model: Some("local/strata/gone-model".to_owned()),
        new_chat_effort: Some("high".to_owned()),
        ..e.ctx.clone()
    };
    let p = place_async(&ctx, msg("hub-m", "still answering?"))
        .await
        .unwrap();
    assert!(matches!(p, Placed::Sent { .. }));
    let calls = e.ops.calls.lock().unwrap().clone();
    let creates: Vec<&Value> = calls
        .iter()
        .filter(|(o, _)| o == "chat_create")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(creates.len(), 2, "{calls:?}");
    assert_eq!(
        creates[0]["model"].as_str(),
        Some("local/strata/gone-model")
    );
    let bare = &creates[1];
    assert!(bare.get("model").is_none(), "{bare}");
    assert!(bare.get("effort").is_none(), "{bare}");
}

/// The round-trip the Settings → Hub pane relies on: what `relay_telegram_save` stored comes back
/// through the view, so a saved model and effort show in the pickers — and unset, blank, a value
/// outside the model list untouched, or a file written by an older Helm shows nothing, leaving the
/// pickers on App default.
#[tokio::test(flavor = "multi_thread")]
async fn the_telegram_settings_view_shows_the_saved_model_and_effort() {
    let e = env();
    let prof = e.dir.path().join("prof");
    let path = telegram::policy_path(&prof);
    let base = telegram::PolicyFile {
        max_age_hours: 6,
        max_tokens: 50_000,
        away_alerts: true,
        approvals: false,
        model: None,
        effort: None,
    };
    for (model, effort, shown) in [
        (
            Some("model-a"),
            Some("medium"),
            (Some("model-a"), Some("medium")),
        ),
        (Some("  "), Some("max"), (None, Some("max"))),
        (None, None, (None, None)),
    ] {
        telegram::save_policy(
            &path,
            telegram::PolicyFile {
                model: model.map(str::to_owned),
                effort: effort.map(str::to_owned),
                ..base.clone()
            },
        )
        .unwrap();
        let v = telegram_view(&e.db, &prof, T0);
        assert_eq!((v.model.as_deref(), v.effort.as_deref()), shown);
    }
    // A policy file written by an older Helm, with neither field, reads as unset.
    std::fs::write(
        &path,
        r#"{"maxAgeHours": 6, "maxTokens": 50000, "awayAlerts": true, "approvals": false}"#,
    )
    .unwrap();
    let v = telegram_view(&e.db, &prof, T0);
    assert_eq!((v.model.as_deref(), v.effort.as_deref()), (None, None));
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
    // The model and effort saved in Settings reach the new chat: the sink reads the saved
    // settings file per delivery and hands them to `chat_create`.
    telegram::save_policy(
        &telegram::policy_path(&e.dir.path().join("prof")),
        telegram::PolicyFile {
            max_age_hours: 6,
            max_tokens: 50_000,
            away_alerts: true,
            approvals: false,
            model: Some("model-a".into()),
            effort: Some("low".into()),
        },
    )
    .unwrap();
    sink.deliver(msg("m1", "hello")).await.unwrap();
    let create = e
        .ops
        .calls
        .lock()
        .unwrap()
        .iter()
        .find(|(o, _)| o == "chat_create")
        .map(|(_, a)| a.clone())
        .unwrap();
    assert_eq!(create.get("model").and_then(Value::as_str), Some("model-a"));
    assert_eq!(create.get("effort").and_then(Value::as_str), Some("low"));
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

/// Only a voice note the hub could understand answers an agent's question, and what it answers
/// with is the transcript; a picture, a document or a recording nobody heard does not.
#[test]
fn a_voice_note_answers_with_its_transcript_and_nothing_else_does() {
    let file = |media: &str| crate::client::TelegramFile {
        name: "note.ogg".into(),
        media_type: media.into(),
        data: String::new(),
    };
    for (text, files, images, want) in [
        (
            "[Voice note] yes, go ahead",
            vec!["audio/ogg"],
            vec![],
            Some("yes, go ahead"),
        ),
        (
            "[Voice note] (no speech was heard)",
            vec!["audio/ogg"],
            vec![],
            None,
        ),
        (
            "[Voice note] (not transcribed: transcription was unavailable)",
            vec!["audio/ogg"],
            vec![],
            None,
        ),
        ("[Voice note] yes", vec!["application/pdf"], vec![], None),
        (
            "[Voice note] yes",
            vec!["audio/ogg"],
            vec!["image/png"],
            None,
        ),
        ("[Voice note] yes", vec![], vec![], None),
        ("yes", vec!["audio/ogg"], vec![], None),
    ] {
        let mut m = msg("v", text);
        m.files = files.into_iter().map(file).collect();
        m.images = images.into_iter().map(file).collect();
        assert_eq!(spoken_answer(&m).as_deref(), want, "{text:?}");
    }
}

/// A saved local model whose server is set up but switched off is left out when a new Telegram chat
/// opens, so the chat answers on the app default instead of refusing every turn.
#[tokio::test(flavor = "multi_thread")]
async fn a_saved_model_on_a_switched_off_server_is_left_out() {
    struct Off;
    impl helm_chats::local::LocalModels for Off {
        fn knows(&self, _: &str) -> bool {
            true
        }
        fn available(&self, _: &str) -> bool {
            false
        }
        fn server(&self, _: &str, _: &str) -> Result<helm_chats::local::LocalServer, String> {
            Err("off".into())
        }
        fn prepare(
            &self,
            _: &helm_chats::local::LocalTurn,
            _: &dyn Fn(&str),
        ) -> Result<String, String> {
            Err("off".into())
        }
        fn finished(&self, _: &str, _: &str) {}
        fn catalogue(&self, _: bool) -> Vec<helm_chats::types::LocalModelInfo> {
            vec![]
        }
    }
    let e = env();
    e.ctx.rt.set_local_models(Arc::new(Off));
    let ctx = Ctx {
        new_chat_model: Some("local/strata/some-model".to_owned()),
        new_chat_effort: Some("high".to_owned()),
        ..e.ctx.clone()
    };
    let p = place_async(&ctx, msg("hub-off", "anyone home?"))
        .await
        .unwrap();
    assert!(matches!(p, Placed::Sent { .. }));
    let calls = e.ops.calls.lock().unwrap().clone();
    let creates: Vec<&Value> = calls
        .iter()
        .filter(|(o, _)| o == "chat_create")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(creates.len(), 1, "{calls:?}");
    assert!(creates[0].get("model").is_none(), "{}", creates[0]);
    assert_eq!(creates[0]["effort"].as_str(), Some("high"));
}

// ------------------------------------------------------------------ the voice chat

use crate::client::{VoiceSink, VoiceTurn};
use crate::voice;
use crate::voice_sink as vs;

fn owed_of(out: &[vs::Up]) -> vs::Owed {
    match out.last() {
        Some(vs::Up::Final(o)) => o.clone(),
        other => panic!("expected an answer, got {other:?}"),
    }
}

#[test]
fn the_voice_mirror_gives_a_working_line_then_one_answer_and_keeps_an_unsent_one() {
    let mut m = vs::Mirror::new(0, T0);
    // A working line, held to the gap and sent only when it changes.
    assert_eq!(
        m.step(&busy(&[], &[]), T0),
        vec![vs::Up::Status("Thinking".into())]
    );
    assert!(m.step(&busy(&[], &[]), T0 + 5_000).is_empty());
    let b = busy(&["Checking the nightly backup"], &[]);
    assert!(m.step(&b, T0 + STATUS_GAP_MS - 1).is_empty());
    assert_eq!(
        m.step(&b, T0 + STATUS_GAP_MS),
        vec![vs::Up::Status("Checking the nightly backup".into())]
    );
    // The answer goes up the moment its turn closes, while the chat is still busy. One that the
    // line would not take is offered again with the same reply id, and nothing else meanwhile.
    let mut done = busy(&[], &[]);
    done.turn_done = Some((7, "It passed.".into()));
    let owed = owed_of(&m.step(&done, T0 + 10_000));
    assert_eq!(owed.what, vs::What::Answer("It passed.".into()));
    m.ack(&owed.id, false);
    assert_eq!(
        m.step(&busy(&[], &[]), T0 + 11_000),
        vec![vs::Up::Final(owed.clone())]
    );
    m.ack(&owed.id, true);
    // Once it is up the chat only winds down: no working line, and the close says nothing more.
    assert!(m.step(&done, T0 + 20_000).is_empty());
    let settled = Snap {
        turn_done: done.turn_done.clone(),
        final_text: Some("It passed.".into()),
        max_id: 9,
        ..Default::default()
    };
    assert!(m.step(&settled, T0 + 21_000).is_empty());
    assert!(m.step(&settled, T0 + 21_500).is_empty());
    assert!(m.done);

    // A prompt only Helm's window can answer: said once, as a working line and a sentence.
    let mut m = vs::Mirror::new(0, T0);
    let mut blocked = busy(&[], &[]);
    blocked.blocked = Some(("req1".into(), "Waiting for you in Helm".into()));
    let out = m.step(&blocked, T0);
    assert_eq!(out[0], vs::Up::Status(vs::WAITING_STATUS.into()));
    let owed = owed_of(&out);
    assert_eq!((out.len(), &owed.what), (2, &vs::What::Waiting));
    m.ack(&owed.id, true);
    assert!(m.step(&blocked, T0 + 1_000).is_empty(), "said once");
    // The turn then ends with nothing to say: it still gets a closing word.
    assert!(m.step(&idle(None), T0 + 2_000).is_empty());
    let owed = owed_of(&m.step(&idle(None), T0 + 2_500));
    assert_eq!((owed.text(), &owed.what), ("Done.", &vs::What::Nothing));
    m.ack(&owed.id, true);
    assert!(m.done);

    // A message that never starts a turn is not left silent.
    let mut m = vs::Mirror::new(0, T0);
    assert!(m.step(&idle(None), T0 + vs::START_WAIT_MS - 1).is_empty());
    let owed = owed_of(&m.step(&idle(None), T0 + vs::START_WAIT_MS));
    assert_eq!(owed.what, vs::What::NotStarted);
    m.ack(&owed.id, true);
    assert!(m.done);

    // The reply is made for the mode the phone is in, under the answer's own id.
    let live = vs::Live {
        turn: Some("t1".into()),
        conversation: "c1".into(),
        ..Default::default()
    };
    let r = vs::reply_for(&live, &owed, 5);
    assert_eq!(
        (r.id.as_str(), r.turn.as_deref()),
        (owed.id.as_str(), Some("t1"))
    );
    assert_eq!(
        r.sentences,
        [
            "Helm has not started that yet.",
            "It is saved in the voice chat."
        ]
    );
    let chat = vs::Live {
        mode: voice::Mode::Chat,
        ..live
    };
    let r = vs::reply_for(&chat, &owed, 5);
    assert!(r.sentences.is_empty() && r.text == vs::NOT_STARTED);
}

fn vturn(id: &str, text: &str, mode: &str) -> VoiceTurn {
    VoiceTurn {
        id: id.into(),
        mode: mode.into(),
        text: text.into(),
        source: "speech".into(),
        sent_at: 0,
        age_minutes: None,
        continues: None,
    }
}

fn vctx(e: &Env, file: voice::PolicyFile) -> vs::Ctx {
    let prof = e.dir.path().join("prof");
    vs::Ctx {
        rt: e.ctx.rt.clone(),
        db: e.db.clone(),
        ops: e.ops.clone(),
        handle: tokio::runtime::Handle::current(),
        tag_path: voice::tag_path(&prof),
        folder: plain_path(&voice::folder(&prof)),
        policy: file.check().unwrap(),
        file,
    }
}

async fn vplace(c: &vs::Ctx, t: VoiceTurn, now: i64) -> Result<vs::Placed, String> {
    let c = c.clone();
    tokio::task::spawn_blocking(move || vs::place(&c, &t, &[], now))
        .await
        .unwrap()
}

fn vchat(p: &vs::Placed) -> (String, String) {
    match p {
        vs::Placed::Sent {
            chat, conversation, ..
        } => (chat.clone(), conversation.clone()),
        vs::Placed::Already => panic!("expected a placed turn"),
    }
}

fn set_idle(e: &Env, chat: &str) {
    sql(
        &e.db,
        "UPDATE chats SET status = 'idle' WHERE id = ?1",
        &[&chat],
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn voice_turns_enter_one_chat_with_origin_voice_a_primer_once_and_a_new_chat_after_idle() {
    let e = env();
    let c = vctx(&e, voice::PolicyFile::default());
    let t0 = helm_core::now_ms();

    // The first turn starts "Voice <date>" on the saved model and effort, with the primer.
    let p = vplace(
        &c,
        vturn("t1", "Synthetic question one.", "conversation"),
        t0,
    )
    .await
    .unwrap();
    let (chat, conversation) = vchat(&p);
    let calls = e.ops.calls.lock().unwrap().clone();
    assert_eq!(calls[0].0, "chat_create");
    assert_eq!(calls[0].1["model"], "claude-sonnet-5-5");
    assert_eq!(calls[0].1["effort"], "low");
    assert!(
        calls[0].1["cwd"].as_str().unwrap().ends_with("voice"),
        "{calls:?}"
    );
    let title: String =
        e.db.snapshot(|tx| {
            tx.query_row("SELECT title FROM chats WHERE id = ?1", [&chat], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert!(title.starts_with("Voice "), "{title}");
    let s = subs(&e.db, &chat);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].0, voice::submission_id("t1"));
    assert_eq!(s[0].1, "voice", "never the user's typed words");
    assert!(s[0].4.starts_with(voice::PRIMER_START), "{}", s[0].4);
    assert!(
        s[0].4.ends_with(
            "\n[Voice · conversation: speak it, short, no markup]\nSynthetic question one."
        ),
        "{}",
        s[0].4
    );
    assert_eq!(conversation.len(), 16);

    // The hub resends it while its turn runs (Helm restarted before the acknowledgement): stored
    // once, and the turn is watched. Once the turn is over, there is nothing more to do.
    sql(
        &e.db,
        "UPDATE chats SET status = 'running' WHERE id = ?1",
        &[&chat],
    );
    let running = vplace(
        &c,
        vturn("t1", "Synthetic question one.", "conversation"),
        t0 + 1,
    )
    .await
    .unwrap();
    assert_eq!(vchat(&running), (chat.clone(), conversation.clone()));
    set_idle(&e, &chat);
    assert_eq!(
        vplace(
            &c,
            vturn("t1", "Synthetic question one.", "conversation"),
            t0 + 1
        )
        .await
        .unwrap(),
        vs::Placed::Already
    );
    assert_eq!(subs(&e.db, &chat).len(), 1);

    // The next turn goes to the same chat and conversation: no primer, its mode and its lateness.
    let mut t2 = vturn("t2", "Synthetic question two.", "chat");
    t2.age_minutes = Some(5);
    t2.continues = Some("t1".into());
    let p = vplace(&c, t2, t0 + 60_000).await.unwrap();
    assert_eq!(vchat(&p), (chat.clone(), conversation.clone()));
    let s = subs(&e.db, &chat);
    assert_eq!(s.len(), 2);
    let second = s
        .iter()
        .find(|r| r.0 == voice::submission_id("t2"))
        .unwrap();
    assert_eq!(
        second.4,
        "[Voice · chat: read on screen, formatting fine · continuing what they just said]\n(sent 5 min ago, queued while Helm was offline)\nSynthetic question two."
    );

    // Idle for longer than the limit: a new chat and conversation, and the primer again. A
    // limit the user has set applies from the next turn.
    set_idle(&e, &chat);
    let c = vctx(
        &e,
        voice::PolicyFile {
            model: String::new(),
            effort: "high".into(),
            idle_minutes: 30,
            ..Default::default()
        },
    );
    let p = vplace(
        &c,
        vturn("t3", "Synthetic question three.", "conversation"),
        t0 + 60_000 + 31 * 60_000,
    )
    .await
    .unwrap();
    let (chat3, conversation3) = vchat(&p);
    assert_ne!(chat3, chat);
    assert_ne!(conversation3, conversation);
    assert!(subs(&e.db, &chat3)[0].4.starts_with(voice::PRIMER_START));
    let calls = e.ops.calls.lock().unwrap().clone();
    let last = &calls.iter().rfind(|(o, _)| o == "chat_create").unwrap().1;
    assert!(
        last.get("model").is_none(),
        "blank model: the app default, {last}"
    );
    assert_eq!(last["effort"], "high");
}

/// A line that takes nothing, for sinks whose mirror is not what is being looked at.
struct Rec;
impl vs::VoiceUpstream for Rec {
    fn status(&self, _turn: &str, _text: &str) {}
    fn reply(&self, _reply: &voice::Reply) -> bool {
        false
    }
}

fn wired_voice(e: &Env) -> vs::ChatVoice {
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
    vs::ChatVoice::new(exec, e.dir.path().join("prof"))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_voice_sink_delivers_only_once_wired_and_stop_presence_and_fresh_act_on_the_tag() {
    let e = env();
    // Unwired: a delivery is refused, so the hub keeps the turn.
    let bare = vs::ChatVoice::new(Arc::new(AppExecutor::default()), e.dir.path().join("prof"));
    assert!(bare.deliver(vturn("x", "y", "chat")).await.is_err());
    let v = wired_voice(&e);
    assert!(
        v.deliver(vturn("x", "y", "chat")).await.is_err(),
        "no ops yet"
    );

    v.attach_ops(e.ops.clone());
    v.attach_line(Arc::new(Rec));
    v.deliver(vturn("t1", "Synthetic question one.", "chat"))
        .await
        .unwrap();
    let live = v.live();
    assert_eq!(live.turn.as_deref(), Some("t1"));
    assert_eq!(
        (live.mode, live.conversation.len()),
        (voice::Mode::Chat, 16)
    );
    let tag_path = voice::tag_path(&e.dir.path().join("prof"));
    let tag = voice::load_tag(&tag_path).unwrap();
    assert_eq!(tag.conversation, live.conversation);
    assert!(tag.primed, "the primer went with the first message");

    // Stop acts only on the turn being answered, and on the tagged chat.
    let stops = || {
        e.ops
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(o, _)| o == "chat_stop")
            .map(|(_, a)| a.clone())
            .collect::<Vec<_>>()
    };
    v.stop("an-older-turn".into()).await;
    assert!(stops().is_empty());
    v.stop("t1".into()).await;
    assert_eq!(stops(), vec![json!({ "chatId": tag.chat_id })]);

    // The phone's screen is remembered; a fresh start drops the tag.
    v.presence(true, "chat");
    let live = v.live();
    assert_eq!(
        (live.presence_live, live.presence_mode),
        (true, voice::Mode::Chat)
    );
    v.fresh().await;
    assert!(voice::load_tag(&tag_path).is_none());

    // What Settings shows: the saved policy, and no chat once the tag is gone.
    let view = vs::voice_view(&e.db, &e.dir.path().join("prof"), helm_core::now_ms());
    assert!(view.chat.is_none());
    assert_eq!(
        (view.model.as_str(), view.idle_minutes),
        ("claude-sonnet-5-5", 240)
    );
}

// ------------------------------------------------------------------ whose answer, and what to show

use crate::voice_show::{Attachment, Kind, Shown};
use crate::voice_watch;
use helm_chats::types::{AttentionKind, AttentionRow, AttentionSummary, SubmissionOrigin};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

fn starter(origin: SubmissionOrigin, text: &str) -> vs::Starter {
    vs::Starter {
        origin,
        text: text.into(),
    }
}

#[test]
fn an_answer_goes_where_the_message_that_began_the_exchange_says() {
    use vs::Route::{Phone, Silent};
    use SubmissionOrigin::{App, OtherChat, Typed, Voice};
    let titles = |id: &str| (id == "c-build").then(|| "Build".to_owned());
    let news = |about: Option<&str>| vs::Route::News {
        about: about.map(str::to_owned),
    };
    let wait_build = "[Wait finished] Wait w1 on chat c-build: chat idle.\nThis message is from Helm's wait watcher.";
    let wait_file = "[Wait finished] Wait w2 on file C:/w/out.txt: file changed.\nDetail: x";
    let wait_gone = "[Wait finished] Wait w3 on chat c-gone: chat idle.";
    let other = "[Message from another chat: \"Docs: \"v2\" notes\" (c-docs)]\nSent by another Helm chat.\n\nIt passed.";
    let one = "[Attention] Build: Waiting for an approval\n(This came from Helm's watcher.)";
    let two = "[Attention] Build: Waiting\n[Attention] Docs: Looks unfinished\n(This came from Helm's watcher.)";
    // (the messages of the exchange, who it is for while none are in the transcript, the route)
    let rows: Vec<(Vec<vs::Starter>, vs::Route, vs::Route)> = vec![
        (vec![], Phone, Phone),
        (vec![], Silent, Silent),
        (vec![], news(None), news(None)),
        (vec![starter(Voice, "hello")], Silent, Phone),
        (vec![starter(Typed, "hi")], Phone, Silent),
        (vec![starter(App, wait_build)], Silent, news(Some("Build"))),
        (vec![starter(App, wait_file)], Silent, news(None)),
        (vec![starter(App, wait_gone)], Silent, news(None)),
        (
            vec![starter(OtherChat, other)],
            Silent,
            news(Some("Docs: \"v2\" notes")),
        ),
        (vec![starter(App, one)], Silent, news(Some("Build"))),
        (vec![starter(App, two)], Silent, news(None)),
        // The app's other notes to itself, and typed words that only look like news, are not news.
        (
            vec![starter(
                App,
                "[Helm] Your last reply did not reach the phone.",
            )],
            Phone,
            Silent,
        ),
        (vec![starter(Typed, wait_build)], Phone, Silent),
        // The phone wins over everything; otherwise the latest news names the chat.
        (
            vec![starter(App, wait_build), starter(Voice, "hello")],
            Silent,
            Phone,
        ),
        (
            vec![starter(Typed, "hi"), starter(App, wait_build)],
            Silent,
            news(Some("Build")),
        ),
        (
            vec![starter(App, wait_build), starter(OtherChat, other)],
            Silent,
            news(Some("Docs: \"v2\" notes")),
        ),
    ];
    for (i, (began, fallback, want)) in rows.into_iter().enumerate() {
        assert_eq!(vs::route_of(&began, &fallback, &titles), want, "row {i}");
    }

    // What becomes of an answer: news says nothing when the chat says `[quiet]` or has nothing to
    // say, but a prompt only Helm's window can answer is still worth telling.
    let answer = |t: &str| vs::What::Answer(t.into());
    let build = news(Some("Build"));
    let told = Some("Build".to_owned());
    let rows: Vec<(vs::Route, vs::What, vs::Delivery)> = vec![
        (Phone, answer("Hi."), vs::Delivery::Phone),
        (Phone, vs::What::Nothing, vs::Delivery::Phone),
        (Silent, answer("Hi."), vs::Delivery::Skip),
        (Silent, vs::What::Waiting, vs::Delivery::Skip),
        (
            build.clone(),
            answer("It passed."),
            vs::Delivery::News(told.clone()),
        ),
        (build.clone(), answer(" [Quiet]\n"), vs::Delivery::Skip),
        (build.clone(), answer("[ QUIET ]"), vs::Delivery::Skip),
        (
            build,
            answer("[quiet] but not only"),
            vs::Delivery::News(told),
        ),
        (news(None), vs::What::Waiting, vs::Delivery::News(None)),
        (news(None), vs::What::Nothing, vs::Delivery::Skip),
        (news(None), vs::What::NotStarted, vs::Delivery::Skip),
    ];
    for (i, (route, what, want)) in rows.into_iter().enumerate() {
        let owed = vs::Owed::unrowed("r", what);
        assert_eq!(vs::decide(&route, &owed), want, "row {i}");
    }
}

/// A line that records what goes up, and can be down.
#[derive(Default)]
struct Wire {
    sent: Mutex<Vec<Value>>,
    up: AtomicBool,
}

impl vs::VoiceUpstream for Wire {
    fn status(&self, turn: &str, text: &str) {
        self.sent
            .lock()
            .unwrap()
            .push(json!({ "status": text, "turn": turn }));
    }
    fn reply(&self, reply: &voice::Reply) -> bool {
        let up = self.up.load(AtomicOrdering::SeqCst);
        if up {
            self.sent.lock().unwrap().push(reply.frame());
        }
        up
    }
    fn attachment(&self, frame: &Value) -> bool {
        let up = self.up.load(AtomicOrdering::SeqCst);
        if up {
            self.sent.lock().unwrap().push(frame.clone());
        }
        up
    }
}

#[test]
fn things_to_show_go_up_before_the_answer_and_stay_claimed_until_it_is_sent() {
    use vs::Route::{Phone, Silent};
    let wire = Arc::new(Wire::default());
    let shown = Arc::new(Shown::default());
    let item = |t: &str| Attachment {
        kind: Kind::Link,
        title: t.into(),
        media_type: None,
        data: None,
        markdown: None,
        url: Some("https://example.com".into()),
    };
    let mut out = vs::Outbox::new(wire.clone(), shown.clone(), "chat1".into());
    let live = vs::Live {
        turn: Some("t1".into()),
        conversation: "c1".into(),
        ..Default::default()
    };
    let sent = || wire.sent.lock().unwrap().clone();
    let kinds = |v: &[Value]| -> Vec<String> {
        v.iter()
            .map(|f| f["type"].as_str().unwrap_or("status").to_owned())
            .collect()
    };
    shown.add("chat1", item("one")).unwrap();
    shown.add("chat1", item("two")).unwrap();
    shown.add("other", item("elsewhere")).unwrap();

    // The line is down: nothing goes, the answer stays owed and its things stay claimed, so one
    // queued meanwhile belongs to the next answer.
    let owed = vs::Owed::unrowed("r1", vs::What::Answer("Here it is.".into()));
    assert!(!out.send(&Phone, &live, None, &owed, 5));
    assert!(sent().is_empty());
    shown.add("chat1", item("late")).unwrap();
    wire.up.store(true, AtomicOrdering::SeqCst);
    assert!(out.send(&Phone, &live, None, &owed, 6));
    let got = sent();
    assert_eq!(
        kinds(&got),
        ["voice_attachment", "voice_attachment", "voice_final"]
    );
    assert_eq!(
        (
            got[0]["reply"].as_str(),
            got[0]["n"].as_i64(),
            got[0]["title"].as_str()
        ),
        (Some("r1"), Some(1), Some("one"))
    );
    assert_eq!(
        (got[1]["n"].as_i64(), got[1]["title"].as_str()),
        (Some(2), Some("two"))
    );
    assert_eq!(
        (
            got[2]["id"].as_str(),
            got[2]["turn"].as_str(),
            got[2]["attachments"].as_i64()
        ),
        (Some("r1"), Some("t1"), Some(2))
    );

    // The next answer takes what was queued since; another chat's queue is not touched.
    let next = vs::Owed::unrowed("r2", vs::What::Answer("Next.".into()));
    assert!(out.send(&Phone, &live, None, &next, 7));
    let got = sent();
    assert_eq!(kinds(&got[3..]), ["voice_attachment", "voice_final"]);
    assert_eq!(
        (got[3]["title"].as_str(), got[3]["reply"].as_str()),
        (Some("late"), Some("r2"))
    );
    assert_eq!(got[4]["attachments"].as_i64(), Some(1));
    assert_eq!(shown.count("other"), 1);

    // An answer that is not going up empties the queue and sends nothing.
    shown.add("chat1", item("never")).unwrap();
    assert!(out.send(&Silent, &live, None, &next, 8));
    assert_eq!((shown.count("chat1"), sent().len()), (0, 5));

    // News: no turn, the mode the phone's screen is in, the chat's own conversation, and the chat
    // it is about. `[quiet]` says nothing.
    let screen = vs::Live {
        presence_mode: voice::Mode::Chat,
        conversation: "old".into(),
        ..live.clone()
    };
    let route = vs::Route::News {
        about: Some("Build".into()),
    };
    let news = vs::Owed::unrowed("r3", vs::What::Answer("It passed.".into()));
    assert!(out.send(&route, &screen, Some("c2"), &news, 9));
    let f = sent().pop().unwrap();
    assert_eq!(f["type"], "voice_notify");
    assert_eq!(
        (
            f["turn"].clone(),
            f["mode"].as_str(),
            f["conversation"].as_str(),
            f["text"].as_str()
        ),
        (Value::Null, Some("chat"), Some("c2"), Some("It passed."))
    );
    assert_eq!(f["about"], json!({ "chat": "Build" }));
    let quiet = vs::Owed::unrowed("r4", vs::What::Answer("[quiet]".into()));
    assert!(out.send(&route, &screen, Some("c2"), &quiet, 10));
    assert_eq!(sent().len(), 6);

    // The working line is the phone's own turn's alone.
    out.status(&Phone, &live, "Thinking");
    out.status(&route, &live, "Thinking");
    out.status(&Phone, &vs::Live::default(), "Thinking");
    assert_eq!(sent().len(), 7);
    assert_eq!(sent()[6], json!({ "status": "Thinking", "turn": "t1" }));
}

fn a_note(chat: &str) -> voice_watch::Note {
    let empty = AttentionSummary {
        badge: 0,
        busy: 0,
        needs_you: vec![],
        running: vec![],
        unread: vec![],
        stalled: vec![],
        flagged: vec![],
        revision: 1,
    };
    let mut full = empty.clone();
    full.revision = 2;
    full.needs_you.push(AttentionRow {
        chat_id: chat.into(),
        title: "Build".into(),
        project: "p".into(),
        kind: AttentionKind::Approval,
        reason: "Waiting for an approval".into(),
    });
    let mut w = voice_watch::Watch::default();
    w.summary(&empty, Some("v"), 0);
    w.summary(&full, Some("v"), 1);
    w.due(1 + voice_watch::COALESCE_MS).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_enter_the_voice_chat_as_the_apps_words_and_the_rows_say_what_began_a_turn() {
    let e = env();
    let prof = e.dir.path().join("prof");
    let v = wired_voice(&e);
    v.attach_ops(e.ops.clone());
    v.attach_line(Arc::new(Rec));
    let note = a_note("c-build");
    let now = helm_core::now_ms();

    // No voice chat yet: nobody to tell.
    assert_eq!(v.deliver_note(&note, now).await, vs::Noted::Skipped);
    assert_eq!(v.active_chat(now).await, None);
    assert_eq!(v.watch_level(), voice::WatchLevel::Flags);

    // The phone's first turn starts the chat and the tag.
    v.deliver(vturn("t1", "Synthetic question one.", "conversation"))
        .await
        .unwrap();
    let tag = voice::load_tag(&voice::tag_path(&prof)).unwrap();
    let chat = tag.chat_id.clone();
    assert!(v.is_voice_chat(&chat) && !v.is_voice_chat("c-other"));
    set_idle(&e, &chat);
    assert_eq!(v.active_chat(now).await.as_deref(), Some(chat.as_str()));
    // Idle for longer than the limit, nobody is told.
    let later = now + 5 * 3_600_000;
    assert_eq!(v.active_chat(later).await, None);
    assert_eq!(v.deliver_note(&note, later).await, vs::Noted::Skipped);

    // The note goes in as the app's message, once however often it is delivered, and the phone's
    // idle time does not move.
    assert_eq!(v.deliver_note(&note, now).await, vs::Noted::Sent);
    let _ = v.deliver_note(&note, now).await;
    let s = subs(&e.db, &chat);
    assert_eq!(s.len(), 2);
    let n = s
        .iter()
        .find(|r| r.0 == voice::submission_id(&note.key))
        .unwrap();
    assert_eq!(n.1, "app");
    assert!(
        n.4.starts_with("[Attention] Build: Waiting for an approval"),
        "{}",
        n.4
    );
    assert_eq!(
        voice::load_tag(&voice::tag_path(&prof))
            .unwrap()
            .last_turn_at,
        tag.last_turn_at
    );

    // The transcript's user rows say how each message entered, in order, after any row.
    for (sid, text) in [
        (voice::submission_id("t1"), "asked"),
        (voice::submission_id(&note.key), "noted"),
    ] {
        sql(
            &e.db,
            "INSERT INTO messages(chat_id, source_key, role, text, timestamp)
             VALUES (?1, ?2, 'user', ?3, 1)",
            &[&chat, &format!("u:{sid}"), &text],
        );
        if text == "asked" {
            assert_eq!(
                vs::starters(&e.db, &chat, 0),
                vec![starter(SubmissionOrigin::Voice, "asked")]
            );
        }
    }
    let all = vs::starters(&e.db, &chat, 0);
    assert_eq!(
        all.iter()
            .map(|s| (s.origin, s.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (SubmissionOrigin::Voice, "asked"),
            (SubmissionOrigin::App, "noted")
        ]
    );
    assert_eq!(vs::starters(&e.db, &chat, max_row(&e.db, &chat)), vec![]);

    // A turn that starts after the last answer began an exchange nobody on the phone waits on.
    assert_eq!(vs::last_answer_row(&e.db, &chat), 0);
    for (key, reason) in [
        ("a:1", "tool_use"),
        ("a:2", "end_turn"),
        ("a:3", "tool_use"),
    ] {
        sql(
            &e.db,
            "INSERT INTO messages(chat_id, source_key, role, text, timestamp, details)
             VALUES (?1, ?2, 'assistant', 'x', 1, ?3)",
            &[&chat, &key, &format!("{{\"stopReason\":\"{reason}\"}}")],
        );
    }
    let ended: i64 =
        e.db.snapshot(|tx| {
            tx.query_row(
                "SELECT id FROM messages WHERE chat_id = ?1 AND source_key = 'a:2'",
                [&chat],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(vs::last_answer_row(&e.db, &chat), ended);
}
