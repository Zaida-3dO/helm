//! Who may open a turn now, and who waits.
//!
//! Nothing here refuses a start: when a turn cannot open, its send stays saved and the chat sits
//! at its start, visibly queued, until the reason lifts. Three questions decide each turn start,
//! in one pure function ([`decide`]):
//!
//! - **A manual hold** — the user (or a queue tool) parked this chat; it waits until released,
//!   whatever the other answers say.
//! - **Capacity** — a Claude chat whose plan windows read full waits for the window to reset
//!   (which lifts the hold by itself, because an expired window stops counting), and any chat
//!   waits while the whole app already runs as many working chats as it may run at once. An
//!   interactive start is never held here; with Claude's windows full it is answered where it
//!   stands (a plain sentence, nothing started).
//! - **The local server's cap** — at most `localChatCap` of our chats work on one local model
//!   server at a time; the rest queue at their start, in order: pushed-to-top first, then normal
//!   before background, then first-come.
//!
//! The class comes from the submission's origin, never from a column on the chat: a Telegram or
//! voice turn is *interactive* (a person is waiting on the other end — it is never queued behind
//! the cap or the windows); `schedule` and app notes are *background* (they move only when nobody
//! else is waiting); everything else is *normal*.
//!
//! A held start is recorded in `chat_dispatch_holds` while it stands, so the queue can be listed,
//! reordered and released without asking every actor; a manual hold survives a restart, and the
//! automatic ones are re-derived by each actor's own gate on its first tick after one. A chat
//! mid-turn is never preempted: this is consulted only where no turn is open.

use std::sync::atomic::{AtomicU64, Ordering};

use helm_core::db::Database;
use rusqlite::OptionalExtension;

/// Why a start is held, as stored in `chat_dispatch_holds.reason` and mirrored on the chat
/// record's `queueReason`.
pub type Reason = &'static str;
/// The local model server already carries its cap of working chats.
pub const LOCAL_CAP: Reason = "local_cap";
/// The chat runs on Claude and the plan windows read full.
pub const CLAUDE_CAPACITY: Reason = "claude_capacity";
/// The user parked this chat; it waits until they (or a queue tool) release it.
pub const MANUAL: Reason = "manual";
/// The app is already running as many working chats as it may run at once.
pub const AT_LIMIT: Reason = "at_limit";

/// Where a turn start came from, in queue terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueClass {
    /// A person is waiting on the other end: Telegram, voice. Never queued; answered at once.
    Interactive,
    /// The queue's default: typed sends, button and notification sends, other chats' messages.
    Normal,
    /// Schedules and app notes: they move only when no normal or interactive start waits.
    Background,
}

impl QueueClass {
    /// Smaller goes first when two starts wait on the same place.
    pub fn rank(self) -> u8 {
        match self {
            QueueClass::Interactive => 0,
            QueueClass::Normal => 1,
            QueueClass::Background => 2,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            QueueClass::Interactive => "interactive",
            QueueClass::Normal => "normal",
            QueueClass::Background => "background",
        }
    }
}

/// The class a submission of `origin` belongs to. An unknown origin is normal.
pub fn class_for_origin(origin: &str) -> QueueClass {
    match origin {
        "telegram" | "voice" => QueueClass::Interactive,
        "schedule" | "app" => QueueClass::Background,
        _ => QueueClass::Normal,
    }
}

/// The next place in line. One counter for the whole process: a chat takes its place the moment
/// it starts to wait and keeps it while it waits.
pub fn next_seq() -> u64 {
    static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);
    NEXT_SEQ.fetch_add(1, Ordering::SeqCst)
}

/// This chat's standing at the gate.
#[derive(Clone, Copy, Debug)]
pub struct Mine {
    pub class: QueueClass,
    /// The user pushed this chat to the front of its line.
    pub top: bool,
    pub seq: u64,
}

/// Another chat already waiting on the same place.
#[derive(Clone, Copy, Debug)]
pub struct Waiter {
    pub class: QueueClass,
    pub top: bool,
    pub seq: u64,
}

/// What the gate says about a turn start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Open the turn.
    Go,
    /// Wait at the start of the local server's line; `ahead` counts the starts that stand
    /// before it.
    Behind(usize),
    /// Wait at the start for this reason; `ahead` counts the starts waiting for the same
    /// reason that stand before it, when that can be told.
    Hold(Reason, usize),
    /// Say so where the start came from, plainly; nothing is started and nothing waits.
    Refuse,
}

