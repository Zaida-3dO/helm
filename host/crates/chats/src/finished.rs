//! Finished chats archive themselves.
//!
//! A chat that has sent its hand-over (an `app_send_message` of kind `completed`) and has gone
//! quiet since is finished work. This sweep archives such a chat, through the same archive step
//! the `app_chat` archive action uses, once it has stood quiet for [`IDLE_GRACE_MS`]. It is on by
//! default and switched by the `archiveFinishedChats` preference.
//!
//! The chat that *receives* the hand-over (usually the one that started the work) is never
//! touched: only the sender is finished. A chat that waits on something else is left alone
//! ([`decide`], one table-driven test): a turn or a queued message in flight, a worker's
//! background job or running subagent, an open wait or a working chat of its own, a chat it
//! started that has not yet sent it a completed report, a restart or a stop check still owed to
//! it, a question or approval waiting on the person, a flag it raised for them, a message that
//! arrived after the hand-over (the person typing included), a draft in its composer, a pinned
//! chat, a side chat, and the chat the person has open. Each chat is archived once per
//! hand-over: a chat the person restores stays restored until it hands over again. A message
//! another chat sends to a chat the sweep archived restores it and is delivered; a chat the
//! person archived by hand stays archived.
//!
//! The first time the sweep runs it notes the time, and hand-overs from before that are never
//! looked at: turning it on does not archive what was handed over earlier. The decision and the
//! archive happen in one database write, which looks at the chat's facts again, so a message
//! that lands in between keeps the chat. What the sweep did is written into the chat as a plain
//! transcript line once it is archived. One chat is archived per maintenance tick.

use crate::attention::Needs;
use crate::runtime::{ChatRuntime, Shared};
use crate::types::*;
use helm_core::prefs::ARCHIVE_FINISHED_CHATS;
use rusqlite::{params, OptionalExtension, Transaction};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// How long a finished chat must have been quiet before it is archived.
pub const IDLE_GRACE_MS: i64 = 10 * 60_000;
/// Hand-overs older than this are not looked at (so turning the sweep on never walks old
/// history).
pub const WINDOW_MS: i64 = 72 * 3_600_000;
/// The most hand-overs read in one pass.
const MAX_HANDOVERS: i64 = 500;

/// The sweep's own table: which hand-over a chat was last archived for.
pub(crate) fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_finished_sweep (
             chat_id         TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             handed_over_at  INTEGER NOT NULL,
             archived_at     INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_finished_sweep_start (
             id            INTEGER PRIMARY KEY CHECK (id = 1),
             first_ran_at  INTEGER NOT NULL
         );",
    )
}

/// When the sweep first ran, if it has.
fn first_ran(tx: &Transaction<'_>) -> rusqlite::Result<Option<i64>> {
    tx.query_row(
        "SELECT first_ran_at FROM chat_finished_sweep_start WHERE id = 1",
        [],
        |r| r.get(0),
    )
    .optional()
}

/// Whether the sweep is switched on (on when the preference is missing).
pub fn enabled(tx: &Transaction<'_>) -> bool {
    helm_core::prefs::get_or(tx, ARCHIVE_FINISHED_CHATS, true)
}

/// What the decision needs to know about one chat that has handed over.
#[derive(Clone, Debug, Default)]
pub struct Cand {
    pub id: String,
    pub busy: bool,
    /// A turn is actually on its way: the status is starting, running or stopping. `busy` also
    /// counts a chat that stopped or was interrupted, which holds nothing of a real turn — for
    /// example when the local server went away, a GPU hold ended it, or the Stop button was hit.
    pub running: bool,
    pub archived: bool,
    pub side: bool,
    pub pinned: bool,
    /// A message is saved, queued or being handed over.
    pub queued: bool,
    /// A background job or a subagent is running in the chat's worker.
    pub background: bool,
    /// An open wait, or a chat it started that is working.
    pub helpers: bool,
    /// A chat it started (and that is still around) has not yet sent it a completed report.
    pub reports_owed: bool,
    /// A restart continuation or a stop check is still owed to it.
    pub continuing: bool,
    /// An approval, question or plan waits on the person.
    pub waiting: bool,
    /// The chat's agent flagged something for the person.
    pub flagged: bool,
    /// Something was sent to the chat after its hand-over.
    pub newer_message: bool,
    pub has_draft: bool,
    /// The person has this chat open.
    pub open: bool,
    /// Already archived by the sweep for this very hand-over.
    pub swept: bool,
    /// When the hand-over went out.
    pub handed_over_at: i64,
    /// The later of the chat's last update and its last worker output.
    pub stamp: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Archived,
    Side,
    Pinned,
    Busy,
    Queued,
    Background,
    Helpers,
    Reports,
    Continuing,
    Waiting,
    Flagged,
    NewerMessage,
    Draft,
    Open,
    Swept,
    TooSoon,
}

