//! What this PC answers when the hub asks.
//!
//! **`ix_embed`**: embed a search query, or say the graphics card is reserved, so the hub asks its
//! own embedder instead. The hub gives up after 600 ms and ignores a late answer.
//!
//! **`ix_trace`**: search the work trace of one chat for a viewer: the thinking, tool results and
//! intermediate narration that the hub's index does not hold. For a Helm chat that is the chat's own
//! message rows; for a Claude Code session it is one scan of the record's file, at most
//! [`SCAN_BUDGET`]. The viewer is a hub person; the chat must be one their profile here owns or
//! has been shown, and a Claude Code session is its home person's alone. The hub checks the viewer
//! may read the doc before it asks, and this is the second check.

use crate::engine::{EmbedErr, Embedder};
use crate::feeds::Env;
use crate::hub::{Embedded, Responder};
use crate::model::{normalise_model, Kind};
use crate::sources::claude_code::record_ts;
use crate::store;
use helm_core::secrets::looks_like_secret;
use helm_profiles::store as profiles;
use rusqlite::params;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A Claude Code record is scanned for at most this long.
pub const SCAN_BUDGET: Duration = Duration::from_secs(3);
const SNIPPET: usize = 300;
/// How much of one Claude Code record is read: as much as any record holds in practice.
const RECORD_READ: usize = 4_000_000;
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;

pub struct Answerer {
    pub env: Arc<Env>,
    pub embedder: Arc<dyn Embedder>,
}

fn person_of(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    }
}

/// A moment as the hub may send it: milliseconds, or an ISO 8601 string.
fn millis(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s.trim())
            .ok()
            .map(|t| t.timestamp_millis()),
        _ => None,
    }
}

/// About `SNIPPET` characters around the first place `needle` (lowercase) occurs in `hay`.
pub fn snippet(hay: &str, needle: &str) -> String {
    let chars: Vec<char> = hay.chars().collect();
    if needle.is_empty() {
        return chars.iter().take(SNIPPET).collect();
    }
    let lower: Vec<char> = hay.to_lowercase().chars().collect();
    let n: Vec<char> = needle.chars().collect();
    // Lowercasing can change a text's length; then the match is shown from the start.
    let at = if lower.len() == chars.len() {
        lower.windows(n.len()).position(|w| w == n.as_slice())
    } else {
        None
    }
    .unwrap_or(0);
    let from = at.saturating_sub(SNIPPET / 3);
    let text: String = chars[from..].iter().take(SNIPPET).collect();
    format!(
        "{}{}{}",
        if from > 0 { "…" } else { "" },
        text.replace('\n', " "),
        if from + SNIPPET < chars.len() {
            "…"
        } else {
            ""
        }
    )
}

/// Every string in a JSON record, depth-first, up to a budget of characters.
fn strings(v: &Value, out: &mut String, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    match v {
        Value::String(s) => {
            let take: String = s.chars().take(*budget).collect();
            *budget -= take.chars().count();
            out.push_str(&take);
            out.push('\n');
        }
        Value::Array(a) => a.iter().for_each(|x| strings(x, out, budget)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out, budget)),
        _ => {}
    }
}

