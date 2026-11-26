//! A child chat that ended its turn without reporting.
//!
//! The silent-chat note (see `notify_parent_silent` in the actor) covers a child whose turn is
//! open and quiet. It cannot cover a child that *ends* its turn early: that chat is idle, not
//! silent, and an idle parent was never going to be told.
//!
//! This module answers the one question the actor asks when a child's turn ends: did the turn
//! end unfinished, and has the parent not heard from the child since the child was last written
//! to? A turn counts as unfinished when it has no closing reply at all (the turn's last step was
//! a tool, or its text is blank). A child that wrote its parent a message after the message it
//! last received is reporting, whatever its last words were.

use rusqlite::{OptionalExtension, Transaction};

/// Why a child's turn counts as ended without reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfinished {
    /// The turn has no closing reply: its last step was a tool call, or its text is blank.
    NoReport,
}

/// Whether the child has written to `parent` since the last message the child received.
fn child_reported(tx: &Transaction<'_>, parent: &str, child: &str) -> rusqlite::Result<bool> {
    // The last message the child received, whoever sent it.
    let last_in: i64 = tx.query_row(
        "SELECT COALESCE(MAX(created_at), 0) FROM submissions WHERE chat_id = ?1",
        [child],
        |r| r.get(0),
    )?;
    // A message from the child sits in the parent's inbox with the child's id in its frame.
    tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM submissions s JOIN submission_meta m ON m.submission_id = s.id
             WHERE s.chat_id = ?1 AND m.origin = 'other_chat' AND s.created_at >= ?2
               AND instr(s.text, '(' || ?3 || ')') > 0)",
        rusqlite::params![parent, last_in, child],
        |r| r.get(0),
    )
}

/// Whether the wait `parent` declared on `child` has nothing to add when the child's turn ends:
/// the child already wrote to the parent, or the turn ended unfinished and the parent is told so
/// by [`unreported_turn`]'s note. Either way the parent has one message about it, not two.
pub fn watch_redundant(tx: &Transaction<'_>, parent: &str, child: &str) -> rusqlite::Result<bool> {
    if child_reported(tx, parent, child)? {
        return Ok(true);
    }
    Ok(unreported_turn(tx, child)?.is_some_and(|(p, _)| p == parent))
}

/// The parent to tell and why, when a child's just-ended turn is unfinished and unreported.
/// `None` for a child with no live parent, one that has reported, or a turn that closed
/// normally.
pub fn unreported_turn(
    tx: &Transaction<'_>,
    child: &str,
) -> rusqlite::Result<Option<(String, Unfinished)>> {
    let row: Option<(Option<String>, bool)> = tx
        .query_row(
            "SELECT parent_chat_id, archived FROM chats WHERE id = ?1",
            [child],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(parent), false)) = row else {
        return Ok(None);
    };
    let parent_live: bool = tx
        .query_row(
            "SELECT archived = 0 FROM chats WHERE id = ?1",
            [&parent],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if !parent_live {
        return Ok(None);
    }
    if child_reported(tx, &parent, child)? {
        return Ok(None);
    }
    Ok(turn_why(tx, child)?.map(|why| (parent, why)))
}

/// Why the chat's just-ended turn closed, read from the turn shape alone: no closing reply, or
/// `None` when it closed with words. It says nothing about parents or reports, so a chat that
/// has none can ask it too.
pub fn turn_why(tx: &Transaction<'_>, chat: &str) -> rusqlite::Result<Option<Unfinished>> {
    Ok(match last_step(tx, chat)? {
        Some((role, text)) if role == "assistant" && !text.trim().is_empty() => None,
        _ => Some(Unfinished::NoReport),
    })
}

/// The start of the chat's latest run, 0 when it has never had one.
fn run_start(tx: &Transaction<'_>, chat: &str) -> rusqlite::Result<i64> {
    tx.query_row(
        "SELECT started_at FROM runs WHERE chat_id = ?1 ORDER BY started_at DESC, rowid DESC
         LIMIT 1",
        [chat],
        |r| r.get(0),
    )
    .optional()
    .map(|v| v.unwrap_or(0))
}

