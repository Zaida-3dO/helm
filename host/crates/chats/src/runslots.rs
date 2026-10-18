//! The run-slot queue.
//!
//! At most `lowRunningChats` chats (2–20, default 3) run at once before **automated** starts wait.
//! A chat runs while a turn is starting, running or stopping. Only a **Gated** send
//! ([`crate::submit::AdmissionClass::Gated`]: another chat's message, the first message of a chat
//! an agent creates, a schedule's prompt) to an **idle** chat can wait; the user's own sends,
//! answers, app notes, AFK digests and restart continuations never do, and a message to a chat
//! that is already running joins that turn as before.
//!
//! - **When it waits:** the slots are full, or the hold says no (the usage governor at wind-down
//!   or stop: nothing new starts), or the chat already has a wait, or an earlier wait would be
//!   admitted first (no overtaking).
//! - **A wait** is the send's own submission left `saved` plus a row in `slot_waits` (cascading
//!   with the chat and the submission), so it survives a restart and is never dispatched by
//!   anything but the pump. Startup recovery does not touch saved submissions.
//! - **Order:** genuine replies first (`head`: the sender already holds a message framed as from
//!   the target, so the target messaged or created it), then oldest first, then insertion order.
//! - **The pump** runs every [`TICK`] and on a wake (settings, Start now, Cancel). Pass 1: a wait
//!   whose chat is running now joins its turn as a follow-up; with the queue switched off every
//!   wait is released. Pass 2: fills the free slots in order. A wait row is deleted before its
//!   dispatch and restored if the dispatch is refused, so a wait is sent at most once. When a
//!   readmit opens a turn, that chat's other waits follow it as follow-ups.
//! - **Start now** readmits one wait over the limit (and past the hold). **Cancel wait** removes
//!   the row and marks the message not sent, so it can be recovered into the draft.
//!
//! Notes: there is no run-priority feature, so every local chat counts as low;
//! sub-agents do not hold a slot; the load gate is not built (`loadGate` off).

use crate::actor::Cmd;
use crate::runtime::ChatRuntime;
use crate::types::ChatStatus;
use helm_ipc::{ApiError, AppEvent};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub const DEFAULT_LOW_RUNNING: u32 = 3;
pub const MIN_LOW_RUNNING: u32 = 2;
pub const MAX_LOW_RUNNING: u32 = 20;
/// The pump's backstop tick.
pub const TICK: Duration = Duration::from_secs(5);
/// The event the queue's view goes out on.
pub const EVENT: &str = "resource-queue";

/// Why automated starts are held right now (`None`: not held). The usage governor installs one.
pub type HoldFn = Arc<dyn Fn(i64) -> Option<String> + Send + Sync>;

/// The runtime's run-slot state.
#[derive(Default)]
pub struct Slots {
    hold: OnceLock<HoldFn>,
    pub(crate) wake: Arc<tokio::sync::Notify>,
    /// One pass (or Start now) at a time.
    pass: Mutex<()>,
    last_view: Mutex<Option<Value>>,
    /// Chats a readmit just handed a turn: they hold a slot until their actor marks them busy
    /// (or [`RESERVE`] passes), so the next pass cannot give the same slot away again.
    reserved: Mutex<Vec<(String, std::time::Instant)>>,
}

/// How long a just-readmitted chat holds its slot before its own status takes over.
pub const RESERVE: Duration = Duration::from_secs(10);

impl Slots {
    fn reserve(&self, chat: &str) {
        let mut r = self.reserved.lock().unwrap_or_else(|p| p.into_inner());
        r.retain(|(c, _)| c != chat);
        r.push((chat.to_owned(), std::time::Instant::now()));
    }

