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
//!   a chat on a local model server (its outages are not the internet's). A turn whose worker
//!   died under it (an end Helm did not ask for, see [`crate::exits`]) is carried on the same
//!   way, local model chats included, and a chat Helm gives up on gets one attention flag. A turn
//!   that failed because Claude Code could not refresh its sign-in token (another Claude Code
//!   process held the refresh) is tried again after about 20 s, 1, 3, 5, 8 and 10 min, each wait
//!   spread by a few seconds per chat and at most two chats started a look, so chats do not retry
//!   together; only after the sixth, about half an hour in, does the chat get one "sign in again"
//!   card and an attention flag. Every retry still passes the sign-in gate
//!   ([`helm_core::signin_gate`]). Once the gate opens (a Claude request was answered, so the
//!   sign-in works again) the chats still waiting to retry go at the next look instead of after
//!   their wait; and once a sign-in finishes, the chats Helm had given up on are tried once more
//!   and lose their flag. The person can also ask for a chat's retry now ([`retry_now`]).
//! - **Probe.** Whether the connection is back is read by opening (and at once closing) a plain
//!   TCP connection to Claude's API host: no request is made and no credential is involved. It
//!   only runs while a chat is working or waiting to be continued.
//!
//! [`Outages`] is the plain state machine behind all three; [`OutageWatch`] feeds it from the
//! database, the probe and the system's sleep and wake notices, and carries out what it decides.

use crate::actor::Cmd;
use crate::exits::Crash;
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
pub(crate) const RECENT_MS: i64 = 6 * 60 * 60 * 1000;
/// Continuations sent for one run of failures before Helm stops trying.
pub const MAX_ATTEMPTS: u32 = 3;
/// After a connection error, wait this long (with the connection answering) before continuing.
const CONNECTION_SETTLE_MS: i64 = 20_000;
/// After a service error, wait this long before the first, second and third continuation.
const SERVICE_WAIT_MS: [i64; 3] = [2 * 60_000, 5 * 60_000, 15 * 60_000];
/// After a worker crash, wait this long before the first, second and third continuation.
const WORKER_EXIT_WAIT_MS: [i64; 3] = [30_000, 2 * 60_000, 10 * 60_000];
/// Chats whose workers crashed together are continued this far apart, in the order they stopped.
pub const WORKER_EXIT_STAGGER_MS: i64 = 10_000;
/// After a failed token refresh, wait this long before each try, before the per-chat spread
/// ([`refresh_spread_ms`]). Claude Code's own advice is "retry in a minute", and the failure is a
/// lock race at the moment a token expires, so it nearly always clears within a few minutes; the
/// waits add up to about 27 minutes, so only a failure that lasts that long asks for a sign-in.
const REFRESH_WAIT_MS: [i64; 6] = [
    20_000,
    60_000,
    3 * 60_000,
    5 * 60_000,
    8 * 60_000,
    10 * 60_000,
];
/// Chats that failed a token refresh together are tried again this many to a look (a look is
/// every [`WATCH_EVERY`]), so they do not all meet the refresh lock at the same moment and spend
/// their tries on it.
pub const REFRESH_RETRIES_PER_LOOK: usize = 2;
/// After sending one, do not send another for this long even if the chat is still stopped.
const AFTER_SEND_MS: i64 = 10 * 60_000;

/// The fixed prompt for a chat whose turn the connection or the service cut short.
pub const OUTAGE_CONTINUATION_TEXT: &str = "Your last turn was cut short because the connection \
or the service failed, not because of anything you did. Before you carry on, check the current \
state (files, branches, running processes and any command you had started) and do not repeat an \
action that may already have completed. Then continue the task.";

/// The fixed prompt for a chat whose turn ended because its worker stopped unexpectedly.
pub const WORKER_EXIT_CONTINUATION_TEXT: &str = "Your last turn was cut short because Helm's \
worker process stopped unexpectedly, not because of anything you did. Before you carry on, check \
the current state (files, branches, running processes and any command you had started) and do \
not repeat an action that may already have completed. Then continue the task.";

/// The fixed prompt for a chat whose turn failed because Claude Code could not refresh its token.
pub const REFRESH_CONTINUATION_TEXT: &str = "Your last turn failed because Claude Code could not refresh its sign-in token at that moment (another Claude Code process was refreshing it), not because of anything you did. Before you carry on, check the current state (files, branches, running processes and any command you had started) and do not repeat an action that may already have completed. Then continue the task.";

/// The card and flag text for a chat whose token refresh kept failing.
pub const SIGN_IN_AGAIN_TEXT: &str = "Claude Code could not refresh its sign-in for about half an hour. Sign in again; Helm carries on by itself once it works.";

/// The head of a chat's error after a failed token refresh, before the raw line (kept after a
/// blank line for the details fold under the banner). The raw line still follows it, so the error
/// still reads as a token refresh everywhere it is checked.
pub const SIGN_IN_BUSY_HEAD: &str = "Claude's sign-in was busy (another Claude Code process was refreshing it). Helm tries again by itself.";

/// The attention flag left on a chat whose token refresh kept failing; the queue view lists
/// every chat that holds it.
pub const SIGN_IN_FLAG_QUESTION: &str = "Claude needs you to sign in again. Its token refresh kept failing for about half an hour while Helm tried again.";

/// The attention flag left on a chat whose worker kept stopping.
pub const WORKER_EXIT_FLAG_QUESTION: &str = "Helm asked Claude to carry on three times after its \
worker stopped, and it stopped again. Look at the chat and send a message when you want it to go \
on.";

/// The line left when Helm has stopped trying.
pub const GAVE_UP_TEXT: &str = "Helm asked Claude to carry on three times and it stopped again. \
Send a message when you want it to go on.";