/// Whether a waiting start goes ahead of `mine`. Pushed-to-top first, then class rank, then the
/// older place in line.
fn goes_first(w: &Waiter, mine: &Mine) -> bool {
    match (w.top, mine.top) {
        (true, false) => return true,
        (false, true) => return false,
        _ => {}
    }
    w.class.rank() < mine.class.rank() || (w.class.rank() == mine.class.rank() && w.seq < mine.seq)
}

/// The whole gate as one pure decision.
///
/// - `waiters`: the other chats waiting at the same place (the same local server, or the same
///   Claude windows) — not the working ones, which never wait.
/// - `working`: chats with a turn open on that place; for a Claude target, chats working anywhere.
/// - `cap`: how many chats that place may carry (`0` would hold everything — the caller passes
///   `usize::MAX` when the cap is off).
/// - `claude_full`: the plan windows read at or over the hold threshold.
/// - `at_limit`: the app already runs its most working chats.
/// - `manual`: this chat is parked by hand.
///
/// An interactive start is never held by any rule; with the windows full it is answered instead
/// of started. A manual hold outranks everything.
pub fn decide(
    mine: &Mine,
    waiters: &[Waiter],
    working: usize,
    cap: usize,
    claude_full: bool,
    at_limit: bool,
    manual: bool,
) -> Verdict {
    if manual {
        return Verdict::Hold(MANUAL, 0);
    }
    if mine.class == QueueClass::Interactive {
        return if claude_full {
            Verdict::Refuse
        } else {
            Verdict::Go
        };
    }
    let older = waiters.iter().filter(|w| goes_first(w, mine)).count();
    if claude_full {
        return Verdict::Hold(CLAUDE_CAPACITY, older);
    }
    if at_limit {
        return Verdict::Hold(AT_LIMIT, older);
    }
    let over = working.saturating_add(1).saturating_sub(cap);
    match older + over {
        0 => Verdict::Go,
        n => Verdict::Behind(n),
    }
}

// ------------------------------------------------------------------ the holds table

/// One held start, as the queue list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct HoldRow {
    pub chat_id: String,
    pub reason: String,
    pub class: String,
    pub server_id: Option<String>,
    pub seq: i64,
    pub since: i64,
    pub top: bool,
}

const HOLD_COLUMNS: &str =
    "chat_id, reason, class, server_id, seq, since, top FROM chat_dispatch_holds";

/// Record that this chat's turn start waits. Replaces any hold already kept for it: one chat,
/// one place in line.
pub fn write_hold(
    db: &Database,
    chat_id: &str,
    reason: Reason,
    class: QueueClass,
    server_id: Option<&str>,
    seq: u64,
    now: i64,
) {
    let seq_i = i64::try_from(seq).unwrap_or(i64::MAX);
    let top = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT top FROM chat_dispatch_holds WHERE chat_id = ?1",
                [chat_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()
        })
        .ok()
        .flatten()
        .unwrap_or(0);
    let r = db.write(|tx| {
        tx.execute(
            "INSERT INTO chat_dispatch_holds(chat_id, reason, class, server_id, seq, since, top)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(chat_id) DO UPDATE SET
               reason = excluded.reason, class = excluded.class, server_id = excluded.server_id,
               seq = excluded.seq, since = excluded.since, top = excluded.top",
            rusqlite::params![chat_id, reason, class.key(), server_id, seq_i, now, top],
        )
    });
    if let Err(e) = r {
        log::warn!("chats {chat_id}: could not record the held start: {e}");
    }
}

/// The held start is over: it opened, went back, or was released.
pub fn clear_hold(db: &Database, chat_id: &str) {
    let _ = db.write(|tx| {
        tx.execute(
            "DELETE FROM chat_dispatch_holds WHERE chat_id = ?1",
            [chat_id],
        )
    });
}

/// The automatic holds go — a released chat, a turn that ended — but a hand-parked chat keeps
/// its row: that wait is the user's to end.
pub fn clear_auto_hold(db: &Database, chat_id: &str) {
    let _ = db.write(|tx| {
        tx.execute(
            "DELETE FROM chat_dispatch_holds WHERE chat_id = ?1 AND reason <> 'manual'",
            [chat_id],
        )
    });
}

