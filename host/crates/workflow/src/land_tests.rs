//! The land step against a scripted `gh` (no network, no real PR).

use crate::gh::{Gh, GhError, Out};
use crate::land::{self, Flight};
use crate::store::{self, Change};
use crate::types::*;
use helm_core::db::{Database, MigrationRegistry};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

const TIP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA: &str = "ffffffffffffffffffffffffffffffffffffffff";
const PR: &str = "https://github.com/o/r/pull/7";

/// Answers each needle from its own queue; the last answer repeats.
#[derive(Default)]
struct Script {
    answers: Mutex<HashMap<&'static str, VecDeque<Result<Out, GhError>>>>,
    calls: Mutex<Vec<String>>,
}

impl Script {
    fn say(&self, needle: &'static str, ok: bool, stdout: &str) -> &Self {
        self.answers
            .lock()
            .unwrap()
            .entry(needle)
            .or_default()
            .push_back(Ok(Out {
                ok,
                stdout: stdout.into(),
                stderr: if ok { String::new() } else { stdout.into() },
            }));
        self
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Gh for Script {
    fn run(&self, args: &[String]) -> Result<Out, GhError> {
        let line = args.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        let mut a = self.answers.lock().unwrap();
        for (needle, q) in a.iter_mut() {
            if line.contains(needle) {
                return if q.len() > 1 {
                    q.pop_front().unwrap()
                } else {
                    q.front().cloned().unwrap()
                };
            }
        }
        Ok(Out {
            ok: false,
            stdout: String::new(),
            stderr: format!("unscripted: {line}"),
        })
    }
}

fn pr_json(state: &str, head: &str, base: &str, mergeable: &str, draft: bool) -> String {
    json!({"state": state, "isDraft": draft, "headRefOid": head, "baseRefName": base, "mergeable": mergeable,
           "mergeStateStatus": if mergeable == "UNKNOWN" { "UNKNOWN" } else { "CLEAN" }, "mergeCommit": {"oid": SHA}})
    .to_string()
}

struct T {
    _d: tempfile::TempDir,
    db: Database,
}

fn setup(enabled: bool, mode: LandMode, state: &str) -> T {
    let mut reg = MigrationRegistry::new();
    helm_consent::register_migrations(&mut reg);
    crate::register_migrations(&mut reg);
    let d = tempfile::tempdir().unwrap();
    let db = Database::open(&d.path().join("t.sqlite3"), &reg).unwrap();
    db.write::<_, rusqlite::Error>(|tx| {
        tx.execute("INSERT INTO projects(id, name, path, created_at) VALUES ('p', 'P', 'C:/w', 1)", [])?;
        tx.execute("INSERT INTO chats(id, project_id, title, created_at, updated_at) VALUES ('c', 'p', 'T', 1, 1)", [])?;
        let mut s = WorkflowSettings { enabled, new_chats: NewChats::Always, ..Default::default() };
        s.landing.repos.push(RepoRule { repo: "github.com/o/r".into(), mode, method: (mode == LandMode::Merge).then_some(MergeMethod::Squash) });
        s.landing.draft_labels = vec!["needs-review".into()];
        crate::settings::store(tx, &s, 1)?;
        store::insert_run(tx, "r1", "c", "Task", None, 1)?;
        let c = Change {
            run_id: "r1".into(),
            repo: "github.com/o/r".into(),
            branch: "feat-x".into(),
            pr: Some(PR.into()),
            base: Some("main".into()),
            tip: Some(TIP.into()),
            approved_tip: Some(TIP.into()),
            approved_base: Some("main".into()),
            state: state.into(),
            handoff_attempt: Some(1),
            ..Default::default()
        };
        store::upsert_change(tx, &c, 1)
    })
    .unwrap();
    T { _d: d, db }
}

impl T {
    fn change(&self) -> Change {
        self.db
            .snapshot(|tx| store::change(tx, "r1", "github.com/o/r", "feat-x"))
            .unwrap()
            .unwrap()
    }
    fn pass(&self, gh: &dyn Gh, now: i64) {
        land::pass(&self.db, gh, &Flight::default(), now).unwrap();
    }
}

#[test]
fn a_merge_is_pinned_to_the_approved_head_and_counts_only_on_a_merged_readback() {
    let t = setup(true, LandMode::Merge, "pending_land");
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "MERGEABLE", true),
    )
    .say(
        "pr view",
        true,
        &pr_json("MERGED", TIP, "main", "MERGEABLE", false),
    );
    gh.say("pr ready", true, "").say("pr merge", true, "");
    t.pass(&gh, 1000);
    let c = t.change();
    assert_eq!(c.state, "landed");
    assert_eq!(c.landed_sha.as_deref(), Some(SHA));
    let calls = gh.calls();
    assert!(
        calls.iter().any(|l| l.starts_with("pr ready 7 --repo o/r")),
        "a draft is marked ready first: {calls:?}"
    );
    let merge = calls.iter().find(|l| l.starts_with("pr merge")).unwrap();
    assert!(
        merge.contains("--squash") && merge.contains(&format!("--match-head-commit {TIP}")),
        "{merge}"
    );
}