/// Continuations sent for one run of failures of this cause before Helm stops trying.
pub fn max_attempts(cause: Cause) -> u32 {
    match cause {
        Cause::TokenRefresh => REFRESH_WAIT_MS.len() as u32,
        _ => MAX_ATTEMPTS,
    }
}

/// Why a turn stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The computer could not reach the service.
    Connection,
    /// The service answered with a failure (overloaded, unavailable, a server error).
    Service,
    /// The chat's worker ended mid-turn without Helm asking it to.
    WorkerExit,
    /// Claude Code could not refresh its sign-in token because another Claude Code process held
    /// the refresh, or gave up halfway through one. Many chats on one sign-in raise this.
    TokenRefresh,
}

impl Cause {
    fn continuation_text(self) -> &'static str {
        match self {
            Self::Connection | Self::Service => OUTAGE_CONTINUATION_TEXT,
            Self::WorkerExit => WORKER_EXIT_CONTINUATION_TEXT,
            Self::TokenRefresh => REFRESH_CONTINUATION_TEXT,
        }
    }
}

/// Whether the error text is Claude Code's failed token refresh. Text only: no token store is
/// ever read to tell.
pub fn is_token_refresh_error(error: &str) -> bool {
    error
        .to_ascii_lowercase()
        .contains("failed to refresh oauth token")
}

/// Whether the error text reads as a failed sign-in or token refresh (the refresh race, an expired
/// or rejected login). Text only.
pub fn is_sign_in_error(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    is_token_refresh_error(error)
        || e.contains("/login")
        || e.contains("token has expired")
        || e.contains("invalid authentication credentials")
        || e.contains("not logged in")
}

/// The head of the notice built for a sign-in style failure while a plan window reads used up.
/// The stored text starts with it, so `classify` can tell the notice apart from the raw sign-in
/// error behind it and stop retrying it as a token refresh.
pub const PLAN_LIMIT_HEAD: &str = "Plan limit reached";

/// What to show for a failed turn while a plan window reads used up. A sign-in style failure then
/// is the plan's limit in disguise, so it says so, with the window's reset time, and keeps the
/// raw error after a blank line for the details fold under the banner. Anything else, and any
/// turn while no window is full, is `None`: the error stands.
pub fn plan_limit_error<Tz: TimeZone>(
    tz: &Tz,
    error: &str,
    cap: Option<helm_core::usage_windows::Capacity>,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let cap = cap.filter(|c| c.pct >= 100.0)?;
    if !is_sign_in_error(error) {
        return None;
    }
    let head = match cap.until.and_then(|u| tz.timestamp_millis_opt(u).single()) {
        Some(at) => format!("{PLAN_LIMIT_HEAD}, resets at {}", at.format("%H:%M")),
        None => PLAN_LIMIT_HEAD.to_owned(),
    };
    Some(format!(
        "{head}

{error}"
    ))
}

/// What to show for a turn that failed on a token refresh: [`SIGN_IN_BUSY_HEAD`], then the raw
/// error after a blank line. `None` for any other error, and for one already worded so.
pub fn sign_in_busy_error(error: &str) -> Option<String> {
    if !is_token_refresh_error(error)
        || error.starts_with(SIGN_IN_BUSY_HEAD)
        || error.starts_with(PLAN_LIMIT_HEAD)
    {
        return None;
    }
    Some(format!("{SIGN_IN_BUSY_HEAD}\n\n{error}"))
}

/// A few seconds of spread, fixed per chat and attempt, added to the wait before a token-refresh
/// retry: chats that failed together do not retry together, and the same chat's spread is the
/// same on every look. At most a quarter of the wait.
pub fn refresh_spread_ms(chat_id: &str, attempts: u32) -> i64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    chat_id.hash(&mut h);
    attempts.hash(&mut h);
    let wait = wait_before_try(Cause::TokenRefresh, attempts);
    (h.finish() % (wait as u64 / 4 + 1)) as i64
}

