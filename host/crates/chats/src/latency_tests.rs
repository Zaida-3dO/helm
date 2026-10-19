//! The send timeline, pre-warm, spawn off the actor, hot-path writes and deferred
//! titles, against the in-process fake worker of [`crate::runtime_tests`].

use crate::runtime::Limits;
use crate::runtime_tests::*;
use serde_json::json;
use std::time::Duration;

fn timeline_file(h: &H) -> String {
    std::fs::read_to_string(h.profile.path("diagnostics/send-timeline.log")).unwrap_or_default()
}

/// One line per turn-opening send, written at its first output, with the worker's marks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cold_send_logs_its_timeline_at_the_first_output() {
    let mut h = harness();
    let chat = h.create().await;
    let (sid, _) = h.send(&chat, "hello", "default").await;
    let mut w = h.worker().await;
    let gs = w.expect("get_state").await;
    let now = helm_core::now_ms();
    w.emit(json!({"type": "response", "id": gs["id"], "command": "get_state", "success": true,
                  "data": {"capabilities": {}}, "timing": {"workerStart": now, "claudeSpawned": now + 1}}))
        .await;
    let p = w.expect("prompt").await;
    w.emit(json!({"type": "response", "id": p["id"], "command": "prompt", "success": true,
                  "data": {"submissionId": sid, "state": "accepted"}, "timing": {"received": now + 2, "written": now + 3}}))
        .await;
    w.emit(json!({"type": "agent_start"})).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(timeline_file(&h), "", "nothing before the first output");
    w.emit(json!({"type": "message_start", "message": {"id": "m1", "role": "assistant", "content": []}}))
        .await;
    w.emit(json!({"type": "message_update", "messageId": "m1", "delta": {"type": "text_delta", "text": "Hi"}}))
        .await;
    h.until("the timeline line", |h| !timeline_file(h).is_empty())
        .await;
    let line = timeline_file(&h);
    let short: String = sid.chars().take(8).collect();
    assert!(
        line.contains(&format!("chat {chat}: send {short} timeline")),
        "{line}"
    );
    for part in [
        "dispatched +",
        "spawn +",
        "worker +",
        "claude +",
        "ready +",
        "written +",
        "acked +",
        "first output +",
        "; cold",
    ] {
        assert!(line.contains(part), "{part:?} in {line}");
    }
    assert_eq!(line.lines().count(), 1);
    // The turn ends: no second line.
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("chat idle", |h| h.status(&chat) == "idle").await;
    assert_eq!(timeline_file(&h).lines().count(), 1);
}

/// A send whose turn ends with no output still gets its line, marked as such; a warm worker says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_without_output_is_logged_at_its_end_and_warm_sends_say_so() {
    let mut h = harness();
    let chat = h.create().await;
    let (sid, _) = h.send(&chat, "first", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    w.respond(&p, json!({"submissionId": sid, "state": "accepted"}))
        .await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("chat idle", |h| h.status(&chat) == "idle").await;
    h.until("the first line", |h| timeline_file(h).lines().count() == 1)
        .await;
    assert!(
        timeline_file(&h).contains("no output; cold"),
        "{}",
        timeline_file(&h)
    );
    let (sid2, _) = h.send(&chat, "second", "default").await;
    let p = w.expect("prompt").await;
    w.respond(&p, json!({"submissionId": sid2, "state": "accepted"}))
        .await;
    w.emit(
        json!({"type": "tool_execution_start", "toolCallId": "t1", "toolName": "Bash", "args": {}}),
    )
    .await;
    h.until("the second line", |h| timeline_file(h).lines().count() == 2)
        .await;
    let second = timeline_file(&h).lines().nth(1).unwrap().to_owned();
    assert!(second.ends_with("; warm"), "{second}");
    assert!(!second.contains("spawn"), "{second}");
}

async fn save_draft(h: &H, chat: &str, text: &str) {
    let rev = helm_core::now_ms();
    h.call(
        "draft_save",
        json!({"chatId": chat, "text": text, "revision": rev}),
    )
    .await
    .unwrap();
}

