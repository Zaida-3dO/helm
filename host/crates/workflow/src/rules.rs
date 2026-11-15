//! The generated Workflow rules and the Move fast block.
//!
//! Built sentence by sentence from the saved settings, so a setting change alters only its own
//! sentence. The text holds only fixed sentences, validated agent names, branch names and
//! integers. It **grants nothing**: every gate is the host's own check whatever the prompt says.

use crate::types::*;

pub const HEADING: &str = "Workflow rules (set by the user in Settings > Orchestrator and enforced by Helm; Helm refuses any step that breaks them):";

pub const MOVE_FAST: &str = "Move fast is on. Tell every coder and reviewer to test only what the change touches: the tests of the touched module, the one check script involved, and a type check when typed code moved. No whole-project build or test run, no UI checks, golden images or smoketests, unless Helm itself requires a smoketest (a visual task, or Smoketest set to required). Reviewers read the diff and run no more than those same tests. When one feature is split across a series of PRs, the earlier PRs may land on targeted tests alone; the full build, the full suites, UI checks and any smoketest still owed belong to the last PR of the series. Otherwise rely on a full-check lane: one chat, which the user names, builds main and runs every suite after 10 merges or 2 hours, whichever comes first, and any failure gets an immediate fix-forward PR from the most likely culprit; a briefly red main is fine. If there is no full-check lane and the user has not said there is one, mention it to the user once rather than starting one. Move fast changes only how much testing is done: every other rule here still applies, and so does every step that needs the user's consent.";

pub const CONSENT: &str = "When Helm turns a step down, read the reason and take the next allowed step instead of stopping. If the step needs the user's permission, keep going in one of two ways: ask the user to send the one-line sentence Helm names and pass their words, unchanged, as userQuote; or call app_request_allow {action, target, reason} (not in your tool list: call it with app_mcp_call; app_help helm/consent) and, once the user allows it, pass grant: <requestId>. Never claim consent the user did not give.";

pub const AUTO: &str = "Orchestrator is on Auto for this chat. If the user's request is a code change that should run under the orchestrator workflow (planned, reviewed, built by agents and landed by Helm), call app_orchestrator_on {reason} (not in your tool list: call it with app_mcp_call) with one plain sentence saying why; it takes effect from your next turn. For questions and small, direct tasks, just do the work.";

fn agent(a: &str) -> String {
    format!("`{a}`")
}