/// Whether the queue has pushed this chat's held start to the front of its line.
pub fn top_of(db: &Database, chat_id: &str) -> bool {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT top FROM chat_dispatch_holds WHERE chat_id = ?1",
            [chat_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
    })
    .ok()
    .flatten()
    .unwrap_or(0)
        != 0
}

/// Park this chat by hand, remembering the queue class of its waiting start (`QueueClass::key`,
/// or normal when it waits for nothing yet). It waits until released, across restarts.
pub fn manual_hold(db: &Database, chat_id: &str, class_key: &str, now: i64) {
    let seq = next_seq();
    let _ = db.write(|tx| {
        tx.execute(
            "INSERT INTO chat_dispatch_holds(chat_id, reason, class, server_id, seq, since, top)
             VALUES (?1, 'manual', ?2, NULL, ?3, ?4, 0)
             ON CONFLICT(chat_id) DO UPDATE SET
               reason = 'manual', class = excluded.class, server_id = NULL, since = excluded.since",
            rusqlite::params![chat_id, class_key, seq, now],
        )
    });
}

/// Whether this chat stands parked by hand, which every gate check obeys.
pub fn is_manual(db: &Database, chat_id: &str) -> bool {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT 1 FROM chat_dispatch_holds WHERE chat_id = ?1 AND reason = 'manual'",
            [chat_id],
            |_| Ok(()),
        )
        .optional()
    })
    .ok()
    .flatten()
    .is_some()
}

/// Push a held chat to the front of its line, or drop it back.
pub fn set_top(db: &Database, chat_id: &str, top: bool) {
    let _ = db.write(|tx| {
        tx.execute(
            "UPDATE chat_dispatch_holds SET top = ?2 WHERE chat_id = ?1",
            rusqlite::params![chat_id, top as i64],
        )
    });
}

/// Every held start, front of the lines first.
pub fn holds(db: &Database) -> Vec<HoldRow> {
    let rows = db.snapshot(|tx| {
        let mut st = tx.prepare(&format!("SELECT {HOLD_COLUMNS} ORDER BY top DESC, seq"))?;
        let mapped = st.query_map([], |r| {
            Ok(HoldRow {
                chat_id: r.get(0)?,
                reason: r.get(1)?,
                class: r.get(2)?,
                server_id: r.get(3)?,
                seq: r.get(4)?,
                since: r.get(5)?,
                top: r.get::<_, i64>(6)? != 0,
            })
        })?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()
    });
    rows.unwrap_or_default()
}

/// Swap the places in line of two held starts, so the queue panel can move one up or down.
pub fn swap_seq(db: &Database, a: &str, b: &str) {
    let _ = db.write(|tx| {
        let (sa, sb) = (
            tx.query_row(
                "SELECT seq FROM chat_dispatch_holds WHERE chat_id = ?1",
                [a],
                |r| r.get::<_, i64>(0),
            ),
            tx.query_row(
                "SELECT seq FROM chat_dispatch_holds WHERE chat_id = ?1",
                [b],
                |r| r.get::<_, i64>(0),
            ),
        );
        match (sa, sb) {
            (Ok(sa), Ok(sb)) => {
                tx.execute(
                    "UPDATE chat_dispatch_holds SET seq = ?2 WHERE chat_id = ?1",
                    rusqlite::params![a, sb],
                )?;
                tx.execute(
                    "UPDATE chat_dispatch_holds SET seq = ?2 WHERE chat_id = ?1",
                    rusqlite::params![b, sa],
                )?;
            }
            _ => return Ok(()),
        }
        Ok::<_, rusqlite::Error>(())
    });
}

// ------------------------------------------------------------------ the queue as one read

use crate::types::{QueueEntry, QueueServer, QueueSummary};

