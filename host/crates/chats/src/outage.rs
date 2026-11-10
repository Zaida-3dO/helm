//! Outages and sleep: what chats do when the computer, the connection or the service goes away.
//!
//! Three things happen here, all around the chats' turns and none of them inside one:
//!
//! - **Markers.** When the computer sleeps while chats are working, or the connection is down for
//!   a while, those chats get a line in their transcript ("The computer slept 02:10–06:40",
//!   "Offline for 3 min"), so a long silence has a reason on the page.
//! - **Continuation.** A turn that ended `interrupted` with a connection or service error is
//!   given one message once things look right again: it is asked to check the current state and
//!   not repeat anything that may already have happened, then carry on. That is the same wording
//!   discipline as the restart continuation. It is sent at most [`MAX_ATTEMPTS`] times for one
//!   run of failures, never to a chat the user stopped, archived or wrote to since, and never to
//!   a chat on a local model server (its outages are not the internet's).
//! - **Probe.** Whether the connection is back is read by opening (and at once closing) a plain
//!   TCP connection to Claude's API host: no request is made and no credential is involved. It
//!   only runs while a chat is working or waiting to be continued.
//!
//! [`Outages`] is the plain state machine behind all three; [`OutageWatch`] feeds it from the
//! database, the probe and the system's sleep and wake notices, and carries out what it decides.

use crate::actor::Cmd;
use crate::runtime::ChatRuntime;
use crate::session_log::{self, RowWrite};
use crate::submit::{insert_submission, NewSubmission};
use crate::types::{ChatStatus, ContextKind, SubmissionMode, SubmissionOrigin, SubmissionState};
use chrono::TimeZone;
use helm_ipc::ApiError;
use rusqlite::OptionalExtension;
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the watch looks (and probes, when it has a reason to).
pub const WATCH_EVERY: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Claude's API host: reached or not is all the probe asks.
pub const PROBE_TARGET: &str = "api.anthropic.com:443";
/// A sleep or an outage shorter than this is not worth a line.
const MIN_GAP_MS: i64 = 60_000;
/// An interrupted turn older than this is no longer carried on by itself.
const RECENT_MS: i64 = 6 * 60 * 60 * 1000;
/// Continuations sent for one run of failures before Helm stops trying.
pub const MAX_ATTEMPTS: u32 = 3;
/// After a connection error, wait this long (with the connection answering) before continuing.
const CONNECTION_SETTLE_MS: i64 = 20_000;
/// After a service error, wait this long before the first, second and third continuation.
const SERVICE_WAIT_MS: [i64; 3] = [2 * 60_000, 5 * 60_000, 15 * 60_000];
/// After sending one, do not send another for this long even if the chat is still stopped.
const AFTER_SEND_MS: i64 = 10 * 60_000;

/// The fixed prompt for a chat whose turn the connection or the service cut short.
pub const OUTAGE_CONTINUATION_TEXT: &str = "Your last turn was cut short because the connection \
or the service failed, not because of anything you did. Before you carry on, check the current \
state (files, branches, running processes and any command you had started) and do not repeat an \
action that may already have completed. Then continue the task.";

/// The line left when Helm has stopped trying.
pub const GAVE_UP_TEXT: &str = "Helm asked Claude to carry on three times and it stopped again. \
Send a message when you want it to go on.";

/// Why a turn stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The computer could not reach the service.
    Connection,
    /// The service answered with a failure (overloaded, unavailable, a server error).
    Service,
}