#[test]
fn a_moved_head_a_wrong_base_or_a_conflict_fails_without_merging() {
    for (json, why) in [
        (
            pr_json("OPEN", SHA, "main", "MERGEABLE", false),
            "head moved",
        ),
        (
            pr_json("OPEN", TIP, "develop", "MERGEABLE", false),
            "not the approved",
        ),
        (
            pr_json("OPEN", TIP, "main", "CONFLICTING", false),
            "conflicts",
        ),
        (pr_json("CLOSED", TIP, "main", "MERGEABLE", false), "closed"),
    ] {
        let t = setup(true, LandMode::Merge, "pending_land");
        let gh = Script::default();
        gh.say("pr view", true, &json);
        t.pass(&gh, 1000);
        let c = t.change();
        assert_eq!(
            (c.state.as_str(), c.failed_mode.as_deref()),
            ("land_failed", Some("merge")),
            "{why}"
        );
        assert!(c.error.as_deref().unwrap().contains(why), "{:?}", c.error);
        assert!(
            !gh.calls().iter().any(|l| l.starts_with("pr merge")),
            "nothing merged for {why}"
        );
    }
}

#[test]
fn a_merge_that_does_not_read_back_merged_is_a_failure() {
    let t = setup(true, LandMode::Merge, "pending_land");
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "MERGEABLE", false),
    );
    gh.say("pr merge", false, "GraphQL: Head branch was modified");
    t.pass(&gh, 1000);
    assert_eq!(t.change().state, "land_failed");
}

#[test]
fn unknown_mergeability_waits_then_goes_to_awaiting_merge_after_fifteen_minutes() {
    let t = setup(true, LandMode::Merge, "pending_land");
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "UNKNOWN", false),
    );
    t.pass(&gh, 0);
    assert_eq!(t.change().state, "landing");
    let views = || {
        gh.calls()
            .iter()
            .filter(|l| l.starts_with("pr view"))
            .count()
    };
    let n = views();
    t.pass(&gh, 5_000);
    assert_eq!(views(), n, "not due again before 10 s");
    t.pass(&gh, 10_000);
    assert_eq!(views(), n + 1);
    t.pass(&gh, land::BUDGET_MS + 1);
    let c = t.change();
    assert_eq!(c.state, "awaiting_merge");
    assert!(c.error.unwrap().contains("15 minutes"));
    assert!(!gh.calls().iter().any(|l| l.starts_with("pr merge")));
}

#[test]
fn the_draft_land_labels_the_pr_and_leaves_a_ready_pr_alone() {
    let t = setup(true, LandMode::Draft, "landing_draft");
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "MERGEABLE", true),
    )
    .say("pr edit", true, "");
    t.pass(&gh, 1000);
    assert_eq!(t.change().state, "handed_off");
    assert!(gh
        .calls()
        .iter()
        .any(|l| l.contains("--add-label needs-review")));
    let t = setup(true, LandMode::Draft, "landing_draft");
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "MERGEABLE", false),
    );
    t.pass(&gh, 1000);
    let c = t.change();
    assert_eq!(c.state, "handed_off");
    assert!(c.error.unwrap().contains("already ready"));
    assert!(
        !gh.calls()
            .iter()
            .any(|l| l.starts_with("pr edit") || l.starts_with("pr ready")),
        "never relabelled or un-readied"
    );
}

#[test]
fn the_mode_is_the_hosts_and_nothing_runs_with_the_switch_off() {
    // Handed off as a merge, but the repository's rule is now draft: no merge.
    let t = setup(true, LandMode::Draft, "pending_land");
    let gh = Script::default();
    t.pass(&gh, 1000);
    let c = t.change();
    assert_eq!(
        (c.state.as_str(), c.failed_mode.as_deref()),
        ("land_failed", Some("draft"))
    );
    assert!(gh.calls().is_empty());
    let off = setup(false, LandMode::Merge, "pending_land");
    off.pass(&gh, 1000);
    assert_eq!(off.change().state, "pending_land");
}

