//! Continuing one of Claude Code's chats as a new Helm chat.
//!
//! The trick is the session binding: the new chat's `.claude.json` names the record's own
//! session id under `forkOf` and none of its own, so its first launch runs `claude --resume
//! <that id> --fork-session` — Claude Code carries the conversation over as a fresh session,
//! with its full context, without Helm touching any file under `.claude`. The visible history is
//! copied into the new chat's session log (converted line by line, newest kept within budget),
//! and the chat is created exactly like a new chat in that folder would be: folder project,
//! Settings defaults for model and effort, the archived-project gate checked inside the
//! transaction so a refusal leaves nothing behind.

use crate::parse::{self, Decoded, Role};
use crate::store;
use crate::types::Forked;
use helm_chats::archive::Refusal;
use helm_chats::seams::ProjectGates;
use helm_chats::session_log::{
    self, ClaudeBinding, ContextEntry, Entry, Header, MessageEntry, ToolEntry,
};
use helm_chats::types::{ContextKind, MessageRole, MessageStatus};
use helm_core::db::Database;
use rusqlite::{params, Error as SqlError};
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The new chat's title is the record's own, cut like the other title bases.
pub const MAX_TITLE_CHARS: usize = 450;
/// The copied history keeps at most this much line text, newest first.
pub const LOG_TEXT_BUDGET: usize = 400_000;
/// …and at most this many lines.
pub const MAX_LOG_ENTRIES: usize = 10_000;

fn refuse(msg: &str) -> Refusal {
    Refusal::new("refused", msg)
}

/// Session ids come from Claude Code (UUIDs) or, for a file it did not label, from its name;
/// anything that could be a path or a glob is refused before it reaches a query.
pub(crate) fn valid_session_id(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !s.starts_with('.')
}

