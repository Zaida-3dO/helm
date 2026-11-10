//! Subscriptions: what each watching device has asked to hear about, and the news owed to it.
//!
//! A watcher holds one list subscription (`chats`) and up to [`MAX_CHAT_SUBS`] chat
//! subscriptions (`chat:<id>`; the oldest is dropped for a new one). Each has its own `seq`,
//! counting from 1, so the watcher can tell when a push went missing and subscribe again with the
//! revision it holds.
//!
//! Message rows for one chat are held for [`COALESCE`] and sent together, the latest row per id.
//! A subscription is *held* from the moment it is made until its result has gone out
//! ([`Subs::activate`]): news that arrives in between waits, so the watcher never sees a push for
//! a subscription whose result it has not read yet.

use crate::wire::{Push, MAX_ROW_BYTES, TRIMMED_TEXT_BYTES};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;

/// How long rows of one chat gather before they are sent.
pub const COALESCE: Duration = Duration::from_millis(250);
/// The most chat subscriptions one watcher holds.
pub const MAX_CHAT_SUBS: usize = 16;
/// The most news held for a subscription whose result has not gone out.
const MAX_HELD: usize = 1000;
/// The most row bytes in one push.
pub const PUSH_ROW_BUDGET: usize = 900 * 1024;

pub const CHATS_SUB: &str = "chats";

pub fn chat_sub_id(chat_id: &str) -> String {
    format!("chat:{chat_id}")
}

/// One push to send, to the device `to`.
#[derive(Clone, Debug, PartialEq)]
pub struct Out {
    pub to: String,
    pub push: Push,
}

#[derive(Default)]
struct Sub {
    seq: u64,
    active: bool,
    held: Vec<Value>,
    /// News was dropped from `held`: the watcher is told to read the chat again.
    dropped: bool,
    rows: BTreeMap<i64, Value>,
    revision: i64,
    flush_at: Option<Instant>,
}

impl Sub {
    fn new() -> Sub {
        Sub::default()
    }

    /// Number the event and hand it out, or hold it while the subscription's result is unsent.
    fn emit(&mut self, to: &str, sub: &str, event: Value) -> Option<Out> {
        if !self.active {
            self.held.push(event);
            if self.held.len() > MAX_HELD {
                self.held.remove(0);
                self.dropped = true;
            }
            return None;
        }
        self.seq += 1;
        Some(Out {
            to: to.to_owned(),
            push: Push {
                to: Some(to.to_owned()),
                from: None,
                sub: sub.to_owned(),
                seq: self.seq,
                event,
            },
        })
    }
}

#[derive(Default)]
struct Watcher {
    chats: Option<Sub>,
    /// Oldest first.
    chat: Vec<(String, Sub)>,
    /// For a device of another person in the household: that person's hub id. Their list hears
    /// only their own chats and chats shared with them.
    scope: Option<String>,
    /// The chats a scoped watcher's list holds, so a chat that stops being theirs is taken out.
    listed: HashSet<String>,
}

/// Whether a chat (as other devices see it) belongs to the person or is shared with them.
pub fn visible_to(chat: &Value, person_id: &str) -> bool {
    owned_by(chat, person_id)
        || chat
            .get("sharedWith")
            .and_then(Value::as_array)
            .is_some_and(|l| l.iter().any(|p| p.as_str() == Some(person_id)))
}

/// Whether a chat (as other devices see it) belongs to the person.
pub fn owned_by(chat: &Value, person_id: &str) -> bool {
    chat.get("personId").and_then(Value::as_str) == Some(person_id)
}

#[derive(Default)]
pub struct Subs {
    watchers: HashMap<String, Watcher>,
}

impl Subs {
    pub fn add_chats(&mut self, uid: &str) {
        self.watchers.entry(uid.to_owned()).or_default().chats = Some(Sub::new());
    }

    /// Who a watcher's list is for: `Some(person)` for a device of another person in the
    /// household, `None` for one of this PC's own person.
    pub fn set_scope(&mut self, uid: &str, scope: Option<&str>) {
        let w = self.watchers.entry(uid.to_owned()).or_default();
        if w.scope.as_deref() != scope {
            w.scope = scope.map(str::to_owned);
            w.listed.clear();
        }
    }

    /// The chats a scoped watcher's list was answered with.
    pub fn listed(&mut self, uid: &str, ids: impl IntoIterator<Item = String>) {
        if let Some(w) = self.watchers.get_mut(uid) {
            w.listed.extend(ids);
        }
    }

    /// Subscribe to a chat (afresh if it already was). Returns the chat whose subscription was
    /// dropped to make room, if any.
    pub fn add_chat(&mut self, uid: &str, chat_id: &str) -> Option<String> {
        let w = self.watchers.entry(uid.to_owned()).or_default();
        w.chat.retain(|(c, _)| c != chat_id);
        w.chat.push((chat_id.to_owned(), Sub::new()));
        (w.chat.len() > MAX_CHAT_SUBS).then(|| w.chat.remove(0).0)
    }

