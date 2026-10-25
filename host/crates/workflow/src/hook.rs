//! The landing guard's hook endpoint.
//!
//! Before Claude Code runs a Bash, PowerShell or MCP tool call, the chat's worker sends one
//! `guard_check` line and waits for the `guard_verdict` the host sends back:
//!
//! - `guard_check {id, kind: "bash" | "powershell" | "mcp", root, turn, cwd?, command?, tool?,
//!   input?}`: `root` is false for a sub-agent's call; `turn` changes with every turn.
//! - `guard_verdict {id, allow, reason?, context?, active}`: `reason` is the refusal the agent
//!   reads, `context` a note added to an allowed call, `active` whether the guard is in force for
//!   the chat (the worker uses it to decide what to do when no answer comes in time).
//!
//! The guard is in force only while the landing guard setting and the master switch are on, the
//! chat's Orchestrator is on and it has an active task. Everywhere else the answer is an
//! immediate allow and nothing is read or written, so a chat with Orchestrator off runs exactly
//! as it would without the guard (this app's own fast-forward pushes to main included).
//!
//! In force, a refusal is soft: it says why and how to get through. The orchestrator (never a
//! sub-agent) can override with a first line `# helm-override: <reason>` (reason tier) or
//! `# helm-override: <reason> [user: "<the user's own words>"]` (quote tier, checked against what
//! the user typed in this chat since the task's turn began; a Telegram message, a schedule or
//! another chat never counts), or for the reason tier by sending the same command again as its
//! next Bash call in the turn. A refused MCP call is overridden by a Bash call whose whole
//! command is the override line, then the same MCP call again. Each override writes one
//! `guard_override` row and each refusal a `gate_denied` row; both show in the workflow panel's
//! Needs you until the user's next message.

use crate::guard::{self, RepoFacts, ShellKind, Tier, Tracked, Verdict};
use crate::overrides::{self, Decision};
use crate::store::{self, Event};
use crate::{mode, settings};
use helm_core::db::Database;
use helm_hosttools::lines::{Broadcaster, WorkerLines};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How a refused command can be let through, said at the end of every refusal the orchestrator
/// gets.
const TURN_OFF: &str = "The user can also turn the landing guard off in Settings → Orchestrator.";

/// One `guard_check` line.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    #[serde(default)]
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub root: bool,
    #[serde(default)]
    pub turn: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub input: Value,
}

/// The host's answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub allow: bool,
    pub reason: Option<String>,
    pub context: Option<String>,
    pub active: bool,
}

impl Answer {
    fn pass(active: bool) -> Self {
        Answer {
            allow: true,
            active,
            ..Default::default()
        }
    }
    fn refuse(reason: String) -> Self {
        Answer {
            allow: false,
            reason: Some(reason),
            active: true,
            ..Default::default()
        }
    }
    pub fn line(&self, id: &str) -> Value {
        let mut v =
            json!({"type": "guard_verdict", "id": id, "allow": self.allow, "active": self.active});
        if let Some(r) = &self.reason {
            v["reason"] = json!(r);
        }
        if let Some(c) = &self.context {
            v["context"] = json!(c);
        }
        v
    }
}

/// What the hook remembers per chat between calls: the last refused root Bash command (for the
/// resend override) and the MCP override offer.
#[derive(Default)]
struct ChatState {
    /// (turn, command, tier, rule) of a refusal that the next root Bash call may resend.
    refused: Option<(String, String, Tier, &'static str)>,
    /// A refused root MCP call: (turn, call key, rule). `armed` once an override line covers it.
    offer: Option<(String, String, &'static str)>,
    armed: Option<(String, String, Decision)>,
}

fn states() -> &'static Mutex<HashMap<String, ChatState>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, ChatState>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// The workflow crate's worker lines: `guard_check`, answered off the actor.
pub fn register_worker_lines(l: &mut WorkerLines) {
    l.register_worker_line("guard_check", |ctx, v| {
        let chat = ctx.chat_id.to_owned();
        let services = ctx.services.clone();
        let events = ctx.events.clone();
        let check: Check = match serde_json::from_value(v.clone()) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("chat {chat}: bad guard_check: {e}");
                return;
            }
        };
        let work = move || {
            let answer = match services.get::<Database>() {
                Some(db) => check_logged(&db, &chat, &check, helm_core::now_ms()),
                None => Answer::pass(false),
            };
            store::emit_touched(&*events);
            if let Some(b) = services.get::<Broadcaster>() {
                if !b.0.send_to_worker(&chat, &answer.line(&check.id)) {
                    log::warn!("chat {chat}: the landing guard's answer found no worker");
                }
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn_blocking(work);
            }
            Err(_) => {
                std::thread::spawn(work);
            }
        }
    });
}

/// [`check`] with real git, logging what it refused or let through.
fn check_logged(db: &Database, chat: &str, c: &Check, now: i64) -> Answer {
    let facts = GitFacts::default();
    let a = check(db, chat, c, &facts, now);
    if !a.allow {
        log::info!("chat {chat}: the landing guard refused a {} call", c.kind);
    }
    a
}

/// What the guard reads from the host's records for one call, when it is in force.
struct InForce {
    ctx: guard::Ctx,
    run: store::Run,
    flip_label: Option<String>,
    /// What the user typed in this chat since the task's turn began.
    typed: Vec<String>,
}

