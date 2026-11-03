//! Reading one Claude Code record file (JSONL, one record per line).
//!
//! The shape is what the CLI writes for its own `--resume`: user and assistant records whose
//! `message.content` is blocks (`text`, `tool_use`, `tool_result`), a `cwd` and `sessionId` on
//! every record, and a `summary` record naming the chat once it has a title. Sidechain records
//! belong to sub-agent runs and `isMeta` records are the CLI's own scaffolding (slash-command
//! expansion, local command output): both stay out of what Helm shows. Lines that are not valid
//! JSON or not a shape we know are skipped — a record file is still being appended to while
//! Claude Code runs, so a torn last line is normal.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Message text longer than this is cut for indexing and display.
pub const MAX_TEXT_BYTES: usize = 32 * 1024;
/// A tool call's arguments, same bound (they are shown in the read view).
pub const MAX_ARGS_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One decoded step of the conversation.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    Message {
        role: Role,
        text: String,
        timestamp: i64,
    },
    Tool {
        call_id: String,
        name: String,
        args: String,
        result: String,
        is_error: bool,
        timestamp: i64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    pub session_id: String,
    pub cwd: String,
    /// The chat's own title record, else its first prompt.
    pub title: String,
    /// User and assistant messages counted.
    pub message_count: i64,
    /// The newest timestamp seen (0 when the records carried none).
    pub last_ts: i64,
    pub lines: Vec<Decoded>,
}

/// Parse a whole record file. The session id falls back to the file's name stem, which is how
/// Claude Code names these files.
pub fn parse_file(path: &Path) -> std::io::Result<Transcript> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut t = Transcript {
        session_id: String::new(),
        cwd: String::new(),
        title: String::new(),
        message_count: 0,
        last_ts: 0,
        lines: Vec::new(),
    };
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(e) => return Err(e),
            Ok(_) => {}
        }
        let Ok(record) = serde_json::from_str::<Value>(line.trim_end()) else {
            continue;
        };
        let ts = record_ts(&record);
        if ts > t.last_ts {
            t.last_ts = ts;
        }
        if t.session_id.is_empty() {
            if let Some(s) = record.get("sessionId").and_then(Value::as_str) {
                t.session_id = s.to_owned();
            }
        }
        if t.cwd.is_empty() {
            if let Some(s) = record.get("cwd").and_then(Value::as_str) {
                t.cwd = s.to_owned();
            }
        }
        match record.get("type").and_then(Value::as_str) {
            // The last summary record wins: it is the running title.
            Some("summary") => {
                if let Some(s) = record.get("summary").and_then(Value::as_str) {
                    t.title = s.trim().to_owned();
                }
            }
            Some("user") if skip(&record) => {}
            Some("user") => {
                let content = record
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .cloned()
                    .unwrap_or(Value::Null);
                if content.is_null() {
                    continue;
                }
                if let Some(text) = plain_text(&content) {
                    push_message(&mut t, Role::User, &text, ts);
                }
                collect_results(&mut t, &content, ts);
            }
            Some("assistant") if skip(&record) => {}
            Some("assistant") => {
                let Some(content) = record.get("message").and_then(|m| m.get("content")) else {
                    continue;
                };
                if let Some(text) = plain_text(content) {
                    push_message(&mut t, Role::Assistant, &text, ts);
                }
                if let Some(blocks) = content.as_array() {
                    for b in blocks {
                        if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                            continue;
                        }
                        let Some(call_id) = b.get("id").and_then(Value::as_str) else {
                            continue;
                        };
                        let args = b
                            .get("input")
                            .and_then(|i| serde_json::to_string(i).ok())
                            .unwrap_or_default();
                        t.lines.push(Decoded::Tool {
                            call_id: call_id.to_owned(),
                            name: b
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_owned(),
                            args: cut_bytes(&args, MAX_ARGS_BYTES).into_owned(),
                            result: String::new(),
                            is_error: false,
                            timestamp: ts,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if t.session_id.is_empty() {
        t.session_id = stem;
    }
    if t.title.is_empty() {
        if let Some(Decoded::Message { text, .. }) = t.lines.iter().find(|l| {
            matches!(
                l,
                Decoded::Message {
                    role: Role::User,
                    ..
                }
            )
        }) {
            t.title = text
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>()
                .trim()
                .to_owned();
        }
    }
    Ok(t)
}

fn skip(record: &Value) -> bool {
    record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || record.get("isMeta").and_then(Value::as_bool) == Some(true)
}

fn push_message(t: &mut Transcript, role: Role, text: &str, ts: i64) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    t.message_count += 1;
    t.lines.push(Decoded::Message {
        role,
        text: cut_bytes(text, MAX_TEXT_BYTES).into_owned(),
        timestamp: ts,
    });
}

/// The text of a string content, or of the `text` blocks of an array, joined. `None` when there
/// is no text at all (a pure tool_result record).
fn plain_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_owned());
    }
    let blocks = content.as_array()?;
    let parts: Vec<&str> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n"))
}