    /// The chats still holding a reserved slot.
    pub(crate) fn reserved(&self) -> Vec<String> {
        let mut r = self.reserved.lock().unwrap_or_else(|p| p.into_inner());
        r.retain(|(_, t)| t.elapsed() < RESERVE);
        r.iter().map(|(c, _)| c.clone()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSlotSettings {
    pub enabled: bool,
    pub low_running_chats: u32,
}

impl Default for RunSlotSettings {
    fn default() -> Self {
        RunSlotSettings {
            enabled: true,
            low_running_chats: DEFAULT_LOW_RUNNING,
        }
    }
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS slot_waits (
             seq            INTEGER PRIMARY KEY AUTOINCREMENT,
             submission_id  TEXT NOT NULL UNIQUE REFERENCES submissions(id) ON DELETE CASCADE,
             chat_id        TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             since          INTEGER NOT NULL,
             head           INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS run_slot_settings (
             id     INTEGER PRIMARY KEY CHECK (id = 1),
             value  TEXT NOT NULL
         );",
    )
}

/// Read leniently: a missing or bad field is its default.
pub fn settings(tx: &Transaction<'_>) -> RunSlotSettings {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM run_slot_settings WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let v: Value = raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let d = RunSlotSettings::default();
    RunSlotSettings {
        enabled: v["enabled"].as_bool().unwrap_or(d.enabled),
        low_running_chats: v["lowRunningChats"]
            .as_u64()
            .filter(|n| (MIN_LOW_RUNNING as u64..=MAX_LOW_RUNNING as u64).contains(n))
            .map(|n| n as u32)
            .unwrap_or(d.low_running_chats),
    }
}

/// Write strictly: unknown keys, wrong types and out-of-range values refuse the save.
pub fn parse_strict(v: &Value, current: RunSlotSettings) -> Result<RunSlotSettings, String> {
    let Value::Object(m) = v else {
        return Err("Run-slot settings are an object.".into());
    };
    let mut out = current;
    for (k, val) in m {
        match k.as_str() {
            "enabled" => out.enabled = val.as_bool().ok_or("enabled is true or false.")?,
            "lowRunningChats" => {
                let n = val
                    .as_u64()
                    .filter(|n| (MIN_LOW_RUNNING as u64..=MAX_LOW_RUNNING as u64).contains(n))
                    .ok_or("Run slots: 2–20 chats.")?;
                out.low_running_chats = n as u32;
            }
            other => return Err(format!("Unknown run-slot setting: {other}.")),
        }
    }
    Ok(out)
}

fn running_rows(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    let mut st = tx.prepare(
        "SELECT id FROM chats WHERE status IN ('starting', 'running', 'stopping') ORDER BY id",
    )?;
    let rows = st.query_map([], |r| r.get(0))?;
    rows.collect()
}

/// The chats holding a slot: busy ones, plus any a readmit just handed a turn.
fn running(tx: &Transaction<'_>, reserved: &[String]) -> rusqlite::Result<Vec<String>> {
    let mut out = running_rows(tx)?;
    for c in reserved {
        if !out.contains(c) {
            out.push(c.clone());
        }
    }
    Ok(out)
}

/// The sender of a cross-chat message, from the host's own frame (never the sender's words).
fn framed_sender(text: &str) -> Option<&str> {
    let first = text.lines().next()?;
    if !first.starts_with("[Message from another chat: ") {
        return None;
    }
    let inner = first.strip_suffix(")]")?;
    let at = inner.rfind('(')?;
    Some(&inner[at + 1..])
}

/// A genuine reply: `text` is framed as from chat A, and A already holds a message framed as
/// from `target`, so the target messaged or created A.
pub fn is_head(tx: &Transaction<'_>, target: &str, text: &str) -> rusqlite::Result<bool> {
    let Some(sender) = framed_sender(text) else {
        return Ok(false);
    };
    let mut st = tx.prepare(
        "SELECT text FROM submissions WHERE chat_id = ?1 AND text LIKE '[Message from another chat: %'",
    )?;
    let texts = st.query_map([sender], |r| r.get::<_, String>(0))?;
    for t in texts {
        if framed_sender(&t?) == Some(target) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a gated send to the idle `chat_id` must wait (called inside the send's transaction).
pub fn must_wait(
    tx: &Transaction<'_>,
    chat_id: &str,
    head: bool,
    held: bool,
    reserved: &[String],
) -> rusqlite::Result<bool> {
    let s = settings(tx);
    if !s.enabled {
        return Ok(false);
    }
    if held || running(tx, reserved)?.len() >= s.low_running_chats as usize {
        return Ok(true);
    }
    // Never ahead of its own chat's wait, and never past an earlier candidate.
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM slot_waits WHERE chat_id = ?1 OR head >= ?2)",
        params![chat_id, head as i64],
        |r| r.get(0),
    )
}

pub fn insert_wait(
    tx: &Transaction<'_>,
    submission_id: &str,
    chat_id: &str,
    since: i64,
    head: bool,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO slot_waits(submission_id, chat_id, since, head) VALUES (?1, ?2, ?3, ?4)",
        params![submission_id, chat_id, since, head as i64],
    )?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Wait {
    pub chat_id: String,
    pub submission_id: String,
    /// 1-based, in admission order.
    pub position: usize,
    pub since: i64,
    pub head: bool,
}

/// Every wait in admission order: replies first, then oldest, then insertion order.
pub fn waits(tx: &Transaction<'_>) -> rusqlite::Result<Vec<Wait>> {
    let mut st = tx.prepare(
        "SELECT chat_id, submission_id, since, head FROM slot_waits
         ORDER BY head DESC, since ASC, seq ASC",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, bool>(3)?,
        ))
    })?;
    let mut out = vec![];
    for (i, row) in rows.enumerate() {
        let (chat_id, submission_id, since, head) = row?;
        out.push(Wait {
            chat_id,
            submission_id,
            position: i + 1,
            since,
            head,
        });
    }
    Ok(out)
}

pub fn position(tx: &Transaction<'_>, submission_id: &str) -> rusqlite::Result<Option<usize>> {
    Ok(waits(tx)?
        .into_iter()
        .find(|w| w.submission_id == submission_id)
        .map(|w| w.position))
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueView {
    pub settings: RunSlotSettings,
    pub limit: u32,
    pub running: Vec<String>,
    pub waiting: Vec<Wait>,
    /// Why automated starts are held (the usage governor), if they are.
    pub held: Option<String>,
}

impl ChatRuntime {
    /// Install the hold (once): asked at each send and each pump pass.
    pub fn set_slot_hold(&self, f: HoldFn) {
        if self.sh.run_slots.hold.set(f).is_err() {
            log::warn!("run slots: a hold is already installed; keeping the first");
        }
    }

    /// Why automated starts are held now.
    pub fn slot_hold(&self, now: i64) -> Option<String> {
        self.sh.run_slots.hold.get().and_then(|f| f(now))
    }

    /// Ask the pump for a pass now.
    pub fn wake_slots(&self) {
        self.sh.run_slots.wake.notify_one();
    }
}

pub fn view(rt: &ChatRuntime) -> Result<QueueView, ApiError> {
    let held = rt.slot_hold(helm_core::now_ms());
    let reserved = rt.sh.run_slots.reserved();
    rt.db()
        .snapshot(|tx| {
            let s = settings(tx);
            Ok::<_, rusqlite::Error>(QueueView {
                settings: s,
                limit: s.low_running_chats,
                running: running(tx, &reserved)?,
                waiting: waits(tx)?,
                held,
            })
        })
        .map_err(crate::ops::sql)
}

fn publish(rt: &ChatRuntime, force: bool) {
    let Ok(v) = view(rt) else { return };
    let Ok(value) = serde_json::to_value(&v) else {
        return;
    };
    let mut last = rt
        .sh
        .run_slots
        .last_view
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if !force && last.as_ref() == Some(&value) {
        return;
    }
    *last = Some(value.clone());
    drop(last);
    let mut e = AppEvent::new(EVENT);
    if let Value::Object(m) = value {
        for (k, v) in m {
            e = e.with(&k, v);
        }
    }
    rt.sh.events.emit(e);
}

fn is_busy(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row("SELECT status FROM chats WHERE id = ?1", [chat_id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .map(|s| ChatStatus::parse(&s).is_busy())
        .unwrap_or(false))
}

/// Turn a wait into a follow-up of the chat's running (or just opened) turn.
fn absorb(tx: &Transaction<'_>, w: &Wait) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM slot_waits WHERE submission_id = ?1",
        [&w.submission_id],
    )?;
    let n = tx.execute(
        "UPDATE submissions SET mode = 'follow_up', state = 'queued' WHERE id = ?1 AND state = 'saved'",
        [&w.submission_id],
    )?;
    if n == 1 {
        let pos: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM queued_submissions WHERE chat_id = ?1",
            [&w.chat_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO queued_submissions(submission_id, chat_id, position) VALUES (?1, ?2, ?3)",
            params![w.submission_id, w.chat_id, pos],
        )?;
    }
    Ok(())
}

/// What a readmit did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readmit {
    /// Handed to the chat to open a turn.
    Opened,
    /// The chat was running: the message follows its turn.
    Joined,
    /// The message is gone or no longer saved: the row is dropped.
    Nothing,
    /// The chat refused it for now: the row is back, unchanged.
    Refused(String),
}

/// Readmit one wait (the commit point: the row goes before the dispatch and comes back if the
/// chat refuses it). On an opened turn the chat's other waits follow it.
fn readmit(rt: &ChatRuntime, w: &Wait) -> Readmit {
    let step = rt.db().write(|tx| {
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM submissions WHERE id = ?1",
                [&w.submission_id],
                |r| r.get(0),
            )
            .optional()?;
        if state.as_deref() != Some("saved") {
            tx.execute(
                "DELETE FROM slot_waits WHERE submission_id = ?1",
                [&w.submission_id],
            )?;
            return Ok(Readmit::Nothing);
        }
        if is_busy(tx, &w.chat_id)? {
            absorb(tx, w)?;
            return Ok(Readmit::Joined);
        }
        tx.execute(
            "DELETE FROM slot_waits WHERE submission_id = ?1",
            [&w.submission_id],
        )?;
        Ok::<_, rusqlite::Error>(Readmit::Opened)
    });
    let r = match step {
        Ok(r) => r,
        Err(e) => return Readmit::Refused(e.to_string()),
    };
    if r != Readmit::Opened {
        return r;
    }
    rt.sh.run_slots.reserve(&w.chat_id);
    if let Err(e) = rt.command(&w.chat_id, Cmd::Dispatch(w.submission_id.clone())) {
        let _ = rt
            .db()
            .write(|tx| insert_wait(tx, &w.submission_id, &w.chat_id, w.since, w.head));
        return Readmit::Refused(e.message);
    }
    // The chat's other waits follow the turn it just opened.
    let _ = rt.db().write(|tx| {
        for other in waits(tx)?.iter().filter(|o| o.chat_id == w.chat_id) {
            absorb(tx, other)?;
        }
        Ok::<_, rusqlite::Error>(())
    });
    Readmit::Opened
}

