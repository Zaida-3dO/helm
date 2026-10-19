//! The user's own task and change actions ([`crate::user`], [`crate::land::ready`]) against the
//! same engine the orchestrator's tool drives.

use crate::gh::{Gh, GhError, Out};
use crate::store::{self, Change};
use crate::types::*;
use crate::user::{task_action, CHAT_OFF};
use crate::wftools::workflow_call;
use crate::{actions, settings};
use helm_consent::AllowKinds;
use helm_core::db::{Database, MigrationRegistry};
use serde_json::{json, Value};
use std::sync::Mutex;

const REPO: &str = "github.com/o/r";
const TIP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PR: &str = "https://github.com/o/r/pull/7";

struct T {
    _d: tempfile::TempDir,
    db: Database,
    kinds: AllowKinds,
}

fn setup(f: impl FnOnce(&mut WorkflowSettings)) -> T {
    let mut reg = MigrationRegistry::new();
    helm_consent::register_migrations(&mut reg);
    crate::register_migrations(&mut reg);
    let d = tempfile::tempdir().unwrap();
    let db = Database::open(&d.path().join("t.sqlite3"), &reg).unwrap();
    let mut s = WorkflowSettings {
        enabled: true,
        ..Default::default()
    };
    f(&mut s);
    db.write::<_, rusqlite::Error>(|tx| {
        tx.execute(
            "INSERT INTO projects(id, name, path, created_at) VALUES ('p', 'P', 'C:/w', 1)",
            [],
        )?;
        tx.execute(
            "INSERT INTO chats(id, project_id, title, created_at, updated_at) VALUES ('c', 'p', 'T', 1, 1)",
            [],
        )?;
        settings::store(tx, &s, 1)
    })
    .unwrap();
    let mut kinds = AllowKinds::new();
    crate::kinds::register_allow_kinds(&mut kinds);
    T { _d: d, db, kinds }
}

impl T {
    fn wf(&self, args: Value) -> Result<Value, String> {
        workflow_call(&self.db, &self.kinds, "c", false, &args, 10).map_err(|e| e.text())
    }
    fn user(&self, run: &str, a: Value) -> Result<Value, String> {
        self.db
            .write(|tx| task_action(tx, &self.kinds, run, &a, 20))
            .unwrap()
    }
    fn run(&self, id: &str) -> store::Run {
        self.db.snapshot(|tx| store::run(tx, id)).unwrap().unwrap()
    }
    fn run_id(&self) -> String {
        self.wf(json!({"action": "status"})).unwrap()["task"]["runId"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn events(&self, kind: &str, verdict: Option<&str>) -> i64 {
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT count(*) FROM workflow_events WHERE kind = ?1 AND (?2 IS NULL OR verdict = ?2)",
                    rusqlite::params![kind, verdict],
                    |r| r.get(0),
                )
            })
            .unwrap()
    }
    /// Put a change of `run` in `state` (as the land step or a hand-off would have).
    fn change(&self, run: &str, branch: &str, state: &str) {
        self.db
            .write(|tx| {
                let c = Change {
                    run_id: run.into(),
                    repo: REPO.into(),
                    branch: branch.into(),
                    pr: Some(PR.into()),
                    tip: Some(TIP.into()),
                    approved_tip: Some(TIP.into()),
                    state: state.into(),
                    handoff_attempt: Some(1),
                    ..Default::default()
                };
                store::upsert_change(tx, &c, 5)
            })
            .unwrap();
    }
    fn state_of(&self, run: &str, branch: &str) -> String {
        self.db
            .snapshot(|tx| store::change(tx, run, REPO, branch))
            .unwrap()
            .unwrap()
            .state
    }
    fn set_status(&self, run: &str, status: &str) {
        let mut r = self.run(run);
        r.status = status.into();
        self.db
            .write(|tx| store::save_run(tx, &r, None, 6))
            .unwrap();
    }
}

#[test]
fn sign_off_binds_to_the_plan_shown_and_waits_for_the_review() {
    let t = setup(|s| s.plan_sign_off = SignOff::Me);
    t.wf(json!({"action": "plan", "text": "# Do it\nsteps"}))
        .unwrap();
    let run = t.run_id();
    let plan = actions::plan_id("# Do it\nsteps");
    // Plan review is required and has not approved yet.
    let e = t
        .user(&run, json!({"action": "signOff", "planId": plan}))
        .unwrap_err();
    assert!(e.contains("not approved"), "{e}");
    t.db.write(|tx| {
        let mut r = store::run(tx, &run)?.unwrap();
        r.plan_approved = true;
        store::save_run(tx, &r, None, 7)
    })
    .unwrap();
    // A plan id other than the plan of record (the plan changed after the panel showed it).
    let e = t
        .user(&run, json!({"action": "signOff", "planId": "0".repeat(64)}))
        .unwrap_err();
    assert!(e.contains("plan changed"), "{e}");
    assert!(!t.run(&run).plan_signed_off);
    let ok = t
        .user(&run, json!({"action": "signOff", "planId": plan}))
        .unwrap();
    assert_eq!(ok, json!({"signedOff": true, "changed": true}));
    assert!(t.run(&run).plan_signed_off);
    assert_eq!(t.events("consent", Some("signOff")), 1);
    let s = t.db.snapshot(settings::load).unwrap();
    assert_eq!(crate::engine::plan_missing(&s, &t.run(&run)), None);
    // A new plan of record needs a new sign-off.
    t.wf(json!({"action": "plan", "text": "# Do it differently"}))
        .unwrap();
    assert!(!t.run(&run).plan_signed_off);
    // With sign-off by review only, there is nothing to sign.
    let t = setup(|_| {});
    t.wf(json!({"action": "plan", "text": "x"})).unwrap();
    let run = t.run_id();
    let e = t
        .user(
            &run,
            json!({"action": "signOff", "planId": actions::plan_id("x")}),
        )
        .unwrap_err();
    assert!(e.contains("needs no sign-off"), "{e}");
}

