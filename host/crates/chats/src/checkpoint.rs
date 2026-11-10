//! Turn checkpoints: going back to before one of the user's own messages.
//!
//! Every message the user sent is a checkpoint. Going back to one is a fork cut just before that
//! message (see [`crate::fork`]): the copy holds the conversation up to there, its draft holds the
//! message's text, and the original chat is left exactly as it was. A fork was chosen over
//! rewinding in place because nothing is thrown away: the user can still open the longer chat,
//! and a mistaken click costs one extra chat in the sidebar, not a lost conversation.
//!
//! - `checkpoint_info {chatId, messageId}` tells the UI what going back means, for its
//!   confirmation: the message's text, how many of the user's messages the copy leaves out, and
//!   the files the agent changed from that message on, as far as the chat's log records them
//!   (the edit tools' own paths and the files shell commands changed).
//! - **Files are never restored.** The log records what the agent was told to change, not what
//!   the files held before, so a file cannot be put back from it with any confidence (a rewritten
//!   file's old content is nowhere). Helm therefore restores the conversation only and says so;
//!   the answer's `filesRestored` is always `false`, and the list of changed files is there so
//!   the user knows what to check or undo with their own tools. Going back reads the chat's log
//!   and never touches a file in the project or anywhere else.

use crate::archive::Refusal;
use crate::session_log::Entry;
use crate::types::MessageRole;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// A message row's role, with the user rows Helm itself wrote (a continuation, a notification, a
/// schedule, another chat's message) read as `helm`: only the user's own messages are places to
/// go back to.
pub(crate) const OWN_ROLE: &str = "CASE WHEN role = 'user'
     AND COALESCE(CASE WHEN json_valid(details) THEN json_extract(details, '$.origin') END, 'typed')
         IN ('app', 'notification', 'schedule', 'other_chat')
     THEN 'helm' ELSE role END";

/// The most changed files named in one answer; the rest are counted.
pub const MAX_LISTED: usize = 40;
/// The edit tools whose arguments name the file they change.
const EDIT_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];

pub fn register(reg: &mut OpRegistry) {
    reg.blocking("checkpoint_info", OpClass::Blocking, checkpoint_info);
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InfoArgs {
    chat_id: String,
    message_id: i64,
}

/// What going back to a message means.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointInfo {
    /// The message's text, which the copy's draft will hold.
    pub prompt: String,
    /// The user's messages from this one on, this one included: what the copy leaves out.
    pub messages_after: usize,
    /// Files changed from this message on, sorted; inside the chat's folder they are relative to it.
    pub changed_files: Vec<String>,
    /// Changed files beyond [`MAX_LISTED`].
    pub more_files: usize,
    /// Always false: only the conversation is restored.
    pub files_restored: bool,
}

fn checkpoint_info(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: InfoArgs = crate::ops::parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let profile = ctx.services.need::<Profile>()?;
    let info = info(&db, &profile.path(""), &a.chat_id, a.message_id)?;
    serde_json::to_value(&info).map_err(|e| ApiError::internal(e.to_string()))
}

