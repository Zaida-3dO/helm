//! Store tests: caps, compare-and-set decide, single-use spend, the words episode, upkeep, the
//! hint and the ops.

use crate::kinds::{test_kind, AllowKind, AllowKinds};
use crate::store::{self, NewRequest, HOUR, MINUTE, PENDING_LIFE};
use crate::types::{Decision, RequestState};
use helm_core::db::{Database, MigrationRegistry};
use helm_ipc::{OpCtx, RecordingSink, Services};
use rusqlite::{params, Transaction};
use serde_json::{json, Value};
use std::sync::Arc;

const T0: i64 = 1_800_000_000_000;

fn meta(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    // The chats crate's origin table, as the words verifier reads it.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS submission_meta (
             submission_id TEXT PRIMARY KEY REFERENCES submissions(id) ON DELETE CASCADE,
             origin TEXT NOT NULL DEFAULT 'user', priority TEXT, run_id TEXT);",
    )
}

struct Env {
    _dir: tempfile::TempDir,
    db: Arc<Database>,
    kinds: AllowKinds,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    reg.initializer("meta", meta);
    crate::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
    db.write::<_, rusqlite::Error>(|tx| {
        tx.execute(
            "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
            [],
        )?;
        for i in 0..20 {
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 1, 1)",
                [format!("c{i}")],
            )?;
        }
        Ok(())
    })
    .unwrap();
    let mut kinds = AllowKinds::new();
    kinds.register(test_kind());
    kinds.register(AllowKind {
        id: "click_only",
        accepts_words: false,
        ..test_kind()
    });
    Env {
        _dir: dir,
        db,
        kinds,
    }
}

fn ask<'a>(chat: &'a str, target: &'a Value) -> NewRequest<'a> {
    NewRequest {
        chat_id: chat,
        kind: "test_action",
        target,
        reason: "Because the user asked.",
        evidence: "",
        grant_minutes: None,
        tool_use_id: None,
    }
}

fn t(name: &str) -> Value {
    json!({ "name": name })
}

fn spend(e: &Env, chat: &str, target: &Value, now: i64) -> Result<store::ConsentRecord, String> {
    e.db.write::<_, store::Refusal>(|tx| {
        store::spend_grant(tx, &e.kinds, chat, "test_action", target, "test", now)
    })
    .map_err(|r| r.0)
}

#[test]
fn a_grant_is_minted_only_by_decide_and_spent_once() {
    let e = env();
    let a = t("alpha");
    let r = store::request(&e.db, &e.kinds, ask("c0", &a), T0).unwrap();
    assert_eq!(r.state, RequestState::Pending);
    assert_eq!(r.summary, "Run the test action alpha");
    assert_eq!(r.expires_at, T0 + PENDING_LIFE);
    // Pending is not consent.
    assert!(spend(&e, "c0", &a, T0 + 1)
        .unwrap_err()
        .contains("no unused Allow"));
    store::decide(&e.db, &e.kinds, &r.id, Decision::Allow, None, None, T0 + 2).unwrap();
    // Another chat cannot use it.
    assert!(spend(&e, "c1", &a, T0 + 3).is_err());
    let rec = spend(&e, "c0", &a, T0 + 3).unwrap();
    assert_eq!(rec.proof, "grant");
    assert_eq!(rec.request_id.as_deref(), Some(r.id.as_str()));
    // Single use.
    assert!(spend(&e, "c0", &a, T0 + 4).is_err());
    let l = store::list(&e.db, &e.kinds, "c0", T0 + 5).unwrap();
    assert_eq!(l.decided[0].state, RequestState::Used);
}

#[test]
fn decide_is_compare_and_set_out_of_pending() {
    let e = env();
    let a = t("alpha");
    let r = store::request(&e.db, &e.kinds, ask("c0", &a), T0).unwrap();
    store::decide(
        &e.db,
        &e.kinds,
        &r.id,
        Decision::Deny,
        None,
        Some("not now"),
        T0 + 1,
    )
    .unwrap();
    let again = store::decide(&e.db, &e.kinds, &r.id, Decision::Allow, None, None, T0 + 2);
    assert_eq!(again.unwrap_err().code, "conflict");
    assert!(
        spend(&e, "c0", &a, T0 + 3).is_err(),
        "a denied request grants nothing"
    );
    // An expired pending request cannot be decided either.
    let b = t("beta");
    let r2 = store::request(&e.db, &e.kinds, ask("c0", &b), T0).unwrap();
    let late = store::decide(
        &e.db,
        &e.kinds,
        &r2.id,
        Decision::Allow,
        None,
        None,
        T0 + PENDING_LIFE,
    );
    assert_eq!(late.unwrap_err().code, "conflict");
    let missing = store::decide(&e.db, &e.kinds, "nope", Decision::Allow, None, None, T0);
    assert_eq!(missing.unwrap_err().code, "not_found");
}

