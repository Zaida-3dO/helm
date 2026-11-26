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

/// A turn that stopped on an error never sends the error up. While Helm retries it by itself the
/// phone gets one line saying so and the exchange waits for the continued turn's answer; when
/// the wait runs out, or Helm gives up, the last word is the error's headline; any other error
/// goes up as its headline; and a stop from before the exchange began is not its own.
#[test]
fn a_stopped_turn_says_so_in_words_and_waits_for_helms_own_retry() {
    let raw = "Failed to refresh OAuth token: another Claude Code process is refreshing it";
    let retrying = Stop::Retrying {
        headline: raw.into(),
    };
    // (status, error, local, typing, gave up) -> the stop.
    type Row<'a> = (&'a str, Option<&'a str>, bool, bool, bool, Option<Stop>);
    let rows: Vec<Row> = vec![
        (
            "interrupted",
            Some(raw),
            false,
            false,
            false,
            Some(retrying.clone()),
        ),
        (
            "interrupted",
            Some(raw),
            false,
            false,
            true,
            Some(Stop::Error(raw.into())),
        ),
        // Helm stopped retrying a connection or service failure: the error is the last word.
        (
            "interrupted",
            Some("fetch failed"),
            false,
            false,
            true,
            Some(Stop::Error("fetch failed".into())),
        ),
        (
            "interrupted",
            Some("fetch failed"),
            false,
            false,
            false,
            Some(Stop::Retrying {
                headline: "fetch failed".into(),
            }),
        ),
        // Helm does not carry on a chat the user is writing in, or a local model's outage.
        (
            "interrupted",
            Some("fetch failed"),
            false,
            true,
            false,
            Some(Stop::Error("fetch failed".into())),
        ),
        (
            "interrupted",
            Some("fetch failed"),
            true,
            false,
            false,
            Some(Stop::Error("fetch failed".into())),
        ),
        (
            "interrupted",
            Some("Plan limit reached, resets at 01:13\n\nFailed to refresh OAuth token"),
            false,
            false,
            false,
            Some(Stop::Error("Plan limit reached, resets at 01:13".into())),
        ),
        ("idle", Some(raw), false, false, false, None),
        ("interrupted", None, false, false, false, None),
    ];
    for (status, error, local, typing, gave_up, want) in rows {
        assert_eq!(
            Stop::of(status, error, local, typing, gave_up),
            want,
            "{error:?}"
        );
    }
    let snap = |stop: Stop, id: i64| Snap {
        stopped: Some(stop),
        max_id: id,
        ..Default::default()
    };
    // Retried by Helm: one line, the exchange stays open, the continued turn's answer goes up.
    let mut m = Mirror::new(0, T0);
    step_ok(&mut m, &busy(&[], &[]), T0);
    assert_eq!(
        step_ok(&mut m, &snap(retrying.clone(), 9), T0 + 3_000),
        [Up::Status(telegram::OUTAGE_RETRYING.into())]
    );
    for i in 1..6 {
        assert!(step_ok(&mut m, &snap(retrying.clone(), 9), T0 + 3_000 + i * 500).is_empty());
    }
    let answered = Snap {
        busy: true,
        turn_done: Some((12, "All done.".into())),
        max_id: 12,
        ..Default::default()
    };
    assert_eq!(
        step_ok(&mut m, &answered, T0 + 60_000),
        [Up::Final("All done.".into())]
    );
    // Helm gave up, or the wait ran out: the last word is the error's headline.
    for (late, last) in [
        (snap(Stop::Error(raw.into()), 9), 0),
        (snap(retrying.clone(), 9), telegram::RETRY_HOLD_MS),
    ] {
        let mut m = Mirror::new(0, T0);
        step_ok(&mut m, &busy(&[], &[]), T0);
        step_ok(&mut m, &snap(retrying.clone(), 9), T0 + 1);
        let at = T0 + 1 + last;
        let mut out = step_ok(&mut m, &late, at);
        out.extend(step_ok(&mut m, &late, at + 500));
        assert_eq!(out, [Up::Final(telegram::render_stopped(raw))]);
        assert!(m.done);
    }
    // Any other error: its headline, never the raw detail or a reply it cut short.
    let mut m = Mirror::new(0, T0);
    step_ok(&mut m, &busy(&[], &[]), T0);
    let stopped = Snap {
        final_text: Some("Let me look".into()),
        ..snap(Stop::Error("Plan limit reached, resets at 01:13".into()), 9)
    };
    step_ok(&mut m, &stopped, T0 + 500);
    assert_eq!(
        step_ok(&mut m, &stopped, T0 + 1_000),
        [Up::Final(
            "The turn stopped before it finished: Plan limit reached, resets at 01:13".into()
        )]
    );
    // A stop the chat had before this exchange's message reached it is not this exchange's.
    let mut m = Mirror::new(9, T0);
    assert!(step_ok(&mut m, &snap(Stop::Error("old".into()), 9), T0 + 500).is_empty());
    assert!(step_ok(&mut m, &snap(Stop::Error("old".into()), 9), T0 + 1_000).is_empty());
    assert!(!m.done);
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
    /// When set, the working-mode ops answer with an error instead of a view.
    fail_workmode: std::sync::atomic::AtomicBool,
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
            "workmode_get" | "workmode_set"
                if self.fail_workmode.load(std::sync::atomic::Ordering::SeqCst) =>
            {
                Err(ApiError::internal("the mode is out of reach right now"))
            }
            "workmode_get" => Ok(mode_view_json("check_in")),
            // A set answers with the view as if its word had taken hold, the way the real op
            // answers with the view after the change.
            "workmode_set" => Ok(mode_view_json(args["mode"].as_str().unwrap_or("check_in"))),
            _ => Ok(Value::Null),
        };
        Box::pin(async move { r })
    }
}

/// The view the working-mode ops answer with: the given app mode, one board with its own mode,
/// and the counts (the real shape, as the gatekeeper's view serialises).
fn mode_view_json(app: &str) -> Value {
    json!({
        "app": app,
        "effective": app,
        "source": "app",
        "overrides": [{ "id": "b-1", "name": "Alpha board", "mode": "run" }],
        "counts": { "decided": 4, "needsYou": 1 }
    })
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
    helm_profiles::register_migrations(&mut reg);
    helm_devices::register_migrations(&mut reg);
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
        fail_workmode: std::sync::atomic::AtomicBool::new(false),
    });
    // The home profile made at open owns this test's channel files, in the per-person layout.
    let pid = db
        .snapshot(|tx| helm_profiles::store::home(tx))
        .unwrap()
        .unwrap()
        .id;
    let paths = crate::people::telegram_paths(&dir.path().join("prof"), &pid);
    let ctx = Ctx {
        rt,
        db: db.clone(),
        ops: ops.clone(),
        handle: tokio::runtime::Handle::current(),
        profile_id: pid,
        person: String::new(),
        tag_path: paths.tag,
        folder: plain_path(&paths.folder),
        policy: TagPolicy::default(),
        policy_path: paths.policy,
        new_chat_model: None,
        new_chat_effort: None,
        answered: Arc::new(Mutex::new(VecDeque::new())),
        answered_path: paths.answered,
        sent_path: paths.sent,
    };
    Env { dir, db, ops, ctx }
}

