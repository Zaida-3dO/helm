//! One Claude Code record file (JSONL) to one session doc, with the same turn shape as a
//! Helm chat: the ask plus the final assistant answer before the next ask.
//!
//! This is the conversation only: tool calls and tool results, sidechain records (a sub-agent
//! run's own steps, which live in separate files anyway) and the CLI's own meta records stay
//! out — that traffic is this session's drill-down material and answered from the file. The
//! first line that decodes supplies the session id and folder (`cwd`) when they are not
//! already known; the last `summary` record, if any, names the doc, else the first ask does.
//! Whether a record file is a main session or a duplicate of a chat Helm already indexed is
//! the indexer's call, not this reader's; here every file is taken at its word.

use super::{
    finish_doc, fmt_date, wrap_capped, ChunkOut, DocOut, ASK_PREFIX_CHARS, MAX_CHUNK_CHARS,
};
use serde_json::Value;

/// Build a session doc from a whole record file. `session_id` is the caller's stable name
/// (the file stem is what Claude Code uses); it becomes the doc's key and locator. Returns
/// `None` when the file holds no asked turn.
pub fn build_session_doc(session_id: &str, jsonl: &str) -> Option<DocOut> {
    let mut session_cwd = String::new();
    let mut summary: Option<String> = None;
    let mut turns: Vec<Turn> = Vec::new();
    let mut first_at = i64::MAX;
    let mut last_at = 0i64;
    let mut any_ts = false;

    for (idx, line) in jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue; // a torn or unknown line: skip, the file is being appended to
        };
        if record.get("isMeta").and_then(Value::as_bool) == Some(true)
            || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        if session_cwd.is_empty() {
            if let Some(s) = record.get("cwd").and_then(Value::as_str) {
                session_cwd = s.to_string();
            }
        }
        if record.get("type").and_then(Value::as_str) == Some("summary") {
            if let Some(s) = record.get("summary").and_then(Value::as_str) {
                summary = Some(s.to_string());
            }
            continue;
        }
        let Some(ts) = record_ts(&record) else {
            continue;
        };
        if ts < first_at {
            first_at = ts;
        }
        if ts > last_at {
            last_at = ts;
        }
        any_ts = true;
        let role = record.get("type").and_then(Value::as_str).unwrap_or("");
        let text = message_text(&record);
        if text.trim().is_empty() {
            continue; // a tool-result row or an empty message carries nothing to search
        }
        let anchor = match record.get("uuid").and_then(Value::as_str) {
            Some(u) => u.to_string(),
            None => idx.to_string(),
        };
        match role {
            "user" => turns.push(Turn {
                at: ts,
                ask: text.to_string(),
                reply: String::new(),
                anchor,
            }),
            "assistant" => {
                if let Some(turn) = turns.last_mut() {
                    turn.reply = text.to_string(); // the last answer before the next ask wins
                }
            }
            _ => {}
        }
    }
    if turns.is_empty() || !any_ts {
        return None;
    }
    let first_at = if first_at == i64::MAX { 0 } else { first_at };
    let title = summary
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| clip(turns[0].ask.trim(), 200));
    let header = format!(
        "{title}\n{session_cwd}\n{n} turns, {from} to {to}\nFirst ask: {ask}",
        n = turns.len(),
        from = fmt_date(first_at),
        to = fmt_date(last_at),
        ask = clip(turns[0].ask.trim(), ASK_PREFIX_CHARS),
    );
    let mut chunks = vec![ChunkOut {
        ord: 0,
        heading: String::new(),
        text: header,
        at: first_at,
        anchor: None,
    }];
    for turn in &turns {
        let ask = clip(turn.ask.trim(), 1500);
        let reply = turn.reply.trim();
        if reply.is_empty() {
            chunks.push(ChunkOut {
                ord: 0,
                heading: String::new(),
                text: ask,
                at: turn.at,
                anchor: Some(turn.anchor.clone()),
            });
            continue;
        }
        let whole = format!("{ask}\n\n{reply}");
        if whole.chars().count() <= MAX_CHUNK_CHARS {
            chunks.push(ChunkOut {
                ord: 0,
                heading: String::new(),
                text: whole,
                at: turn.at,
                anchor: Some(turn.anchor.clone()),
            });
            continue;
        }
        let prefix = format!("{}\n\n", clip(turn.ask.trim(), ASK_PREFIX_CHARS));
        let cap = (MAX_CHUNK_CHARS - prefix.chars().count()).max(1);
        for piece in wrap_capped(reply, cap) {
            chunks.push(ChunkOut {
                ord: 0,
                heading: String::new(),
                text: format!("{prefix}{piece}"),
                at: turn.at,
                anchor: Some(turn.anchor.clone()),
            });
        }
    }
    Some(finish_doc(
        session_id.to_string(),
        title,
        session_id.to_string(),
        first_at,
        last_at,
        chunks,
    ))
}