#[test]
fn grant_lengths_and_expiry() {
    let e = env();
    let a = t("alpha");
    let mut req = ask("c0", &a);
    req.grant_minutes = Some(121);
    assert!(store::request(&e.db, &e.kinds, req, T0)
        .unwrap_err()
        .0
        .contains("5-120"));
    let mut req = ask("c0", &a);
    req.grant_minutes = Some(10);
    let r = store::request(&e.db, &e.kinds, req, T0).unwrap();
    assert_eq!(r.grant_minutes, 10);
    let bad = store::decide(&e.db, &e.kinds, &r.id, Decision::Allow, Some(241), None, T0);
    assert_eq!(bad.unwrap_err().code, "bad_args");
    let v = store::decide(&e.db, &e.kinds, &r.id, Decision::Allow, Some(5), None, T0).unwrap();
    assert_eq!(v.grant_expires_at, Some(T0 + 5 * MINUTE));
    assert!(
        spend(&e, "c0", &a, T0 + 5 * MINUTE).is_err(),
        "expired at the boundary"
    );
    assert!(spend(&e, "c0", &a, T0 + 5 * MINUTE - 1).is_ok());
}

#[test]
fn the_default_grant_is_30_minutes() {
    let e = env();
    let a = t("alpha");
    let r = store::request(&e.db, &e.kinds, ask("c0", &a), T0).unwrap();
    let v = store::decide(&e.db, &e.kinds, &r.id, Decision::Allow, None, None, T0).unwrap();
    assert_eq!(v.grant_expires_at, Some(T0 + 30 * MINUTE));
}

#[test]
fn a_mismatched_target_names_the_field() {
    let e = env();
    let r = store::request(&e.db, &e.kinds, ask("c0", &t("alpha")), T0).unwrap();
    store::decide(&e.db, &e.kinds, &r.id, Decision::Allow, None, None, T0).unwrap();
    let err = spend(&e, "c0", &t("beta"), T0 + 1).unwrap_err();
    assert_eq!(err, "The user's Allow is for a different name.");
    // The grant is still there for the right target.
    assert!(spend(&e, "c0", &t("alpha"), T0 + 2).is_ok());
}

#[test]
fn caps_per_chat_per_hour_and_app_wide() {
    let e = env();
    for i in 0..4 {
        store::request(&e.db, &e.kinds, ask("c0", &t(&format!("n{i}"))), T0).unwrap();
    }
    let fifth = store::request(&e.db, &e.kinds, ask("c0", &t("n4")), T0).unwrap_err();
    assert!(fifth.0.contains("4 Allow requests"), "{}", fifth.0);
    // Per hour: deny as we go so the pending cap never binds; the 21st creation is refused.
    let e = env();
    for i in 0..20 {
        let r = store::request(&e.db, &e.kinds, ask("c1", &t(&format!("h{i}"))), T0 + i).unwrap();
        store::decide(&e.db, &e.kinds, &r.id, Decision::Deny, None, None, T0 + i).unwrap();
    }
    let over = store::request(&e.db, &e.kinds, ask("c1", &t("h20")), T0 + 30).unwrap_err();
    assert!(over.0.contains("20 Allow requests"), "{}", over.0);
    assert!(store::request(&e.db, &e.kinds, ask("c1", &t("h21")), T0 + HOUR + 30).is_ok());
    // App-wide: 50 pending across chats.
    let e = env();
    let mut n = 0;
    'outer: for c in 0..20 {
        for i in 0..4 {
            if n == 50 {
                break 'outer;
            }
            store::request(
                &e.db,
                &e.kinds,
                ask(&format!("c{c}"), &t(&format!("a{i}"))),
                T0,
            )
            .unwrap();
            n += 1;
        }
    }
    let app = store::request(&e.db, &e.kinds, ask("c19", &t("z")), T0).unwrap_err();
    assert!(app.0.contains("50 Allow requests"), "{}", app.0);
}

