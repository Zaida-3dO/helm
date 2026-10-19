//! End-to-end checks of the engine through the two tools' entry points, with a fake git.

use crate::git::tests::FakeTrees;
use crate::types::*;
use crate::wftools::{report_call, workflow_call, OFF};
use crate::{settings, store};
use helm_consent::AllowKinds;
use helm_core::db::{Database, MigrationRegistry};
use rusqlite::params;
use serde_json::{json, Value};

const REPO: &str = "github.com/o/r";
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

struct T {
    _d: tempfile::TempDir,
    db: Database,
    kinds: AllowKinds,
    trees: FakeTrees,
}

fn setup(f: impl FnOnce(&mut WorkflowSettings)) -> T {
    let mut reg = MigrationRegistry::new();
    helm_consent::register_migrations(&mut reg);
    crate::register_migrations(&mut reg);
    let d = tempfile::tempdir().unwrap();
    let db = Database::open(&d.path().join("t.sqlite3"), &reg).unwrap();
    // Chats start on (the default is Auto).
    let mut s = WorkflowSettings {
        enabled: true,
        new_chats: NewChats::Always,
        ..Default::default()
    };
    f(&mut s);
    db.write::<_, rusqlite::Error>(|tx| {
        // The chats crate's submission origins (the words verifier reads them).
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS submission_meta (submission_id TEXT PRIMARY KEY, origin TEXT NOT NULL DEFAULT 'user', priority TEXT, run_id TEXT);",
        )?;
        tx.execute("INSERT INTO projects(id, name, path, created_at) VALUES ('p', 'P', 'C:/w', 1)", [])?;
        tx.execute(
            "INSERT INTO chats(id, project_id, title, created_at, updated_at) VALUES ('c', 'p', 'T', 1, 1)",
            params![],
        )?;
        settings::store(tx, &s, 1)
    })
    .unwrap();
    let mut kinds = AllowKinds::new();
    crate::kinds::register_allow_kinds(&mut kinds);
    T {
        _d: d,
        db,
        kinds,
        trees: FakeTrees::default(),
    }
}

impl T {
    fn wf(&self, args: Value) -> Result<Value, String> {
        workflow_call(&self.db, &self.kinds, "c", false, &args, 10).map_err(|e| e.text())
    }
    fn rep(&self, args: Value) -> Result<Value, String> {
        report_call(&self.db, &self.kinds, &self.trees, "c", true, args, 10).map_err(|e| e.text())
    }
    fn events(&self, kind: &str) -> i64 {
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT count(*) FROM workflow_events WHERE kind = ?1",
                    [kind],
                    |r| r.get(0),
                )
            })
            .unwrap()
    }
    fn state(&self) -> String {
        self.db
            .snapshot(|tx| tx.query_row("SELECT state FROM workflow_changes", [], |r| r.get(0)))
            .unwrap()
    }
    fn approved_plan(&self) {
        self.wf(json!({"action": "plan", "text": "# Do the thing\nsteps"}))
            .unwrap();
        self.rep(json!({"kind": "plan_review", "verdict": "approve", "summary": "ok"}))
            .unwrap();
    }
    fn build(&self, tip: &str) -> Result<Value, String> {
        self.rep(json!({"kind": "build", "repo": REPO, "branch": "feat-x", "tip": tip, "base": "main", "pr": "https://github.com/o/r/pull/7", "summary": "built"}))
    }
    fn review(&self, tip: &str, verdict: &str) -> Result<Value, String> {
        self.rep(json!({"kind": "code_review", "repo": REPO, "branch": "feat-x", "tip": tip, "base": "main", "verdict": verdict, "summary": "r"}))
    }
}

