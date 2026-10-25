//! The agent's own attention flag: a chat's root agent files one question for the user, which
//! lists the chat under "needs you" with the question as its reason.
//!
//! Rules:
//! - One open flag per chat: filing again replaces it (the same text again is a duplicate and
//!   changes nothing).
//! - At most [`PER_HOUR`] filings per chat per hour, counted by this app run.
//! - The flag ends when the user next sends a message in the chat, or marks the chat addressed.
//!   Archiving or deleting the chat takes it with it.

use rusqlite::{params, OptionalExtension, Transaction};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PER_HOUR: usize = 6;
const WINDOW: Duration = Duration::from_secs(3600);

pub(crate) fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_flags (
             chat_id     TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             question    TEXT NOT NULL,
             reason      TEXT NOT NULL,
             priority    TEXT NOT NULL DEFAULT 'normal',
             created_at  INTEGER NOT NULL
         );",
    )
}

/// What filing did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filed {
    Filed,
    Replaced,
    Duplicate,
}

impl Filed {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Filed => "filed",
            Self::Replaced => "replaced",
            Self::Duplicate => "duplicate",
        }
    }
}

/// File (or replace) the chat's flag. `Ok(None)` when the chat does not exist.
pub fn file(
    tx: &Transaction<'_>,
    chat_id: &str,
    question: &str,
    reason: &str,
    priority: &str,
    now: i64,
) -> rusqlite::Result<Option<Filed>> {
    let exists: bool = tx
        .query_row("SELECT 1 FROM chats WHERE id = ?1", [chat_id], |_| Ok(true))
        .optional()?
        .unwrap_or(false);
    if !exists {
        return Ok(None);
    }
    let old: Option<(String, String, String)> = tx
        .query_row(
            "SELECT question, reason, priority FROM chat_flags WHERE chat_id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if old
        .as_ref()
        .is_some_and(|(q, r, p)| q == question && r == reason && p == priority)
    {
        return Ok(Some(Filed::Duplicate));
    }
    tx.execute(
        "INSERT INTO chat_flags(chat_id, question, reason, priority, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(chat_id) DO UPDATE SET question = ?2, reason = ?3, priority = ?4, created_at = ?5",
        params![chat_id, question, reason, priority, now],
    )?;
    Ok(Some(if old.is_some() {
        Filed::Replaced
    } else {
        Filed::Filed
    }))
}

/// End the chat's flag (the user answered, or marked it addressed).
pub fn clear(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM chat_flags WHERE chat_id = ?1", [chat_id])?;
    Ok(())
}

/// The sliding hourly allowance per chat.
#[derive(Default)]
pub struct FlagLimits {
    seen: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl FlagLimits {
    /// Say whether another filing is allowed now, without counting it.
    pub fn check(&self, chat_id: &str, now: Instant) -> Result<(), String> {
        let mut m = self.seen.lock().unwrap();
        let q = m.entry(chat_id.to_owned()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
            q.pop_front();
        }
        if q.len() >= PER_HOUR {
            return Err(format!(
                "You have already flagged this chat {PER_HOUR} times in the last hour. Put the question in your reply instead."
            ));
        }
        Ok(())
    }

    /// Count one filing.
    pub fn record(&self, chat_id: &str, now: Instant) {
        let mut m = self.seen.lock().unwrap();
        m.entry(chat_id.to_owned()).or_default().push_back(now);
    }
}