#[test]
fn requests_are_idempotent_and_bounded() {
    let e = env();
    let a = t("alpha");
    let mut r1 = ask("c0", &a);
    r1.tool_use_id = Some("tu1");
    let x = store::request(&e.db, &e.kinds, r1, T0).unwrap();
    let other = t("other");
    let mut r2 = ask("c0", &other);
    r2.tool_use_id = Some("tu1");
    assert_eq!(
        store::request(&e.db, &e.kinds, r2, T0).unwrap().id,
        x.id,
        "same tool use"
    );
    assert_eq!(
        store::request(&e.db, &e.kinds, ask("c0", &a), T0)
            .unwrap()
            .id,
        x.id,
        "same pending target"
    );
    let mut long = ask("c0", &a);
    let reason = "r".repeat(1_001);
    long.reason = &reason;
    assert!(store::request(&e.db, &e.kinds, long, T0).is_err());
    let mut ev = ask("c0", &a);
    let evidence = "e".repeat(4_001);
    ev.evidence = &evidence;
    assert!(store::request(&e.db, &e.kinds, ev, T0).is_err());
    let mut unknown = ask("c0", &a);
    unknown.kind = "land";
    assert!(store::request(&e.db, &e.kinds, unknown, T0)
        .unwrap_err()
        .0
        .contains("no Allow action"));
    let bad = json!({});
    assert!(
        store::request(&e.db, &e.kinds, ask("c0", &bad), T0).is_err(),
        "host validates the target"
    );
}

fn typed(e: &Env, chat: &str, id: &str, text: &str, origin: &str, at: i64) {
    e.db.write::<_, rusqlite::Error>(|tx| {
        tx.execute(
            "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES (?1, ?2, ?3, 'complete', ?4)",
            params![id, chat, text, at],
        )?;
        tx.execute(
            "INSERT INTO submission_meta(submission_id, origin) VALUES (?1, ?2)",
            params![id, origin],
        )?;
        Ok(())
    })
    .unwrap();
}

fn words(
    e: &Env,
    chat: &str,
    kind: &str,
    target: &Value,
    now: i64,
) -> Result<store::ConsentRecord, String> {
    e.db.write::<_, store::Refusal>(|tx| {
        store::spend_words(tx, &e.kinds, chat, kind, target, "test", now)
    })
    .map_err(|r| r.0)
}

fn record_ask(e: &Env, chat: &str, kind: &str, target: &Value, now: i64) {
    e.db.write::<_, store::Refusal>(|tx| store::record_ask(tx, &e.kinds, chat, kind, target, now))
        .unwrap();
}

#[test]
fn words_need_a_recorded_refusal_first() {
    let e = env();
    let a = t("alpha");
    typed(
        &e,
        "c0",
        "s1",
        "Run the test action alpha.",
        "typed",
        T0 + 1,
    );
    assert!(words(&e, "c0", "test_action", &a, T0 + 2)
        .unwrap_err()
        .starts_with("Ask first"));
}

#[test]
fn words_count_only_typed_messages_in_the_episode_once() {
    let e = env();
    let a = t("alpha");
    record_ask(&e, "c0", "test_action", &a, T0);
    // App-originated rows never count, whatever they say.
    typed(
        &e,
        "c0",
        "s0",
        "Run the test action alpha.",
        "other_chat",
        T0 + 1,
    );
    typed(
        &e,
        "c0",
        "s1",
        "Yes. Run the test action alpha.",
        "typed",
        T0 + 2,
    );
    let rec = words(&e, "c0", "test_action", &a, T0 + 3).unwrap();
    assert_eq!(rec.proof, "words");
    assert_eq!(rec.submission_id.as_deref(), Some("s1"));
    // The same message cannot be spent twice.
    let again = words(&e, "c0", "test_action", &a, T0 + 4).unwrap_err();
    assert!(again.contains("already used"), "{again}");
}

#[test]
fn only_the_first_two_typed_messages_and_two_hours_count() {
    let e = env();
    let a = t("alpha");
    record_ask(&e, "c0", "test_action", &a, T0);
    typed(&e, "c0", "s1", "hmm", "typed", T0 + 1);
    typed(&e, "c0", "s2", "let me look", "typed", T0 + 2);
    typed(
        &e,
        "c0",
        "s3",
        "Run the test action alpha.",
        "typed",
        T0 + 3,
    );
    assert!(
        words(&e, "c0", "test_action", &a, T0 + 4).is_err(),
        "third message is too late"
    );
    let e = env();
    record_ask(&e, "c0", "test_action", &a, T0);
    typed(
        &e,
        "c0",
        "s1",
        "Run the test action alpha.",
        "typed",
        T0 + 2 * HOUR + 1,
    );
    assert!(
        words(&e, "c0", "test_action", &a, T0 + 2 * HOUR + 2).is_err(),
        "outside the episode"
    );
    // A message from before the refusal does not count.
    let e = env();
    typed(
        &e,
        "c0",
        "s1",
        "Run the test action alpha.",
        "typed",
        T0 - 1,
    );
    record_ask(&e, "c0", "test_action", &a, T0);
    assert!(words(&e, "c0", "test_action", &a, T0 + 1).is_err());
    // Words for another target do not count.
    let e = env();
    record_ask(&e, "c0", "test_action", &a, T0);
    typed(&e, "c0", "s1", "Run the test action beta.", "typed", T0 + 1);
    assert!(words(&e, "c0", "test_action", &a, T0 + 2).is_err());
}

