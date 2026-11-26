//! Finished chats archive themselves.
//!
//! A chat that has sent its hand-over (an `app_send_message` of kind `completed`) and has gone
//! quiet since is finished work. This sweep archives such a chat, through the same op the
//! `app_chat` archive action uses, once it has stood quiet for [`IDLE_GRACE_MS`]. It is on by
//! default and switched by the `archiveFinishedChats` preference.
//!
//! The chat that *receives* the hand-over (usually the one that started the work) is never
//! touched: only the sender is finished. A chat that waits on something else is left alone
//! ([`decide`], one table-driven test): a turn or a queued message in flight, a worker's
//! background job or running subagent, an open wait or a working chat of its own, a question or
//! approval waiting on the person, a flag it raised for them, a message that arrived after the
//! hand-over (the person typing included), a draft in its composer, a pinned chat, a side chat,
//! and the chat the person has open. Each chat is archived once per hand-over: a chat the person
//! restores stays restored until it hands over again.
//!
//! What the sweep did is written into the chat as a plain transcript line before it archives.
//! One chat is archived per maintenance tick.

use crate::attention::Needs;
use crate::runtime::{ChatRuntime, Shared};
use crate::types::*;
use helm_core::prefs::ARCHIVE_FINISHED_CHATS;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::json;
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
         );",
    )
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
    pub archived: bool,
    pub side: bool,
    pub pinned: bool,
    /// A message is saved, queued or being handed over.
    pub queued: bool,
    /// A background job or a subagent is running in the chat's worker.
    pub background: bool,
    /// An open wait, or a chat it started that is working.
    pub helpers: bool,
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
    let rows = st.query_map(params![now - WINDOW_MS, MAX_HANDOVERS], |r| {
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
    let mut st = tx.prepare(
        "SELECT c.status, c.archived, c.side_source_id IS NOT NULL, c.pinned,
                EXISTS(SELECT 1 FROM submissions q WHERE q.chat_id = c.id
                       AND q.state IN ('saved', 'dispatching', 'accepted', 'delivered')),
                EXISTS(SELECT 1 FROM chat_flags f WHERE f.chat_id = c.id),
                EXISTS(SELECT 1 FROM submissions q WHERE q.chat_id = c.id AND q.created_at > ?2
                       AND q.state NOT IN ('withdrawn', 'rejected', 'superseded')),
                trim(c.draft) <> '',
                EXISTS(SELECT 1 FROM chat_finished_sweep w WHERE w.chat_id = c.id
                       AND w.handed_over_at >= ?2),
                MAX(c.updated_at, COALESCE(a.last_activity_at, 0))
         FROM chats c LEFT JOIN chat_activity a ON a.chat_id = c.id
         WHERE c.id = ?1",
    )?;
    for (id, at) in handovers(tx, now)? {
        let facts = st
            .query_row(params![id, at], |r| {
                Ok(Cand {
                    busy: ChatStatus::parse(&r.get::<_, String>(0)?) != ChatStatus::Idle,
                    archived: r.get(1)?,
                    side: r.get(2)?,
                    pinned: r.get(3)?,
                    queued: r.get(4)?,
                    flagged: r.get(5)?,
                    newer_message: r.get(6)?,
                    has_draft: r.get(7)?,
                    swept: r.get(8)?,
                    stamp: r.get(9)?,
                    ..Cand::default()
                })
            })
            .optional()?;
        let Some(mut c) = facts else { continue };
        c.helpers = crate::triage::helpers_working(tx, &id)?;
        c.waiting = needs.contains_key(&id);
        c.background = background.contains(&id);
        c.open = open == Some(id.as_str());
        c.handed_over_at = at;
        c.id = id;
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

/// One maintenance-tick pass: archive the longest-finished chat that is due, if any. The chat is
/// marked as swept for its hand-over first, so a failed archive is not tried again every tick.
pub(crate) fn pass(sh: &Arc<Shared>) {
    let Some(seam) = sh.host.get().cloned() else {
        return;
    };
    let now = helm_core::now_ms();
    let needs = crate::attention::live_needs(sh);
    let background = background_chats(sh);
    let picked = sh.db.snapshot(|tx| {
        if !enabled(tx) {
            return Ok(None);
        }
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
    let claimed = sh.db.write(|tx| {
        tx.execute(
            "INSERT INTO chat_finished_sweep(chat_id, handed_over_at, archived_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(chat_id) DO UPDATE SET handed_over_at = ?2, archived_at = ?3",
            params![id, at, now],
        )
    });
    if claimed.is_err() {
        return;
    }
    ChatRuntime { sh: sh.clone() }.add_context_row(&id, ContextKind::Notice, &note());
    if let Err(e) = crate::manage::chat_archive(&seam.ctx, json!({"chatId": id})) {
        log::warn!("finished chat {id} was not archived: {}", e.message);
    }
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
        let rows: [(&str, Edit, Result<(), Skip>); 17] = [
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
    }
}