/// The queue whole in one read: every held start, front of the lines first, with each chat's
/// title; what every local server carries; and what the Claude windows say.
pub fn summary(db: &Database, rt: &crate::runtime::ChatRuntime) -> QueueSummary {
    let entries = db
        .snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT h.chat_id, c.title, h.reason, h.class, h.server_id, h.since, h.top
                 FROM chat_dispatch_holds h JOIN chats c ON c.id = h.chat_id
                 ORDER BY h.top DESC, h.seq",
            )?;
            let rows = st.query_map([], |r| {
                Ok(QueueEntry {
                    chat_id: r.get(0)?,
                    title: r.get(1)?,
                    reason: r.get(2)?,
                    class: r.get(3)?,
                    server_id: r.get(4)?,
                    since: r.get(5)?,
                    top: r.get::<_, i64>(6)? != 0,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    let servers = rt
        .local_load()
        .into_iter()
        .map(|l| QueueServer {
            server_id: l.server_id,
            working: l.working as i64,
            queued: l.queued as i64,
        })
        .collect();
    let now = helm_core::now_ms();
    let capacity = claude_capacity(db, now);
    let claude_queued = entries
        .iter()
        .filter(|e| e.reason == CLAUDE_CAPACITY || e.reason == AT_LIMIT)
        .count() as i64;
    let claude_full = capacity.is_some_and(|c| {
        let hold = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, "claudeHoldPct")))
            .ok()
            .flatten()
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_CLAUDE_HOLD_PCT);
        hold != 0 && c.full_at(hold)
    });
    QueueSummary {
        entries,
        servers,
        claude_queued,
        claude_full,
        claude_until: capacity.and_then(|c| c.until),
    }
}

/// Whether two held starts wait on the same line: the same local server, or the Claude
/// windows together. A hand-parked chat has no line to move along.
fn same_line(a: &HoldRow, b: &HoldRow) -> bool {
    if a.reason == MANUAL || b.reason == MANUAL {
        return false;
    }
    match (&a.server_id, &b.server_id) {
        (Some(x), Some(y)) => x == y,
        (None, None) => true,
        _ => false,
    }
}

/// One action from the queue panel or a queue tool, then the fresh summary.
pub fn action(
    rt: &crate::runtime::ChatRuntime,
    chat_id: &str,
    action: &str,
) -> Result<QueueSummary, helm_ipc::ApiError> {
    use helm_ipc::ApiError;
    let db = rt.db();
    let now = helm_core::now_ms();
    let row = holds(db).into_iter().find(|h| h.chat_id == chat_id);
    match action {
        "hold" => {
            let class = row
                .as_ref()
                .map(|r| r.class.clone())
                .unwrap_or_else(|| QueueClass::Normal.key().to_owned());
            manual_hold(db, chat_id, &class, now);
        }
        "release" => {
            clear_hold(db, chat_id);
        }
        "top" => {
            if row.is_none() {
                return Err(ApiError::new(
                    "refused",
                    "That chat is not waiting in the queue.",
                ));
            }
            set_top(db, chat_id, true);
        }
        updown @ ("up" | "down") => {
            let mine = match row {
                Some(r) if r.reason != MANUAL => r,
                Some(_) => {
                    return Err(ApiError::new(
                        "refused",
                        "A hand-held chat has no line to move along.",
                    ))
                }
                None => {
                    return Err(ApiError::new(
                        "refused",
                        "That chat is not waiting in the queue.",
                    ))
                }
            };
            let mut line: Vec<HoldRow> = holds(db)
                .into_iter()
                .filter(|h| same_line(&mine, h))
                .collect();
            line.sort_by(|a, b| b.top.cmp(&a.top).then(a.seq.cmp(&b.seq)));
            let i = line.iter().position(|h| h.chat_id == chat_id).unwrap_or(0);
            let j = if updown == "up" {
                i.checked_sub(1)
            } else if i + 1 < line.len() {
                Some(i + 1)
            } else {
                None
            };
            if let Some(j) = j {
                let other = &line[j];
                let (sa, sb) = (mine.seq, other.seq);
                swap_seq(db, chat_id, &other.chat_id);
                // Keep the actors' own places in step with the swapped rows.
                let _ = rt.command(chat_id, crate::actor::Cmd::QueueSeq(sb as u64));
                let _ = rt.command(&other.chat_id, crate::actor::Cmd::QueueSeq(sa as u64));
            }
        }
        "cancel" => {
            if row.is_none() {
                return Err(ApiError::new(
                    "refused",
                    format!("Chat {chat_id} is not waiting in the queue right now."),
                ));
            }
            // The actor gives the waiting sends back (typed text to the draft) and clears the
            // record; with no actor the record simply goes.
            if rt.command(chat_id, crate::actor::Cmd::QueueCancel).is_err() {
                clear_hold(db, chat_id);
            }
        }
        other => {
            return Err(ApiError::new(
                "bad_args",
                format!(
                    "The queue knows no action \"{other}\" (top, up, down, hold, release, cancel)."
                ),
            ))
        }
    }
    Ok(summary(db, rt))
}