/// One pump pass. Returns how many waits were readmitted (opened or joined).
pub fn pass(rt: &ChatRuntime, now: i64) -> usize {
    let _one = rt
        .sh
        .run_slots
        .pass
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let held = rt.slot_hold(now);
    let reserved = rt.sh.run_slots.reserved();
    let Ok((s, all, run, busy)) = rt.db().snapshot(|tx| {
        Ok::<_, rusqlite::Error>((
            settings(tx),
            waits(tx)?,
            running(tx, &reserved)?,
            running_rows(tx)?,
        ))
    }) else {
        return 0;
    };
    let mut done = 0;
    let mut touched = vec![];
    if !s.enabled {
        // Switched off: every wait goes, oldest first.
        let mut all = all;
        all.sort_by_key(|w| w.since);
        for w in all {
            if matches!(readmit(rt, &w), Readmit::Opened | Readmit::Joined) {
                done += 1;
                touched.push(w.chat_id);
            }
        }
    } else {
        // Pass 1: a chat that is running now takes its waits as follow-ups.
        for w in all.iter().filter(|w| busy.contains(&w.chat_id)) {
            if readmit(rt, w) == Readmit::Joined {
                done += 1;
                touched.push(w.chat_id.clone());
            }
        }
        // Pass 2: fill the free slots in admission order, reloading after each readmit.
        let mut free = if held.is_some() {
            0
        } else {
            (s.low_running_chats as usize).saturating_sub(run.len())
        };
        let mut skipped: Vec<String> = vec![];
        while free > 0 {
            let next = rt
                .db()
                .snapshot(waits)
                .ok()
                .and_then(|ws| ws.into_iter().find(|w| !skipped.contains(&w.chat_id)));
            let Some(w) = next else { break };
            match readmit(rt, &w) {
                Readmit::Opened => {
                    free -= 1;
                    done += 1;
                    touched.push(w.chat_id);
                }
                Readmit::Joined => {
                    done += 1;
                    touched.push(w.chat_id);
                }
                Readmit::Nothing => {}
                Readmit::Refused(e) => {
                    log::info!("run slots: {} not started yet: {e}", w.chat_id);
                    skipped.push(w.chat_id);
                }
            }
        }
    }
    for c in &touched {
        crate::submit::emit_queue(rt, c);
        crate::submit::emit_chat(rt, c);
    }
    publish(rt, done > 0);
    done
}