fn in_force(tx: &Transaction<'_>, chat: &str, cwd: &Path) -> rusqlite::Result<Option<InForce>> {
    let s = settings::load(tx)?;
    if !s.enabled || !s.landing_guard || !mode::in_effect(tx, chat, &s)? {
        return Ok(None);
    }
    let Some(run) = store::active_run(tx, chat)? else {
        return Ok(None);
    };
    let ctx = guard::context(tx, chat, cwd, &s)?;
    let typed = typed_since_task(tx, chat, &run.id)?;
    Ok(Some(InForce {
        ctx,
        run,
        flip_label: s.landing.flip_label.clone(),
        typed,
    }))
}

/// The user's own typed messages in this chat from the turn the task began in (the message just
/// before the task started) onwards. Only typed messages count: never Telegram, schedules, other
/// chats, app notes or answers.
fn typed_since_task(tx: &Transaction<'_>, chat: &str, run: &str) -> rusqlite::Result<Vec<String>> {
    let started: i64 = tx
        .query_row(
            "SELECT created_at FROM workflow_runs WHERE id = ?1",
            [run],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let from: i64 = tx
        .query_row(
            "SELECT MAX(s.created_at) FROM submissions s JOIN submission_meta m ON m.submission_id = s.id
             WHERE s.chat_id = ?1 AND m.origin = 'typed' AND s.created_at <= ?2",
            params![chat, started],
            |r| r.get::<_, Option<i64>>(0),
        )?
        .unwrap_or(started);
    let mut st = tx.prepare(
        "SELECT s.text FROM submissions s JOIN submission_meta m ON m.submission_id = s.id
         WHERE s.chat_id = ?1 AND m.origin = 'typed' AND s.created_at >= ?2
         ORDER BY s.created_at DESC LIMIT 200",
    )?;
    let rows = st.query_map(params![chat, from], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Judge one call. Pure apart from the database and the repository facts.
pub fn check(db: &Database, chat: &str, c: &Check, facts: &dyn RepoFacts, now: i64) -> Answer {
    let cwd = c
        .cwd
        .as_deref()
        .map(guard::native_path)
        .or_else(|| crate::wftools::chat_cwd(db, chat))
        .unwrap_or_default();
    let found = db.snapshot(|tx| in_force(tx, chat, &cwd));
    let f = match found {
        Ok(Some(f)) => f,
        Ok(None) => return Answer::pass(false),
        Err(e) => {
            // The guard can't tell whether it is in force; it is a safety net, not a sandbox.
            log::warn!("chat {chat}: the landing guard could not read its records: {e}");
            return Answer::pass(false);
        }
    };
    let result = match c.kind.as_str() {
        "bash" | "powershell" => bash(db, chat, c, &f, facts, now),
        "mcp" => mcp(db, chat, c, &f, now),
        _ => Ok(Answer::pass(true)),
    };
    match result {
        Ok(a) => a,
        Err(e) => {
            // In force but the check itself failed: refuse, with the user's words as the way through.
            log::warn!("chat {chat}: the landing guard failed: {e}");
            let cmd = c.command.as_deref().unwrap_or("");
            let crashed = Verdict::Deny {
                tier: Tier::Quote,
                rule: "crashed",
                why: "Helm could not finish checking this call".into(),
            };
            match (c.root, overrides::decide(cmd, &crashed, &f.typed)) {
                (true, d @ Decision::Overridden { tier: Tier::Quote, .. }) => {
                    let _ = db.write(|tx| {
                        overrides::record(tx, chat, Some(&f.run), "bash", "comment", &d, cmd, "crashed", now)
                    });
                    Answer {
                        context: Some("Helm recorded this override of the landing guard.".into()),
                        ..Answer::pass(true)
                    }
                }
                _ => Answer::refuse(format!(
                    "Helm could not check this call against the landing guard. Try it once more. {TURN_OFF}"
                )),
            }
        }
    }
}

fn gate_denied(
    db: &Database,
    chat: &str,
    run: &store::Run,
    gate: &str,
    command: &str,
    rule: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let body = json!({"gate": gate, "command": command.chars().take(overrides::MAX_COMMAND).collect::<String>()}).to_string();
    db.write(|tx| {
        store::event(
            tx,
            chat,
            Some(run),
            &Event {
                kind: "gate_denied",
                verdict: Some(rule),
                body: Some(&body),
                ..Default::default()
            },
            now,
        )
        .map(|_| ())
    })
}

/// The way through that fits a refusal, added after the override recipe.
fn way_through(rule: &str) -> &'static str {
    match rule {
        "task_repo" | "containment" | "untracked" => {
            " Helm lands a task's changes itself: hand the change off with app_workflow hand_off and the user merges it."
        }
        "draft_rule" => " Open it with --draft.",
        _ => "",
    }
}

fn bash(
    db: &Database,
    chat: &str,
    c: &Check,
    f: &InForce,
    facts: &dyn RepoFacts,
    now: i64,
) -> rusqlite::Result<Answer> {
    let cmd = c.command.as_deref().unwrap_or("");
    let shell = if c.kind == "powershell" {
        ShellKind::PowerShell
    } else {
        ShellKind::Posix
    };
    let mut states = states().lock().unwrap_or_else(|p| p.into_inner());
    let st = states.entry(chat.to_owned()).or_default();
    // Only the orchestrator's own next Bash call can be the resend.
    let resent = if c.root { st.refused.take() } else { None };
    // A root Bash call that is only an override line arms a refused MCP call's override.
    if c.root {
        if let Some((turn, key, rule)) = st.offer.clone().filter(|o| o.0 == c.turn) {
            if is_only_marker(cmd) {
                let need = Verdict::Deny {
                    tier: Tier::Quote,
                    rule,
                    why: "the refused MCP call".into(),
                };
                return Ok(match overrides::decide(cmd, &need, &f.typed) {
                    d @ Decision::Overridden { .. } => {
                        st.offer = None;
                        st.armed = Some((turn, key, d));
                        Answer::refuse("Helm armed the override for the refused MCP call. This command did not run: send that exact MCP call again now, as your next MCP call.".into())
                    }
                    Decision::Refuse(why) => Answer::refuse(format!("{why} {TURN_OFF}")),
                    Decision::Allow => Answer::pass(true),
                });
            }
        }
    }
    let v = guard::judge_as(cmd, shell, &f.ctx, facts);
    let Verdict::Deny { tier, rule, why } = &v else {
        return Ok(Answer::pass(true));
    };
    if !c.root {
        drop(states);
        gate_denied(db, chat, &f.run, "bash", cmd, rule, now)?;
        return Ok(Answer::refuse(format!(
            "Helm refused: {why}. A sub-agent can't override the landing guard: stop and report this to the orchestrator, which can ask the user."
        )));
    }
    // The same command sent again as the next Bash call of the turn covers the reason tier.
    let resend = resent
        .is_some_and(|(turn, prev, t, _)| turn == c.turn && prev == cmd && t == Tier::Reason)
        && *tier == Tier::Reason;
    let decision = if resend {
        Decision::Overridden {
            tier: Tier::Reason,
            verified: false,
            reason: String::new(),
            quote: None,
        }
    } else {
        overrides::decide(cmd, &v, &f.typed)
    };
    match decision {
        d @ Decision::Overridden { .. } => {
            drop(states);
            let path = if resend { "resend" } else { "comment" };
            db.write(|tx| {
                overrides::record(tx, chat, Some(&f.run), "bash", path, &d, cmd, rule, now)
            })?
            .map_err(rusqlite::Error::InvalidParameterName)?;
            Ok(Answer {
                context: Some(
                    "Helm recorded this override of the landing guard; the user sees it.".into(),
                ),
                ..Answer::pass(true)
            })
        }
        Decision::Refuse(text) => {
            st.refused = Some((c.turn.clone(), cmd.to_owned(), *tier, rule));
            drop(states);
            gate_denied(db, chat, &f.run, "bash", cmd, rule, now)?;
            let resend_hint = if *tier == Tier::Reason {
                " Or send the same command again as your next Bash call."
            } else {
                " The user's words must be typed in this chat; a Telegram message does not count."
            };
            Ok(Answer::refuse(format!(
                "{text}{resend_hint}{} {TURN_OFF}",
                way_through(rule)
            )))
        }
        Decision::Allow => Ok(Answer::pass(true)),
    }
}

/// Whether the whole command is one override line (blank lines aside).
fn is_only_marker(cmd: &str) -> bool {
    let lines: Vec<&str> = cmd
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines.len() == 1 && overrides::marker(lines[0]).is_some()
}

// ---- the MCP guard --------------------------------------------------------------------------

/// A GitHub tool call that lands, as the guard reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpLanding {
    /// A PR merge, an auto-merge, or a branch update that merges the base in.
    Merge {
        repo: Option<String>,
        pr: Option<String>,
    },
    /// Marking a draft ready, or removing the review label.
    Flip {
        repo: Option<String>,
        pr: Option<String>,
    },
    /// A file write or commit made on GitHub, onto `branch` (`None`: the default branch).
    Write {
        repo: Option<String>,
        branch: Option<String>,
    },
    /// A fork sync.
    Sync { repo: Option<String> },
    /// A PR opened without draft.
    NonDraftPr { repo: Option<String> },
}

/// The tool's own name: `mcp__github__merge_pull_request` → `merge_pull_request`; a call through
/// Helm's MCP dispatcher is judged by the tool it calls (`mcp_github_merge_pull_request_1a2b3c`).
fn mcp_target(tool: &str, input: &Value) -> (String, Value) {
    let lower = tool.to_ascii_lowercase();
    let dispatched = lower.ends_with("__app_mcp_call") || lower == "app_mcp_call";
    if dispatched {
        let inner = input
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let args = input.get("arguments").cloned().unwrap_or(Value::Null);
        return (inner, args);
    }
    let name = lower.rsplit("__").next().unwrap_or(&lower).to_owned();
    (name, input.clone())
}

fn text(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match v.get(*k) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

/// Read a GitHub MCP call: which landing it is, on which repository, PR or branch. `None` for
/// every call that lands nothing.
pub fn mcp_landing(tool: &str, input: &Value, flip_label: Option<&str>) -> Option<McpLanding> {
    let (name, args) = mcp_target(tool, input);
    if name.is_empty() {
        return None;
    }
    let repo = match (
        text(&args, &["owner"]),
        text(&args, &["repo", "repository", "name"]),
    ) {
        (Some(o), Some(r)) if !r.contains('/') => {
            Some(format!("github.com/{o}/{r}").to_ascii_lowercase())
        }
        (_, Some(r)) if r.matches('/').count() == 1 => {
            Some(format!("github.com/{r}").to_ascii_lowercase())
        }
        _ => None,
    };
    let pr = text(
        &args,
        &[
            "pullNumber",
            "pull_number",
            "prNumber",
            "pr_number",
            "number",
            "pr",
        ],
    );
    let has = |w: &str| name.contains(w);
    if has("auto_merge")
        || has("automerge")
        || (has("merge") && (has("pull") || has("_pr")) && !has("list") && !has("get"))
        || has("update_pull_request_branch")
        || has("update_pr_branch")
    {
        return Some(McpLanding::Merge { repo, pr });
    }
    if has("sync_fork") || has("fork_sync") || has("merge_upstream") || has("merge_branch") {
        return Some(McpLanding::Sync { repo });
    }
    if has("ready_for_review") || has("mark_ready") || has("pr_ready") {
        return Some(McpLanding::Flip { repo, pr });
    }
    if has("update_pull_request") && args.get("draft") == Some(&Value::Bool(false)) {
        return Some(McpLanding::Flip { repo, pr });
    }
    if has("label") && has("remove") {
        let labels = serde_json::to_string(&args).unwrap_or_default();
        if flip_label.is_some_and(|l| labels.contains(&format!("\"{l}\""))) {
            return Some(McpLanding::Flip { repo, pr });
        }
    }
    if has("create_or_update_file")
        || has("push_files")
        || has("delete_file")
        || has("create_commit")
        || has("update_ref")
        || has("create_file")
        || has("update_file")
    {
        let branch =
            text(&args, &["branch", "ref"]).map(|b| b.trim_start_matches("refs/heads/").to_owned());
        return Some(McpLanding::Write { repo, branch });
    }
    if (has("create_pull_request") || has("create_pr"))
        && args.get("draft") != Some(&Value::Bool(true))
    {
        return Some(McpLanding::NonDraftPr { repo });
    }
    None
}

/// The guard's rules for one MCP landing. MCP refusals are never lowered to the reason tier.
pub fn judge_mcp(m: &McpLanding, ctx: &guard::Ctx) -> Verdict {
    let deny = |rule: &'static str, why: String| Verdict::Deny {
        tier: Tier::Quote,
        rule,
        why,
    };
    let task_repo =
        |r: &str| ctx.task_active && ctx.task_repos.iter().any(|t| guard::same_repo(t, r));
    let tracked: Vec<&Tracked> = ctx.tracked.iter().collect();
    let has_tracked = |r: &str| tracked.iter().any(|t| guard::same_repo(&t.repo, r));
    let held = |r: &str, n: &str| {
        tracked.iter().any(|t| {
            guard::same_repo(&t.repo, r)
                && t.pr.as_deref().and_then(|p| p.rsplit('/').next())
                    == Some(n.trim_start_matches('#'))
        })
    };
    let (repo, what, pr, branch_ok) = match m {
        McpLanding::Merge { repo, pr } => (repo, "a PR merge on GitHub", pr.as_deref(), false),
        McpLanding::Flip { repo, pr } => (repo, "a ready flip on GitHub", pr.as_deref(), false),
        McpLanding::Sync { repo } => (repo, "a fork sync on GitHub", None, false),
        McpLanding::Write { repo, branch } => {
            let protected = branch
                .as_deref()
                .is_none_or(|b| ctx.protected.iter().any(|p| p == b));
            (
                repo,
                "a commit made on GitHub onto a protected or default branch",
                None,
                !protected,
            )
        }
        McpLanding::NonDraftPr { repo } => {
            return match repo {
                Some(r) if ctx.draft_repos.iter().any(|d| guard::same_repo(d, r)) => deny(
                    "draft_rule",
                    format!("{r} lands by the draft rule; open PRs there as drafts"),
                ),
                _ => Verdict::Allow,
            };
        }
    };
    if branch_ok {
        return Verdict::Allow;
    }
    let Some(r) = repo else {
        return deny(
            "fail_closed",
            format!("{what}, in a repository the guard could not name"),
        );
    };
    if task_repo(r) {
        return deny("task_repo", format!("{what} in {r}, a repository of the active task: Helm lands this task's changes itself"));
    }
    if has_tracked(r) && pr.is_none_or(|n| held(r, n)) {
        return deny(
            "containment",
            format!("{what} in {r}, which has tracked work: Helm lands tracked changes itself"),
        );
    }
    Verdict::Allow
}

/// The key an MCP override is bound to: the full tool name and its input, in canonical form.
fn call_key(tool: &str, input: &Value) -> String {
    fn canon(v: &Value) -> Value {
        match v {
            Value::Object(o) => {
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|k| (k.clone(), canon(&o[k])))
                        .collect(),
                )
            }
            Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
            v => v.clone(),
        }
    }
    format!("{tool} {}", canon(input))
}