#[test]
fn a_merge_in_flight_at_quit_becomes_a_failed_land_and_the_user_acts_on_it() {
    let t = setup(true, LandMode::Merge, "landing");
    assert_eq!(land::recover(&t.db, 5).unwrap(), 1);
    let c = t.change();
    assert_eq!(
        (c.state.as_str(), c.failed_mode.as_deref()),
        ("land_failed", Some("merge"))
    );
    // Retry puts it back to landing; Decline is only for an awaiting change.
    let r =
        t.db.write(|tx| land::user_action(tx, "r1", "github.com/o/r", "feat-x", "decline", 6))
            .unwrap();
    assert!(r.is_err());
    let r =
        t.db.write(|tx| land::user_action(tx, "r1", "github.com/o/r", "feat-x", "retry", 7))
            .unwrap();
    assert_eq!(r.unwrap(), "landing");
    let r =
        t.db.write(|tx| land::user_action(tx, "r1", "github.com/o/r", "feat-x", "drop", 8))
            .unwrap();
    assert!(
        r.unwrap_err().contains("being merged"),
        "never dropped mid-land"
    );
}

#[test]
fn the_agent_lands_an_awaiting_change_only_with_the_users_consent() {
    let t = setup(true, LandMode::Merge, "awaiting_merge");
    let mut kinds = helm_consent::AllowKinds::new();
    crate::kinds::register_allow_kinds(&mut kinds);
    let args = json!({"action": "land", "repo": "github.com/o/r", "branch": "feat-x"});
    let e = crate::wftools::workflow_call(&t.db, &kinds, "c", false, &args, 10)
        .unwrap_err()
        .text();
    assert!(e.contains("Merge #7."), "{e}");
    assert!(e.contains("app_request_allow"), "{e}");
    assert_eq!(
        t.change().state,
        "awaiting_merge",
        "nothing moved without consent"
    );
}

#[test]
fn a_retry_after_the_repo_switched_to_draft_never_merges() {
    let t = setup(true, LandMode::Merge, "landing");
    land::recover(&t.db, 1).unwrap();
    t.db.write::<_, rusqlite::Error>(|tx| {
        let mut s = crate::settings::load(tx)?;
        s.landing.repos[0] = RepoRule {
            repo: "github.com/o/r".into(),
            mode: LandMode::Draft,
            method: None,
        };
        crate::settings::store(tx, &s, 2)
    })
    .unwrap();
    let r =
        t.db.write(|tx| land::user_action(tx, "r1", "github.com/o/r", "feat-x", "retry", 3))
            .unwrap();
    assert_eq!(r.unwrap(), "landing");
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "MERGEABLE", false),
    );
    t.pass(&gh, 1000);
    let c = t.change();
    assert_eq!(
        (c.state.as_str(), c.failed_mode.as_deref()),
        ("land_failed", Some("draft"))
    );
    assert!(gh.calls().is_empty(), "no gh call at all: {:?}", gh.calls());
}

#[test]
fn an_accepted_merge_that_has_not_shown_yet_waits_and_a_foreign_pr_url_fails() {
    let t = setup(true, LandMode::Merge, "pending_land");
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "MERGEABLE", false),
    )
    .say("pr merge", true, "");
    t.pass(&gh, 1000);
    assert_eq!(
        t.change().state,
        "landing",
        "waits for the readback instead of failing"
    );
    let t = setup(true, LandMode::Merge, "pending_land");
    t.db.write::<_, rusqlite::Error>(|tx| {
        tx.execute(
            "UPDATE workflow_changes SET pr = 'https://github.com/evil/fork/pull/7'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let gh = Script::default();
    t.pass(&gh, 1000);
    assert_eq!(t.change().state, "land_failed");
    assert!(gh.calls().is_empty());
}

#[test]
fn a_pass_that_writes_marks_its_chat_and_a_waiting_pass_does_not() {
    let t = setup(true, LandMode::Merge, "landing");
    let _ = store::take_touched();
    let gh = Script::default();
    // GitHub still computing mergeability: only the wait row is written, so no event.
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "UNKNOWN", false),
    );
    t.pass(&gh, 10);
    assert_eq!(t.change().state, "landing");
    assert!(
        store::take_touched().is_empty(),
        "a wait is not a run write"
    );
    // A head that moved fails the land: a change write, so the chat is marked.
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", SHA, "main", "MERGEABLE", false),
    );
    t.pass(&gh, 10 + land::FAST_WAIT_MS);
    assert_eq!(t.change().state, "land_failed");
    assert_eq!(store::take_touched(), ["c"]);
}