/// Read a turn's error text. `None` for everything that is not an outage (a usage limit, a
/// refusal, a crash, the user's stop): those are not carried on by themselves.
pub fn classify(error: &str) -> Option<Cause> {
    // A plan-limit notice carries the raw sign-in error after its head. Retrying it while the
    // window is full would just fail again, so it is never an outage.
    if error.starts_with(PLAN_LIMIT_HEAD) {
        return None;
    }
    if is_token_refresh_error(error) {
        return Some(Cause::TokenRefresh);
    }
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
        Cause::WorkerExit => {
            WORKER_EXIT_WAIT_MS[(attempts as usize).min(WORKER_EXIT_WAIT_MS.len() - 1)]
        }
        Cause::TokenRefresh => REFRESH_WAIT_MS[(attempts as usize).min(REFRESH_WAIT_MS.len() - 1)],
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
    pub cause: Cause,
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
    /// Tried at the next look, whatever its wait and the per-look limit say.
    now_asked: bool,
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
        self.note_stopped_later(now, chat_id, stamp, cause, 0);
    }

    /// As [`Self::note_stopped`], with `extra` ms added to the wait (to spread out chats that
    /// stopped together).
    pub fn note_stopped_later(
        &mut self,
        now: i64,
        chat_id: &str,
        stamp: i64,
        cause: Cause,
        extra: i64,
    ) {
        match self.stopped.get_mut(chat_id) {
            Some(s) if s.stamp == stamp => s.stopped_now = true,
            Some(s) => {
                // Stopped again since the last look (or since it was continued). Tries are
                // counted per cause: a new cause starts from none, whatever the last one used.
                if s.cause != cause {
                    s.attempts = 0;
                }
                s.stamp = stamp;
                s.cause = cause;
                // A crash with no try left is given up on at this look: there is nothing to wait
                // for, and the attention flag should not come minutes after the crash. A failed
                // token refresh is the same: the card should not come minutes after the last try.
                s.next_at = if matches!(cause, Cause::WorkerExit | Cause::TokenRefresh)
                    && s.attempts >= max_attempts(cause)
                {
                    now
                } else {
                    now + wait_before_try(cause, s.attempts) + extra
                };
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
                        next_at: now + wait_before_try(cause, 0) + extra,
                        gave_up: false,
                        stopped_now: true,
                        now_asked: false,
                    },
                );
            }
        }
    }

    /// How many continuations this chat has had for its current run of failures.
    pub fn attempts(&self, chat_id: &str) -> u32 {
        self.stopped.get(chat_id).map_or(0, |s| s.attempts)
    }

    /// Claude's sign-in works again (the gate opened): every chat waiting to retry a token
    /// refresh goes at this look instead of after its wait. A chat given up on stays given up.
    pub fn sign_in_open(&mut self) {
        for s in self.stopped.values_mut() {
            if s.cause == Cause::TokenRefresh && !s.gave_up && s.attempts < max_attempts(s.cause) {
                s.now_asked = true;
            }
        }
    }

    /// A sign-in finished: every chat given up on after token refreshes is tried once more.
    /// Answers the chats it re-armed.
    pub fn signed_in(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for (id, s) in self.stopped.iter_mut() {
            if s.cause == Cause::TokenRefresh && s.gave_up {
                s.gave_up = false;
                s.attempts = max_attempts(s.cause) - 1;
                s.now_asked = true;
                out.push(id.clone());
            }
        }
        out.sort();
        out
    }

    /// The person asked for this chat's retry now: it goes at the next look (a chat Helm gave
    /// up on gets one more try). Answers whether the chat is waiting to be carried on at all.
    pub fn retry_now(&mut self, chat_id: &str) -> bool {
        let Some(s) = self.stopped.get_mut(chat_id) else {
            return false;
        };
        if s.gave_up || s.attempts >= max_attempts(s.cause) {
            s.gave_up = false;
            s.attempts = max_attempts(s.cause) - 1;
        }
        s.now_asked = true;
        true
    }

    /// The chat is working again, idle, stopped by the user, archived or gone.
    pub fn forget(&mut self, chat_id: &str) {
        self.stopped.remove(chat_id);
    }

    /// Chats to send a continuation to now. Connection stops wait for the probe to answer.
    pub fn due(&mut self, now: i64) -> Vec<Due> {
        let online = self.online();
        let mut out = Vec::new();
        let mut refreshes = 0;
        // Sorted, so the chats held back by the refresh limit are the same on every look.
        let mut ids: Vec<String> = self.stopped.keys().cloned().collect();
        ids.sort();
        for id in ids {
            let s = self.stopped.get_mut(&id).expect("a key just listed");
            let ready = s.stopped_now
                && !s.gave_up
                && s.attempts < max_attempts(s.cause)
                && (now >= s.next_at || s.now_asked)
                && (s.cause != Cause::Connection || online);
            let held = s.cause == Cause::TokenRefresh
                && refreshes >= REFRESH_RETRIES_PER_LOOK
                && !s.now_asked;
            if ready && !held {
                refreshes += usize::from(s.cause == Cause::TokenRefresh && !s.now_asked);
                s.now_asked = false;
                s.attempts += 1;
                s.next_at = now + AFTER_SEND_MS;
                out.push(Due {
                    chat_id: id,
                    stamp: s.stamp,
                    attempt: s.attempts,
                    cause: s.cause,
                });
            }
        }
        out.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        out
    }

    /// Chats that stopped again after their last allowed continuation: they get one closing line
    /// (and, after worker crashes, one attention flag), once per run of failures.
    pub fn give_up(&mut self, now: i64) -> Vec<(String, Cause)> {
        let mut out = Vec::new();
        for (id, s) in self.stopped.iter_mut() {
            if s.stopped_now
                && !s.gave_up
                && !s.now_asked
                && s.attempts >= max_attempts(s.cause)
                && now >= s.next_at
            {
                s.gave_up = true;
                out.push((id.clone(), s.cause));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
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

/// A stopped chat's stamp: its latest run (a draft save, which also moves `updated_at`, leaves it
/// alone), or its `updated_at` when it has no run.
const RUN_STAMP: &str = "COALESCE(
    (SELECT r.rowid FROM runs r WHERE r.chat_id = c.id ORDER BY r.started_at DESC, r.rowid DESC
     LIMIT 1),
    c.updated_at)";

/// `crashes`: the chats whose worker died mid-turn, with that turn's run. A chat is read as stopped
/// by that crash only while that run is still its latest.
fn read_look(
    db: &helm_core::db::Database,
    tracked: &[String],
    since: i64,
    crashes: &HashMap<String, Crash>,
) -> rusqlite::Result<Look> {
    db.snapshot(|tx| {
        let mut look = Look::default();
        let mut st = tx.prepare(&format!(
            "SELECT c.id, c.status, c.error, {RUN_STAMP}, c.provider, c.draft,
                    (SELECT r.id FROM runs r WHERE r.chat_id = c.id
                     ORDER BY r.started_at DESC, r.rowid DESC LIMIT 1)
             FROM chats c
             WHERE c.archived = 0
               AND (c.status IN ('starting', 'running')
                    OR (c.status = 'interrupted'
                        AND COALESCE((SELECT MAX(r.ended_at) FROM runs r WHERE r.chat_id = c.id),
                                     c.updated_at) >= ?1))
             ORDER BY c.updated_at DESC LIMIT 300"
        ))?;
        let rows = st.query_map([since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        for row in rows {
            let (id, status, error, stamp, provider, draft, run) = row?;
            let crashed = status == "interrupted"
                && run.is_some_and(|r| crashes.get(&id).is_some_and(|c| c.run_id == r));
            // A chat on a local model server does not depend on the internet: only its worker
            // dying mid-turn is carried on.
            let local = provider.is_some_and(|p| !p.is_empty());
            if local && !crashed {
                continue;
            }
            if status == "interrupted" {
                // A chat the user is writing in is theirs to carry on.
                let typing = draft.is_some_and(|d| !d.trim().is_empty());
                let outage = if local {
                    None
                } else {
                    error.as_deref().and_then(classify)
                };
                let cause = outage
                    .or(crashed.then_some(Cause::WorkerExit))
                    .filter(|_| !typing);
                look.interrupted.push((id, stamp, cause));
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
            let row: Option<(String, i64, bool, String)> = tx
                .query_row(
                    &format!(
                        "SELECT c.status, {RUN_STAMP}, c.archived, COALESCE(c.draft, '')
                         FROM chats c WHERE c.id = ?1"
                    ),
                    [&due.chat_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            match row {
                Some((status, stamp, false, draft))
                    if status == "interrupted" && stamp == due.stamp && draft.trim().is_empty() => {
                }
                _ => return Ok(false),
            }
            insert_submission(
                tx,
                &NewSubmission {
                    id: &sid,
                    chat_id: &due.chat_id,
                    text: due.cause.continuation_text(),
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
                    "Helm asked Claude to check its state and carry on (try {} of {}).",
                    due.attempt,
                    max_attempts(due.cause)
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

/// Carry on turns whose worker died under them. Off: such chats are left interrupted with their
/// reason, as before (the exit is still logged and explained).
const RESUME_AFTER_WORKER_EXIT: bool = true;

/// File the one attention flag for a chat Helm stopped carrying on after worker crashes. Its
/// reason is the chat's own error line. A muted chat's flag is kept in the Muted group.
fn flag_gave_up(rt: &ChatRuntime, chat_id: &str, question: &str, now: i64) -> rusqlite::Result<()> {
    let filed = rt.db().write(|tx| {
        let error: Option<String> = tx
            .query_row("SELECT error FROM chats WHERE id = ?1", [chat_id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        let reason: String = error
            .unwrap_or_else(|| "The worker stopped.".to_owned())
            .chars()
            .take(240)
            .collect();
        crate::flags::file(tx, chat_id, question, &reason, "normal", now)
    })?;
    if filed.is_some_and(|f| f != crate::flags::Filed::Duplicate) {
        rt.publish_attention();
    }
    Ok(())
}

/// Clear the attention flag a chat got when Helm gave up on its token refresh (and no other).
fn clear_sign_in_flag(rt: &ChatRuntime, chat_id: &str) -> rusqlite::Result<()> {
    let n = rt.db().write(|tx| {
        tx.execute(
            "DELETE FROM chat_flags WHERE chat_id = ?1 AND question = ?2",
            rusqlite::params![chat_id, SIGN_IN_FLAG_QUESTION],
        )
    })?;
    if n > 0 {
        rt.publish_attention();
    }
    Ok(())
}

/// `chat_retry_now`: the person asks for a chat's pending retry now (the "Try now" button on a
/// failed turn). The outage watch takes it at its next look, which the ask brings forward. Only
/// a chat Helm would carry on by itself has a retry to bring forward; anything else is refused in
/// plain words.
pub fn retry_now(rt: &ChatRuntime, chat_id: &str) -> Result<bool, ApiError> {
    let row: Option<(String, Option<String>, Option<String>, Option<String>, bool)> = rt
        .db()
        .snapshot(|tx| {
            tx.query_row(
                "SELECT status, error, provider, draft, archived FROM chats WHERE id = ?1",
                [chat_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
        })
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let Some((status, error, provider, draft, archived)) = row else {
        return Err(ApiError::new("not_found", "That chat does not exist."));
    };
    let nothing = || {
        ApiError::new(
            "refused",
            "Helm is not waiting to try that chat again, so there is nothing to try now. Send a \
             message to carry on.",
        )
    };
    if archived || status != "interrupted" {
        return Err(nothing());
    }
    if draft.is_some_and(|d| !d.trim().is_empty()) {
        return Err(ApiError::new(
            "refused",
            "That chat has a message being written: send it to carry on.",
        ));
    }
    let local = provider.is_some_and(|p| !p.is_empty());
    let outage = !local && error.as_deref().and_then(classify).is_some();
    let crashed = rt
        .sh
        .exits
        .recent_crashes(helm_core::now_ms())
        .contains_key(chat_id);
    if !outage && !crashed {
        return Err(nothing());
    }
    rt.sh.retry_now.lock().unwrap().insert(chat_id.to_owned());
    rt.sh.outage_poke.notify_one();
    Ok(true)
}

/// Watches for outages and sleep on behalf of the chats. Created once by the app; the system's
/// sleep and wake notices call [`Self::suspending`] and [`Self::resumed`].
pub struct OutageWatch {
    rt: ChatRuntime,
    probe: Arc<dyn Probe>,
    state: Mutex<Outages>,
    /// The sign-ins the sign-in gate had counted at the last look.
    sign_ins_seen: std::sync::atomic::AtomicU64,
}

impl OutageWatch {
    pub fn new(rt: ChatRuntime) -> Arc<Self> {
        Self::with_probe(rt, Arc::new(TcpProbe(PROBE_TARGET.to_owned())))
    }

    pub fn with_probe(rt: ChatRuntime, probe: Arc<dyn Probe>) -> Arc<Self> {
        let seen = rt.sh.spawner.sign_in_gate().map_or(0, |g| g.sign_ins());
        Arc::new(Self {
            rt,
            probe,
            state: Mutex::new(Outages::default()),
            sign_ins_seen: std::sync::atomic::AtomicU64::new(seen),
        })
    }

    /// Start looking every [`WATCH_EVERY`] until the app quits, and at once when someone asks
    /// for a chat's retry now.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        self.rt.sh.handle.spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(WATCH_EVERY) => {}
                    _ = me.rt.sh.outage_poke.notified() => {}
                }
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

    /// The computer woke. What the sign-in gate knew is stale too: the token may have expired
    /// while it slept, so the next Claude start checks it.
    pub fn resumed(&self) {
        if let Some(gate) = self.rt.sh.spawner.sign_in_gate() {
            gate.reset();
        }
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
        self.look_at(helm_core::now_ms()).await
    }

    /// One look, as of `now` (tests pass a later time to skip a wait).
    pub(crate) async fn look_at(self: &Arc<Self>, now: i64) {
        let tracked = self.state.lock().unwrap().tracked();
        let rt = self.rt.clone();
        let since = now - RECENT_MS;
        let crashes = if RESUME_AFTER_WORKER_EXIT {
            self.rt.sh.exits.recent_crashes(now)
        } else {
            HashMap::new()
        };
        let seen = crashes.clone();
        let look = match tokio::task::spawn_blocking(move || {
            read_look(rt.db(), &tracked, since, &crashes)
        })
        .await
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
        let gate = self.rt.sh.spawner.sign_in_gate();
        let (marker, due, gave_up, rearmed) = {
            let mut t = self.state.lock().unwrap();
            for (id, stamp, cause) in &look.interrupted {
                match (cause, seen.get(id)) {
                    // Counted from the crash itself, and spread out across the chats that
                    // crashed together.
                    (Some(Cause::WorkerExit), Some(c)) => t.note_stopped_later(
                        now,
                        id,
                        *stamp,
                        Cause::WorkerExit,
                        (c.at_ms - now) + i64::from(c.order) * WORKER_EXIT_STAGGER_MS,
                    ),
                    // Spread over the chats that failed together, so they do not all retry the
                    // refresh at the same moment.
                    (Some(Cause::TokenRefresh), _) => {
                        let spread = refresh_spread_ms(id, t.attempts(id));
                        t.note_stopped_later(now, id, *stamp, Cause::TokenRefresh, spread)
                    }
                    (Some(c), _) => t.note_stopped(now, id, *stamp, *c),
                    (None, crash) => {
                        t.forget(id);
                        // A remembered crash that is not carried on (the user is writing in the
                        // chat, or a newer run replaced it) is done with: clearing that draft
                        // later must not bring a resume back.
                        if crash.is_some() {
                            self.rt.sh.exits.forget(id);
                        }
                    }
                }
            }
            for (id, status) in &look.status {
                let still_stopped = look.interrupted.iter().any(|(i, ..)| i == id);
                if !still_stopped && !status.is_some_and(ChatStatus::is_busy) {
                    t.forget(id);
                }
            }
            let marker = reached.and_then(|ok| t.probe(now, ok, &look.working));
            // The sign-in gate: a sign-in that finished since the last look re-arms the chats
            // given up on, and an open gate (a Claude request was answered) lets the chats
            // waiting to retry a token refresh go now.
            let mut rearmed = Vec::new();
            if let Some(gate) = &gate {
                let signs = gate.sign_ins();
                let seen = self
                    .sign_ins_seen
                    .swap(signs, std::sync::atomic::Ordering::SeqCst);
                if signs != seen {
                    rearmed = t.signed_in();
                }
                if gate.is_open(helm_core::now_ms()) {
                    t.sign_in_open();
                }
            }
            // Chats the person asked to try now.
            for id in std::mem::take(&mut *self.rt.sh.retry_now.lock().unwrap()) {
                t.retry_now(&id);
            }
            (marker, t.due(now), t.give_up(now), rearmed)
        };
        self.write_marker(marker);
        for id in rearmed {
            let rt = self.rt.clone();
            let cleared = tokio::task::spawn_blocking(move || clear_sign_in_flag(&rt, &id)).await;
            if let Ok(Err(e)) = cleared {
                log::warn!("sign-in flag not cleared: {e}");
            }
        }
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
        for (id, cause) in gave_up {
            let (kind, line, question) = match cause {
                Cause::TokenRefresh => (
                    ContextKind::SignIn,
                    SIGN_IN_AGAIN_TEXT,
                    Some(SIGN_IN_FLAG_QUESTION),
                ),
                Cause::WorkerExit => (
                    ContextKind::Interruption,
                    GAVE_UP_TEXT,
                    Some(WORKER_EXIT_FLAG_QUESTION),
                ),
                Cause::Connection | Cause::Service => {
                    (ContextKind::Interruption, GAVE_UP_TEXT, None)
                }
            };
            self.rt.add_context_row(&id, kind, line);
            if let Some(question) = question {
                let rt = self.rt.clone();
                let filed =
                    tokio::task::spawn_blocking(move || flag_gave_up(&rt, &id, question, now))
                        .await;
                if let Ok(Err(e)) = filed {
                    log::warn!("give-up flag not filed: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn a_full_plan_window_turns_a_sign_in_failure_into_the_limit_notice() {
        use helm_core::usage_windows::Capacity;
        let tz = chrono::FixedOffset::east_opt(0).unwrap();
        let at = 1_700_000_000_000i64 + 3 * 3_600_000; // 01:13 UTC
        let full = |pct: f64, until: Option<i64>| Some(Capacity { pct, until });
        let refresh = "Failed to refresh OAuth token: another Claude Code process is refreshing it";
        let rows: &[(&str, Option<Capacity>, Option<&str>)] = &[
            (
                refresh,
                full(100.0, Some(at)),
                Some("Plan limit reached, resets at 01:13"),
            ),
            (
                "Please run /login",
                full(100.0, None),
                Some("Plan limit reached"),
            ),
            (refresh, full(99.0, Some(at)), None),
            (refresh, None, None),
            ("fetch failed", full(100.0, Some(at)), None),
        ];
        for (error, cap, want) in rows {
            let got = plan_limit_error(&tz, error, *cap);
            match want {
                Some(head) => assert_eq!(
                    got,
                    Some(format!(
                        "{head}

{error}"
                    )),
                    "{error}"
                ),
                None => assert_eq!(got, None, "{error}"),
            }
        }
    }

    #[test]
    fn only_recent_internet_chats_stopped_by_an_outage_are_picked_up() {
        let (_d, db) = crate::schema::tests::temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects (id, name, created_at) VALUES ('p', 'P', 1);
                 INSERT INTO chats (id, project_id, title, status, error, provider, draft,
                                    created_at, updated_at)
                 VALUES ('net', 'p', 't', 'interrupted', 'fetch failed', NULL, '', 1, 900),
                        ('local', 'p', 't', 'interrupted', 'fetch failed', 'srv', '', 1, 900),
                        ('typing', 'p', 't', 'interrupted', 'fetch failed', NULL, 'half a thou', 1, 900),
                        ('other', 'p', 't', 'interrupted', 'bad request', NULL, '', 1, 900),
                        ('busy', 'p', 't', 'running', NULL, NULL, '', 1, 900),
                        ('crash', 'p', 't', 'interrupted', 'The worker stopped', NULL, '', 1, 900),
                        ('lcrash', 'p', 't', 'interrupted', 'The worker stopped', 'srv', '', 1, 900),
                        ('tcrash', 'p', 't', 'interrupted', 'The worker stopped', 'srv', 'half', 1, 900),
                        ('old', 'p', 't', 'interrupted', 'The worker stopped', NULL, '', 1, 900),
                        ('refresh', 'p', 't', 'interrupted',
                         'Failed to refresh OAuth token: another Claude Code process is refreshing it or exited mid-refresh',
                         NULL, '', 1, 900),
                        ('lrefresh', 'p', 't', 'interrupted', 'Failed to refresh OAuth token', 'srv', '', 1, 900);
                 INSERT INTO runs (id, chat_id, cwd, model, thinking, status, started_at, ended_at)
                 VALUES ('r1', 'net', '.', NULL, NULL, 'interrupted', 100, 800),
                        ('r2', 'crash', '.', NULL, NULL, 'interrupted', 100, 800),
                        ('r3', 'lcrash', '.', NULL, NULL, 'interrupted', 100, 800),
                        ('r4', 'tcrash', '.', NULL, NULL, 'interrupted', 100, 800),
                        ('r5', 'old', '.', NULL, NULL, 'interrupted', 100, 800),
                        ('r6', 'old', '.', NULL, NULL, 'interrupted', 200, 800);",
            )
        })
        .unwrap();
        // The worker crashes Helm remembers: each with the run it cut short. `old` has had a
        // newer run since its crash, so that crash is no longer its latest stop.
        let crash = |run: &str| Crash {
            run_id: run.into(),
            at_ms: 800,
            order: 0,
        };
        let crashes: HashMap<String, Crash> = [
            ("crash", "r2"),
            ("lcrash", "r3"),
            ("tcrash", "r4"),
            ("old", "r5"),
        ]
        .into_iter()
        .map(|(c, r)| (c.to_owned(), crash(r)))
        .collect();
        let look = read_look(&db, &[], 500, &crashes).unwrap();
        let mut got: Vec<_> = look
            .interrupted
            .iter()
            .map(|(id, _, c)| (id.as_str(), *c))
            .collect();
        got.sort_by_key(|g| g.0);
        // A local model's chat is tracked only after a crash; one the user is typing in, or that
        // stopped for another reason, is seen but not continued.
        assert_eq!(
            got,
            [
                ("crash", Some(Cause::WorkerExit)),
                ("lcrash", Some(Cause::WorkerExit)),
                ("net", Some(Cause::Connection)),
                ("old", None),
                ("other", None),
                ("refresh", Some(Cause::TokenRefresh)),
                ("tcrash", None),
                ("typing", None)
            ]
        );
        assert_eq!(look.working, ["busy"]);
        // Without the crashes, only the outage is continued and the local chats are not seen.
        let plain = read_look(&db, &[], 500, &HashMap::new()).unwrap();
        assert!(plain.interrupted.iter().all(|(id, _, c)| matches!(
            id.as_str(),
            "net" | "refresh"
        ) == c.is_some()
            && !id.contains("lcrash")));
        // The stamp is the run, so a draft save (which moves `updated_at`) does not renew it.
        let stamp = |look: &Look| {
            look.interrupted
                .iter()
                .find(|(i, ..)| i == "net")
                .unwrap()
                .1
        };
        let before = stamp(&look);
        db.write(|tx| {
            tx.execute(
                "UPDATE chats SET updated_at = 950, draft = '' WHERE id = 'net'",
                [],
            )
        })
        .unwrap();
        assert_eq!(
            stamp(&read_look(&db, &[], 500, &HashMap::new()).unwrap()),
            before
        );
        // A stop long ago is not picked up, however recently the chat was touched.
        assert!(read_look(&db, &[], 850, &HashMap::new())
            .unwrap()
            .interrupted
            .iter()
            .all(|(i, ..)| i != "net"));
    }

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
            (
                "Failed to refresh OAuth token: another Claude Code process is refreshing it or exited mid-refresh",
                Some(Cause::TokenRefresh),
            ),
            ("failed to refresh oauth token", Some(Cause::TokenRefresh)),
            // The chat's error after a failed refresh: the words first, the raw line kept.
            (
                "Claude's sign-in was busy (another Claude Code process was refreshing it). Helm tries again by itself.\n\nFailed to refresh OAuth token: another Claude Code process is refreshing it",
                Some(Cause::TokenRefresh),
            ),
            // Not outages.
            (
                "Plan limit reached, resets at 01:13\n\nFailed to refresh OAuth token: another Claude Code process is refreshing it",
                None,
            ),
            ("Plan limit reached\n\nfailed to refresh oauth token", None),
            ("Failed to authenticate: OAuth session expired", None),
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
        // That headline form is what a failed refresh becomes, once, and nothing else is.
        let raw = "Failed to refresh OAuth token: another Claude Code process is refreshing it";
        let worded = sign_in_busy_error(raw).expect("worded");
        assert_eq!(worded, format!("{SIGN_IN_BUSY_HEAD}\n\n{raw}"));
        assert_eq!(sign_in_busy_error(&worded), None, "never worded twice");
        assert_eq!(sign_in_busy_error("fetch failed"), None);
        assert_eq!(
            sign_in_busy_error("Plan limit reached\n\nfailed to refresh oauth token"),
            None
        );
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
                attempt: 1,
                cause: Cause::Connection,
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
            assert_eq!(o.give_up(now + 1_000_000), vec![]);
            now += 30_000;
            assert_eq!(
                o.due(now),
                vec![Due {
                    chat_id: "a".into(),
                    stamp: 10 * attempt as i64,
                    attempt,
                    cause: Cause::Connection,
                }]
            );
            now += 100_000;
        }
        // The fourth stop is not continued; the chat gets its closing line once.
        o.begin_look();
        o.note_stopped(now, "a", 99, Cause::Connection);
        assert!(o.due(now + 60_000).is_empty());
        assert_eq!(
            o.give_up(now + 60_000),
            [("a".to_owned(), Cause::Connection)]
        );
        o.begin_look();
        o.note_stopped(now, "a", 99, Cause::Connection);
        assert_eq!(o.give_up(now + 120_000), vec![]);
        // Once the chat is back to normal it is forgotten and starts afresh next time.
        o.forget("a");
        assert!(o.tracked().is_empty());
    }

    /// Worker crashes wait 30 s, 2 min and 10 min, do not need the connection, are spread 10 s
    /// apart across the chats of one incident, and are tried three times before the one give-up.
    #[test]
    fn a_crashed_chat_is_carried_on_after_its_waits_staggered_then_given_up_on() {
        let mut o = Outages::default();
        o.probe(0, false, &[]);
        o.probe(1, false, &[]);
        assert!(!o.online(), "the connection does not matter for a crash");
        let t0 = 10 * MIN;
        o.begin_look();
        o.note_stopped_later(t0, "a", 1, Cause::WorkerExit, 0);
        o.note_stopped_later(t0, "b", 1, Cause::WorkerExit, WORKER_EXIT_STAGGER_MS);
        let ids = |d: Vec<Due>| {
            d.into_iter()
                .map(|d| (d.chat_id, d.attempt, d.cause))
                .collect::<Vec<_>>()
        };
        assert!(o.due(t0 + 29_000).is_empty());
        assert_eq!(
            ids(o.due(t0 + 30_000)),
            [("a".to_owned(), 1, Cause::WorkerExit)]
        );
        assert!(
            o.due(t0 + 39_000).is_empty(),
            "the second chat ten seconds later"
        );
        assert_eq!(
            ids(o.due(t0 + 40_000)),
            [("b".to_owned(), 1, Cause::WorkerExit)]
        );
        // Crashed again, twice: two minutes, then ten.
        let mut now = t0 + MIN;
        for (stamp, wait) in [(2, 2 * MIN), (3, 10 * MIN)] {
            o.begin_look();
            o.note_stopped(now, "a", stamp, Cause::WorkerExit);
            assert!(o.due(now + wait - 1_000).is_empty());
            assert_eq!(ids(o.due(now + wait)).len(), 1);
            now += wait + MIN;
        }
        // A fourth crash is not carried on; it is given up on once, with its cause, at the
        // look that sees it.
        o.begin_look();
        o.note_stopped(now, "a", 4, Cause::WorkerExit);
        assert!(o.due(now + 60 * MIN).is_empty());
        assert_eq!(o.give_up(now), [("a".to_owned(), Cause::WorkerExit)]);
        o.begin_look();
        o.note_stopped(now, "a", 4, Cause::WorkerExit);
        assert_eq!(o.give_up(now + 61 * MIN), vec![]);
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

    /// A failed token refresh waits about 20 s, 1, 3, 5, 8 and 10 min (each a little longer by a
    /// spread fixed per chat), does not need the connection, is tried six times over more than
    /// 15 minutes before the sign-in card, and is given up on at once after the sixth try, with
    /// its cause, so the card is not minutes late. Chats that failed together are retried at most
    /// [`REFRESH_RETRIES_PER_LOOK`] a look.
    #[test]
    fn a_failed_token_refresh_is_retried_for_a_long_while_one_look_at_a_time_then_given_up_on() {
        const WAITS: [i64; 6] = [20_000, 60_000, 3 * MIN, 5 * MIN, 8 * MIN, 10 * MIN];
        assert_eq!(max_attempts(Cause::TokenRefresh), 6);
        assert_eq!(max_attempts(Cause::WorkerExit), MAX_ATTEMPTS);
        assert!(WAITS.iter().sum::<i64>() >= 15 * MIN, "a long persistence");
        // The spread: stable for a chat and attempt, within a quarter of the wait, and not the
        // same for every chat.
        for attempts in 0..6 {
            let wait = wait_before_try(Cause::TokenRefresh, attempts);
            assert_eq!(wait, WAITS[attempts as usize]);
            let spreads: Vec<i64> = (0..40)
                .map(|i| refresh_spread_ms(&format!("chat-{i}"), attempts))
                .collect();
            assert!(spreads.iter().all(|s| (0..=wait / 4).contains(s)));
            assert!(
                spreads.iter().any(|s| *s != spreads[0]),
                "attempt {attempts}"
            );
            assert_eq!(refresh_spread_ms("chat-7", attempts), spreads[7]);
        }
        // Five chats that failed together: two a look, the rest on the looks after.
        let mut herd = Outages::default();
        for i in 0..5 {
            herd.note_stopped(0, &format!("c{i}"), 1, Cause::TokenRefresh);
        }
        herd.note_stopped(0, "net", 1, Cause::Service);
        let sizes: Vec<usize> = (0..4)
            .map(|look| {
                herd.begin_look();
                for i in 0..5 {
                    herd.note_stopped(0, &format!("c{i}"), 1, Cause::TokenRefresh);
                }
                herd.note_stopped(0, "net", 1, Cause::Service);
                herd.due(20 * MIN + look * 15_000)
                    .iter()
                    .filter(|d| d.cause == Cause::TokenRefresh)
                    .count()
            })
            .collect();
        assert_eq!(sizes, [REFRESH_RETRIES_PER_LOOK, 2, 1, 0]);
        // Once the sign-in works again (the gate opened), every chat still waiting to retry a
        // refresh goes at that look, in order, without its wait or the per-look limit; the
        // service stop keeps its own wait.
        let mut herd = Outages::default();
        let look = |o: &mut Outages| {
            o.begin_look();
            for i in 0..5 {
                o.note_stopped(0, &format!("c{i}"), 1, Cause::TokenRefresh);
            }
            o.note_stopped(0, "net", 1, Cause::Service);
        };
        look(&mut herd);
        assert!(herd.due(5_000).is_empty(), "still inside the first wait");
        look(&mut herd);
        herd.sign_in_open();
        let resumed: Vec<String> = herd.due(5_000).into_iter().map(|d| d.chat_id).collect();
        assert_eq!(resumed, ["c0", "c1", "c2", "c3", "c4"]);
        // A chat Helm gave up on stays given up on while the gate is open, and is tried once
        // more when a sign-in finishes; one the person asks for goes at the next look too.
        let mut o = Outages::default();
        o.begin_look();
        o.note_stopped(0, "gone", 1, Cause::TokenRefresh);
        for _ in 0..max_attempts(Cause::TokenRefresh) {
            assert!(o.retry_now("gone"));
            assert_eq!(o.due(1_000).len(), 1);
            o.begin_look();
            o.note_stopped(
                0,
                "gone",
                1 + o.attempts("gone") as i64,
                Cause::TokenRefresh,
            );
        }
        assert_eq!(o.give_up(0), [("gone".to_owned(), Cause::TokenRefresh)]);
        o.begin_look();
        o.note_stopped(0, "gone", 7, Cause::TokenRefresh);
        o.sign_in_open();
        assert!(
            o.due(60 * MIN).is_empty(),
            "given up on: the open gate alone does not retry it"
        );
        assert_eq!(o.signed_in(), ["gone"]);
        let again = o.due(0);
        assert_eq!(
            (again.len(), again[0].attempt),
            (1, max_attempts(Cause::TokenRefresh))
        );
        assert!(!o.retry_now("never-seen"), "nothing to bring forward");
        let mut o = Outages::default();
        o.probe(0, false, &[]);
        o.probe(1, false, &[]);
        assert!(
            !o.online(),
            "the connection does not matter for a token refresh"
        );
        let mut now = 10 * MIN;
        for (attempt, wait) in (1u32..=6).zip(WAITS) {
            o.begin_look();
            let spread = refresh_spread_ms("a", o.attempts("a"));
            o.note_stopped_later(now, "a", attempt as i64, Cause::TokenRefresh, spread);
            assert!(
                o.due(now + wait - 1_000).is_empty(),
                "attempt {attempt}: not yet"
            );
            let due = o.due(now + wait + spread);
            assert_eq!(due.len(), 1, "attempt {attempt}");
            assert_eq!(
                (due[0].attempt, due[0].cause),
                (attempt, Cause::TokenRefresh)
            );
            now += wait + spread + 10_000;
        }
        // The seventh failure is not retried; the chat gets its card and flag once.
        o.begin_look();
        o.note_stopped_later(now, "a", 7, Cause::TokenRefresh, 0);
        assert!(o.due(now + 60 * MIN).is_empty());
        assert_eq!(o.give_up(now), [("a".to_owned(), Cause::TokenRefresh)]);
        o.begin_look();
        o.note_stopped_later(now, "a", 7, Cause::TokenRefresh, 0);
        assert_eq!(o.give_up(now + 61 * MIN), vec![]);
        assert_eq!(
            Cause::TokenRefresh.continuation_text(),
            REFRESH_CONTINUATION_TEXT
        );
        assert!(REFRESH_CONTINUATION_TEXT.contains("do not repeat an action"));
        // Tries are counted per cause: three refresh retries do not use up the few another
        // cause allows.
        let mut o = Outages::default();
        o.probe(0, true, &[]);
        o.note_stopped(0, "b", 1, Cause::TokenRefresh);
        for stamp in 1..=3 {
            o.begin_look();
            o.note_stopped(0, "b", stamp, Cause::TokenRefresh);
            assert_eq!(o.due(40 * MIN).len(), 1);
        }
        assert_eq!(o.attempts("b"), 3);
        o.begin_look();
        o.note_stopped(40 * MIN, "b", 4, Cause::WorkerExit);
        assert_eq!(o.attempts("b"), 0, "a new cause starts from no tries");
    }
}
