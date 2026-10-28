//! The `app_workflow` (orchestrator) and `app_workflow_report` (stage agents) host tools.
//!
//! Both are listed in every root chat (the catalogue has no per-chat condition) and refuse unless
//! the workflow is in effect for the calling chat: master switch on, a Claude Code chat, not
//! archived, state on. So with the workflow off they write nothing.

use crate::actions::{self, Act};
use crate::git::{GitTrees, TreeReader};
use crate::{handoff, mode, reports, settings};
use helm_consent::AllowKinds;
use helm_core::db::Database;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolSpec};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const OFF: &str = "Orchestrator is off for this chat, so the workflow tools do nothing here. The user can turn it on with the Orchestrator switch in the composer.";

pub const WORKFLOW_DESCRIPTION: &str = "The orchestrator workflow for THIS chat (only while Orchestrator is on). action is one of: start {title, itemId?, new?}; plan {text | path, title?, itemId?}; status; update {repo, branch, pr?, base?, visual?}; skip {stage, reason}; park {reason}; drop {repo, branch, reason} or drop {queued} (discard a queued follow-up); hand_off {changes?, final?, summary?}; finish; allowMore {stage, repo?, branch?, reason}; land {repo, branch} (the user's Merge of a change waiting for it; needs userQuote or grant); take_over {repo, branch, reason, check?} (move a change another chat holds into this task; check: true only reports); allow_edit {file, reason} (edit that one file yourself until the task ends). repo is host/owner/name. Steps only the user could take accept userQuote (the user's exact words) or grant (an Allow request id); never claim consent the user did not give. Only the chat's own agent may call it.";

pub const REPORT_DESCRIPTION: &str = "Report a stage verdict to Helm. Only stage agents the orchestrator dispatched may call it, and only this report counts. kind: plan_review {verdict: approve|changes_required, findings?}; build {repo, branch, tip, base?, pr?}; code_review {repo, branch, tip, base, verdict: lgtm|lgtm_with_nits|changes_required, findings?, followUps?}; cleanup {repo, branch, tip, pr}; visual_review {repo, branch, verdict: pass|fail|pending, evidence?}; after_land {verdict: done|awaiting_merge|not_done, followUps?} (post-land work on a task that finished landing). Every report has summary (at most 8000 characters). tip is the full 40-character commit sha. Unknown fields are refused. If a refusal says Correctable, fix the report and send it once more; if it says Permanent, state the refusal in your reply.";

fn workflow_schema() -> Value {
    json!({"type": "object", "properties": {
        "action": {"type": "string", "enum": ["start", "plan", "status", "update", "skip", "park", "drop", "hand_off", "finish", "allowMore", "land", "take_over", "allow_edit"]},
        "title": {"type": "string"}, "itemId": {"type": "string"}, "new": {"type": "boolean"},
        "text": {"type": "string"}, "path": {"type": "string"},
        "repo": {"type": "string"}, "branch": {"type": "string"}, "pr": {"type": "string"}, "base": {"type": "string"},
        "visual": {"type": "boolean"}, "stage": {"type": "string"}, "reason": {"type": "string"},
        "changes": {"type": "array", "items": {"type": "object", "properties": {"repo": {"type": "string"}, "branch": {"type": "string"}}}},
        "final": {"type": "boolean"}, "summary": {"type": "string"}, "n": {"type": "integer"},
        "userQuote": {"type": "string"}, "grant": {"type": "string"},
        "check": {"type": "boolean"}, "file": {"type": "string"}, "queued": {"type": "string"}
    }, "required": ["action"]})
}

fn report_schema() -> Value {
    json!({"type": "object", "properties": {
        "kind": {"type": "string", "enum": ["plan_review", "build", "code_review", "cleanup", "visual_review", "after_land"]},
        "verdict": {"type": "string"}, "repo": {"type": "string"}, "branch": {"type": "string"},
        "tip": {"type": "string"}, "base": {"type": "string"}, "pr": {"type": "string"},
        "summary": {"type": "string"},
        "findings": {"type": "array"}, "followUps": {"type": "array", "items": {"type": "string"}},
        "evidence": {"type": "array"}
    }, "required": ["kind", "summary"], "additionalProperties": false})
}

/// The chat's folder (its project's path), for git reads and relative plan paths.
pub fn chat_cwd(db: &Database, chat_id: &str) -> Option<PathBuf> {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT p.path FROM chats c JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
            [chat_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
    })
    .ok()
    .flatten()
    .flatten()
    .filter(|p| !p.is_empty())
    .map(PathBuf::from)
}

/// A plan from `path` (absolute, or relative to the chat's folder), at most 256 KiB.
pub fn read_plan(cwd: Option<&Path>, path: &str) -> Result<String, String> {
    let p = Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.ok_or("A relative path needs the chat's folder, and this chat has none.")?
            .join(p)
    };
    let meta =
        std::fs::metadata(&full).map_err(|_| "The plan file could not be read.".to_owned())?;
    if !meta.is_file() || meta.len() as usize > actions::MAX_PLAN {
        return Err("The plan file must be a file of at most 256 KiB.".into());
    }
    std::fs::read_to_string(&full).map_err(|_| "The plan file is not UTF-8 text.".into())
}