fn names(list: &[String]) -> String {
    let quoted: Vec<String> = list.iter().map(|a| agent(a)).collect();
    match quoted.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The Workflow rules for these settings (with the Move fast block last when it is on).
fn steps(s: &WorkflowSettings) -> Vec<(&'static str, String)> {
    let st = &s.stages;
    let mut out: Vec<(&'static str, String)> = vec![("heading", HEADING.into())];
    let mut step = "start";
    fn put(out: &mut Vec<(&'static str, String)>, step: &'static str, text: String) {
        out.push((step, text));
    }
    put(&mut out, step, "You are the orchestrator: you build nothing yourself. Start a task with app_workflow {action: \"start\", title}, read where it stands with {action: \"status\"}, and park it with {action: \"park\", reason}.".into());
    step = "plan";
    match st.plan.use_ {
        StageUse::Required => put(&mut out, step, format!("Every task needs a plan before any build. Have {} write it, then adopt its text with app_workflow {{action: \"plan\", text}}; the adopted text is the plan of record.", agent(&st.plan.agent))),
        StageUse::Auto => put(&mut out, step, format!("A task needs a plan before any build unless you skip it with app_workflow {{action: \"skip\", stage: \"plan\", reason}}. Have {} write it and adopt its text with app_workflow {{action: \"plan\", text}}.", agent(&st.plan.agent))),
        StageUse::Off => put(&mut out, step, "A plan is up to you; adopting one with app_workflow {action: \"plan\", text} makes it the plan of record.".into()),
    }
    if st.plan_review.use_ != StageUse::Off {
        let skip = if st.plan_review.use_ == StageUse::Auto {
            " You may skip plan review with a stated reason."
        } else {
            ""
        };
        put(&mut out, step, format!(
            "A fresh {} agent must approve that exact plan; a changed plan needs a new review. At most {} plan-review rounds per plan.{skip}",
            agent(&st.plan_review.agent),
            st.plan_review.max_rounds
        ));
    }
    if s.plan_sign_off == SignOff::Me {
        put(
            &mut out,
            step,
            "After the plan is approved, the user signs it off before any build starts.".into(),
        );
    }
    step = "build";
    let exempt = if st.build.build_exempt.is_empty() {
        String::new()
    } else {
        format!(" ({} are not builds)", names(&st.build.build_exempt))
    };
    put(&mut out, step, format!(
        "Builds are done by {}; during a task any other agent you dispatch counts as a build{exempt}. Only you dispatch stage agents, and record each change with app_workflow {{action: \"update\", repo, branch, pr?}}.",
        agent(&st.build.agent)
    ));
    put(&mut out, step, "Don't plan or build yourself: brief each stage agent with file paths, have it hand off through files, and read its short report rather than its files. Keep one piece of work on one branch and one PR: coders write code and merge into it, and the finished branch goes to review once, with every finding in one fix list for one coder to apply.".into());
    put(&mut out, step, match s.orchestrator_edits {
        EditsRule::Deny => "Do not edit files in a task's repositories yourself; Helm refuses those edits during a task. If one file is genuinely yours (a plan or a note, not the change's code), grant it with app_workflow {action: \"allow_edit\", file, reason}.".into(),
        EditsRule::Allow => "You may edit files in a task's repositories yourself.".into(),
    });
    step = "review";
    if st.review.use_ == StageUse::Off {
        put(&mut out, step, "Code review is off.".into());
    } else {
        put(&mut out, step, format!(
            "A fresh {} agent reviews each change at its exact latest commit; an approval is recorded for that commit (or a commit with an identical tree). At most {} review rounds per change. Once a review has run, Helm lands the change even if its head later moves: it nudges you about that instead of blocking, and a second review is your call, with a high bar.",
            agent(&st.review.agent),
            st.review.max_rounds
        ));
    }
    match st.cleanup.use_ {
        StageUse::Off => {}
        u => put(&mut out, step, format!(
            "After approval, {} tidies the PR without changing the code: the tree must stay identical.{}",
            agent(&st.cleanup.agent),
            if u == StageUse::Auto { " You may skip clean-up with a stated reason." } else { "" }
        )),
    }
    put(
        &mut out,
        step,
        match st.smoketest.use_ {
            StageUse::Required => format!(
                "Every change needs a smoketest pass from {} at its latest commit.",
                agent(&st.smoketest.agent)
            ),
            StageUse::Auto => format!(
                "A smoketest by {} is your call, except that a task marked visual needs a pass.",
                agent(&st.smoketest.agent)
            ),
            StageUse::Off => {
                "Smoketests are off, except that a task marked visual needs a pass.".into()
            }
        },
    );
    put(&mut out, step, "Tell each stage agent to report its verdict with app_workflow_report; only that report counts, never its reply text. A verdict is final: a reviewer that has reported is not asked again (Helm refuses the message); a fresh agent takes the next round. app_workflow status lists a roundLines entry for each review round that can still run: put its line, unchanged, at the top of that agent's brief.".into());
    if st.plan_review.use_ != StageUse::Off
        || st.review.use_ != StageUse::Off
        || st.smoketest.use_ != StageUse::Off
    {
        put(&mut out, step, "A cap is a limit, not a goal: stop a loop as soon as it is approved. When a plan or change is at its cap you may give it one more round yourself with app_workflow {action: \"allowMore\", stage, reason}; past that only the user can.".into());
    }
    step = "land";
    let protected = if s.landing.protected_branches.is_empty() {
        "a protected branch".to_owned()
    } else {
        format!(
            "a protected branch ({} or a default branch)",
            s.landing.protected_branches.join(", ")
        )
    };
    put(&mut out, step, format!(
        "If the task has a board item, record the plan and the reviews on it. Create PRs as drafts. When every change holds, call app_workflow {{action: \"hand_off\"}}, and Helm lands each change by its repository's rule. Never mark a PR ready, merge it, push onto {protected} or remove a review label yourself. Never run a round just to use it up."
    ));
    put(&mut out, step, crate::outage_rules::BOARD_OUTAGE.into());
    put(&mut out, step, "One change belongs to one task: Helm refuses a branch another task holds and names it; app_workflow {action: \"take_over\", repo, branch, reason} moves it into this task (check: true only reports). When a task finishes landing, Helm removes its merged worktrees and tells you; status lists the follow-ups reviewers queued (discard one with {action: \"drop\", queued}).".into());
    step = "consent";
    put(&mut out, step, CONSENT.into());
    if s.move_fast {
        step = "fast";
        put(&mut out, step, MOVE_FAST.into());
    }
    let _ = step;
    out
}

/// The Workflow rules for these settings (with the Move fast block last when it is on).
pub fn workflow_rules(s: &WorkflowSettings) -> String {
    steps(s)
        .into_iter()
        .map(|(_, t)| t)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One line for each step's help topic.
pub const STEP_SUMMARIES: &[(&str, &str)] = &[
    ("start", "Starting, checking and parking a task."),
    ("plan", "The plan, its review and its sign-off."),
    (
        "build",
        "Who builds, how to brief them and whether you may edit.",
    ),
    (
        "review",
        "Code review, clean-up, smoketests, reports and round caps.",
    ),
    (
        "land",
        "Board records, the hand-off, landing and taking a change over.",
    ),
    ("consent", "What to do when Helm turns a step down."),
    ("fast", "What Move fast changes."),
];

/// What the prompt carries while the workflow is on: the heading, the start sentence and where
/// each step's rules are. The steps themselves are read with `app_help` when their turn comes.
pub fn workflow_index(s: &WorkflowSettings) -> String {
    let all = steps(s);
    let heading = all.first().map(|(_, t)| t.as_str()).unwrap_or(HEADING);
    let start = all
        .iter()
        .find(|(k, _)| *k == "start")
        .map(|(_, t)| t.as_str())
        .unwrap_or_default();
    let topics: Vec<String> = all
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| *k != "heading" && *k != "start")
        .fold(Vec::new(), |mut v, k| {
            if !v.contains(&k) {
                v.push(k);
            }
            v
        })
        .into_iter()
        .map(|k| format!("workflow/{k}"))
        .collect();
    format!(
        "{heading} {start} The rules of each step are in app_help: {}. Read a step's topic before you start that step; Helm refuses a step that breaks them and says why.",
        topics.join(", ")
    )
}

/// The text of one step's help topic, or `None` when the settings have no such step.
pub fn step_text(s: &WorkflowSettings, step: &str) -> Option<String> {
    let parts: Vec<String> = steps(s)
        .into_iter()
        .filter(|(k, _)| *k == step)
        .map(|(_, t)| t)
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The steps these settings have, in order.
pub fn step_names(s: &WorkflowSettings) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for (k, _) in steps(s) {
        if k != "heading" && !v.contains(&k) {
            v.push(k);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rules_follow_the_settings_sentence_by_sentence() {
        let d = WorkflowSettings::default();
        let r = workflow_rules(&d);
        assert!(r.starts_with(HEADING));
        assert!(r.contains("At most 2 review rounds per change."));
        assert!(r.contains("`coder`"));
        assert!(r.contains("Don't plan or build yourself"));
        // The prompt carries an index; each step's rules are a topic, and together they are the rules.
        let index = workflow_index(&d);
        assert!(index.starts_with(HEADING) && index.contains("workflow/plan, workflow/build, workflow/review, workflow/land, workflow/consent"));
        assert!(
            index.len() < 900 && !index.contains("At most 2 review rounds"),
            "{}",
            index.len()
        );
        let names = step_names(&d);
        assert_eq!(
            names,
            ["start", "plan", "build", "review", "land", "consent"]
        );
        assert!(step_text(&d, "review")
            .unwrap()
            .contains("At most 2 review rounds per change."));
        assert!(
            step_text(&d, "fast").is_none(),
            "no Move fast block while it is off"
        );
        let joined = names
            .iter()
            .map(|n| step_text(&d, n).unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            format!("{HEADING} {joined}"),
            r,
            "the topics hold every sentence, once"
        );
        assert!(STEP_SUMMARIES.iter().all(|(n, _)| [
            "start", "plan", "build", "review", "land", "consent", "fast"
        ]
        .contains(n)));
        assert!(r.contains("main, master"));
        assert!(!r.contains("Move fast"), "Move fast is off by default");
        assert!(
            r.contains(crate::outage_rules::BOARD_OUTAGE),
            "the board-outage rule is part of the rules"
        );
        assert!(!r.contains("signs it off"));
        assert!(r.contains("roundLines") && r.contains("A verdict is final"));
        let mut s = d.clone();
        s.stages.review.max_rounds = 7;
        s.plan_sign_off = SignOff::Me;
        s.stages.build.build_exempt = vec!["scout".into(), "explore".into()];
        let r2 = workflow_rules(&s);
        assert!(r2.contains("At most 7 review rounds per change."));
        assert!(r2.contains("signs it off"));
        assert!(r2.contains("(`scout` and `explore` are not builds)"));
    }

    #[test]
    fn move_fast_comes_last_and_only_when_on() {
        let s = WorkflowSettings {
            move_fast: true,
            ..Default::default()
        };
        let r = workflow_rules(&s);
        assert!(r.ends_with(MOVE_FAST));
        assert!(MOVE_FAST.contains("10 merges or 2 hours"));
    }

    #[test]
    fn the_caps_sentence_goes_when_every_verdict_stage_is_off() {
        let mut s = WorkflowSettings::default();
        s.stages.plan_review.use_ = StageUse::Off;
        s.stages.review.use_ = StageUse::Off;
        s.stages.smoketest.use_ = StageUse::Off;
        let r = workflow_rules(&s);
        assert!(!r.contains("allowMore"));
        assert!(r.contains("Code review is off."));
    }
}
