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
//!   server at a time (a server can set its own cap in its settings), and the rest queue at
//!   their start, in order: pushed-to-top first, then normal before background, then first-come.
//!   What counts as working is the server's own answer about itself — it is answering something
//!   right now and what it holds waiting, whoever put those requests there — floored by this
//!   PC's chats that are open on it but whose request the server has not taken yet. So chats
//!   wait when the GPU is actually busy, not only when this app's own list says so.
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
        "telegram" | "voice" | "speaker" => QueueClass::Interactive,
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

/// How many of `waiters` go ahead of `mine`.
pub fn ahead(mine: &Mine, waiters: &[Waiter]) -> usize {
    waiters.iter().filter(|w| goes_first(w, mine)).count()
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
/// - `working`: chats with a turn open on that place; for a Claude target, chats working
///   anywhere. For a local model target the caller passes [`server_working`] — what the server
///   itself says it is carrying, floored by this instance's chats open on it.
/// - `cap`: how many chats that place may carry (`0` would hold everything — the caller passes
///   `usize::MAX` when the cap is off).
/// - `claude_full`: the plan windows read at or over the hold threshold.
/// - `at_limit`: the app already runs its most working chats.
/// - `manual`: this chat is parked by hand.
///
/// An interactive start is held by no rule: with the windows full, or the chat parked by hand,
/// it is answered in words instead of started (the person on the phone must not be left waiting
/// on a park they cannot see). Otherwise a manual hold outranks everything.
#[allow(clippy::too_many_arguments)]
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
        return if mine.class == QueueClass::Interactive {
            Verdict::Refuse
        } else {
            Verdict::Hold(MANUAL, 0)
        };
    }
    let older = ahead(mine, waiters);
    if mine.class == QueueClass::Interactive {
        return if claude_full {
            Verdict::Refuse
        } else {
            Verdict::Go
        };
    }
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
    // A hand park's row is made by the park itself and ended only by a release. A gate that
    // read "parked" just before the release must not write the row back, or the chat would
    // stay parked with nobody to release it.
    if reason == MANUAL {
        let _ = db.write(|tx| {
            tx.execute(
                "UPDATE chat_dispatch_holds SET class = ?2 WHERE chat_id = ?1 AND reason = 'manual'",
                rusqlite::params![chat_id, class.key()],
            )
        });
        return;
    }
    // One statement, so a push to the front or a hand park made between the gate's look and
    // this write is never undone: the row keeps its front place, and a hand park is only ever
    // ended by a release.
    let r = db.write(|tx| {
        tx.execute(
            "INSERT INTO chat_dispatch_holds(chat_id, reason, class, server_id, seq, since, top)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)
             ON CONFLICT(chat_id) DO UPDATE SET
               reason = excluded.reason, class = excluded.class, server_id = excluded.server_id,
               seq = excluded.seq, since = excluded.since
             WHERE chat_dispatch_holds.reason <> 'manual' OR excluded.reason = 'manual'",
            rusqlite::params![chat_id, reason, class.key(), server_id, seq_i, now],
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

/// Give back what waits at this chat's start but leave a hand park in place: the park is the
/// user's, whatever sent the waiting message.
pub fn cancel_waiting_keeping_park(rt: &crate::runtime::ChatRuntime, chat_id: &str) {
    let _ = rt.command(
        chat_id,
        crate::actor::Cmd::QueueCancel {
            keep_park: is_manual(rt.db(), chat_id),
        },
    );
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

/// The working-chat cap a local model server names for itself, if it names one (0 switches the
/// cap off, and no answer means the `localChatCap` preference decides). The local-models crate
/// keeps the server list in its own settings row; it is read here by name for the same reason
/// the plan windows are: the gate runs where that crate cannot be reached.
pub fn server_cap(db: &Database, server_id: &str) -> Option<u64> {
    let raw: rusqlite::Result<Option<String>> = db.snapshot(|tx| {
        tx.query_row(
            "SELECT value FROM local_model_settings WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()
    });
    let raw = raw.ok().flatten()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("servers")?
        .as_array()?
        .iter()
        .find(|s| s.get("id").and_then(|i| i.as_str()) == Some(server_id))?
        .get("chatCap")?
        .as_u64()
}

/// The working count a local model server's line is measured against: what the server itself
/// says it carries — the one request it is answering, if it says it is, plus what it holds
/// waiting — floored by `ours`, this instance's chats open on it whose request may not have
/// landed yet. A server that answers nothing about its queue (Ollama serves several at once, or
/// a server not answering) leaves the instance's own count, as before. This is what makes chats
/// wait while the GPU is busy with anyone's work, not just this app's.
pub fn server_working(seen: Option<crate::local::QueueInfo>, ours: usize) -> usize {
    match seen {
        Some(q) => {
            let at_server = usize::from(q.busy) + q.queued.unwrap_or(0) as usize;
            at_server.max(ours)
        }
        None => ours,
    }
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

/// Drag one held start to stand just before another (or at the end with `before_chat_id`
/// `None`), taking that anchor's place in the pushed-to-front group so the drop is seen even
/// beside a pushed chat. The places in line are then renumbered, smallest at the front, so
/// the order the panel shows is exactly the order that now stands. Answers every place that
/// changed (`chat_id`, new `seq`) and the moved chat's new front flag — the callers keep the
/// actors in step with them. `None` when the chat is not waiting or the named anchor no
/// longer is.
pub fn move_hold(
    db: &Database,
    chat_id: &str,
    before_chat_id: Option<&str>,
) -> Option<(Vec<(String, i64)>, bool)> {
    let mut order = holds(db);
    let at = order.iter().position(|h| h.chat_id == chat_id)?;
    let mut mine = order.remove(at);
    if before_chat_id == Some(chat_id) {
        return Some((Vec::new(), mine.top));
    }
    let top = match before_chat_id {
        None => false,
        Some(b) => order.iter().find(|h| h.chat_id == b)?.top,
    };
    let to = match before_chat_id {
        None => order.len(),
        Some(b) => order.iter().position(|h| h.chat_id == b)?,
    };
    mine.top = top;
    order.insert(to, mine.clone());
    let mut seqs: Vec<i64> = order.iter().map(|h| h.seq).collect();
    seqs.sort_unstable();
    let mut changed: Vec<(String, i64)> = Vec::new();
    let wrote = db.write(|tx| {
        for (h, s) in order.iter().zip(seqs) {
            if h.chat_id == chat_id {
                tx.execute(
                    "UPDATE chat_dispatch_holds SET seq = ?2, top = ?3 WHERE chat_id = ?1",
                    rusqlite::params![chat_id, s, top as i64],
                )?;
                if h.seq != s {
                    changed.push((chat_id.to_owned(), s));
                }
            } else if h.seq != s {
                tx.execute(
                    "UPDATE chat_dispatch_holds SET seq = ?2 WHERE chat_id = ?1",
                    rusqlite::params![h.chat_id, s],
                )?;
                changed.push((h.chat_id.clone(), s));
            }
        }
        Ok::<_, rusqlite::Error>(())
    });
    wrote.ok()?;
    Some((changed, top))
}

// ------------------------------------------------------------------ the queue as one read

use crate::types::{QueueActive, QueueEntry, QueueServer, QueueSummary};

/// The queue whole in one read: every held start, front of the lines first, with each chat's
/// title and profile; the chats with a turn open right now (the panel shows them above the
/// waiting ones, each openable and haltable); what every local server carries; and what the
/// Claude windows say.
pub fn summary(db: &Database, rt: &crate::runtime::ChatRuntime) -> QueueSummary {
    let entries = db
        .snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT h.chat_id, c.title, h.reason, h.class, h.server_id, h.since, h.top,
                        NULLIF(c.profile_id, '')
                 FROM chat_dispatch_holds h JOIN chats c ON c.id = h.chat_id
                 WHERE c.archived = 0
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
                    profile_id: r.get(7)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    // The working chats come from the actors' live state; their titles and profiles are read
    // back from the db, and a chat whose turn ended between the two reads is simply gone.
    let active = {
        let live = rt.active_chats();
        let titles: std::collections::HashMap<String, (String, Option<String>)> = db
            .snapshot(|tx| {
                let mut map = std::collections::HashMap::new();
                for a in &live {
                    if let Some((title, pid)) = tx
                        .query_row(
                            "SELECT title, NULLIF(profile_id, '') FROM chats WHERE id = ?1 AND archived = 0",
                            [&a.chat_id],
                            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                        )
                        .optional()?
                    {
                        map.insert(a.chat_id.clone(), (title, pid));
                    }
                }
                Ok::<_, rusqlite::Error>(map)
            })
            .unwrap_or_default();
        live.iter()
            .filter_map(|a| {
                titles.get(&a.chat_id).map(|(title, pid)| QueueActive {
                    chat_id: a.chat_id.clone(),
                    title: title.clone(),
                    server_id: a.server_id.clone(),
                    since: a.since_ms,
                    profile_id: pid.clone(),
                })
            })
            .collect()
    };
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
    let capacity = helm_core::usage_windows::capacity(db, now);
    let claude_queued = entries
        .iter()
        .filter(|e| e.reason == CLAUDE_CAPACITY || e.reason == AT_LIMIT)
        .count() as i64;
    let hold = helm_core::usage_windows::hold_pct(db);
    let claude_full = hold != 0 && capacity.is_some_and(|c| c.full_at(hold));
    QueueSummary {
        entries,
        active,
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

/// The chats waiting on the same line as `mine`, front to back (pushed-to-top first, then the
/// oldest).
fn line_of(db: &Database, mine: &HoldRow) -> Vec<HoldRow> {
    let mut line: Vec<HoldRow> = holds(db)
        .into_iter()
        .filter(|h| same_line(mine, h))
        .collect();
    line.sort_by(|a, b| b.top.cmp(&a.top).then(a.seq.cmp(&b.seq)));
    line
}

/// The chat ids a move of `chat_id` would touch besides itself: the line's other members for
/// "top", the one it would swap places with for "up" and "down". Empty when the move touches no
/// one (or the chat is not waiting on a line).
pub fn move_touches(db: &Database, chat_id: &str, action: &str) -> Vec<String> {
    let Some(mine) = holds(db).into_iter().find(|h| h.chat_id == chat_id) else {
        return Vec::new();
    };
    if mine.reason == MANUAL {
        return Vec::new();
    }
    let line = line_of(db, &mine);
    let i = line.iter().position(|h| h.chat_id == chat_id).unwrap_or(0);
    let others = |j: Option<usize>| -> Vec<String> {
        j.and_then(|j| line.get(j))
            .map(|h| vec![h.chat_id.clone()])
            .unwrap_or_default()
    };
    match action {
        "top" => line
            .iter()
            .filter(|h| h.chat_id != chat_id)
            .map(|h| h.chat_id.clone())
            .collect(),
        "up" => others(i.checked_sub(1)),
        "down" => others(Some(i + 1)),
        _ => Vec::new(),
    }
}

/// One action from the queue panel or a queue tool, then the fresh summary. `before_chat_id`
/// belongs to `move` and names the waiting chat to stand in front of.
pub fn action(
    rt: &crate::runtime::ChatRuntime,
    chat_id: &str,
    action: &str,
    before_chat_id: Option<&str>,
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
            let Some(row) = row else {
                return Err(ApiError::new(
                    "refused",
                    "That chat is not waiting in the queue.",
                ));
            };
            if row.reason == MANUAL {
                return Err(ApiError::new(
                    "refused",
                    "A hand-held chat has no line to move along.",
                ));
            }
            set_top(db, chat_id, true);
            rt.set_hold_top(chat_id, true);
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
            let line = line_of(db, &mine);
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
        "move" => {
            let Some(row) = row else {
                return Err(ApiError::new(
                    "refused",
                    "That chat is not waiting in the queue.",
                ));
            };
            if row.reason == MANUAL {
                return Err(ApiError::new(
                    "refused",
                    "A hand-held chat has no line to move along.",
                ));
            }
            let Some((changed, top)) = move_hold(db, chat_id, before_chat_id) else {
                return Err(ApiError::new(
                    "refused",
                    "The chat it was dropped beside is no longer waiting.",
                ));
            };
            // Keep the actors' own places in step with the renumbered rows.
            for (id, seq) in changed {
                let _ = rt.command(&id, crate::actor::Cmd::QueueSeq(seq as u64));
            }
            rt.set_hold_top(chat_id, top);
        }
        "cancel" => {
            if row.is_none() {
                return Err(ApiError::new(
                    "refused",
                    format!("Chat {chat_id} is not waiting in the queue right now."),
                ));
            }
            // A hand park on a chat whose turn is running has no wait to cancel: ending it
            // would mark a live turn finished. Releasing the park is the way out.
            if row.as_ref().is_some_and(|r| r.reason == MANUAL) && rt.turn_under_way(chat_id) {
                return Err(ApiError::new(
                    "refused",
                    "That chat is working, not waiting to start. Release it instead.",
                ));
            }
            // The actor gives the waiting sends back (typed text to the draft) and clears the
            // record; with no actor the record simply goes.
            let cancel = crate::actor::Cmd::QueueCancel { keep_park: false };
            if rt.command(chat_id, cancel).is_err() {
                clear_hold(db, chat_id);
            }
        }
        "start_anyway" => {
            let Some(row) = row else {
                return Err(ApiError::new(
                    "refused",
                    "That chat is not waiting in the queue.",
                ));
            };
            if row.reason == MANUAL {
                return Err(ApiError::new(
                    "refused",
                    "A hand-held chat waits until you release it.",
                ));
            }
            if row.reason != CLAUDE_CAPACITY {
                return Err(ApiError::new(
                    "refused",
                    "That chat does not wait on Claude's plan windows; Start anyway would not change its wait.",
                ));
            }
            // The actor re-asks its gate at once with the windows answered open; the one-shot
            // lives with the actor and the start that goes spends it.
            if rt.command(chat_id, crate::actor::Cmd::QueueStartAnyway).is_err() {
                return Err(ApiError::new(
                    "refused",
                    "That chat is not waiting to start right now.",
                ));
            }
        }
        "pause" => {
            // Pause is the queue's halt for any chat on this host, whoever's it is: a working
            // chat is told to stop at its next boundary; a waiting one is parked until released.
            let by = acting_name(db);
            if rt.turn_under_way(chat_id) {
                halt_working(rt, chat_id, &by, now);
            } else {
                let class = row
                    .as_ref()
                    .map(|r| r.class.clone())
                    .unwrap_or_else(|| QueueClass::Normal.key().to_owned());
                manual_hold(db, chat_id, &class, now);
                stamp(rt, chat_id, "queuePause", format!("{by} paused this chat from the queue: it waits until they release it."), now);
            }
        }
        "stop" => {
            let by = acting_name(db);
            if rt.turn_under_way(chat_id) {
                // The existing stop semantics: the worker is taken down at once.
                stamp(rt, chat_id, "queueStop", format!("{by} stopped this chat from the queue."), now);
                let (reply, _gone) = tokio::sync::oneshot::channel();
                if rt.command(chat_id, crate::actor::Cmd::Stop { reply }).is_err() {
                    log::warn!("queue: stop of {chat_id} did not reach its actor");
                }
            } else if row.is_some() {
                // A chat that only waits has no worker to end: its stop ends the wait and
                // gives the send back, which is what "cancel" does.
                let cancel = crate::actor::Cmd::QueueCancel { keep_park: false };
                if rt.command(chat_id, cancel).is_err() {
                    clear_hold(db, chat_id);
                }
                stamp(rt, chat_id, "queueStop", format!("{by} stopped this chat from the queue: its waiting send went back."), now);
            } else {
                return Err(ApiError::new(
                    "refused",
                    "That chat is neither working nor waiting; there is nothing to stop.",
                ));
            }
        }
        other => {
            return Err(ApiError::new(
                "bad_args",
                format!(
                    "The queue knows no action \"{other}\" (top, up, down, move, hold, release, cancel, pause, stop, start_anyway)."
                ),
            ))
        }
    }
    Ok(summary(db, rt))
}

// ------------------------------------------------------------------ halting any chat from the panel

/// Who is acting: the name of the profile this Helm is showing (the home profile while all of
/// them are), used to stamp the transcript rows a queue pause or stop leaves in the target chat.
fn acting_name(db: &Database) -> String {
    let chosen: Option<String> = db
        .snapshot(|tx| {
            Ok::<_, rusqlite::Error>(
                helm_core::prefs::read(tx, "activeProfile")
                    .and_then(|v| v.as_str().map(str::to_owned)),
            )
        })
        .ok()
        .flatten();
    let name: Option<String> = db
        .snapshot(|tx| {
            let q = match chosen.as_deref() {
                Some(id) if id != "all" => {
                    tx.query_row("SELECT name FROM profiles WHERE id = ?1", [id], |r| {
                        r.get::<_, String>(0)
                    })
                }
                _ => tx.query_row("SELECT name FROM profiles WHERE is_home = 1", [], |r| {
                    r.get::<_, String>(0)
                }),
            };
            q.optional()
        })
        .ok()
        .flatten();
    name.unwrap_or_else(|| "Helm".to_owned())
}

/// The transcript row a queue halt leaves in the target chat, naming who asked. Context rows
/// are the transcript's own notes; the panel's transcript already draws them.
fn stamp(
    rt: &crate::runtime::ChatRuntime,
    chat_id: &str,
    kind: &'static str,
    text: String,
    now: i64,
) {
    let _ = rt.db().write(|tx| {
        let row = crate::session_log::RowWrite {
            source_key: format!("c:queue:{kind}:{chat_id}:{now}"),
            role: "context",
            text,
            thinking: String::new(),
            tool_name: None,
            tool_call_id: None,
            status: "complete",
            timestamp: now,
            details: serde_json::json!({ "contextKind": kind }),
            entry_id: None,
            parent_id: None,
        };
        crate::session_log::upsert_row(tx, &rt.sh.cfg.artifacts(), chat_id, &row, rt.clock().next())
    });
}

/// Tell a working chat to halt at its next boundary and stamp who asked. The instruction is a
/// steer message, so a chat mid-turn reads it at the next step it takes — finish the current
/// work, stop, and wait for the person who paused it.
fn halt_working(rt: &crate::runtime::ChatRuntime, chat_id: &str, by: &str, now: i64) {
    use crate::types::{SubmissionMode, SubmissionOrigin, SubmissionState};
    let sid = uuid::Uuid::new_v4().to_string();
    let text = format!(
        "{by} paused this chat from the queue: finish the step you are on, stop at the next \
         natural boundary, and wait for them there — do not start new work."
    );
    // The chat can end its turn between the panel's read and this write. A steer note is an
    // instruction to work: on a chat that is no longer working it would open a fresh turn
    // instead of halting one. So the write looks at the chat's state first and lets the note
    // and its transcript row go when there is nothing left to halt.
    let mut halted = false;
    let wrote = rt
        .db()
        .write(|tx| {
            let busy: bool = tx
                .query_row(
                    "SELECT status IN ('starting', 'running', 'stopping') FROM chats WHERE id = ?1",
                    [chat_id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if !busy {
                return Ok(0);
            }
            halted = true;
            crate::submit::insert_submission(
                tx,
                &crate::submit::NewSubmission {
                    id: &sid,
                    chat_id,
                    text: &text,
                    mode: SubmissionMode::Steer,
                    state: SubmissionState::Saved,
                    origin: SubmissionOrigin::App,
                    priority: None,
                    // A small prompt carrying no new work from Helm: the pause note.
                    kind: Some(crate::types::MessageKind::Nudge),
                    now,
                },
            )?;
            let row = crate::session_log::RowWrite {
                source_key: format!("c:queue:queuePause:{sid}"),
                role: "context",
                text: format!("{by} paused this chat from the queue."),
                thinking: String::new(),
                tool_name: None,
                tool_call_id: None,
                status: "complete",
                timestamp: now,
                details: serde_json::json!({ "contextKind": "queuePause" }),
                entry_id: None,
                parent_id: None,
            };
            crate::session_log::upsert_row(
                tx,
                &rt.sh.cfg.artifacts(),
                chat_id,
                &row,
                rt.clock().next(),
            )
        })
        .is_ok();
    if halted && wrote {
        let _ = rt.command(chat_id, crate::actor::Cmd::Dispatch(sid));
    } else if !halted {
        log::debug!("queue: pause of {chat_id}: its turn had already ended, nothing to halt");
    }
}

// ------------------------------------------------------------------ Claude plan capacity

/// What the plan windows say right now, and whether they read used up: the core's one reading,
/// the same rules the usage governor acts on (an expired window counts as gone — which is what
/// releases a capacity hold at the reset — and a stale reading answers nothing). `None` is
/// unknown, treated as not full everywhere.
pub use helm_core::usage_windows::claude_full;

/// The plain sentence a full window answers an interactive start with. No jargon: it goes to a
/// person on Telegram or by voice, not to a log.
pub fn claude_full_line(cap: &helm_core::usage_windows::Capacity, now: i64) -> String {
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
            // A manual hold outranks every other answer; an interactive start on a parked chat
            // is answered in words, not left waiting.
            (
                m(QueueClass::Interactive, false, 1),
                vec![],
                0,
                3,
                true,
                false,
                true,
                Verdict::Refuse,
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
            (
                m(QueueClass::Interactive, false, 1),
                vec![],
                0,
                usize::MAX,
                true,
                false,
                false,
                Verdict::Refuse,
            ),
            (
                normal,
                vec![],
                0,
                usize::MAX,
                false,
                true,
                false,
                Verdict::Hold(AT_LIMIT, 0),
            ),
            (
                normal,
                vec![],
                0,
                usize::MAX,
                false,
                false,
                true,
                Verdict::Hold(MANUAL, 0),
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
    fn a_local_server_answers_for_itself_and_our_just_opened_turns_top_it_up() {
        // (what the server says, our own open turns, expected count)
        use crate::local::QueueInfo;
        let q = |busy: bool, queued: Option<u64>| Some(QueueInfo { busy, queued });
        let cases: Vec<(Option<QueueInfo>, usize, usize)> = vec![
            // No answer at all (Ollama, or the first read on Strata): only our own chats count.
            (None, 0, 0),
            (None, 2, 2),
            // Idle and holding nothing: our own turns are the whole load.
            (q(false, Some(0)), 1, 1),
            // Answering something, whoever sent it: that is one working turn.
            (q(true, Some(0)), 0, 1),
            // Busy and holding a line behind it: the whole queue at the server counts.
            (q(true, Some(3)), 0, 4),
            // Our just-opened turns the server has not taken yet are floored over its answer,
            // and ours already at the server are not counted twice.
            (q(false, Some(0)), 2, 2),
            (q(true, Some(1)), 3, 3),
            // A server that says it is busy but not how many: busy alone.
            (q(true, None), 0, 1),
            (q(true, None), 4, 4),
        ];
        for (seen, ours, want) in cases {
            assert_eq!(server_working(seen, ours), want, "{seen:?} / ours {ours}");
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
            "page",
            "other_chat",
            "harness",
        ] {
            assert_eq!(class_for_origin(o), QueueClass::Normal, "{o}");
        }
    }

    #[test]
    fn a_server_can_name_its_own_cap_and_the_settings_answer_for_the_rest() {
        let (_d, db) = crate::schema::tests::temp_db();
        db.write(|tx| {
            // The server list lives in the local-models crate's own row; the reading here names
            // its fields, not its types, so the test makes the table by hand.
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS local_model_settings (
                     id INTEGER PRIMARY KEY CHECK (id = 1), value TEXT NOT NULL);",
            )?;
            tx.execute(
                "INSERT INTO local_model_settings(id, value) VALUES (1, ?1)",
                [serde_json::json!({
                    "servers": [
                        {"id": "strata", "kind": "strata", "chatCap": 1},
                        {"id": "ollama", "kind": "ollama"}
                    ]
                })
                .to_string()],
            )
        })
        .unwrap();
        assert_eq!(server_cap(&db, "strata"), Some(1));
        assert_eq!(
            server_cap(&db, "ollama"),
            None,
            "no answer: the default decides"
        );
        assert_eq!(server_cap(&db, "nope"), None, "no such server");
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
        manual_hold(&db, "c", QueueClass::Normal.key(), 12);
        // A hand park takes its place from the process counter, which starts at 1; give this
        // row a known one so the swap and move checks do not depend on which tests ran first.
        db.write(|tx| {
            tx.execute(
                "UPDATE chat_dispatch_holds SET seq = 3 WHERE chat_id = 'c'",
                [],
            )
        })
        .unwrap();
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
        // A gate that looked just before a hand park cannot undo it by writing its own hold.
        write_hold(&db, "c", LOCAL_CAP, QueueClass::Normal, Some("srv"), 9, 30);
        assert!(is_manual(&db, "c"), "only a release ends a hand park");
        // And a gate that read "parked" just before the release does not bring the park back.
        clear_hold(&db, "c");
        write_hold(&db, "c", MANUAL, QueueClass::Normal, None, 9, 31);
        assert!(!is_manual(&db, "c"), "a release stays released");

        // A drag-move renumbers the places so the order the panel shows is the order that
        // stands, and joining or leaving the pushed-to-front group follows where it lands.
        // (Only b still waits at this point: the test ended the others' holds along the way.)
        write_hold(&db, "a", LOCAL_CAP, QueueClass::Normal, Some("srv"), 12, 40);
        let (changed, top) = move_hold(&db, "a", Some("b")).unwrap();
        assert!(top, "in front of the pushed b means joining the front");
        assert!(!changed.is_empty());
        let order: Vec<String> = holds(&db).into_iter().map(|h| h.chat_id).collect();
        assert_eq!(order, ["a", "b"], "a leads the line now");
        write_hold(&db, "c", LOCAL_CAP, QueueClass::Normal, Some("srv"), 20, 50);
        let (_, top) = move_hold(&db, "b", Some("c")).unwrap();
        assert!(!top, "dropped behind the normal rows, b leaves the front");
        let order: Vec<String> = holds(&db).into_iter().map(|h| h.chat_id).collect();
        assert_eq!(
            order,
            ["a", "b", "c"],
            "b stays ahead of c, just no longer pushed"
        );
        // Unknown chats answer nothing, so the caller can refuse in words.
        assert!(move_hold(&db, "gone", Some("a")).is_none());
        assert!(move_hold(&db, "a", Some("gone")).is_none());
    }
}