pub fn fork(
    db: &Database,
    profile_dir: &Path,
    session_id: &str,
    project_archived: Option<&ProjectGates>,
) -> Result<Forked, Refusal> {
    if !valid_session_id(session_id) {
        return Err(Refusal::new("bad_args", "That session id is not valid."));
    }
    let sql = |e: SqlError| Refusal::new("internal", e.to_string());
    let (path, row_folder, row_title, _updated) = store::record(db, session_id)
        .map_err(sql)?
        .ok_or_else(|| Refusal::new("not_found", "That chat is not in the indexed records."))?;
    let tr = parse::parse_file(&PathBuf::from(&path)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Refusal::new("not_found", "That chat's record is no longer on disk.")
        } else {
            Refusal::new(
                "internal",
                format!("That chat's record could not be read: {e}"),
            )
        }
    })?;
    let cwd = if tr.cwd.is_empty() {
        row_folder
    } else {
        tr.cwd.clone()
    };
    if cwd.is_empty() {
        return Err(refuse(
            "That chat has no working folder, so it cannot be continued.",
        ));
    }
    if !Path::new(&cwd).is_dir() {
        return Err(refuse("The folder that chat worked in no longer exists."));
    }

    let now = helm_core::now_ms();
    let chat_id = uuid::Uuid::new_v4().to_string();
    let title = base_title(&tr.title, &row_title);

    // Convert, newest first, within the budget; the oldest lines are dropped, and the
    // boundary says so.
    let mut kept: Vec<Map<String, Value>> = Vec::new();
    let mut seen_calls: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut msg_seq = 0usize;
    let mut budget = LOG_TEXT_BUDGET;
    let mut dropped = 0usize;
    for d in tr.lines.iter().rev() {
        if kept.len() >= MAX_LOG_ENTRIES {
            dropped += 1;
            continue;
        }
        let entry = match d {
            Decoded::Message {
                role,
                text,
                timestamp,
            } => {
                budget = match budget.checked_sub(text.len()) {
                    Some(b) => b,
                    None => {
                        dropped += 1;
                        continue;
                    }
                };
                let (prefix, role) = match role {
                    Role::User => ("u", MessageRole::User),
                    Role::Assistant => ("m", MessageRole::Assistant),
                };
                let id = format!("{prefix}:cc-import-{msg_seq}");
                msg_seq += 1;
                Entry::message(MessageEntry {
                    id,
                    parent_id: None,
                    timestamp: *timestamp,
                    role,
                    text: text.clone(),
                    thinking: String::new(),
                    status: if role == MessageRole::User {
                        MessageStatus::Saved
                    } else {
                        MessageStatus::Complete
                    },
                    details: Default::default(),
                })
            }
            Decoded::Tool {
                call_id,
                name,
                args,
                result,
                is_error,
                timestamp,
            } => {
                let cost = result.len().max(args.len());
                budget = match budget.checked_sub(cost) {
                    Some(b) => b,
                    None => {
                        dropped += 1;
                        continue;
                    }
                };
                if !seen_calls.insert(call_id.clone()) {
                    dropped += 1;
                    continue;
                }
                Entry::Tool(ToolEntry {
                    id: format!("t:cc-{call_id}"),
                    parent_id: None,
                    timestamp: *timestamp,
                    tool_call_id: call_id.clone(),
                    tool_name: name.clone(),
                    args: args.clone(),
                    result: result.clone(),
                    is_error: *is_error,
                    started_at: *timestamp,
                    ended_at: *timestamp,
                })
            }
        };
        if let Ok(Value::Object(m)) = serde_json::to_value(&entry) {
            kept.push(m);
        }
    }
    kept.reverse();
    let boundary = Entry::Context(ContextEntry {
        id: format!("c:cc-import:{}", uuid::Uuid::new_v4()),
        parent_id: None,
        timestamp: now,
        kind: ContextKind::Fork,
        text: format!(
            "Continued from a Claude Code chat in {cwd}{}",
            if dropped > 0 {
                " (the oldest lines did not fit and were left behind)"
            } else {
                ""
            }
        ),
    });
    if let Ok(Value::Object(mut m)) = serde_json::to_value(&boundary) {
        m.insert("parentId".into(), Value::Null);
        kept.push(m);
    }
    // Chain the copy linearly.
    let mut prev: Option<String> = None;
    for e in kept.iter_mut() {
        e.insert(
            "parentId".into(),
            prev.clone().map(Value::String).unwrap_or(Value::Null),
        );
        prev = e.get("id").and_then(Value::as_str).map(str::to_owned);
    }

    let sessions = profile_dir.join("sessions");
    let artifacts = profile_dir.join("artifacts");
    let log = session_log::log_path(&sessions, &chat_id);
    let header = Header {
        version: session_log::FORMAT_VERSION.into(),
        chat_id: chat_id.clone(),
        harness: "claude".into(),
        cwd: cwd.clone(),
        created_at: now,
    };
    let cleanup = || {
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(session_log::binding_path(&sessions, &chat_id));
    };
    let write_log = || -> std::io::Result<()> {
        helm_core::privfs::create_private_dir_all(&sessions)?;
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&log)?;
        helm_platform::fs::restrict_to_owner(&log)?;
        let mut h = serde_json::to_value(&header).map_err(std::io::Error::other)?;
        h["type"] = json!("header");
        let mut out = format!("{h}\n");
        for e in &kept {
            out.push_str(&Value::Object(e.clone()).to_string());
            out.push('\n');
        }
        f.write_all(out.as_bytes())?;
        f.sync_data()
    };
    if let Err(e) = write_log() {
        return Err(Refusal::new(
            "internal",
            format!("The continuation could not be written: {e}"),
        ));
    }
    // The binding first: without it Claude's history does not carry over, and this feature is
    // precisely that promise — so a save failure refuses rather than starting a hollow chat.
    let binding = ClaudeBinding {
        native_session_id: None,
        cwd: cwd.clone(),
        last_leaf_id: None,
        harness_switch_id: None,
        updated_at: now,
        fork_of: Some(tr.session_id.clone()),
    };
    if let Err(e) = binding.save(&sessions, &chat_id) {
        let _ = std::fs::remove_file(&log);
        return Err(Refusal::new(
            "internal",
            format!("The continuation could not be prepared: {e}"),
        ));
    }

    let result = db.write(|tx| {
        let project = helm_chats::store::upsert_folder_project(tx, &cwd, now)?;
        let archived = match project_archived {
            Some(g) => g.0.project_archived(tx, &project)?,
            None => false,
        };
        if archived {
            return Ok(Err(refuse(
                "That folder is an archived project in Helm; restore it first.",
            )));
        }
        let (model, effort) = helm_chats::runtime_settings::new_chat_defaults(tx);
        let (provider, model_col) = helm_chats::local::columns(model.as_deref())
            .map_err(SqlError::InvalidParameterName)?;
        tx.execute(
            "INSERT INTO chats(id, project_id, title, status, harness, provider, model, thinking, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'idle', 'claude', ?4, ?5, ?6, ?7, ?7)",
            params![
                chat_id,
                project,
                title,
                provider,
                model_col,
                effort,
                now
            ],
        )?;
        for e in &kept {
            session_log::project(tx, &artifacts, &chat_id, e, 0)?;
        }
        Ok::<_, SqlError>(Ok(()))
    });
    match result {
        Ok(Ok(())) => {}
        // The transaction rolled back on its own; so do the files it was paired with.
        Ok(Err(r)) => {
            cleanup();
            return Err(r);
        }
        Err(e) => {
            cleanup();
            return Err(sql(e));
        }
    }
    Ok(Forked { chat_id })
}

fn base_title(tr: &str, row: &str) -> String {
    let pick = if tr.trim().is_empty() { row } else { tr };
    pick.trim().chars().take(MAX_TITLE_CHARS).collect()
}