/// Read a turn's error text. `None` for everything that is not an outage (a usage limit, a
/// refusal, a crash, the user's stop): those are not carried on by themselves.
pub fn classify(error: &str) -> Option<Cause> {
    let e = error.to_ascii_lowercase();
    const SERVICE_WORDS: &[&str] = &[
        "overloaded",
        "internal server error",
        "service unavailable",
        "bad gateway",
        "gateway timeout",
        "temporarily unavailable",
    ];
    const SERVICE_CODES: &[&str] = &["500", "502", "503", "504", "529"];
    const CONNECTION_WORDS: &[&str] = &[
        "econnreset",
        "econnrefused",
        "enotfound",
        "etimedout",
        "eai_again",
        "enetunreach",
        "ehostunreach",
        "enetdown",
        "socket hang up",
        "fetch failed",
        "network error",
        "network is unreachable",
        "connection error",
        "unable to connect",
        "connection refused",
        "connection reset",
        "connection closed",
        "connection lost",
        "could not resolve",
        "request timed out",
    ];
    // A bare number is only a status code when the text says it came from the API.
    if SERVICE_WORDS.iter().any(|w| e.contains(w))
        || (e.contains("api error") && SERVICE_CODES.iter().any(|c| has_number(&e, c)))
    {
        return Some(Cause::Service);
    }
    if CONNECTION_WORDS.iter().any(|w| e.contains(w))
        || e.contains("getaddrinfo")
        || e.contains("connection timed out")
    {
        return Some(Cause::Connection);
    }
    None
}

/// `code` appears in `text` as a whole number (not inside a longer one).
fn has_number(text: &str, code: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(code).any(|(i, _)| {
        let before = i.checked_sub(1).and_then(|j| bytes.get(j));
        let after = bytes.get(i + code.len());
        !before.is_some_and(u8::is_ascii_digit) && !after.is_some_and(u8::is_ascii_digit)
    })
}

fn wait_before_try(cause: Cause, attempts: u32) -> i64 {
    match cause {
        Cause::Connection => CONNECTION_SETTLE_MS,
        Cause::Service => SERVICE_WAIT_MS[(attempts as usize).min(SERVICE_WAIT_MS.len() - 1)],
    }
}

/// "Offline for 3 min", "Offline for 1 h 5 min".
pub fn offline_text(gap_ms: i64) -> String {
    let mins = (gap_ms + 30_000) / 60_000;
    match (mins / 60, mins % 60) {
        (0, m) => format!("Offline for {m} min"),
        (h, 0) => format!("Offline for {h} h"),
        (h, m) => format!("Offline for {h} h {m} min"),
    }
}

/// "The computer slept 02:10–06:40"; with the weekday on both ends when it slept past midnight.
pub fn sleep_text(from_ms: i64, to_ms: i64) -> String {
    sleep_text_in(&chrono::Local, from_ms, to_ms)
}

fn sleep_text_in<Tz: TimeZone>(tz: &Tz, from_ms: i64, to_ms: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let (Some(a), Some(b)) = (
        tz.timestamp_millis_opt(from_ms).single(),
        tz.timestamp_millis_opt(to_ms).single(),
    ) else {
        return "The computer slept".to_owned();
    };
    let fmt = if a.date_naive() == b.date_naive() {
        "%H:%M"
    } else {
        "%a %H:%M"
    };
    format!("The computer slept {}–{}", a.format(fmt), b.format(fmt))
}

/// A line to write in each of these chats' transcripts.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Marker {
    pub chats: Vec<String>,
    pub text: String,
}

/// A chat to send a continuation to now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Due {
    pub chat_id: String,
    /// The `updated_at` the stop was seen at: the send is refused if the chat has moved on.
    pub stamp: i64,
    pub attempt: u32,
}

#[derive(Debug)]
struct Stopped {
    stamp: i64,
    cause: Cause,
    attempts: u32,
    next_at: i64,
    gave_up: bool,
    /// Seen stopped in the current look (cleared by [`Outages::begin_look`]).
    stopped_now: bool,
}

/// The state machine. Every method takes the time it happens at, and nothing here touches the
/// outside world.
#[derive(Debug, Default)]
pub(crate) struct Outages {
    fails: u32,
    first_fail: Option<i64>,
    offline_since: Option<i64>,
    busy_offline: BTreeSet<String>,
    slept: Option<(i64, Vec<String>)>,
    stopped: HashMap<String, Stopped>,
}

impl Outages {
    /// The last probes reached the host (or none was needed).
    pub fn online(&self) -> bool {
        self.fails == 0
    }