/// Whether the finished chat may be archived now.
pub fn decide(c: &Cand, now: i64) -> Result<(), Skip> {
    if c.archived {
        Err(Skip::Archived)
    } else if c.side {
        Err(Skip::Side)
    } else if c.pinned {
        Err(Skip::Pinned)
    } else if c.busy {
        Err(Skip::Busy)
    } else if c.queued {
        Err(Skip::Queued)
    } else if c.background {
        Err(Skip::Background)
    } else if c.helpers {
        Err(Skip::Helpers)
    } else if c.reports_owed {
        Err(Skip::Reports)
    } else if c.continuing {
        Err(Skip::Continuing)
    } else if c.waiting {
        Err(Skip::Waiting)
    } else if c.flagged {
        Err(Skip::Flagged)
    } else if c.newer_message {
        Err(Skip::NewerMessage)
    } else if c.has_draft {
        Err(Skip::Draft)
    } else if c.open {
        Err(Skip::Open)
    } else if c.swept {
        Err(Skip::Swept)
    } else if now - c.stamp.max(c.handed_over_at) < IDLE_GRACE_MS {
        Err(Skip::TooSoon)
    } else {
        Ok(())
    }
}

/// The chat that sent a message, read from the header Helm puts in front of every chat-to-chat
/// message: `[Message from another chat: "<title>" (<id>)]`.
fn sender_of(text: &str) -> Option<&str> {
    let first = text.lines().next()?;
    let inner = first
        .strip_prefix("[Message from another chat: ")?
        .strip_suffix(")]")?;
    let (_, id) = inner.rsplit_once(" (")?;
    (!id.is_empty() && !id.contains(char::is_whitespace)).then_some(id)
}