    pub fn remove_chats(&mut self, uid: &str) {
        if let Some(w) = self.watchers.get_mut(uid) {
            w.chats = None;
        }
    }

    pub fn remove_chat(&mut self, uid: &str, chat_id: &str) {
        if let Some(w) = self.watchers.get_mut(uid) {
            w.chat.retain(|(c, _)| c != chat_id);
        }
    }

    pub fn drop_watcher(&mut self, uid: &str) {
        self.watchers.remove(uid);
    }

    /// Keep only the watchers `online` says are up.
    pub fn retain_online(&mut self, online: impl Fn(&str) -> bool) {
        self.watchers.retain(|uid, _| online(uid));
    }

    pub fn watchers(&self) -> Vec<String> {
        self.watchers.keys().cloned().collect()
    }

    pub fn watching_chats(&self) -> bool {
        self.watchers.values().any(|w| w.chats.is_some())
    }

    pub fn watching_chat(&self, chat_id: &str) -> bool {
        self.watchers
            .values()
            .any(|w| w.chat.iter().any(|(c, _)| c == chat_id))
    }

    /// A chat changed: every list subscription hears the whole record. A scoped watcher hears
    /// only chats that are its person's or shared with them, and hears a chat that stopped being
    /// so as removed (its chat subscription ends too).
    pub fn chat_changed(&mut self, chat: Value) -> Vec<Out> {
        let id = chat
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let event = json!({ "kind": "chat", "chat": chat });
        let removed = json!({ "kind": "chat_removed", "chatId": id });
        let mut out = Vec::new();
        for (uid, w) in &mut self.watchers {
            let event = match &w.scope {
                None => &event,
                Some(p) if visible_to(&chat, p) => {
                    w.listed.insert(id.clone());
                    &event
                }
                Some(_) => {
                    w.chat.retain(|(c, _)| *c != id);
                    if !w.listed.remove(&id) {
                        continue;
                    }
                    &removed
                }
            };
            if let Some(s) = &mut w.chats {
                out.extend(s.emit(uid, CHATS_SUB, event.clone()));
            }
        }
        out
    }

    /// A chat is gone: list subscriptions hear it (a scoped one only if it held the chat), and
    /// its chat subscriptions end.
    pub fn chat_removed(&mut self, chat_id: &str) -> Vec<Out> {
        let event = json!({ "kind": "chat_removed", "chatId": chat_id });
        let mut out = Vec::new();
        for (uid, w) in &mut self.watchers {
            w.chat.retain(|(c, _)| c != chat_id);
            if w.scope.is_some() && !w.listed.remove(chat_id) {
                continue;
            }
            if let Some(s) = &mut w.chats {
                out.extend(s.emit(uid, CHATS_SUB, event.clone()));
            }
        }
        out
    }

    /// News for chat subscriptions, sent as it is: `thread`, `pending` and `refresh`.
    pub fn chat_event(&mut self, chat_id: &str, event: Value) -> Vec<Out> {
        let sub = chat_sub_id(chat_id);
        let mut out = Vec::new();
        for (uid, w) in &mut self.watchers {
            for (c, s) in &mut w.chat {
                if c == chat_id {
                    out.extend(s.emit(uid, &sub, event.clone()));
                }
            }
        }
        out
    }

    /// Rows of a chat to send in the next batch: the latest row per id wins.
    pub fn add_rows(&mut self, chat_id: &str, revision: i64, rows: &[Value], now: Instant) {
        for w in self.watchers.values_mut() {
            for (c, s) in &mut w.chat {
                if c != chat_id {
                    continue;
                }
                for r in rows {
                    if let Some(id) = r.get("id").and_then(Value::as_i64) {
                        s.rows.insert(id, r.clone());
                    }
                }
                s.revision = s.revision.max(revision);
                s.flush_at.get_or_insert(now + COALESCE);
            }
        }
    }

