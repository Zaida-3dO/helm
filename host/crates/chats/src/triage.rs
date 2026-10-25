//! Idle-chat triage.
//!
//! A chat that has been quiet for a while may be waiting on you without anything saying so: a
//! request left half done, a failure nobody saw, a question the last reply never got answered. A
//! quiet helper (`helpers/triage`, one short-lived process per job, the unmodified `claude` binary
//! with a small model) reads the end of the chat and says whether it needs you, with a short
//! reason. A yes shows as a flag on the chat's row and as an entry in Needs attention.
//!
//! - **On by default**, switched by the `taskTriage` preference (`{enabled, delayMinutes, prompt}`;
//!   only `enabled` and `delayMinutes` are read, 0 to 1,440 minutes, default 15). A check costs a
//!   few hundred tokens of the small model, at most [`MAX_PER_HOUR`] an hour and [`MIN_GAP_MS`]
//!   apart, so the spend is small and bounded; the Needs attention dialog says so and holds the
//!   switch.
//! - **Which chats** ([`decide`], one table-driven test): idle, stopped or interrupted; not
//!   archived, not a side chat, not muted; not already listed by the host (a pending request, an
//!   unread result or mark); not set aside (addressed or snoozed since the last activity); has a
//!   message from you; quiet for at least the delay and no longer than [`WINDOW_MS`] (so turning
//!   it on never walks old history); and not yet checked for this stretch of quiet. The stretch
//!   is the chat's activity stamp: the later of its last update and its last worker output, so
//!   new activity ends the stretch and clears the flag.
//! - **Limits:** nothing runs while the usage governor holds automated starts (wind-down or
//!   stop), one check at a time, one chat per pass, a pass per maintenance tick.
//! - **A check** is claimed first (the row is written as failed), so a crash or a timeout never
//!   replays it; the verdict is written only if the chat's stamp is still the one it was made
//!   for and triage is still on. A helper failure leaves the claim: no retry until the chat has
//!   new activity.
//! - The conversation is data: triage never sends a prompt to a chat and never changes read state.

use crate::attention::{live_needs, Needs};
use crate::runtime::{ChatRuntime, Shared};
use crate::types::*;
use rusqlite::{params, Transaction};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_DELAY_MINUTES: i64 = 15;
pub const MAX_DELAY_MINUTES: i64 = 1_440;
/// Chats quiet for longer than this are left alone.
pub const WINDOW_MS: i64 = 72 * 3_600_000;
pub const MAX_PER_HOUR: i64 = 12;
pub const MIN_GAP_MS: i64 = 30_000;
/// The helper makes up to two 50 s attempts; the host gives up a little after.
pub const HOST_TIMEOUT: Duration = Duration::from_secs(115);
/// The most of one turn that is sent (the end is kept; the helper trims the middle further).
pub const TURN_CHARS: usize = 6_000;
pub const MAX_REASON_CHARS: usize = 160;

/// One check at a time.
#[derive(Default)]
pub(crate) struct Gate {
    running: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub delay_ms: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_ms: DEFAULT_DELAY_MINUTES * 60_000,
        }
    }
}

/// The stored setting, leniently: missing or malformed parts fall back to the defaults.
pub fn settings(tx: &Transaction<'_>) -> Settings {
    let mut s = Settings::default();
    if let Some(Value::Object(m)) = helm_core::prefs::read(tx, "taskTriage") {
        if let Some(Value::Bool(b)) = m.get("enabled") {
            s.enabled = *b;
        }
        if let Some(d) = m.get("delayMinutes").and_then(Value::as_f64) {
            s.delay_ms = (d.clamp(0.0, MAX_DELAY_MINUTES as f64) as i64) * 60_000;
        }
    }
    s
}

