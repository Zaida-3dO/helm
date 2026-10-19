//! Fork and side-chat creation.
//!
//! `chat_fork {chatId, forkId, messageId?, side?, excerpt?}` makes a new chat from a source chat.
//!
//! - **Idempotent on the fork id** (`chat_forks`): a repeat with the same source, message and side
//!   flag answers the existing fork; anything else under that id is refused.
//! - **Helm's history.** The fork gets its own session log holding the source's entries (up to
//!   the chosen assistant message for a message fork), then a `fork` boundary context entry. A
//!   whole-chat fork of a running chat also gets a synthetic error result for every tool call
//!   still running and keeps a reply still streaming as an interrupted message; either makes the
//!   boundary say the copy is partial.
//! - **Claude's history.** Only through the CLI's own flags; Helm never reads Claude's files. A
//!   whole-chat fork's binding names the source's native session, and its first launch resumes it
//!   with `--fork-session`, which gives the fork its own session. A message fork cannot cut a
//!   native session there, so it starts a fresh one and its first prompt carries the conversation
//!   up to the chosen reply as one text block (`fork_carry`, budgeted like the carried turn
//!   text: newest steps kept, with an "earlier steps omitted" line).
//! - **Creation and placement.** Same folder project, model and effort; titled "Fork · <source>"
//!   with no stacked prefix; placed directly below the source when the source has a position
//!   (later rows shift by one, and a `sidebar` signal names every row that moved). A whole-chat
//!   fork copies the draft; a message fork starts empty.
//! - **Side chats** (`side: true` plus an `excerpt` of 1–16,000 characters): a message fork whose
//!   draft is a short lead-in and the excerpt as a block quote, marked with `side_source_id` so it
//!   stays out of the list until promoted, and never placed.

use crate::archive::Refusal;
use crate::session_log::{
    self, ClaudeBinding, ContextEntry, Entry, Header, MessageEntry, ToolEntry,
};
use crate::store;
use crate::types::*;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::Path;

pub const TITLE_PREFIX: &str = "Fork · ";
pub const SIDE_TITLE_PREFIX: &str = "Side chat · ";
pub const MAX_TITLE_BASE_CHARS: usize = 450;
pub const MAX_EXCERPT_CHARS: usize = 16_000;
pub const SIDE_LEAD_IN: &str = "Regarding this excerpt:";
/// What a tool still running in the source says in the fork.
pub const STILL_RUNNING: &str =
    "Still running when this chat was forked; it was not run again in the fork.";
/// The carried-history budget and the per-step cut.
pub const CARRY_BUDGET: usize = 300_000;
pub const CARRY_STEP: usize = 40_000;

/// "Fork · <source>", with any prefix the source already has stripped, so prefixes never stack.
pub fn fork_title(source: &str, side: bool) -> String {
    let mut base = source.trim();
    while let Some(rest) = base
        .strip_prefix(TITLE_PREFIX)
        .or_else(|| base.strip_prefix(SIDE_TITLE_PREFIX))
    {
        base = rest.trim_start();
    }
    let base: String = base.chars().take(MAX_TITLE_BASE_CHARS).collect();
    let base = if base.is_empty() {
        store::DEFAULT_TITLE.to_owned()
    } else {
        base
    };
    let prefix = if side {
        SIDE_TITLE_PREFIX
    } else {
        TITLE_PREFIX
    };
    format!("{prefix}{base}")
}

/// A side chat's opening draft: the lead-in, then the excerpt as a block quote.
pub fn side_draft(excerpt: &str) -> String {
    let quoted: Vec<String> = excerpt
        .lines()
        .map(|l| {
            if l.is_empty() {
                ">".to_owned()
            } else {
                format!("> {l}")
            }
        })
        .collect();
    format!("{SIDE_LEAD_IN}\n\n{}\n\n", quoted.join("\n"))
}

