//! Startup instructions: what a project wants done first in every new chat on it.
//!
//! A project carries optional startup instructions (`project_local.startup_instructions`: make a
//! worktree and branch, start the local servers, send the user the links). The chat's own board
//! decides when it has instructions, else its folder project. They reach the agent in three steps:
//!
//! 1. **The first message carries them.** [`Startup`] is the chats crate's startup seam: a chat
//!    that has not started gets the instructions as hidden context on its first message, and one
//!    visible transcript line says so. The message that carried them is remembered
//!    (`chat_startup_sent`), so a second message sent before the first runs does not repeat them,
//!    and a first message that never went out hands them to the next one.
//! 2. **The agent reports.** `app_startup_done` records what it did (`chat_startup`).
//! 3. **The turn may not end before that.** The worker's Stop hook asks `app_startup_turn_end`
//!    (never listed for agents); it blocks while the chat carries the instructions and has not
//!    reported. [`owes_startup`] is the one test, shared with the launch variable that switches the
//!    hook on ([`crate::launch::ProjectLaunch`]).
//!
//! A roll-over successor is told its predecessor's startup is done and to carry on with the same
//! worktree and servers, when the predecessor had reported; otherwise it gets the instructions as
//! any new chat would.
//!
//! An agent reads them with `app_project_startup {action: get}` and changes them with
//! `{action: set}`, which stages one Approve card (allow kind [`KIND`]) that Helm applies on the
//! click, as for the settings an agent may not change itself.

use crate::store::{self, Res};
use crate::types::{ProjectNode, ProjectUpdateArgs};
use helm_chats::seams::{StartupInjection, StartupSeam};
use helm_consent::store::{self as consent, NewRequest};
use helm_consent::{AllowKind, AllowKinds, Applier, ApplyJob, Canonical};
use helm_core::db::Database;
use helm_hosttools::{Condition, HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use helm_ipc::AppEvent;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

pub const KIND: &str = "project_startup";
pub const DONE_TOOL: &str = "app_startup_done";
pub const TURN_END_TOOL: &str = "app_startup_turn_end";
pub const PROJECT_TOOL: &str = "app_project_startup";

/// The longest text an agent may stage: the Approve card shows all of it, and a card holds 1,200
/// characters. Longer instructions are typed in Helm's own dialog.
pub const MAX_STAGED_CHARS: usize = 700;
/// A staged card line is cut here, so no line of the text is lost to the card's own line limit.
const CARD_WRAP: usize = 200;

const MAX_STEPS: usize = 20;
const MAX_STEP_CHARS: usize = 200;
const MAX_ADDRESSES: usize = 10;
const MAX_PATH_CHARS: usize = 300;
const MAX_NOTES_CHARS: usize = 1000;

/// The reason the Stop hook gives while the report is missing.
pub const BLOCK_REASON: &str = "This project has startup instructions and they are not reported yet. Finish them and call app_startup_done (or app_startup_done with continued: true after a roll-over) before ending this turn.";

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_startup (
             chat_id  TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             done_at  INTEGER NOT NULL,
             state    TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_startup_sent (
             chat_id        TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             submission_id  TEXT NOT NULL,
             kind           TEXT NOT NULL CHECK (kind IN ('start', 'continue')),
             at             INTEGER NOT NULL
         );",
    )
}

// ---- which instructions apply ---------------------------------------------------------------

/// The instructions that apply to a chat, and the project they hang on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupSet {
    pub project_id: String,
    pub project_name: String,
    pub text: String,
}

