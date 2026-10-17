//! The real staged worker, started the way the host starts it, driving the fake claude.

use helm_workers::launch::{claude_launch, ClaudeRuntime, Launch, LaunchSendBehavior, TestClaude};
use helm_workers::node::{resolve_node, NodeLookup};
use helm_workers::{spawn, stage, SpawnSpec, WorkerOutput, MAX_RECORD_BYTES};
use serde_json::json;
use std::path::Path;
use std::time::Duration;

fn repo() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[tokio::test]
async fn the_staged_worker_answers_get_state_through_the_fake_claude() {
    let node = resolve_node(&NodeLookup::from_env()).expect("node installed");
    let profile = tempfile::tempdir().unwrap();
    let main =
        stage::stage_worker(&stage::repo_worker_src(), &profile.path().join("runtime")).unwrap();
    let fake = repo().join("fakes/claude/fake-claude.ts");
    let fixture = repo().join("fakes/claude/fixtures/streaming.json");
    let test = TestClaude {
        command: node.path.to_string_lossy().into_owned(),
        args: vec![
            fake.to_string_lossy().into_owned(),
            fixture.to_string_lossy().into_owned(),
        ],
    };
    let cwd = tempfile::tempdir().unwrap();
    let launch = Launch {
        chat_id: "chat-1".into(),
        harness: "claude",
        cwd: cwd.path().to_string_lossy().into_owned(),
        claude: claude_launch(&ClaudeRuntime::default(), Some(&test), None, None, None),
        send_behavior: LaunchSendBehavior {
            long_commands: "redirect".into(),
            opt_in: false,
        },
        limits: None,
        host_tools: None,
        chat_tools_directory: None,
    };
    let launch_path = launch
        .write(&profile.path().join("workers").join("chat-1"))
        .unwrap();
    let env = helm_workers::env::worker_environment("chat-1", &profile.path().to_string_lossy());
    let (worker, mut rx) = spawn(SpawnSpec {
        node: node.path.clone(),
        script: main,
        launch: launch_path,
        cwd: cwd.path().to_path_buf(),
        env: env.into_pairs(),
        max_record_bytes: MAX_RECORD_BYTES,
    })
    .unwrap();
    assert!(worker.send(&json!({"type": "get_state", "id": "ready"})));
    let mut ready = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Ok(Some(out)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        match out {
            WorkerOutput::Record(r) if r.get("type") == Some(&json!("response")) => {
                ready = Some(r);
                break;
            }
            WorkerOutput::Record(_) => {}
            other => panic!("worker ended early: {other:?}"),
        }
    }
    let ready = ready.expect("get_state answered");
    assert_eq!(ready["id"], "ready");
    assert_eq!(ready["success"], true, "{ready:?}");
    assert!(worker.send(&json!({"type": "shutdown"})));
    let mut exited = false;
    while let Ok(Some(out)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        if let WorkerOutput::Exited { code, .. } = out {
            assert_eq!(code, Some(0));
            exited = true;
            break;
        }
    }
    assert!(exited, "shutdown ends the worker");
}