    /// When the next batch of rows is due.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.watchers
            .values()
            .flat_map(|w| w.chat.iter().filter_map(|(_, s)| s.flush_at))
            .min()
    }

    /// Send every batch that is due.
    pub fn flush_due(&mut self, now: Instant) -> Vec<Out> {
        let mut out = Vec::new();
        for (uid, w) in &mut self.watchers {
            for (chat_id, s) in &mut w.chat {
                if s.flush_at.is_none_or(|t| t > now) {
                    continue;
                }
                s.flush_at = None;
                let mut rows: Vec<Value> = std::mem::take(&mut s.rows).into_values().collect();
                rows.sort_by_key(|r| {
                    (
                        r.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
                        r.get("id").and_then(Value::as_i64).unwrap_or(0),
                    )
                });
                let sub = chat_sub_id(chat_id);
                for chunk in chunks(rows, PUSH_ROW_BUDGET) {
                    let event = json!({
                        "kind": "message",
                        "chatId": chat_id,
                        "revision": s.revision,
                        "rows": chunk,
                    });
                    out.extend(s.emit(uid, &sub, event));
                }
            }
        }
        out
    }

    /// The result of a subscription has gone out: send what was held, in order. A chat
    /// subscription that lost news is told to read the chat again first.
    pub fn activate(&mut self, uid: &str, sub: &str) -> Vec<Out> {
        let Some(w) = self.watchers.get_mut(uid) else {
            return Vec::new();
        };
        let state = if sub == CHATS_SUB {
            w.chats.as_mut()
        } else {
            sub.strip_prefix("chat:")
                .and_then(|id| w.chat.iter_mut().find(|(c, _)| c == id))
                .map(|(_, s)| s)
        };
        let Some(s) = state else {
            return Vec::new();
        };
        s.active = true;
        let mut events = std::mem::take(&mut s.held);
        if std::mem::take(&mut s.dropped) && sub != CHATS_SUB {
            let id = sub.trim_start_matches("chat:");
            events = vec![json!({ "kind": "refresh", "chatId": id })];
        }
        events
            .into_iter()
            .filter_map(|e| s.emit(uid, sub, e))
            .collect()
    }
}

/// Split rows into groups that each weigh at most `budget` bytes (a row heavier than that goes
/// alone).
fn chunks(rows: Vec<Value>, budget: usize) -> Vec<Vec<Value>> {
    let mut out: Vec<Vec<Value>> = Vec::new();
    let mut weight = 0;
    for r in rows {
        let w = r.to_string().len();
        match out.last_mut() {
            Some(last) if weight + w <= budget => {
                last.push(r);
                weight += w;
            }
            _ => {
                out.push(vec![r]);
                weight = w;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Trimming
// ---------------------------------------------------------------------------------------------

fn size(v: &Value) -> usize {
    v.to_string().len()
}

/// Cut a string to at most `max` bytes on a character boundary, marking the cut.
fn cut_str(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    s.truncate(i);
    s.push('…');
}

fn cut_field(v: &mut Value, key: &str, max: usize) {
    if let Some(Value::String(s)) = v.get_mut(key) {
        cut_str(s, max);
    }
}

fn longest_string(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Array(a) => a.iter().map(longest_string).max().unwrap_or(0),
        Value::Object(m) => m.values().map(longest_string).max().unwrap_or(0),
        _ => 0,
    }
}

/// Halve the first string of exactly `len` bytes; true if one was found.
fn halve_string(v: &mut Value, len: usize) -> bool {
    match v {
        Value::String(s) if s.len() == len => {
            cut_str(s, len / 2);
            true
        }
        Value::Array(a) => a.iter_mut().any(|x| halve_string(x, len)),
        Value::Object(m) => m.values_mut().any(|x| halve_string(x, len)),
        _ => false,
    }
}

/// Cut the biggest strings of a row until it weighs at most `limit` bytes (or nothing long is
/// left). Marks the row `remoteTrimmed`.
fn shrink_to(row: &mut Value, limit: usize) {
    let mut guard = 0;
    while size(row) > limit && guard < 64 {
        guard += 1;
        let longest = longest_string(row);
        if longest <= 256 || !halve_string(row, longest) {
            break;
        }
    }
    if let Some(d) = row.get_mut("details") {
        if let Some(m) = d.as_object_mut() {
            m.insert("remoteTrimmed".into(), json!(true));
        }
    } else if let Some(m) = row.as_object_mut() {
        m.insert("details".into(), json!({ "remoteTrimmed": true }));
    }
}

/// A row over [`MAX_ROW_BYTES`] goes out with its tool output (its text) cut to 64 KiB and its
/// other long parts cut to fit; its `details` gain `remoteTrimmed: true`. Returns whether it
/// was cut.
pub fn trim_row(row: &mut Value) -> bool {
    if size(row) <= MAX_ROW_BYTES {
        return false;
    }
    cut_field(row, "text", TRIMMED_TEXT_BYTES);
    cut_field(row, "thinking", TRIMMED_TEXT_BYTES / 4);
    cut_field(row, "thinkingTail", TRIMMED_TEXT_BYTES / 16);
    if let Some(d) = row.get_mut("details") {
        cut_field(d, "toolArgs", 8 * 1024);
        if let Some(Value::Array(tools)) = d.get_mut("tools") {
            for t in tools {
                cut_field(t, "args", 8 * 1024);
            }
        }
    }
    shrink_to(row, MAX_ROW_BYTES);
    true
}

/// Make rows fit one frame: each is trimmed, and when they still weigh more than `budget`
/// together every row is cut to its share.
pub fn fit_rows(rows: &mut [Value], budget: usize) {
    for r in rows.iter_mut() {
        trim_row(r);
    }
    let total: usize = rows.iter().map(size).sum();
    if total <= budget || rows.is_empty() {
        return;
    }
    let share = (budget / rows.len()).max(2048);
    for r in rows.iter_mut() {
        if size(r) > share {
            shrink_to(r, share);
        }
    }
}