fn instructions_of(tx: &Transaction<'_>, project_id: &str) -> rusqlite::Result<Option<StartupSet>> {
    Ok(tx
        .query_row(
            "SELECT p.id, COALESCE(l.title_override, b.title, p.name), l.startup_instructions
             FROM projects p
             LEFT JOIN project_local l ON l.project_id = p.id
             LEFT JOIN board_projects b ON b.project_id = p.id
             WHERE p.id = ?1",
            [project_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
        .and_then(|(project_id, project_name, text)| {
            text.filter(|t| !t.trim().is_empty())
                .map(|text| StartupSet {
                    project_id,
                    project_name,
                    text,
                })
        }))
}

/// The chat's board when it has instructions, otherwise its folder project when that has them.
pub fn set_for_chat(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Option<StartupSet>> {
    if let Some(board) = crate::boards::chat_board(tx, chat_id)? {
        if let Some(s) = instructions_of(tx, &board)? {
            return Ok(Some(s));
        }
    }
    let folder: Option<String> = tx
        .query_row(
            "SELECT project_id FROM chats WHERE id = ?1",
            [chat_id],
            |r| r.get(0),
        )
        .optional()?;
    match folder {
        Some(f) => instructions_of(tx, &f),
        None => Ok(None),
    }
}

// ---- what a chat was told and what it reported ----------------------------------------------

/// The submission that carried the startup text to this chat, while that message still stands (a
/// message the chat refused, withdrew or replaced carried nothing).
fn carried_by(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT s.submission_id FROM chat_startup_sent s
         JOIN submissions m ON m.id = s.submission_id
         WHERE s.chat_id = ?1 AND m.state NOT IN ('rejected', 'withdrawn', 'superseded')",
        [chat_id],
        |r| r.get(0),
    )
    .optional()
}

/// True while the chat was told its startup and has not reported it: the Stop hook blocks, and the
/// launch switches the hook on.
pub fn owes_startup(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<bool> {
    if carried_by(tx, chat_id)?.is_none() {
        return Ok(false);
    }
    let done: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_startup WHERE chat_id = ?1)",
        [chat_id],
        |r| r.get(0),
    )?;
    Ok(!done)
}

/// What a chat reported, as stored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    #[serde(default)]
    continued: bool,
}

impl Report {
    /// The state as short lines, for a successor to carry on from.
    fn lines(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        if let Some(w) = &self.worktree {
            out.push(format!("worktree: {w}"));
        }
        if let Some(b) = &self.branch {
            out.push(format!("branch: {b}"));
        }
        for a in &self.addresses {
            out.push(format!("address: {a}"));
        }
        if let Some(n) = &self.notes {
            out.push(format!("notes: {n}"));
        }
        if out.is_empty() {
            out.push("no worktree or address was recorded".to_owned());
            if !self.steps.is_empty() {
                out.push(format!("steps done: {}", self.steps.join("; ")));
            }
        }
        out.join("\n")
    }
}

/// The report of the chat a successor continues, and that chat's title.
fn predecessor_report(
    tx: &Transaction<'_>,
    predecessor: &str,
) -> rusqlite::Result<Option<(String, Report)>> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT c.title, s.state FROM chat_startup s JOIN chats c ON c.id = s.chat_id
             WHERE s.chat_id = ?1",
            [predecessor],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(title, state)| (title, serde_json::from_str(&state).unwrap_or_default())))
}

// ---- the chats crate's seam -----------------------------------------------------------------

/// The text a new chat's first message carries.
pub fn start_text(instructions: &str) -> String {
    format!(
        "Startup instructions for this project. Do these first, in order, before any other work. When they are done, call app_startup_done with the steps you completed (and the worktree, branch and local addresses if you made any). Then continue with the user's message.\n\n{instructions}"
    )
}

/// The text a roll-over successor's first message carries.
pub fn continue_text(old_title: &str, state_lines: &str) -> String {
    format!(
        "This chat continues \"{old_title}\". Its startup was already done: do not rebuild. Continue with the same worktree and local servers:\n{state_lines}\nCheck they are still up; restart only what is down. Then call app_startup_done with continued: true."
    )
}

pub struct Startup;

impl StartupSeam for Startup {
    fn first_send(
        &self,
        tx: &Transaction<'_>,
        chat_id: &str,
        submission_id: &str,
        predecessor: Option<&str>,
    ) -> rusqlite::Result<Option<StartupInjection>> {
        // A side chat is a quick aside: it never runs a project's setup.
        let side: bool = tx.query_row(
            "SELECT side_source_id IS NOT NULL FROM chats WHERE id = ?1",
            [chat_id],
            |r| r.get(0),
        )?;
        if side {
            return Ok(None);
        }
        // Already carried by a message that still stands.
        if carried_by(tx, chat_id)?.is_some_and(|s| s != submission_id) {
            return Ok(None);
        }
        let carry_on = match predecessor {
            Some(p) => predecessor_report(tx, p)?,
            None => None,
        };
        let (kind, injection) = if let Some((title, report)) = carry_on {
            (
                "continue",
                StartupInjection {
                    context: continue_text(&title, &report.lines()),
                    row: format!(
                        "This chat continues the startup of \"{title}\": its worktree and local servers carry over"
                    ),
                },
            )
        } else if let Some(set) = set_for_chat(tx, chat_id)? {
            (
                "start",
                StartupInjection {
                    context: start_text(&set.text),
                    row: format!(
                        "Startup instructions for {} were added to this chat",
                        set.project_name
                    ),
                },
            )
        } else {
            return Ok(None);
        };
        tx.execute(
            "INSERT OR REPLACE INTO chat_startup_sent(chat_id, submission_id, kind, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![chat_id, submission_id, kind, helm_core::now_ms()],
        )?;
        Ok(Some(injection))
    }
}