#[test]
fn the_user_skips_a_required_stage_and_allows_more_through_the_consent_path() {
    let t = setup(|_| {});
    t.wf(json!({"action": "start", "title": "T"})).unwrap();
    let run = t.run_id();
    // The agent alone is refused; the user's click is the consent.
    assert!(t
        .wf(json!({"action": "skip", "stage": "review", "reason": "x"}))
        .is_err());
    t.user(&run, json!({"action": "skip", "stage": "review"}))
        .unwrap();
    assert_eq!(t.run(&run).skipped, ["review"]);
    assert_eq!(t.events("consent", Some("skip")), 1);
    assert_eq!(t.events("skip", Some("review")), 1);
    assert!(t
        .user(&run, json!({"action": "skip", "stage": "build"}))
        .is_err());
    t.user(
        &run,
        json!({"action": "allowMore", "stage": "planReview", "n": 2}),
    )
    .unwrap();
    assert_eq!(t.run(&run).plan_extra, 2);
    assert_eq!(t.events("consent", Some("allowMore")), 1);
    assert!(t
        .user(
            &run,
            json!({"action": "allowMore", "stage": "review", "n": 9})
        )
        .unwrap_err()
        .contains("1 to 5"));
}

#[test]
fn an_agent_has_no_route_to_the_users_task_actions() {
    let t = setup(|_| {});
    t.wf(json!({"action": "start", "title": "T"})).unwrap();
    for a in ["signOff", "reopen", "end", "notVisual", "visual"] {
        let e = t.wf(json!({"action": a})).unwrap_err();
        assert!(e.starts_with("action must be"), "{a}: {e}");
    }
    // Only the user clears the visual flag.
    t.wf(json!({"action": "update", "visual": true})).unwrap();
    assert!(t.wf(json!({"action": "update", "visual": false})).is_err());
    let run = t.run_id();
    t.user(&run, json!({"action": "notVisual"})).unwrap();
    assert!(!t.run(&run).visual);
    assert_eq!(t.events("visual_cleared", None), 1);
}

#[test]
fn park_reopen_and_end() {
    let t = setup(|_| {});
    t.wf(json!({"action": "start", "title": "T"})).unwrap();
    let run = t.run_id();
    t.change(&run, "a", "awaiting_merge");
    t.change(&run, "b", "landed");
    t.user(&run, json!({"action": "park"})).unwrap();
    assert_eq!(t.run(&run).status, "parked");
    // Reopen: the next attempt, and the awaiting change goes back to open; a landed one stays.
    let r = t.user(&run, json!({"action": "reopen"})).unwrap();
    assert_eq!(r["attempt"], 2);
    assert_eq!(t.run(&run).status, "active");
    assert_eq!(t.state_of(&run, "a"), "open");
    assert_eq!(t.state_of(&run, "b"), "landed");
    assert_eq!(t.events("reopened", None), 1);
    assert!(t
        .user(&run, json!({"action": "reopen"}))
        .unwrap_err()
        .contains("parked or handed-off"));
    // Another active task blocks a Reopen; a change mid-land blocks Reopen and End.
    t.user(&run, json!({"action": "park"})).unwrap();
    t.wf(json!({"action": "start", "title": "Other", "new": true}))
        .unwrap();
    let e = t.user(&run, json!({"action": "reopen"})).unwrap_err();
    assert!(e.contains("\"Other\" is active"), "{e}");
    t.change(&run, "c", "landing");
    let other = t.run_id();
    t.user(&other, json!({"action": "end"})).unwrap();
    let e = t.user(&run, json!({"action": "reopen"})).unwrap_err();
    assert!(e.contains("being landed"), "{e}");
    assert!(t
        .user(&run, json!({"action": "end"}))
        .unwrap_err()
        .contains("being landed"));
    // End releases what the task still holds and keeps the record.
    t.change(&run, "c", "land_failed");
    t.user(&run, json!({"action": "end"})).unwrap();
    assert_eq!(t.run(&run).status, "ended");
    assert_eq!(t.state_of(&run, "a"), "dropped");
    assert_eq!(t.state_of(&run, "c"), "dropped");
    assert_eq!(t.state_of(&run, "b"), "landed");
    assert_eq!(t.events("ended", None), 2);
}

