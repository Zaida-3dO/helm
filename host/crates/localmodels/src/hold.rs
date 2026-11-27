//! The graphics-card hold: a note, set by a chat, that something is using the card and sending to a
//! chat on a local model would reload the model server and spoil it (a benchmark run, say).
//!
//! The person is warned, not stopped: while a hold is set the composer of every local-model chat
//! says why and asks once before the first send. Nothing else wakes the card: a message from a
//! schedule, a wait or another chat to a local-model chat is saved but not handed over until the
//! hold ends, and a new local-model chat waits for its first message the same way. The lock in
//! [`crate::lock`] is the one that refuses loads outright. A hold has an owner: only the chat that
//! set it (or the person, from the app) clears it, and a hold whose owner ended or was archived is
//! dropped. It ends by itself too: two hours by default, eight at most. The hold lives in one row
//! of its own table, so it survives a restart of Helm; its end time still counts after one.

use crate::service::LocalService;
use crate::types::LockView;
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// How long a hold lasts when the caller names no time, and the longest one it may ask for.
pub const DEFAULT_HOURS: f64 = 2.0;
pub const MAX_HOURS: f64 = 8.0;

/// A reason is one line of at most this many characters.
const MAX_REASON: usize = 160;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub reason: String,
    /// When it ends by itself, in ms since the epoch.
    pub until: i64,
    /// The chat that set the hold; `None` for one the person set from the app. Only the owner
    /// (or the person) clears it, and a hold whose owner ended or was archived is dropped.
    pub owner: Option<String>,
}

/// The arguments of `local_hold_set`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HoldArgs {
    pub reason: String,
    #[serde(default)]
    pub hours: Option<f64>,
    /// The chat making the call, passed by the tool layer; never typed by an agent. `None` means
    /// the person set it from the app.
    #[serde(default)]
    pub owner_chat_id: Option<String>,
}

/// The arguments of `local_hold_clear`.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClearArgs {
    /// The chat making the call, passed by the tool layer; `None` means the person cleared it.
    #[serde(default)]
    pub by_chat_id: Option<String>,
}

/// The reason as one plain line, or why it cannot be used.
pub fn clean_reason(raw: &str) -> Result<String, String> {
    let one: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_REASON)
        .collect();
    let one = one.trim().trim_end_matches(['.', ':']).trim().to_owned();
    if one.is_empty() {
        Err("A hold needs a reason: one short line the composers show, such as \"Benchmark running\".".into())
    } else {
        Ok(one)
    }
}

/// When a hold set at `now` ends: the default when no time is named, never more than the longest.
pub fn expiry(hours: Option<f64>, now: i64) -> Result<i64, String> {
    let h = hours.unwrap_or(DEFAULT_HOURS);
    if !h.is_finite() || h <= 0.0 {
        return Err("hours must be a number above 0.".into());
    }
    Ok(now + (h.min(MAX_HOURS) * 3_600_000.0) as i64)
}

/// The one hold there can be, as the service keeps it in memory.
#[derive(Default)]
pub struct HoldCell(Mutex<Option<Hold>>);

impl HoldCell {
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Hold>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, hold: Hold) {
        *self.slot() = Some(hold);
    }

    /// Whether there was a hold to clear.
    pub fn clear(&self) -> bool {
        self.slot().take().is_some()
    }

    /// The stored hold even if its time has run out: whether there is anything at all to tidy.
    pub fn peek(&self) -> Option<Hold> {
        self.slot().clone()
    }

    /// The hold in force at `now`. A pure read: it never drops anything, not even a hold that has
    /// run out. Only the watch thread's tidy (`hold_tick`) removes one — and hands over what
    /// waited for the card; if a plain read cleared an expired hold from memory first, the tidy
    /// would find nothing to remove and the parked sends would wait forever.
    pub fn current(&self, now: i64) -> Option<Hold> {
        self.slot().clone().filter(|h| h.until > now)
    }
}

// ---- the one row that keeps a hold across a restart -----------------------------------------

/// The stored form: `{reason, until, ownerChatId?}`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    reason: String,
    until: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_chat_id: Option<String>,
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_model_hold (
             id     INTEGER PRIMARY KEY CHECK (id = 1),
             value  TEXT NOT NULL
         );",
    )
}