// ---- the PR view's buttons and the fast-forward method ---------------------------------------

fn pr_view_json(state: &str, head: &str, draft: bool, head_ref: Option<&str>) -> String {
    json!({"state": state, "isDraft": draft, "headRefOid": head, "baseRefName": "main",
           "headRefName": head_ref.unwrap_or("someone-else"), "isCrossRepository": head_ref.is_none(),
           "mergeCommit": if state == "MERGED" { json!({"oid": SHA}) } else { json!(null) }})
    .to_string()
}

impl T {
    fn set_rule(&self, mode: Option<LandMode>, method: Option<MergeMethod>) {
        self.db
            .write::<_, rusqlite::Error>(|tx| {
                let mut s = crate::settings::load(tx)?;
                s.landing.repos.clear();
                if let Some(mode) = mode {
                    s.landing.repos.push(RepoRule {
                        repo: "github.com/o/r".into(),
                        mode,
                        method,
                    });
                }
                crate::settings::store(tx, &s, 2)
            })
            .unwrap();
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
}

#[derive(Clone, Copy)]
enum Click {
    Merge {
        n: u64,
        sha: &'static str,
        anyway: bool,
    },
    Ready {
        anyway: bool,
    },
    Draft,
}

type Rule = Option<(LandMode, Option<MergeMethod>)>;

struct Case {
    name: &'static str,
    /// Settings → Orchestrator on.
    on: bool,
    state: &'static str,
    /// The repository's own rule; `None` for no rule (then `landing.method`, merge here).
    rule: Rule,
    click: Click,
    /// The PR as GitHub reads it: state, head, draft, head branch when in the same repository.
    pr: (&'static str, &'static str, bool, Option<&'static str>),
    /// More scripted answers; `MERGED` / `OPEN` stand for a read-back of the PR in that state.
    say: &'static [(&'static str, bool, &'static str)],
    /// `Ok(outcome)` or `Err(class)`.
    want: Result<PrOutcome, &'static str>,
    change: &'static str,
    must: &'static [&'static str],
    never: &'static [&'static str],
    override_row: bool,
}

const SQUASH: Rule = Some((LandMode::Merge, Some(MergeMethod::Squash)));
const FF: Rule = Some((LandMode::Merge, Some(MergeMethod::FastForward)));
const DRAFT: Rule = Some((LandMode::Draft, None));
const MERGE7: Click = Click::Merge {
    n: 7,
    sha: TIP,
    anyway: false,
};
const ANYWAY7: Click = Click::Merge {
    n: 7,
    sha: TIP,
    anyway: true,
};
const MERGE8: Click = Click::Merge {
    n: 8,
    sha: TIP,
    anyway: false,
};
const OPEN_PR: (&str, &str, bool, Option<&str>) = ("OPEN", TIP, false, None);

const BASE: Case = Case {
    name: "",
    on: true,
    state: "open",
    rule: SQUASH,
    click: MERGE7,
    pr: OPEN_PR,
    say: &[],
    want: Err("[Tracked]"),
    change: "open",
    must: &[],
    never: &["pr merge", "PATCH"],
    override_row: false,
};