/// `resource_queue_start_now`: one readmit over the limit and past the hold.
pub fn start_now(rt: &ChatRuntime, chat_id: &str, submission_id: &str) -> Result<(), ApiError> {
    let _one = rt
        .sh
        .run_slots
        .pass
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let w = rt
        .db()
        .snapshot(waits)
        .map_err(crate::ops::sql)?
        .into_iter()
        .find(|w| w.submission_id == submission_id && w.chat_id == chat_id)
        .ok_or_else(|| ApiError::new("not_found", "That message is no longer waiting."))?;
    let r = readmit(rt, &w);
    crate::submit::emit_queue(rt, chat_id);
    crate::submit::emit_chat(rt, chat_id);
    publish(rt, true);
    match r {
        Readmit::Refused(e) => Err(ApiError::new("busy", e)),
        _ => Ok(()),
    }
}

/// `resource_queue_cancel`: end the wait; the message reads Not sent and can be recovered.
pub fn cancel(rt: &ChatRuntime, submission_id: &str) -> Result<(), ApiError> {
    let chat: Option<String> = rt
        .db()
        .write(|tx| {
            let chat: Option<String> = tx
                .query_row(
                    "SELECT chat_id FROM slot_waits WHERE submission_id = ?1",
                    [submission_id],
                    |r| r.get(0),
                )
                .optional()?;
            if chat.is_some() {
                tx.execute(
                    "DELETE FROM slot_waits WHERE submission_id = ?1",
                    [submission_id],
                )?;
                tx.execute(
                    "UPDATE submissions SET state = 'rejected' WHERE id = ?1 AND state = 'saved'",
                    [submission_id],
                )?;
            }
            Ok::<_, rusqlite::Error>(chat)
        })
        .map_err(crate::ops::sql)?;
    let chat =
        chat.ok_or_else(|| ApiError::new("not_found", "That message is no longer waiting."))?;
    crate::submit::emit_queue(rt, &chat);
    crate::submit::emit_chat(rt, &chat);
    publish(rt, true);
    rt.wake_slots();
    Ok(())
}