#[test]
fn with_the_workflow_off_the_tools_refuse_and_write_nothing() {
    let t = setup(|s| s.enabled = false);
    assert!(t
        .wf(json!({"action": "start", "title": "x"}))
        .unwrap_err()
        .starts_with(OFF));
    assert!(t
        .rep(json!({"kind": "plan_review", "verdict": "approve", "summary": "s"}))
        .unwrap_err()
        .starts_with(OFF));
    let runs: i64 =
        t.db.snapshot(|tx| tx.query_row("SELECT count(*) FROM workflow_runs", [], |r| r.get(0)))
            .unwrap();
    assert_eq!(runs, 0);
    // A chat whose own state is off behaves the same under an enabled master switch.
    let t = setup(|s| s.new_chats = NewChats::Never);
    assert!(t
        .wf(json!({"action": "status"}))
        .unwrap_err()
        .starts_with(OFF));
}

#[test]
fn the_pipeline_runs_plan_review_build_review_cleanup_and_hand_off() {
    let t = setup(|s| {
        s.landing.repos.push(RepoRule {
            repo: REPO.into(),
            mode: LandMode::Merge,
            method: Some(MergeMethod::Squash),
        })
    });
    t.trees.set(A, "t1");
    t.trees.set(B, "t1");
    // A build before the plan is approved is refused by the build gate and recorded.
    t.wf(json!({"action": "start", "title": "Feature X"}))
        .unwrap();
    assert!(t.build(A).unwrap_err().contains("build gate"));
    assert_eq!(t.events("gate_denied"), 1);
    t.approved_plan();
    t.build(A).unwrap();
    // The orchestrator never reports for itself.
    let e = report_call(
        &t.db,
        &t.kinds,
        &t.trees,
        "c",
        false,
        json!({"kind": "plan_review", "verdict": "approve", "summary": "s"}),
        1,
    )
    .unwrap_err();
    assert!(e.text().contains("never reports itself"));
    // A review naming an older tip is stale and approves nothing.
    assert_eq!(t.review(C, "lgtm").unwrap()["outcome"], "stale");
    assert_eq!(t.review(A, "lgtm").unwrap()["outcome"], "approved");
    // Hand-off waits for clean-up (the change has a PR and clean-up is required).
    let e = t.wf(json!({"action": "hand_off"})).unwrap_err();
    assert!(e.contains("clean-up"), "{e}");
    // A clean-up that kept the tree keeps the approval at its new tip.
    let out = t.rep(json!({"kind": "cleanup", "repo": REPO, "branch": "feat-x", "tip": B, "pr": "https://github.com/o/r/pull/7", "summary": "squashed"})).unwrap();
    assert_eq!(out["outcome"], "kept_tree");
    assert_eq!(out["approved"], true);
    let h = t
        .wf(json!({"action": "hand_off", "summary": "done"}))
        .unwrap();
    assert_eq!(h["taskStatus"], "handed_off");
    assert_eq!(h["handedOff"][0]["mode"], "merge:squash");
    assert_eq!(h["handedOff"][0]["tip"], B);
    assert_eq!(t.state(), "pending_land");
    // One final hand-off per attempt.
    assert_eq!(t.events("handoff_requested"), 1);
}

#[test]
fn a_new_commit_voids_the_approval_unless_the_tree_is_identical() {
    let t = setup(|s| s.stages.cleanup.use_ = StageUse::Off);
    t.trees.set(A, "t1");
    t.trees.set(B, "t1");
    t.trees.set(C, "t2");
    t.approved_plan();
    t.build(A).unwrap();
    t.review(A, "lgtm_with_nits").unwrap();
    assert_eq!(t.build(B).unwrap()["approved"], true, "same tree keeps it");
    let out = t.build(C).unwrap();
    assert_eq!(out["approved"], false);
    assert_eq!(out["outcome"], "approval_voided");
    let e = t.wf(json!({"action": "hand_off"})).unwrap_err();
    assert!(e.contains("no review approval"), "{e}");
    // An unknown tree counts as different.
    t.review(C, "lgtm").unwrap();
    let out = t.build("dddddddddddddddddddddddddddddddddddddddd").unwrap();
    assert_eq!(out["treeKnown"], false);
    assert_eq!(out["approved"], false);
}

