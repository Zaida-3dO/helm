//! One chat end to end through the real host runtime, the real staged worker and the fake claude
//! (the integration tests drive the real worker through the test seam).
//!
//! `HELM_TEST_CLAUDE` is process-wide, which is why this lives in its own test binary.

use helm_chats::clock::Clock;
use helm_chats::{ChatRuntime, ProcessSpawner, RuntimeConfig};
use helm_core::db::{Database, MigrationRegistry};
use helm_core::profile::Profile;
use helm_ipc::{Caller, Dispatcher, OpCtx, OpRegistry, RecordingSink, Services};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn repo() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_round_trips_through_the_real_worker() {
    let node = helm_workers::node::resolve_node(&helm_workers::node::NodeLookup::from_env())
        .expect("node installed");
    let fake = repo().join("fakes/claude/fake-claude.ts");
    let fixture = repo().join("fakes/claude/fixtures/streaming.json");
    std::env::set_var(
        "HELM_TEST_CLAUDE",
        json!({"command": node.path, "args": [fake, fixture]}).to_string(),
    );
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::open(&dir.path().join("prof")).unwrap();
    let mut reg = MigrationRegistry::new();
    helm_chats::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&profile.db_path(), &reg).unwrap());
    let clock = Arc::new(Clock::new(0));
    let sink = Arc::new(RecordingSink::default());
    let spawner = Arc::new(ProcessSpawner::new(
        profile.path(""),
        profile.display_dir(),
        true,
        Some(helm_workers::stage::repo_worker_src()),
    ));
    let rt = ChatRuntime::new(
        db.clone(),
        clock.clone(),
        sink.clone(),
        spawner,
        RuntimeConfig {
            profile_dir: profile.path(""),
            limits: Default::default(),
        },
        tokio::runtime::Handle::current(),
    );
    let mut s = Services::new();
    s.insert(Arc::new(profile.clone()));
    s.insert(db.clone());
    s.insert(clock);
    s.insert(Arc::new(rt.clone()));
    let mut ops = OpRegistry::new();
    helm_chats::register_ops(&mut ops);
    let api = Dispatcher::new(
        ops,
        OpCtx {
            services: Arc::new(s),
            events: sink.clone(),
        },
        false,
    );
    let main = Caller {
        window_label: "main".into(),
        url: "http://tauri.localhost/".into(),
    };
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let chat = api
        .call(&main, "chat_create", json!({"cwd": work.to_string_lossy()}))
        .await
        .unwrap();
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let sid = uuid::Uuid::new_v4().to_string();
    api.call(
        &main,
        "chat_send",
        json!({"chatId": chat_id, "submissionId": sid, "text": "hello", "action": "default", "draftRevision": 0}),
    )
    .await
    .unwrap();
    let mut settled = false;
    for _ in 0..300 {
        let got = api
            .call(&main, "chat_get", json!({"chatId": chat_id}))
            .await
            .unwrap();
        if got["chat"]["status"] == "idle"
            && sink.kinds().iter().filter(|k| *k == "message").count() > 1
        {
            settled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let page = api
        .call(&main, "transcript_page", json!({"chatId": chat_id}))
        .await
        .unwrap();
    assert!(settled, "the chat settled: {page}");
    let rows = page["messages"].as_array().unwrap();
    assert_eq!(rows[0]["role"], "user");
    assert_eq!(rows[0]["status"], "complete");
    let assistant = rows
        .iter()
        .find(|r| r["role"] == "assistant")
        .expect("an assistant row");
    assert_eq!(assistant["status"], "complete");
    assert!(!assistant["text"].as_str().unwrap().is_empty());
    let state: String = db
        .snapshot(|t| t.query_row("SELECT state FROM submissions", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(state, "complete");
    rt.drain().await;
}