fn msg(id: &str, text: &str) -> TelegramMessage {
    TelegramMessage {
        images: vec![],
        files: vec![],
        id: id.into(),
        text: text.into(),
        person: None,
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

/// Wait until the chat's actor is done with `sid`: its submission has reached an end state, so
/// the actor's own status changes are behind us and the status we set afterwards is the last
/// word. The real sink runs one delivery at a time under its serial lock; a test that calls
/// `place` directly must not let a resend race the actor still reacting to the first send.
async fn wait_quiet(db: &Database, sid: &str) {
    for _ in 0..100 {
        let ended = db
            .snapshot(|tx| {
                Ok::<_, rusqlite::Error>(
                    tx.query_row(
                        "SELECT state IN ('delivered', 'complete', 'uncertain', 'rejected',
                                  'withdrawn', 'superseded')
                         FROM submissions WHERE id = ?1",
                        [sid],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(1),
                )
            })
            .unwrap_or(1)
            != 0;
        if ended {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    settle().await;
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
    // An error row is never the answer, even one that carries words (as older rows do).
    let row = |tx: &rusqlite::Transaction<'_>, key: &str, role: &str, text: &str, status: &str| {
        tx.execute(
            "INSERT INTO messages(chat_id, source_key, role, text, status, timestamp, details)
             VALUES (?1, ?2, ?3, ?4, ?5, 2000, '{\"stopReason\":\"end_turn\"}')",
            rusqlite::params![&chat, key, role, text, status],
        )
        .map(|_| ())
    };
    let refresh = "Failed to refresh OAuth token: another Claude Code process is refreshing it";
    e.db.write(|tx| {
        row(tx, "m:a", "assistant", "Here is the plan.", "complete")?;
        row(tx, "m:b", "assistant", refresh, "error")?;
        tx.execute(
            "UPDATE chats SET status = 'interrupted', error = ?2 WHERE id = ?1",
            rusqlite::params![&chat, refresh],
        )
        .map(|_| ())
    })
    .unwrap();
    let snap = snapshot(&e.ctx.rt, &e.db, &chat, 0, false);
    assert_eq!(snap.final_text.as_deref(), Some("Here is the plan."));
    assert_eq!(
        snap.turn_done.map(|t| t.1).as_deref(),
        Some("Here is the plan.")
    );
    assert!(matches!(snap.stopped, Some(Stop::Retrying { .. })));
    // Helm's give-up line makes it a plain error; a continuation sent after it (the person asked
    // for another try) makes it a retry again.
    let text = helm_chats::outage::GAVE_UP_TEXT;
    e.db.write(|tx| row(tx, "c:gave-up", "context", text, "complete"))
        .unwrap();
    let stopped = |e: &Env| snapshot(&e.ctx.rt, &e.db, &chat, 0, false).stopped;
    assert!(matches!(stopped(&e), Some(Stop::Error(_))));
    e.db.write(|tx| {
        row(
            tx,
            "c:outage:again",
            "context",
            "Helm asked Claude to carry on",
            "complete",
        )
    })
    .unwrap();
    assert!(matches!(stopped(&e), Some(Stop::Retrying { .. })));
}

/// The hub's outage report opens a repair chat, once per service per hour.
#[tokio::test(flavor = "multi_thread")]
async fn the_hub_outage_opens_one_repair_chat_per_service_per_hour() {
    let e = env();
    let down = ServiceOutage {
        person: None,
        service: "stt".into(),
        since: "09:12".into(),
        detail: Some("its health address does not answer".into()),
    };
    let last = Mutex::new(HashMap::new());
    let open = |at: i64| {
        let (c, o, last) = (e.ctx.clone(), down.clone(), &last);
        async move {
            if !repair_due(last, &o.service, at) {
                return Ok::<Option<String>, String>(None);
            }
            tokio::task::spawn_blocking(move || open_repair_chat(&c, &o, at))
                .await
                .unwrap()
                .map(Some)
        }
    };
    let first = open(T0).await.unwrap().expect("the outage opens a chat");
    assert_eq!(
        open(T0 + 60_000).await.unwrap(),
        None,
        "a second report inside the hour opens no second chat"
    );
    let again = open(T0 + REPAIR_GAP_MS + 1)
        .await
        .unwrap()
        .expect("an hour on, one more");
    assert_ne!(first, again);
    let title = |id: &str| -> String {
        e.db.snapshot(|tx| {
            tx.query_row("SELECT title FROM chats WHERE id = ?1", [id], |r| r.get(0))
        })
        .unwrap()
    };
    assert_eq!(title(&first), "Fix the speech-to-text service");
    let s = subs(&e.db, &first);
    assert_eq!(s.len(), 1, "the task is the chat's first message");
    assert_eq!(s[0].1, "app", "Helm opened the chat, not a person");
    assert_eq!(
        s[0].4,
        "The speech-to-text service has been unreachable since 09:12 (its health address does \
         not answer). Find out why and bring it back, then say it is back."
    );
    let calls = e.ops.calls.lock().unwrap().clone();
    let creates: Vec<&Value> = calls
        .iter()
        .filter(|(o, _)| o == "chat_create")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(creates.len(), 2, "no chat for the rate-limited report");
    let repair_folder = std::path::Path::new(&e.ctx.folder)
        .join("service-repair")
        .to_string_lossy()
        .into_owned();
    assert!(
        creates[0]["cwd"]
            .as_str()
            .unwrap()
            .ends_with(&repair_folder),
        "{creates:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(o, _)| o == "profiles_assign_chats")
            .count(),
        2,
        "each repair chat is kept to the profile whose line reported it"
    );
}

/// The hub's recovery closes the repair chat it opened for that service, and says nothing when
/// there is no repair chat for it.
#[tokio::test(flavor = "multi_thread")]
async fn the_hub_recovery_tells_the_repair_chat_it_opened() {
    let e = env();
    let down = ServiceOutage {
        person: None,
        service: "stt".into(),
        since: "09:12".into(),
        detail: Some("its health address does not answer".into()),
    };
    // Both helpers block on the runtime handle, so they run off the async thread.
    let c = e.ctx.clone();
    let chat = tokio::task::spawn_blocking(move || open_repair_chat(&c, &down, T0))
        .await
        .unwrap()
        .unwrap();
    let chats: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    chats.lock().unwrap().insert("stt".into(), chat.clone());
    let back = ServiceRecovered {
        person: None,
        service: "stt".into(),
        since: Some("09:12".into()),
        back: "10:05".into(),
    };
    let (c2, chats2, back2) = (e.ctx.clone(), chats.clone(), back.clone());
    tokio::task::spawn_blocking(move || stand_down_repair_chat(&c2, &chats2, &back2))
        .await
        .unwrap()
        .unwrap();
    let s = subs(&e.db, &chat);
    assert_eq!(
        s.len(),
        2,
        "the recovery is the repair chat's second message"
    );
    assert_eq!(
        s[1].4,
        "The speech-to-text service answers the hub's health probe again (back at 10:05; it \
         was unreachable since 09:12). If it checks out, say it is back and close this out."
    );
    // A second recovery for the same service (a flap, or a throttled outage the PC never got a
    // chat for): the entry went with the first stand-down, so the old chat is not woken again.
    let (c3, chats3, back3) = (e.ctx.clone(), chats.clone(), back.clone());
    tokio::task::spawn_blocking(move || stand_down_repair_chat(&c3, &chats3, &back3))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        subs(&e.db, &chat).len(),
        2,
        "one stand-down per repair chat"
    );
    // A service no repair chat was opened for: nothing to stand down, and nothing sent.
    let other = ServiceRecovered {
        person: None,
        service: "hub-search".into(),
        since: None,
        back: "now".into(),
    };
    let (c2b, chats2b, other2) = (e.ctx.clone(), chats.clone(), other.clone());
    tokio::task::spawn_blocking(move || stand_down_repair_chat(&c2b, &chats2b, &other2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(subs(&e.db, &chat).len(), 2, "no stray message");
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
    // The chat is named for the profile that owns the channel, never for the bot handle.
    let who =
        e.db.snapshot(|tx| helm_profiles::store::home(tx))
            .unwrap()
            .unwrap()
            .name;
    assert!(title.starts_with(&format!("Telegram {who} ")), "{title}");
    assert!(!title.contains("helm_bot"), "{title}");
    let calls = e.ops.calls.lock().unwrap().clone();
    assert_eq!(calls[0].0, "chat_create");
    assert!(
        calls[0].1["cwd"]
            .as_str()
            .unwrap()
            .ends_with(&e.ctx.profile_id),
        "{calls:?}"
    );
    let s = subs(&e.db, &chat);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].0, telegram::submission_id("hub-1"));
    assert_eq!(s[0].1, "telegram", "never the user's typed words");
    assert!(s[0].4.starts_with(telegram::BRIEF_NOTE));
    assert!(s[0].4.ends_with("hello from the phone"));
    // The hub resends it (no ack reached it): the same submission, nothing new. First let the
    // actor be done with the send: place runs here without the sink's serial lock, so a resend
    // must not race the actor still opening (or closing) the turn the send started.
    wait_quiet(&e.db, &telegram::submission_id("hub-1")).await;
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
    // A send that is refused as it stands (here: the chat was archived meanwhile) saves no files.
    let attachments = e.dir.path().join("prof").join("attachments");
    let count = || std::fs::read_dir(&attachments).unwrap().count();
    let before = count();
    let chat: String =
        e.db.snapshot(|tx| {
            tx.query_row(
                "SELECT chat_id FROM submissions WHERE id = ?1",
                [&sid],
                |r| r.get(0),
            )
        })
        .unwrap();
    sql(
        &e.db,
        "UPDATE chats SET archived = 1 WHERE id = ?1",
        &[&chat],
    );
    let later = uuid::Uuid::new_v4().to_string();
    let refused = host_send_with_attachments(
        &e.ctx.rt,
        HostSend {
            chat_id: &chat,
            submission_id: &later,
            text: "one more",
            origin: helm_chats::types::SubmissionOrigin::App,
            page: None,

            kind: None,
            wait: None,
        },
        &[],
        &[HostFile {
            name: "again.txt",
            bytes: b"again",
        }],
    );
    assert_eq!(refused.unwrap_err().code, "refused");
    assert_eq!(count(), before, "a refused send writes nothing");
    // The sweep removes the files nothing names once they are old, and nothing else: not a young
    // one (its send may still be running), not one a row names, not a stranger's file.
    let orphan = attachments.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("x.txt"), b"x").unwrap();
    let stranger = attachments.join("notes.txt");
    std::fs::write(&stranger, b"mine").unwrap();
    let now = helm_core::now_ms();
    let sweep = |at: i64| helm_chats::submit::sweep_orphan_attachments(&e.db, &attachments, at);
    assert_eq!((sweep(now), orphan.exists()), (0, true), "young");
    assert_eq!((sweep(now + 2 * 3_600_000), orphan.exists()), (1, false));
    assert!(stranger.exists());
    assert_eq!(
        count(),
        before + 1,
        "every named file stays, and the stranger's"
    );
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
        assert!(matches!(p, Placed::Sent { ignored: None, .. }));
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
    // The chat and the phone are told: the chat in its first message, the phone by the caller.
    let Placed::Sent { chat, ignored, .. } = p else {
        panic!("expected a placed message");
    };
    let why = ignored.expect("the refusal is told");
    assert!(
        why.contains("refused") && why.contains("app default"),
        "{why}"
    );
    let sent = subs(&e.db, &chat);
    assert!(
        sent[0].4.ends_with(&format!("[Helm] {why}")),
        "{}",
        sent[0].4
    );
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
    let path = crate::people::telegram_paths(&prof, &e.ctx.profile_id).policy;
    let base = telegram::PolicyFile {
        max_age_hours: 6,
        max_tokens: 50_000,
        away_alerts: true,
        approvals: false,
        remote_answers: false,
        take_telegram: None,
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
        let v = telegram_view(&e.db, &prof, &e.ctx.profile_id, T0);
        assert_eq!((v.model.as_deref(), v.effort.as_deref()), shown);
    }
    // A policy file written by an older Helm, with neither field, reads as unset.
    std::fs::write(
        &path,
        r#"{"maxAgeHours": 6, "maxTokens": 50000, "awayAlerts": true, "approvals": false}"#,
    )
    .unwrap();
    let v = telegram_view(&e.db, &prof, &e.ctx.profile_id, T0);
    assert_eq!((v.model.as_deref(), v.effort.as_deref()), (None, None));
    drop(e.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_chat_gets_a_steer_whatever_the_send_mode() {
    let e = env();
    let chat = chat_of(&place_async(&e.ctx, msg("a", "first")).await.unwrap());
    until_turned_down(&e, &chat, &telegram::submission_id("a")).await;
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
    until_turned_down(&e, &chat, &telegram::submission_id("b")).await;
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
        &json!({"chatId": chat, "message": "from remote", "kind": "update", "mode": "queue"}),
    )
    .unwrap();
    use crate::client::Executor;
    let r = exec.run("remote", None, call).await.unwrap();
    assert_eq!(r["chatId"], json!(chat));
    let s = subs(&e.db, &chat);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].1, "other_chat", "stored as another agent's message");
    assert!(s[0].4.contains("from remote"));
}