/// One `app_workflow` call without the live runtime: every chat counts as possibly running
/// agents, so a take-over always needs the user. The tool handler uses [`workflow_call_with`].
pub fn workflow_call(
    db: &Database,
    kinds: &AllowKinds,
    chat: &str,
    is_child: bool,
    args: &Value,
    now: i64,
) -> Result<Value, ToolError> {
    workflow_call_with(db, kinds, chat, is_child, args, now, &|_| true)
}

/// One `app_workflow` call. `is_child` refuses sub-agents. `agents_running` tells, from the live
/// runtime, whether a chat's sub-agents or background tasks are running; it is asked before the
/// transaction, and only for a take-over's holders.
pub fn workflow_call_with(
    db: &Database,
    kinds: &AllowKinds,
    chat: &str,
    is_child: bool,
    args: &Value,
    now: i64,
    agents_running: &dyn Fn(&str) -> bool,
) -> Result<Value, ToolError> {
    if is_child {
        return Err(ToolError::refused(
            "Only the orchestrator (the chat's own agent) can call app_workflow; ask it.",
        ));
    }
    let action = args.get("action").and_then(Value::as_str).unwrap_or("");
    // Read nothing for a chat the workflow is off in (the plan file read below included).
    let on = db
        .snapshot(|tx| {
            let s = settings::load(tx)?;
            mode::in_effect(tx, chat, &s)
        })
        .map_err(|e| ToolError::maybe(e.to_string()))?;
    if !on {
        return Err(ToolError::refused(OFF));
    }
    let plan_text = if action == "plan" {
        match (
            args.get("text").and_then(Value::as_str),
            args.get("path").and_then(Value::as_str),
        ) {
            (Some(t), None) => Some(t.to_owned()),
            (None, Some(p)) => {
                Some(read_plan(chat_cwd(db, chat).as_deref(), p).map_err(ToolError::refused)?)
            }
            _ => return Err(ToolError::refused("plan takes text or path, exactly one.")),
        }
    } else {
        None
    };
    // allow_edit: the file and the repositories are read by the host, before the transaction.
    let (edit_file, edit_repo, cwd_repo) = if action == "allow_edit" {
        let cwd = chat_cwd(db, chat);
        let file = args
            .get("file")
            .and_then(Value::as_str)
            .and_then(|f| crate::gates::lexical_abs(cwd.as_deref(), f));
        let repo = file
            .as_deref()
            .and_then(|f| crate::gates::repo_of(f.parent().unwrap_or(f)));
        (file, repo, cwd.as_deref().and_then(crate::gates::repo_of))
    } else {
        (None, None, None)
    };
    let agents_live: Vec<String> = if action == "take_over" {
        crate::claims::holder_chats(db, args)
            .into_iter()
            .filter(|c| agents_running(c))
            .collect()
    } else {
        Vec::new()
    };
    db.write(|tx| {
        let s = settings::load(tx)?;
        if !mode::in_effect(tx, chat, &s)? {
            return Ok(Err(OFF.to_owned()));
        }
        let x = Act { chat_id: chat, s: &s, kinds, now };
        match action {
            "start" => actions::start(tx, &x, args),
            "plan" => actions::plan(tx, &x, args, plan_text.as_deref().unwrap_or("")),
            "status" => actions::status(tx, &x),
            "update" => actions::update(tx, &x, args),
            "skip" => actions::skip(tx, &x, args),
            "park" => actions::park(tx, &x, args),
            "drop" => actions::drop(tx, &x, args),
            "hand_off" => handoff::hand_off(tx, &x, args),
            "finish" => handoff::finish(tx, &x),
            "allowMore" => actions::allow_more(tx, &x, args),
            "land" => land_consented(tx, &x, args),
            "take_over" => crate::claims::take_over(tx, &x, args, &agents_live),
            "allow_edit" => crate::gates::allow_edit(tx, &x, args, edit_file.as_deref(), edit_repo.as_deref(), cwd_repo.as_deref()),
            _ => Ok(Err("action must be start, plan, status, update, skip, park, drop, hand_off, finish, allowMore, land, take_over or allow_edit.".into())),
        }
    })
    .map_err(|e| ToolError::maybe(e.to_string()))?
    .map_err(ToolError::refused)
}

/// One `app_workflow_report` call; the tree is read from git first, outside the transaction.
pub fn report_call(
    db: &Database,
    kinds: &AllowKinds,
    trees: &dyn TreeReader,
    chat: &str,
    is_child: bool,
    args: Value,
    now: i64,
) -> Result<Value, ToolError> {
    let r = reports::parse(args).map_err(ToolError::refused)?;
    let tree = match (&r.tip, chat_cwd(db, chat)) {
        (Some(t), Some(cwd)) => trees.tree(&cwd, t),
        _ => None,
    };
    db.write(|tx| {
        let s = settings::load(tx)?;
        if !mode::in_effect(tx, chat, &s)? {
            return Ok(Err(OFF.to_owned()));
        }
        let x = Act {
            chat_id: chat,
            s: &s,
            kinds,
            now,
        };
        reports::report(tx, &x, is_child, tree, r)
    })
    .map_err(|e| ToolError::maybe(e.to_string()))?
    .map_err(ToolError::refused)
}