#[test]
fn the_pr_view_routes_refuses_overrides_and_obeys_the_repository_rule() {
    use crate::prview::{self, PrRef};
    let cases = [
        Case {
            name: "awaiting the user's Merge at the approved head: handed to the task",
            state: "awaiting_merge",
            want: Ok(PrOutcome::Routed),
            change: "landing",
            ..BASE
        },
        Case {
            name: "awaiting a Merge, but the head is not the approved commit",
            state: "awaiting_merge",
            click: Click::Merge {
                n: 7,
                sha: SHA,
                anyway: false,
            },
            pr: ("OPEN", SHA, false, None),
            change: "awaiting_merge",
            ..BASE
        },
        Case {
            name: "a change still being built holds the PR back",
            ..BASE
        },
        Case {
            name: "the head branch tracks it too when the head is in the same repository",
            click: MERGE8,
            pr: ("OPEN", TIP, false, Some("feat-x")),
            ..BASE
        },
        Case {
            name: "Merge anyway merges by the rule and records the landing on the task",
            click: ANYWAY7,
            say: &[("pr view", true, "MERGED"), ("pr merge", true, "")],
            want: Ok(PrOutcome::Merged),
            change: "landed",
            must: &["--squash --match-head-commit"],
            never: &["PATCH"],
            override_row: true,
            ..BASE
        },
        Case {
            name: "the land step working on it: busy, even with anyway",
            state: "landing",
            click: ANYWAY7,
            want: Err("[TrackedBusy]"),
            change: "landing",
            ..BASE
        },
        Case {
            name: "new commits since the page looked",
            click: ANYWAY7,
            pr: ("OPEN", SHA, false, None),
            want: Err("[HeadMoved]"),
            ..BASE
        },
        Case {
            name: "a draft rule is never waived, not even with anyway",
            rule: DRAFT,
            click: ANYWAY7,
            want: Err("[DraftRule]"),
            ..BASE
        },
        Case {
            name: "an untracked PR in a draft-rule repository",
            rule: DRAFT,
            click: MERGE8,
            want: Err("[DraftRule]"),
            ..BASE
        },
        Case {
            name: "with the workflow off no task holds it back; no rule means landing.method",
            on: false,
            rule: None,
            say: &[("pr view", true, "MERGED"), ("pr merge", true, "")],
            want: Ok(PrOutcome::Merged),
            must: &["pr merge 7 --repo o/r --merge"],
            never: &[],
            ..BASE
        },
        Case {
            name: "a fast-forward is refused when the base is not an ancestor of the head",
            rule: FF,
            click: MERGE8,
            say: &[("compare", true, "diverged\n")],
            want: Err("[NotFastForward]"),
            must: &["api repos/o/r/compare/main...aaaa"],
            ..BASE
        },
        Case {
            name: "a fast-forward moves the base with a non-forced update and waits for MERGED",
            rule: FF,
            click: MERGE8,
            say: &[
                ("compare", true, "ahead\n"),
                ("PATCH", true, "{}"),
                ("pr view", true, "OPEN"),
                ("pr view", true, "MERGED"),
            ],
            want: Ok(PrOutcome::Merged),
            must: &["api -X PATCH repos/o/r/git/refs/heads/main -f sha=aaaa"],
            never: &["pr merge"],
            ..BASE
        },
        Case {
            name: "GitHub refusing a non-fast-forward update leaves nothing merged",
            rule: FF,
            click: MERGE8,
            say: &[
                ("compare", true, "ahead\n"),
                ("PATCH", false, "Update is not a fast forward (HTTP 422)"),
            ],
            want: Err("[NotFastForward]"),
            must: &["force=false"],
            never: &["pr merge"],
            ..BASE
        },
        Case {
            name: "Ready for review on a change handed off as a draft runs the task's own flip",
            state: "handed_off",
            rule: DRAFT,
            click: Click::Ready { anyway: false },
            pr: ("OPEN", TIP, true, None),
            say: &[("pr ready", true, "")],
            want: Ok(PrOutcome::Routed),
            change: "flipped",
            must: &["pr ready 7 --repo o/r"],
            never: &[],
            ..BASE
        },
        Case {
            name: "Mark ready anyway on a change still being built leaves the change as it is",
            click: Click::Ready { anyway: true },
            pr: ("OPEN", TIP, true, None),
            say: &[("pr ready", true, "")],
            want: Ok(PrOutcome::Ready),
            must: &["pr ready 7 --repo o/r"],
            never: &[],
            override_row: true,
            ..BASE
        },
        Case {
            name: "Convert to draft is never refused as tracked",
            state: "awaiting_merge",
            click: Click::Draft,
            say: &[("pr ready", true, "")],
            want: Ok(PrOutcome::Tracked),
            change: "awaiting_merge",
            must: &["pr ready 7 --repo o/r --undo"],
            never: &[],
            ..BASE
        },
        Case {
            name: "a closed PR is refused before anything",
            click: Click::Draft,
            pr: ("CLOSED", TIP, false, None),
            want: Err("[Closed]"),
            never: &["pr ready"],
            ..BASE
        },
    ];
    for c in cases {
        let t = setup(c.on, LandMode::Merge, c.state);
        t.set_rule(c.rule.map(|r| r.0), c.rule.and_then(|r| r.1));
        let gh = Script::default();
        gh.say(
            "pr view",
            true,
            &pr_view_json(c.pr.0, c.pr.1, c.pr.2, c.pr.3),
        );
        for (needle, ok, out) in c.say {
            let out = match *out {
                "MERGED" | "OPEN" => pr_view_json(out, c.pr.1, false, c.pr.3),
                o => o.to_owned(),
            };
            gh.say(needle, *ok, &out);
        }
        let flight = Flight::default();
        let cx = prview::Ctx {
            db: &t.db,
            gh: &gh,
            flight: &flight,
            pause: std::time::Duration::ZERO,
            now: 50,
        };
        let pr = |n| PrRef::new("o", "r", n).unwrap();
        let got = match c.click {
            Click::Merge { n, sha, anyway } => prview::merge(&cx, &pr(n), sha, anyway),
            Click::Ready { anyway } => prview::ready(&cx, &pr(7), anyway),
            Click::Draft => prview::draft(&cx, &pr(7)),
        };
        match (&got, &c.want) {
            (Ok(r), Ok(o)) => assert_eq!(r.outcome, *o, "{}", c.name),
            (Err(e), Err(class)) => assert!(e.starts_with(class), "{}: {e}", c.name),
            _ => panic!("{}: got {got:?}", c.name),
        }
        assert_eq!(t.change().state, c.change, "{}", c.name);
        let calls = gh.calls().join("\n");
        for m in c.must {
            assert!(calls.contains(m), "{}: {m} missing from\n{calls}", c.name);
        }
        for m in c.never {
            assert!(!calls.contains(m), "{}: {m} must not run\n{calls}", c.name);
        }
        assert_eq!(
            t.events("pr_view_override"),
            c.override_row as i64,
            "{}",
            c.name
        );
        if c.override_row {
            let r = got.as_ref().unwrap();
            assert!(r.overridden && r.record_error.is_none(), "{}", c.name);
            let needs = t.db.snapshot(|tx| crate::hook::needs_you(tx, "c")).unwrap();
            assert_eq!(needs.len(), 1, "{}: one Needs you row", c.name);
            assert_eq!((needs[0].gate.as_str(), needs[0].overridden), ("pr", true));
            assert!(needs[0].command.ends_with("o/r#7"), "{:?}", needs[0]);
        }
        if let Err(e) = &got {
            if e.starts_with("[Tracked]") {
                assert!(
                    e.contains("\"Task\"") && e.contains("anyway"),
                    "{}: the refusal names the task and the way through: {e}",
                    c.name
                );
            }
        }
    }
}