struct Turn {
    at: i64,
    ask: String,
    reply: String,
    anchor: String,
}

/// The text of a record's `message.content`: the string itself (how a typed prompt is stored),
/// or the first `text` block of a list.
fn message_text(record: &Value) -> &str {
    let content = record.get("message").and_then(|m| m.get("content"));
    if let Some(s) = content.and_then(Value::as_str) {
        return s;
    }
    content
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        })
        .and_then(|b| b.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// The record's timestamp (`YYYY-MM-DDTHH:MM:SS[.mmm]Z`, UTC) in Unix milliseconds.
pub(crate) fn record_ts(record: &Value) -> Option<i64> {
    let s = record.get("timestamp").and_then(Value::as_str)?;
    let (date, rest) = s.split_once('T')?;
    let day = super::days_from_civil(
        date.get(0..4)?.parse().ok()?,
        date.get(5..7)?.parse().ok()?,
        date.get(8..10)?.parse().ok()?,
    );
    let time = rest.trim_end_matches('Z');
    let mut parts = time.split(':');
    let hh: u32 = parts.next()?.parse().ok()?;
    let mm: u32 = parts.next()?.parse().ok()?;
    let secs: f64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&(date.get(5..7)?.parse::<u32>().ok()?)) {
        return None;
    }
    Some(day * 86_400_000 + hh as i64 * 3_600_000 + mm as i64 * 60_000 + (secs * 1000.0) as i64)
}