// ---- the agent's tools ----------------------------------------------------------------------

fn refused(m: impl Into<String>) -> ToolError {
    ToolError::refused(m.into())
}

fn text_arg(args: &Value, key: &str, max: usize) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(None);
            }
            if t.chars().count() > max || helm_core::hidden::has_hidden(t) {
                return Err(refused(format!(
                    "{key} must be plain text of at most {max} characters."
                )));
            }
            Ok(Some(t.to_owned()))
        }
        Some(_) => Err(refused(format!("{key} must be text."))),
    }
}

fn list_arg(
    args: &Value,
    key: &str,
    max_items: usize,
    max: usize,
) -> Result<Vec<String>, ToolError> {
    let Some(v) = args.get(key).filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let items = v
        .as_array()
        .ok_or_else(|| refused(format!("{key} must be a list of text.")))?;
    if items.len() > max_items {
        return Err(refused(format!("{key} takes at most {max_items} entries.")));
    }
    items
        .iter()
        .map(|i| {
            let t = i.as_str().map(str::trim).unwrap_or("");
            if t.is_empty() || t.chars().count() > max || helm_core::hidden::has_hidden(t) {
                return Err(refused(format!(
                    "Each entry of {key} is plain text of 1 to {max} characters."
                )));
            }
            Ok(t.to_owned())
        })
        .collect()
}

