//! The `item_chat_links` table and the rules on it.
//!
//! `(item_id, chat_id)` is the key. An item has at most one main chat; the first chat linked to an
//! item is its main chat, and removing the main chat promotes the newest remaining one. Item ids
//! are kept at the longest length ever given, so a prefix link and a full-id link of one item are
//! one group ([`crate::ids`]).

use crate::ids;
use crate::types::*;
use rusqlite::{params, OptionalExtension, Transaction};
use std::collections::BTreeSet;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS item_chat_links (
             item_id    TEXT NOT NULL,
             chat_id    TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             is_main    INTEGER NOT NULL DEFAULT 0,
             linked_by  TEXT NOT NULL CHECK (linked_by IN ('agent', 'user')),
             linked_at  INTEGER NOT NULL,
             title      TEXT,
             PRIMARY KEY (item_id, chat_id)
         );
         CREATE INDEX IF NOT EXISTS item_chat_links_chat ON item_chat_links(chat_id);",
    )?;
    // The title arrived after the table did: an older profile gets the column here, with no
    // title until an agent next names the item.
    let present: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('item_chat_links') WHERE name = 'title')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        tx.execute_batch("ALTER TABLE item_chat_links ADD COLUMN title TEXT")?;
    }
    Ok(())
}

/// Longest stored title, in characters. The title comes from the board, so it is cut, not trusted.
pub const MAX_TITLE: usize = 160;