/// The chats that handed over within the window, each with its latest hand-over's time.
fn handovers(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<HashMap<String, i64>> {
    let mut st = tx.prepare(
        "SELECT s.text, s.created_at FROM submissions s
         JOIN submission_meta m ON m.submission_id = s.id
         WHERE m.kind = 'completed' AND m.origin = 'other_chat' AND s.created_at > ?1
         ORDER BY s.created_at DESC LIMIT ?2",
    )?;
    let since = (now - WINDOW_MS).max(first_ran(tx)?.unwrap_or(i64::MIN));
    let rows = st.query_map(params![since, MAX_HANDOVERS], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut out: HashMap<String, i64> = HashMap::new();
    for row in rows {
        let (text, at) = row?;
        if let Some(sender) = sender_of(&text) {
            let e = out.entry(sender.to_owned()).or_insert(at);
            *e = (*e).max(at);
        }
    }
    Ok(out)
}

/// The test "this message was sent by the chat `k`": the first line of a chat-to-chat message names
/// its sender and ends in `(<id>)]`. Only the first line counts, so a body cannot pass for it.
const SENT_BY_K: &str = "substr(substr(s.text, 1, instr(s.text || char(10), char(10)) - 1), -(length(k.id) + 3)) = '(' || k.id || ')]'";

/// Everything the database says about one chat that handed over at `at`; the live parts of the
/// decision (`needs`, `background`, `open`) come from the caller. `None` when the chat is gone.
fn candidate(
    tx: &Transaction<'_>,
    id: &str,
    at: i64,
    needs: &Needs,
    background: &HashSet<String>,
    open: Option<&str>,
) -> rusqlite::Result<Option<Cand>> {
    let mut st = tx.prepare_cached(&format!(
        "SELECT c.status, c.archived, c.side_source_id IS NOT NULL, c.pinned,
                EXISTS(SELECT 1 FROM submissions q WHERE q.chat_id = c.id
                       AND q.state IN ('saved', 'dispatching', 'accepted', 'delivered')),
                EXISTS(SELECT 1 FROM chat_flags f WHERE f.chat_id = c.id),
                EXISTS(SELECT 1 FROM submissions q WHERE q.chat_id = c.id AND q.created_at > ?2
                       AND q.state NOT IN ('withdrawn', 'rejected', 'superseded')),
                trim(c.draft) <> '',
                EXISTS(SELECT 1 FROM chat_finished_sweep w WHERE w.chat_id = c.id
                       AND w.handed_over_at >= ?2),
                MAX(c.updated_at, COALESCE(a.last_activity_at, 0)),
                EXISTS(SELECT 1 FROM chats k
                       WHERE k.archived = 0 AND (k.parent_chat_id = c.id OR k.created_by_chat = c.id)
                         AND NOT EXISTS(SELECT 1 FROM submissions s
                                        JOIN submission_meta m ON m.submission_id = s.id
                                        WHERE s.chat_id = c.id AND m.kind = 'completed'
                                          AND m.origin = 'other_chat' AND s.created_at >= k.created_at
                                          AND {SENT_BY_K})),
                EXISTS(SELECT 1 FROM restart_continuations r WHERE r.chat_id = c.id)
                  OR EXISTS(SELECT 1 FROM chat_stop_holds h WHERE h.chat_id = c.id)
         FROM chats c LEFT JOIN chat_activity a ON a.chat_id = c.id
         WHERE c.id = ?1"
    ))?;
    let facts = st
        .query_row(params![id, at], |r| {
            let status = ChatStatus::parse(&r.get::<_, String>(0)?);
            Ok(Cand {
                busy: status != ChatStatus::Idle,
                running: status.is_busy(),
                archived: r.get(1)?,
                side: r.get(2)?,
                pinned: r.get(3)?,
                queued: r.get(4)?,
                flagged: r.get(5)?,
                newer_message: r.get(6)?,
                has_draft: r.get(7)?,
                swept: r.get(8)?,
                stamp: r.get(9)?,
                reports_owed: r.get(10)?,
                continuing: r.get(11)?,
                ..Cand::default()
            })
        })
        .optional()?;
    let Some(mut c) = facts else {
        return Ok(None);
    };
    c.helpers = crate::triage::helpers_working(tx, id)?;
    c.waiting = needs.contains_key(id);
    c.background = background.contains(id);
    c.open = open == Some(id);
    c.handed_over_at = at;
    c.id = id.to_owned();
    Ok(Some(c))
}

/// The finished chats the sweep may archive now, each with the time of its hand-over, oldest
/// first. `needs` and `background` hold what only the running actors know (approvals, background
/// jobs, subagents); `open` is the chat the person has open, if any.
pub fn due(
    tx: &Transaction<'_>,
    needs: &Needs,
    background: &HashSet<String>,
    open: Option<&str>,
    now: i64,
) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut out: Vec<(i64, String)> = Vec::new();
    for (id, at) in handovers(tx, now)? {
        let Some(c) = candidate(tx, &id, at, needs, background, open)? else {
            continue;
        };
        if decide(&c, now).is_ok() {
            out.push((at, c.id));
        }
    }
    out.sort();
    Ok(out.into_iter().map(|(at, id)| (id, at)).collect())
}

/// The chat the person has open, from the saved selection.
fn open_chat(tx: &Transaction<'_>) -> Option<String> {
    helm_core::prefs::read(tx, "selection")?
        .get("chatId")?
        .as_str()
        .map(str::to_owned)
}

/// The chats whose worker holds a background job or a running subagent, which only the live
/// actors know.
fn background_chats(sh: &Shared) -> HashSet<String> {
    sh.actors
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, h)| {
            let live = h.live.lock().unwrap();
            live.thread.subagents_running > 0 || !live.thread.background_tasks.is_empty()
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// The line the chat gets before it is archived.
fn note() -> String {
    format!(
        "Helm archived this chat: it had sent its completed report and nothing had happened in it for {} minutes. Restore it from the archive if you need it again. Archiving finished chats can be switched off in Settings > Chat.",
        IDLE_GRACE_MS / 60_000
    )
}

/// One maintenance-tick pass: archive the longest-finished chat that is due, if any. The choice
/// is made again inside the write that archives, so a chat that has just started working, or
/// was sent something, since the pick is left alone. The claim (this hand-over is dealt with, so
/// it is not tried again) and the archive are one write.
pub(crate) fn pass(sh: &Arc<Shared>) {
    let now = helm_core::now_ms();
    let started = sh.db.snapshot(|tx| {
        Ok::<_, rusqlite::Error>(if enabled(tx) {
            Some(first_ran(tx)?.is_some())
        } else {
            None
        })
    });
    match started {
        Ok(Some(true)) => {}
        Ok(Some(false)) => {
            // The first run only notes the time: nothing handed over before it is archived.
            let _ = sh.db.write(|tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO chat_finished_sweep_start(id, first_ran_at) VALUES (1, ?1)",
                    [now],
                )
            });
            return;
        }
        _ => return,
    }
    let needs = crate::attention::live_needs(sh);
    let background = background_chats(sh);
    let picked = sh.db.snapshot(|tx| {
        let open = open_chat(tx);
        Ok::<_, rusqlite::Error>(
            due(tx, &needs, &background, open.as_deref(), now)?
                .into_iter()
                .next(),
        )
    });
    let Ok(Some((id, at))) = picked else {
        return;
    };
    let archived = sh.db.write(|tx| {
        let open = open_chat(tx);
        let Some(c) = candidate(tx, &id, at, &needs, &background, open.as_deref())? else {
            return Ok(false);
        };
        if decide(&c, now).is_err() {
            return Ok(false);
        }
        if !matches!(crate::archive::archive_in_tx(tx, &id, now)?, Ok(true)) {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO chat_finished_sweep(chat_id, handed_over_at, archived_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(chat_id) DO UPDATE SET handed_over_at = ?2, archived_at = ?3",
            params![id, at, now],
        )?;
        Ok::<_, rusqlite::Error>(true)
    });
    match archived {
        Ok(true) => {
            // The line goes in first, so it is in the transcript before the worker is let go.
            let rt = ChatRuntime { sh: sh.clone() };
            rt.add_context_row(&id, ContextKind::Notice, &note());
            rt.archived(&id);
            if let Some(seam) = sh.host.get() {
                if let Err(e) = crate::manage::changed(&seam.ctx, &id) {
                    log::warn!(
                        "finished chat {id} was archived, but its update was not sent: {}",
                        e.message
                    );
                }
            }
        }
        Ok(false) => {}
        Err(e) => log::warn!("finished chat {id} was not archived: {e}"),
    }
}

/// Whether a chat that has **not** handed over is nevertheless held open by something the person
/// should still see — asked when a channel closes one of its chats and wants to put it away. The
/// same facts [`candidate`] reads, with the two hand-over ones made impossible (no message can be
/// newer than `i64::MAX`, nothing was swept for a hand-over at that time); the first reason that
/// holds is returned, or `None` when the chat may go. An archived or missing chat holds nothing.
/// Two facts only the live actors know are not visible here and so never hold: a background job
/// in the worker, and a question or plan card open on screen (a pending **approval** is read from
/// the database, which covers the common case of a turn stopped at a prompt).
pub fn keeps_open(
    tx: &Transaction<'_>,
    id: &str,
    open: Option<&str>,
) -> rusqlite::Result<Option<&'static str>> {
    let Some(c) = candidate(tx, id, i64::MAX, &Needs::new(), &HashSet::new(), open)? else {
        return Ok(None);
    };
    if c.archived {
        return Ok(None);
    }
    if c.flagged {
        return Ok(Some("a flag its agent filed"));
    }
    if pending_approval(tx, id) {
        return Ok(Some("an approval waiting for the person"));
    }
    if c.queued {
        return Ok(Some("a message not delivered yet"));
    }
    if c.running {
        return Ok(Some("a turn still running"));
    }
    if c.helpers {
        return Ok(Some("an open wait or a chat it started, working"));
    }
    if c.reports_owed {
        return Ok(Some("a chat it started has not reported yet"));
    }
    if c.continuing {
        return Ok(Some("a continuation Helm still owes the chat"));
    }
    if c.has_draft {
        return Ok(Some("words left unsent in its composer"));
    }
    if c.pinned {
        return Ok(Some("the chat is pinned"));
    }
    if c.open {
        return Ok(Some("the person has the chat open"));
    }
    Ok(None)
}

