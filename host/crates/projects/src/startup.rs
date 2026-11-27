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
//!
//! The startup set also holds **context files** (listed in the first message, never pasted; an
//! agent reads one with `app_help project/<name>`) and the **MCP servers** the project's chats get
//! (the others are left out of the chat's server list). An agent changes these two with
//! `app_update_project` (`startupFiles`, `startupServers`), like the project's other details.

use crate::store::{self, Res};
use crate::types::{ProjectNode, ProjectUpdateArgs};
use helm_chats::seams::{StartupInjection, StartupSeam};
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

/// The longest text an agent may set: the same limit as the text typed in Helm's own dialog.
pub const MAX_STAGED_CHARS: usize = 4000;

const MAX_STEPS: usize = 20;
const MAX_STEP_CHARS: usize = 200;
const MAX_ADDRESSES: usize = 10;
const MAX_PATH_CHARS: usize = 300;
const MAX_NOTES_CHARS: usize = 1000;

/// The reason the Stop hook gives while the report is missing.
pub const BLOCK_REASON: &str = "This project has startup instructions and they are not reported yet. Finish them and call app_startup_done through app_mcp_call (or with continued: true after a roll-over) before ending this turn.";

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
    /// The instructions; empty when the project has only files or servers.
    pub text: String,
    pub files: Vec<String>,
    pub servers: Vec<String>,
}

fn instructions_of(tx: &Transaction<'_>, project_id: &str) -> rusqlite::Result<Option<StartupSet>> {
    Ok(tx
        .query_row(
            "SELECT p.id, COALESCE(l.title_override, b.title, p.name), l.startup_instructions,
                    l.startup_files, l.startup_servers
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
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?
        .and_then(|(project_id, project_name, text, files, servers)| {
            let text = text.filter(|t| !t.trim().is_empty()).unwrap_or_default();
            let files = store::json_list(files);
            let servers = store::json_list(servers);
            (!text.is_empty() || !files.is_empty() || !servers.is_empty()).then_some(StartupSet {
                project_id,
                project_name,
                text,
                files,
                servers,
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

/// True while the chat was told its startup instructions (or told to carry on its predecessor's)
/// and has not reported it: the Stop hook blocks, and the launch switches the hook on. A project
/// that has only context files or servers gives a chat nothing to report.
pub fn owes_startup(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<bool> {
    if carried_by(tx, chat_id)?.is_none() {
        return Ok(false);
    }
    let done: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_startup WHERE chat_id = ?1)",
        [chat_id],
        |r| r.get(0),
    )?;
    if done {
        return Ok(false);
    }
    let kind: String = tx.query_row(
        "SELECT kind FROM chat_startup_sent WHERE chat_id = ?1",
        [chat_id],
        |r| r.get(0),
    )?;
    if kind == "continue" {
        return Ok(true);
    }
    Ok(set_for_chat(tx, chat_id)?.is_some_and(|s| !s.text.is_empty()))
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
        "Startup instructions for this project. Do these first, in order, before any other work. When they are done, call app_startup_done (not in your tool list: call it with app_mcp_call) with the steps you completed (and the worktree, branch and local addresses if you made any). Then continue with the user's message.\n\n{instructions}"
    )
}

/// What a project's context files and servers add to the first message, or nothing.
pub fn context_text(set: &StartupSet) -> String {
    let mut out = String::new();
    if !set.files.is_empty() {
        out.push_str("Context files for this project. Read the ones the work needs before you start; they are not pasted here (app_help project/<name> reads one in parts):\n");
        for (i, f) in set.files.iter().enumerate() {
            out.push_str(&format!(
                "- {f}  (app_help project/{})\n",
                file_topic(&set.files, i)
            ));
        }
    }
    if !set.servers.is_empty() {
        out.push_str(&format!(
            "MCP servers for this project: {}. They are the only ones app_mcp_tools finds in this chat.\n",
            set.servers.join(", ")
        ));
    }
    out.trim_end().to_owned()
}

/// A context file's help topic: its file name without the extension, kept apart from the others.
fn file_topic(files: &[String], index: usize) -> String {
    let stem = |p: &str| {
        let name = p.rsplit(['/', '\\']).next().unwrap_or(p);
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
        let clean: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        if clean.is_empty() {
            "file".to_owned()
        } else {
            clean
        }
    };
    let mine = stem(&files[index]);
    let earlier = files[..index].iter().filter(|p| stem(p) == mine).count();
    if earlier == 0 {
        mine
    } else {
        format!("{mine}-{}", earlier + 1)
    }
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
            let extra = context_text(&set);
            let (context, row) = if set.text.is_empty() {
                (
                    extra,
                    format!("Context for {} was added to this chat", set.project_name),
                )
            } else {
                let mut c = start_text(&set.text);
                if !extra.is_empty() {
                    c.push_str("\n\n");
                    c.push_str(&extra);
                }
                (
                    c,
                    format!(
                        "Startup instructions for {} were added to this chat",
                        set.project_name
                    ),
                )
            };
            ("start", StartupInjection { context, row })
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
            "contextFiles": node.startup_files,
            "mcpServers": node.startup_servers,
            "note": "Context files and MCP servers are changed with app_update_project (startupFiles, startupServers); set here saves the instructions at once, with a notice and Undo for the user.",
        })),
        Some("set") => set_now(ctx, &db, &node, &args),
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

/// Change the instructions at once: a change a chat makes to its own prompt, with a notice and
/// Undo for the user (the change ring, [`crate::fields`]).
fn set_now(ctx: &ToolCtx, db: &Arc<Database>, node: &ProjectNode, args: &Value) -> ToolResult {
    let to = match args.get("instructions") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            store::validate_startup(s).map_err(|e| refused(refusal_text(&e)))?
        }
        Some(_) => return Err(refused("instructions must be text (empty clears).")),
    };
    if to == node.startup_instructions {
        return Err(refused("those are the instructions already."));
    }
    let done = db.write(|tx| {
        let before = crate::fields::snapshot(tx, &node.id)?;
        let now = store::get_project(tx, &node.id)?;
        store::update_project(
            tx,
            &ProjectUpdateArgs {
                project_id: node.id.clone(),
                revision: now.revision,
                startup_instructions: Some(to.clone()),
                ..Default::default()
            },
        )?;
        let after = crate::fields::snapshot(tx, &node.id)?;
        Ok::<_, store::Refusal>((before, after, store::get_project(tx, &node.id)?))
    });
    let (before, after, saved) = done.map_err(|e| refused(refusal_text(&e)))?;
    if let Ok(v) = serde_json::to_value(&saved) {
        ctx.events.emit(AppEvent::new("project").with("project", v));
    }
    let mut out = json!({
        "saved": true,
        "project": node.name,
        "characters": to.as_deref().map_or(0, |t| t.chars().count()),
        "note": "Chats started from now on carry the new instructions.",
    });
    if helm_hosttools::settings::record_external_change(
        ctx,
        crate::fields::KEY,
        json!({ node.id.clone(): before }),
        json!({ node.id.clone(): after }),
    )
    .is_ok()
    {
        out["undo"] = json!("The user was shown a notice with Undo for this change.");
    }
    Ok(out)
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
            format!("Read or change a project's startup instructions: what every new chat on it does first (a worktree and branch, local servers, links for the user). get is free and defaults to this chat's board, else its folder project. set saves the new text at once (at most {MAX_STAGED_CHARS} characters; empty clears), and the user is shown a notice with Undo. Use set only when the user asked for it."),
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["get", "set"]},
                    "projectId": {"type": "string"},
                    "instructions": {"type": "string", "maxLength": MAX_STAGED_CHARS}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        )
        .self_locking(),
        project_startup,
    );
}