    /// Worth probing now: something is working on the network, something waits to be continued,
    /// or the last probe failed and the end of the outage is still unknown.
    pub fn wants_probe(&self, a_chat_is_working: bool) -> bool {
        a_chat_is_working || !self.stopped.is_empty() || self.fails > 0
    }

    pub fn tracked(&self) -> Vec<String> {
        self.stopped.keys().cloned().collect()
    }

    /// Start of one look at the chats: whatever is not seen stopped again is not stopped now.
    pub fn begin_look(&mut self) {
        for s in self.stopped.values_mut() {
            s.stopped_now = false;
        }
    }

    /// A probe's result, with the chats working at the time. Two misses in a row make an
    /// outage; the first answer after one ends it and may return its marker.
    pub fn probe(&mut self, now: i64, reached: bool, busy: &[String]) -> Option<Marker> {
        if !reached {
            self.fails += 1;
            let first = *self.first_fail.get_or_insert(now);
            if self.fails >= 2 && self.offline_since.is_none() {
                self.offline_since = Some(first);
            }
            self.busy_offline.extend(busy.iter().cloned());
            return None;
        }
        self.fails = 0;
        self.first_fail = None;
        let Some(since) = self.offline_since.take() else {
            // A blip, not an outage.
            self.busy_offline.clear();
            return None;
        };
        let mut chats = std::mem::take(&mut self.busy_offline);
        chats.extend(
            self.stopped
                .iter()
                .filter(|(_, s)| s.cause == Cause::Connection)
                .map(|(id, _)| id.clone()),
        );
        let gap = now - since;
        (gap >= MIN_GAP_MS && !chats.is_empty()).then(|| Marker {
            chats: chats.into_iter().collect(),
            text: offline_text(gap),
        })
    }

    /// The computer is about to sleep with these chats working. A repeat notice changes nothing.
    pub fn suspend(&mut self, now: i64, busy: Vec<String>) {
        if self.slept.is_none() {
            self.slept = Some((now, busy));
        }
    }

    /// The computer woke. What it knew of the connection is stale, so that starts afresh.
    pub fn resume(&mut self, now: i64) -> Option<Marker> {
        let (at, chats) = self.slept.take()?;
        self.fails = 0;
        self.first_fail = None;
        self.offline_since = None;
        self.busy_offline.clear();
        (now - at >= MIN_GAP_MS && !chats.is_empty()).then(|| Marker {
            chats,
            text: sleep_text(at, now),
        })
    }

    /// A chat is stopped by an outage, as of `stamp`.
    pub fn note_stopped(&mut self, now: i64, chat_id: &str, stamp: i64, cause: Cause) {
        match self.stopped.get_mut(chat_id) {
            Some(s) if s.stamp == stamp => s.stopped_now = true,
            Some(s) => {
                // Stopped again since the last look (or since it was continued).
                s.stamp = stamp;
                s.cause = cause;
                s.next_at = now + wait_before_try(cause, s.attempts);
                s.gave_up = false;
                s.stopped_now = true;
            }
            None => {
                self.stopped.insert(
                    chat_id.to_owned(),
                    Stopped {
                        stamp,
                        cause,
                        attempts: 0,
                        next_at: now + wait_before_try(cause, 0),
                        gave_up: false,
                        stopped_now: true,
                    },
                );
            }
        }
    }

    /// The chat is working again, idle, stopped by the user, archived or gone.
    pub fn forget(&mut self, chat_id: &str) {
        self.stopped.remove(chat_id);
    }

    /// Chats to send a continuation to now. Connection stops wait for the probe to answer.
    pub fn due(&mut self, now: i64) -> Vec<Due> {
        let online = self.online();
        let mut out = Vec::new();
        for (id, s) in self.stopped.iter_mut() {
            let ready = s.stopped_now
                && !s.gave_up
                && s.attempts < MAX_ATTEMPTS
                && now >= s.next_at
                && (s.cause == Cause::Service || online);
            if ready {
                s.attempts += 1;
                s.next_at = now + AFTER_SEND_MS;
                out.push(Due {
                    chat_id: id.clone(),
                    stamp: s.stamp,
                    attempt: s.attempts,
                });
            }
        }
        out.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        out
    }