/// Typing in an idle chat starts its worker with `get_state` only; the send then goes straight out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typing_prewarms_the_worker_and_the_send_reuses_it() {
    let mut h = harness();
    let chat = h.create().await;
    save_draft(&h, &chat, "   ").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), h.workers.recv())
            .await
            .is_err(),
        "a blank draft starts nothing"
    );
    save_draft(&h, &chat, "hel").await;
    let mut w = h.worker().await;
    w.ready().await;
    assert!(
        w.no_command_for(Duration::from_millis(100)).await.is_none(),
        "pre-warm sends no prompt"
    );
    assert_eq!(h.status(&chat), "idle", "a warm worker is not a turn");
    save_draft(&h, &chat, "hello").await;
    let (sid, _) = h.send(&chat, "hello", "default").await;
    let p = w.expect("prompt").await;
    assert_eq!(p["submissionId"], sid.as_str());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), h.workers.recv())
            .await
            .is_err(),
        "one worker in all"
    );
    w.respond(&p, json!({"submissionId": sid, "state": "accepted"}))
        .await;
    w.emit(
        json!({"type": "tool_execution_start", "toolCallId": "t1", "toolName": "Bash", "args": {}}),
    )
    .await;
    h.until("the timeline line", |h| !timeline_file(h).is_empty())
        .await;
    assert!(
        timeline_file(&h).trim_end().ends_with("; warm"),
        "{}",
        timeline_file(&h)
    );
}

/// No pre-warm for archived or side chats, chats with a turn running, or at the idle-worker cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prewarm_skips_archived_side_and_busy_chats_and_respects_the_cap() {
    let mut h = harness();
    let archived = h.create().await;
    let side = h.create().await;
    let busy = h.create().await;
    h.db.write(|tx| {
        tx.execute("UPDATE chats SET archived = 1 WHERE id = ?1", [&archived])?;
        tx.execute(
            "UPDATE chats SET side_source_id = ?1 WHERE id = ?2",
            [&busy, &side],
        )?;
        tx.execute("UPDATE chats SET status = 'running' WHERE id = ?1", [&busy])
    })
    .unwrap();
    for c in [&archived, &side, &busy] {
        h.rt.prewarm(c);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(200), h.workers.recv())
            .await
            .is_err(),
        "none of them starts a worker"
    );
    // At the cap (0 idle workers allowed here) nothing is started either.
    let mut capped = harness_with(Limits {
        max_idle_workers: 0,
        ..limits()
    });
    let chat = capped.create().await;
    capped.rt.prewarm(&chat);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), capped.workers.recv())
            .await
            .is_err(),
        "the cap is respected"
    );
    // An eligible chat does start one, once.
    let ok = h.create().await;
    h.rt.prewarm(&ok);
    h.rt.prewarm(&ok);
    let w = h.worker().await;
    assert_eq!(w.req.chat_id, ok);
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.rt.prewarm(&ok);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), h.workers.recv())
            .await
            .is_err(),
        "a chat with a worker is not started twice"
    );
}

/// A spawner whose start blocks until the test lets it go, then fails.
struct GatedSpawner {
    gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    calls: std::sync::atomic::AtomicUsize,
}

impl GatedSpawner {
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl crate::runtime::Spawner for GatedSpawner {
    fn spawn(
        &self,
        _req: &crate::runtime::SpawnRequest,
    ) -> Result<crate::runtime::Spawned, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = self.gate.lock().unwrap().recv();
        Err("let go".into())
    }
}

/// The worker start runs off the actor: while it blocks, the chat still answers its commands. A
/// start that failed is not retried by typing for a while.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_worker_start_does_not_block_the_chat() {
    let dir = tempfile::tempdir().unwrap();
    let profile = helm_core::profile::Profile::open(&dir.path().join("prof")).unwrap();
    let db = std::sync::Arc::new(
        helm_core::db::Database::open(&profile.db_path(), &crate::schema::tests::registry())
            .unwrap(),
    );
    let (release, gate) = std::sync::mpsc::channel();
    let spawner = std::sync::Arc::new(GatedSpawner {
        gate: std::sync::Mutex::new(gate),
        calls: Default::default(),
    });
    let rt = crate::runtime::ChatRuntime::new(
        db.clone(),
        std::sync::Arc::new(crate::clock::Clock::new(0)),
        std::sync::Arc::new(helm_ipc::RecordingSink::default()),
        spawner.clone(),
        crate::runtime::RuntimeConfig {
            profile_dir: profile.path(""),
            limits: limits(),
        },
        tokio::runtime::Handle::current(),
    );
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let chat = db
        .write(|tx| {
            crate::store::create_chat(tx, &work.to_string_lossy(), None, None, helm_core::now_ms())
        })
        .unwrap();
    let has_worker = |rt: &crate::runtime::ChatRuntime| {
        rt.sh.actors.lock().unwrap()[&chat]
            .live
            .lock()
            .unwrap()
            .has_worker
    };
    rt.prewarm(&chat);
    for _ in 0..200 {
        if spawner.calls() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(spawner.calls(), 1);
    // The start is still blocked; the actor answers anyway.
    let answered = tokio::time::timeout(
        Duration::from_secs(2),
        rt.ask(
            &chat,
            |reply| crate::actor::Cmd::RefreshCatalogue { reply },
            Duration::from_secs(2),
        ),
    )
    .await;
    assert!(matches!(answered, Ok(Ok(false))), "{answered:?}");
    release.send(()).unwrap();
    for _ in 0..200 {
        if !has_worker(&rt) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!has_worker(&rt), "the failed start was taken back");
    rt.prewarm(&chat);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        spawner.calls(),
        1,
        "typing right after a failed start does not start another"
    );
}

