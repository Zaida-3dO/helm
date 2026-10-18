use super::*;
use crate::submit::{host_send, HostSend, HostSendMode, HostSent};
use crate::types::SubmissionOrigin;
use helm_core::db::Database;

struct NoWorker;
impl crate::runtime::Spawner for NoWorker {
    fn spawn(
        &self,
        _req: &crate::runtime::SpawnRequest,
    ) -> Result<crate::runtime::Spawned, String> {
        Err("no worker in this test".into())
    }
}

/// A worker that does not start until released, so an opened turn holds its slot until then.
struct SlowWorker(Arc<std::sync::atomic::AtomicBool>);
impl crate::runtime::Spawner for SlowWorker {
    fn spawn(
        &self,
        _req: &crate::runtime::SpawnRequest,
    ) -> Result<crate::runtime::Spawned, String> {
        for _ in 0..1500 {
            if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err("no worker in this test".into())
    }
}

struct Env {
    dir: tempfile::TempDir,
    db: Arc<Database>,
    rt: ChatRuntime,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        Database::open(
            &dir.path().join("t.sqlite3"),
            &crate::schema::tests::registry(),
        )
        .unwrap(),
    );
    let rt = runtime(&db, &dir);
    Env { dir, db, rt }
}

fn runtime(db: &Arc<Database>, dir: &tempfile::TempDir) -> ChatRuntime {
    runtime_with(db, dir, Arc::new(NoWorker))
}

fn runtime_with(
    db: &Arc<Database>,
    dir: &tempfile::TempDir,
    spawner: Arc<dyn crate::runtime::Spawner>,
) -> ChatRuntime {
    ChatRuntime::new(
        db.clone(),
        Arc::new(crate::clock::Clock::new(0)),
        Arc::new(helm_ipc::NullSink),
        spawner,
        crate::RuntimeConfig {
            profile_dir: dir.path().join("prof"),
            limits: crate::runtime::Limits::default(),
        },
        tokio::runtime::Handle::current(),
    )
}

fn chat(db: &Database) -> String {
    db.write(|tx| crate::store::create_chat(tx, "C:/w", None, None, 1))
        .unwrap()
}

fn set_status(db: &Database, id: &str, status: &str) {
    db.write(|tx| tx.execute("UPDATE chats SET status = ?1 WHERE id = ?2", [status, id]))
        .unwrap();
}

/// `n` running chats.
fn fill(db: &Database, n: usize) -> Vec<String> {
    (0..n)
        .map(|_| {
            let c = chat(db);
            set_status(db, &c, "running");
            c
        })
        .collect()
}

fn send(rt: &ChatRuntime, to: &str, text: &str, gated: bool) -> HostSent {
    host_send(
        rt,
        HostSend {
            chat_id: to,
            submission_id: &uuid::Uuid::new_v4().to_string(),
            text,
            mode: HostSendMode::Queue,
            origin: if gated {
                SubmissionOrigin::Schedule
            } else {
                SubmissionOrigin::App
            },
            gated,
        },
    )
    .unwrap()
}

fn wait_rows(db: &Database) -> Vec<Wait> {
    db.snapshot(waits).unwrap()
}

fn state_of(db: &Database, sid: &str) -> String {
    db.snapshot(|tx| {
        tx.query_row("SELECT state FROM submissions WHERE id = ?1", [sid], |r| {
            r.get(0)
        })
    })
    .unwrap()
}