fn clip(s: &str, chars: usize) -> String {
    if s.chars().count() <= chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(uuid: &str, ts: &str, text: &str) -> String {
        format!(
            "{{\"type\":\"user\",\"uuid\":\"{uuid}\",\"timestamp\":\"{ts}\",\"cwd\":\"/work/site\",\"sessionId\":\"abc\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}"
        )
    }

    fn assistant(uuid: &str, ts: &str, text: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"uuid\":\"{uuid}\",\"timestamp\":\"{ts}\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}"
        )
    }

    #[test]
    fn turns_header_and_skips() {
        let jsonl = [
            r#"{"type":"summary","summary":"Fixing the relay","sessionId":"abc"}"#.to_string(),
            user("u1", "2026-11-10T09:00:00Z", "why does the line drop?"),
            r#"{"type":"user","uuid":"t1","timestamp":"2026-11-10T09:00:05Z","message":{"content":[{"type":"tool_result","tool_use_id":"x","content":"ls output"}]}}"#.to_string(),
            assistant("a1", "2026-11-10T09:00:10Z", "first thought."),
            r#"{"type":"assistant","uuid":"a2","timestamp":"2026-11-10T09:00:20Z","isSidechain":true,"message":{"content":[{"type":"text","text":"sub-agent chatter"}]}}"#.to_string(),
            assistant("a3", "2026-11-10T09:00:30Z", "a ping-pong in the handshake."),
            user("u2", "2026-11-10T09:05:00Z", "fixed, thanks"),
            r#"not json at all, torn line"#.to_string(),
            assistant("a4", "2026-11-10T09:05:10Z", "glad to hear it."),
        ]
        .join("\n");
        let doc = build_session_doc("abc", &jsonl).unwrap();
        assert_eq!(doc.title, "Fixing the relay", "the summary names the doc");
        assert_eq!(doc.key, "abc");
        // header + two asked turns
        assert_eq!(doc.chunks.len(), 3);
        assert!(doc.chunks[0].text.contains("/work/site"));
        assert!(doc.chunks[0].text.contains("2 turns"));
        assert_eq!(
            doc.chunks[1].text,
            "why does the line drop?\n\na ping-pong in the handshake."
        );
        assert_eq!(doc.chunks[1].anchor.as_deref(), Some("u1"));
        assert_eq!(doc.chunks[2].text, "fixed, thanks\n\nglad to hear it.");
        let t0 = record_ts(&serde_json::json!({"timestamp": "2026-11-10T09:00:00Z"})).unwrap();
        assert_eq!(doc.chunks[1].at, t0);
        assert_eq!(
            doc.occurred_to,
            record_ts(&serde_json::json!({"timestamp": "2026-11-10T09:05:10Z"})).unwrap()
        );
    }

    #[test]
    fn tables() {
        // no summary: the first ask titles it
        let jsonl = user("u1", "2026-01-01T00:00:00.500Z", "a question about fonts?");
        let d = build_session_doc("s", &jsonl).unwrap();
        assert_eq!(d.title, "a question about fonts?");
        assert_eq!(d.chunks.len(), 2);
        assert_eq!(d.chunks[1].text, "a question about fonts?");
        // fractional seconds land on the millisecond
        assert_eq!(
            d.chunks[1].at,
            super::super::days_from_civil(2026, 1, 1) * 86_400_000 + 500
        );

        // a typed prompt is stored as a plain string
        let typed = r#"{"type":"user","uuid":"u9","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"typed prompt"}}"#;
        assert_eq!(
            build_session_doc("s", typed).unwrap().chunks[1].text,
            "typed prompt"
        );

        // a file with no asked turn is nothing to index
        assert!(build_session_doc("s", "").is_none());
        let only_tool = r#"{"type":"user","uuid":"t","timestamp":"2026-01-01T00:00:00Z","message":{"content":[{"type":"tool_result","tool_use_id":"x","content":"out"}]}}"#.to_string();
        assert!(build_session_doc("s", &only_tool).is_none());

        // a long answer splits with the ask repeated on every piece
        let long = "answer word ".repeat(400); // 4800 chars
        let jsonl = [
            user("u1", "2026-02-02T10:00:00Z", "tell me all about it"),
            assistant("a1", "2026-02-02T10:00:05Z", &long),
        ]
        .join("\n");
        let d = build_session_doc("s", &jsonl).unwrap();
        assert!(d.chunks.len() >= 3);
        for c in &d.chunks[1..] {
            assert!(c.text.chars().count() <= MAX_CHUNK_CHARS);
            assert!(c.text.starts_with("tell me all about it\n\n"));
        }

        // meta records never ask
        let jsonl = r#"{"type":"user","uuid":"m","timestamp":"2026-01-01T00:00:00Z","isMeta":true,"message":{"content":[{"type":"text","text":"<local-command>"}]}}"#.to_string();
        assert!(build_session_doc("s", &jsonl).is_none());

        // stable hash for the same file, different for a new turn
        let a = build_session_doc("s", &user("u1", "2026-01-01T00:00:00Z", "one")).unwrap();
        let b = build_session_doc("s", &user("u1", "2026-01-01T00:00:00Z", "one")).unwrap();
        assert_eq!(a.hash, b.hash);
        assert_ne!(
            a.hash,
            build_session_doc("s", &user("u1", "2026-01-01T00:00:00Z", "two"))
                .unwrap()
                .hash
        );
    }
}