fn sent_texts(h: &H) -> Vec<String> {
    h.events("message")
        .iter()
        .flat_map(|e| e["rows"].as_array().cloned().unwrap_or_default())
        .filter(|r| r["role"] == "assistant")
        .map(|r| {
            format!(
                "{}:{}",
                r["status"].as_str().unwrap_or(""),
                r["text"].as_str().unwrap_or("")
            )
        })
        .collect()
}

fn stored(h: &H) -> Vec<String> {
    h.col("SELECT status || ':' || text FROM messages WHERE role = 'assistant' ORDER BY id")
}

async fn running(h: &mut H) -> FakeWorker {
    let chat = h.create().await;
    let (sid, _) = h.send(&chat, "hello", "default").await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    w.respond(&p, json!({"submissionId": sid, "state": "accepted"}))
        .await;
    w.emit(json!({"type": "agent_start"})).await;
    w
}

fn delta(mid: &str, text: &str) -> serde_json::Value {
    json!({"type": "message_update", "messageId": mid, "delta": {"type": "text_delta", "text": text}})
}

/// The UI gets every tick's text; the database gets it at most every `stream_db`, catches up
/// when the stream goes quiet, and has the whole message at `message_end`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_rows_reach_the_database_at_most_every_interval() {
    let mut h = harness_with(Limits {
        flush: Duration::from_millis(20),
        stream_db: Duration::from_millis(700),
        ..limits()
    });
    let w = running(&mut h).await;
    w.emit(json!({"type": "message_start", "message": {"id": "m1", "role": "assistant", "content": []}}))
        .await;
    w.emit(delta("m1", "Hel")).await;
    w.emit(delta("m1", "lo")).await;
    h.until("the UI has the text", |h| {
        sent_texts(h).contains(&"streaming:Hello".to_owned())
    })
    .await;
    assert_eq!(
        stored(&h),
        ["streaming:"],
        "the database is not written per tick"
    );
    h.until("the database catches up once the stream is quiet", |h| {
        stored(h) == ["streaming:Hello"]
    })
    .await;
    w.emit(json!({"type": "message_end", "message": {"id": "m1", "role": "assistant", "content": [{"type": "text", "text": "Hello!"}], "status": "complete"}}))
        .await;
    h.until("the message is complete", |h| {
        stored(h) == ["complete:Hello!"]
    })
    .await;
}

/// A message's first delta is sent at once, even right after a tick has gone by.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_messages_first_words_go_out_without_waiting_for_the_tick() {
    let mut h = harness_with(Limits {
        flush: Duration::from_secs(10),
        ..limits()
    });
    let w = running(&mut h).await;
    // A first message uses up the tick.
    w.emit(json!({"type": "message_start", "message": {"id": "m0", "role": "assistant", "content": []}}))
        .await;
    w.emit(delta("m0", "x")).await;
    h.until("m0 sent", |h| {
        sent_texts(h).contains(&"streaming:x".to_owned())
    })
    .await;
    w.emit(json!({"type": "message_end", "message": {"id": "m0", "role": "assistant", "content": [{"type": "text", "text": "x"}], "status": "complete"}}))
        .await;
    w.emit(json!({"type": "message_start", "message": {"id": "m1", "role": "assistant", "content": []}}))
        .await;
    w.emit(delta("m1", "First")).await;
    h.until("the first words, long before the next tick", |h| {
        sent_texts(h).contains(&"streaming:First".to_owned())
    })
    .await;
}

fn title_state(h: &H, chat: &str) -> (String, String) {
    let t = h
        .col(&format!("SELECT title FROM chats WHERE id = '{chat}'"))
        .pop()
        .unwrap();
    let s = h
        .col(&format!(
            "SELECT state || '/' || COALESCE(job, '') FROM chat_titles WHERE chat_id = '{chat}'"
        ))
        .pop()
        .unwrap_or_default();
    (t, s)
}

/// A title job waits for the first turn to settle, so its claude never competes with the chat's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_title_job_waits_for_the_first_turn_to_settle() {
    let mut h = harness_with(Limits {
        title_wait: Duration::from_secs(5),
        ..limits()
    });
    *h.spawner.title.lock().unwrap() = Ok("Fix the login redirect".into());
    let chat = h.create().await;
    let (sid, _) = h
        .send(
            &chat,
            "the login page redirects twice, please fix",
            "default",
        )
        .await;
    let mut w = h.worker().await;
    w.ready().await;
    let p = w.expect("prompt").await;
    w.respond(&p, json!({"submissionId": sid, "state": "accepted"}))
        .await;
    w.emit(json!({"type": "agent_start"})).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        h.spawner.title_jobs.lock().unwrap().is_empty(),
        "no title job while the first turn runs"
    );
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("the generated title", |h| {
        title_state(h, &chat) == ("Fix the login redirect".into(), "generated/".into())
    })
    .await;
}