impl Answerer {
    fn trace_chat(
        &self,
        viewer: &str,
        key: &str,
        query: &str,
        limit: usize,
        after: i64,
        before: i64,
    ) -> Result<Vec<Value>, String> {
        let chat = key.rsplit_once('#').map_or(key, |(c, _)| c);
        let needle = query.to_lowercase();
        self.env
            .db
            .snapshot(|tx| {
                let Some(me) = profiles::profile_for_person(tx, viewer)? else {
                    return Ok(Err("This PC does not act for that person.".to_owned()));
                };
                let owner: Option<String> = tx
                    .query_row("SELECT profile_id FROM chats WHERE id = ?1", [chat], |r| {
                        r.get(0)
                    })
                    .ok();
                let Some(owner) = owner else {
                    return Ok(Err("That chat is not on this PC.".to_owned()));
                };
                let shared: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM chat_shares WHERE chat_id = ?1 AND profile_id = ?2)",
                    params![chat, me.id],
                    |r| r.get(0),
                )?;
                if !(owner == me.id || (owner.is_empty() && me.is_home) || shared) {
                    return Ok(Err("That chat is not shown to this person.".to_owned()));
                }
                // No LIMIT in the query: a message that looks like a credential is skipped, and
                // the limit counts what is left.
                let mut st = tx.prepare(
                    "SELECT id, role, text, thinking, timestamp FROM messages
                     WHERE chat_id = ?1 AND timestamp >= ?2 AND timestamp <= ?3
                       AND (?4 = '' OR instr(lower(text), ?4) > 0 OR instr(lower(thinking), ?4) > 0)
                     ORDER BY timestamp, id",
                )?;
                let mut found = st.query(params![chat, after, before, needle])?;
                let mut rows = Vec::new();
                while let Some(r) = found.next()? {
                    if rows.len() >= limit {
                        break;
                    }
                    let (id, role, text, thinking, at): (i64, String, String, String, i64) =
                        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
                    // Whole texts, not the snippet: a credential near the match is as good as in it.
                    if looks_like_secret(&text) || looks_like_secret(&thinking) {
                        continue;
                    }
                    let hay = if !needle.is_empty() && !text.to_lowercase().contains(&needle) {
                        thinking
                    } else {
                        text
                    };
                    rows.push(json!({
                        "messageId": id, "role": role, "at": at,
                        "snippet": snippet(&hay, &needle),
                        "link": format!("helm://chats/{chat}/messages/{id}"),
                    }));
                }
                Ok::<_, rusqlite::Error>(Ok(rows))
            })
            .map_err(|e| e.to_string())?
    }

    #[allow(clippy::too_many_arguments)]
    fn trace_session(
        &self,
        viewer: &str,
        owner: Option<&str>,
        key: &str,
        query: &str,
        limit: usize,
        after: i64,
        before: i64,
    ) -> Result<Vec<Value>, String> {
        let session = key.rsplit_once('#').map_or(key, |(c, _)| c);
        // A session's id is a file stem: nothing that could name another path.
        if session.is_empty() || session.contains(['/', '\\', '.']) {
            return Err("That is not a session.".into());
        }
        // The PC's own person: the home profile's, or the one the hub filed the session under
        // when the home profile is not linked.
        let home = self
            .env
            .home()
            .and_then(|p| p.hub_person_id)
            .or_else(|| owner.map(str::to_owned));
        if home.as_deref() != Some(viewer) {
            return Err("Claude Code sessions on this PC belong to its own person.".into());
        }
        let root = self
            .env
            .claude_projects
            .clone()
            .ok_or("Claude Code's records were not found on this PC.")?;
        let file = std::fs::read_dir(&root)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|d| d.path().join(format!("{session}.jsonl")))
            .find(|p| p.is_file())
            .ok_or("That session's record is gone.")?;
        let needle = query.to_lowercase();
        let started = Instant::now();
        let mut out = Vec::new();
        for (i, line) in BufReader::new(std::fs::File::open(file).map_err(|e| e.to_string())?)
            .lines()
            .map_while(Result::ok)
            .enumerate()
        {
            if started.elapsed() > SCAN_BUDGET || out.len() >= limit {
                break;
            }
            let Ok(rec) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let at = record_ts(&rec).unwrap_or(0);
            if at != 0 && (at < after || at > before) {
                continue;
            }
            // The whole record is read for the credential check, so a token past the part a
            // snippet could show still holds the record back.
            let mut text = String::new();
            let mut budget = RECORD_READ;
            strings(&rec, &mut text, &mut budget);
            if looks_like_secret(&text) {
                continue;
            }
            if !needle.is_empty() && !text.to_lowercase().contains(&needle) {
                continue;
            }
            let id = rec
                .get("uuid")
                .and_then(Value::as_str)
                .map_or(i.to_string(), str::to_owned);
            out.push(json!({
                "role": rec.get("type").and_then(Value::as_str).unwrap_or(""),
                "at": at,
                "snippet": snippet(&text, &needle),
                "link": format!("claude-code:{session}@{id}"),
            }));
        }
        Ok(out)
    }
}

impl Responder for Answerer {
    fn embed(&self, text: &str) -> Embedded {
        let model = self
            .env
            .db
            .snapshot(store::load_settings)
            .map(|s| normalise_model(&s.model))
            .unwrap_or_default();
        match self.embedder.embed(&[text.to_owned()]) {
            Ok(mut v) if v.len() == 1 => Embedded::Vector(v.remove(0), model),
            Err(EmbedErr::Locked) => Embedded::Locked,
            _ => Embedded::Failed,
        }
    }

    fn trace(&self, frame: &Value) -> Value {
        let text = |k: &str| frame.get(k).and_then(Value::as_str).unwrap_or("");
        let bad = |why: &str| json!({"ok": false, "error": why});
        let Some(viewer) = frame.get("viewer").and_then(person_of) else {
            return bad("The request did not say who is asking.");
        };
        let limit = frame
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_LIMIT, |n| (n as usize).clamp(1, MAX_LIMIT));
        let after = frame.get("after").and_then(millis).unwrap_or(0);
        let before = frame.get("before").and_then(millis).unwrap_or(i64::MAX);
        let source = if text("kind") == "claude_code" {
            Kind::ClaudeCode
        } else {
            Kind::HelmChats
        };
        // What the switches say: the hub keeps answering from what it holds, but this PC's own
        // text is read for a search only while indexing is on and the source is one of its own.
        let allowed = self
            .env
            .db
            .snapshot(store::load_settings)
            .is_ok_and(|s| s.enabled)
            && self
                .env
                .db
                .snapshot(store::sources)
                .is_ok_and(|all| all.iter().any(|s| s.enabled && s.kind == source));
        if !allowed {
            return bad("Indexing is off here for that source.");
        }
        let done = match text("kind") {
            "claude_code" => self.trace_session(
                &viewer,
                frame.get("owner").and_then(person_of).as_deref(),
                text("chat"),
                text("query"),
                limit,
                after,
                before,
            ),
            _ => self.trace_chat(&viewer, text("chat"), text("query"), limit, after, before),
        };
        match done {
            Ok(results) => json!({"ok": true, "results": results}),
            Err(why) => bad(&why),
        }
    }
}