fn report_from(args: &Value) -> Result<Report, ToolError> {
    let steps = list_arg(args, "steps", MAX_STEPS, MAX_STEP_CHARS)?;
    if steps.is_empty() {
        return Err(refused("steps needs at least one entry: what you did."));
    }
    Ok(Report {
        steps,
        worktree: text_arg(args, "worktree", MAX_PATH_CHARS)?,
        branch: text_arg(args, "branch", MAX_STEP_CHARS)?,
        addresses: list_arg(args, "addresses", MAX_ADDRESSES, MAX_STEP_CHARS)?,
        notes: text_arg(args, "notes", MAX_NOTES_CHARS)?,
        continued: args
            .get("continued")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn done(ctx: &ToolCtx, args: Value) -> ToolResult {
    let report = report_from(&args)?;
    let db = ctx
        .services
        .get::<Database>()
        .ok_or_else(|| refused("This tool is not available here."))?;
    let state = serde_json::to_string(&report).map_err(|e| refused(e.to_string()))?;
    db.write(|tx| {
        tx.execute(
            "INSERT OR REPLACE INTO chat_startup(chat_id, done_at, state) VALUES (?1, ?2, ?3)",
            params![ctx.caller_chat_id, helm_core::now_ms(), state],
        )
    })
    .map_err(|e| refused(format!("The report was not saved: {e}")))?;
    Ok(json!({"recorded": true}))
}

fn turn_end(ctx: &ToolCtx, _args: Value) -> ToolResult {
    let Some(db) = ctx.services.get::<Database>() else {
        return Ok(json!({"block": false}));
    };
    match db.snapshot(|tx| owes_startup(tx, &ctx.caller_chat_id)) {
        Ok(true) => Ok(json!({"block": true, "reason": BLOCK_REASON})),
        Ok(false) => Ok(json!({"block": false})),
        Err(e) => {
            log::warn!("chat {}: the startup check failed: {e}", ctx.caller_chat_id);
            Ok(json!({"block": false}))
        }
    }
}

/// The project a `get` or `set` is about: the one named, else the one whose instructions this
/// chat's own launch reads (its board, else its folder project).
fn target_project(tx: &Transaction<'_>, chat_id: &str, named: Option<&str>) -> Res<ProjectNode> {
    if let Some(id) = named {
        return store::get_project(tx, id);
    }
    let id = match crate::boards::chat_board(tx, chat_id)? {
        Some(b) => b,
        None => tx
            .query_row(
                "SELECT project_id FROM chats WHERE id = ?1",
                [chat_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| store::Refusal::NotFound("That chat does not exist.".into()))?,
    };
    store::get_project(tx, &id)
}

fn project_startup(ctx: &ToolCtx, args: Value) -> ToolResult {
    let db = ctx
        .services
        .get::<Database>()
        .ok_or_else(|| refused("This tool is not available here."))?;
    let named = args.get("projectId").and_then(Value::as_str);
    let node = db
        .snapshot(|tx| target_project(tx, &ctx.caller_chat_id, named))
        .map_err(|e| refused(refusal_text(&e)))?;
    match args.get("action").and_then(Value::as_str) {
        Some("get") => Ok(json!({
            "projectId": node.id,
            "name": node.name,
            "startupInstructions": node.startup_instructions,
            "characters": node.startup_instructions.as_deref().map_or(0, |t| t.chars().count()),
        })),
        Some("set") => stage_set(ctx, &db, &node, &args),
        _ => Err(refused("action is get or set.")),
    }
}

fn refusal_text(r: &store::Refusal) -> String {
    match r {
        store::Refusal::BadArgs(m)
        | store::Refusal::NotFound(m)
        | store::Refusal::Conflict(m)
        | store::Refusal::Refused(m) => m.clone(),
        store::Refusal::Sql(e) => e.to_string(),
    }
}

/// The canonical target: what the card is built from and what Helm applies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Staged {
    project_id: String,
    project_name: String,
    /// The instructions as they stand when staged; a change after that refuses the approval.
    from: Option<String>,
    /// The new instructions; `None` clears.
    to: Option<String>,
}

fn stage_set(ctx: &ToolCtx, db: &Arc<Database>, node: &ProjectNode, args: &Value) -> ToolResult {
    let fail = |m: String| ToolError::refused(format!("[Helm] Nothing was staged: {m}"));
    let to = match args.get("instructions") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => store::validate_startup(s).map_err(|e| fail(refusal_text(&e)))?,
        Some(_) => return Err(fail("instructions must be text (empty clears).".into())),
    };
    if to
        .as_deref()
        .is_some_and(|t| t.chars().count() > MAX_STAGED_CHARS)
    {
        return Err(fail(format!(
            "an agent may stage at most {MAX_STAGED_CHARS} characters, so that the Approve card shows all of them; the user can type longer instructions in the project's Edit startup instructions dialog."
        )));
    }
    if to == node.startup_instructions {
        return Err(fail("those are the instructions already.".into()));
    }
    let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
    let staged = Staged {
        project_id: node.id.clone(),
        project_name: node.name.chars().take(80).collect(),
        from: node.startup_instructions.clone(),
        to,
    };
    let kinds = match ctx.services.get::<AllowKinds>() {
        Some(k) if k.applies_on_allow(KIND) => k,
        _ => {
            return Err(fail(
                "changing startup instructions is not available here.".into(),
            ))
        }
    };
    let target = serde_json::to_value(&staged).map_err(|e| fail(e.to_string()))?;
    let r = consent::request(
        db,
        &kinds,
        NewRequest {
            chat_id: &ctx.caller_chat_id,
            kind: KIND,
            target: &target,
            reason,
            evidence: "",
            grant_minutes: None,
            tool_use_id: ctx.tool_call_id.as_deref(),
        },
        helm_core::now_ms(),
    )
    .map_err(|r| fail(r.0))?;
    ctx.events
        .emit(AppEvent::new("consent").with("chatId", ctx.caller_chat_id.clone()));
    if let Some(w) = ctx.services.get::<helm_consent::ConsentWake>() {
        w.0.notify_one();
    }
    Ok(json!({
        "requestId": r.id,
        "state": "pending",
        "note": "The user now sees an Approve card in this chat showing the new startup instructions. Helm applies them when the user approves and sends you the outcome as a note; do not call again or poll.",
    }))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write(
            DONE_TOOL,
            "Report that this chat has done its project's startup instructions: the steps you completed, and the worktree, branch and local addresses you made, so a roll-over successor can carry on with them. Call it once, after the instructions are done. After a roll-over, check the old worktree and servers are still up, then call it with continued: true.",
            json!({
                "type": "object",
                "properties": {
                    "steps": {"type": "array", "minItems": 1, "maxItems": MAX_STEPS, "items": {"type": "string", "maxLength": MAX_STEP_CHARS}},
                    "worktree": {"type": "string", "maxLength": MAX_PATH_CHARS},
                    "branch": {"type": "string", "maxLength": MAX_STEP_CHARS},
                    "addresses": {"type": "array", "maxItems": MAX_ADDRESSES, "items": {"type": "string", "maxLength": MAX_STEP_CHARS}},
                    "notes": {"type": "string", "maxLength": MAX_NOTES_CHARS},
                    "continued": {"type": "boolean"}
                },
                "required": ["steps"],
                "additionalProperties": false
            }),
        ),
        done,
    )
    .blocking(
        ToolSpec::read(
            TURN_END_TOOL,
            "Helm's own question when a turn is about to end in a chat that was told its project's startup instructions. The worker asks it; agents never see it.",
            json!({"type": "object", "properties": {}}),
        )
        .when(Condition::Worker)
        .self_locking(),
        turn_end,
    )
    .blocking(
        ToolSpec::write(
            PROJECT_TOOL,
            format!("Read or change a project's startup instructions: what every new chat on it does first (a worktree and branch, local servers, links for the user). get is free and defaults to this chat's board, else its folder project. set stages one Approve card showing the new text (at most {MAX_STAGED_CHARS} characters; empty clears), and Helm applies it only when the user approves. Use set only when the user asked for it."),
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["get", "set"]},
                    "projectId": {"type": "string"},
                    "instructions": {"type": "string", "maxLength": MAX_STAGED_CHARS},
                    "reason": {"type": "string", "maxLength": 1000}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        )
        .self_locking(),
        project_startup,
    );
}