/// "Hello" is never titled; the next, substantive message is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_greeting_is_not_titled_and_the_next_message_is() {
    let h = harness();
    *h.spawner.title.lock().unwrap() = Ok("Refactor the config parser".into());
    let chat = h.create().await;
    h.send(&chat, "Hello!", "default").await;
    h.until("the greeting is parked", |h| {
        title_state(h, &chat) == ("Hello!".into(), "fallback/greeting".into())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(h.spawner.title_jobs.lock().unwrap().is_empty());
    h.send(&chat, "please refactor the config parser", "queue")
        .await;
    h.until("the generated title", |h| {
        title_state(h, &chat) == ("Refactor the config parser".into(), "generated/".into())
    })
    .await;
    let jobs = h.spawner.title_jobs.lock().unwrap().clone();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["firstRequest"], "please refactor the config parser");
}

#[test]
fn greetings_and_check_ins_are_told_from_requests() {
    use crate::titles::is_greeting;
    for g in [
        "Hello",
        "hi there!",
        "Hey Claude",
        "good morning",
        "thanks!",
        "ok",
        "  ",
        "👋",
        "how's it going?",
    ] {
        assert!(is_greeting(g), "{g:?} is a greeting");
    }
    for r in [
        "Hello, fix the build",
        "hi can you review PR 12",
        "refactor the parser",
        "test the login flow please",
    ] {
        assert!(!is_greeting(r), "{r:?} is a request");
    }
}

/// Starts a background Agent call `id` on a running turn (its call returns at once).
async fn launch_agent(w: &FakeWorker, id: &str) {
    w.emit(json!({"type": "tool_execution_start", "toolCallId": id, "toolName": "Agent", "args": {"description": "Echo"}, "stoppable": false}))
        .await;
    w.emit(json!({"type": "subagent_start", "toolCallId": id, "agentType": "reviewer", "name": "Echo", "description": "Echo", "assignment": "Run echo.", "startedAt": 1}))
        .await;
    w.emit(json!({"type": "tool_execution_end", "toolCallId": id, "toolName": "Agent", "result": {"text": "Async agent launched successfully."}, "isError": false}))
        .await;
}

fn the_chat(h: &H) -> String {
    h.col("SELECT id FROM chats").pop().unwrap()
}

/// A settled chat whose background agents still run is not idle. The idle-worker cap
/// leaves its worker alone and quit lists it; once the last agent ends, the cap applies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_agents_keep_their_worker_from_the_idle_cap_and_show_at_quit() {
    let mut h = harness_with(Limits {
        max_idle_workers: 0,
        ..limits()
    });
    let mut w = running(&mut h).await;
    let chat = the_chat(&h);
    launch_agent(&w, "sa_1").await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("chat idle", |h| h.status(&chat) == "idle").await;
    assert_eq!(
        w.no_command_for(Duration::from_millis(300)).await,
        None,
        "not retired while its agent runs"
    );
    assert_eq!(h.rt.running_titles().len(), 1, "quit warns about the agent");
    w.emit(json!({"type": "subagent_end", "toolCallId": "sa_1", "status": "completed", "report": "done", "durationMs": 10}))
        .await;
    w.expect("shutdown").await;
    assert!(h.rt.running_titles().is_empty());
}

/// A send-behaviour relaunch waits for running agents, then happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relaunch_waits_for_running_agents() {
    let mut h = harness();
    let mut w = running(&mut h).await;
    let chat = the_chat(&h);
    launch_agent(&w, "sa_1").await;
    w.emit(json!({"type": "agent_settled", "outcome": "complete"}))
        .await;
    h.until("chat idle", |h| h.status(&chat) == "idle").await;
    let sb = crate::types::SendBehavior {
        long_commands: crate::types::LongCommands::Off,
        ..crate::send_behavior::HOME_DEFAULTS
    };
    assert_ne!(sb, crate::send_behavior::HOME_DEFAULTS);
    let _ = crate::runtime::enqueue(&h.rt.actor(&chat).tx, crate::actor::Cmd::SendBehavior(sb));
    assert_eq!(
        w.no_command_for(Duration::from_millis(300)).await,
        None,
        "no relaunch while the agent runs"
    );
    w.emit(json!({"type": "subagent_end", "toolCallId": "sa_1", "status": "completed", "report": "done", "durationMs": 10}))
        .await;
    w.expect("shutdown").await;
}