#[test]
fn a_click_only_kind_refuses_words() {
    let e = env();
    let a = t("alpha");
    record_ask(&e, "c0", "click_only", &a, T0);
    typed(
        &e,
        "c0",
        "s1",
        "Run the test action alpha.",
        "typed",
        T0 + 1,
    );
    assert!(words(&e, "c0", "click_only", &a, T0 + 2)
        .unwrap_err()
        .contains("click"));
}

#[test]
fn upkeep_expires_and_lists_deliveries_once() {
    let e = env();
    let a = t("alpha");
    let p = store::request(&e.db, &e.kinds, ask("c0", &a), T0).unwrap();
    let g = store::request(&e.db, &e.kinds, ask("c1", &a), T0).unwrap();
    store::decide(&e.db, &e.kinds, &g.id, Decision::Allow, Some(5), None, T0).unwrap();
    let u = store::upkeep(&e.db, &e.kinds, T0 + MINUTE).unwrap();
    assert!(u.changed_chats.is_empty());
    assert_eq!(u.deliveries.len(), 1);
    assert_eq!(
        u.deliveries[0].submission_id,
        format!("consent-decision:{}", g.id)
    );
    assert!(u.deliveries[0].text.starts_with("[Helm] The user allowed"));
    store::mark_delivered(&e.db, &g.id, T0 + MINUTE).unwrap();
    assert!(store::upkeep(&e.db, &e.kinds, T0 + 2 * MINUTE)
        .unwrap()
        .deliveries
        .is_empty());
    // The grant lapses; the pending request expires after 24 h and is owed a notice.
    let u = store::upkeep(&e.db, &e.kinds, T0 + PENDING_LIFE).unwrap();
    assert_eq!(u.changed_chats, ["c0", "c1"]);
    assert_eq!(u.deliveries.len(), 1);
    assert_eq!(u.deliveries[0].request_id, p.id);
    assert!(u.deliveries[0].text.contains("expired without an answer"));
    assert!(
        spend(&e, "c1", &a, T0 + 2 * MINUTE).is_err(),
        "a lapsed grant is gone"
    );
}

#[test]
fn the_hint_is_last_once_and_not_while_pending_or_denied() {
    let e = env();
    let a = t("alpha");
    let hint = |body: &str| {
        e.db.write::<_, store::Refusal>(|tx| {
            store::with_allow_hint(tx, &e.kinds, "c0", "test_action", &a, body)
        })
        .unwrap()
    };
    let once = hint("Refused.");
    assert!(once.ends_with("call app_request_allow {action, target, reason} to ask the user."));
    assert_eq!(hint(&once), once, "never appended twice");
    let long = hint(&"x".repeat(3_000));
    assert!(long.starts_with(&"x".repeat(1_600)) && !long.starts_with(&"x".repeat(1_601)));
    let r = store::request(&e.db, &e.kinds, ask("c0", &a), T0).unwrap();
    assert_eq!(hint("Refused."), "Refused.", "pending: no hint");
    store::decide(&e.db, &e.kinds, &r.id, Decision::Deny, None, None, T0).unwrap();
    assert_eq!(hint("Refused."), "Refused.", "denied: no hint");
}

#[test]
fn ops_decide_and_signal() {
    let e = env();
    let r = store::request(&e.db, &e.kinds, ask("c0", &t("alpha")), crate::tests::now()).unwrap();
    let mut services = Services::new();
    services.insert(e.db.clone());
    services.insert(Arc::new(e.kinds.clone()));
    let sink = Arc::new(RecordingSink::default());
    let ctx = OpCtx {
        services: Arc::new(services),
        events: sink.clone(),
    };
    let list = crate::ops::consent_list(&ctx, json!({"chatId": "c0"})).unwrap();
    assert_eq!(list["pending"][0]["id"], r.id);
    assert_eq!(list["pending"][0]["label"], "Run a test action");
    let v = crate::ops::consent_decide(
        &ctx,
        json!({"requestId": r.id, "decision": "allow", "grantMinutes": 60}),
    )
    .unwrap();
    assert_eq!(v["state"], "allowed");
    assert_eq!(sink.kinds(), ["consent"]);
    let again = crate::ops::consent_decide(&ctx, json!({"requestId": r.id, "decision": "deny"}));
    assert_eq!(again.unwrap_err().code, "conflict");
    assert!(
        crate::ops::consent_decide(&ctx, json!({"requestId": r.id, "decision": "maybe"})).is_err()
    );
}

pub(crate) fn now() -> i64 {
    helm_core::now_ms()
}