/// A title as stored: control characters and runs of spaces collapsed, trimmed, capped. `None`
/// when nothing is left.
pub fn clean_title(raw: &str) -> Option<String> {
    let flat = raw
        .split(|c: char| c.is_control() || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if flat.is_empty() {
        return None;
    }
    Some(flat.chars().take(MAX_TITLE).collect())
}

/// Remember the item's title (on every chat linked to it, so any of them can show it). A repeat
/// with a new title replaces the old one, which is how a renamed item catches up. An empty title
/// changes nothing.
pub fn set_title(tx: &Transaction<'_>, item_id: &str, raw_title: &str) -> rusqlite::Result<()> {
    let Some(title) = clean_title(raw_title) else {
        return Ok(());
    };
    tx.execute(
        "UPDATE item_chat_links SET title = ?2 WHERE item_id = ?1",
        params![item_id, title],
    )?;
    Ok(())
}

/// The newest stored title among an item's links.
fn stored_title(tx: &Transaction<'_>, id: &str) -> Option<String> {
    tx.query_row(
        "SELECT title FROM item_chat_links WHERE item_id = ?1 AND title IS NOT NULL
         ORDER BY linked_at DESC LIMIT 1",
        [id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

fn by_str(b: LinkedBy) -> &'static str {
    match b {
        LinkedBy::Agent => "agent",
        LinkedBy::User => "user",
    }
}

fn by_of(s: &str) -> LinkedBy {
    if s == "user" {
        LinkedBy::User
    } else {
        LinkedBy::Agent
    }
}

/// The stored ids that name the same item as `id`.
fn group_ids(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Vec<String>> {
    let mut st = tx.prepare("SELECT DISTINCT item_id FROM item_chat_links")?;
    let all = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(all.into_iter().filter(|o| ids::same_item(o, id)).collect())
}

/// The board project's title for an item the board mirror holds (a task is not in the mirror;
/// its title is the one stored with the link).
fn title_of(tx: &Transaction<'_>, id: &str) -> Option<String> {
    let has: bool = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'board_projects'",
            [],
            |_| Ok(true),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(false);
    if !has {
        return None;
    }
    // The board's ids carry hyphens; compare without them.
    tx.query_row(
        "SELECT title FROM board_projects
         WHERE replace(lower(board_project_id), '-', '') LIKE ?1 || '%' ESCAPE '\\'
            OR ?1 LIKE replace(lower(board_project_id), '-', '') || '%' ESCAPE '\\'
         ORDER BY stale ASC LIMIT 1",
        [id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub item_id: String,
    pub main: bool,
    /// Chats now linked to the item.
    pub chats: usize,
}

/// Link `chat_id` to the item. `main`: make it the item's main chat (the first link is main
/// anyway). Re-linking keeps `linked_at` and whoever linked it first, so an agent repeating the
/// call changes nothing.
pub fn link(
    tx: &Transaction<'_>,
    chat_id: &str,
    raw_item: &str,
    main: bool,
    by: LinkedBy,
    now: i64,
) -> Result<Linked, String> {
    let id = ids::normalise(raw_item)
        .ok_or("itemId must be an item's full id or the first 8 or more hex characters of it.")?;
    let sql = |e: rusqlite::Error| e.to_string();
    let group = group_ids(tx, &id).map_err(sql)?;
    let canonical = group
        .iter()
        .chain(std::iter::once(&id))
        .max_by_key(|s| s.len())
        .cloned()
        .unwrap_or(id);
    for other in group.iter().filter(|o| **o != canonical) {
        // A chat linked under both lengths keeps the longer-id row.
        tx.execute(
            "DELETE FROM item_chat_links WHERE item_id = ?1
               AND chat_id IN (SELECT chat_id FROM item_chat_links WHERE item_id = ?2)",
            params![other, canonical],
        )
        .map_err(sql)?;
        tx.execute(
            "UPDATE item_chat_links SET item_id = ?2 WHERE item_id = ?1",
            params![other, canonical],
        )
        .map_err(sql)?;
    }
    let has_main: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM item_chat_links WHERE item_id = ?1 AND is_main = 1)",
            [&canonical],
            |r| r.get(0),
        )
        .map_err(sql)?;
    let is_main = main || !has_main;
    if is_main {
        tx.execute(
            "UPDATE item_chat_links SET is_main = 0 WHERE item_id = ?1",
            [&canonical],
        )
        .map_err(sql)?;
    }
    tx.execute(
        "INSERT INTO item_chat_links (item_id, chat_id, is_main, linked_by, linked_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(item_id, chat_id) DO UPDATE SET is_main = max(is_main, excluded.is_main)",
        params![canonical, chat_id, is_main, by_str(by), now],
    )
    .map_err(sql)?;
    let main_now: bool = tx
        .query_row(
            "SELECT is_main FROM item_chat_links WHERE item_id = ?1 AND chat_id = ?2",
            params![canonical, chat_id],
            |r| r.get(0),
        )
        .map_err(sql)?;
    let chats: i64 = tx
        .query_row(
            "SELECT count(*) FROM item_chat_links WHERE item_id = ?1",
            [&canonical],
            |r| r.get(0),
        )
        .map_err(sql)?;
    Ok(Linked {
        item_id: canonical,
        main: main_now,
        chats: chats as usize,
    })
}

/// Remove the link. Returns the item id it was stored under, or `None` when there was none. When
/// the main chat leaves, the newest remaining chat becomes main.
pub fn unlink(
    tx: &Transaction<'_>,
    chat_id: &str,
    raw_item: &str,
) -> Result<Option<String>, String> {
    let id = ids::normalise(raw_item)
        .ok_or("itemId must be an item's full id or the first 8 or more hex characters of it.")?;
    let sql = |e: rusqlite::Error| e.to_string();
    let group = group_ids(tx, &id).map_err(sql)?;
    let mut found = None;
    for g in &group {
        let was_main: Option<bool> = tx
            .query_row(
                "SELECT is_main FROM item_chat_links WHERE item_id = ?1 AND chat_id = ?2",
                params![g, chat_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        let Some(was_main) = was_main else { continue };
        tx.execute(
            "DELETE FROM item_chat_links WHERE item_id = ?1 AND chat_id = ?2",
            params![g, chat_id],
        )
        .map_err(sql)?;
        if was_main {
            tx.execute(
                "UPDATE item_chat_links SET is_main = 1
                 WHERE item_id = ?1 AND chat_id = (
                     SELECT chat_id FROM item_chat_links WHERE item_id = ?1
                     ORDER BY linked_at DESC, chat_id LIMIT 1)",
                [g],
            )
            .map_err(sql)?;
        }
        found = Some(g.clone());
    }
    Ok(found)
}

/// The user's choice of an item's main chat. The chat must already be linked to it.
pub fn set_main(tx: &Transaction<'_>, raw_item: &str, chat_id: &str) -> Result<String, String> {
    let id = ids::normalise(raw_item).ok_or("That is not an item id.")?;
    let sql = |e: rusqlite::Error| e.to_string();
    for g in group_ids(tx, &id).map_err(sql)? {
        let linked: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM item_chat_links WHERE item_id = ?1 AND chat_id = ?2)",
                params![g, chat_id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if linked {
            tx.execute(
                "UPDATE item_chat_links SET is_main = (chat_id = ?2) WHERE item_id = ?1",
                params![g, chat_id],
            )
            .map_err(sql)?;
            return Ok(g);
        }
    }
    Err("That chat is not linked to this item.".into())
}

fn row_link(tx: &Transaction<'_>, r: &rusqlite::Row<'_>) -> rusqlite::Result<ItemLink> {
    let item_id: String = r.get(0)?;
    Ok(ItemLink {
        item_title: title_of(tx, &item_id).or_else(|| stored_title(tx, &item_id)),
        item_id,
        chat_id: r.get(1)?,
        is_main: r.get(2)?,
        linked_by: by_of(&r.get::<_, String>(3)?),
        linked_at: r.get(4)?,
    })
}

const COLS: &str = "item_id, chat_id, is_main, linked_by, linked_at";

/// Every link of the asked chats, plus the other chats linked to the same items.
pub fn for_chats(tx: &Transaction<'_>, chat_ids: &[String]) -> rusqlite::Result<Vec<ItemLink>> {
    let mut items = BTreeSet::new();
    let mut st = tx.prepare("SELECT item_id FROM item_chat_links WHERE chat_id = ?1")?;
    for c in chat_ids {
        for id in st.query_map([c], |r| r.get::<_, String>(0))? {
            items.insert(id?);
        }
    }
    let mut out = Vec::new();
    let mut st = tx.prepare(&format!(
        "SELECT {COLS} FROM item_chat_links WHERE item_id = ?1 ORDER BY is_main DESC, linked_at DESC, chat_id"
    ))?;
    for item in &items {
        let rows = st.query_map([item], |r| row_link(tx, r))?;
        for l in rows {
            out.push(l?);
        }
    }
    Ok(out)
}

/// An item's chats, main first, then newest link first.
pub fn for_item(tx: &Transaction<'_>, raw_item: &str) -> Result<Vec<ItemLink>, String> {
    let id = ids::normalise(raw_item).ok_or("That is not an item id.")?;
    let sql = |e: rusqlite::Error| e.to_string();
    let mut out = Vec::new();
    for g in group_ids(tx, &id).map_err(sql)? {
        let mut st = tx
            .prepare(&format!(
                "SELECT {COLS} FROM item_chat_links WHERE item_id = ?1"
            ))
            .map_err(sql)?;
        let rows = st.query_map([&g], |r| row_link(tx, r)).map_err(sql)?;
        for l in rows {
            out.push(l.map_err(sql)?);
        }
    }
    out.sort_by(|a, b| {
        b.is_main
            .cmp(&a.is_main)
            .then(b.linked_at.cmp(&a.linked_at))
            .then(a.chat_id.cmp(&b.chat_id))
    });
    Ok(out)
}

/// What `item_open` answers: one chat, or the chats to choose from (main first).
pub fn open(tx: &Transaction<'_>, raw_item: &str) -> Result<ItemOpen, String> {
    let links = for_item(tx, raw_item)?;
    let item_id = match links.first() {
        Some(l) => l.item_id.clone(),
        None => return Err("No chat is linked to this item.".into()),
    };
    let item_title = links.first().and_then(|l| l.item_title.clone());
    if links.len() == 1 {
        return Ok(ItemOpen {
            chat_id: Some(links[0].chat_id.clone()),
            candidates: vec![],
            item_id,
            item_title,
        });
    }
    let sql = |e: rusqlite::Error| e.to_string();
    let mut candidates = Vec::new();
    for l in &links {
        let (title, archived): (String, bool) = tx
            .query_row(
                "SELECT title, archived FROM chats WHERE id = ?1",
                [&l.chat_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sql)?;
        candidates.push(ItemCandidate {
            chat_id: l.chat_id.clone(),
            title,
            is_main: l.is_main,
            archived,
            linked_at: l.linked_at,
        });
    }
    Ok(ItemOpen {
        chat_id: None,
        candidates,
        item_id,
        item_title,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    pub fn db(chats: &[&str]) -> (tempfile::TempDir, Database) {
        let mut reg = MigrationRegistry::new();
        reg.initializer("itemlinks", init);
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute("INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)", [])?;
            for c in chats {
                tx.execute(
                    "INSERT INTO chats(id, project_id, title, created_at, updated_at) VALUES (?1, 'p', ?2, 1, 1)",
                    params![c, format!("Chat {c}")],
                )?;
            }
            Ok(())
        })
        .unwrap();
        (dir, db)
    }

    const ITEM: &str = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";

    fn mains(db: &Database, item: &str) -> Vec<(String, bool)> {
        db.snapshot(|tx| for_item(tx, item).map_err(rusqlite::Error::InvalidParameterName))
            .unwrap()
            .into_iter()
            .map(|l| (l.chat_id, l.is_main))
            .collect()
    }

    #[test]
    fn the_first_link_is_main_and_main_moves_only_when_asked() {
        let (_d, db) = db(&["a", "b", "c"]);
        let w = |chat: &str, main: bool, now: i64| {
            db.write::<_, rusqlite::Error>(|tx| {
                Ok(link(tx, chat, ITEM, main, LinkedBy::Agent, now).unwrap())
            })
            .unwrap()
        };
        assert_eq!(
            w("a", false, 10),
            Linked {
                item_id: "a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5d".into(),
                main: true,
                chats: 1
            }
        );
        assert!(
            !w("b", false, 20).main,
            "a second chat is not main unless it asks"
        );
        assert_eq!(mains(&db, ITEM), [("a".into(), true), ("b".into(), false)]);
        assert!(w("c", true, 30).main);
        assert_eq!(
            mains(&db, ITEM),
            [("c".into(), true), ("b".into(), false), ("a".into(), false)]
        );
        // Repeating a link changes nothing: not its time, not its main flag.
        assert!(!w("b", false, 99).main);
        assert_eq!(
            mains(&db, "a1b2c3d4"),
            [("c".into(), true), ("b".into(), false), ("a".into(), false)]
        );
    }

    #[test]
    fn a_prefix_link_and_a_full_id_link_are_one_item() {
        let (_d, db) = db(&["a", "b"]);
        db.write::<_, rusqlite::Error>(|tx| {
            link(tx, "a", "a1b2c3d4", false, LinkedBy::Agent, 1).unwrap();
            let l = link(tx, "b", ITEM, false, LinkedBy::Agent, 2).unwrap();
            assert_eq!(l.chats, 2);
            assert_eq!(
                l.item_id.len(),
                32,
                "the group is stored under the longest id"
            );
            // The same chat under both lengths is one row.
            let l = link(tx, "a", "a1b2c3d4e5f6", false, LinkedBy::Agent, 3).unwrap();
            assert_eq!(l.chats, 2);
            Ok(())
        })
        .unwrap();
        assert_eq!(mains(&db, "a1b2c3d4").len(), 2);
    }

    #[test]
    fn unlinking_main_promotes_the_newest_and_the_last_unlink_empties_the_item() {
        let (_d, db) = db(&["a", "b", "c"]);
        db.write::<_, rusqlite::Error>(|tx| {
            for (c, t) in [("a", 1), ("b", 2), ("c", 3)] {
                link(tx, c, ITEM, false, LinkedBy::Agent, t).unwrap();
            }
            assert!(unlink(tx, "a", ITEM).unwrap().is_some());
            assert_eq!(unlink(tx, "a", ITEM).unwrap(), None, "already gone");
            Ok(())
        })
        .unwrap();
        assert_eq!(mains(&db, ITEM), [("c".into(), true), ("b".into(), false)]);
        db.write::<_, rusqlite::Error>(|tx| {
            unlink(tx, "c", ITEM).unwrap();
            unlink(tx, "b", ITEM).unwrap();
            Ok(())
        })
        .unwrap();
        assert!(mains(&db, ITEM).is_empty());
    }

    #[test]
    fn the_user_sets_main_only_among_linked_chats_and_a_deleted_chat_drops_its_link() {
        let (_d, db) = db(&["a", "b", "x"]);
        db.write::<_, rusqlite::Error>(|tx| {
            link(tx, "a", ITEM, false, LinkedBy::Agent, 1).unwrap();
            link(tx, "b", ITEM, false, LinkedBy::Agent, 2).unwrap();
            assert!(set_main(tx, ITEM, "x").is_err());
            set_main(tx, "a1b2c3d4", "b").unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(mains(&db, ITEM)[0], ("b".into(), true));
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute("DELETE FROM chats WHERE id = 'b'", [])
                .map(|_| ())
        })
        .unwrap();
        assert_eq!(
            mains(&db, ITEM),
            [("a".into(), false)],
            "cascade: the link goes with its chat"
        );
    }

    #[test]
    fn open_answers_one_chat_or_the_candidates_main_first() {
        let (_d, db) = db(&["a", "b"]);
        let open_it = |db: &Database| {
            db.snapshot(|tx| open(tx, "a1b2c3d4").map_err(rusqlite::Error::InvalidParameterName))
        };
        assert!(open_it(&db).is_err(), "nothing linked");
        db.write::<_, rusqlite::Error>(|tx| {
            Ok(link(tx, "a", ITEM, false, LinkedBy::Agent, 1).unwrap())
        })
        .unwrap();
        let one = open_it(&db).unwrap();
        assert_eq!(one.chat_id.as_deref(), Some("a"));
        assert!(one.candidates.is_empty());
        db.write::<_, rusqlite::Error>(|tx| {
            Ok(link(tx, "b", ITEM, false, LinkedBy::Agent, 2).unwrap())
        })
        .unwrap();
        let many = open_it(&db).unwrap();
        assert_eq!(many.chat_id, None);
        assert_eq!(
            many.candidates
                .iter()
                .map(|c| (c.chat_id.as_str(), c.is_main, c.title.as_str()))
                .collect::<Vec<_>>(),
            [("a", true, "Chat a"), ("b", false, "Chat b")]
        );
    }

    #[test]
    fn a_link_is_titled_from_the_board_mirror_when_the_item_is_a_board_project() {
        let (_d, db) = db(&["a"]);
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute_batch("CREATE TABLE board_projects (board_project_id TEXT, title TEXT, stale INTEGER DEFAULT 0);")?;
            tx.execute("INSERT INTO board_projects(board_project_id, title) VALUES ('a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d', 'Sprint 4')", [])?;
            link(tx, "a", "a1b2c3d4", false, LinkedBy::Agent, 1).unwrap();
            Ok(())
        })
        .unwrap();
        let links = db.snapshot(|tx| for_chats(tx, &["a".to_owned()])).unwrap();
        assert_eq!(links[0].item_title.as_deref(), Some("Sprint 4"));
        let none = db
            .snapshot(|tx| for_chats(tx, &["zzz".to_owned()]))
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn a_stored_title_names_a_task_the_mirror_lacks_and_a_repeat_renames_it() {
        let (_d, db) = db(&["a", "b"]);
        let title = |db: &Database, chat: &str| {
            db.snapshot(|tx| for_chats(tx, &[chat.to_owned()]))
                .unwrap()
                .into_iter()
                .find(|l| l.chat_id == chat)
                .and_then(|l| l.item_title)
        };
        db.write::<_, rusqlite::Error>(|tx| {
            link(tx, "a", "a1b2c3d4", false, LinkedBy::Agent, 1).unwrap();
            set_title(
                tx,
                "a1b2c3d4",
                "  Fix	the
 deploy ",
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(title(&db, "a").as_deref(), Some("Fix the deploy"));
        // Every chat on the item shows it, including one linked later; blank input keeps it.
        db.write::<_, rusqlite::Error>(|tx| {
            let l = link(tx, "b", ITEM, false, LinkedBy::Agent, 2).unwrap();
            set_title(tx, &l.item_id, "   ")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(title(&db, "a").as_deref(), Some("Fix the deploy"));
        db.write::<_, rusqlite::Error>(|tx| set_title(tx, ITEM_ID, &"x".repeat(500)))
            .unwrap();
        assert_eq!(title(&db, "b").unwrap().chars().count(), MAX_TITLE);
        // A profile from before titles loads: the column is added and no link has a title.
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute_batch("ALTER TABLE item_chat_links DROP COLUMN title")?;
            init(tx)
        })
        .unwrap();
        assert_eq!(title(&db, "a"), None);
    }

    const ITEM_ID: &str = "a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5d";
}