/// What the decision needs to know about one chat.
#[derive(Clone, Debug)]
pub struct Cand {
    pub id: String,
    pub title: String,
    pub status: ChatStatus,
    pub archived: bool,
    pub side: bool,
    pub muted: bool,
    /// A pending approval, question or plan.
    pub waiting: bool,
    /// An unread completion or a manual unread mark.
    pub unread: bool,
    /// Addressed or snoozed since the last activity.
    pub quiet: bool,
    pub has_request: bool,
    /// The later of the chat's last update and its last worker output.
    pub stamp: i64,
    /// The stamp of the last check.
    pub checked_stamp: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Disabled,
    Held,
    Archived,
    Side,
    Busy,
    Muted,
    Waiting,
    Unread,
    SetAside,
    NoRequest,
    TooSoon,
    TooOld,
    AlreadyChecked,
}

/// Whether the chat should be checked now.
pub fn decide(c: &Cand, s: &Settings, held: bool, now: i64) -> Result<(), Skip> {
    let age = now - c.stamp;
    if !s.enabled {
        Err(Skip::Disabled)
    } else if held {
        Err(Skip::Held)
    } else if c.archived {
        Err(Skip::Archived)
    } else if c.side {
        Err(Skip::Side)
    } else if c.status.is_busy() {
        Err(Skip::Busy)
    } else if c.muted {
        Err(Skip::Muted)
    } else if c.waiting {
        Err(Skip::Waiting)
    } else if c.unread {
        Err(Skip::Unread)
    } else if c.quiet {
        Err(Skip::SetAside)
    } else if !c.has_request {
        Err(Skip::NoRequest)
    } else if age < s.delay_ms {
        Err(Skip::TooSoon)
    } else if age > WINDOW_MS {
        Err(Skip::TooOld)
    } else if c.checked_stamp == Some(c.stamp) {
        Err(Skip::AlreadyChecked)
    } else {
        Ok(())
    }
}

/// The chats worth looking at: those active within the window, least recently checked first.
fn candidates(tx: &Transaction<'_>, needs: &Needs, now: i64) -> rusqlite::Result<Vec<Cand>> {
    let mut st = tx.prepare(concat!(
        "SELECT c.id, c.title, c.status, c.archived, c.side_source_id IS NOT NULL, \
                EXISTS(SELECT 1 FROM chat_mutes m WHERE m.chat_id = c.id), \
                EXISTS(SELECT 1 FROM unread_marks u WHERE u.chat_id = c.id) \
                  OR EXISTS(SELECT 1 FROM chat_completions k WHERE k.chat_id = c.id AND k.read = 0), ",
        crate::quiet_sql!(),
        ", EXISTS(SELECT 1 FROM messages x WHERE x.chat_id = c.id AND x.role = 'user' AND x.active = 1 AND x.text <> ''), \
           MAX(c.updated_at, COALESCE(a.last_activity_at, 0)), t.stamp \
         FROM chats c LEFT JOIN chat_activity a ON a.chat_id = c.id \
                      LEFT JOIN chat_triage t ON t.chat_id = c.id \
         WHERE MAX(c.updated_at, COALESCE(a.last_activity_at, 0)) > ?1 - ?2 \
         ORDER BY COALESCE(t.checked_at, 0), MAX(c.updated_at, COALESCE(a.last_activity_at, 0)) DESC \
         LIMIT 200"
    ))?;
    let rows = st.query_map(params![now, WINDOW_MS], |r| {
        let id: String = r.get(0)?;
        Ok(Cand {
            waiting: needs.contains_key(&id),
            id,
            title: r.get(1)?,
            status: ChatStatus::parse(&r.get::<_, String>(2)?),
            archived: r.get(3)?,
            side: r.get(4)?,
            muted: r.get(5)?,
            unread: r.get(6)?,
            quiet: r.get(7)?,
            has_request: r.get(8)?,
            stamp: r.get(9)?,
            checked_stamp: r.get(10)?,
        })
    })?;
    rows.collect()
}

fn tail_chars(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        text.to_owned()
    } else {
        text.chars().skip(n - max).collect()
    }
}

