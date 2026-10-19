//! The send timeline, pre-warm, spawn off the actor, hot-path writes and deferred
//! titles, against the in-process fake worker of [`crate::runtime_tests`].

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