fn mcp(db: &Database, chat: &str, c: &Check, f: &InForce, now: i64) -> rusqlite::Result<Answer> {
    let tool = c.tool.as_deref().unwrap_or("");
    let key = call_key(tool, &c.input);
    let mut states = states().lock().unwrap_or_else(|p| p.into_inner());
    let st = states.entry(chat.to_owned()).or_default();
    // The next root MCP call takes an armed override, whatever it is.
    let armed = if c.root { st.armed.take() } else { None };
    let Some(m) = mcp_landing(tool, &c.input, f.flip_label.as_deref()) else {
        return Ok(Answer::pass(true));
    };
    let v = judge_mcp(&m, &f.ctx);
    let Verdict::Deny { rule, why, .. } = &v else {
        return Ok(Answer::pass(true));
    };
    if !c.root {
        drop(states);
        gate_denied(db, chat, &f.run, "mcp", &key, rule, now)?;
        return Ok(Answer::refuse(format!(
            "Helm refused: {why}. A sub-agent can't override the landing guard: stop and report this to the orchestrator, which can ask the user."
        )));
    }
    if let Some((turn, k, d)) = armed {
        if turn == c.turn && k == key {
            drop(states);
            db.write(|tx| {
                overrides::record(
                    tx,
                    chat,
                    Some(&f.run),
                    "mcp",
                    "comment",
                    &d,
                    &key,
                    rule,
                    now,
                )
            })?
            .map_err(rusqlite::Error::InvalidParameterName)?;
            return Ok(Answer {
                context: Some(
                    "Helm recorded this override of the landing guard; the user sees it.".into(),
                ),
                ..Answer::pass(true)
            });
        }
    }
    st.offer = Some((c.turn.clone(), key.clone(), rule));
    drop(states);
    gate_denied(db, chat, &f.run, "mcp", &key, rule, now)?;
    Ok(Answer::refuse(format!(
        "Helm refused: {why}. This needs the user's permission: ask them, then run a Bash command that is only `{} <reason> [user: \"<their exact words>\"]` and send this exact call again. The user's words must be typed in this chat; a Telegram message does not count.{} {TURN_OFF}",
        overrides::MARKER,
        way_through(rule)
    )))
}