/// The background pump may take a pass first (a send or a settings save wakes it): wait for the
/// outcome rather than count one pass's readmits.
async fn settled(db: &Database, rt: &ChatRuntime, want: usize) -> Vec<Wait> {
    for _ in 0..40 {
        run_pass(rt).await;
        let w = wait_rows(db);
        if w.len() == want {
            return w;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    wait_rows(db)
}

async fn run_pass(rt: &ChatRuntime) -> usize {
    let rt = rt.clone();
    tokio::task::spawn_blocking(move || pass(&rt, 0))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gated_start_waits_only_when_the_slots_are_full() {
    let e = env();
    let x = chat(&e.db);
    fill(&e.db, DEFAULT_LOW_RUNNING as usize - 1);
    // A slot is free: it starts.
    let s = send(&e.rt, &x, "first", true);
    assert_eq!((s.delivered, s.waiting), (true, None));
    // Full: the next gated start of an idle chat waits, with its place.
    let y = chat(&e.db);
    set_status(&e.db, &x, "running");
    let s = send(&e.rt, &y, "scheduled", true);
    assert_eq!((s.delivered, s.queued, s.waiting), (false, false, Some(1)));
    let w = wait_rows(&e.db);
    assert_eq!(w.len(), 1);
    assert_eq!(state_of(&e.db, &w[0].submission_id), "saved");
    // Exempt sends never wait.
    let z = chat(&e.db);
    let s = send(&e.rt, &z, "an app note", false);
    assert_eq!((s.delivered, s.waiting), (true, None));
    // A running chat's message joins its turn, gated or not.
    set_status(&e.db, &x, "running");
    let s = send(&e.rt, &x, "into the turn", true);
    assert_eq!((s.queued, s.waiting), (true, None));
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_overtakes_and_replies_go_first() {
    let e = env();
    fill(&e.db, DEFAULT_LOW_RUNNING as usize);
    let (a, b, c) = (chat(&e.db), chat(&e.db), chat(&e.db));
    assert_eq!(send(&e.rt, &a, "one", true).waiting, Some(1));
    assert_eq!(send(&e.rt, &b, "two", true).waiting, Some(2));
    // Its own chat already waits: behind it, never ahead.
    assert_eq!(send(&e.rt, &a, "three", true).waiting, Some(3));
    // A genuine reply: c holds a message framed as from b, and now replies to b.
    let from_b = format!("[Message from another chat: \"B\" ({b})]\nhello");
    e.db.write(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES ('f', ?1, ?2, 'complete', 1)",
            [c.as_str(), from_b.as_str()],
        )
    })
    .unwrap();
    let reply = format!("[Message from another chat: \"C\" ({c})]\nanswer");
    assert_eq!(
        send(&e.rt, &b, &reply, true).waiting,
        Some(1),
        "replies first"
    );
    let order: Vec<(String, bool)> = wait_rows(&e.db)
        .into_iter()
        .map(|w| (w.chat_id, w.head))
        .collect();
    assert_eq!(
        order,
        vec![
            (b.clone(), true),
            (a.clone(), false),
            (b.clone(), false),
            (a, false)
        ]
    );
    // A framed message that is not a reply is not a head: it goes to the back.
    let stranger = format!(
        "[Message from another chat: \"A\" ({})]\nhi",
        uuid::Uuid::new_v4()
    );
    let d = chat(&e.db);
    assert_eq!(send(&e.rt, &d, &stranger, true).waiting, Some(5));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pump_fills_free_slots_in_order_and_sends_each_wait_once() {
    let mut e = env();
    // Turns that open hold their slot for a while (the worker is slow to start).
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    e.rt = runtime_with(&e.db, &e.dir, Arc::new(SlowWorker(release.clone())));
    let busy = fill(&e.db, DEFAULT_LOW_RUNNING as usize);
    let (a, b) = (chat(&e.db), chat(&e.db));
    send(&e.rt, &a, "a", true);
    send(&e.rt, &b, "b", true);
    assert_eq!(run_pass(&e.rt).await, 0, "no slot free");
    set_status(&e.db, &busy[0], "idle");
    let w = settled(&e.db, &e.rt, 1).await;
    assert_eq!(w.len(), 1, "one slot, one readmit");
    assert_eq!(w[0].chat_id, b, "a went first");
    // a's opened turn holds the slot: b keeps waiting, and a is never sent twice.
    assert_eq!(run_pass(&e.rt).await, 0);
    assert_eq!(wait_rows(&e.db).len(), 1);
    release.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_whose_chat_starts_running_joins_its_turn() {
    let e = env();
    fill(&e.db, DEFAULT_LOW_RUNNING as usize);
    let a = chat(&e.db);
    send(&e.rt, &a, "a", true);
    let sid = wait_rows(&e.db)[0].submission_id.clone();
    set_status(&e.db, &a, "running");
    assert!(settled(&e.db, &e.rt, 0).await.is_empty());
    assert_eq!(
        state_of(&e.db, &sid),
        "queued",
        "a follow-up of the running turn"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hold_stops_new_starts_and_start_now_still_works() {
    let e = env();
    let held = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let h = held.clone();
    e.rt.set_slot_hold(Arc::new(move |_| {
        h.load(std::sync::atomic::Ordering::SeqCst)
            .then(|| "usage".to_owned())
    }));
    let a = chat(&e.db);
    // Every slot is free, but the governor says nothing new starts.
    assert_eq!(send(&e.rt, &a, "scheduled", true).waiting, Some(1));
    // Exempt sends still go.
    let b = chat(&e.db);
    assert_eq!(send(&e.rt, &b, "note", false).waiting, None);
    assert_eq!(run_pass(&e.rt).await, 0, "held");
    let sid = wait_rows(&e.db)[0].submission_id.clone();
    let rt = e.rt.clone();
    let (a2, s2) = (a.clone(), sid.clone());
    tokio::task::spawn_blocking(move || start_now(&rt, &a2, &s2))
        .await
        .unwrap()
        .unwrap();
    assert!(wait_rows(&e.db).is_empty(), "Start now goes past the hold");
    let rt = e.rt.clone();
    let again = tokio::task::spawn_blocking(move || start_now(&rt, &a, &sid))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(again.code, "not_found");
    // Hold lifted: new starts go again.
    held.store(false, std::sync::atomic::Ordering::SeqCst);
    let c = chat(&e.db);
    assert_eq!(send(&e.rt, &c, "x", true).waiting, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_leaves_the_message_not_sent_and_waits_survive_a_restart() {
    let e = env();
    fill(&e.db, DEFAULT_LOW_RUNNING as usize);
    let (a, b) = (chat(&e.db), chat(&e.db));
    send(&e.rt, &a, "a", true);
    send(&e.rt, &b, "b", true);
    let sa = wait_rows(&e.db)[0].submission_id.clone();
    cancel(&e.rt, &sa).unwrap();
    assert_eq!(
        state_of(&e.db, &sa),
        "rejected",
        "reads Not sent, recoverable"
    );
    assert_eq!(cancel(&e.rt, &sa).unwrap_err().code, "not_found");
    // A new runtime over the same database (a restart) still has b's wait, and admits it.
    let rt2 = runtime(&e.db, &e.dir);
    assert_eq!(wait_rows(&e.db).len(), 1);
    e.db.write(|tx| {
        tx.execute(
            "UPDATE chats SET status = 'idle' WHERE status = 'running'",
            [],
        )
    })
    .unwrap();
    assert!(settled(&e.db, &rt2, 0).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_are_strict_on_write_and_off_releases_every_wait() {
    let e = env();
    let cur = RunSlotSettings::default();
    assert!(parse_strict(&serde_json::json!({"lowRunningChats": 1}), cur).is_err());
    assert!(parse_strict(&serde_json::json!({"lowRunningChats": 21}), cur).is_err());
    assert!(parse_strict(&serde_json::json!({"loadGate": true}), cur).is_err());
    assert_eq!(
        parse_strict(&serde_json::json!({"lowRunningChats": 5}), cur)
            .unwrap()
            .low_running_chats,
        5
    );
    fill(&e.db, 2);
    let rt = e.rt.clone();
    tokio::task::spawn_blocking(move || {
        save_settings(&rt, &serde_json::json!({"lowRunningChats": 2})).unwrap()
    })
    .await
    .unwrap();
    let (a, b) = (chat(&e.db), chat(&e.db));
    assert_eq!(
        send(&e.rt, &a, "a", true).waiting,
        Some(1),
        "2 running fill 2 slots"
    );
    send(&e.rt, &b, "b", true);
    let rt = e.rt.clone();
    tokio::task::spawn_blocking(move || {
        save_settings(&rt, &serde_json::json!({"enabled": false})).unwrap()
    })
    .await
    .unwrap();
    assert!(
        settled(&e.db, &e.rt, 0).await.is_empty(),
        "switched off: every wait goes, whatever the slots"
    );
}
