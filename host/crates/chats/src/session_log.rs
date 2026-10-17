//! The session log: each chat's canonical, append-only history (format in
//! `SESSION-FORMAT.md` beside this crate's `Cargo.toml`).
//!
//! The host writes the log from worker events, then projects each entry into `messages` and
//! `entries` by `source_key` (the entry id). Projection is idempotent, so a crash between the
//! append and the projection is repaired by [`reconcile`] at startup. Torn lines are
//! passed over on read and quarantined to `<chat>.jsonl.torn`; more than [`MAX_TORN`] fail the read.

use crate::types::{ContextKind, MessageDetails, MessageRole, MessageStatus};
use helm_core::db::Database;
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const FORMAT_VERSION: &str = "helm-session 1";
pub const MAX_TORN: usize = 8;
/// Text kept on a message row; the rest lives in the artifact.
pub const ROW_TEXT_BYTES: usize = 64 * 1024;
pub const ROW_THINKING_BYTES: usize = 16 * 1024;
pub const THINKING_TAIL_CHARS: usize = 2400;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub version: String,
    pub chat_id: String,
    pub harness: String,
    pub cwd: String,
    pub created_at: i64,
}

/// A user or assistant message. `id` is its source key: `u:<submissionId>` or `m:<messageId>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEntry {
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    pub role: MessageRole,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub thinking: String,
    pub status: MessageStatus,
    #[serde(default)]
    pub details: MessageDetails,
}

/// One tool call, start and end merged, written when the call ends. `id` is `t:<toolCallId>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolEntry {
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    pub tool_call_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub args: String,
    #[serde(default)]
    pub result: String,
    pub is_error: bool,
    pub started_at: i64,
    pub ended_at: i64,
}

/// A context row: compaction, model or effort change, notice, restart. `id` is `c:<uuid>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntry {
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    pub kind: ContextKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Entry {
    Message(MessageEntry),
    Tool(ToolEntry),
    Context(ContextEntry),
}