fn kinds_of(ctx: &ToolCtx) -> Arc<AllowKinds> {
    ctx.services.get::<AllowKinds>().unwrap_or_else(|| {
        let mut k = AllowKinds::new();
        crate::kinds::register_allow_kinds(&mut k);
        Arc::new(k)
    })
}

/// After a tool call, tell the UI which chats' runs it wrote (a refusal can write a row too), and
/// deliver the workflow notes it queued.
fn emit(ctx: &ToolCtx) {
    crate::store::emit_touched(&*ctx.events);
    let notes = crate::notes::take_pending();
    if notes.is_empty() {
        return;
    }
    if let (Some(db), Some(rt)) = (
        ctx.services.get::<Database>(),
        ctx.services.get::<helm_chats::ChatRuntime>(),
    ) {
        crate::notes::deliver_pending(&db, &rt, notes);
    }
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write("app_workflow", WORKFLOW_DESCRIPTION, workflow_schema()).large(),
        |ctx, args| {
            let db = ctx
                .services
                .get::<Database>()
                .ok_or_else(|| ToolError::refused(OFF))?;
            let rt = ctx.services.get::<helm_chats::ChatRuntime>();
            // Without the runtime nothing is known, so every holder counts as running agents.
            let agents = |c: &str| {
                rt.as_ref().is_none_or(|rt| {
                    rt.live_thread(c)
                        .is_some_and(|t| t.subagents_running > 0 || !t.background_tasks.is_empty())
                })
            };
            let out = workflow_call_with(
                &db,
                &kinds_of(ctx),
                &ctx.caller_chat_id,
                ctx.is_child,
                &args,
                helm_core::now_ms(),
                &agents,
            );
            if out.is_err() {
                // A refused call delivers nothing it may have queued.
                crate::notes::take_pending();
            }
            afk_sign_off(ctx, &db);
            emit(ctx);
            let out = out?;
            crate::land::kick();
            Ok(out)
        },
    )
    .blocking(
        ToolSpec::write("app_workflow_report", REPORT_DESCRIPTION, report_schema())
            .large()
            .children(),
        |ctx, args| {
            let db = ctx
                .services
                .get::<Database>()
                .ok_or_else(|| ToolError::refused(OFF))?;
            let child = ctx.is_child || ctx.origin == helm_hosttools::CallOrigin::Subagent;
            let out = report_call(
                &db,
                &kinds_of(ctx),
                &GitTrees,
                &ctx.caller_chat_id,
                child,
                args,
                helm_core::now_ms(),
            );
            afk_sign_off(ctx, &db);
            emit(ctx);
            out
        },
    );
}

/// While AFK is on with plan approval set to approve, a plan that now waits on the user's
/// sign-off (just recorded, or just approved by its review) is signed off for them.
fn afk_sign_off(ctx: &ToolCtx, db: &Database) {
    let Some(on) = ctx.services.get::<crate::user::AfkPlanSignOff>() else {
        return;
    };
    if on.0() {
        let chat = ctx.caller_chat_id.clone();
        let _ = db.write(|tx| crate::user::afk_sign_off(tx, &chat, helm_core::now_ms()));
    }
}

/// `land {repo, branch, userQuote | grant}`: the user's Merge of this task's `awaiting_merge`
/// change, on their verified words or an Allow grant. Without a proof it records
/// the ask and names the sentence to ask for.
fn land_consented(tx: &rusqlite::Transaction<'_>, x: &Act<'_>, a: &Value) -> actions::Out {
    let run = match actions::active(tx, x.chat_id)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let (repo, branch) = match actions::target(a) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let Some(c) = crate::store::change(tx, &run.id, &repo, &branch)? else {
        return Ok(Err(format!("The task has no change {repo} {branch}.")));
    };
    if c.state != "awaiting_merge" {
        return Ok(Err(format!(
            "{branch} is {}, not waiting for a Merge.",
            c.state
        )));
    }
    let t = json!({"runId": run.id, "repo": repo, "branch": branch});
    let proof = crate::kinds::Proof::from_args(a);
    if let Err(e) = crate::kinds::consent(
        tx,
        x.kinds,
        x.chat_id,
        "land",
        &t,
        &proof,
        "Merging is the user's step.",
        x.now,
    )? {
        return Ok(Err(e));
    }
    crate::store::event(
        tx,
        x.chat_id,
        Some(&run),
        &crate::store::Event {
            kind: "consent",
            repo: Some(&repo),
            branch: Some(&branch),
            verdict: Some("land"),
            ..Default::default()
        },
        x.now,
    )?;
    match crate::land::user_action(tx, &run.id, &repo, &branch, "merge", x.now)? {
        Ok(state) => Ok(Ok(
            json!({"state": state, "next": "Helm re-checks the PR and merges it if it still holds."}),
        )),
        Err(e) => Ok(Err(e)),
    }
}