// ------------------------------------------------------------------ Claude plan capacity

/// The percent of the plan windows at which a Claude start holds (0 switches the hold off).
pub const DEFAULT_CLAUDE_HOLD_PCT: u64 = 95;
/// A usage window older than this answers nothing (the usage crate reads fresh figures every few
/// minutes; this is its own staleness line, kept apart on purpose).
const USAGE_WINDOW_STALE_MS: i64 = 15 * 60_000;

/// What the plan windows say right now: the fuller of the live windows, and when it reopens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Capacity {
    /// The higher percent in use, 0 to 100.
    pub pct: f64,
    /// When that window resets.
    pub until: Option<i64>,
}

impl Capacity {
    pub fn full_at(&self, hold_pct: u64) -> bool {
        self.pct >= hold_pct as f64
    }
}

/// The plan windows as the usage meter last read them, with dead windows dropped: one whose
/// reset time has passed (the reset is what releases a capacity hold), and one read too long ago
/// to trust. `None` means nothing is known — which never holds anything.
///
/// Reads the `plan_usage` row the usage crate keeps; the JSON shape is that crate's, used here by
/// field name so the chats crate stays free of it.
pub fn claude_capacity(db: &Database, now: i64) -> Option<Capacity> {
    let body: rusqlite::Result<Option<String>> = db.snapshot(|tx| {
        tx.query_row("SELECT body FROM plan_usage WHERE id = 1", [], |r| r.get(0))
            .optional()
    });
    let body = match body {
        Ok(Some(b)) => b,
        _ => return None,
    };
    let stored: serde_json::Value = serde_json::from_str(&body).ok()?;
    let mut best: Option<Capacity> = None;
    for key in ["fiveHour", "weekly"] {
        let Some(w) = stored.get(key) else { continue };
        let pct = w.get("pct")?.as_f64()?;
        if !pct.is_finite() {
            continue;
        }
        let resets = w.get("resetsAt").and_then(|v| v.as_i64());
        if resets.is_some_and(|r| r <= now) {
            continue;
        }
        let at = w.get("at").and_then(|v| v.as_i64())?;
        if now - at > USAGE_WINDOW_STALE_MS {
            continue;
        }
        if best.is_none_or(|b| pct > b.pct) {
            best = Some(Capacity { pct, until: resets });
        }
    }
    best
}

/// Read the hold threshold (`claudeHoldPct`; 0 switches the capacity hold off) and ask the
/// windows whether they are full. `None` is unknown, treated as not full everywhere.
pub fn claude_full(db: &Database, now: i64) -> Option<Capacity> {
    let hold_pct = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, "claudeHoldPct")))
        .ok()
        .flatten()
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_CLAUDE_HOLD_PCT);
    if hold_pct == 0 {
        return None;
    }
    let cap = claude_capacity(db, now)?;
    cap.full_at(hold_pct).then_some(cap)
}