impl Entry {
    pub fn id(&self) -> &str {
        match self {
            Entry::Message(e) => &e.id,
            Entry::Tool(e) => &e.id,
            Entry::Context(e) => &e.id,
        }
    }
    fn set_parent(&mut self, parent: Option<String>) {
        match self {
            Entry::Message(e) => e.parent_id = parent,
            Entry::Tool(e) => e.parent_id = parent,
            Entry::Context(e) => e.parent_id = parent,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Entry::Message(_) => "message",
            Entry::Tool(_) => "tool",
            Entry::Context(_) => "context",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("the session log has no header")]
    NoHeader,
    #[error("the session log repeats entry {0}")]
    DuplicateId(String),
    #[error("entry {0} names a parent that is not in the log")]
    UnknownParent(String),
    #[error("a session log entry has no id")]
    MissingId,
    #[error("the session log has more than {MAX_TORN} torn lines")]
    TooManyTorn,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A fragment the reader passed over.
#[derive(Clone, Debug, PartialEq)]
pub struct Torn {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct LogRead {
    pub header: Option<Header>,
    /// Every entry in file order, as raw objects (typed parsing happens at projection, so an
    /// entry kind this build does not know still holds its place in the chain).
    pub entries: Vec<Map<String, Value>>,
    pub torn: Vec<Torn>,
}

pub fn log_path(sessions: &Path, chat_id: &str) -> PathBuf {
    sessions.join(format!("{chat_id}.jsonl"))
}

fn torn_path(log: &Path) -> PathBuf {
    let mut p = log.as_os_str().to_owned();
    p.push(".torn");
    PathBuf::from(p)
}

/// Replace unpaired UTF-16 surrogate escapes with U+FFFD so the record parses.
fn sanitize_surrogates(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let hex4 = |at: usize| -> Option<u16> {
        s.get(at..at + 4)
            .and_then(|h| u16::from_str_radix(h, 16).ok())
    };
    while i < b.len() {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'u') {
            if let Some(u) = hex4(i + 2) {
                let is_high = (0xD800..0xDC00).contains(&u);
                let is_low = (0xDC00..0xE000).contains(&u);
                let next_low = b.get(i + 6) == Some(&b'\\')
                    && b.get(i + 7) == Some(&b'u')
                    && hex4(i + 8).is_some_and(|l| (0xDC00..0xE000).contains(&l));
                if (is_high && !next_low) || is_low {
                    out.push_str("\\ufffd");
                    i += 6;
                    continue;
                }
                if is_high && next_low {
                    out.push_str(&s[i..i + 12]);
                    i += 12;
                    continue;
                }
            }
            out.push_str(&s[i..i + 2]);
            i += 2;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn parse_object(bytes: &[u8]) -> Option<Map<String, Value>> {
    if let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(bytes) {
        return Some(m);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    match serde_json::from_str::<Value>(&sanitize_surrogates(text)) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// Read a session log. A missing file reads as empty.
pub fn read(path: &Path) -> Result<LogRead, LogError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LogRead::default()),
        Err(e) => return Err(e.into()),
    };
    let mut out = LogRead::default();
    let mut ids: HashSet<String> = HashSet::new();
    let mut offset = 0usize;
    let mut first = true;
    let ends_clean = bytes.last() == Some(&b'\n');
    let lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    let count = lines.len();
    for (idx, raw) in lines.into_iter().enumerate() {
        let line_offset = offset;
        offset += raw.len() + 1;
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let is_last = idx + 1 == count;
        if first {
            first = false;
            let header = parse_object(line)
                .filter(|m| m.get("type") == Some(&json!("header")))
                .and_then(|m| serde_json::from_value::<Header>(Value::Object(m)).ok())
                .ok_or(LogError::NoHeader)?;
            out.header = Some(header);
            continue;
        }
        let accept = |m: &Map<String, Value>, ids: &HashSet<String>| -> bool {
            let has_id = m.get("id").and_then(Value::as_str).is_some();
            let parent_ok = match m.get("parentId") {
                None | Some(Value::Null) => true,
                Some(Value::String(p)) => ids.contains(p),
                _ => false,
            };
            has_id && parent_ok
        };
        if let Some(m) = parse_object(line) {
            let id = m
                .get("id")
                .and_then(Value::as_str)
                .ok_or(LogError::MissingId)?
                .to_owned();
            if ids.contains(&id) {
                return Err(LogError::DuplicateId(id));
            }
            if !accept(&m, &ids) {
                return Err(LogError::UnknownParent(id));
            }
            ids.insert(id);
            out.entries.push(m);
            continue;
        }
        if is_last && !ends_clean {
            // A writer killed mid-append: the next append starts on a fresh line, and the
            // fragment then reads as a mid-file torn line. Leave it for now.
            continue;
        }
        // A mid-file torn line: recover a whole record that follows the damage, if its parent is
        // already known; quarantine everything before it.
        let mut recovered = None;
        for (p, _) in line
            .iter()
            .enumerate()
            .filter(|(p, &b)| *p > 0 && b == b'{')
        {
            if let Some(m) = parse_object(&line[p..]) {
                if accept(&m, &ids) {
                    let id = m["id"].as_str().unwrap().to_owned();
                    if !ids.contains(&id) {
                        recovered = Some((p, id, m));
                        break;
                    }
                }
            }
        }
        let cut = recovered.as_ref().map(|(p, _, _)| *p).unwrap_or(line.len());
        out.torn.push(Torn {
            offset: line_offset as u64,
            bytes: line[..cut].to_vec(),
        });
        if out.torn.len() > MAX_TORN {
            return Err(LogError::TooManyTorn);
        }
        if let Some((_, id, m)) = recovered {
            ids.insert(id);
            out.entries.push(m);
        }
    }
    Ok(out)
}

/// Append newly found torn fragments to the quarantine, de-duplicated by offset and hash. Returns
/// the fragments that were new.
pub fn quarantine(log: &Path, torn: &[Torn]) -> io::Result<Vec<Torn>> {
    if torn.is_empty() {
        return Ok(Vec::new());
    }
    let qpath = torn_path(log);
    let existing = std::fs::read_to_string(&qpath).unwrap_or_default();
    let seen: HashSet<(u64, String)> = existing
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| Some((v["offset"].as_u64()?, v["hash"].as_str()?.to_owned())))
        .collect();
    let mut fresh = Vec::new();
    let mut add = String::new();
    for t in torn {
        let hash = helm_core::sha256_hex(&t.bytes);
        if seen.contains(&(t.offset, hash.clone())) {
            continue;
        }
        add.push_str(
            &json!({"offset": t.offset, "hash": hash, "bytes": String::from_utf8_lossy(&t.bytes)})
                .to_string(),
        );
        add.push('\n');
        fresh.push(t.clone());
    }
    if !add.is_empty() {
        let new_file = !qpath.exists();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&qpath)?;
        if new_file {
            helm_platform::fs::restrict_to_owner(&qpath)?;
        }
        f.write_all(add.as_bytes())?;
    }
    Ok(fresh)
}

/// An open session log for appending.
pub struct SessionLog {
    path: PathBuf,
    last_id: Option<String>,
    ids: HashSet<String>,
    needs_newline: bool,
}

impl SessionLog {
    /// Open (creating it with `header` when missing) and read what is there.
    pub fn open(sessions: &Path, header: &Header) -> Result<(Self, LogRead), LogError> {
        helm_core::privfs::create_private_dir_all(sessions)?;
        let path = log_path(sessions, &header.chat_id);
        if !path.exists() {
            let mut line = serde_json::to_value(header).map_err(io::Error::other)?;
            line["type"] = json!("header");
            let mut f = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)?;
            helm_platform::fs::restrict_to_owner(&path)?;
            f.write_all(format!("{line}\n").as_bytes())?;
            f.sync_data()?;
        }
        let read = read(&path)?;
        let needs_newline = std::fs::read(&path)
            .map(|b| b.last().is_some_and(|&c| c != b'\n'))
            .unwrap_or(false);
        let ids = read
            .entries
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect();
        let last_id = read
            .entries
            .last()
            .and_then(|m| m.get("id").and_then(Value::as_str).map(str::to_owned));
        Ok((
            Self {
                path,
                last_id,
                ids,
                needs_newline,
            },
            read,
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn last_id(&self) -> Option<&str> {
        self.last_id.as_deref()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Append `entry` as the child of the last entry. An id that is
    /// already in the log is not written again; returns whether it was written.
    pub fn append(&mut self, entry: &mut Entry) -> io::Result<bool> {
        if self.ids.contains(entry.id()) {
            return Ok(false);
        }
        entry.set_parent(self.last_id.clone());
        let mut line = String::new();
        if self.needs_newline {
            line.push('\n');
        }
        line.push_str(&serde_json::to_string(entry).map_err(io::Error::other)?);
        line.push('\n');
        let mut f = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.flush()?;
        self.needs_newline = false;
        self.ids.insert(entry.id().to_owned());
        self.last_id = Some(entry.id().to_owned());
        Ok(true)
    }
}

/// Cut `s` to at most `max` bytes on a character boundary.
pub fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn tail_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

/// A message row as projection writes it.
#[derive(Clone, Debug)]
pub struct RowWrite {
    pub source_key: String,
    pub role: &'static str,
    pub text: String,
    pub thinking: String,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    pub status: &'static str,
    pub timestamp: i64,
    pub details: Value,
    pub entry_id: Option<String>,
    pub parent_id: Option<String>,
}

fn role_str(r: MessageRole) -> &'static str {
    match r {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::ToolResult => "toolResult",
        MessageRole::Context => "context",
    }
}

pub fn status_str(s: MessageStatus) -> &'static str {
    match s {
        MessageStatus::Saved => "saved",
        MessageStatus::Streaming => "streaming",
        MessageStatus::Complete => "complete",
        MessageStatus::Error => "error",
        MessageStatus::Interrupted => "interrupted",
    }
}

impl RowWrite {
    pub fn from_entry(e: &Entry) -> Self {
        match e {
            Entry::Message(m) => RowWrite {
                source_key: m.id.clone(),
                role: role_str(m.role),
                text: m.text.clone(),
                thinking: m.thinking.clone(),
                tool_name: None,
                tool_call_id: None,
                status: status_str(m.status),
                timestamp: m.timestamp,
                details: serde_json::to_value(&m.details).unwrap_or(json!({})),
                entry_id: Some(m.id.clone()),
                parent_id: m.parent_id.clone(),
            },
            Entry::Tool(t) => RowWrite {
                source_key: t.id.clone(),
                role: "toolResult",
                text: t.result.clone(),
                thinking: String::new(),
                tool_name: Some(t.tool_name.clone()),
                tool_call_id: Some(t.tool_call_id.clone()),
                status: if t.is_error { "error" } else { "complete" },
                timestamp: t.started_at,
                details: {
                    let args = cut(&t.args, 4096);
                    let mut d = json!({"toolArgs": args});
                    if args.len() < t.args.len() {
                        d["textTruncated"] = json!(true);
                    }
                    if t.is_error {
                        d["isError"] = json!(true);
                    }
                    d
                },
                entry_id: Some(t.id.clone()),
                parent_id: t.parent_id.clone(),
            },
            Entry::Context(c) => RowWrite {
                source_key: c.id.clone(),
                role: "context",
                text: c.text.clone(),
                thinking: String::new(),
                tool_name: None,
                tool_call_id: None,
                status: "complete",
                timestamp: c.timestamp,
                details: json!({"contextKind": c.kind}),
                entry_id: Some(c.id.clone()),
                parent_id: c.parent_id.clone(),
            },
        }
    }
}

/// Upsert one message row by `(chat, source_key)`. Text plus thinking over 64 KiB goes to a
/// content-addressed artifact (`artifacts/<sha256>.json`), and the row keeps 64 KiB of text,
/// 16 KiB of thinking and the thinking tail. Returns the row id.
pub fn upsert_row(
    tx: &Transaction<'_>,
    artifacts: &Path,
    chat_id: &str,
    row: &RowWrite,
    revision: i64,
) -> rusqlite::Result<i64> {
    let mut details = row.details.clone();
    if !details.is_object() {
        details = json!({});
    }
    let total = row.text.len() + row.thinking.len();
    let mut artifact = None;
    let (text, thinking) = if total > ROW_TEXT_BYTES {
        let raw = json!({"text": row.text, "thinking": row.thinking}).to_string();
        let name = format!("{}.json", helm_core::sha256_hex(raw.as_bytes()));
        let path = artifacts.join(&name);
        let written = path.exists()
            || match helm_core::privfs::create_private_dir_all(artifacts)
                .and_then(|_| helm_core::privfs::write_atomic(&path, raw.as_bytes()))
            {
                Ok(()) => true,
                Err(e) => {
                    log::error!("could not write artifact {name}: {e}");
                    false
                }
            };
        // Name the artifact only when the file really exists.
        artifact = written.then_some(name);
        let text = cut(&row.text, ROW_TEXT_BYTES).to_owned();
        let thinking = cut(&row.thinking, ROW_THINKING_BYTES).to_owned();
        if text.len() < row.text.len() {
            details["textTruncated"] = json!(true);
        }
        if thinking.len() < row.thinking.len() {
            details["thinkingTail"] = json!(tail_chars(&row.thinking, THINKING_TAIL_CHARS));
        }
        (text, thinking)
    } else {
        (row.text.clone(), row.thinking.clone())
    };
    let bytes = (text.len() + thinking.len()) as i64;
    tx.query_row(
        "INSERT INTO messages(chat_id, source_key, role, text, thinking, tool_name, tool_call_id,
                              status, timestamp, revision, entry_id, parent_id, bytes, artifact, details)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(chat_id, source_key) DO UPDATE SET
             role = excluded.role, text = excluded.text, thinking = excluded.thinking,
             tool_name = excluded.tool_name, tool_call_id = excluded.tool_call_id,
             status = excluded.status, revision = excluded.revision,
             entry_id = COALESCE(excluded.entry_id, messages.entry_id),
             parent_id = COALESCE(excluded.parent_id, messages.parent_id),
             bytes = excluded.bytes, artifact = excluded.artifact,
             details = json_patch(COALESCE(messages.details, '{}'), excluded.details)
         RETURNING id",
        rusqlite::params![
            chat_id,
            row.source_key,
            row.role,
            text,
            thinking,
            row.tool_name,
            row.tool_call_id,
            row.status,
            row.timestamp,
            revision,
            row.entry_id,
            row.parent_id,
            bytes,
            artifact,
            details.to_string(),
        ],
        |r| r.get(0),
    )
}

/// Project one log entry into `entries` and `messages`. Idempotent.
pub fn project(
    tx: &Transaction<'_>,
    artifacts: &Path,
    chat_id: &str,
    raw: &Map<String, Value>,
    revision: i64,
) -> rusqlite::Result<bool> {
    let Some(id) = raw.get("id").and_then(Value::as_str) else {
        return Ok(false);
    };
    let kind = raw.get("type").and_then(Value::as_str).unwrap_or("unknown");
    let parent = raw.get("parentId").and_then(Value::as_str);
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO entries(chat_id, entry_id, parent_id, kind, raw) VALUES (?1, ?2, ?3, ?4, ?5)",
        (chat_id, id, parent, kind, Value::Object(raw.clone()).to_string()),
    )? > 0;
    if let Ok(entry) = serde_json::from_value::<Entry>(Value::Object(raw.clone())) {
        upsert_row(
            tx,
            artifacts,
            chat_id,
            &RowWrite::from_entry(&entry),
            revision,
        )?;
    }
    Ok(inserted)
}

/// Startup repair: project every log entry the database does not have yet, and post one
/// inert notice row per newly quarantined torn fragment. Returns how many entries were projected.
pub fn reconcile(
    db: &Database,
    sessions: &Path,
    artifacts: &Path,
    chat_id: &str,
    revision: i64,
) -> Result<usize, LogError> {
    let path = log_path(sessions, chat_id);
    let read = read(&path)?;
    let fresh_torn = quarantine(&path, &read.torn)?;
    let n = db
        .write(|tx| {
            let mut n = 0;
            for raw in &read.entries {
                let id = raw.get("id").and_then(Value::as_str).unwrap_or_default();
                let known: Option<i64> = tx
                    .query_row(
                        "SELECT 1 FROM entries WHERE chat_id = ?1 AND entry_id = ?2",
                        (chat_id, id),
                        |r| r.get(0),
                    )
                    .optional()?;
                if known.is_none() && project(tx, artifacts, chat_id, raw, revision)? {
                    n += 1;
                }
            }
            for t in &fresh_torn {
                let row = RowWrite {
                    source_key: format!("torn:{}", t.offset),
                    role: "context",
                    text: "Part of this chat's history file was damaged and has been set aside; the rest loaded normally.".into(),
                    thinking: String::new(),
                    tool_name: None,
                    tool_call_id: None,
                    status: "complete",
                    timestamp: helm_core::now_ms(),
                    details: json!({"contextKind": "notice"}),
                    entry_id: None,
                    parent_id: None,
                };
                upsert_row(tx, artifacts, chat_id, &row, revision)?;
            }
            Ok::<_, rusqlite::Error>(n)
        })
        .map_err(|e| LogError::Io(io::Error::other(e)))?;
    Ok(n)
}

/// `sessions/<chat>.claude.json`: what the host needs to resume the native session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeBinding {
    pub native_session_id: Option<String>,
    pub cwd: String,
    pub last_leaf_id: Option<String>,
    pub harness_switch_id: Option<String>,
    pub updated_at: i64,
    /// A whole-chat fork that has not run yet: the source's native session, resumed once with
    /// `--fork-session` ([`crate::fork`]). Cleared when the fork saves its own session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_of: Option<String>,
}