#[test]
fn caps_hold_and_the_orchestrator_gets_one_extra_round_per_target_and_attempt() {
    let t = setup(|s| s.stages.review.max_rounds = 1);
    t.trees.set(A, "t1");
    t.approved_plan();
    t.build(A).unwrap();
    // Not at the cap yet: no self-grant.
    let e = t.wf(json!({"action": "allowMore", "stage": "review", "repo": REPO, "branch": "feat-x", "reason": "close"})).unwrap_err();
    assert!(e.contains("not at its cap"), "{e}");
    t.review(A, "changes_required").unwrap();
    assert!(t.review(A, "lgtm").unwrap_err().contains("review cap"));
    let g = t.wf(json!({"action": "allowMore", "stage": "review", "repo": REPO, "branch": "feat-x", "reason": "one nit left"})).unwrap();
    assert!(g["row"].as_str().unwrap().contains("past your cap 1"));
    assert_eq!(t.events("cap_self_granted"), 1);
    // The extra round is used up, so the target is at its cap again: no second self-grant.
    t.review(A, "changes_required").unwrap();
    let e = t.wf(json!({"action": "allowMore", "stage": "review", "repo": REPO, "branch": "feat-x", "reason": "again"})).unwrap_err();
    assert!(e.contains("already gave"), "{e}");
    assert!(t.review(A, "lgtm").unwrap_err().contains("review cap"));
}

#[test]
fn consented_steps_ask_first_and_parked_tasks_block_a_new_start() {
    let t = setup(|_| {});
    t.wf(json!({"action": "start", "title": "One"})).unwrap();
    // Skipping a required stage needs the user: the refusal names the sentence and Allow.
    let e = t
        .wf(json!({"action": "skip", "stage": "review", "reason": "trivial"}))
        .unwrap_err();
    assert!(e.contains("Skip the review stage."), "{e}");
    assert!(e.contains("app_request_allow"), "{e}");
    let asks: i64 =
        t.db.snapshot(|tx| tx.query_row("SELECT count(*) FROM consent_asks", [], |r| r.get(0)))
            .unwrap();
    assert_eq!(asks, 1, "the refusal opened a words episode");
    // A quote with no typed message from the user does not count.
    assert!(t.wf(json!({"action": "skip", "stage": "review", "reason": "x", "userQuote": "Skip the review stage."})).is_err());
    // An auto stage is skipped freely, and a visual task's smoketest never is.
    t.wf(json!({"action": "skip", "stage": "smoketest", "reason": "no UI"}))
        .unwrap();
    t.wf(json!({"action": "update", "repo": REPO, "branch": "feat-x"}))
        .unwrap();
    assert!(t
        .wf(json!({"action": "update", "visual": false}))
        .unwrap_err()
        .contains("Only the user"));
    t.wf(json!({"action": "park", "reason": "later"})).unwrap();
    let e = t
        .wf(json!({"action": "start", "title": "Two"}))
        .unwrap_err();
    assert!(e.contains("start {new: true}"), "{e}");
    assert_eq!(
        t.wf(json!({"action": "start", "title": "Two", "new": true}))
            .unwrap()["started"],
        true
    );
}

#[test]
fn turning_a_chat_off_parks_its_active_task() {
    let t = setup(|_| {});
    t.wf(json!({"action": "start", "title": "One"})).unwrap();
    t.db.write::<_, rusqlite::Error>(|tx| {
        assert!(crate::engine::park_for_off(tx, "c", 5)?);
        assert!(store::active_run(tx, "c")?.is_none());
        Ok(())
    })
    .unwrap();
    assert_eq!(t.events("parked"), 1);
}