/// The turn's newest step that is not a person's message or a note, of the chat's latest run.
fn last_step(tx: &Transaction<'_>, chat: &str) -> rusqlite::Result<Option<(String, String)>> {
    let run_start = run_start(tx, chat)?;
    tx.query_row(
        "SELECT role, text FROM messages
         WHERE chat_id = ?1 AND active = 1 AND timestamp >= ?2
           AND role NOT IN ('user', 'context')
         ORDER BY id DESC LIMIT 1",
        rusqlite::params![chat, run_start],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// The note the parent gets.
pub fn note_text(title: &str, why: Unfinished) -> String {
    let what = match why {
        Unfinished::NoReport => "its last step has no closing reply",
    };
    format!(
        "Helm noticed: your subchat \"{title}\" ended its turn without reporting to you: {what}, \
         and it has sent you nothing since it was last written to. It is idle now. Read it, \
         then send it a message to carry on, or take the work back."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child under a parent, a turn of replies, and what the parent was sent, as one table.
    #[test]
    fn a_turn_is_unreported_only_when_unfinished_and_unheard_from() {
        const BLANK: &str = "  ";
        // (name, last step role and text, parent heard from the child after its last message,
        // parent archived, want)
        type Row = (
            &'static str,
            &'static str,
            &'static str,
            bool,
            bool,
            Option<Unfinished>,
        );
        let rows: [Row; 7] = [
            (
                "blank closing reply",
                "assistant",
                "  ",
                false,
                false,
                Some(Unfinished::NoReport),
            ),
            (
                "ended on a tool step",
                "toolResult",
                "ok",
                false,
                false,
                Some(Unfinished::NoReport),
            ),
            ("closed normally", "assistant", "Done.", false, false, None),
            (
                "reported before ending",
                "assistant",
                BLANK,
                true,
                false,
                None,
            ),
            ("parent archived", "assistant", BLANK, false, true, None),
            (
                "no turn rows",
                "none",
                "",
                false,
                false,
                Some(Unfinished::NoReport),
            ),
            ("reported, closed", "assistant", "Done.", true, false, None),
        ];
        for (name, role, text, heard, parent_archived, want) in rows {
            let (_d, db) = crate::schema::tests::temp_db();
            db.write(|tx| {
                tx.execute_batch(
                    "INSERT INTO projects (id, name, created_at) VALUES ('p', 'P', 1);
                     INSERT INTO chats (id, project_id, title, status, created_at, updated_at)
                     VALUES ('par', 'p', 'Parent', 'idle', 1, 1);
                     INSERT INTO chats (id, project_id, title, status, parent_chat_id, created_at,
                                        updated_at)
                     VALUES ('kid', 'p', 'Kid', 'idle', 'par', 1, 1);
                     INSERT INTO runs (id, chat_id, status, started_at) VALUES ('r', 'kid', 'complete', 100);
                     INSERT INTO submissions (id, chat_id, text, state, created_at)
                     VALUES ('in', 'kid', 'do it', 'complete', 100);",
                )?;
                if role != "none" {
                    tx.execute(
                        "INSERT INTO messages (chat_id, source_key, role, text, timestamp)
                         VALUES ('kid', 'm1', ?1, ?2, 200)",
                        rusqlite::params![role, text],
                    )?;
                }
                if heard {
                    tx.execute_batch(
                        "INSERT INTO submissions (id, chat_id, text, state, created_at)
                         VALUES ('rep', 'par', '[Message from another chat: \"Kid\" (kid)]\nbody', 'complete', 150);
                         INSERT INTO submission_meta (submission_id, origin) VALUES ('rep', 'other_chat');",
                    )?;
                }
                if parent_archived {
                    tx.execute("UPDATE chats SET archived = 1 WHERE id = 'par'", [])?;
                }
                Ok::<_, rusqlite::Error>(())
            })
            .unwrap();
            let got = db
                .snapshot(|tx| unreported_turn(tx, "kid"))
                .unwrap()
                .map(|(p, w)| {
                    assert_eq!(p, "par");
                    w
                });
            assert_eq!(got, want, "{name}");
            // A watch on that child adds nothing when the parent already has one message about
            // the turn (the child's own report, or the note that it ended unfinished).
            let redundant = db.snapshot(|tx| watch_redundant(tx, "par", "kid")).unwrap();
            assert_eq!(redundant, heard || want.is_some(), "{name}: watch");
        }
        // A chat with no parent has nobody to tell.
        let (_d, db) = crate::schema::tests::temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects (id, name, created_at) VALUES ('p', 'P', 1);
                 INSERT INTO chats (id, project_id, title, status, created_at, updated_at)
                 VALUES ('solo', 'p', 'Solo', 'idle', 1, 1);",
            )
        })
        .unwrap();
        assert_eq!(db.snapshot(|tx| unreported_turn(tx, "solo")).unwrap(), None);
    }

    /// The turn-shape question stands on its own, so a chat with no parent can ask it too.
    #[test]
    fn a_parentless_chat_can_ask_why_its_turn_closed() {
        // (last step role and text, why the turn closed)
        let rows: [(&str, &str, Option<Unfinished>); 4] = [
            ("assistant", "Done: tests pass, twelve of twelve.", None),
            ("assistant", "  ", Some(Unfinished::NoReport)),
            ("toolResult", "ok", Some(Unfinished::NoReport)),
            ("none", "", Some(Unfinished::NoReport)),
        ];
        for (role, text, want_why) in rows {
            let (_d, db) = crate::schema::tests::temp_db();
            db.write(|tx| {
                tx.execute_batch(
                    "INSERT INTO projects (id, name, created_at) VALUES ('p', 'P', 1);
                     INSERT INTO chats (id, project_id, title, status, created_at, updated_at)
                     VALUES ('solo', 'p', 'Solo', 'idle', 1, 1);
                     INSERT INTO runs (id, chat_id, status, started_at)
                     VALUES ('r', 'solo', 'complete', 100);",
                )?;
                if role != "none" {
                    tx.execute(
                        "INSERT INTO messages (chat_id, source_key, role, text, timestamp)
                         VALUES ('solo', 'm1', ?1, ?2, 200)",
                        rusqlite::params![role, text],
                    )?;
                }
                Ok::<_, rusqlite::Error>(())
            })
            .unwrap();
            let label = format!("{role}: {text}");
            assert_eq!(
                db.snapshot(|tx| turn_why(tx, "solo")).unwrap(),
                want_why,
                "{label}"
            );
        }
    }

    #[test]
    fn the_note_names_the_chat_and_the_reason() {
        let t = note_text("Doc builder", Unfinished::NoReport);
        assert!(t.contains("\"Doc builder\"") && t.contains("no closing reply"));
    }
}