/// Attach tool results to the calls they answer (matched by call id, newest pending first).
fn collect_results(t: &mut Transcript, content: &Value, ts: i64) {
    let Some(blocks) = content.as_array() else {
        return;
    };
    for b in blocks {
        if b.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(call_id) = b.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let result = match b.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(c) => {
                plain_text(c).unwrap_or_else(|| serde_json::to_string(c).unwrap_or_default())
            }
            None => String::new(),
        };
        let Some(Decoded::Tool {
            result: slot,
            is_error,
            timestamp,
            ..
        }) = t.lines.iter_mut().rev().find(|l| {
            matches!(l, Decoded::Tool { call_id: c, result, .. } if c == call_id && result.is_empty())
        }) else {
            continue;
        };
        *slot = cut_bytes(&result, MAX_TEXT_BYTES).into_owned();
        *is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        *timestamp = ts;
    }
}

fn record_ts(record: &Value) -> i64 {
    record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or(0, |d| d.timestamp_millis())
}

/// A byte-budget cut on a char boundary.
fn cut_bytes(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.len() <= max {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    std::borrow::Cow::Owned(s[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, lines: &[&str]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        p
    }

    fn record(fields: &str) -> String {
        format!("{{\"sessionId\":\"s-1\",\"cwd\":\"C:/proj\",\"timestamp\":\"2026-11-03T10:00:00.000Z\",{fields}}}")
    }

    #[test]
    fn one_pass_decodes_messages_tools_results_title_and_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "bbbbbbbb-0000-0000-0000-000000000001.jsonl",
            &[
                &record("\"type\":\"user\",\"isMeta\":true,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"<local-command>ignored\"}]}"),
                &record("\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"fix the login\"}]}"),
                &record("\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"hmm\"},{\"type\":\"text\",\"text\":\"looking now\"},{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"Bash\",\"input\":{\"command\":\"npm test\"}}]}"),
                &record("\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_9\",\"content\":\"all green\",\"is_error\":false}]}"),
                &record("\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"subagent chatter\"}]}"),
                &record("\"type\":\"file-history-snapshot\",\"message\":\"junk\""),
                "not json at all {{{",
                &record("\"type\":\"summary\",\"summary\":\"Login work\""),
            ],
        );
        let t = parse_file(&path).unwrap();
        assert_eq!(t.session_id, "s-1");
        assert_eq!(t.cwd, "C:/proj");
        assert_eq!(t.title, "Login work");
        assert_eq!(t.message_count, 2, "meta and sidechain stay out");
        assert_eq!(t.last_ts, 1_793_700_000_000);
        let tools: Vec<&Decoded> = t
            .lines
            .iter()
            .filter(|l| matches!(l, Decoded::Tool { .. }))
            .collect();
        assert_eq!(tools.len(), 1);
        match &tools[0] {
            Decoded::Tool {
                call_id,
                name,
                args,
                result,
                is_error,
                timestamp,
            } => {
                assert_eq!((call_id.as_str(), name.as_str()), ("toolu_9", "Bash"));
                assert_eq!(args, "{\"command\":\"npm test\"}");
                assert_eq!(result, "all green");
                assert!(!*is_error);
                assert_eq!(*timestamp, 1_793_700_000_000, "the result's time wins");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn falls_back_to_the_file_stem_first_prompt_and_no_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "aaaaaaaa-0000-0000-0000-000000000002.jsonl",
            &[
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"\\n\\n  Plan the migration  \"}}",
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"second prompt\"}}",
            ],
        );
        let t = parse_file(&path).unwrap();
        assert_eq!(t.session_id, "aaaaaaaa-0000-0000-0000-000000000002");
        assert_eq!(t.title, "Plan the migration", "first prompt, trimmed");
        assert_eq!(t.message_count, 2);
        assert_eq!(t.last_ts, 0);
        assert_eq!(t.cwd, "");
    }

    #[test]
    fn long_text_is_cut_on_char_boundaries_and_results_match_the_newest_call() {
        let dir = tempfile::tempdir().unwrap();
        let big = "字あ".repeat(20_000);
        let path = write(
            dir.path(),
            "c.jsonl",
            &[
                &format!("{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Read\",\"input\":{{\"file\":\"{}\"}}}}]}}}}", "x".repeat(8_000)),
                &format!("{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Read\",\"input\":{{}}}}]}}}}"),
                &format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"{big}\"}}]}}}}"),
            ],
        );
        let t = parse_file(&path).unwrap();
        let filled: Vec<&Decoded> = t
            .lines
            .iter()
            .filter(|l| matches!(l, Decoded::Tool { result, .. } if !result.is_empty()))
            .collect();
        assert_eq!(filled.len(), 1, "the newest open call got the result");
        let Decoded::Tool { result, .. } = filled[0] else {
            unreachable!()
        };
        assert!(
            result.len() <= MAX_TEXT_BYTES && result.len() + 3 > MAX_TEXT_BYTES,
            "cut to the last whole character: {}",
            result.len()
        );
        // The first call (the one the result did not go to) carried the oversized arguments.
        let Decoded::Tool { args, .. } = t
            .lines
            .iter()
            .find(|l| matches!(l, Decoded::Tool { call_id, result, .. } if *call_id == "t1" && result.is_empty()))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            args.len(),
            MAX_ARGS_BYTES,
            "arguments cut at the byte bound"
        );
    }
}
