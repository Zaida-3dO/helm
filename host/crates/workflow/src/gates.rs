//! The dispatch gate and the orchestrator's edit gate, asked by the worker before an agent spawn
//! or a main-thread file edit.
//!
//! Both act only where the workflow is in effect (master switch on, a Claude Code chat, not
//! archived, Orchestrator on) **and** the chat has an active task. Everywhere else they answer
//! allow, and a chat whose Orchestrator is off at launch does not even ask: the worker registers
//! its hook only when the launch carries [`GATES_ENV`]. Every refusal is soft: it says why and the
//! way through, and each gate has a user-only switch (the dispatch gate's own setting, and
//! Orchestrator edits: allow). A worker that cannot reach the host lets the call through.
//!
//! **Dispatch gate** (`Agent` / `Task`), during a task:
//! - a remote-isolation spawn is refused (its tools run outside the app's checks);
//! - a sub-agent may not spawn a stage agent or a background agent; its other helpers run;
//! - the orchestrator's spawn of a stage agent faces that stage's gate: plan review needs a plan
//!   of record and a round under the cap; review and smoketest are refused when every change that
//!   still needs one is at its cap; clean-up needs a change with a clean-up basis;
//! - any other spawn counts as a build unless its type is build-exempt: it needs the plan
//!   approved (and signed off if configured), and is refused when every unapproved change is at
//!   its review cap. A stage whose use is off is not gated.
//!
//! **Edit gate** (`Write`, `Edit`, `MultiEdit`, `NotebookEdit` on the main thread), during a task
//! with Orchestrator edits on deny: an edit to a file inside the task's repositories (its live
//! changes' repositories, plus the chat folder's unless that is a home repository) is refused,
//! unless the orchestrator granted itself that one file with `allow_edit {file, reason}`. A grant
//! lasts until the task ends (at most 32 per task) and is a visible row. A file's repository is
//! read from its git folder by the host, never taken from the agent.

use crate::actions::{self, need, Act, Out};
use crate::engine::{self, CapStage};
use crate::store::{self, Event, Run};
use crate::types::*;
use crate::{mode, settings};
use helm_core::db::Database;
use helm_hosttools::{Condition, HostToolRegistry, ToolError, ToolSpec};
use helm_workers::contrib::{LaunchContributor, LaunchCtx, LaunchExtras};
use rusqlite::Transaction;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Set on a worker's launch when its chat may be under the gates; the worker's gate hook is
/// registered only then.
pub const GATES_ENV: &str = "HELM_WORKFLOW_GATES";
pub const GATE_TOOL: &str = "app_workflow_gate";
pub const SPAWN_TOOLS: [&str; 2] = ["Agent", "Task"];
pub const EDIT_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];
pub const MAX_GRANTS: i64 = 32;
pub const MAX_FILE: usize = 2000;
pub const MAX_GRANT_REASON: usize = 500;

const OFF_SWITCH: &str = "The user can turn the dispatch gate off in Settings > Orchestrator.";

// ---- paths and repositories -----------------------------------------------------------------

/// `file` made absolute against `cwd` and resolved lexically (`.` and `..`), never touching disk.
pub fn lexical_abs(cwd: Option<&Path>, file: &str) -> Option<PathBuf> {
    let p = Path::new(file);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd?.join(p)
    };
    let mut out = PathBuf::new();
    for c in full.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    (out.is_absolute() && out.to_string_lossy().chars().count() <= MAX_FILE).then_some(out)
}

/// A grant's key: the leading 16 hex digits of the SHA-256 of the path, slashes made forward
/// and, on Windows (whose paths ignore case), case folded.
pub fn path_key(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    hex::encode(Sha256::digest(s.as_bytes()))[..16].to_owned()
}

/// The `host/owner/name` of the repository `path` lies in: the origin remote of the nearest
/// folder with a `.git` (a linked worktree's `.git` file is followed to its main repository).
/// `None` when it is in no repository, or the remote is not one Helm reads.
pub fn repo_of(path: &Path) -> Option<String> {
    let mut dir = Some(path);
    while let Some(d) = dir {
        let dot = d.join(".git");
        if let Ok(m) = std::fs::symlink_metadata(&dot) {
            let main = if m.is_dir() {
                d.to_path_buf()
            } else {
                main_of_worktree(d, &dot)?
            };
            let url = helm_worktrees::config::origin_url(&main)?;
            return helm_worktrees::config::parse_remote(&url).map(|r| r.key());
        }
        dir = d.parent();
    }
    None
}