    /// Chats that stopped again after their last allowed continuation: they get one closing line.
    pub fn give_up(&mut self, now: i64) -> Vec<String> {
        let mut out = Vec::new();
        for (id, s) in self.stopped.iter_mut() {
            if s.stopped_now && !s.gave_up && s.attempts >= MAX_ATTEMPTS && now >= s.next_at {
                s.gave_up = true;
                out.push(id.clone());
            }
        }
        out.sort();
        out
    }
}

/// Reaching the service, as a seam so a test never opens a real connection.
pub trait Probe: Send + Sync {
    fn reach(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

/// A plain TCP connection to `host:port`, opened and dropped.
pub struct TcpProbe(pub String);

impl Probe for TcpProbe {
    fn reach(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async move {
            matches!(
                tokio::time::timeout(
                    PROBE_TIMEOUT,
                    tokio::net::TcpStream::connect(self.0.as_str())
                )
                .await,
                Ok(Ok(_))
            )
        })
    }
}

/// What one look at the database found.
#[derive(Debug, Default)]
struct Look {
    /// Chats working on the internet right now.
    working: Vec<String>,
    /// Recent chats whose last turn ended `interrupted`: id, when, and the cause if it was an outage.
    interrupted: Vec<(String, i64, Option<Cause>)>,
    /// For the chats already being tracked: where they are now (`None`: gone or archived).
    status: HashMap<String, Option<ChatStatus>>,
}

fn read_look(rt: &ChatRuntime, tracked: &[String], since: i64) -> rusqlite::Result<Look> {
    rt.db().snapshot(|tx| {
        let mut look = Look::default();
        let mut st = tx.prepare(
            "SELECT id, status, error, updated_at, model FROM chats
             WHERE archived = 0
               AND (status IN ('starting', 'running') OR (status = 'interrupted' AND updated_at >= ?1))
             ORDER BY updated_at DESC LIMIT 300",
        )?;
        let rows = st.query_map([since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (id, status, error, updated_at, model) = row?;
            // A chat on a local model server does not depend on the internet.
            if model.as_deref().is_some_and(|m| crate::local::split(m).is_some()) {
                continue;
            }
            if status == "interrupted" {
                let cause = error.as_deref().and_then(classify);
                look.interrupted.push((id, updated_at, cause));
            } else {
                look.working.push(id);
            }
        }
        for id in tracked {
            let row: Option<(String, bool)> = tx
                .query_row(
                    "SELECT status, archived FROM chats WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let status = row.and_then(|(s, archived)| (!archived).then(|| ChatStatus::parse(&s)));
            look.status.insert(id.clone(), status);
        }
        Ok(look)
    })
}

/// Send one chat its outage continuation, unless it has moved on since the stop was seen.
/// Returns whether anything was sent.
fn send_continuation(rt: &ChatRuntime, due: &Due) -> Result<bool, ApiError> {
    let sid = uuid::Uuid::new_v4().to_string();
    let now = helm_core::now_ms();
    let sent = rt
        .db()
        .write(|tx| {
            let row: Option<(String, i64, bool)> = tx
                .query_row(
                    "SELECT status, updated_at, archived FROM chats WHERE id = ?1",
                    [&due.chat_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            match row {
                Some((status, stamp, false)) if status == "interrupted" && stamp == due.stamp => {}
                _ => return Ok(false),
            }
            insert_submission(
                tx,
                &NewSubmission {
                    id: &sid,
                    chat_id: &due.chat_id,
                    text: OUTAGE_CONTINUATION_TEXT,
                    mode: SubmissionMode::Prompt,
                    state: SubmissionState::Saved,
                    origin: SubmissionOrigin::App,
                    priority: None,
                    now,
                },
            )?;
            let row = RowWrite {
                source_key: format!("c:outage:{sid}"),
                role: "context",
                text: format!(
                    "Helm asked Claude to check its state and carry on (try {} of {MAX_ATTEMPTS}).",
                    due.attempt
                ),
                thinking: String::new(),
                tool_name: None,
                tool_call_id: None,
                status: "complete",
                timestamp: now,
                details: json!({"contextKind": "interruption"}),
                entry_id: None,
                parent_id: None,
            };
            session_log::upsert_row(
                tx,
                &rt.sh.cfg.artifacts(),
                &due.chat_id,
                &row,
                rt.clock().next(),
            )?;
            Ok(true)
        })
        .map_err(|e: rusqlite::Error| ApiError::internal(e.to_string()))?;
    if sent {
        rt.command(&due.chat_id, Cmd::Dispatch(sid))?;
    }
    Ok(sent)
}

/// Watches for outages and sleep on behalf of the chats. Created once by the app; the system's
/// sleep and wake notices call [`Self::suspending`] and [`Self::resumed`].
pub struct OutageWatch {
    rt: ChatRuntime,
    probe: Arc<dyn Probe>,
    state: Mutex<Outages>,
}

impl OutageWatch {
    pub fn new(rt: ChatRuntime) -> Arc<Self> {
        Self::with_probe(rt, Arc::new(TcpProbe(PROBE_TARGET.to_owned())))
    }

    pub fn with_probe(rt: ChatRuntime, probe: Arc<dyn Probe>) -> Arc<Self> {
        Arc::new(Self {
            rt,
            probe,
            state: Mutex::new(Outages::default()),
        })
    }

    /// Start looking every [`WATCH_EVERY`] until the app quits.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        self.rt.sh.handle.spawn(async move {
            loop {
                tokio::time::sleep(WATCH_EVERY).await;
                if me
                    .rt
                    .sh
                    .shutting_down
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    break;
                }
                me.look().await;
            }
        });
    }

    /// The computer is about to sleep. Quick by design: the system waits about two seconds. The
    /// chats that are working are remembered (their transcripts get the line when it wakes) and
    /// the database's log is folded into the main file, so the work so far is safe even if the
    /// machine never wakes.
    pub fn suspending(&self) {
        let now = helm_core::now_ms();
        let busy = self.rt.busy_chats();
        self.state.lock().unwrap().suspend(now, busy);
        let db = self.rt.db().clone();
        let (done, wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Err(e) = db.checkpoint() {
                log::warn!("checkpoint before sleep failed: {e}");
            }
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(Duration::from_millis(1200));
    }

    /// The computer woke.
    pub fn resumed(&self) {
        let marker = self.state.lock().unwrap().resume(helm_core::now_ms());
        self.write_marker(marker);
    }

    fn write_marker(&self, marker: Option<Marker>) {
        if let Some(m) = marker {
            for id in &m.chats {
                self.rt
                    .add_context_row(id, ContextKind::Interruption, &m.text);
            }
        }
    }

    /// One look: read the chats, probe if there is a reason, then do what the state machine says.
    async fn look(self: &Arc<Self>) {
        let tracked = self.state.lock().unwrap().tracked();
        let rt = self.rt.clone();
        let since = helm_core::now_ms() - RECENT_MS;
        let look = match tokio::task::spawn_blocking(move || read_look(&rt, &tracked, since)).await
        {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => return log::warn!("outage watch could not read the chats: {e}"),
            Err(_) => return,
        };
        let probe_now = {
            let mut t = self.state.lock().unwrap();
            t.begin_look();
            t.wants_probe(!look.working.is_empty())
        };
        let reached = if probe_now {
            Some(self.probe.reach().await)
        } else {
            None
        };
        let now = helm_core::now_ms();
        let (marker, due, gave_up) = {
            let mut t = self.state.lock().unwrap();
            for (id, stamp, cause) in &look.interrupted {
                match cause {
                    Some(c) => t.note_stopped(now, id, *stamp, *c),
                    None => t.forget(id),
                }
            }
            for (id, status) in &look.status {
                let still_stopped = look.interrupted.iter().any(|(i, ..)| i == id);
                if !still_stopped && !status.is_some_and(ChatStatus::is_busy) {
                    t.forget(id);
                }
            }
            let marker = reached.and_then(|ok| t.probe(now, ok, &look.working));
            (marker, t.due(now), t.give_up(now))
        };
        self.write_marker(marker);
        for d in due {
            let rt = self.rt.clone();
            let id = d.chat_id.clone();
            match tokio::task::spawn_blocking(move || send_continuation(&rt, &d)).await {
                Ok(Ok(true)) => log::info!("chat {id}: asked to carry on after an outage"),
                Ok(Ok(false)) => {}
                Ok(Err(e)) => log::warn!("chat {id}: outage continuation not sent: {}", e.message),
                Err(_) => {}
            }
        }
        for id in gave_up {
            self.rt
                .add_context_row(&id, ContextKind::Interruption, GAVE_UP_TEXT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn errors_are_read_as_connection_service_or_neither() {
        for (text, want) in [
            (
                "API Error: Unable to connect to API (ConnectionRefused)",
                Some(Cause::Connection),
            ),
            (
                "getaddrinfo ENOTFOUND api.anthropic.com",
                Some(Cause::Connection),
            ),
            ("fetch failed", Some(Cause::Connection)),
            ("Request timed out.", Some(Cause::Connection)),
            (
                "API Error: 529 {\"type\":\"overloaded_error\"}",
                Some(Cause::Service),
            ),
            ("API Error (503): Service Unavailable", Some(Cause::Service)),
            ("API Error: 502", Some(Cause::Service)),
            ("overloaded", Some(Cause::Service)),
            // Not outages.
            ("Rate limit reached (429)", None),
            ("Claude usage limit reached", None),
            ("Prompt is too long: 5290 tokens", None),
            ("Prompt is too long: 503 tokens over the limit", None),
            ("The task ended without completing.", None),
            ("Claude Code exited with code 1", None),
            ("", None),
        ] {
            assert_eq!(classify(text), want, "{text}");
        }
    }

    #[test]
    fn the_texts_read_plainly() {
        for (ms, want) in [
            (MIN, "Offline for 1 min"),
            (3 * MIN + 10_000, "Offline for 3 min"),
            (60 * MIN, "Offline for 1 h"),
            (65 * MIN, "Offline for 1 h 5 min"),
        ] {
            assert_eq!(offline_text(ms), want);
        }
        let utc = chrono::Utc;
        let at = |h: i64, m: i64| (h * 60 + m) * MIN;
        assert_eq!(
            sleep_text_in(&utc, at(2, 10), at(6, 40)),
            "The computer slept 02:10–06:40"
        );
        // Past midnight: the weekday says which morning (1 Jan 1970 was a Thursday).
        assert_eq!(
            sleep_text_in(&utc, at(23, 0), at(30, 5)),
            "The computer slept Thu 23:00–Fri 06:05"
        );
    }

    #[test]
    fn an_outage_is_two_misses_and_is_marked_when_it_ends() {
        let mut o = Outages::default();
        let busy = vec!["a".to_owned()];
        // One miss is a blip: nothing is written when the connection answers again.
        assert_eq!(o.probe(0, false, &busy), None);
        assert!(!o.online());
        assert_eq!(o.probe(15_000, true, &busy), None);
        assert!(o.online());
        // Two misses start an outage from the first one; the chats seen working are marked.
        assert_eq!(o.probe(100_000, false, &busy), None);
        assert_eq!(o.probe(115_000, false, &["b".to_owned()]), None);
        let m = o.probe(100_000 + 3 * MIN, true, &[]).expect("marker");
        assert_eq!(m.chats, ["a", "b"]);
        assert_eq!(m.text, "Offline for 3 min");
        // Over: the next answer says nothing more.
        assert_eq!(o.probe(100_000 + 4 * MIN, true, &busy), None);
        // A short outage (under a minute) is not worth a line.
        o.probe(1_000_000, false, &busy);
        o.probe(1_015_000, false, &busy);
        assert_eq!(o.probe(1_030_000, true, &busy), None);
    }

    #[test]
    fn sleep_marks_the_chats_that_were_working_and_resets_the_connection() {
        let mut o = Outages::default();
        o.suspend(1_000, vec!["a".to_owned()]);
        // The second notice for one sleep changes nothing.
        o.suspend(2_000, vec!["b".to_owned()]);
        o.probe(3_000, false, &[]);
        let m = o.resume(1_000 + 8 * 60 * MIN).expect("marker");
        assert_eq!(m.chats, ["a"]);
        assert!(m.text.starts_with("The computer slept "), "{}", m.text);
        assert!(o.online(), "what it knew of the connection is stale");
        // A repeated wake notice, a short nap, or no working chats: nothing to write.
        assert_eq!(o.resume(1_000 + 9 * 60 * MIN), None);
        o.suspend(0, vec!["a".to_owned()]);
        assert_eq!(o.resume(10_000), None);
        o.suspend(0, vec![]);
        assert_eq!(o.resume(60 * MIN), None);
    }

    #[test]
    fn a_stopped_chat_is_continued_when_due_at_most_three_times_then_given_up_on() {
        let mut o = Outages::default();
        let t0 = 1_000_000;
        // A connection stop waits 20 s and the connection answering.
        o.begin_look();
        o.note_stopped(t0, "a", 10, Cause::Connection);
        assert!(o.due(t0 + 1_000).is_empty());
        o.probe(t0, false, &[]);
        o.probe(t0 + 5_000, false, &[]);
        assert!(o.due(t0 + 30_000).is_empty(), "offline: not yet");
        o.probe(t0 + 31_000, true, &[]);
        assert_eq!(
            o.due(t0 + 31_000),
            vec![Due {
                chat_id: "a".into(),
                stamp: 10,
                attempt: 1
            }]
        );
        // Sent: not sent again while the chat works (it is no longer seen stopped).
        o.begin_look();
        assert!(o.due(t0 + 60_000).is_empty());
        // Stopped again, as of a newer stamp: the next try waits again, and counts on.
        let mut now = t0 + 100_000;
        for attempt in 2..=MAX_ATTEMPTS {
            o.begin_look();
            o.note_stopped(now, "a", 10 * attempt as i64, Cause::Connection);
            assert_eq!(o.give_up(now + 1_000_000), Vec::<String>::new());
            now += 30_000;
            assert_eq!(
                o.due(now),
                vec![Due {
                    chat_id: "a".into(),
                    stamp: 10 * attempt as i64,
                    attempt
                }]
            );
            now += 100_000;
        }
        // The fourth stop is not continued; the chat gets its closing line once.
        o.begin_look();
        o.note_stopped(now, "a", 99, Cause::Connection);
        assert!(o.due(now + 60_000).is_empty());
        assert_eq!(o.give_up(now + 60_000), ["a"]);
        o.begin_look();
        o.note_stopped(now, "a", 99, Cause::Connection);
        assert_eq!(o.give_up(now + 120_000), Vec::<String>::new());
        // Once the chat is back to normal it is forgotten and starts afresh next time.
        o.forget("a");
        assert!(o.tracked().is_empty());
    }

    #[test]
    fn a_service_stop_backs_off_and_does_not_need_the_probe() {
        let mut o = Outages::default();
        o.probe(0, false, &[]);
        o.begin_look();
        o.note_stopped(0, "a", 1, Cause::Service);
        assert!(o.due(MIN).is_empty(), "two minutes first");
        assert_eq!(o.due(2 * MIN).len(), 1);
        o.begin_look();
        o.note_stopped(3 * MIN, "a", 2, Cause::Service);
        assert!(o.due(7 * MIN).is_empty(), "five minutes next");
        assert_eq!(o.due(8 * MIN).len(), 1);
    }
}