/// The plain sentence a full window answers an interactive start with. No jargon: it goes to a
/// person on Telegram or by voice, not to a log.
pub fn claude_full_line(cap: &Capacity, now: i64) -> String {
    match cap.until {
        Some(until) if until > now => {
            let minutes = ((until - now) / 60_000).max(1);
            format!(
                "Claude's plan allowance is used up right now, and it should open again in about \
                 {minutes} minute{} — nothing was started, so please send it again then.",
                if minutes == 1 { "" } else { "s" }
            )
        }
        _ => "Claude's plan allowance is used up right now — nothing was started, so please send \
              it again in a little while."
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn w(class: QueueClass, top: bool, seq: u64) -> Waiter {
        Waiter { class, top, seq }
    }
    fn m(class: QueueClass, top: bool, seq: u64) -> Mine {
        Mine { class, top, seq }
    }

    #[test]
    fn the_gate_is_one_table_of_answers() {
        // (mine, waiters, working, cap, claude_full, at_limit, manual, expected)
        let normal = m(QueueClass::Normal, false, 5);
        type Case = (Mine, Vec<Waiter>, usize, usize, bool, bool, bool, Verdict);
        let cases: Vec<Case> = vec![
            // A free server opens the turn.
            (normal, vec![], 0, 3, false, false, false, Verdict::Go),
            // A place over the cap waits, and says how many stand ahead.
            (
                normal,
                vec![],
                3,
                3,
                false,
                false,
                false,
                Verdict::Behind(1),
            ),
            // Waiters that go first are counted; one behind is not.
            (
                normal,
                vec![
                    w(QueueClass::Normal, false, 4),
                    w(QueueClass::Normal, false, 9),
                ],
                0,
                3,
                false,
                false,
                false,
                Verdict::Behind(1),
            ),
            // Background waits behind a normal waiter even from an older place.
            (
                m(QueueClass::Background, false, 1),
                vec![w(QueueClass::Normal, false, 8)],
                0,
                3,
                false,
                false,
                false,
                Verdict::Behind(1),
            ),
            // Pushed to the front: the top waiter goes ahead of everyone, and the top chat
            // goes ahead of waiters that are not.
            (
                m(QueueClass::Normal, true, 9),
                vec![w(QueueClass::Normal, false, 1)],
                0,
                3,
                false,
                false,
                false,
                Verdict::Go,
            ),
            (
                m(QueueClass::Normal, false, 1),
                vec![w(QueueClass::Normal, true, 9)],
                0,
                3,
                false,
                false,
                false,
                Verdict::Behind(1),
            ),
            // Interactive never waits on the cap, however full the server is.
            (
                m(QueueClass::Interactive, false, 7),
                vec![],
                9,
                3,
                false,
                false,
                false,
                Verdict::Go,
            ),
            // Claude windows full: normal and background hold; interactive is answered instead.
            (
                normal,
                vec![],
                0,
                3,
                true,
                false,
                false,
                Verdict::Hold(CLAUDE_CAPACITY, 0),
            ),
            (
                m(QueueClass::Background, false, 1),
                vec![],
                0,
                3,
                true,
                false,
                false,
                Verdict::Hold(CLAUDE_CAPACITY, 0),
            ),
            (
                m(QueueClass::Interactive, false, 1),
                vec![],
                0,
                3,
                true,
                false,
                false,
                Verdict::Refuse,
            ),
            // Too many working chats at once holds everything but interactive.
            (
                normal,
                vec![],
                0,
                3,
                false,
                true,
                false,
                Verdict::Hold(AT_LIMIT, 0),
            ),
            (
                m(QueueClass::Interactive, false, 1),
                vec![],
                0,
                3,
                false,
                true,
                false,
                Verdict::Go,
            ),
            // A manual hold outranks every other answer, interactive included.
            (
                m(QueueClass::Interactive, false, 1),
                vec![],
                0,
                3,
                true,
                false,
                true,
                Verdict::Hold(MANUAL, 0),
            ),
            (
                normal,
                vec![],
                0,
                3,
                false,
                false,
                true,
                Verdict::Hold(MANUAL, 0),
            ),
            // Cap off (usize::MAX) only orders by the waiters.
            (
                normal,
                vec![],
                9,
                usize::MAX,
                false,
                false,
                false,
                Verdict::Go,
            ),
        ];
        for (mine, waiters, working, cap, cf, at_limit, manual, want) in cases {
            let got = decide(&mine, &waiters, working, cap, cf, at_limit, manual);
            assert_eq!(
                got, want,
                "{mine:?} / waiters {waiters:?} / {working} of {cap}"
            );
        }
    }

    #[test]
    fn classes_come_from_the_origin() {
        for o in ["telegram", "voice"] {
            assert_eq!(class_for_origin(o), QueueClass::Interactive, "{o}");
        }
        for o in ["schedule", "app"] {
            assert_eq!(class_for_origin(o), QueueClass::Background, "{o}");
        }
        for o in [
            "typed",
            "action",
            "notification",
            "answer",
            "remote",
            "other_chat",
            "harness",
        ] {
            assert_eq!(class_for_origin(o), QueueClass::Normal, "{o}");
        }
    }

    /// A temp database with only what these tests ask of it: the chats tables plus the usage
    /// row (a table another crate makes; the reading here names its fields, not its types).
    fn usage_db(body: Option<serde_json::Value>) -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(
            &dir.path().join("t.sqlite3"),
            &crate::schema::tests::registry(),
        )
        .unwrap();
        db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS plan_usage (
                     id INTEGER PRIMARY KEY CHECK (id = 1), body TEXT NOT NULL, updated_at INTEGER NOT NULL);",
            )?;
            if let Some(b) = body {
                tx.execute(
                    "INSERT INTO plan_usage(id, body, updated_at) VALUES (1, ?1, 0)",
                    [b.to_string()],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        (dir, db)
    }

    #[test]
    fn the_windows_answer_full_only_while_live_and_past_the_line() {
        let now = 1_000_000_000_000i64;
        let win =
            |pct: f64, resets: i64, at: i64| json!({"pct": pct, "resetsAt": resets, "at": at});
        // A live window past the line: full, and it says when it reopens.
        let (_d, db) = usage_db(Some(
            json!({"weekly": win(96.0, now + 3_600_000, now - 60_000)}),
        ));
        let cap = claude_capacity(&db, now).expect("the window answers");
        assert!(cap.full_at(95));
        assert_eq!(cap.until, Some(now + 3_600_000));
        // The fuller of two live windows answers.
        drop(db);
        let (_d, db) = usage_db(Some(json!({
            "fiveHour": win(40.0, now + 900_000, now - 60_000),
            "weekly": win(97.5, now + 86_400_000, now - 120_000),
        })));
        assert_eq!(claude_capacity(&db, now).unwrap().pct, 97.5);
        // A window whose reset has passed answers nothing: that is the automatic release.
        drop(db);
        let (_d, db) = usage_db(Some(json!({"weekly": win(99.0, now - 1, now - 60_000)})));
        assert!(claude_capacity(&db, now).is_none());
        // A window read too long ago answers nothing either (unknown, never full).
        drop(db);
        let (_d, db) = usage_db(Some(
            json!({"weekly": win(99.0, now + 3_600_000, now - USAGE_WINDOW_STALE_MS - 1)}),
        ));
        assert!(claude_capacity(&db, now).is_none());
        // Under the line: not full. And with no reading at all: unknown, not full.
        drop(db);
        let (_d, db) = usage_db(Some(
            json!({"weekly": win(94.0, now + 3_600_000, now - 60_000)}),
        ));
        assert!(!claude_capacity(&db, now).unwrap().full_at(95));
        drop(db);
        let (_d, db) = usage_db(None);
        assert!(claude_capacity(&db, now).is_none());
    }

    #[test]
    fn a_parked_chat_waits_until_released_and_the_lines_sort_by_front() {
        let (_d, db) = crate::schema::tests::temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats(id, project_id, created_at, updated_at) VALUES
                   ('a', 'p', 0, 0), ('b', 'p', 0, 0), ('c', 'p', 0, 0);",
            )
        })
        .unwrap();
        write_hold(&db, "a", LOCAL_CAP, QueueClass::Normal, Some("srv"), 1, 10);
        write_hold(
            &db,
            "b",
            CLAUDE_CAPACITY,
            QueueClass::Background,
            None,
            2,
            11,
        );
        write_hold(&db, "c", MANUAL, QueueClass::Normal, None, 3, 12);
        set_top(&db, "b", true);
        assert!(is_manual(&db, "c"));
        assert!(!is_manual(&db, "a"));
        let order: Vec<String> = holds(&db).into_iter().map(|h| h.chat_id).collect();
        assert_eq!(
            order,
            ["b", "a", "c"],
            "the pushed-to-top line first, then places"
        );
        swap_seq(&db, "a", "c");
        let order: Vec<String> = holds(&db).into_iter().map(|h| h.chat_id).collect();
        assert_eq!(order, ["b", "c", "a"], "the swap moves c ahead of a");
        clear_hold(&db, "a");
        assert!(holds(&db).into_iter().all(|h| h.chat_id != "a"));
        // A re-hold keeps the top flag the queue panel set.
        write_hold(&db, "b", LOCAL_CAP, QueueClass::Normal, None, 3, 20);
        assert!(holds(&db).into_iter().any(|h| h.chat_id == "b" && h.top));
    }
}
