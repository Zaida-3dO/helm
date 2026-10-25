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

// ---- claims, take-over, gates and post-land ------------------------------------------------

const MIN: i64 = 60_000;

impl T {
    fn add_chat(&self, id: &str, title: &str) {
        self.db
            .write::<_, rusqlite::Error>(|tx| {
                tx.execute(
                    "INSERT INTO chats(id, project_id, title, created_at, updated_at) VALUES (?1, 'p', ?2, 1, 1)",
                    params![id, title],
                )?;
                Ok(())
            })
            .unwrap();
    }
    fn wf_in(&self, chat: &str, args: Value, now: i64) -> Result<Value, String> {
        workflow_call(&self.db, &self.kinds, chat, false, &args, now).map_err(|e| e.text())
    }
    fn sql(&self, q: &str) {
        self.db
            .write::<_, rusqlite::Error>(|tx| tx.execute_batch(q))
            .unwrap();
    }
    fn count(&self, q: &str) -> i64 {
        self.db
            .snapshot(|tx| tx.query_row(q, [], |r| r.get(0)))
            .unwrap()
    }
    fn gate(&self, tool: &str, input: Value, child: bool) -> Value {
        crate::gates::gate_call(
            &self.db,
            "c",
            &json!({"tool": tool, "input": input, "child": child}),
            50,
        )
        .unwrap()
    }
    fn set_path(&self, p: &std::path::Path) {
        self.db
            .write::<_, rusqlite::Error>(|tx| {
                tx.execute("UPDATE projects SET path = ?1", [p.to_string_lossy()])?;
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn one_task_holds_a_change_and_take_over_moves_it_from_a_stopped_chat() {
    let t = setup(|_| {});
    t.add_chat("d", "Second");
    let feat = json!({"action": "update", "repo": REPO, "branch": "feat-x"});
    t.wf_in(
        "c",
        json!({"action": "start", "title": "One", "itemId": "item-1"}),
        10,
    )
    .unwrap();
    t.wf_in("c", feat.clone(), 10).unwrap();
    // A second start on the same board item from another chat is refused, naming the holder.
    let e = t
        .wf_in(
            "d",
            json!({"action": "start", "title": "Two", "itemId": "item-1"}),
            20,
        )
        .unwrap_err();
    assert!(
        e.contains("already being worked on") && e.contains("\"T\""),
        "{e}"
    );
    assert!(
        e.contains("Take over item item-1."),
        "the user's sentence is named: {e}"
    );
    t.wf_in("d", json!({"action": "start", "title": "Two"}), 20)
        .unwrap();
    // Opening the same branch is refused softly with the way through.
    let e = t.wf_in("d", feat.clone(), 20).unwrap_err();
    assert!(e.contains("held by") && e.contains("take_over"), "{e}");
    // A build report that would open it is refused the same way.
    t.sql("UPDATE workflow_runs SET plan_id = 'p', plan_approved = 1 WHERE chat_id = 'd'");
    let e = report_call(
        &t.db,
        &t.kinds,
        &t.trees,
        "d",
        true,
        json!({"kind": "build", "repo": REPO, "branch": "feat-x", "tip": A, "summary": "b"}),
        20,
    )
    .map_err(|e| e.text())
    .unwrap_err();
    assert!(e.starts_with("Permanent") && e.contains("held by"), "{e}");
    // The holder is active and was touched just now: a check reports it needs the user.
    let take = json!({"action": "take_over", "repo": REPO, "branch": "feat-x", "reason": "continuing it here"});
    let chk = t
        .wf_in(
            "d",
            json!({"action": "take_over", "repo": REPO, "branch": "feat-x", "check": true}),
            30,
        )
        .unwrap();
    assert_eq!(chk["wouldGoThrough"], false);
    assert_eq!(chk["needsUser"], true);
    let takeovers = "SELECT count(*) FROM workflow_events WHERE kind = 'claim_takeover'";
    assert_eq!(t.count(takeovers), 0, "a check writes nothing");
    let e = t.wf_in("d", take.clone(), 30).unwrap_err();
    assert!(
        e.contains("Take over feat-x.") && e.contains("app_request_allow"),
        "{e}"
    );
    let _ = crate::notes::take_pending();
    // Idle for 30 minutes: the holder counts as stopped, so no words are needed.
    let out = t.wf_in("d", take.clone(), 10 + 31 * MIN).unwrap();
    assert_eq!(out["takenOver"], true);
    assert_eq!(out["withUser"], false);
    let notes = crate::notes::take_pending();
    let to: Vec<&str> = notes.iter().map(|n| n.chat_id.as_str()).collect();
    assert_eq!(to, ["c", "d"], "a note in both chats");
    assert!(
        notes[0].steer && !notes[1].steer,
        "only the holder may be steered"
    );
    assert_eq!(t.count(takeovers), 2);
    assert_eq!(t.count("SELECT count(*) FROM workflow_events WHERE kind = 'change_dropped' AND verdict = 'taken_over' AND chat_id = 'c'"), 1);
    assert_eq!(t.count("SELECT count(*) FROM workflow_changes c JOIN workflow_runs r ON r.id = c.run_id WHERE r.chat_id = 'd' AND c.state = 'open'"), 1);
    // Now the second chat is the holder another chat is told about.
    t.add_chat("e", "Third");
    t.wf_in(
        "e",
        json!({"action": "start", "title": "Three"}),
        10 + 32 * MIN,
    )
    .unwrap();
    assert!(t
        .wf_in("e", feat.clone(), 10 + 32 * MIN)
        .unwrap_err()
        .contains("\"Second\""));
    // Mid-land changes are never taken over; an archived chat's item no longer blocks a start.
    t.sql("UPDATE workflow_changes SET state = 'landing' WHERE state = 'open'");
    let e = t
        .wf_in(
            "c",
            json!({"action": "take_over", "repo": REPO, "branch": "feat-x", "reason": "x"}),
            10 + 33 * MIN,
        )
        .unwrap_err();
    assert!(e.contains("being landed"), "{e}");
    t.add_chat("f", "Fourth");
    t.sql("UPDATE chats SET archived = 1 WHERE id = 'c'");
    t.wf_in(
        "f",
        json!({"action": "start", "title": "Four", "itemId": "item-1"}),
        10 + 34 * MIN,
    )
    .unwrap();
}

#[test]
fn the_gates_act_only_with_orchestrator_on_and_a_task_active() {
    let allow = json!({"allow": true});
    // Orchestrator off for the chat: no launch variable, and every question is allowed.
    let off = setup(|s| s.new_chats = NewChats::Never);
    assert!(!crate::gates::launch_wants_gates(&off.db, "c"));
    assert_eq!(
        off.gate("Agent", json!({"subagent_type": "coder"}), false),
        allow
    );
    assert_eq!(
        off.gate("Write", json!({"file_path": "C:/w/a.rs"}), false),
        allow
    );
    assert_eq!(off.count("SELECT count(*) FROM workflow_events"), 0);
    let master_off = setup(|s| s.enabled = false);
    assert!(!crate::gates::launch_wants_gates(&master_off.db, "c"));
    // On, but no task yet: allowed.
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("r");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(
        repo.join(".git/config"),
        "[remote \"origin\"]\n\turl = https://github.com/o/r.git\n",
    )
    .unwrap();
    let t = setup(|_| {});
    t.set_path(&repo);
    assert!(crate::gates::launch_wants_gates(&t.db, "c"));
    assert_eq!(
        t.gate("Agent", json!({"subagent_type": "coder"}), false),
        allow
    );
    t.wf(json!({"action": "start", "title": "One"})).unwrap();
    let f = repo.join("src").join("a.rs").to_string_lossy().to_string();
    let outside = dir.path().join("notes.md").to_string_lossy().to_string();
    let deny = |v: Value, has: &str| {
        assert_eq!(v["allow"], false, "{v}");
        assert!(v["reason"].as_str().unwrap().contains(has), "{v}");
    };
    // (tool, input, child, None = allowed or Some(text in the refusal))
    let rows: Vec<(&str, Value, bool, Option<&str>)> = vec![
        ("Agent", json!({"subagent_type": "planner"}), false, None),
        (
            "Agent",
            json!({"subagent_type": "coder"}),
            false,
            Some("needs the plan first"),
        ),
        ("Agent", json!({}), false, Some("Build-exempt")),
        (
            "Task",
            json!({"subagent_type": "Explore"}),
            false,
            Some("Build-exempt"),
        ),
        (
            "Agent",
            json!({"subagent_type": "plan-reviewer"}),
            false,
            Some("no plan of record"),
        ),
        (
            "Agent",
            json!({"subagent_type": "pr-cleanup"}),
            false,
            Some("clean-up basis"),
        ),
        (
            "Agent",
            json!({"subagent_type": "planner", "isolation": "remote"}),
            false,
            Some("remote"),
        ),
        (
            "Agent",
            json!({"subagent_type": "reviewer--2"}),
            true,
            Some("only the task's orchestrator"),
        ),
        (
            "Agent",
            json!({"subagent_type": "Explore", "run_in_background": true}),
            true,
            Some("background"),
        ),
        ("Agent", json!({"subagent_type": "Explore"}), true, None),
        ("Bash", json!({"command": "ls"}), false, None),
        ("Write", json!({"file_path": f}), false, Some("allow_edit")),
        ("Edit", json!({"file_path": f}), true, None),
        ("Write", json!({"file_path": outside}), false, None),
    ];
    for (tool, input, child, want) in rows {
        let got = t.gate(tool, input.clone(), child);
        match want {
            None => assert_eq!(got, allow, "{tool} {input} child={child}"),
            Some(has) => deny(got, has),
        }
    }
    assert_eq!(
        t.count("SELECT count(*) FROM workflow_events WHERE kind = 'gate_denied'"),
        9
    );
    // The ways through: an approved plan opens the build gate; allow_edit opens one file.
    t.approved_plan();
    assert_eq!(
        t.gate("Agent", json!({"subagent_type": "coder"}), false),
        allow
    );
    let g = t
        .wf(json!({"action": "allow_edit", "file": f, "reason": "the task's notes file"}))
        .unwrap();
    assert_eq!(g["granted"], true);
    assert_eq!(t.gate("Write", json!({"file_path": f}), false), allow);
    let other = repo.join("b.rs").to_string_lossy().to_string();
    deny(
        t.gate("Write", json!({"file_path": other}), false),
        "allow_edit",
    );
    let g = t
        .wf(json!({"action": "allow_edit", "file": outside, "reason": "x"}))
        .unwrap();
    assert_eq!(
        g["gated"], false,
        "a file outside the task's repositories needs no grant"
    );
    // Each gate's user switch turns it off.
    t.db.write::<_, rusqlite::Error>(|tx| {
        let mut s = settings::load(tx)?;
        s.dispatch_gate = false;
        s.orchestrator_edits = EditsRule::Allow;
        settings::store(tx, &s, 2)
    })
    .unwrap();
    assert_eq!(
        t.gate(
            "Task",
            json!({"subagent_type": "Explore", "isolation": "remote"}),
            false
        ),
        allow
    );
    assert_eq!(t.gate("Write", json!({"file_path": other}), false), allow);
    assert!(
        !crate::gates::launch_wants_gates(&t.db, "c"),
        "nothing left to gate"
    );
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git on PATH");
    assert!(o.status.success(), "git {args:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

#[test]
fn a_finished_land_wraps_up_its_worktree_and_queues_follow_ups() {
    let t = setup(|s| {
        s.stages.cleanup.use_ = StageUse::Off;
        s.landing.repos.push(RepoRule {
            repo: REPO.into(),
            mode: LandMode::Merge,
            method: Some(MergeMethod::Squash),
        })
    });
    // A clone of the change's repository with a worktree for its branch.
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    git(
        &main,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    std::fs::write(main.join("a.txt"), "a").unwrap();
    git(&main, &["add", "a.txt"]);
    git(&main, &["commit", "-q", "-m", "one"]);
    let wt = dir.path().join("wt");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat-x",
            &wt.to_string_lossy(),
        ],
    );
    assert_eq!(
        crate::gates::repo_of(&wt).as_deref(),
        Some(REPO),
        "a worktree's repository is its main clone's"
    );
    t.set_path(&main);
    let after_land = json!({"kind": "after_land", "verdict": "done", "summary": "closed the item", "followUps": ["File the docs item"]});
    assert!(t
        .rep(after_land.clone())
        .unwrap_err()
        .contains("no task in this chat has finished landing"));
    t.trees.set(A, "t1");
    t.approved_plan();
    t.build(A).unwrap();
    t.rep(json!({"kind": "code_review", "repo": REPO, "branch": "feat-x", "tip": A, "base": "main", "verdict": "lgtm", "summary": "r",
        "followUps": ["Rename the helper", "Rename the helper", "Add a test for empty input"]}))
        .unwrap();
    assert_eq!(t.events("followups_queued"), 1);
    t.wf(json!({"action": "hand_off", "summary": "done"}))
        .unwrap();
    // The land step merges it.
    let before =
        t.db.write::<_, rusqlite::Error>(|tx| {
            let run = store::run(
                tx,
                &tx.query_row("SELECT id FROM workflow_runs", [], |r| {
                    r.get::<_, String>(0)
                })?,
            )?
            .unwrap();
            let mut c = store::change(tx, &run.id, REPO, "feat-x")?.unwrap();
            crate::engine::move_change(tx, &run, &mut c, "landing", false, 20)?.unwrap();
            crate::land::apply(tx, &c, &crate::land::Step::Landed(B.into()), 20)?;
            Ok(c)
        })
        .unwrap();
    let _ = crate::notes::take_pending();
    crate::postland::after_step(&t.db, &before, 21).unwrap();
    assert!(!wt.exists(), "the merged, clean, unused worktree is gone");
    assert!(main.join("a.txt").exists());
    assert_eq!(t.count("SELECT count(*) FROM workflow_events WHERE kind = 'stage_report' AND verdict = 'worktree_removed'"), 1);
    assert_eq!(t.events("stage_due"), 1);
    let notes = crate::notes::take_pending();
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].text.contains("finished landing")
            && notes[0].text.contains("removed the worktree"),
        "{}",
        notes[0].text
    );
    // The orchestrator sees the queue and the due post-land stage; it discards one follow-up.
    let s = t.wf(json!({"action": "status"})).unwrap();
    assert_eq!(s["followUps"]["total"], 2, "duplicates are skipped: {s}");
    assert_eq!(s["postLand"]["due"].as_array().unwrap().len(), 1);
    let id = s["followUps"]["queued"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    t.wf(json!({"action": "drop", "queued": id})).unwrap();
    assert!(t.wf(json!({"action": "drop", "queued": id})).is_err());
    // One post-land report settles it and queues its own follow-ups.
    let out = t.rep(after_land.clone()).unwrap();
    assert_eq!(out["queued"], 1);
    assert!(t.rep(after_land).is_err(), "nothing is due any more");
    let s = t.wf(json!({"action": "status"})).unwrap();
    assert!(s.get("postLand").is_none());
    assert_eq!(s["followUps"]["total"], 2);
    // Running it again changes nothing: the due row is once per attempt.
    crate::postland::after_step(&t.db, &before, 22).unwrap();
    assert_eq!(t.events("stage_due"), 1);
}