/// The history a message fork's first prompt carries, from the copied entries: user and
/// assistant text plus tool names, newest kept within [`CARRY_BUDGET`].
pub fn carry_text(entries: &[Map<String, Value>]) -> Option<String> {
    let mut steps: Vec<String> = Vec::new();
    for e in entries {
        match serde_json::from_value::<Entry>(Value::Object(e.clone())) {
            Ok(Entry::Message(m)) if !m.text.trim().is_empty() => {
                let who = match m.role {
                    MessageRole::User => "User",
                    _ => "Assistant",
                };
                steps.push(format!("{who}: {}", session_log::cut(&m.text, CARRY_STEP)));
            }
            Ok(Entry::Tool(t)) => steps.push(format!(
                "Tool {} ({}): {}",
                t.tool_name,
                if t.is_error { "failed" } else { "done" },
                session_log::cut(&t.result, 2_000)
            )),
            _ => {}
        }
    }
    if steps.is_empty() {
        return None;
    }
    let mut kept: Vec<&String> = Vec::new();
    let mut used = 0;
    for s in steps.iter().rev() {
        if used + s.len() > CARRY_BUDGET {
            break;
        }
        used += s.len();
        kept.push(s);
    }
    kept.reverse();
    let omitted = steps.len() - kept.len();
    let mut out = String::from(
        "This chat is a fork. The conversation it was forked from is below, as reference: it is \
         the history up to the point of the fork, and every tool listed there already ran, so do \
         not repeat it.\n\n",
    );
    if omitted > 0 {
        out.push_str(&format!("({omitted} earlier steps omitted.)\n\n"));
    }
    for s in kept {
        out.push_str(s);
        out.push_str("\n\n");
    }
    Some(out.trim_end().to_owned())
}

/// A row still streaming in the source: role, text, tool name, call id, details, timestamp.
type LiveRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
);

/// What `chat_fork` was asked for, checked.
pub struct ForkRequest<'a> {
    pub source: &'a str,
    pub fork_id: &'a str,
    pub message_id: Option<i64>,
    pub side: bool,
    pub excerpt: Option<&'a str>,
}

/// The fork as written, for the op to emit.
pub struct Forked {
    pub chat_id: String,
    /// Rows whose position moved, the fork included.
    pub moved: Vec<String>,
    pub existing: bool,
}

fn refuse(msg: &str) -> Refusal {
    Refusal::new("refused", msg)
}

/// Read the source log's entries; a missing log is an empty history.
fn source_entries(sessions: &Path, source: &str) -> Result<Vec<Map<String, Value>>, Refusal> {
    let path = session_log::log_path(sessions, source);
    if !path.exists() {
        return Ok(Vec::new());
    }
    session_log::read(&path).map(|r| r.entries).map_err(|e| {
        Refusal::new(
            "internal",
            format!("The chat's history could not be read: {e}"),
        )
    })
}