/// The last four user and four assistant messages with text, oldest first.
fn turns(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut all: Vec<(i64, i64, &'static str, String)> = Vec::new();
    for role in ["user", "assistant"] {
        let mut st = tx.prepare(
            "SELECT timestamp, id, text FROM messages WHERE chat_id = ?1 AND role = ?2 AND active = 1
             AND text <> '' ORDER BY timestamp DESC, id DESC LIMIT 4",
        )?;
        let rows = st.query_map(params![chat_id, role], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (t, i, text) = row?;
            all.push((
                t,
                i,
                if role == "user" { "user" } else { "assistant" },
                text,
            ));
        }
    }
    all.sort_by_key(|(t, i, _, _)| (*t, *i));
    Ok(all
        .into_iter()
        .map(|(_, _, role, text)| json!({"role": role, "text": tail_chars(&text, TURN_CHARS)}))
        .collect())
}

/// One maintenance-tick pass: claim the next chat that is due, if any, and check it in the
/// background.
pub(crate) fn pass(sh: &Arc<Shared>) {
    let rt = ChatRuntime { sh: sh.clone() };
    let now = helm_core::now_ms();
    {
        let g = sh.triage.lock().unwrap_or_else(|p| p.into_inner());
        if g.running {
            return;
        }
    }
    let held = rt.slot_hold(now).is_some();
    let needs = live_needs(sh);
    let claimed = sh.db.write(|tx| {
        let s = settings(tx);
        if !s.enabled || held {
            return Ok(None);
        }
        let (recent, last): (i64, i64) = tx.query_row(
            "SELECT count(*), COALESCE(MAX(checked_at), 0) FROM chat_triage WHERE checked_at > ?1 - 3600000",
            [now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if recent >= MAX_PER_HOUR || now - last < MIN_GAP_MS {
            return Ok(None);
        }
        let Some(c) = candidates(tx, &needs, now)?
            .into_iter()
            .find(|c| decide(c, &s, held, now).is_ok())
        else {
            return Ok(None);
        };
        tx.execute(
            "INSERT INTO chat_triage(chat_id, stamp, checked_at, needs, reason, failed) VALUES (?1, ?2, ?3, 0, '', 1)
             ON CONFLICT(chat_id) DO UPDATE SET stamp = ?2, checked_at = ?3, needs = 0, reason = '', failed = 1",
            params![c.id, c.stamp, now],
        )?;
        let job = json!({
            "title": c.title,
            "status": c.status.as_str(),
            "idleMinutes": (now - c.stamp) / 60_000,
            "turns": turns(tx, &c.id)?,
        });
        Ok::<_, rusqlite::Error>(Some((c.id, c.stamp, job)))
    });
    let Ok(Some((chat, stamp, job))) = claimed else {
        return;
    };
    sh.triage.lock().unwrap_or_else(|p| p.into_inner()).running = true;
    let sh = sh.clone();
    sh.handle.clone().spawn(async move {
        let spawner = sh.spawner.clone();
        let out = tokio::time::timeout(
            HOST_TIMEOUT,
            tokio::task::spawn_blocking(move || spawner.run_triage(&job)),
        )
        .await;
        match out {
            Ok(Ok(Ok((needs, reason)))) => finish(&sh, &chat, stamp, needs, &reason),
            other => log::warn!(
                "chat {chat}: the idle check did not finish ({})",
                match other {
                    Ok(Ok(Err(e))) => e,
                    Err(_) => "timed out".into(),
                    _ => "the helper did not run".into(),
                }
            ),
        }
        sh.triage.lock().unwrap_or_else(|p| p.into_inner()).running = false;
    });
}

/// Write a verdict, unless the chat has moved on since the check was claimed or triage was
/// switched off meanwhile.
fn finish(sh: &Arc<Shared>, chat_id: &str, stamp: i64, needs: bool, reason: &str) {
    let reason: String = reason.chars().take(MAX_REASON_CHARS).collect();
    let wrote = sh
        .db
        .write(|tx| {
            if !settings(tx).enabled {
                return Ok(false);
            }
            let n = tx.execute(
                "UPDATE chat_triage SET needs = ?1, reason = ?2, failed = 0
                 WHERE chat_id = ?3 AND stamp = ?4
                   AND stamp = (SELECT MAX(c.updated_at, COALESCE(a.last_activity_at, 0)) FROM chats c
                                LEFT JOIN chat_activity a ON a.chat_id = c.id WHERE c.id = ?3)",
                params![needs && !reason.is_empty(), reason, chat_id, stamp],
            )?;
            Ok::<_, rusqlite::Error>(n > 0)
        })
        .unwrap_or(false);
    if wrote {
        crate::attention::publish(sh);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn cand() -> Cand {
        Cand {
            id: "c".into(),
            title: "t".into(),
            status: ChatStatus::Idle,
            archived: false,
            side: false,
            muted: false,
            waiting: false,
            unread: false,
            quiet: false,
            has_request: true,
            stamp: 1_000_000_000,
            checked_stamp: None,
        }
    }

    #[test]
    fn the_idle_pass_decision() {
        let s = Settings::default();
        let now = cand().stamp + 20 * MIN;
        type Edit = fn(&mut Cand);
        let cases: Vec<(&str, Edit, Result<(), Skip>)> = vec![
            ("idle for 20 minutes", |_| {}, Ok(())),
            ("stopped counts", |c| c.status = ChatStatus::Stopped, Ok(())),
            (
                "interrupted counts",
                |c| c.status = ChatStatus::Interrupted,
                Ok(()),
            ),
            (
                "running",
                |c| c.status = ChatStatus::Running,
                Err(Skip::Busy),
            ),
            (
                "starting",
                |c| c.status = ChatStatus::Starting,
                Err(Skip::Busy),
            ),
            ("archived", |c| c.archived = true, Err(Skip::Archived)),
            ("side chat", |c| c.side = true, Err(Skip::Side)),
            ("muted", |c| c.muted = true, Err(Skip::Muted)),
            (
                "a pending request is already listed",
                |c| c.waiting = true,
                Err(Skip::Waiting),
            ),
            (
                "an unread result is already listed",
                |c| c.unread = true,
                Err(Skip::Unread),
            ),
            (
                "addressed or snoozed",
                |c| c.quiet = true,
                Err(Skip::SetAside),
            ),
            (
                "nothing was ever asked",
                |c| c.has_request = false,
                Err(Skip::NoRequest),
            ),
            (
                "quiet for 14 minutes",
                |c| c.stamp += 6 * MIN,
                Err(Skip::TooSoon),
            ),
            (
                "quiet for exactly the delay",
                |c| c.stamp += 5 * MIN,
                Ok(()),
            ),
            (
                "quiet for four days",
                |c| c.stamp -= 4 * 24 * 60 * MIN,
                Err(Skip::TooOld),
            ),
            (
                "already checked for this stretch",
                |c| c.checked_stamp = Some(c.stamp),
                Err(Skip::AlreadyChecked),
            ),
            (
                "checked before the last activity",
                |c| c.checked_stamp = Some(c.stamp - MIN),
                Ok(()),
            ),
        ];
        for (name, edit, want) in cases {
            let mut c = cand();
            edit(&mut c);
            assert_eq!(decide(&c, &s, false, now), want, "{name}");
        }
        let c = cand();
        assert_eq!(
            decide(
                &c,
                &Settings {
                    enabled: false,
                    ..s
                },
                false,
                now
            ),
            Err(Skip::Disabled)
        );
        assert_eq!(
            decide(&c, &s, true, now),
            Err(Skip::Held),
            "the usage governor holds"
        );
        let eager = Settings { delay_ms: 0, ..s };
        assert_eq!(
            decide(&c, &eager, false, c.stamp),
            Ok(()),
            "a zero delay checks at once"
        );
    }
}
