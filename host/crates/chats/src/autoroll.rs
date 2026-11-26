//! When a chat rolls itself over: the checks behind the automatic roll over.
//!
//! At the end of a turn the chat's real context size (the figure the context line and the
//! large-chat bar show, [`crate::turn_usage::context_reported_at`]) is compared with the
//! `rolloverTokens` setting. The roll over then runs through the same flow as the button and
//! `app_roll_over`, hand-off included ([`crate::rollover::on_turn_end`] starts it); this module
//! only decides whether it should, and words the note the person is given.
//!
//! The chat is left alone when the setting is 0, when it is a side chat, archived or not idle,
//! when a message of it is saved or on its way, and when it is a roll-over successor that has
//! not yet had a turn of its own after the one that carried the hand-off in: two roll overs
//! never follow each other without work in between, however low the setting is. A roll over
//! already running for the chat, and one already started for it this run, are checked by the
//! caller.

use crate::rollover::DEFAULT_ROLLOVER_TOKENS;
use helm_core::prefs::ROLLOVER_TOKENS;
use rusqlite::{OptionalExtension, Transaction};

/// The context size the chat has reached, when it is over the line and nothing holds the roll
/// over back; `None` otherwise. The size counts as over at exactly the setting.
pub(crate) fn over_the_line(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Option<u64>> {
    let threshold = helm_core::prefs::get_or(tx, ROLLOVER_TOKENS, DEFAULT_ROLLOVER_TOKENS);
    if threshold <= 0 {
        return Ok(None);
    }
    let can_roll: bool = tx
        .query_row(
            "SELECT c.archived = 0 AND c.side_source_id IS NULL AND c.status = 'idle'
                    AND NOT EXISTS(SELECT 1 FROM submissions s WHERE s.chat_id = c.id
                        AND s.state IN ('saved', 'dispatching', 'accepted', 'delivered'))
                    AND (NOT EXISTS(SELECT 1 FROM chat_succession x WHERE x.chat_id = c.id)
                         OR (SELECT COUNT(*) FROM submissions s WHERE s.chat_id = c.id
                             AND s.state = 'complete') >= 2)
             FROM chats c WHERE c.id = ?1",
            [chat_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if !can_roll {
        return Ok(None);
    }
    Ok(crate::turn_usage::context_reported_at(tx, chat_id)
        .map(|(n, _)| n)
        .filter(|n| *n >= threshold as u64))
}

/// The line the old chat gets when Helm starts the roll over by itself.
pub(crate) fn note(tokens: u64) -> String {
    format!(
        "Helm is rolling this chat over: its context reached {} tokens. The agent is writing a hand-off and the work carries on in a new chat; this one will be archived.",
        tokens
    )
}