pub fn save_settings(rt: &ChatRuntime, v: &Value) -> Result<QueueView, ApiError> {
    rt.db()
        .write(|tx| {
            let s = match parse_strict(v, settings(tx)) {
                Ok(s) => s,
                Err(e) => return Ok(Err(ApiError::bad_args(e))),
            };
            tx.execute(
                "INSERT INTO run_slot_settings(id, value) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET value = excluded.value",
                [serde_json::to_string(&s).expect("settings serialise")],
            )?;
            Ok::<_, rusqlite::Error>(Ok(()))
        })
        .map_err(crate::ops::sql)??;
    rt.wake_slots();
    view(rt)
}

/// The pump: a pass every [`TICK`] and on each wake, until the runtime goes.
pub(crate) fn start(rt: &ChatRuntime) {
    let weak = Arc::downgrade(&rt.sh);
    let wake = rt.sh.run_slots.wake.clone();
    rt.sh.handle.spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(TICK) => {}
                _ = wake.notified() => {}
            }
            let Some(sh) = weak.upgrade() else { break };
            if sh.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
                continue;
            }
            let rt = ChatRuntime { sh };
            let _ = tokio::task::spawn_blocking(move || pass(&rt, helm_core::now_ms())).await;
        }
    });
}

#[cfg(test)]
#[path = "runslots_tests.rs"]
mod tests;