/// The stored hold, if one is waiting out its time. A row that cannot be read counts as no hold:
/// with a hold whose meaning is unknown, the card should not stay reserved for ever.
pub fn load(tx: &Transaction<'_>) -> rusqlite::Result<Option<Hold>> {
    let raw: Option<String> = tx
        .query_row("SELECT value FROM local_model_hold WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(raw) = raw else { return Ok(None) };
    Ok(serde_json::from_str::<Stored>(&raw).ok().map(|s| Hold {
        reason: s.reason,
        until: s.until,
        owner: s.owner_chat_id,
    }))
}

pub fn store(tx: &Transaction<'_>, hold: Option<&Hold>) -> rusqlite::Result<()> {
    let Some(h) = hold else {
        tx.execute("DELETE FROM local_model_hold WHERE id = 1", [])?;
        return Ok(());
    };
    let v = serde_json::to_string(&Stored {
        reason: h.reason.clone(),
        until: h.until,
        owner_chat_id: h.owner.clone(),
    })
    .expect("the hold serialises");
    tx.execute(
        "INSERT INTO local_model_hold (id, value) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET value = excluded.value",
        [v],
    )?;
    Ok(())
}

impl LocalService {
    /// Set the hold for `owner` (`None`: the person set it from the app), replacing any earlier
    /// one, and store it so it outlives a restart. Answers the lock view with the hold in it.
    pub fn set_hold(
        &self,
        reason: &str,
        hours: Option<f64>,
        owner: Option<String>,
    ) -> Result<LockView, String> {
        let now = helm_core::now_ms();
        // A live hold belongs to whoever set it. Another chat cannot take the card over: its
        // later release would silently end the first holder's work (chat B holds, releases, and
        // chat A's hold is gone). The owner may extend its own; a hold the person set in the app
        // outranks chats both ways.
        if let Some(live) = self.hold.current(now) {
            let allowed = match (live.owner.as_deref(), owner.as_deref()) {
                (None, Some(_)) => false,
                (Some(o), Some(b)) => o == b,
                _ => true,
            };
            if !allowed {
                let who = if live.owner.is_some() {
                    "another chat"
                } else {
                    "the user from the app"
                };
                return Err(format!(
                    "a GPU hold is already on: \"{}\", set by {who}; only that holder, or the \
                     user from the app, can end it — it ends by itself when its time comes",
                    live.reason
                ));
            }
        }
        let hold = Hold {
            reason: clean_reason(reason)?,
            until: expiry(hours, now)?,
            owner,
        };
        self.hold.set(hold.clone());
        if let Err(e) = self.db().write(|tx| store(tx, Some(&hold))) {
            log::warn!("the GPU hold could not be stored: {e}");
        }
        self.emit_lock();
        Ok(self.lock_view())
    }

    /// Clear the hold; clearing none is not an error. `by` is the chat asked (from the tool
    /// layer); `None` means the person did it from the app and any hold may be cleared. A hold
    /// with an owner belongs to that chat alone: another chat cannot end its work.
    pub fn clear_hold(&self, by: Option<&str>) -> Result<LockView, String> {
        // Ownership is checked with `peek`, not `current`: asking must never drop an ended hold
        // itself — the watch thread's tidy does that once, together with handing over what waited.
        if let Some(h) = self.hold.peek() {
            match (h.owner.as_deref(), by) {
                (Some(o), Some(b)) if o != b => {
                    return Err(
                        "that hold was set by another chat: only that chat, or the user from \
                         the app, can clear it; it ends by itself when its time comes."
                            .into(),
                    );
                }
                // A hold the person set in the app is theirs to end: no chat may release it.
                (None, Some(_)) => {
                    return Err(
                        "that hold was set from the app: only the user can clear it there; \
                         it ends by itself when its time comes."
                            .into(),
                    );
                }
                _ => {}
            }
        }
        if self.hold.clear() {
            if let Err(e) = self.db().write(|tx| store(tx, None)) {
                log::warn!("the GPU hold could not be cleared in storage: {e}");
            }
            self.emit_lock();
            // Whatever waited for the card can go now.
            self.dispatch_held();
        }
        Ok(self.lock_view())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_one_plain_line_and_times_are_bounded() {
        let ok = |s: &str| clean_reason(s).unwrap();
        assert_eq!(ok("  Benchmark\n running. "), "Benchmark running");
        assert_eq!(ok("GPU benchmark:"), "GPU benchmark");
        assert_eq!(ok(&"x".repeat(500)).chars().count(), MAX_REASON);
        assert!(clean_reason("  \n ").is_err());
        assert!(clean_reason(".").is_err());

        let h = 3_600_000;
        assert_eq!(expiry(None, 0), Ok(2 * h));
        assert_eq!(expiry(Some(0.5), 0), Ok(h / 2));
        assert_eq!(expiry(Some(100.0), 0), Ok(8 * h), "capped at eight hours");
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(expiry(Some(bad), 0).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_hold_ends_when_it_runs_out_or_is_cleared() {
        let cell = HoldCell::default();
        assert_eq!(cell.current(0), None);
        assert!(!cell.clear());
        let hold = Hold {
            reason: "Benchmark running".into(),
            until: 1_000,
            owner: Some("chat-1".into()),
        };
        cell.set(hold.clone());
        assert_eq!(cell.current(999), Some(hold.clone()));
        // peek sees an ended hold too: the service still has a row to remove.
        assert_eq!(cell.peek(), Some(hold.clone()));
        assert_eq!(cell.current(1_000), None, "ends at its time");
        // Reading does not remove: the ended hold stays until the watch thread's tidy clears it.
        assert_eq!(cell.peek(), Some(hold.clone()), "still there to be tidied");
        assert!(cell.clear());
        assert_eq!(cell.current(0), None);
    }

    #[test]
    fn a_hold_round_trips_through_storage_and_its_owner_with_it() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        init(&tx).unwrap();
        assert_eq!(load(&tx).unwrap(), None);
        store(
            &tx,
            Some(&Hold {
                reason: "Soak test".into(),
                until: 9_000,
                owner: Some("chat-9".into()),
            }),
        )
        .unwrap();
        assert_eq!(
            load(&tx).unwrap(),
            Some(Hold {
                reason: "Soak test".into(),
                until: 9_000,
                owner: Some("chat-9".into()),
            })
        );
        // A hold with no owner (the person set it) round-trips as one without.
        store(
            &tx,
            Some(&Hold {
                reason: "Family movie night".into(),
                until: 4_000,
                owner: None,
            }),
        )
        .unwrap();
        assert_eq!(load(&tx).unwrap().unwrap().owner, None);
        store(&tx, None).unwrap();
        assert_eq!(load(&tx).unwrap(), None);
    }
}