// ---- the panel's Needs you ------------------------------------------------------------------

/// The guard's overrides and refusals since the user last typed in this chat, newest first (at
/// most 8): the workflow panel lists them under Needs you.
pub fn needs_you(
    tx: &Transaction<'_>,
    chat: &str,
) -> rusqlite::Result<Vec<crate::types::GuardNoteView>> {
    let since: i64 = tx
        .query_row(
            "SELECT MAX(s.created_at) FROM submissions s JOIN submission_meta m ON m.submission_id = s.id
             WHERE s.chat_id = ?1 AND m.origin = 'typed'",
            [chat],
            |r| r.get::<_, Option<i64>>(0),
        )
        .unwrap_or(None)
        .unwrap_or(0);
    let mut st = tx.prepare(
        "SELECT kind, verdict, body FROM workflow_events
         WHERE chat_id = ?1 AND kind IN ('guard_override', 'gate_denied') AND created_at > ?2
         ORDER BY id DESC LIMIT 8",
    )?;
    let rows = st.query_map(params![chat, since], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, verdict, body) = row?;
        let body: Value = body
            .as_deref()
            .and_then(|b| serde_json::from_str(b).ok())
            .unwrap_or(Value::Null);
        let gate = body.get("gate").and_then(Value::as_str).unwrap_or("");
        if !matches!(gate, "bash" | "mcp") {
            continue;
        }
        let command: String = body
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        out.push(crate::types::GuardNoteView {
            overridden: kind == "guard_override",
            gate: gate.to_owned(),
            rule: verdict.unwrap_or_default(),
            command,
            verified: body
                .get("verified")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(out)
}

// ---- repository facts from git ----------------------------------------------------------------

/// Longest one git lookup may take, and all of them for one call.
const LOOKUP: Duration = Duration::from_secs(1);
const BUDGET: Duration = Duration::from_secs(4);

/// [`RepoFacts`] from git itself, each lookup time-limited and the whole call budgeted. A lookup
/// that fails or runs out of time says "can't tell", which makes the guard fail closed.
#[derive(Default)]
pub struct GitFacts {
    cache: Mutex<HashMap<(PathBuf, String), Lookup>>,
    spent: Mutex<Duration>,
}

/// What one git lookup said: whether it succeeded and its output, or `None` when it could not run.
type Lookup = Option<(bool, String)>;

impl GitFacts {
    /// `git -C <cwd> <args>`: whether it succeeded and its trimmed output; `None` when it could
    /// not run or ran out of time.
    fn git(&self, cwd: &Path, args: &[&str]) -> Option<(bool, String)> {
        let key = (cwd.to_path_buf(), args.join(" "));
        if let Some(v) = self.cache.lock().ok()?.get(&key) {
            return v.clone();
        }
        let v = self.run(cwd, args);
        self.cache.lock().ok()?.insert(key, v.clone());
        v
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Option<(bool, String)> {
        if *self.spent.lock().ok()? >= BUDGET || !cwd.is_dir() {
            return None;
        }
        let started = Instant::now();
        let mut child = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let done = loop {
            match child.try_wait() {
                Ok(Some(_)) => break true,
                Ok(None) if started.elapsed() < LOOKUP => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => break false,
            }
        };
        *self.spent.lock().ok()? += started.elapsed();
        if !done {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        let out = child.wait_with_output().ok()?;
        Some((
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        ))
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> Option<String> {
        match self.git(cwd, args) {
            Some((true, s)) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
}

/// `host/owner/name` of a remote address: `https://host/o/r.git`, `git@host:o/r`,
/// `ssh://git@host:22/o/r`.
pub fn remote_name(url: &str) -> Option<String> {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    let (host, path) = if let Some((_, rest)) = u.split_once("://") {
        let (h, p) = rest.split_once('/')?;
        (
            h.rsplit('@').next()?.split(':').next()?.to_owned(),
            p.to_owned(),
        )
    } else if let Some((h, p)) = u.split_once(':') {
        (h.rsplit('@').next()?.to_owned(), p.to_owned())
    } else {
        return None;
    };
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let [.., owner, name] = parts[..] else {
        return None;
    };
    settings::normalize_repo(&format!("{host}/{owner}/{name}"))
}

impl RepoFacts for GitFacts {
    fn repo(&self, cwd: &Path) -> Option<String> {
        self.remote_repo(cwd, "origin")
    }
    fn current_branch(&self, cwd: &Path) -> Option<String> {
        match self.git(cwd, &["symbolic-ref", "--short", "-q", "HEAD"])? {
            (true, b) if !b.is_empty() => Some(b),
            // Detached: on no branch.
            (false, _) => self
                .ok(cwd, &["rev-parse", "--verify", "-q", "HEAD"])
                .map(|_| "HEAD".into()),
            _ => None,
        }
    }
    fn default_branch(&self, cwd: &Path) -> Option<String> {
        let b = self.ok(
            cwd,
            &["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"],
        )?;
        Some(b.strip_prefix("origin/").unwrap_or(&b).to_owned())
    }
    fn remote_repo(&self, cwd: &Path, remote: &str) -> Option<String> {
        if remote.starts_with('-') {
            return None;
        }
        remote_name(&self.ok(cwd, &["remote", "get-url", "--", remote])?)
    }
    fn not_a_repo(&self, cwd: &Path) -> bool {
        // git ran and said this folder is in no repository.
        matches!(self.git(cwd, &["rev-parse", "--git-dir"]), Some((false, _)))
    }
    fn upstream(&self, cwd: &Path) -> Option<String> {
        self.ok(
            cwd,
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use crate::wftools::workflow_call;
    use helm_consent::AllowKinds;
    use helm_core::db::MigrationRegistry;

    const REPO: &str = "github.com/o/r";

    /// The repository is `o/r` (default branch `trunk`) and the chat is on `feat-x`.
    struct Here;

    impl RepoFacts for Here {
        fn repo(&self, _cwd: &Path) -> Option<String> {
            Some(REPO.into())
        }
        fn current_branch(&self, _cwd: &Path) -> Option<String> {
            Some("feat-x".into())
        }
        fn default_branch(&self, _cwd: &Path) -> Option<String> {
            Some("trunk".into())
        }
    }

    struct T {
        _d: tempfile::TempDir,
        db: Database,
        kinds: AllowKinds,
        turn: String,
    }

    fn setup() -> T {
        let mut reg = MigrationRegistry::new();
        helm_consent::register_migrations(&mut reg);
        crate::register_migrations(&mut reg);
        let d = tempfile::tempdir().unwrap();
        let db = Database::open(&d.path().join("t.sqlite3"), &reg).unwrap();
        let s = WorkflowSettings {
            enabled: true,
            new_chats: NewChats::Always,
            ..Default::default()
        };
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS submission_meta (submission_id TEXT PRIMARY KEY, origin TEXT NOT NULL DEFAULT 'user', priority TEXT, run_id TEXT);",
            )?;
            tx.execute("INSERT INTO projects(id, name, path, created_at) VALUES ('p', 'P', 'C:/w', 1)", [])?;
            for c in ["c", "off"] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, title, created_at, updated_at) VALUES (?1, 'p', 'T', 1, 1)",
                    [c],
                )?;
            }
            // This chat's Orchestrator is off: it hosts the user's own merges.
            tx.execute(
                "INSERT INTO chat_workflow_modes(chat_id, mode, set_by, updated_at) VALUES ('off', 'off', 'user', 1)",
                [],
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
            turn: "t1".into(),
        }
    }

    impl T {
        fn wf(&self, args: Value) {
            workflow_call(&self.db, &self.kinds, "c", false, &args, 10).unwrap();
        }
        fn says(&self, id: &str, text: &str, origin: &str, at: i64) {
            self.db
                .write::<_, rusqlite::Error>(|tx| {
                    tx.execute(
                        "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES (?1, 'c', ?2, 'done', ?3)",
                        params![id, text, at],
                    )?;
                    tx.execute(
                        "INSERT INTO submission_meta(submission_id, origin) VALUES (?1, ?2)",
                        params![id, origin],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        fn ask(&self, chat: &str, kind: &str, root: bool, what: &str, input: Value) -> Answer {
            let c = Check {
                id: "x".into(),
                kind: kind.into(),
                root,
                turn: self.turn.clone(),
                cwd: None,
                command: (kind != "mcp").then(|| what.to_owned()),
                tool: (kind == "mcp").then(|| what.to_owned()),
                input,
            };
            check(&self.db, chat, &c, &Here, 50)
        }
        fn bash(&self, cmd: &str) -> Answer {
            self.ask("c", "bash", true, cmd, Value::Null)
        }
        fn count(&self, kind: &str) -> i64 {
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
        fn last_override(&self) -> Value {
            let b: String = self
                .db
                .snapshot(|tx| {
                    tx.query_row(
                        "SELECT body FROM workflow_events WHERE kind = 'guard_override' ORDER BY id DESC LIMIT 1",
                        [],
                        |r| r.get(0),
                    )
                })
                .unwrap();
            serde_json::from_str(&b).unwrap()
        }
    }

    const MAIN: &str = "git push origin HEAD:main";
    const SHA_TO_MAIN: &str =
        "git push origin 0123456789abcdef0123456789abcdef01234567:refs/heads/main";

    #[test]
    fn the_guard_acts_only_in_a_task_chat_and_every_way_through_is_recorded() {
        let mut t = setup();
        // No task yet: nothing is read, and the answer says the guard is not in force.
        assert_eq!(t.bash(MAIN), Answer::pass(false));
        t.says("s0", "Start the hotfix task.", "typed", 5);
        t.wf(json!({"action": "start", "title": "Hotfix"}));
        t.wf(json!({"action": "update", "repo": REPO, "branch": "feat-x", "pr": "https://github.com/o/r/pull/7"}));
        // A chat with Orchestrator off is never checked, even with tracked work in its repo: this
        // is how the user's own fast-forward merge to main runs.
        for cmd in [MAIN, SHA_TO_MAIN] {
            assert_eq!(
                t.ask("off", "bash", true, cmd, Value::Null),
                Answer::pass(false),
                "{cmd}"
            );
        }
        // In the task chat: feature pushes pass, a landing is refused softly.
        assert_eq!(t.bash("git push origin feat-x"), Answer::pass(true));
        let a = t.bash(SHA_TO_MAIN);
        assert!(!a.allow && a.active);
        let why = a.reason.unwrap();
        for part in [
            "# helm-override:",
            "[user:",
            "Telegram",
            "hand_off",
            "Settings → Orchestrator",
        ] {
            assert!(why.contains(part), "{part} in {why}");
        }
        assert_eq!(t.count("gate_denied"), 1);
        // A sub-agent is told to go back to the orchestrator, even with an override line.
        let sub = t.ask(
            "c",
            "bash",
            false,
            &format!("# helm-override: x [user: \"Start the hotfix task.\"]\n{MAIN}"),
            Value::Null,
        );
        assert!(!sub.allow && sub.reason.unwrap().contains("sub-agent"));
        // A message that came from Telegram is not the user's typed word.
        t.says("s1", "go ahead and push the hotfix to main", "telegram", 20);
        let tg = t.bash(&format!(
            "# helm-override: asked [user: \"push the hotfix to main\"]\n{MAIN}"
        ));
        assert!(!tg.allow && tg.reason.unwrap().contains("do not match"));
        // The user's own typed words let the one command through, and it is recorded.
        t.says("s2", "Yes, push the hotfix to main now.", "typed", 30);
        let ok = t.bash(&format!(
            "# helm-override: hotfix [user: \"push the hotfix to main\"]\n{MAIN}"
        ));
        assert!(ok.allow && ok.context.is_some());
        let row = t.last_override();
        assert_eq!(
            (
                row["gate"].as_str(),
                row["tier"].as_str(),
                row["verified"].as_bool()
            ),
            (Some("bash"), Some("quote"), Some(true))
        );
        // The reason tier (a PR no tracked change holds) can also be resent once.
        let r1 = t.bash("gh pr merge 9");
        assert!(!r1.allow && r1.reason.unwrap().contains("same command again"));
        assert!(
            t.bash("gh pr merge 9").allow,
            "resent as the next Bash call"
        );
        assert_eq!(t.last_override()["path"], "resend");
        assert!(!t.bash("gh pr merge 9").allow, "a resend covers one call");
        t.turn = "t2".into();
        assert!(
            !t.bash("gh pr merge 9").allow,
            "and not a call in the next turn"
        );
        // MCP: a GitHub merge of the task's PR is refused; an override line arms it; the same call
        // then goes through once.
        let merge = json!({"owner": "o", "repo": "r", "pullNumber": 7});
        let refused = t.ask(
            "c",
            "mcp",
            true,
            "mcp__github__merge_pull_request",
            merge.clone(),
        );
        assert!(!refused.allow && refused.reason.unwrap().contains("send this exact call again"));
        let arm = t.bash("# helm-override: merge it [user: \"push the hotfix to main\"]");
        assert!(!arm.allow && arm.reason.unwrap().contains("armed"));
        assert!(
            t.ask(
                "c",
                "mcp",
                true,
                "mcp__github__merge_pull_request",
                merge.clone()
            )
            .allow
        );
        assert_eq!(t.last_override()["gate"], "mcp");
        assert!(
            !t.ask(
                "c",
                "mcp",
                true,
                "mcp__github__merge_pull_request",
                merge.clone()
            )
            .allow,
            "once"
        );
        // Through Helm's own MCP tool, the inner tool is judged.
        let via = json!({"name": "mcp_github_merge_pull_request_1a2b3c", "arguments": merge});
        assert!(
            !t.ask("c", "mcp", true, "mcp__helm__app_mcp_call", via)
                .allow
        );
        assert!(
            t.ask(
                "c",
                "mcp",
                true,
                "mcp__github__get_pull_request",
                json!({"owner": "o", "repo": "r", "pullNumber": 7})
            )
            .allow
        );
        // The panel lists what happened until the user next types.
        let notes = t.db.snapshot(|tx| needs_you(tx, "c")).unwrap();
        assert!(
            notes.iter().any(|n| !n.overridden)
                && notes.iter().any(|n| n.overridden && n.gate == "mcp")
        );
        t.says("s3", "thanks", "typed", 60);
        assert!(t.db.snapshot(|tx| needs_you(tx, "c")).unwrap().is_empty());
        // The off switch: the guard stops checking at once.
        t.db.write::<_, rusqlite::Error>(|tx| {
            let mut s = settings::load(tx)?;
            s.landing_guard = false;
            settings::store(tx, &s, 70)
        })
        .unwrap();
        assert_eq!(t.bash(MAIN), Answer::pass(false));
    }

    #[test]
    fn github_tools_that_land_are_read_from_their_names_and_inputs() {
        let task = guard::Ctx {
            task_active: true,
            task_repos: vec![REPO.into()],
            tracked: vec![Tracked {
                repo: "github.com/o/other".into(),
                branch: "x".into(),
                pr: Some("https://github.com/o/other/pull/3".into()),
            }],
            protected: vec!["main".into()],
            draft_repos: vec![REPO.into()],
            ..Default::default()
        };
        let rule = |tool: &str, input: Value| match mcp_landing(tool, &input, Some("needs-review"))
        {
            None => "none",
            Some(m) => match judge_mcp(&m, &task) {
                Verdict::Allow => "allow",
                Verdict::Deny { rule, .. } => rule,
            },
        };
        let r = |extra: Value| {
            let mut v = json!({"owner": "o", "repo": "r"});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            v
        };
        for (tool, input, want) in [
            (
                "mcp__github__merge_pull_request",
                r(json!({"pullNumber": 7})),
                "task_repo",
            ),
            (
                "mcp__github__enable_auto_merge",
                r(json!({"pullNumber": 7})),
                "task_repo",
            ),
            (
                "mcp__github__update_pull_request_branch",
                r(json!({"pullNumber": 7})),
                "task_repo",
            ),
            (
                "mcp__github__update_pull_request",
                r(json!({"pullNumber": 7, "draft": false})),
                "task_repo",
            ),
            (
                "mcp__github__update_pull_request",
                r(json!({"pullNumber": 7, "title": "x"})),
                "none",
            ),
            (
                "mcp__github__remove_label",
                r(json!({"issue_number": 7, "label": "needs-review"})),
                "task_repo",
            ),
            (
                "mcp__github__create_or_update_file",
                r(json!({"branch": "main", "path": "a"})),
                "task_repo",
            ),
            (
                "mcp__github__push_files",
                r(json!({"branch": "feat-x"})),
                "allow",
            ),
            ("mcp__github__delete_file", r(json!({})), "task_repo"),
            (
                "mcp__github__create_pull_request",
                r(json!({"draft": false})),
                "draft_rule",
            ),
            (
                "mcp__github__create_pull_request",
                r(json!({"draft": true})),
                "none",
            ),
            (
                "mcp__github__merge_pull_request",
                json!({"pullNumber": 7}),
                "fail_closed",
            ),
            (
                "mcp__github__merge_pull_request",
                json!({"owner": "o", "repo": "other", "pullNumber": 3}),
                "containment",
            ),
            (
                "mcp__github__merge_pull_request",
                json!({"owner": "o", "repo": "other", "pullNumber": 4}),
                "allow",
            ),
            (
                "mcp__github__merge_pull_request",
                json!({"owner": "o", "repo": "free", "pullNumber": 4}),
                "allow",
            ),
            ("mcp__github__list_pull_requests", r(json!({})), "none"),
            (
                "mcp__github__get_pull_request",
                r(json!({"pullNumber": 7})),
                "none",
            ),
            (
                "mcp__helm__app_mcp_call",
                json!({"name": "mcp_github_merge_pull_request_9f8e7d", "arguments": r(json!({"pullNumber": 7}))}),
                "task_repo",
            ),
            (
                "mcp__helm__app_mcp_call",
                json!({"name": "mcp_agent_standup_get_item_296490d2ada4", "arguments": {}}),
                "none",
            ),
        ] {
            assert_eq!(rule(tool, input.clone()), want, "{tool} {input}");
        }
    }

    #[test]
    fn git_facts_read_the_folder_the_way_git_does() {
        for (url, want) in [
            ("https://github.com/O/R.git", Some("github.com/o/r")),
            ("git@github.com:o/r.git", Some("github.com/o/r")),
            ("ssh://git@git.example:22/o/r", Some("git.example/o/r")),
            (
                "https://user@git.example/group/o/r/",
                Some("git.example/o/r"),
            ),
            ("not a url", None),
        ] {
            assert_eq!(remote_name(url).as_deref(), want, "{url}");
        }
        let repo = tempfile::tempdir().unwrap();
        let plain = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "feat-x"]);
        git(&["remote", "add", "origin", "https://github.com/O/R.git"]);
        let f = GitFacts::default();
        assert_eq!(f.repo(repo.path()).as_deref(), Some(REPO));
        assert_eq!(f.current_branch(repo.path()).as_deref(), Some("feat-x"));
        assert!(!f.not_a_repo(repo.path()));
        assert!(
            f.not_a_repo(plain.path()),
            "a folder outside any repository"
        );
        assert!(
            !f.not_a_repo(&plain.path().join("missing")),
            "a missing folder can't be told"
        );
        assert_eq!(f.upstream(repo.path()), None);
    }
}