// ---- the Approve card and applying ----------------------------------------------------------

/// The card: a title, what stands now, then the new text whole (wrapped, never cut).
fn card_lines(s: &Staged) -> Result<Vec<String>, String> {
    let mut lines = vec![match &s.to {
        Some(_) => format!("Change the startup instructions of {}", s.project_name),
        None => format!("Clear the startup instructions of {}", s.project_name),
    }];
    lines.push(match &s.from {
        Some(t) => format!("Replaces {} characters of instructions.", t.chars().count()),
        None => "There are none now.".to_owned(),
    });
    if let Some(t) = &s.to {
        lines.push(format!("New text ({} characters):", t.chars().count()));
        for raw in t.lines() {
            let chars: Vec<char> = raw.chars().collect();
            if chars.is_empty() {
                continue;
            }
            for part in chars.chunks(CARD_WRAP) {
                lines.push(part.iter().collect());
            }
        }
    }
    let fits = lines
        .iter()
        .all(|l| l.chars().count() <= helm_consent::kinds::MAX_SUMMARY_LINE)
        && lines.join("\n").chars().count() <= helm_consent::kinds::MAX_SUMMARY;
    if !fits {
        return Err("These instructions are too long to show on one card.".into());
    }
    Ok(lines)
}

fn canonical(_tx: &Transaction<'_>, _chat: &str, target: &Value) -> Result<Canonical, String> {
    let staged: Staged = serde_json::from_value(target.clone())
        .map_err(|_| format!("Stage this change with {PROJECT_TOOL}."))?;
    let lines = card_lines(&staged)?;
    Ok(Canonical {
        target: serde_json::to_value(&staged).map_err(|e| e.to_string())?,
        summary: lines.join("\n"),
        fixed_form: String::new(),
        details: Vec::new(),
    })
}

/// This module's allow kind, applied by Helm on Approve.
pub fn register_allow_kinds(k: &mut AllowKinds) {
    k.register_applying(
        AllowKind {
            id: KIND,
            label: "Change a project's startup instructions",
            accepts_words: false,
            canonical,
        },
        Arc::new(StartupApplier),
    );
}

pub struct StartupApplier;

impl Applier for StartupApplier {
    fn staged_with(&self) -> &'static str {
        PROJECT_TOOL
    }

    fn apply(&self, job: &ApplyJob<'_>) -> String {
        let Ok(s) = serde_json::from_value::<Staged>(job.target.clone()) else {
            return "Nothing changed: the staged change could not be read.".into();
        };
        let Some(db) = job.services.get::<Database>() else {
            return "Nothing changed: the projects are not available.".into();
        };
        let done = db.write(|tx| {
            let node = store::get_project(tx, &s.project_id)?;
            if node.startup_instructions != s.from {
                return Ok::<_, store::Refusal>(Err(
                    "Nothing changed: the instructions were edited after this card was made. Stage the change again.",
                ));
            }
            store::update_project(
                tx,
                &ProjectUpdateArgs {
                    project_id: s.project_id.clone(),
                    revision: node.revision,
                    startup_instructions: Some(s.to.clone()),
                    ..Default::default()
                },
            )?;
            Ok(Ok(store::get_project(tx, &s.project_id)?))
        });
        match done {
            Ok(Ok(node)) => {
                if let Ok(v) = serde_json::to_value(&node) {
                    job.events.emit(AppEvent::new("project").with("project", v));
                }
                match s.to {
                    Some(t) => format!(
                        "Applied. The startup instructions of {} are now {} characters long; chats started from now on carry them.",
                        s.project_name,
                        t.chars().count()
                    ),
                    None => format!(
                        "Applied. {} has no startup instructions now.",
                        s.project_name
                    ),
                }
            }
            Ok(Err(m)) => m.into(),
            Err(e) => format!("Nothing changed: {}", refusal_text(&e)),
        }
    }
}