// ---- the context files and servers ----------------------------------------------------------

/// The MCP servers a chat's project allows ([`helm_hosttools::McpServerFilter`]).
pub struct StartupServers {
    pub db: Arc<Database>,
}

impl helm_hosttools::McpServerFilter for StartupServers {
    fn allowed(&self, chat_id: &str) -> Option<Vec<String>> {
        let set = self
            .db
            .snapshot(|tx| set_for_chat(tx, chat_id))
            .ok()
            .flatten()?;
        (!set.servers.is_empty()).then_some(set.servers)
    }

    fn any(&self) -> bool {
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM project_local
                     WHERE startup_servers IS NOT NULL AND startup_servers != '')",
                    [],
                    |r| r.get::<_, bool>(0),
                )
            })
            .unwrap_or(false)
    }
}

/// The longest context file Helm reads for `app_help`.
const MAX_CONTEXT_FILE: u64 = 256 * 1024;

/// The `project` namespace of `app_help`: this chat's project's startup instructions and context
/// files, read from where they are when asked.
pub struct ProjectHelp {
    pub db: Arc<Database>,
}

impl helm_hosttools::help::HelpProvider for ProjectHelp {
    fn namespace(&self) -> &'static str {
        "project"
    }

    fn about(&self) -> &'static str {
        "This chat's project: its startup instructions and context files"
    }

    fn index(&self, chat_id: &str) -> Vec<(String, String)> {
        let Some(set) = self
            .db
            .snapshot(|tx| set_for_chat(tx, chat_id))
            .ok()
            .flatten()
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if !set.text.is_empty() {
            out.push((
                "startup".to_owned(),
                format!("Startup instructions of {}", set.project_name),
            ));
        }
        for (i, f) in set.files.iter().enumerate() {
            out.push((file_topic(&set.files, i), f.chars().take(90).collect()));
        }
        out
    }

    fn topic(&self, chat_id: &str, name: &str) -> Option<String> {
        let set = self
            .db
            .snapshot(|tx| set_for_chat(tx, chat_id))
            .ok()
            .flatten()?;
        if name == "startup" {
            return (!set.text.is_empty()).then_some(set.text);
        }
        let i = (0..set.files.len()).find(|i| file_topic(&set.files, *i) == name)?;
        let path = std::path::Path::new(&set.files[i]);
        let plain = std::fs::symlink_metadata(path)
            .is_ok_and(|m| m.is_file() && m.len() <= MAX_CONTEXT_FILE);
        if !plain {
            return Some(format!(
                "{} is not a plain file of at most {} KiB, so Helm does not read it here.",
                set.files[i],
                MAX_CONTEXT_FILE / 1024
            ));
        }
        let text = String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned();
        if helm_core::hidden::hides(&text) {
            return Some("That file holds invisible or direction-changing characters, so Helm does not show it.".into());
        }
        Some(text)
    }
}

/// Add the `project` topics to `app_help`.
pub fn register_help(providers: &helm_hosttools::help::HelpProviders, db: Arc<Database>) {
    providers.register(Arc::new(ProjectHelp { db }));
}