/// A chat some other PC runs, as this PC's copy knows it; the stand-in for the watcher answers
/// only what an outside call touches (list, read, send) and records the sends that cross.
const MIRRORED: &str = "0f0f0f0f-7777-8888-9999-000000000001";
const MIRRORED_ARCHIVED: &str = "0f0f0f0f-7777-8888-9999-000000000002";

#[derive(Default)]
struct FakeWatcher {
    sent: Mutex<Vec<helm_chats::types::ChatSendArgs>>,
    /// The person each send acted as (`None`: the default the sidebar decides).
    acted_as: Mutex<Vec<Option<String>>>,
}

impl helm_chats::remote::RemoteChats for FakeWatcher {
    fn owns(&self, chat_id: &str) -> Option<String> {
        matches!(chat_id, MIRRORED | MIRRORED_ARCHIVED).then(|| "dev-2".to_owned())
    }
    fn this_device(&self) -> String {
        "dev-1".to_owned()
    }
    fn device_name(&self, device_uid: &str) -> String {
        device_uid.to_owned()
    }
    fn page(
        &self,
        a: helm_chats::types::TranscriptPageArgs,
    ) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        assert_eq!(a.chat_id, MIRRORED, "only the living copy is paged");
        Box::pin(async move {
            Ok(json!({
                "messages": [{"id": 2, "role": "user", "text": "Over there.", "timestamp": 900}],
                "hasOlder": false,
            }))
        })
    }
    fn send(
        &self,
        a: helm_chats::types::ChatSendArgs,
    ) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        self.send_as(a, None)
    }
    fn send_as(
        &self,
        a: helm_chats::types::ChatSendArgs,
        person: Option<String>,
    ) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        self.sent.lock().unwrap().push(a);
        self.acted_as.lock().unwrap().push(person);
        Box::pin(async move { Ok(json!({"submission": {"state": "accepted"}, "chat": {}})) })
    }
    fn get(&self, _chat_id: String) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        Box::pin(async { panic!("not used") })
    }
    fn stop(&self, _chat_id: String) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        Box::pin(async { panic!("not used") })
    }
    fn chat_op(
        &self,
        _op: &str,
        _args: serde_json::Value,
    ) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        Box::pin(async { panic!("not used") })
    }
    fn answer(
        &self,
        _a: helm_chats::types::InteractionAnswerArgs,
    ) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        Box::pin(async { panic!("not used") })
    }
    fn set_model(
        &self,
        _a: helm_chats::types::ChatSetModelArgs,
    ) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        Box::pin(async { panic!("not used") })
    }
    fn rename(
        &self,
        _chat_id: String,
        _title: String,
    ) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        Box::pin(async { panic!("not used") })
    }
    fn archive(
        &self,
        _chat_id: String,
        _archived: bool,
    ) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        Box::pin(async { panic!("not used") })
    }
    fn draft_save(
        &self,
        _a: helm_chats::types::DraftSaveArgs,
    ) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        Box::pin(async { panic!("not used") })
    }
    fn catalogue(&self, _device_uid: String) -> helm_chats::remote::Fut<helm_chats::remote::Res> {
        Box::pin(async { panic!("not used") })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn outside_calls_reach_reads_and_sends_cross_to_another_pc() {
    let e = env();
    // Sam, linked to a hub person, and the other PC's chats as this PC's copies know them.
    e.db
        .write(|tx| {
            tx.execute_batch(
                "INSERT INTO profiles (id, name, colour, hub_person_id, is_home, created_at)
                 VALUES ('p-sam', 'Sam', '', 'person-s', 0, 0);
                 INSERT INTO mirror_chats (chat_id, device_uid, record, person_id, shared_with) VALUES
                   ('0f0f0f0f-7777-8888-9999-000000000001', 'dev-2',
                    '{\"title\":\"On the laptop\",\"archived\":false,\"status\":\"idle\",\"updatedAt\":900,\"createdAt\":800,\"harness\":\"claude\",\"pinned\":false,\"muted\":false}',
                    'person-s', '[]'),
                   ('0f0f0f0f-7777-8888-9999-000000000002', 'dev-2',
                    '{\"title\":\"Archived there\",\"archived\":true,\"status\":\"idle\",\"updatedAt\":800,\"createdAt\":800,\"harness\":\"claude\",\"pinned\":false,\"muted\":false}',
                    'person-s', '[]');",
            )
        })
        .unwrap();
    let watcher = Arc::new(FakeWatcher::default());
    let slot = helm_chats::remote::RemoteSlot::default();
    slot.attach(watcher.clone());
    let mut services = helm_ipc::Services::new();
    services.insert(e.db.clone());
    services.insert(Arc::new(e.ctx.rt.clone()));
    services.insert(Arc::new(slot));
    let mut reg = helm_hosttools::HostToolRegistry::new();
    helm_chats::tools::register_host_tools(&mut reg);
    let exec = AppExecutor::default();
    exec.wire(crate::exec::Wiring {
        services: Arc::new(services),
        events: Arc::new(helm_ipc::NullSink),
        tools: Arc::new(helm_hosttools::HostTools::new(Arc::new(reg))),
    });
    use crate::client::Executor;
    // A listing for Sam with the other PCs in scope shows their chat there; the home person,
    // whose copies own nothing, does not. Nothing limits which PC is asked; what a listing
    // shows is the acting person's view.
    let call =
        crate::gate::admit("app_list_chats", &json!({"person": "any", "device": "any"})).unwrap();
    let r = exec.run("remote", Some("person-s"), call).await.unwrap();
    let ids: Vec<&str> = r["chats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["chatId"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&MIRRORED), "{ids:?}");
    // With no person named, the list is the acting person's own, counted before paging; the
    // caller's own `person` is theirs to ask for, and `any` is not narrowed back.
    let call = crate::gate::admit("app_list_chats", &json!({"device": "any"})).unwrap();
    let r = exec.run("remote", Some("person-s"), call).await.unwrap();
    assert!(
        r["chats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["chatId"] == MIRRORED),
        "{r}"
    );
    let call = crate::gate::admit("app_list_chats", &json!({"device": "any"})).unwrap();
    let r = exec.run("remote", None, call).await.unwrap();
    assert!(
        !r["chats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["chatId"] == MIRRORED),
        "the home person's copies hold nothing of it: {r}"
    );
    // An outside read of that chat answers from the copy and the watcher's page.
    let call = crate::gate::admit("app_read_chat", &json!({"chatId": MIRRORED})).unwrap();
    let r = exec.run("remote", None, call).await.unwrap();
    assert_eq!(r["chat"]["title"], "On the laptop");
    assert_eq!(r["messages"][0]["text"], "Over there.");
    // A send for Sam crosses, framed as acting for them, steer, and not the composer's.
    let call = crate::gate::admit(
        "app_send_message",
        &json!({"chatId": MIRRORED, "message": "Cross-PC note.", "kind": "update", "mode": "steer"}),
    )
    .unwrap();
    let r = exec
        .run("remote-caller", Some("person-s"), call)
        .await
        .unwrap();
    assert_eq!(r["delivered"], true);
    assert_eq!(r["title"], "On the laptop");
    {
        let mut s = watcher.sent.lock().unwrap();
        assert_eq!(s.len(), 1);
        let sent = s.pop().unwrap();
        assert_eq!(sent.action, helm_chats::types::SendAction::Steer);
        assert_eq!(sent.origin, Some(helm_chats::types::SendOrigin::Action));
        assert!(
            sent.text
                .starts_with("[Message from an agent acting for \"Sam\""),
            "{}",
            sent.text
        );
        assert!(sent.text.ends_with("\n\nCross-PC note."));
    }
    // An archived copy refuses, and a chat no copy holds stays unknown.
    let call = crate::gate::admit(
        "app_send_message",
        &json!({"chatId": MIRRORED_ARCHIVED, "message": "x", "kind": "update", "mode": "steer"}),
    )
    .unwrap();
    let e1 = exec.run("remote-caller", None, call).await.unwrap_err();
    assert!(e1.text.contains("archived"), "{}", e1.text);
    let call = crate::gate::admit(
        "app_send_message",
        &json!({"chatId": "0f0f0f0f-7777-8888-9999-000000000003", "message": "x", "kind": "update", "mode": "steer"}),
    )
    .unwrap();
    let e2 = exec.run("remote-caller", None, call).await.unwrap_err();
    assert!(e2.text.contains("no chat"), "{}", e2.text);
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
    bare.stop(None).await;
    let sink = wired_sink(&e);
    assert!(
        sink.deliver(msg("x", "y")).await.is_err(),
        "no op dispatcher yet"
    );
    sink.attach_ops(e.ops.clone());
    // The model and effort saved in Settings reach the new chat: the sink reads the saved
    // settings file per delivery and hands them to `chat_create`.
    telegram::save_policy(
        &crate::people::telegram_paths(&e.dir.path().join("prof"), &e.ctx.profile_id).policy,
        telegram::PolicyFile {
            max_age_hours: 6,
            max_tokens: 50_000,
            away_alerts: true,
            approvals: false,
            remote_answers: false,
            take_telegram: None,
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
    sink.stop(None).await;
    let calls = e.ops.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|(o, a)| o == "chat_stop" && a["chatId"] == json!(tag.chat_id)),
        "{calls:?}"
    );
}

/// What went up to the phone as a note, in order.
#[derive(Default)]
struct RecUp {
    notes: Mutex<Vec<(String, String)>>,
}
impl Upstream for RecUp {
    fn status(&self, _person: &str, _text: &str) {}
    fn final_answer(&self, _person: &str, _text: &str) -> bool {
        true
    }
    fn notify(&self, person: &str, text: &str) {
        self.notes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((person.to_owned(), text.to_owned()));
    }
}

fn chat_count(db: &Database) -> i64 {
    db.snapshot(|tx| tx.query_row("SELECT COUNT(*) FROM chats", [], |r| r.get(0)))
        .unwrap()
}

#[test]
fn the_mode_command_parser_takes_only_a_leading_mode_word() {
    let rows: &[(&str, Option<ModeCommand>)] = &[
        ("/mode", Some(ModeCommand::Show)),
        ("  /mode  ", Some(ModeCommand::Show)),
        ("/mode run", Some(ModeCommand::Set("run"))),
        ("/mode complete", Some(ModeCommand::Set("run"))),
        ("/mode check", Some(ModeCommand::Set("check_in"))),
        ("/mode checkin", Some(ModeCommand::Set("check_in"))),
        ("/mode check-in", Some(ModeCommand::Set("check_in"))),
        ("/mode check_in", Some(ModeCommand::Set("check_in"))),
        ("/mode ask", Some(ModeCommand::Set("check_in"))),
        ("/mode@helm_bot run", Some(ModeCommand::Set("run"))),
        ("/Mode Run", Some(ModeCommand::Set("run"))),
        ("/mode run now", Some(ModeCommand::Set("run"))),
        ("/mode nonsense", Some(ModeCommand::Usage)),
        ("/modem", None),
        ("/modem run", None),
        ("/modes run", None),
        ("mode run", None),
        ("the /mode run", None),
        ("", None),
        ("/", None),
    ];
    for (text, want) in rows {
        assert_eq!(&mode_command(text), want, "{text:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mode_commands_are_answered_to_the_phone_and_never_reach_a_chat() {
    let e = env();
    let sink = wired_sink(&e);
    sink.attach_ops(e.ops.clone());
    let up = Arc::new(RecUp::default());
    sink.attach_line(up.clone());
    let said = || -> Vec<String> {
        up.notes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
            .map(|(_, t)| t)
            .collect()
    };
    let clear_calls = || e.ops.calls.lock().unwrap().clear();

    // Show: the mode, the overrides and the counts go up; nothing is asked of any op but the read.
    sink.deliver(msg("w1", "/mode")).await.unwrap();
    let calls = e.ops.calls.lock().unwrap().clone();
    assert_eq!(calls.as_slice(), [("workmode_get".to_owned(), json!({}))]);
    let replies = said();
    assert_eq!(replies.len(), 1);
    assert!(
        replies[0].contains("Working mode: Check in with me."),
        "{replies:?}"
    );
    assert!(
        replies[0].contains("Boards with a mode of their own: 1."),
        "{replies:?}"
    );
    assert!(
        replies[0].contains("Decided for you: 4 \u{b7} need you: 1"),
        "{replies:?}"
    );
    assert_eq!(chat_count(&e.db), 0, "no chat was opened");
    assert!(telegram::load_tag(&e.ctx.tag_path).is_none());

    // Set words, synonyms, a bot handle and mixed case: each reaches one app-scoped set and
    // confirms the new mode on the phone.
    for (words, want) in [
        ("/mode run", "run"),
        ("/mode complete", "run"),
        ("/mode check", "check_in"),
        ("/mode checkin", "check_in"),
        ("/mode check-in", "check_in"),
        ("/mode ask", "check_in"),
        ("/mode@helm_bot run", "run"),
    ] {
        clear_calls();
        sink.deliver(msg(&format!("w{words}"), words))
            .await
            .unwrap();
        let sets = e
            .ops
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(o, _)| o == "workmode_set")
            .map(|(_, a)| a.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            sets,
            vec![json!({ "scope": "app", "mode": want })],
            "{words}"
        );
        let replies = said();
        let want_text = if want == "run" {
            "Run to completion"
        } else {
            "Check in with me"
        };
        assert_eq!(replies.len(), 1, "{words}");
        assert!(
            replies[0].starts_with("Working mode is now ") && replies[0].contains(want_text),
            "{words}: {replies:?}"
        );
    }
    assert_eq!(chat_count(&e.db), 0, "still no chat");

    // Something the command does not know gets the usage line, with no op at all.
    clear_calls();
    sink.deliver(msg("w2", "/mode sideways")).await.unwrap();
    assert!(e.ops.calls.lock().unwrap().is_empty());
    let replies = said();
    assert_eq!(replies.len(), 1);
    assert!(
        replies[0].contains("\"/mode run\" and \"/mode check\""),
        "{replies:?}"
    );

    // An op that fails gets a plain line, for a read and for a set.
    e.ops
        .fail_workmode
        .store(true, std::sync::atomic::Ordering::SeqCst);
    sink.deliver(msg("w3", "/mode")).await.unwrap();
    sink.deliver(msg("w4", "/mode run")).await.unwrap();
    let replies = said();
    assert_eq!(replies.len(), 2);
    assert!(
        replies[0].contains("could not read the working mode"),
        "{replies:?}"
    );
    assert!(
        replies[1].contains("could not change the working mode"),
        "{replies:?}"
    );
    e.ops
        .fail_workmode
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // A household person's phone: this PC may well take her Telegram, but the working mode
    // belongs to the person whose profile this machine is home to. Her `/mode` gets one plain
    // line, reaches no op, opens no chat, and changes nothing.
    e.db.write(|tx| helm_profiles::store::create(tx, "Her", None, Some("hub-h"), 0))
        .unwrap();
    clear_calls();
    let mut hers = msg("wh1", "/mode run");
    hers.person = Some("hub-h".into());
    sink.deliver(hers).await.unwrap();
    assert!(
        e.ops.calls.lock().unwrap().is_empty(),
        "a household person's /mode reached an op"
    );
    let replies = said();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(
        replies[0].contains("only its owner can change it from a phone"),
        "{replies:?}"
    );
    assert_eq!(chat_count(&e.db), 0, "her /mode opened no chat");
    // And her own messages still flow: a plain one opens her chat as usual.
    let mut plain = msg("wh2", "how are things");
    plain.person = Some("hub-h".into());
    sink.deliver(plain).await.unwrap();
    assert!(chat_count(&e.db) > 0, "her plain message opened no chat");

    // `/modem` and a message merely starting "mode" are ordinary messages: stored in the chat.
    clear_calls();
    sink.deliver(msg("w5", "/modem run")).await.unwrap();
    sink.deliver(msg("w6", "mode run")).await.unwrap();
    assert!(
        !e.ops
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(o, _)| o.starts_with("workmode")),
        "the mode ops were called for plain messages"
    );
    let tag = telegram::load_tag(&e.ctx.tag_path).expect("the messages opened the telegram chat");
    let stored: Vec<String> = subs(&e.db, &tag.chat_id)
        .into_iter()
        .map(|(_, origin, _, _, text)| {
            assert_eq!(origin, "telegram");
            text
        })
        .collect();
    // (what is stored carries the inbound note in front, as every delivered message does)
    assert_eq!(stored.len(), 2, "{stored:?}");
    assert!(stored[0].ends_with("/modem run"), "{stored:?}");
    assert!(stored[1].ends_with("mode run"), "{stored:?}");
    assert!(said().is_empty(), "plain messages answered nothing");
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
    assert!(
        matches!(&p, Placed::Sent { ignored: Some(w), .. } if w.contains("not available")),
        "{p:?}"
    );
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

/// The chat is told its answer did not reach the phone only when the line stays down: an answer
/// the line takes again within the grace delivers itself, and the note would only make the agent
/// send it a second time. The note is sent once.
#[test]
fn the_chat_is_told_of_an_undelivered_answer_only_after_the_line_stays_down() {
    let g = UNDELIVERED_GRACE_MS;
    // (when the answer was offered, whether the line took it, whether the chat is told now)
    #[allow(clippy::type_complexity)]
    let rows: &[(&str, &[(i64, bool, bool)])] = &[
        (
            "a short drop that heals",
            &[
                (0, false, false),
                (500, false, false),
                (g - 1, false, false),
                (g + 500, true, false),
            ],
        ),
        (
            "down for good: told once",
            &[
                (0, false, false),
                (g - 1, false, false),
                (g, false, true),
                (g + 500, false, false),
            ],
        ),
        (
            "a break restarts the wait",
            &[
                (0, false, false),
                (g - 500, true, false),
                (g, false, false),
                (g + g - 1, false, false),
                (g + g, false, true),
            ],
        ),
        ("taken at once", &[(0, true, false)]),
    ];
    for (name, offers) in rows {
        let mut late = Undelivered::default();
        for (now, ok, told) in *offers {
            assert_eq!(late.offered(*ok, *now), *told, "{name} at {now}");
        }
    }
}

// ------------------------------------------------------------------ the voice chat

use crate::client::{SpeakerSink, VoiceSink, VoiceTurn};
use crate::voice;
use crate::voice_heard::{self, HeardStore};
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

    // A mirror carried on for a message that came in as the last one finished: it answers a turn
    // that starts, and finishes without a word when nothing does (the message was answered).
    let mut quiet = vs::Mirror::resumed(9, T0);
    assert!(quiet.step(&idle(None), T0 + 1_000).is_empty() && !quiet.done);
    assert!(quiet.step(&idle(None), T0 + 5_000).is_empty() && quiet.done);
    let mut carried = vs::Mirror::resumed(9, T0);
    let mut done = busy(&[], &[]);
    done.turn_done = Some((12, "Yes.".into()));
    let answer = owed_of(&carried.step(&done, T0 + 500));
    assert_eq!(answer.what, vs::What::Answer("Yes.".into()));

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
        person: None,
        source: "speech".into(),
        sent_at: 0,
        age_minutes: None,
        continues: None,
    }
}

fn vctx(e: &Env, file: voice::PolicyFile) -> vs::Ctx {
    let prof = e.dir.path().join("prof");
    let paths = crate::people::voice_paths(&prof, &e.ctx.profile_id);
    let policy = file.check().unwrap();
    vs::Ctx {
        rt: e.ctx.rt.clone(),
        db: e.db.clone(),
        ops: e.ops.clone(),
        handle: tokio::runtime::Handle::current(),
        profile_id: e.ctx.profile_id.clone(),
        person: String::new(),
        tag_path: paths.tag,
        policy_path: paths.policy,
        heard_path: paths.heard,
        folder: plain_path(&voice::folder(&prof)),
        policy,
        file,
    }
}

async fn vplace(c: &vs::Ctx, t: VoiceTurn, now: i64) -> Result<vs::Placed, String> {
    vplace_with(c, t, &[], now).await
}

async fn vplace_with(
    c: &vs::Ctx,
    t: VoiceTurn,
    notes: &[String],
    now: i64,
) -> Result<vs::Placed, String> {
    let (c, notes) = (c.clone(), notes.to_vec());
    tokio::task::spawn_blocking(move || vs::place(&c, &t, &notes, now))
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

/// Wait until the chat's actor has dealt with the turn just sent. There is no worker in these
/// tests, so the turn is turned down and the chat stops being busy; waiting on that, not on a
/// fixed time, keeps a status the test writes next from being overwritten under load.
async fn until_turned_down(e: &Env, chat: &str, submission: &str) {
    use rusqlite::OptionalExtension as _;
    for _ in 0..600 {
        let (state, status): (Option<String>, Option<String>) =
            e.db.snapshot(|tx| {
                Ok::<_, rusqlite::Error>((
                    tx.query_row(
                        "SELECT state FROM submissions WHERE id = ?1",
                        [submission],
                        |r| r.get(0),
                    )
                    .optional()?,
                    tx.query_row("SELECT status FROM chats WHERE id = ?1", [chat], |r| {
                        r.get(0)
                    })
                    .optional()?,
                ))
            })
            .unwrap();
        let settled = state.as_deref() != Some("saved")
            && status.is_some_and(|s| !helm_chats::types::ChatStatus::parse(&s).is_busy());
        if settled {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the chat never dealt with the turn");
}

fn set_idle(e: &Env, chat: &str) {
    sql(
        &e.db,
        "UPDATE chats SET status = 'idle' WHERE id = ?1",
        &[&chat],
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn voice_turns_enter_one_chat_with_origin_voice_its_mode_note_and_a_new_chat_after_idle() {
    let e = env();
    let c = vctx(&e, voice::PolicyFile::default());
    let t0 = helm_core::now_ms();

    // The first turn starts "Phone voice <profile name> <date>" on the saved model and effort.
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
    let who =
        e.db.snapshot(|tx| helm_profiles::store::home(tx))
            .unwrap()
            .unwrap()
            .name;
    assert!(title.starts_with(&format!("Phone voice {who} ")), "{title}");
    let s = subs(&e.db, &chat);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].0, voice::submission_id("t1"));
    assert_eq!(s[0].1, "voice", "never the user's typed words");
    assert!(
        s[0].4.starts_with(
            "[Voice · conversation: speak it, short, no markup]\nSynthetic question one."
        ),
        "{}",
        s[0].4
    );
    assert_eq!(conversation.len(), 16);

    // The hub resends it while its turn runs (Helm restarted before the acknowledgement): stored
    // once, and the turn is watched. Once the turn is over, there is nothing more to do.
    until_turned_down(&e, &chat, &voice::submission_id("t1")).await;
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

    // The next turn goes to the same chat and conversation: its mode and its lateness.
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

    // What the phone did not hear rides along on the next message, once, and only in the
    // conversation it belongs to.
    set_idle(&e, &chat);
    let store = HeardStore::new(
        crate::people::voice_paths(&e.dir.path().join("prof"), &e.ctx.profile_id).heard,
    );
    let spoken = "Sure thing, the build finished and everything looks fine today.";
    store.sent("rA", &conversation, spoken, 5, t0 + 70_000);
    store.sent("rB", "another-conversation", "Elsewhere.", 1, t0 + 70_000);
    store.report(
        &[
            json!({"reply": "rA", "state": "partial", "sentence": 2, "at": 1}),
            json!({"reply": "rB", "state": "none", "at": 1}),
        ],
        t0 + 80_000,
    );
    let pending = store.pending(Some(&conversation), t0 + 90_000);
    let p = vplace_with(
        &c,
        vturn("t2b", "Synthetic question two b.", "conversation"),
        &pending.lines,
        t0 + 90_000,
    )
    .await
    .unwrap();
    assert_eq!(vchat(&p), (chat.clone(), conversation.clone()));
    store.clear(&pending.ids, t0 + 90_000);
    let carried = subs(&e.db, &chat)
        .into_iter()
        .find(|r| r.0 == voice::submission_id("t2b"))
        .unwrap();
    assert_eq!(
        carried.4,
        "[Voice · conversation: speak it, short, no markup]\n[Not heard by the user: your reply \"Sure thing, the build finished and everything looks…\" stopped after sentence 2 of 5]\nSynthetic question two b."
    );
    assert_eq!(
        store.pending(Some(&conversation), t0 + 91_000),
        voice_heard::Pending::default()
    );

    // Idle for longer than the limit: a new chat and conversation. A
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
    assert!(subs(&e.db, &chat3)[0].4.starts_with("[Voice · "));
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
    fn status(&self, _person: &str, _turn: &str, _text: &str) {}
    fn reply(&self, _person: &str, _reply: &voice::Reply) -> bool {
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
    let tag_path = crate::people::voice_paths(&e.dir.path().join("prof"), &e.ctx.profile_id).tag;
    let tag = voice::load_tag(&tag_path).unwrap();
    assert_eq!(tag.conversation, live.conversation);

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
    v.stop(None, "an-older-turn".into()).await;
    assert!(stops().is_empty());
    v.stop(None, "t1".into()).await;
    assert_eq!(stops(), vec![json!({ "chatId": tag.chat_id })]);

    // The phone's screen is remembered; a fresh start drops the tag.
    v.presence(None, true, "chat");
    let live = v.live();
    assert_eq!(
        (live.presence_live, live.presence_mode),
        (true, voice::Mode::Chat)
    );
    // What the phone reports having heard is kept for the next message, and wakes nothing: a
    // report about an answer this PC sent counts, one about another does not.
    let store = HeardStore::new(
        crate::people::voice_paths(&e.dir.path().join("prof"), &e.ctx.profile_id).heard,
    );
    let said = helm_core::now_ms();
    store.sent("rX", &live.conversation, "Hello there, friend.", 3, said);
    v.heard(
        None,
        vec![
            json!({"reply": "rX", "state": "partial", "sentence": 2, "at": 1}),
            json!({"reply": "rY", "state": "none", "at": 1}),
        ],
    )
    .await;
    assert_eq!(
        store.pending(Some(&live.conversation), said).lines,
        ["[Not heard by the user: your reply \"Hello there, friend.\" stopped after sentence 2 of 3]"]
    );
    v.fresh(None).await;
    assert!(voice::load_tag(&tag_path).is_none());

    // What Settings shows: the saved policy, and no chat once the tag is gone.
    let view = vs::voice_view(
        &e.db,
        &e.dir.path().join("prof"),
        &e.ctx.profile_id,
        helm_core::now_ms(),
    );
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
    fn status(&self, _person: &str, turn: &str, text: &str) {
        self.sent
            .lock()
            .unwrap()
            .push(json!({ "status": text, "turn": turn }));
    }
    fn reply(&self, _person: &str, reply: &voice::Reply) -> bool {
        let up = self.up.load(AtomicOrdering::SeqCst);
        if up {
            self.sent.lock().unwrap().push(reply.frame());
        }
        up
    }
    fn attachment(&self, _person: &str, frame: &Value) -> bool {
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
    let heard_dir = tempfile::tempdir().unwrap();
    let heard = HeardStore::at(heard_dir.path());
    let mut out = vs::Outbox::new(wire.clone(), String::new(), shown.clone(), "chat1".into())
        .with_heard(heard.clone());
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

    // The next turn's answer takes what was queued since; another chat's queue is not touched.
    let live = vs::Live {
        turn: Some("t2".into()),
        ..live
    };
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

    // A turn has one `voice_final`: a second answer for it (it said it was waiting on an
    // approval, then finished) goes up as news about the voice chat.
    out.set_title(Some("Voice 2026-01-01".into()));
    let later = vs::Owed::unrowed("r2b", vs::What::Answer("Approved and done.".into()));
    assert!(out.send(&Phone, &live, Some("c1"), &later, 7));
    let f = sent().pop().unwrap();
    assert_eq!(
        (f["type"].as_str(), f["turn"].clone(), f["text"].as_str()),
        (
            Some("voice_notify"),
            Value::Null,
            Some("Approved and done.")
        )
    );
    assert_eq!(f["about"], json!({ "chat": "Voice 2026-01-01" }));
    wire.sent.lock().unwrap().pop();

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
    assert_eq!(sent()[6], json!({ "status": "Thinking", "turn": "t2" }));

    // Every spoken answer that is offered is noted for the phone's report, under the conversation
    // it went up in: the first (offered twice, sent once) and the next. An answer read on screen
    // (the news above went up in chat mode), one that never went up (`[quiet]`, or not for the
    // phone) and one this PC never made are not.
    let reports = [
        json!({"reply": "r1", "state": "none", "at": 1}),
        json!({"reply": "r2", "state": "all", "at": 1}),
        json!({"reply": "r3", "state": "none", "at": 1}),
        json!({"reply": "r4", "state": "none", "at": 1}),
    ];
    assert_eq!(heard.report(&reports, 20), 2);
    let here = heard.pending(Some("c1"), 20);
    assert_eq!(
        here.lines,
        ["[Not heard by the user: your reply \"Here it is.\" was never played]"]
    );
    assert_eq!(here.ids, ["r1"]);
    assert!(heard.pending(Some("c2"), 20).lines.is_empty());
}

fn a_note(chat: &str) -> voice_watch::Note {
    let empty = AttentionSummary {
        badge: 0,
        busy: 0,
        needs_you: vec![],
        running: vec![],
        unread: vec![],
        finished: vec![],
        stalled: vec![],
        flagged: vec![],
        muted: vec![],
        triage: vec![],
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
        profile_id: String::new(),
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
    let tag = voice::load_tag(&crate::people::voice_paths(&prof, &e.ctx.profile_id).tag).unwrap();
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
        .find(|r| r.0 == voice::note_submission_id(&note.key))
        .unwrap();
    assert_eq!(n.1, "app");
    assert!(
        n.4.starts_with("[Attention] Build: Waiting for an approval"),
        "{}",
        n.4
    );
    assert_eq!(
        voice::load_tag(&crate::people::voice_paths(&prof, &e.ctx.profile_id).tag)
            .unwrap()
            .last_turn_at,
        tag.last_turn_at
    );

    // The transcript's user rows say how each message entered, in order, after any row. The rows
    // are written here, once the chat has dealt with the note, over any row the chat wrote for
    // it itself.
    until_turned_down(&e, &chat, &voice::note_submission_id(&note.key)).await;
    sql(&e.db, "DELETE FROM messages WHERE chat_id = ?1", &[&chat]);
    for (sid, text) in [
        (voice::submission_id("t1"), "asked"),
        (voice::note_submission_id(&note.key), "noted"),
    ] {
        sql(
            &e.db,
            "INSERT INTO messages(chat_id, source_key, role, text, timestamp)
             VALUES (?1, ?2, 'user', ?3, 1)
             ON CONFLICT(chat_id, source_key) DO UPDATE SET text = excluded.text",
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

// ------------------------------------------------------------------ one channel per person

/// Two people on one PC: each one's Telegram frame lands in their own profile, under their own
/// policy and tag; a stranger's is refused; an away alert and a Remote-page answer follow the
/// chat's owner; and changing "take it on this PC" tells the hub at once.
#[tokio::test(flavor = "multi_thread")]
async fn each_person_has_their_own_channel_from_frame_to_alert() {
    use helm_profiles::store::{self, Patch};
    let e = env();
    let prof = e.dir.path().join("prof");
    let beta =
        e.db.write(|tx| -> Result<String, store::ProfileError> {
            let home = store::home(tx)?.expect("made at open");
            store::update(
                tx,
                &home.id,
                Patch {
                    hub_person_id: Some(Some("person-a".into())),
                    ..Default::default()
                },
            )?;
            Ok(store::create(tx, "Beta", None, Some("person-b"), 2)?.id)
        })
        .unwrap();
    let alpha = e.ctx.profile_id.clone();
    let file = |model: Option<&str>, remote_answers: bool| telegram::PolicyFile {
        max_age_hours: 6,
        max_tokens: 50_000,
        away_alerts: true,
        approvals: false,
        remote_answers,
        take_telegram: None,
        model: model.map(str::to_owned),
        effort: None,
    };
    telegram::save_policy(
        &crate::people::telegram_paths(&prof, &beta).policy,
        file(Some("model-b"), true),
    )
    .unwrap();
    telegram::save_policy(
        &crate::people::telegram_paths(&prof, &alpha).policy,
        file(None, false),
    )
    .unwrap();

    let sink = wired_sink(&e);
    sink.attach_ops(e.ops.clone());
    let from = |person: &str, id: &str| TelegramMessage {
        person: Some(person.to_owned()),
        ..msg(id, "hello")
    };
    sink.deliver(from("person-a", "a1")).await.unwrap();
    sink.deliver(from("person-b", "b1")).await.unwrap();
    assert!(
        sink.deliver(from("person-z", "z1")).await.is_err(),
        "a person nobody here acts for"
    );
    let calls = e.ops.calls.lock().unwrap().clone();
    let created: Vec<&Value> = calls
        .iter()
        .filter(|(o, _)| o == "chat_create")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(created.len(), 2);
    assert_eq!(created[0].get("model"), None, "A's policy has no model");
    assert_eq!(created[1]["model"], "model-b", "B's own policy applies");
    let assigned: Vec<String> = calls
        .iter()
        .filter(|(o, _)| o == "profiles_assign_chats")
        .map(|(_, a)| a["profileId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(assigned, [alpha.clone(), beta.clone()]);
    let tag = |pid: &str| {
        telegram::load_tag(&crate::people::telegram_paths(&prof, pid).tag)
            .expect("each person has their own tag")
            .chat_id
    };
    let (chat_a, chat_b) = (tag(&alpha), tag(&beta));
    assert_ne!(chat_a, chat_b);

    // The chats now belong to their people (the fake op above only recorded the assignment): an
    // alert for one goes to that person's phone, and only that person's Remote switch counts.
    for (chat, pid) in [(&chat_a, &alpha), (&chat_b, &beta)] {
        sql(
            &e.db,
            "UPDATE chats SET profile_id = ?2 WHERE id = ?1",
            &[chat, pid],
        );
    }
    for (chat, person) in [(&chat_a, "person-a"), (&chat_b, "person-b")] {
        let ch = crate::notify::owner_channel(&e.db, &prof, chat).unwrap();
        assert_eq!(ch.person, person, "the alert carries the owner's person");
    }
    let policy_a = crate::people::telegram_paths(&prof, &alpha).policy;
    for (chat, open) in [(&chat_a, false), (&chat_b, true)] {
        assert_eq!(
            crate::remote::owner_switch(&prof, &e.db, chat, &policy_a),
            open,
            "the owner's switch decides, not the caller's"
        );
    }

    // "Take it on this PC" is said to the hub when it changes, and not when it does not.
    let mut services = helm_ipc::Services::new();
    services.insert(e.db.clone());
    services.insert(Arc::new(helm_core::profile::Profile {
        dir: prof.clone(),
        hash: String::new(),
    }));
    let events = Arc::new(helm_ipc::RecordingSink::default());
    let ctx = helm_ipc::OpCtx {
        services: Arc::new(services),
        events: events.clone(),
    };
    let mut args = serde_json::to_value(file(None, false)).unwrap();
    args["profileId"] = json!(beta);
    args["takeTelegram"] = json!(true);
    crate::telegram_save(&ctx, args.clone()).unwrap();
    crate::telegram_save(&ctx, args).unwrap();
    assert_eq!(events.kinds(), ["device_changed"]);
}

// -------------------------------------------------------------- the room speaker's sink

/// A line that records what the speaker lane is told, and can be down.
#[derive(Default)]
struct SpWire {
    sent: Mutex<Vec<Value>>,
    up: AtomicBool,
}

impl crate::speaker_sink::SpeakerUpstream for SpWire {
    fn status(&self, _person: &str, turn: &str, text: &str) {
        self.sent
            .lock()
            .unwrap()
            .push(json!({ "status": text, "turn": turn }));
    }
    fn reply(&self, _person: &str, reply: &voice::Reply) -> bool {
        let up = self.up.load(AtomicOrdering::SeqCst);
        if up {
            self.sent.lock().unwrap().push(reply.frame());
        }
        up
    }
}

fn srec(id: &str, session: &str, leftover: &[&str]) -> crate::client::SpeakerRecord {
    crate::client::SpeakerRecord {
        id: id.into(),
        device: Some("desk".into()),
        room: Some("kitchen".into()),
        area: None,
        session: session.into(),
        started_at: 1_000,
        text: "Turn the lamp on.".into(),
        answer: "Done.".into(),
        kind: "answer".into(),
        tools: vec![crate::client::RecordedTool {
            name: "light_on".into(),
            summary: "kitchen lamp".into(),
            ok: true,
        }],
        leftover: leftover.iter().map(|l| (*l).to_owned()).collect(),
        model: Some("small".into()),
        person: None,
    }
}

fn sturn(id: &str, text: &str, session: &str) -> crate::client::SpeakerTurn {
    crate::client::SpeakerTurn {
        id: id.into(),
        text: text.into(),
        device: None,
        room: Some("kitchen".into()),
        area: None,
        session: session.into(),
        sent_at: 0,
        person: None,
        context: None,
        escalated: false,
    }
}

fn wired_speaker(e: &Env) -> crate::speaker_sink::HouseSpeaker {
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
    crate::speaker_sink::HouseSpeaker::new(exec, e.dir.path().join("prof"))
}

/// Waits for the mirror's look cycle to settle, up to about ten seconds.
async fn waits(until: impl Fn() -> bool) -> bool {
    for _ in 0..40 {
        if until() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    until()
}

#[tokio::test(flavor = "multi_thread")]
async fn one_talk_one_speaker_chat_whose_answer_goes_up_once_and_the_hub_s_word_ends_it() {
    let e = env();
    let dir = e.dir.path().join("prof");
    let paths = crate::speaker::paths(&dir, &e.ctx.profile_id);
    // The saved settings name the model and effort a new speaker chat opens with.
    crate::speaker::save_policy(
        &paths.policy,
        &crate::speaker::PolicyFile {
            model: "sonnet".into(),
            effort: "low".into(),
            ..Default::default()
        },
    )
    .unwrap();

    // Unwired: a delivery is refused, so the hub keeps the turn.
    let bare =
        crate::speaker_sink::HouseSpeaker::new(Arc::new(AppExecutor::default()), dir.clone());
    assert!(bare.deliver(sturn("x", "y", "s-1")).await.is_err());
    let s = wired_speaker(&e);
    assert!(
        s.deliver(sturn("x", "y", "s-1")).await.is_err(),
        "no ops yet"
    );

    s.attach_ops(e.ops.clone());
    let wire = Arc::new(SpWire::default());
    s.attach_line(wire.clone());
    s.deliver(sturn("t1", "What time is it?", "s-1"))
        .await
        .unwrap();

    // One talk, one chat, named for the room it was spoken in; the words carry the channel note
    // and are stored with the speaker origin, never as typed.
    let talks = crate::speaker::load_talks(&paths.chats);
    assert_eq!(talks.talks.len(), 1);
    let chat = talks.talks[0].chat_id.clone();
    let title: String =
        e.db.snapshot(|tx| {
            tx.query_row("SELECT title FROM chats WHERE id = ?1", [&chat], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert!(title.starts_with("HA speaker kitchen voice "), "{title}");
    let made = e
        .ops
        .calls
        .lock()
        .unwrap()
        .iter()
        .rfind(|(o, _)| o == "chat_create")
        .unwrap()
        .1
        .clone();
    assert_eq!(made["model"], "sonnet", "the saved model opens the chat");
    assert_eq!(made["effort"], "low");
    let rows = subs(&e.db, &chat);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "speaker", "never the user's typed words");
    assert!(rows[0].4.starts_with("[Speaker · kitchen · "), "{rows:?}");

    // The answer goes up once the transcript has an end-of-turn row: one `voice_final` for the
    // turn, its words, in the mode that is spoken.
    wire.up.store(true, AtomicOrdering::SeqCst);
    sql(
        &e.db,
        "UPDATE chats SET status = 'idle', error = NULL WHERE id = ?1",
        &[&chat],
    );
    e.db
        .write(|tx| {
            tx.execute(
                "INSERT INTO messages(chat_id, source_key, role, text, status, timestamp, details)
                 VALUES (?1, 'm:a', 'assistant', 'It is twenty past seven.', 'complete', 2000, '{\"stopReason\":\"end_turn\"}')",
                rusqlite::params![&chat],
            )
        })
        .unwrap();
    let spoken = |turn: &str| {
        wire.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|f| {
                f["type"].as_str() == Some("voice_final") && f["turn"].as_str() == Some(turn)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(
        waits(|| !spoken("t1").is_empty()).await,
        "the answer reaches the speaker: {:?}",
        wire.sent.lock().unwrap().clone()
    );
    // A turn the hub sends again while its answer is on its way stores nothing new and answers
    // nothing twice.
    s.deliver(sturn("t1", "What time is it?", "s-1"))
        .await
        .unwrap();
    let settled = |n: usize| {
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
            spoken("t1").len() == n
        })
    };
    assert!(settled(1).await, "one answer per turn: {:?}", spoken("t1"));
    let final_frame = &spoken("t1")[0];
    assert_eq!(final_frame["text"], "It is twenty past seven.");
    assert_eq!(final_frame["mode"], "conversation");
    assert_eq!(final_frame["id"], crate::speaker::submission_id("t1"));

    // The next turn of the same talk goes to the same chat; another speaker's talk gets a chat of
    // its own, and its turn is answered there.
    s.deliver(sturn("t2", "And the weather?", "s-1"))
        .await
        .unwrap();
    s.deliver(sturn("t3", "Lights off.", "s-2")).await.unwrap();
    let talks = crate::speaker::load_talks(&paths.chats);
    assert_eq!(talks.talks.len(), 2);
    let other = talks
        .talks
        .iter()
        .find(|t| t.session == "s-2")
        .unwrap()
        .chat_id
        .clone();
    assert_ne!(other, chat);
    assert_eq!(subs(&e.db, &chat).len(), 2, "the resend stored nothing new");
    assert_eq!(subs(&e.db, &other).len(), 1);

    // The hub says the talk is over: the chat is put away, and what was last said there may be
    // named to the next talk on that speaker.
    s.talk_closed(None, "s-1".to_owned()).await;
    let talks = crate::speaker::load_talks(&paths.chats);
    assert_eq!(talks.talks.len(), 1);
    assert_eq!(
        talks.previous.as_ref().unwrap().gist,
        "It is twenty past seven."
    );
    assert!(e
        .ops
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|(o, a)| o == "chat_archive" && a["chatId"] == chat.as_str()));
    // A talk this PC never had changes nothing.
    s.talk_closed(None, "never-seen".to_owned()).await;
    assert_eq!(crate::speaker::load_talks(&paths.chats).talks.len(), 1);

    // Settings shows the open talk and what is saved; Start over ends them all.
    let view = crate::speaker_sink::speaker_view(&e.db, &dir, &e.ctx.profile_id);
    assert_eq!(view.talks.len(), 1);
    assert_eq!(view.model, "sonnet");
    // A blank picker choice (App default) stays a plain string, never null, and the next save
    // that hands it straight back is still accepted.
    crate::speaker::save_policy(
        &paths.policy,
        &crate::speaker::PolicyFile {
            model: "".into(),
            effort: "".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let blank = crate::speaker_sink::speaker_view(&e.db, &dir, &e.ctx.profile_id);
    assert_eq!(
        blank.model, "",
        "blank stays a string the form can send back"
    );
    assert_eq!(blank.effort, "");
    let mut back = crate::speaker::load_file(&paths.policy);
    back.take_speaker = Some(false);
    crate::speaker_sink::save(&dir, &e.ctx.profile_id, &back).unwrap();
    assert_eq!(
        crate::speaker::load_file(&paths.policy).take_speaker,
        Some(false),
        "saving after App default still works"
    );
    assert!(view.take_speaker, "the home profile takes it by default");
    assert_eq!(view.previous_hours, 12);
    assert_eq!(s.reset(&e.ctx.profile_id).await.unwrap(), 1);
    assert!(
        crate::speaker_sink::speaker_view(&e.db, &dir, &e.ctx.profile_id)
            .talks
            .is_empty()
    );
}

/// A chat left `interrupted` by a previous turn does not have that old stop spoken as the next
/// turn's answer: the mirror waits for the new turn's own rows, and speaks its answer when it
/// arrives.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_left_interrupted_speaks_the_new_turn_answer_not_the_old_stop() {
    let e = env();
    let dir = e.dir.path().join("prof");
    let s = wired_speaker(&e);
    s.attach_ops(e.ops.clone());
    let wire = Arc::new(SpWire::default());
    s.attach_line(wire.clone());
    wire.up.store(true, AtomicOrdering::SeqCst);
    // The first turn never ran: its failed worker leaves the chat stopped, and that stop stays.
    s.deliver(sturn("old", "old turn", "s-1")).await.unwrap();
    wait_quiet(&e.db, &crate::speaker::submission_id("old")).await;
    let paths = crate::speaker::paths(&dir, &e.ctx.profile_id);
    let chat = crate::speaker::load_talks(&paths.chats).talks[0]
        .chat_id
        .clone();
    // The failed start shows up on the chat itself: wait for that write rather than a fixed
    // pause, so a loaded test binary cannot read the chat mid-reaction.
    let stop_left = || {
        e.db.snapshot(|tx| {
            tx.query_row("SELECT error FROM chats WHERE id = ?1", [&chat], |r| {
                r.get::<_, Option<String>>(0)
            })
        })
        .unwrap_or(None)
    };
    assert!(
        waits(|| stop_left().as_deref() == Some("no worker in this test")).await,
        "the old turn left its stop on the chat: {:?}",
        stop_left()
    );
    // The next turn of the same talk goes to the same chat. Its worker runs and answers at once:
    // idle and the end-turn row, as a real run leaves them.
    s.deliver(sturn("t9", "what time is it now", "s-1"))
        .await
        .unwrap();
    sql(
        &e.db,
        "UPDATE chats SET status = 'idle', error = NULL WHERE id = ?1",
        &[&chat],
    );
    e.db
        .write(|tx| {
            tx.execute(
                "INSERT INTO messages(chat_id, source_key, role, text, status, timestamp, details)
                 VALUES (?1, 'm:a2', 'assistant', 'It is twenty past nine.', 'complete', 3000, '{\"stopReason\":\"end_turn\"}')",
                rusqlite::params![&chat],
            )
        })
        .unwrap();
    let spoken = || {
        wire.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|f| {
                f["type"].as_str() == Some("voice_final") && f["turn"].as_str() == Some("t9")
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(
        waits(|| !spoken().is_empty()).await,
        "the answer reaches the speaker: {:?}",
        wire.sent.lock().unwrap().clone()
    );
    // What is spoken is this turn's answer — never the stop the old turn left behind.
    assert!(
        waits(|| spoken().len() == 1).await,
        "the answer goes up once: {:?}",
        wire.sent.lock().unwrap().clone()
    );
    assert_eq!(spoken()[0]["text"], "It is twenty past nine.");
    // The old turn's own mirror may say its own stop; nothing spoken for t9 ever carries it.
    assert!(
        wire.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["turn"].as_str() == Some("t9"))
            .all(|f| !f.to_string().contains("no worker")),
        "the old stop is never spoken for this turn: {:?}",
        wire.sent.lock().unwrap().clone()
    );
}

#[test]
fn what_a_speaker_turn_settles_on_is_said_in_words() {
    // A stop with no answer in the transcript is said in its own words; an error row never is.
    let s = Snap {
        stopped: Some(Stop::Error("The model server stopped.".into())),
        ..Default::default()
    };
    assert_eq!(
        crate::speaker_sink::answer_of(&s),
        Stop::Error("The model server stopped.".into()).last_word()
    );
    // A stop sitting next to an answer already written to the transcript loses: the answer goes
    // up first, as the phone mirror sends its unsent answers before a stop.
    let s = Snap {
        stopped: Some(Stop::Error("The model server stopped.".into())),
        turn_done: Some((9, "The kitchen lights are off.".into())),
        final_text: Some("The kitchen lights are off.".into()),
        ..Default::default()
    };
    assert_eq!(
        crate::speaker_sink::answer_of(&s),
        "The kitchen lights are off."
    );
    let s = Snap {
        turn_done: Some((9, "The kitchen lights are off.".into())),
        final_text: Some("The kitchen lights are off.".into()),
        ..Default::default()
    };
    assert_eq!(
        crate::speaker_sink::answer_of(&s),
        "The kitchen lights are off."
    );
    let s = Snap {
        final_text: Some("  ".into()),
        ..Default::default()
    };
    assert_eq!(
        crate::speaker_sink::answer_of(&s),
        crate::voice_sink::NOTHING_TO_SAY
    );
    let s = Snap::default();
    assert_eq!(
        crate::speaker_sink::answer_of(&s),
        crate::voice_sink::NOTHING_TO_SAY
    );
}

/// A recorded exchange is written into its talk's chat as a transcript, with no model run and no
/// answer sent up; the chat is made on the board the settings name; the same record twice stores
/// one; a handed-over turn lands in the same chat with what was said before it; and a talk that
/// left something open (a record's note, or an attention flag) is kept when it ends.
#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_exchange_is_written_without_a_model_and_open_talks_are_kept() {
    let e = env();
    let dir = e.dir.path().join("prof");
    let paths = crate::speaker::paths(&dir, &e.ctx.profile_id);
    crate::speaker::save_policy(
        &paths.policy,
        &crate::speaker::PolicyFile {
            board_id: Some("board-1".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let s = wired_speaker(&e);
    s.attach_ops(e.ops.clone());
    let wire = Arc::new(SpWire::default());
    s.attach_line(wire.clone());
    let transcript = |chat: &str| -> Vec<(String, String, Option<String>)> {
        e.db.snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT role, text, json_extract(details, '$.origin') FROM messages
                 WHERE chat_id = ?1 ORDER BY id",
            )?;
            let rows = st
                .query_map([chat], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .unwrap()
    };
    let chat_of = |session: &str| crate::speaker::find(&paths.chats, session).unwrap().chat_id;

    s.record(srec("r1", "s-1", &[])).await.unwrap();
    let chat = chat_of("s-1");
    let made = e
        .ops
        .calls
        .lock()
        .unwrap()
        .iter()
        .rfind(|(o, _)| o == "chat_create")
        .unwrap()
        .1
        .clone();
    assert_eq!(
        made["boardId"], "board-1",
        "the chosen board takes the chat"
    );
    // The question as a speaker message, a line on what was done, the spoken answer: no run, so
    // nothing is said up the line and no worker was asked.
    let rows = transcript(&chat);
    let roles: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(roles, ["user", "context", "assistant"], "{rows:?}");
    assert!(rows[0].1.starts_with("[Speaker · kitchen · "), "{rows:?}");
    assert!(rows[0].1.ends_with("Turn the lamp on."), "{rows:?}");
    assert_eq!(rows[0].2.as_deref(), Some("speaker"), "never as typed");
    assert_eq!(rows[1].1, "The speaker used: kitchen lamp.");
    assert_eq!(rows[2].1, "Done.");
    let stored = subs(&e.db, &chat);
    assert_eq!((stored.len(), stored[0].1.as_str()), (1, "speaker"));
    assert_eq!(stored[0].3, "complete");
    assert!(wire.sent.lock().unwrap().is_empty());

    // The hub sends it again (its acknowledgement was lost): nothing is written twice. A second
    // record of the same talk joins the same chat.
    s.record(srec("r1", "s-1", &[])).await.unwrap();
    assert_eq!(transcript(&chat).len(), 3);
    s.record(srec("r2", "s-1", &[])).await.unwrap();
    assert_eq!(transcript(&chat).len(), 6);
    assert_eq!(chat_of("s-1"), chat);

    // A turn the speaker handed over lands in that same chat, with what it said just before.
    let mut handed = sturn("t1", "And the bedroom?", "s-1");
    handed.context = Some("Turn the lamp on. Done.".into());
    handed.escalated = true;
    s.deliver(handed).await.unwrap();
    let said = subs(&e.db, &chat);
    assert_eq!(said.len(), 3, "{said:?}");
    assert!(
        said[2].4.contains(
            "[Said on the speaker just before: Turn the lamp on. Done.]\nAnd the bedroom?"
        ),
        "{:?}",
        said[2].4
    );

    // The talk ends with nothing open: its chat is put away.
    s.talk_closed(None, "s-1".to_owned()).await;
    let archived = |chat: &str| {
        e.ops
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(o, a)| o == "chat_archive" && a["chatId"] == chat)
    };
    assert!(archived(&chat));

    // A record that left something open keeps its chat, and says what is open.
    s.record(srec("r3", "s-2", &["Buy a new bulb"]))
        .await
        .unwrap();
    let kept = chat_of("s-2");
    assert!(transcript(&kept)[1]
        .1
        .ends_with("Left open: Buy a new bulb."));
    s.talk_closed(None, "s-2".to_owned()).await;
    assert!(!archived(&kept), "a talk with something left open stays");
    assert!(
        crate::speaker::find(&paths.chats, "s-2").is_none(),
        "the talk itself is over"
    );

    // So does a chat whose agent flagged something for the user.
    s.record(srec("r4", "s-3", &[])).await.unwrap();
    let flagged = chat_of("s-3");
    sql(
        &e.db,
        "INSERT INTO chat_flags(chat_id, question, reason, created_at) VALUES (?1, 'Which bulb?', 'asked', 1)",
        &[&flagged],
    );
    s.talk_closed(None, "s-3".to_owned()).await;
    assert!(!archived(&flagged), "an open flag keeps the chat");

    // The settings view tells Settings what is chosen.
    let view = crate::speaker_sink::speaker_view(&e.db, &dir, &e.ctx.profile_id);
    assert_eq!((view.board_id.as_str(), view.lean), ("board-1", false));
}