/// Whether an approval for this chat is waiting on the person right now. The approval table
/// belongs to another part of the app: where it is not there, nothing waits.
fn pending_approval(tx: &Transaction<'_>, id: &str) -> bool {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM consent_requests
         WHERE chat_id = ?1 AND state = 'pending' AND expires_at > ?2)",
        params![id, helm_core::now_ms()],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// The narrow hold for rolling a growing chat over mid-conversation: only work in flight is lost
/// by archiving its chat right now — a turn still running, a message not delivered yet, or an
/// approval waiting for the person. Unlike [`keeps_open`] it ignores flags, pins, drafts, open
/// windows and child chats: those keep a chat in view, but none of them makes a roll lose
/// anything, and letting them hold the gate would leave a flagged watch chat growing forever.
pub fn holds_turn(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<bool> {
    let Some(c) = candidate(tx, id, i64::MAX, &Needs::new(), &HashSet::new(), None)? else {
        return Ok(false);
    };
    if c.archived {
        return Ok(false);
    }
    Ok(c.running || c.queued || pending_approval(tx, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;

    const NOW: i64 = 1_000_000_000;

    fn base() -> Cand {
        Cand {
            id: "k".into(),
            handed_over_at: NOW - 3 * IDLE_GRACE_MS,
            stamp: NOW - 3 * IDLE_GRACE_MS,
            ..Cand::default()
        }
    }

    #[test]
    fn a_finished_chat_is_archived_only_when_nothing_else_holds_it() {
        type Edit = fn(&mut Cand);
        let rows: [(&str, Edit, Result<(), Skip>); 19] = [
            ("quiet past the grace", |_| {}, Ok(())),
            ("archived", |c| c.archived = true, Err(Skip::Archived)),
            ("side chat", |c| c.side = true, Err(Skip::Side)),
            ("pinned", |c| c.pinned = true, Err(Skip::Pinned)),
            ("turn running", |c| c.busy = true, Err(Skip::Busy)),
            ("message queued", |c| c.queued = true, Err(Skip::Queued)),
            (
                "background job",
                |c| c.background = true,
                Err(Skip::Background),
            ),
            (
                "open wait or working child",
                |c| c.helpers = true,
                Err(Skip::Helpers),
            ),
            (
                "a chat it started has not reported",
                |c| c.reports_owed = true,
                Err(Skip::Reports),
            ),
            (
                "restart continuation or stop check owed",
                |c| c.continuing = true,
                Err(Skip::Continuing),
            ),
            ("approval waiting", |c| c.waiting = true, Err(Skip::Waiting)),
            (
                "flag for the person",
                |c| c.flagged = true,
                Err(Skip::Flagged),
            ),
            (
                "message after the hand-over",
                |c| c.newer_message = true,
                Err(Skip::NewerMessage),
            ),
            (
                "draft in the composer",
                |c| c.has_draft = true,
                Err(Skip::Draft),
            ),
            ("the person has it open", |c| c.open = true, Err(Skip::Open)),
            (
                "already swept for this hand-over",
                |c| c.swept = true,
                Err(Skip::Swept),
            ),
            (
                "worked on just now",
                |c| c.stamp = NOW - IDLE_GRACE_MS + 1,
                Err(Skip::TooSoon),
            ),
            (
                "handed over just now",
                |c| c.handed_over_at = NOW - 1,
                Err(Skip::TooSoon),
            ),
            (
                "exactly at the grace",
                |c| {
                    c.stamp = NOW - IDLE_GRACE_MS;
                    c.handed_over_at = NOW - IDLE_GRACE_MS;
                },
                Ok(()),
            ),
        ];
        for (label, edit, want) in rows {
            let mut c = base();
            edit(&mut c);
            assert_eq!(decide(&c, NOW), want, "{label}");
        }
    }

    #[test]
    fn the_sender_is_read_from_the_header_and_only_the_sender_is_due() {
        assert_eq!(
            sender_of("[Message from another chat: \"A title\" (kid-1)]\nThis message was sent"),
            Some("kid-1")
        );
        assert_eq!(sender_of("[Message from another chat: \"\" ()]"), None);
        assert_eq!(sender_of("just a typed message"), None);

        let (_d, db) = temp_db();
        let hand = |from: &str, to: &str, sid: &str, at: i64| {
            format!(
                "INSERT INTO submissions (id, chat_id, text, state, created_at)
                 VALUES ('{sid}', '{to}', '[Message from another chat: \"T\" ({from})]\nDone.', 'complete', {at});
                 INSERT INTO submission_meta (submission_id, origin, kind) VALUES ('{sid}', 'other_chat', 'completed');"
            )
        };
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects (id, name, created_at) VALUES ('p', 'P', 1);
                 INSERT INTO chats (id, project_id, title, status, created_at, updated_at) VALUES
                   ('boss', 'p', 'Boss', 'idle', 1, 1), ('kid', 'p', 'Kid', 'idle', 1, 1),
                   ('again', 'p', 'Again', 'idle', 1, 1), ('typed', 'p', 'Typed', 'idle', 1, 1),
                   ('rest', 'p', 'Restored', 'idle', 1, 1);",
            )?;
            // Four chats hand over to the boss; the boss itself never does.
            for (from, sid) in [
                ("kid", "h1"),
                ("again", "h2"),
                ("typed", "h3"),
                ("rest", "h4"),
            ] {
                tx.execute_batch(&hand(from, "boss", sid, NOW - 100 * 60_000))?;
            }
            // Someone writes to one after its hand-over; one was already swept for it; one
            // handed over a second time, later.
            tx.execute(
                "INSERT INTO submissions (id, chat_id, text, state, created_at)
                 VALUES ('t1', 'typed', 'one more thing', 'complete', ?1)",
                [NOW - 50 * 60_000],
            )?;
            tx.execute(
                "INSERT INTO chat_finished_sweep VALUES ('rest', ?1, ?2)",
                params![NOW - 100 * 60_000, NOW - 90 * 60_000],
            )?;
            tx.execute_batch(&hand("again", "boss", "h5", NOW - 60 * 60_000))?;
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let due_now = |open: Option<&str>| {
            db.snapshot(|tx| due(tx, &Needs::new(), &HashSet::new(), open, NOW))
                .unwrap()
        };
        let ids = |v: Vec<(String, i64)>| v.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        assert_eq!(
            ids(due_now(None)),
            ["kid", "again"],
            "oldest hand-over first"
        );
        assert_eq!(
            ids(due_now(Some("kid"))),
            ["again"],
            "the open chat is left"
        );
        // A hand-over older than the window is ignored.
        let far = NOW + WINDOW_MS + 1;
        assert!(db
            .snapshot(|tx| due(tx, &Needs::new(), &HashSet::new(), None, far))
            .unwrap()
            .is_empty());

        // A chat the kid started, and that has not reported, holds the kid; a restart
        // continuation owed to a chat holds that one.
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO chats (id, project_id, title, status, created_at, updated_at, created_by_chat)
                   VALUES ('sub', 'p', 'Sub', 'idle', 2, 2, 'kid');
                 INSERT INTO restart_continuations (chat_id, created_at) VALUES ('again', 1);",
            )
        })
        .unwrap();
        assert!(ids(due_now(None)).is_empty());
        // A message whose header names someone else, and only mentions the sub chat further
        // down, is not the sub chat's report.
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO submissions (id, chat_id, text, state, created_at)
                   VALUES ('fake', 'kid', '[Message from another chat: \"T\" (zzz)]\n(sub)]', 'complete', 5);
                 INSERT INTO submission_meta (submission_id, origin, kind) VALUES ('fake', 'other_chat', 'completed');",
            )
        })
        .unwrap();
        assert!(ids(due_now(None)).is_empty());
        // Its own report, from before the kid's hand-over, frees the kid (and, having handed
        // over itself, it is due too).
        db.write(|tx| tx.execute_batch(&hand("sub", "kid", "h6", NOW - 110 * 60_000)))
            .unwrap();
        assert_eq!(ids(due_now(None)), ["sub", "kid"]);

        // The first run's time bounds the window: nothing handed over before it is looked at.
        db.write(|tx| {
            tx.execute(
                "INSERT INTO chat_finished_sweep_start (id, first_ran_at) VALUES (1, ?1)",
                [NOW - 105 * 60_000],
            )
        })
        .unwrap();
        assert_eq!(ids(due_now(None)), ["kid"]);
    }

    #[test]
    fn keeps_open_names_the_first_hold_and_only_the_hand_over_facts_never_hold() {
        let (_d, db) = temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects (id, name, created_at) VALUES ('p', 'P', 1);
                 INSERT INTO chats (id, project_id, title, status, created_at, updated_at)
                   VALUES ('k', 'p', 'Keep?', 'idle', 1, 1);",
            )
        })
        .unwrap();
        let ask = |open: Option<&str>| {
            db.snapshot(|tx| keeps_open(tx, "k", open))
                .unwrap()
                .map(str::to_owned)
        };
        let edit = |sql: &str| {
            db.write(|tx| tx.execute_batch(sql)).unwrap();
        };
        // A long-ago queued message holds the chat even though it predates any hand-over: only
        // the hand-over timings themselves are ignored here.
        edit("INSERT INTO submissions (id, chat_id, text, state, created_at) VALUES ('s1', 'k', 'hi', 'saved', 1);");
        assert_eq!(
            ask(None).as_deref(),
            Some("a message not delivered yet"),
            "queued first after the flag check"
        );
        edit("INSERT INTO chat_flags VALUES ('k', 'q?', 'because', 'normal', 1);");
        assert_eq!(ask(None).as_deref(), Some("a flag its agent filed"));
        edit("DELETE FROM chat_flags WHERE chat_id = 'k';");
        assert_eq!(ask(None).as_deref(), Some("a message not delivered yet"));
        edit("DELETE FROM submissions WHERE id = 's1';");
        assert_eq!(ask(None), None, "quiet and idle: nothing holds it");
        edit("UPDATE chats SET status = 'running' WHERE id = 'k';");
        assert_eq!(ask(None).as_deref(), Some("a turn still running"));
        edit("UPDATE chats SET status = 'idle', draft = 'half a thought' WHERE id = 'k';");
        assert_eq!(
            ask(None).as_deref(),
            Some("words left unsent in its composer")
        );
        edit("UPDATE chats SET draft = '', pinned = 1 WHERE id = 'k';");
        assert_eq!(ask(None).as_deref(), Some("the chat is pinned"));
        edit("UPDATE chats SET pinned = 0 WHERE id = 'k';");
        // An approval waits only while it is pending and unexpired (the table belongs to another
        // part of the app; where it is absent, nothing waits).
        edit(
            "CREATE TABLE consent_requests (chat_id TEXT, state TEXT, expires_at INTEGER);
             INSERT INTO consent_requests VALUES ('k', 'pending', 0), ('k', 'allowed', 9999999999999);",
        );
        assert_eq!(ask(None), None, "expired or answered: no hold");
        edit("INSERT INTO consent_requests VALUES ('k', 'pending', 9999999999999);");
        assert_eq!(
            ask(None).as_deref(),
            Some("an approval waiting for the person")
        );
        // The window-open hold comes from the caller's `open`, not the database. An archived or
        // missing chat holds nothing whatever else is true of it.
        edit("UPDATE chats SET archived = 1 WHERE id = 'k';");
        assert_eq!(ask(None), None);
        edit("UPDATE chats SET archived = 0 WHERE id = 'k';");
        // The approval added above still waits: clear it so this check looks at the window hold,
        // which comes later in the order than an approval does.
        edit("DELETE FROM consent_requests;");
        assert_eq!(
            ask(Some("k")).as_deref(),
            Some("the person has the chat open")
        );
        assert_eq!(
            db.snapshot(|tx| keeps_open(tx, "nope", None)).unwrap(),
            None
        );
        // The narrow roll gate beside it: view-level holds (a flag, a draft) never stop a chat
        // rolling over; only work in flight does.
        let rolls = || db.snapshot(|tx| holds_turn(tx, "k")).unwrap();
        edit("INSERT INTO chat_flags VALUES ('k', 'watch', 'attention', 'normal', 1); UPDATE chats SET draft = 'half a thought' WHERE id = 'k';");
        assert!(!rolls(), "a flag and a draft do not stop a roll");
        edit("DELETE FROM chat_flags WHERE chat_id = 'k'; UPDATE chats SET status = 'running' WHERE id = 'k';");
        assert!(rolls(), "a running turn does");
        edit("UPDATE chats SET status = 'idle', draft = ''; INSERT INTO submissions (id, chat_id, text, state, created_at) VALUES ('s2', 'k', 'hi', 'saved', 1);");
        assert!(rolls(), "an undelivered message does");
        edit("UPDATE submissions SET state = 'withdrawn' WHERE id = 's2'; INSERT INTO consent_requests VALUES ('k', 'pending', 9999999999999);");
        assert!(rolls(), "an approval waiting for the person does");
        edit("DELETE FROM consent_requests WHERE chat_id = 'k'; UPDATE chats SET archived = 1 WHERE id = 'k';");
        assert!(!rolls(), "an archived chat holds nothing");
    }
}