#[test]
fn reports_refuse_unknown_fields_and_short_shas_as_correctable() {
    let t = setup(|_| {});
    t.wf(json!({"action": "start", "title": "One"})).unwrap();
    let e = t
        .rep(json!({"kind": "build", "repo": REPO, "branch": "b", "tip": "abc", "summary": "s"}))
        .unwrap_err();
    assert!(e.starts_with("Correctable"), "{e}");
    let e = t.rep(json!({"kind": "build", "repo": REPO, "branch": "b", "tip": A, "summary": "s", "tree": "x"})).unwrap_err();
    assert!(e.starts_with("Correctable") && e.contains("tree"), "{e}");
    let e = t.rep(json!({"kind": "code_review", "repo": REPO, "branch": "b", "tip": A, "base": "main", "verdict": "approve", "summary": "s"})).unwrap_err();
    assert!(e.contains("verdict must be"), "{e}");
}

impl T {
    /// The user types `text` in the chat at time `at`.
    fn user_says(&self, id: &str, text: &str, at: i64) {
        self.db
            .write::<_, rusqlite::Error>(|tx| {
                tx.execute(
                    "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES (?1, 'c', ?2, 'done', ?3)",
                    params![id, text, at],
                )?;
                tx.execute("INSERT INTO submission_meta(submission_id, origin) VALUES (?1, 'typed')", [id])?;
                Ok(())
            })
            .unwrap();
    }
    fn wf_at(&self, args: Value, now: i64) -> Result<Value, String> {
        workflow_call(&self.db, &self.kinds, "c", false, &args, now).map_err(|e| e.text())
    }
}

#[test]
fn the_users_own_words_let_a_consented_step_through_once() {
    let t = setup(|_| {});
    t.wf_at(json!({"action": "start", "title": "One"}), 100)
        .unwrap();
    let skip = json!({"action": "skip", "stage": "review", "reason": "docs only", "userQuote": "Skip the review stage."});
    // Ask first: the step without a proof records the refusal.
    t.wf_at(
        json!({"action": "skip", "stage": "review", "reason": "docs only"}),
        200,
    )
    .unwrap_err();
    t.user_says("s1", "Skip the review stage.", 300);
    let out = t.wf_at(skip.clone(), 400).unwrap();
    assert_eq!(out["withConsent"], true);
    assert_eq!(t.events("consent"), 1);
    let records: i64 =
        t.db.snapshot(|tx| tx.query_row("SELECT count(*) FROM consent_records", [], |r| r.get(0)))
            .unwrap();
    assert_eq!(
        records, 1,
        "the words were spent on the step's own transaction"
    );
}

#[test]
fn dropping_a_handed_off_change_needs_consent_bound_to_its_own_task() {
    let t = setup(|s| s.stages.cleanup.use_ = StageUse::Off);
    t.trees.set(A, "t1");
    t.approved_plan();
    t.build(A).unwrap();
    t.review(A, "lgtm").unwrap();
    t.wf(json!({"action": "hand_off", "changes": [{"repo": REPO, "branch": "feat-x"}]}))
        .unwrap();
    assert_eq!(t.state(), "landing_draft");
    let e = t
        .wf_at(
            json!({"action": "drop", "repo": REPO, "branch": "feat-x", "reason": "obsolete"}),
            200,
        )
        .unwrap_err();
    assert!(e.contains("Drop change #7."), "{e}");
    t.user_says("s1", "Drop change #7.", 300);
    let out = t.wf_at(json!({"action": "drop", "repo": REPO, "branch": "feat-x", "reason": "obsolete", "userQuote": "Drop change #7."}), 400).unwrap();
    assert_eq!(out["dropped"], true);
    assert_eq!(t.state(), "dropped");
    // The consent row is on the task that held the change.
    let on_task: i64 = t
        .db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT count(*) FROM workflow_events e JOIN workflow_changes c ON c.run_id = e.run_id WHERE e.kind = 'consent' AND e.verdict = 'drop'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(on_task, 1);
}