/// Describe going back to the user's message `message_id` in `chat_id`.
pub fn info(
    db: &Database,
    profile_dir: &Path,
    chat_id: &str,
    message_id: i64,
) -> Result<CheckpointInfo, Refusal> {
    let cwd: Option<String> = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT COALESCE(p.path, '') FROM chats c JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
                [chat_id],
                |r| r.get(0),
            )
            .optional()
        })
        .map_err(|e| Refusal::new("internal", e.to_string()))?;
    let Some(cwd) = cwd else {
        return Err(Refusal::new("not_found", "That chat does not exist."));
    };
    let row: Option<(String, String, String, bool, String)> = db
        .snapshot(|tx| {
            tx.query_row(
                &format!(
                    "SELECT source_key, {}, status, active, text FROM messages WHERE chat_id = ?1 AND id = ?2",
                    crate::checkpoint::OWN_ROLE
                ),
                params![chat_id, message_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
        })
        .map_err(|e| Refusal::new("internal", e.to_string()))?;
    let Some((key, _, _, _, prompt)) = row
        .filter(|(_, role, status, active, _)| role == "user" && status == "complete" && *active)
    else {
        return Err(Refusal::new(
            "refused",
            "Only your own sent messages are checkpoints.",
        ));
    };
    let entries = crate::fork::source_entries(&profile_dir.join("sessions"), chat_id)?;
    let Some(at) = entries
        .iter()
        .position(|e| e.get("id").and_then(Value::as_str) == Some(key.as_str()))
    else {
        return Err(Refusal::new(
            "refused",
            "That message is still being saved; try again in a moment.",
        ));
    };
    let (messages_after, mut files) = since(&entries[at..], &cwd);
    let more_files = files.len().saturating_sub(MAX_LISTED);
    files.truncate(MAX_LISTED);
    Ok(CheckpointInfo {
        prompt,
        messages_after,
        changed_files: files,
        more_files,
        files_restored: false,
    })
}

/// From the log entries after a checkpoint: how many of the user's messages they hold, and the
/// files the agent changed (edit tools that succeeded, and shell commands' changed files), sorted
/// and without repeats.
fn since(entries: &[Map<String, Value>], cwd: &str) -> (usize, Vec<String>) {
    let mut messages = 0;
    let mut files: BTreeSet<String> = BTreeSet::new();
    for e in entries {
        match serde_json::from_value::<Entry>(Value::Object(e.clone())) {
            Ok(Entry::Message(m)) if matches!(m.role, MessageRole::User) => messages += 1,
            Ok(Entry::Tool(t)) if !t.is_error && EDIT_TOOLS.contains(&t.tool_name.as_str()) => {
                let args: Value = serde_json::from_str(&t.args).unwrap_or(Value::Null);
                if let Some(p) = ["file_path", "notebook_path"]
                    .iter()
                    .find_map(|k| args.get(k).and_then(Value::as_str))
                    .filter(|p| !p.is_empty())
                {
                    files.insert(relative(cwd, p));
                }
            }
            Ok(Entry::ShellChanges(s)) => files.extend(s.paths),
            _ => {}
        }
    }
    (messages, files.into_iter().collect())
}

/// `path` relative to the chat's folder when it is inside it, otherwise as given; forward slashes.
fn relative(cwd: &str, path: &str) -> String {
    let norm = |s: &str| s.replace('\\', "/");
    let (cwd, path) = (norm(cwd), norm(path));
    let root = cwd.trim_end_matches('/');
    let inside = !root.is_empty()
        && path.len() > root.len() + 1
        && path.is_char_boundary(root.len())
        && path[..root.len()].eq_ignore_ascii_case(root)
        && path.as_bytes()[root.len()] == b'/';
    if inside {
        path[root.len() + 1..].to_owned()
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn changed_files_come_from_edit_tools_and_shell_changes() {
        let tool = |id: &str, name: &str, args: Value, is_error: bool| {
            entry(json!({
                "type": "tool", "id": format!("t:{id}"), "timestamp": 1, "toolCallId": id,
                "toolName": name, "args": args.to_string(), "result": "", "isError": is_error,
                "startedAt": 1, "endedAt": 2,
            }))
        };
        let user = entry(json!({
            "type": "message", "id": "m:u2", "timestamp": 1, "role": "user", "text": "again",
            "thinking": "", "status": "complete", "details": {},
        }));
        let entries = vec![
            user.clone(),
            tool(
                "a",
                "Edit",
                json!({"file_path": "C:\\proj\\src\\a.rs"}),
                false,
            ),
            tool("b", "Write", json!({"file_path": "c:/PROJ/b.txt"}), false),
            tool("c", "Edit", json!({"file_path": "C:/proj/failed.rs"}), true),
            tool("d", "Read", json!({"file_path": "C:/proj/read.rs"}), false),
            tool(
                "e",
                "NotebookEdit",
                json!({"notebook_path": "D:/else/n.ipynb"}),
                false,
            ),
            tool("f", "Edit", json!({"file_path": "C:/proj/src/a.rs"}), false),
            tool(
                "g",
                "Edit",
                json!({"file_path": "C:/projector/x.rs"}),
                false,
            ),
            entry(json!({
                "type": "shellChanges", "id": "s:h", "timestamp": 1, "toolCallId": "h",
                "paths": ["docs/new.md"],
            })),
            user,
        ];
        let (messages, files) = since(&entries, "C:\\proj\\");
        assert_eq!(messages, 2);
        assert_eq!(
            files,
            [
                "C:/projector/x.rs",
                "D:/else/n.ipynb",
                "b.txt",
                "docs/new.md",
                "src/a.rs"
            ]
        );
        assert_eq!(since(&[], "C:/proj"), (0, vec![]));
    }
}
