//! Muted chats, as the notice paths and the host tools see them.
//!
//! A muted chat raises nothing for the user: no flag, badge, toast, sound, taskbar flash or
//! Telegram message. What it holds (a flag, a question, an approval, a plan) is kept and listed in
//! the attention inbox's Muted group, so a muted crew never waits on something nobody can find.
//!
//! - `chat_mutes` says who is muted. The user-only `chat_mute` op writes it from the menus; the
//!   `muted` argument of `app_create_chat` and `app_update_chat` writes it for an agent.
//! - `chats.created_by_chat` records which chat made a chat with `app_create_chat`. Only that chat,
//!   besides the user, may mute or unmute it (`parent_chat_id` is no proof: it can be moved).
//! - Every notice path asks [`is_muted`], so there is one place to look.

use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// `chats` gained the creator column after the first table was written; older databases get it here.
pub(crate) fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let has: bool = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('chats') WHERE name = 'created_by_chat'",
        [],
        |r| r.get::<_, i64>(0).map(|n| n > 0),
    )?;
    if !has {
        tx.execute("ALTER TABLE chats ADD COLUMN created_by_chat TEXT", [])?;
    }
    Ok(())
}

/// Whether the chat is muted.
pub fn is_muted(tx: &Connection, chat_id: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_mutes WHERE chat_id = ?1)",
        [chat_id],
        |r| r.get(0),
    )
}

/// Mute or unmute the chat. The caller has checked it exists.
pub fn set_muted(tx: &Connection, chat_id: &str, muted: bool, now: i64) -> rusqlite::Result<()> {
    if muted {
        tx.execute(
            "INSERT OR IGNORE INTO chat_mutes(chat_id, muted_at) VALUES (?1, ?2)",
            params![chat_id, now],
        )?;
    } else {
        tx.execute("DELETE FROM chat_mutes WHERE chat_id = ?1", [chat_id])?;
    }
    Ok(())
}

/// The chat that made this chat with `app_create_chat`, if any.
pub fn created_by(tx: &Connection, chat_id: &str) -> rusqlite::Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT created_by_chat FROM chats WHERE id = ?1",
            [chat_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Record the chat that made this chat.
pub fn set_created_by(tx: &Connection, chat_id: &str, creator: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE chats SET created_by_chat = ?2 WHERE id = ?1",
        params![chat_id, creator],
    )?;
    Ok(())
}

/// A successor chat (roll over) keeps what its predecessor was: muted or not, and made by the same
/// chat, so the crew that managed the old chat still manages the new one.
pub fn inherit(tx: &Connection, from: &str, to: &str, now: i64) -> rusqlite::Result<()> {
    if is_muted(tx, from)? {
        set_muted(tx, to, true, now)?;
    }
    tx.execute(
        "UPDATE chats SET created_by_chat = (SELECT created_by_chat FROM chats WHERE id = ?1)
         WHERE id = ?2",
        params![from, to],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;
    use crate::store;

    #[test]
    fn mute_state_creator_and_inheritance() {
        let (_d, db) = temp_db();
        let mk = || {
            db.write(|tx| store::create_chat(tx, "C:/w/proj", None, None, 1))
                .unwrap()
        };
        let (a, b, c) = (mk(), mk(), mk());
        db.write(|tx| {
            assert!(!is_muted(tx, &a)?);
            assert_eq!(created_by(tx, &a)?, None);
            set_created_by(tx, &a, &b)?;
            set_muted(tx, &a, true, 5)?;
            set_muted(tx, &a, true, 6)?;
            assert!(is_muted(tx, &a)? && !is_muted(tx, &b)?);
            assert_eq!(created_by(tx, &a)?, Some(b.clone()));
            assert_eq!(created_by(tx, "missing")?, None);
            inherit(tx, &a, &c, 7)?;
            assert!(is_muted(tx, &c)?);
            assert_eq!(created_by(tx, &c)?, Some(b.clone()));
            set_muted(tx, &a, false, 8)?;
            assert!(!is_muted(tx, &a)?);
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
    }
}