#[test]
fn with_the_chat_off_only_end_task_works() {
    let t = setup(|_| {});
    t.wf(json!({"action": "start", "title": "T"})).unwrap();
    let run = t.run_id();
    t.db.write(|tx| {
        let s = settings::load(tx)?;
        crate::mode::user_set(tx, "c", ChatMode::Off, &s, 8).map(|_| ())?;
        crate::engine::park_for_off(tx, "c", 8).map(|_| ())
    })
    .unwrap();
    for a in ["reopen", "visual", "park"] {
        assert_eq!(
            t.user(&run, json!({"action": a})).unwrap_err(),
            CHAT_OFF,
            "{a}"
        );
    }
    t.user(&run, json!({"action": "end"})).unwrap();
    assert_eq!(t.run(&run).status, "ended");
    assert!(t
        .user("nope", json!({"action": "end"}))
        .unwrap_err()
        .contains("No such task"));
    assert!(t
        .user(&run, json!({"action": "explode"}))
        .unwrap_err()
        .starts_with("action must be"));
}

#[test]
fn decline_parks_a_handed_off_task_but_an_active_task_goes_on() {
    let t = setup(|_| {});
    t.wf(json!({"action": "start", "title": "T"})).unwrap();
    let run = t.run_id();
    t.change(&run, "a", "awaiting_merge");
    t.change(&run, "b", "awaiting_merge");
    let act = |branch: &str, action: &str| {
        t.db.write(|tx| crate::land::user_action(tx, &run, REPO, branch, action, 9))
            .unwrap()
    };
    act("a", "decline").unwrap();
    assert_eq!(t.state_of(&run, "a"), "declined");
    assert_eq!(t.run(&run).status, "active", "an active task goes on");
    t.set_status(&run, "handed_off");
    act("b", "drop").unwrap();
    assert_eq!(t.state_of(&run, "b"), "dropped");
    let r = t.run(&run);
    assert_eq!(r.status, "parked");
    assert!(r.park_reason.unwrap().contains("dropped the change b"));
}

/// Answers `pr view` with `view`, and `pr ready` / `pr edit` with `ok`; records every call.
struct Gh1 {
    view: String,
    ok: bool,
    calls: Mutex<Vec<String>>,
}

impl Gh for Gh1 {
    fn run(&self, args: &[String]) -> Result<Out, GhError> {
        let line = args.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        let (ok, stdout) = if line.starts_with("pr view") {
            (true, self.view.clone())
        } else {
            (self.ok, String::new())
        };
        Ok(Out {
            ok,
            stdout,
            stderr: String::new(),
        })
    }
}

fn gh(head: &str, draft: bool, ok: bool) -> Gh1 {
    Gh1 {
        view: json!({"state": "OPEN", "isDraft": draft, "headRefOid": head, "baseRefName": "main",
                     "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN"})
        .to_string(),
        ok,
        calls: Mutex::new(vec![]),
    }
}

#[test]
fn ready_for_review_reverifies_the_approved_head_before_it_flips() {
    let t = setup(|s| s.landing.flip_label = Some("needs-review".into()));
    t.wf(json!({"action": "start", "title": "T"})).unwrap();
    let run = t.run_id();
    t.change(&run, "a", "handed_off");
    // The PR's head moved past the approved commit: not flipped, and the reason is on the change.
    let moved = gh(&"b".repeat(40), true, true);
    let e = crate::land::ready(&t.db, &moved, &run, REPO, "a", 10)
        .unwrap()
        .unwrap_err();
    assert!(e.contains("not the approved commit"), "{e}");
    assert_eq!(t.state_of(&run, "a"), "handed_off");
    assert_eq!(t.events("flip_verify_failed", None), 1);
    assert!(
        !moved
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("pr ready")),
        "nothing was marked ready"
    );
    // At the approved head: marked ready, the flip label removed, then flipped.
    let good = gh(TIP, true, true);
    assert_eq!(
        crate::land::ready(&t.db, &good, &run, REPO, "a", 11).unwrap(),
        Ok("flipped".into())
    );
    let calls = good.calls.lock().unwrap().clone();
    assert!(
        calls.iter().any(|l| l.starts_with("pr ready 7 --repo o/r")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|l| l.contains("--remove-label needs-review")),
        "{calls:?}"
    );
    assert_eq!(t.state_of(&run, "a"), "flipped");
    assert_eq!(t.events("flipped", None), 1);
    // Only a handed-off change can be flipped.
    let e = crate::land::ready(&t.db, &good, &run, REPO, "a", 12)
        .unwrap()
        .unwrap_err();
    assert!(e.contains("flipped"), "{e}");
}
