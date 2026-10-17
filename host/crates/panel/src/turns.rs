//! Turn change sets (plan PN6): the files the agent changed in each turn, with net
//! counts, for the card under a completed turn.
//!
//! `review_turns {chatId, messageIds}` takes a page of at most 600 message ids. For each id that is
//! the last assistant reply of a completed turn, it collects that turn's file-change tool calls
//! (Edit, MultiEdit, Write that ended without error) and computes each file's net change
//! ([`crate::netchange`]). Budgets: the last 64 turns of the page, 512 events per turn, 2,048
//! overall; anything cut is flagged. Each file's repository is found by probing upward for a
//! `.git` entry (which also finds worktrees and nested repositories), cached per request.

use crate::netchange::{self, Event, Net};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const MAX_IDS: usize = 600;
pub const MAX_TURNS: usize = 64;
pub const MAX_EVENTS_PER_TURN: usize = 512;
pub const MAX_EVENTS: usize = 2048;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnFile {
    pub path: String,
    pub repository_root: Option<String>,
    pub repository_path: String,
    pub added: Option<u64>,
    pub removed: Option<u64>,
    pub complete_counts: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnChanges {
    pub message_id: i64,
    pub files: Vec<TurnFile>,
    pub truncated: bool,
}

struct Row {
    id: i64,
    role: String,
    tool: Option<String>,
    entry: Option<String>,
    status: String,
}

/// The events one tool call contributes, from its arguments (Claude Code's Edit/MultiEdit/Write).
pub fn events_of(tool: &str, args: &Value, result: &str) -> Option<(String, Vec<Event>, bool)> {
    let path = args.get("file_path").and_then(Value::as_str)?.to_owned();
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let evs = match tool {
        // Claude Code's Write result says whether it created the file or updated it.
        "Write" => vec![Event::Write {
            content: s(args, "content")?,
            created: result.to_ascii_lowercase().contains("created"),
        }],
        "Edit" => vec![Event::Edit {
            old: s(args, "old_string")?,
            new: s(args, "new_string")?,
            all: args
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }],
        "MultiEdit" => {
            let steps = args.get("edits")?.as_array()?;
            let evs: Vec<Event> = steps
                .iter()
                .filter_map(|e| {
                    Some(Event::Edit {
                        old: s(e, "old_string")?,
                        new: s(e, "new_string")?,
                        all: e
                            .get("replace_all")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect();
            // A malformed step was dropped: the file's counts cannot be complete.
            let whole = evs.len() == steps.len();
            return Some((path, evs, whole));
        }
        _ => return None,
    };
    Some((path, evs, true))
}

/// The repository top above `file`, found by probing for `.git`, cached by folder.
fn repo_root(file: &Path, cache: &mut HashMap<PathBuf, Option<PathBuf>>) -> Option<PathBuf> {
    let mut chain = Vec::new();
    let mut cur = file.parent();
    let mut found = None;
    while let Some(dir) = cur {
        if let Some(hit) = cache.get(dir) {
            found = hit.clone();
            break;
        }
        chain.push(dir.to_path_buf());
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            found = Some(dir.to_path_buf());
            break;
        }
        cur = dir.parent();
    }
    for d in chain {
        cache.insert(d, found.clone());
    }
    found
}

pub fn review_turns(
    tx: &Transaction<'_>,
    chat_id: &str,
    ids: &[i64],
    chat_running: bool,
) -> rusqlite::Result<Vec<TurnChanges>> {
    let mut st = tx.prepare(
        "SELECT id, role, tool_name, entry_id, status FROM messages WHERE chat_id = ?1 AND active = 1 ORDER BY sequence, id",
    )?;
    let rows: Vec<Row> = st
        .query_map([chat_id], |r| {
            Ok(Row {
                id: r.get(0)?,
                role: r.get(1)?,
                tool: r.get(2)?,
                entry: r.get(3)?,
                status: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    // Split into turns at each user message.
    let mut turns: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start = 0;
    for (i, r) in rows.iter().enumerate() {
        if r.role == "user" && i > start {
            turns.push(start..i);
            start = i;
        }
    }
    if start < rows.len() {
        turns.push(start..rows.len());
    }
    let wanted: std::collections::HashSet<i64> = ids.iter().take(MAX_IDS).copied().collect();
    let mut out = Vec::new();
    let mut budget = MAX_EVENTS;
    let mut cache = HashMap::new();
    let last_turn = turns.len().saturating_sub(1);
    let mut picked: Vec<(usize, i64)> = Vec::new();
    for (ti, t) in turns.iter().enumerate() {
        // The turn's last assistant reply carries the card, once the turn is over.
        let Some(end) = rows[t.clone()].iter().rev().find(|r| r.role == "assistant") else {
            continue;
        };
        if !wanted.contains(&end.id) || (ti == last_turn && chat_running) {
            continue;
        }
        picked.push((ti, end.id));
    }
    let skip = picked.len().saturating_sub(MAX_TURNS);
    for (ti, message_id) in picked.into_iter().skip(skip) {
        let mut files: Vec<(String, Vec<Event>, bool)> = Vec::new();
        let mut truncated = false;
        let mut count = 0;
        for r in &rows[turns[ti].clone()] {
            let Some(tool) = r.tool.as_deref() else {
                continue;
            };
            if !matches!(tool, "Edit" | "MultiEdit" | "Write") || r.status != "complete" {
                continue;
            }
            if count >= MAX_EVENTS_PER_TURN || budget == 0 {
                truncated = true;
                break;
            }
            let Some(entry) = r.entry.as_deref() else {
                continue;
            };
            let raw: Option<String> = tx
                .query_row(
                    "SELECT raw FROM entries WHERE chat_id = ?1 AND entry_id = ?2",
                    params![chat_id, entry],
                    |x| x.get(0),
                )
                .optional()?;
            let Some(raw) = raw.and_then(|r| serde_json::from_str::<Value>(&r).ok()) else {
                continue;
            };
            if raw.get("isError").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let args = raw
                .get("args")
                .and_then(Value::as_str)
                .and_then(|a| serde_json::from_str::<Value>(a).ok());
            let result = raw
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some((path, evs, whole)) = args.as_ref().and_then(|a| events_of(tool, a, result))
            else {
                truncated = true;
                continue;
            };
            count += 1;
            budget -= 1;
            let key = path.to_lowercase();
            match files.iter_mut().find(|(p, _, _)| p.to_lowercase() == key) {
                Some((_, list, ok)) => {
                    list.extend(evs);
                    *ok &= whole;
                }
                None => files.push((path, evs, whole)),
            }
        }
        let files = files
            .into_iter()
            .map(|(path, evs, whole)| {
                let Net {
                    added,
                    removed,
                    complete,
                } = netchange::net(&evs);
                let complete = complete && whole;
                let root = repo_root(Path::new(&path), &mut cache);
                let repository_path = match &root {
                    Some(r) => Path::new(&path)
                        .strip_prefix(r)
                        .map(|p| p.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|_| path.clone()),
                    None => path.clone(),
                };
                TurnFile {
                    repository_root: root.map(|r| r.to_string_lossy().into_owned()),
                    repository_path,
                    added: complete.then_some(added),
                    removed: complete.then_some(removed),
                    complete_counts: complete && !truncated,
                    path,
                }
            })
            .collect();
        out.push(TurnChanges {
            message_id,
            files,
            truncated: truncated || skip > 0,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};
    use serde_json::json;

    fn db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &MigrationRegistry::new()).unwrap();
        db.write(|tx| {
            tx.execute("INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'p', 'C:/p', 0)", [])?;
            tx.execute("INSERT INTO chats (id, project_id, created_at, updated_at) VALUES ('c', 'p', 0, 0)", [])
        })
        .unwrap();
        (dir, db)
    }

    fn row(tx: &Transaction<'_>, seq: i64, role: &str, tool: Option<(&str, Value, bool)>) -> i64 {
        let entry = format!("e{seq}");
        tx.execute(
            "INSERT INTO messages (chat_id, source_key, role, tool_name, status, timestamp, sequence, entry_id) VALUES ('c', ?1, ?2, ?3, ?4, 0, ?5, ?1)",
            params![entry, role, tool.as_ref().map(|t| t.0), if tool.as_ref().is_some_and(|t| t.2) { "error" } else { "complete" }, seq],
        )
        .unwrap();
        if let Some((name, args, err)) = tool {
            let raw = json!({"id": entry, "type": "tool", "toolName": name, "args": args.to_string(), "isError": err, "result": "File created successfully"});
            tx.execute(
                "INSERT INTO entries (chat_id, entry_id, kind, raw) VALUES ('c', ?1, 'tool', ?2)",
                params![entry, raw.to_string()],
            )
            .unwrap();
        }
        tx.last_insert_rowid()
    }

    #[test]
    fn a_completed_turn_lists_its_files_with_net_counts() {
        let (_d, db) = db();
        let (end1, end2) = db
            .write(|tx| {
                row(tx, 1, "user", None);
                row(tx, 2, "toolResult", Some(("Edit", json!({"file_path": "C:/x/a.ts", "old_string": "one", "new_string": "two"}), false)));
                row(tx, 3, "toolResult", Some(("Edit", json!({"file_path": "C:/X/A.ts", "old_string": "two", "new_string": "three"}), false)));
                row(tx, 4, "toolResult", Some(("Write", json!({"file_path": "C:/x/new.md", "content": "a\nb\n"}), false)));
                row(tx, 5, "toolResult", Some(("Edit", json!({"file_path": "C:/x/failed.ts", "old_string": "a", "new_string": "b"}), true)));
                row(tx, 6, "toolResult", Some(("Bash", json!({"command": "ls"}), false)));
                let end1 = row(tx, 7, "assistant", None);
                row(tx, 8, "user", None);
                let end2 = row(tx, 9, "assistant", None);
                Ok::<_, rusqlite::Error>((end1, end2))
            })
            .unwrap();
        let out = db
            .write(|tx| review_turns(tx, "c", &[end1, end2], false))
            .unwrap();
        let t1 = out.iter().find(|t| t.message_id == end1).unwrap();
        let by: HashMap<_, _> = t1
            .files
            .iter()
            .map(|f| (f.path.as_str(), (f.added, f.removed, f.complete_counts)))
            .collect();
        assert_eq!(by.len(), 2, "same file in two spellings is one entry; failed and non-file tools are skipped: {by:?}");
        assert_eq!(
            by["C:/x/a.ts"],
            (Some(1), Some(1), true),
            "repeated edits count once"
        );
        assert_eq!(by["C:/x/new.md"], (Some(2), Some(0), true));
        assert!(out
            .iter()
            .find(|t| t.message_id == end2)
            .unwrap()
            .files
            .is_empty());
    }

    #[test]
    fn a_running_last_turn_has_no_card_yet_and_unrequested_ids_are_skipped() {
        let (_d, db) = db();
        let end = db
            .write(|tx| {
                row(tx, 1, "user", None);
                row(
                    tx,
                    2,
                    "toolResult",
                    Some((
                        "Write",
                        json!({"file_path": "C:/x/a", "content": "a"}),
                        false,
                    )),
                );
                Ok::<_, rusqlite::Error>(row(tx, 3, "assistant", None))
            })
            .unwrap();
        assert!(db
            .write(|tx| review_turns(tx, "c", &[end], true))
            .unwrap()
            .is_empty());
        assert_eq!(
            db.write(|tx| review_turns(tx, "c", &[end], false))
                .unwrap()
                .len(),
            1
        );
        assert!(db
            .write(|tx| review_turns(tx, "c", &[], false))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_repository_root_is_found_upward_and_paths_made_relative() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let file = dir.path().join("src").join("gone.ts");
        let mut cache = HashMap::new();
        assert_eq!(
            repo_root(&file, &mut cache).as_deref(),
            Some(dir.path()),
            "works for a file that no longer exists"
        );
        assert!(cache.contains_key(&dir.path().join("src")));
    }

    #[test]
    fn tool_arguments_become_events() {
        let (p, e, whole) = events_of("MultiEdit", &json!({"file_path": "C:/a", "edits": [{"old_string": "a", "new_string": "b"}, {"old_string": "c", "new_string": "d", "replace_all": true}]}), "").unwrap();
        assert_eq!(p, "C:/a");
        assert_eq!(e.len(), 2);
        assert!(whole);
        let (_, e, whole) = events_of("MultiEdit", &json!({"file_path": "C:/a", "edits": [{"old_string": "a", "new_string": "b"}, {"broken": 1}]}), "").unwrap();
        assert_eq!(
            (e.len(), whole),
            (1, false),
            "a dropped step makes the file incomplete"
        );
        let created = |r: &str| {
            matches!(
                events_of("Write", &json!({"file_path": "C:/a", "content": "x"}), r)
                    .unwrap()
                    .1[0],
                Event::Write { created: true, .. }
            )
        };
        assert!(created("File created successfully at: C:/a"));
        assert!(!created("The file C:/a has been updated."));
        assert!(events_of("Read", &json!({"file_path": "C:/a"}), "").is_none());
        assert!(events_of("Edit", &json!({"file_path": "C:/a"}), "").is_none());
    }
}