pub const BINDING_MAX_BYTES: usize = 16 * 1024;

pub fn binding_path(sessions: &Path, chat_id: &str) -> PathBuf {
    sessions.join(format!("{chat_id}.claude.json"))
}

impl ClaudeBinding {
    pub fn load(sessions: &Path, chat_id: &str) -> Option<Self> {
        let bytes = helm_core::privfs::read_capped(
            &binding_path(sessions, chat_id),
            BINDING_MAX_BYTES as u64,
        )
        .ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, sessions: &Path, chat_id: &str) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > BINDING_MAX_BYTES {
            return Err(io::Error::other("binding larger than 16 KiB"));
        }
        helm_core::privfs::create_private_dir_all(sessions)?;
        helm_core::privfs::write_atomic(&binding_path(sessions, chat_id), &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;

    fn header(chat: &str) -> Header {
        Header {
            version: FORMAT_VERSION.into(),
            chat_id: chat.into(),
            harness: "claude".into(),
            cwd: "C:/w".into(),
            created_at: 1,
        }
    }

    fn msg(id: &str, text: &str) -> Entry {
        Entry::Message(MessageEntry {
            id: id.into(),
            parent_id: None,
            timestamp: 10,
            role: MessageRole::Assistant,
            text: text.into(),
            thinking: String::new(),
            status: MessageStatus::Complete,
            details: MessageDetails::default(),
        })
    }

    fn seed_chat(db: &Database, chat: &str) {
        db.write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO projects(id, name, created_at) VALUES ('p', 'P', 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 0, 0)",
                [chat],
            )
        })
        .unwrap();
    }

    fn count(db: &Database, chat: &str) -> i64 {
        db.snapshot(|tx| {
            tx.query_row(
                "SELECT count(*) FROM messages WHERE chat_id = ?1",
                [chat],
                |r| r.get(0),
            )
        })
        .unwrap()
    }

    #[test]
    fn appends_a_linear_chain_and_skips_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let (mut log, read0) = SessionLog::open(dir.path(), &header("c")).unwrap();
        assert!(read0.entries.is_empty());
        assert!(log.append(&mut msg("m:1", "one")).unwrap());
        assert!(log.append(&mut msg("m:2", "two")).unwrap());
        assert!(!log.append(&mut msg("m:2", "again")).unwrap());
        let r = read(log.path()).unwrap();
        assert_eq!(r.header.unwrap().version, FORMAT_VERSION);
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[1]["parentId"], "m:1");
        assert_eq!(r.entries[0]["parentId"], Value::Null);
    }

    /// The writer dies between the log append and the projection; reconcile repairs it, and
    /// running it again adds nothing.
    #[test]
    fn a_crash_between_append_and_projection_is_repaired_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let (_d, db) = temp_db();
        seed_chat(&db, "c");
        let sessions = dir.path().join("sessions");
        let artifacts = dir.path().join("artifacts");
        let (mut log, _) = SessionLog::open(&sessions, &header("c")).unwrap();
        let mut e1 = msg("m:1", "projected");
        log.append(&mut e1).unwrap();
        let raw1 = serde_json::to_value(&e1).unwrap();
        db.write(|tx| project(tx, &artifacts, "c", raw1.as_object().unwrap(), 1))
            .unwrap();
        // Appended, then "killed" before projecting.
        log.append(&mut msg("m:2", "lost in the crash")).unwrap();
        assert_eq!(count(&db, "c"), 1);
        assert_eq!(reconcile(&db, &sessions, &artifacts, "c", 2).unwrap(), 1);
        assert_eq!(count(&db, "c"), 2);
        assert_eq!(reconcile(&db, &sessions, &artifacts, "c", 3).unwrap(), 0);
        assert_eq!(count(&db, "c"), 2, "no duplicate rows");
        // A re-run leaves projected rows alone (it does not rewrite them from the log).
        let revs: Vec<i64> = db
            .snapshot(|tx| {
                let mut st = tx.prepare("SELECT revision FROM messages ORDER BY id")?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<i64>>>()
            })
            .unwrap();
        assert_eq!(revs, [1, 2]);
    }

    #[test]
    fn torn_lines_are_passed_over_quarantined_once_and_noticed_once() {
        let dir = tempfile::tempdir().unwrap();
        let (_d, db) = temp_db();
        seed_chat(&db, "c");
        let sessions = dir.path().join("sessions");
        let artifacts = dir.path().join("artifacts");
        let (mut log, _) = SessionLog::open(&sessions, &header("c")).unwrap();
        log.append(&mut msg("m:1", "one")).unwrap();
        // A writer killed mid-append, then a later whole record on the same line.
        let path = log.path().to_owned();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(br#"{"type":"message","id":"m:half","te"#)
            .unwrap();
        f.write_all(
            br#"{"type":"message","id":"m:2","parentId":"m:1","timestamp":1,"role":"assistant","text":"two","status":"complete"}"#,
        )
        .unwrap();
        f.write_all(b"\n").unwrap();
        drop(f);
        let r = read(&path).unwrap();
        assert_eq!(
            r.entries.len(),
            2,
            "the record after the damage is recovered"
        );
        assert_eq!(r.torn.len(), 1);
        reconcile(&db, &sessions, &artifacts, "c", 1).unwrap();
        reconcile(&db, &sessions, &artifacts, "c", 2).unwrap();
        let q = std::fs::read_to_string(torn_path(&path)).unwrap();
        assert_eq!(q.lines().count(), 1, "quarantined once");
        let notices: i64 = db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT count(*) FROM messages WHERE source_key LIKE 'torn:%'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(notices, 1);
        assert_eq!(count(&db, "c"), 3);
    }

    #[test]
    fn a_trailing_partial_line_is_left_and_the_next_append_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let (mut log, _) = SessionLog::open(dir.path(), &header("c")).unwrap();
        log.append(&mut msg("m:1", "one")).unwrap();
        let path = log.path().to_owned();
        drop(log);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(br#"{"type":"message","id":"m:cut"#).unwrap();
        drop(f);
        assert_eq!(read(&path).unwrap().entries.len(), 1);
        let (mut log, _) = SessionLog::open(dir.path(), &header("c")).unwrap();
        log.append(&mut msg("m:2", "two")).unwrap();
        let r = read(&path).unwrap();
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.torn.len(), 1, "the fragment is now a mid-file torn line");
    }

    #[test]
    fn more_than_eight_torn_lines_fail_and_structure_damage_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (mut log, _) = SessionLog::open(dir.path(), &header("c")).unwrap();
        log.append(&mut msg("m:1", "one")).unwrap();
        let path = log.path().to_owned();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        for _ in 0..9 {
            f.write_all(b"garbage\n").unwrap();
        }
        drop(f);
        assert!(matches!(read(&path), Err(LogError::TooManyTorn)));

        let p2 = dir.path().join("dup.jsonl");
        std::fs::write(
            &p2,
            "{\"type\":\"header\",\"version\":\"helm-session 1\",\"chatId\":\"d\",\"harness\":\"claude\",\"cwd\":\"\",\"createdAt\":0}\n\
             {\"type\":\"message\",\"id\":\"a\"}\n{\"type\":\"message\",\"id\":\"a\"}\n",
        )
        .unwrap();
        assert!(matches!(read(&p2), Err(LogError::DuplicateId(_))));
        std::fs::write(
            &p2,
            "{\"type\":\"header\",\"version\":\"helm-session 1\",\"chatId\":\"d\",\"harness\":\"claude\",\"cwd\":\"\",\"createdAt\":0}\n\
             {\"type\":\"message\",\"id\":\"a\",\"parentId\":\"nope\"}\n",
        )
        .unwrap();
        assert!(matches!(read(&p2), Err(LogError::UnknownParent(_))));
    }

    #[test]
    fn a_lone_surrogate_is_replaced_not_rejected() {
        let s = r#"{"id":"x","text":"half \ud83d emoji \ud83d\ude00 ok"}"#;
        let m = parse_object(s.as_bytes()).unwrap();
        assert_eq!(m["text"], "half \u{fffd} emoji 😀 ok");
    }

    /// Over 64 KiB goes to an artifact; the row keeps 64 KiB of text, 16 KiB of thinking and
    /// a thinking tail.
    #[test]
    fn a_large_message_goes_to_an_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let (_d, db) = temp_db();
        seed_chat(&db, "c");
        let artifacts = dir.path().join("artifacts");
        let row = RowWrite {
            source_key: "m:big".into(),
            role: "assistant",
            text: "é".repeat(40 * 1024),
            thinking: format!("{}THE-END", "t".repeat(20 * 1024)),
            tool_name: None,
            tool_call_id: None,
            status: "complete",
            timestamp: 1,
            details: json!({}),
            entry_id: None,
            parent_id: None,
        };
        db.write(|tx| upsert_row(tx, &artifacts, "c", &row, 1))
            .unwrap();
        let (tlen, thlen, artifact, details): (i64, i64, String, String) = db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT length(CAST(text AS BLOB)), length(thinking), artifact, details FROM messages",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
            })
            .unwrap();
        assert!(tlen <= ROW_TEXT_BYTES as i64 && tlen > ROW_TEXT_BYTES as i64 - 4);
        assert_eq!(thlen, ROW_THINKING_BYTES as i64);
        assert!(artifacts.join(&artifact).is_file());
        let d: Value = serde_json::from_str(&details).unwrap();
        assert!(d["thinkingTail"].as_str().unwrap().ends_with("THE-END"));
        assert_eq!(d["textTruncated"], true);
    }

    #[test]
    fn the_binding_round_trips_and_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let b = ClaudeBinding {
            native_session_id: Some("s-1".into()),
            cwd: "C:/w".into(),
            last_leaf_id: Some("m:9".into()),
            harness_switch_id: None,
            updated_at: 5,
            fork_of: None,
        };
        b.save(dir.path(), "c").unwrap();
        assert_eq!(ClaudeBinding::load(dir.path(), "c"), Some(b));
        let huge = ClaudeBinding {
            cwd: "x".repeat(20_000),
            ..Default::default()
        };
        assert!(huge.save(dir.path(), "d").is_err());
    }
}