#[test]
fn the_land_step_fast_forwards_only_when_the_repository_rule_says_so() {
    // Lands: the comparison says ahead, a non-forced ref update, then the MERGED read-back.
    let t = setup(true, LandMode::Merge, "pending_land");
    t.set_rule(Some(LandMode::Merge), Some(MergeMethod::FastForward));
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "MERGEABLE", false),
    )
    .say(
        "pr view",
        true,
        &pr_json("MERGED", TIP, "main", "MERGEABLE", false),
    )
    .say("compare", true, "ahead")
    .say("PATCH", true, "{}");
    t.pass(&gh, 1000);
    assert_eq!(t.change().state, "landed");
    let calls = gh.calls();
    let patch = format!("api -X PATCH repos/o/r/git/refs/heads/main -f sha={TIP} -F force=false");
    assert!(calls.contains(&patch), "{calls:?}");
    assert!(!calls.iter().any(|l| l.starts_with("pr merge")));
    // Refused before any write when the base has moved on.
    let t = setup(true, LandMode::Merge, "pending_land");
    t.set_rule(Some(LandMode::Merge), Some(MergeMethod::FastForward));
    let gh = Script::default();
    gh.say(
        "pr view",
        true,
        &pr_json("OPEN", TIP, "main", "MERGEABLE", false),
    )
    .say("compare", true, "diverged");
    t.pass(&gh, 1000);
    let c = t.change();
    assert_eq!(c.state, "land_failed");
    assert!(c.error.unwrap().contains("fast-forwarded"));
    assert!(!gh.calls().iter().any(|l| l.contains("PATCH")));
    // landing.method is never a fast-forward: only a repository's own rule picks it.
    assert!(crate::settings::validate(json!({"landing": {"method": "fast_forward"}})).is_err());
}