/// Create the fork. `project_archived` is the projects crate's gate (see `crate::seams`).
pub fn fork(
    db: &helm_core::db::Database,
    profile_dir: &Path,
    req: &ForkRequest<'_>,
    project_archived: &dyn Fn(&Transaction<'_>, &str) -> rusqlite::Result<bool>,
) -> Result<Forked, Refusal> {
    if uuid::Uuid::parse_str(req.fork_id).is_err() {
        return Err(Refusal::new("bad_args", "forkId must be a UUID."));
    }
    if req.side {
        let n = req.excerpt.map(|e| e.chars().count()).unwrap_or(0);
        if !(1..=MAX_EXCERPT_CHARS).contains(&n) {
            return Err(Refusal::new(
                "bad_args",
                "A side chat quotes 1 to 16,000 characters.",
            ));
        }
        if req.message_id.is_none() {
            return Err(Refusal::new(
                "bad_args",
                "A side chat needs the reply it quotes.",
            ));
        }
    }
    let sql = |e: rusqlite::Error| Refusal::new("internal", e.to_string());
    // Idempotency first: the same request answers the same fork.
    let prior: Option<(String, Option<i64>, bool, String)> = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT source_chat_id, message_id, side, chat_id FROM chat_forks WHERE fork_id = ?1",
                [req.fork_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
        })
        .map_err(sql)?;
    if let Some((src, msg, side, chat)) = prior {
        if src == req.source && msg == req.message_id && side == req.side {
            return Ok(Forked {
                chat_id: chat,
                moved: vec![],
                existing: true,
            });
        }
        return Err(refuse("That fork id is already in use."));
    }
    // The source and the cut point.
    struct Src {
        title: String,
        cwd: String,
        project: String,
        model: Option<String>,
        effort: Option<String>,
        draft: String,
        side: Option<String>,
        position: Option<i64>,
        pinned: bool,
        archived: bool,
    }
    let src: Src = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT c.title, COALESCE(p.path, ''), c.project_id, c.model, c.thinking, c.draft,
                        c.side_source_id, c.position, c.pinned, c.archived
                 FROM chats c JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
                [req.source],
                |r| {
                    Ok(Src {
                        title: r.get(0)?,
                        cwd: r.get(1)?,
                        project: r.get(2)?,
                        model: r.get(3)?,
                        effort: r.get(4)?,
                        draft: r.get(5)?,
                        side: r.get(6)?,
                        position: r.get(7)?,
                        pinned: r.get(8)?,
                        archived: r.get(9)?,
                    })
                },
            )
            .optional()
        })
        .map_err(sql)?
        .ok_or_else(|| Refusal::new("not_found", "That chat does not exist."))?;
    if src.side.is_some() {
        return Err(refuse("Promote this side chat before forking it."));
    }
    let cut_key: Option<String> = match req.message_id {
        None => None,
        Some(mid) => {
            let row: Option<(String, String, String, bool)> = db
                .snapshot(|tx| {
                    tx.query_row(
                        "SELECT source_key, role, status, active FROM messages WHERE chat_id = ?1 AND id = ?2",
                        params![req.source, mid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()
                })
                .map_err(sql)?;
            match row {
                Some((key, role, status, true)) if role == "assistant" => {
                    if status == "streaming" {
                        return Err(refuse(
                            "That reply is still being saved; try again in a moment.",
                        ));
                    }
                    Some(key)
                }
                _ => return Err(refuse("Fork from a reply that is part of this chat.")),
            }
        }
    };
    let sessions = profile_dir.join("sessions");
    let artifacts = profile_dir.join("artifacts");
    let mut entries = source_entries(&sessions, req.source)?;
    if let Some(key) = &cut_key {
        let Some(at) = entries
            .iter()
            .position(|e| e.get("id").and_then(Value::as_str) == Some(key))
        else {
            return Err(refuse(
                "That reply is still being saved; try again in a moment.",
            ));
        };
        entries.truncate(at + 1);
    }
    let now = helm_core::now_ms();
    let fork_id_chat = uuid::Uuid::new_v4().to_string();
    // Fix-ups for a whole-chat fork of a running chat: what is live is not in the log yet.
    let mut partial = false;
    if cut_key.is_none() {
        let live: Vec<LiveRow> = db
            .snapshot(|tx| {
                let mut st = tx.prepare(
                    "SELECT role, text, tool_name, tool_call_id, details, timestamp FROM messages
                     WHERE chat_id = ?1 AND active = 1 AND status = 'streaming' ORDER BY timestamp, id",
                )?;
                let rows = st.query_map([req.source], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(sql)?;
        for (role, text, tool, call, details, ts) in live {
            partial = true;
            let entry = if role == "toolResult" {
                let call = call.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let args = details
                    .and_then(|d| serde_json::from_str::<Value>(&d).ok())
                    .and_then(|d| d.get("toolArgs").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_default();
                Entry::Tool(ToolEntry {
                    id: format!("t:{call}"),
                    parent_id: None,
                    timestamp: ts,
                    tool_call_id: call,
                    tool_name: tool.unwrap_or_else(|| "tool".into()),
                    args,
                    result: STILL_RUNNING.into(),
                    is_error: true,
                    started_at: ts,
                    ended_at: now,
                })
            } else {
                Entry::message(MessageEntry {
                    id: format!("m:fork-partial-{}", uuid::Uuid::new_v4()),
                    parent_id: None,
                    timestamp: ts,
                    role: MessageRole::Assistant,
                    text,
                    thinking: String::new(),
                    status: MessageStatus::Interrupted,
                    details: MessageDetails::default(),
                })
            };
            if let Ok(Value::Object(m)) = serde_json::to_value(&entry) {
                entries.push(m);
            }
        }
    }
    let boundary = Entry::Context(ContextEntry {
        id: format!("c:fork:{}", req.fork_id),
        parent_id: None,
        timestamp: now,
        kind: ContextKind::Fork,
        text: format!(
            "Forked from {}{}",
            if src.title.trim().is_empty() {
                "Untitled"
            } else {
                src.title.trim()
            },
            if partial {
                " (taken mid-turn; unfinished work is marked)"
            } else {
                ""
            }
        ),
    });
    if let Ok(Value::Object(m)) = serde_json::to_value(&boundary) {
        entries.push(m);
    }
    // Chain the copy linearly and write the fork's log.
    let mut prev: Option<String> = None;
    for e in entries.iter_mut() {
        e.insert(
            "parentId".into(),
            prev.clone().map(Value::String).unwrap_or(Value::Null),
        );
        prev = e.get("id").and_then(Value::as_str).map(str::to_owned);
    }
    let header = Header {
        version: session_log::FORMAT_VERSION.into(),
        chat_id: fork_id_chat.clone(),
        harness: "claude".into(),
        cwd: src.cwd.clone(),
        created_at: now,
    };
    let log = session_log::log_path(&sessions, &fork_id_chat);
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
        for e in &entries {
            out.push_str(&Value::Object(e.clone()).to_string());
            out.push('\n');
        }
        f.write_all(out.as_bytes())?;
        f.sync_data()
    };
    write_log()
        .map_err(|e| Refusal::new("internal", format!("The fork could not be written: {e}")))?;
    let carry = cut_key.as_ref().and_then(|_| carry_text(&entries));
    let title = fork_title(&src.title, req.side);
    let draft = if req.side {
        side_draft(req.excerpt.unwrap_or_default())
    } else if cut_key.is_none() {
        src.draft.clone()
    } else {
        String::new()
    };
    let rev = 0;
    let result = db.write(|tx| {
        if project_archived(tx, &src.project)? {
            return Ok(Err(refuse("This chat's project is archived; restore it first.")));
        }
        if src.archived {
            return Ok(Err(refuse("Restore this chat before forking it.")));
        }
        tx.execute(
            "INSERT INTO chats(id, project_id, title, status, harness, model, thinking, created_at, updated_at, draft, draft_revision, side_source_id)
             VALUES (?1, ?2, ?3, 'idle', 'claude', ?4, ?5, ?6, ?6, ?7, ?8, ?9)",
            params![
                fork_id_chat,
                src.project,
                title,
                src.model,
                src.effort,
                now,
                draft,
                if draft.is_empty() { 0 } else { now },
                req.side.then_some(req.source)
            ],
        )?;
        tx.execute(
            "INSERT INTO chat_forks(fork_id, source_chat_id, message_id, side, chat_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![req.fork_id, req.source, req.message_id, req.side, fork_id_chat, now],
        )?;
        if let Some(c) = &carry {
            tx.execute(
                "INSERT INTO fork_carry(chat_id, text) VALUES (?1, ?2)",
                params![fork_id_chat, c],
            )?;
        }
        for e in &entries {
            session_log::project(tx, &artifacts, &fork_id_chat, e, rev)?;
        }
        // Placement: directly below an unpinned source that has a position.
        let mut moved = Vec::new();
        if let (false, false, Some(pos)) = (req.side, src.pinned, src.position) {
            let shifted: Vec<String> = {
                let mut st = tx.prepare(
                    "SELECT id FROM chats WHERE project_id = ?1 AND pinned = 0 AND position > ?2 AND id != ?3",
                )?;
                let rows = st.query_map(params![src.project, pos, fork_id_chat], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            tx.execute(
                "UPDATE chats SET position = position + 1 WHERE project_id = ?1 AND pinned = 0 AND position > ?2 AND id != ?3",
                params![src.project, pos, fork_id_chat],
            )?;
            tx.execute(
                "UPDATE chats SET position = ?1 WHERE id = ?2",
                params![pos + 1, fork_id_chat],
            )?;
            moved = shifted;
            moved.push(fork_id_chat.clone());
        }
        Ok::<_, rusqlite::Error>(Ok(moved))
    });
    let moved = match result {
        Ok(Ok(m)) => m,
        Ok(Err(r)) => {
            let _ = std::fs::remove_file(&log);
            return Err(r);
        }
        Err(e) => {
            let _ = std::fs::remove_file(&log);
            return Err(sql(e));
        }
    };
    // A whole-chat fork's first launch resumes the source's native session as a new session.
    if cut_key.is_none() {
        if let Some(native) = ClaudeBinding::load(&sessions, req.source)
            .filter(|b| b.cwd == src.cwd)
            .and_then(|b| b.native_session_id)
        {
            let b = ClaudeBinding {
                native_session_id: None,
                cwd: src.cwd.clone(),
                last_leaf_id: None,
                harness_switch_id: None,
                updated_at: now,
                fork_of: Some(native),
            };
            if let Err(e) = b.save(&sessions, &fork_id_chat) {
                log::error!("fork {fork_id_chat}: binding not saved ({e}); it starts without Claude's history");
            }
        }
    }
    Ok(Forked {
        chat_id: fork_id_chat,
        moved,
        existing: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_never_stack_their_prefix() {
        assert_eq!(fork_title("Fix the build", false), "Fork · Fix the build");
        assert_eq!(fork_title("Fork · Fork · Fix", false), "Fork · Fix");
        assert_eq!(fork_title("Side chat · Fix", true), "Side chat · Fix");
        assert_eq!(fork_title("  ", false), "Fork · New chat");
        assert_eq!(
            fork_title(&"x".repeat(600), false).chars().count(),
            TITLE_PREFIX.chars().count() + MAX_TITLE_BASE_CHARS
        );
    }

    #[test]
    fn a_side_draft_quotes_every_line() {
        assert_eq!(
            side_draft("a\n\nb"),
            "Regarding this excerpt:\n\n> a\n>\n> b\n\n"
        );
    }

    #[test]
    fn carried_history_keeps_the_newest_steps_within_budget() {
        let msg = |id: &str, text: String| {
            serde_json::to_value(Entry::message(MessageEntry {
                id: id.into(),
                parent_id: None,
                timestamp: 1,
                role: MessageRole::User,
                text,
                thinking: String::new(),
                status: MessageStatus::Complete,
                details: MessageDetails::default(),
            }))
            .unwrap()
            .as_object()
            .unwrap()
            .clone()
        };
        let entries: Vec<_> = (0..20)
            .map(|i| {
                msg(
                    &format!("u:{i}"),
                    format!("step {i} {}", "y".repeat(30_000)),
                )
            })
            .collect();
        let t = carry_text(&entries).unwrap();
        assert!(t.len() <= CARRY_BUDGET + 1_000);
        assert!(t.contains("earlier steps omitted"));
        assert!(t.contains("step 19"), "the newest step is kept");
        assert!(!t.contains("step 0 "), "the oldest is dropped");
        assert_eq!(carry_text(&[]), None);
    }
}