/// A linked worktree's main checkout: `.git` says `gitdir: <common>/worktrees/<name>`, and that
/// folder's `commondir` points back at the common `.git` folder.
fn main_of_worktree(dir: &Path, dot: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot).ok()?;
    let gitdir = text.trim().strip_prefix("gitdir:")?.trim();
    let gitdir = if Path::new(gitdir).is_absolute() {
        PathBuf::from(gitdir)
    } else {
        dir.join(gitdir)
    };
    let common = std::fs::read_to_string(gitdir.join("commondir"))
        .map(|c| {
            let c = c.trim().to_owned();
            if Path::new(&c).is_absolute() {
                PathBuf::from(c)
            } else {
                gitdir.join(c)
            }
        })
        .unwrap_or(gitdir);
    let common = lexical_abs(None, &common.to_string_lossy())?;
    Some(common.parent()?.to_path_buf())
}

/// The task's repositories: its live changes', plus the chat folder's unless it is a home one.
pub fn task_repos(
    tx: &Transaction<'_>,
    s: &WorkflowSettings,
    run: &Run,
    cwd_repo: Option<&str>,
) -> rusqlite::Result<Vec<String>> {
    let mut out: Vec<String> = store::changes(tx, &run.id)?
        .into_iter()
        .filter(|c| !engine::is_terminal(&c.state))
        .map(|c| c.repo)
        .collect();
    if let Some(r) = cwd_repo {
        if !s.home_repos.iter().any(|h| h == r) {
            out.push(r.to_owned());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

// ---- the dispatch gate ----------------------------------------------------------------------

fn stage_of(s: &WorkflowSettings, agent: &str) -> Option<&'static str> {
    let st = &s.stages;
    [
        (st.plan.agent.as_str(), "plan"),
        (st.plan_review.agent.as_str(), "planReview"),
        (st.build.agent.as_str(), "build"),
        (st.review.agent.as_str(), "review"),
        (st.cleanup.agent.as_str(), "cleanup"),
        (st.smoketest.agent.as_str(), "smoketest"),
    ]
    .into_iter()
    .find(|(a, _)| *a == agent)
    .map(|(_, s)| s)
}

fn stage_off(s: &WorkflowSettings, run: &Run, stage: &str) -> bool {
    let st = &s.stages;
    match stage {
        "plan" => st.plan.use_ == StageUse::Off,
        "planReview" => st.plan_review.use_ == StageUse::Off,
        "review" => st.review.use_ == StageUse::Off,
        "cleanup" => st.cleanup.use_ == StageUse::Off,
        "smoketest" => st.smoketest.use_ == StageUse::Off && !run.visual,
        _ => false,
    }
}

fn cap_hint(stage: CapStage) -> String {
    format!(
        "app_workflow allowMore {{stage: \"{}\", repo, branch, reason}} gives one more round at the cap; past that only the user's Allow more does",
        stage.name()
    )
}

/// Why the spawn is refused, or `None`. `input` is the spawn tool's input.
pub fn spawn_refusal(
    tx: &Transaction<'_>,
    s: &WorkflowSettings,
    run: &Run,
    input: &Value,
    child: bool,
) -> rusqlite::Result<Option<(String, String)>> {
    let agent = input
        .get("subagent_type")
        .and_then(Value::as_str)
        .filter(|a| !a.is_empty())
        .unwrap_or("general-purpose");
    let base = agent.split("--").next().unwrap_or(agent);
    let stage = stage_of(s, base).filter(|st| !stage_off(s, run, st));
    let deny = |gate: &str, why: String| Ok(Some((gate.to_owned(), why)));
    if input.get("isolation").and_then(Value::as_str) == Some("remote") {
        return deny("dispatch", "a remote agent runs its tools outside Helm's checks. Dispatch it without isolation: remote".into());
    }
    if child {
        if input.get("run_in_background").and_then(Value::as_bool) == Some(true) {
            return deny("dispatch", "during a task a sub-agent may not start background agents. Run the helper in the foreground, or finish and say in your reply what the orchestrator should dispatch".into());
        }
        if let Some(st) = stage {
            return deny(st, format!("only the task's orchestrator dispatches stage agents ({agent} is the {st} stage's) during a task. Finish your work and say in your reply what the orchestrator should dispatch"));
        }
        return Ok(None);
    }
    let all = store::changes(tx, &run.id)?;
    let open: Vec<_> = all.iter().filter(|c| c.state == "open").collect();
    let at_cap = |stage: CapStage, c: &store::Change| -> rusqlite::Result<bool> {
        let cap = engine::cap(tx, s, run, stage, Some((&c.repo, &c.branch)))?;
        let used = if stage == CapStage::Review {
            c.review_rounds
        } else {
            c.smoke_rounds
        };
        Ok(used >= cap)
    };
    let review_needed = s.stages.review.use_ == StageUse::Required;
    let unapproved: Vec<_> = open
        .iter()
        .filter(|c| c.tip.is_some() && !engine::approval_holds(c))
        .collect();
    let all_at_cap = |list: &[&&store::Change], stage: CapStage| -> rusqlite::Result<bool> {
        if list.is_empty() {
            return Ok(false);
        }
        for c in list {
            if !at_cap(stage, c)? {
                return Ok(false);
            }
        }
        Ok(true)
    };
    match stage {
        Some("plan") => Ok(None),
        Some("planReview") => {
            if run.plan_id.is_none() {
                return deny("planReview", "the task has no plan of record to review yet. Adopt it with app_workflow plan {text | path} first".into());
            }
            let cap = engine::cap(tx, s, run, CapStage::PlanReview, None)?;
            if run.plan_rounds >= cap && !run.plan_approved {
                return deny("planReview", format!("the plan is at its review cap ({cap} rounds). app_workflow allowMore {{stage: \"planReview\", reason}} gives one more round; past that only the user's Allow more does"));
            }
            Ok(None)
        }
        Some("review") => {
            if all_at_cap(&unapproved, CapStage::Review)? {
                return deny(
                    "review",
                    format!(
                        "every change that still needs an approval is at its review cap. {}",
                        cap_hint(CapStage::Review)
                    ),
                );
            }
            Ok(None)
        }
        Some("smoketest") => {
            let lacking: Vec<_> = open
                .iter()
                .filter(|c| c.tip.is_some() && c.smoke_pass_tip != c.tip)
                .collect();
            if all_at_cap(&lacking, CapStage::Smoketest)? {
                return deny("smoketest", format!("every change that still needs a smoketest pass is at its smoketest cap. {}", cap_hint(CapStage::Smoketest)));
            }
            Ok(None)
        }
        Some("cleanup") => {
            let basis = open.iter().any(|c| {
                if review_needed {
                    engine::approval_holds(c)
                } else {
                    c.build_tip.is_some()
                }
            });
            if !basis {
                return deny(
                    "cleanup",
                    format!(
                        "no change has a clean-up basis yet ({}). Clean up after that",
                        if review_needed {
                            "an approval at its latest commit"
                        } else {
                            "a reported build"
                        }
                    ),
                );
            }
            Ok(None)
        }
        // A build: the build stage's agent, or any spawn that is not build-exempt.
        _ => {
            if s.stages
                .build
                .build_exempt
                .iter()
                .any(|a| a.eq_ignore_ascii_case(base))
            {
                return Ok(None);
            }
            let exempt_hint = if stage.is_none() {
                format!(" If {agent} does not write code, the user can list it under Build-exempt agents in Settings > Orchestrator.")
            } else {
                String::new()
            };
            if let Some(why) = engine::plan_missing(s, run) {
                return deny("build", format!("during a task every dispatch that is not a stage agent counts as a build, and the build gate needs the plan first: {why}. Get the plan reviewed (and signed off if asked) before building.{exempt_hint}"));
            }
            if review_needed && all_at_cap(&unapproved, CapStage::Review)? {
                return deny("build", format!("every unapproved change is at its review cap, so a new build could not be reviewed. {}.{exempt_hint}", cap_hint(CapStage::Review)));
            }
            Ok(None)
        }
    }
}

// ---- the edit gate --------------------------------------------------------------------------

fn granted(tx: &Transaction<'_>, run: &Run, key: &str) -> rusqlite::Result<bool> {
    Ok(store::count_events(tx, &run.id, "edits_allowed", None, None, None, Some(key))? > 0)
}

/// Why the orchestrator's own edit of `file` is refused, or `None`.
pub fn edit_refusal(
    tx: &Transaction<'_>,
    s: &WorkflowSettings,
    run: &Run,
    file: &Path,
    file_repo: Option<&str>,
    cwd_repo: Option<&str>,
) -> rusqlite::Result<Option<String>> {
    if s.orchestrator_edits == EditsRule::Allow {
        return Ok(None);
    }
    let Some(repo) = file_repo else {
        return Ok(None);
    };
    if !task_repos(tx, s, run, cwd_repo)?.iter().any(|r| r == repo) {
        return Ok(None);
    }
    if granted(tx, run, &path_key(file))? {
        return Ok(None);
    }
    Ok(Some(format!(
        "Orchestrator edits are off, and {} is in this task's repository {repo}. Dispatch a build agent for code changes. If this one file is genuinely yours to edit (a plan or a note, not the change's code), call app_workflow allow_edit {{file, reason}} and edit it again. The user can let the orchestrator edit in Settings > Orchestrator (Orchestrator edits: allow).",
        file.display()
    )))
}

/// `allow_edit {file, reason}`: the orchestrator grants itself one file until the task ends.
/// `abs` is the file resolved by the caller; the repositories are read by the host.
pub fn allow_edit(
    tx: &Transaction<'_>,
    x: &Act<'_>,
    a: &Value,
    abs: Option<&Path>,
    file_repo: Option<&str>,
    cwd_repo: Option<&str>,
) -> Out {
    let run = match actions::active(tx, x.chat_id)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    if let Err(e) = need(a, "file", 4096) {
        return Ok(Err(e));
    }
    let reason = match need(a, "reason", MAX_GRANT_REASON) {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let Some(abs) = abs else {
        return Ok(Err(
            "file must be absolute or relative to the chat's folder, at most 2000 characters."
                .into(),
        ));
    };
    let gated = match file_repo {
        Some(r) => task_repos(tx, x.s, &run, cwd_repo)?.iter().any(|t| t == r),
        None => false,
    };
    if !gated || x.s.orchestrator_edits == EditsRule::Allow {
        return Ok(Ok(
            json!({"granted": false, "gated": false, "note": "Your edits to that file are not gated, so nothing was recorded."}),
        ));
    }
    let key = path_key(abs);
    if granted(tx, &run, &key)? {
        return Ok(Ok(
            json!({"granted": true, "already": true, "file": abs.display().to_string()}),
        ));
    }
    if store::count_events(tx, &run.id, "edits_allowed", None, None, None, None)? >= MAX_GRANTS {
        return Ok(Err(format!(
            "This task already holds {MAX_GRANTS} file grants; dispatch a build agent for the rest."
        )));
    }
    let file = abs.display().to_string();
    let body = json!({"file": file, "reason": reason}).to_string();
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            kind: "edits_allowed",
            verdict: Some(&key),
            body: Some(&body),
            ..Default::default()
        },
        x.now,
    )?;
    crate::notes::queue(
        x.chat_id,
        format!("Orchestrator edit of {file}: the edit gate lets it through until the task ends, on the agent's reason: {reason}"),
        false,
        format!("wf-edit-{}-{key}", run.id),
    );
    Ok(Ok(
        json!({"granted": true, "file": file, "until": "the task ends"}),
    ))
}

// ---- the host side of the worker's hook -----------------------------------------------------

fn allow() -> Value {
    json!({"allow": true})
}

/// One gate question from the worker: `{tool, input, child, cwd?}`. Answers `{allow, reason?}`.
pub fn gate_call(db: &Database, chat: &str, args: &Value, now: i64) -> Result<Value, String> {
    let tool = args.get("tool").and_then(Value::as_str).unwrap_or("");
    let input = args.get("input").cloned().unwrap_or(Value::Null);
    let child = args.get("child").and_then(Value::as_bool).unwrap_or(true);
    let spawn = SPAWN_TOOLS.contains(&tool);
    let edit = EDIT_TOOLS.contains(&tool) && !child;
    if !spawn && !edit {
        return Ok(allow());
    }
    let (s, on) = db
        .snapshot(|tx| {
            let s = settings::load(tx)?;
            let on = mode::in_effect(tx, chat, &s)? && store::active_run(tx, chat)?.is_some();
            Ok::<_, rusqlite::Error>((s, on))
        })
        .map_err(|e| e.to_string())?;
    if !on || (spawn && !s.dispatch_gate) || (edit && s.orchestrator_edits == EditsRule::Allow) {
        return Ok(allow());
    }
    // Files and git folders are read before the transaction.
    let cwd = args
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::wftools::chat_cwd(db, chat));
    let (file, file_repo, cwd_repo) = if edit {
        let raw = input
            .get("file_path")
            .or_else(|| input.get("notebook_path"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let file = lexical_abs(cwd.as_deref(), raw);
        let file_repo = file
            .as_deref()
            .and_then(|f| repo_of(f.parent().unwrap_or(f)));
        let chat_dir = crate::wftools::chat_cwd(db, chat);
        (file, file_repo, chat_dir.as_deref().and_then(repo_of))
    } else {
        (None, None, None)
    };
    let out = db
        .write(|tx| {
            let Some(run) = store::active_run(tx, chat)? else {
                return Ok(allow());
            };
            let refusal = if spawn {
                spawn_refusal(tx, &s, &run, &input, child)?.map(|(gate, why)| {
                    (
                        gate,
                        format!("Helm's dispatch gate refused this dispatch: {why}. {OFF_SWITCH}"),
                    )
                })
            } else if let Some(f) = &file {
                edit_refusal(tx, &s, &run, f, file_repo.as_deref(), cwd_repo.as_deref())?
                    .map(|why| ("edit".to_owned(), why))
            } else {
                None
            };
            let Some((gate, why)) = refusal else {
                return Ok(allow());
            };
            store::event(
                tx,
                chat,
                Some(&run),
                &Event {
                    kind: "gate_denied",
                    verdict: Some(&gate),
                    body: Some(&why),
                    ..Default::default()
                },
                now,
            )?;
            Ok::<_, rusqlite::Error>(json!({"allow": false, "reason": why}))
        })
        .map_err(|e| e.to_string())?;
    Ok(out)
}

pub const GATE_DESCRIPTION: &str = "Helm's own question before an agent spawn or a file edit in a chat under the orchestrator workflow. The worker asks it; agents never see it.";

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            GATE_TOOL,
            GATE_DESCRIPTION,
            json!({"type": "object", "properties": {
                "tool": {"type": "string"}, "input": {"type": "object"},
                "child": {"type": "boolean"}, "cwd": {"type": "string"}
            }, "required": ["tool"]}),
        )
        .large()
        .children()
        .when(Condition::Worker)
        // A quick check that must not wait behind slow reads or the maintenance lock; its only
        // write is a refusal row in its own transaction.
        .self_locking(),
        |ctx, args| {
            let Some(db) = ctx.services.get::<Database>() else {
                return Ok(allow());
            };
            let out = gate_call(&db, &ctx.caller_chat_id, &args, helm_core::now_ms())
                .map_err(ToolError::maybe);
            crate::store::emit_touched(&*ctx.events);
            out
        },
    );
}

/// Puts [`GATES_ENV`] on the launch of a chat the gates may act in: master switch on, a Claude
/// Code chat that is not archived, Orchestrator on or Auto, and at least one gate switched on.
/// Every other launch is unchanged.
pub struct GateLaunch {
    pub db: Arc<Database>,
}

pub fn launch_wants_gates(db: &Database, chat_id: &str) -> bool {
    db.snapshot(|tx| {
        let s = settings::load(tx)?;
        let Some(f) = mode::facts(tx, chat_id)? else {
            return Ok(false);
        };
        if mode::unavailable(&s, &f).is_some()
            || (!s.dispatch_gate && s.orchestrator_edits == EditsRule::Allow)
        {
            return Ok(false);
        }
        Ok::<_, rusqlite::Error>(mode::current(tx, chat_id, &s)?.0 != ChatMode::Off)
    })
    .unwrap_or(false)
}

impl LaunchContributor for GateLaunch {
    fn contribute(&self, ctx: &LaunchCtx<'_>) -> LaunchExtras {
        let mut e = LaunchExtras::default();
        if launch_wants_gates(&self.db, ctx.chat_id) {
            e.env.push((GATES_ENV.to_owned(), "1".to_owned()));
        }
        e
    }
}
