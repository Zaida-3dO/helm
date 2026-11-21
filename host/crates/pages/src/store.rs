//! What Helm keeps about pages: the pinned list and each page's saved state.
//!
//! - `pinned_pages`: one row per (profile, page). A page is pinned by the user or by an agent; the
//!   row remembers the chat that pinned it, which is where the page's messages go. Deleting that
//!   chat leaves the pin and clears the link. A hand-reordered list is kept in `position` (0..n
//!   within one profile's list, the pins that profile sees); pins without one keep the plain
//!   newest-first order and sort after the placed ones.
//! - `page_state`: the JSON a page saves with `helm.setState`, filed under the page's key (its
//!   canonical path) and only ever reached through a session the host opened for that path, so a
//!   page can neither name nor read another page's row. Saves are optimistic: a save quoting a
//!   stale revision is refused rather than overwriting.

use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};

/// The most a page may save.
pub const MAX_STATE_BYTES: usize = 256 * 1024;
pub const MAX_TITLE_CHARS: usize = 160;
/// Pinned pages kept at most, per profile.
pub const MAX_PINS: usize = 200;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS pinned_pages (
             id         TEXT PRIMARY KEY,
             profile_id TEXT NOT NULL DEFAULT '',
             path_key   TEXT NOT NULL,
             path       TEXT NOT NULL,
             title      TEXT NOT NULL,
             chat_id    TEXT REFERENCES chats(id) ON DELETE SET NULL,
             pinned_at  INTEGER NOT NULL,
             position   INTEGER,
             UNIQUE (profile_id, path_key)
         );
         CREATE TABLE IF NOT EXISTS page_state (
             path_key   TEXT PRIMARY KEY,
             state      TEXT NOT NULL,
             revision   INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         );",
    )
}

/// Gives `pinned_pages` its `position` column (set only once someone reorders the list;
/// unset pins keep the plain newest-first order). Re-running does nothing.
pub fn add_pin_position(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let has: bool = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('pinned_pages') WHERE name = 'position'",
        [],
        |r| r.get::<_, i64>(0).map(|n| n > 0),
    )?;
    if !has {
        tx.execute("ALTER TABLE pinned_pages ADD COLUMN position INTEGER", [])?;
    }
    Ok(())
}

/// A saved state and the revision it is at (0 and `null` before the first save).
pub fn state_get(tx: &Transaction<'_>, key: &str) -> rusqlite::Result<(Value, i64)> {
    let row: Option<(String, i64)> = tx
        .query_row(
            "SELECT state, revision FROM page_state WHERE path_key = ?1",
            [key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((text, revision)) => (serde_json::from_str(&text).unwrap_or(Value::Null), revision),
        None => (Value::Null, 0),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    TooLarge,
    /// The stored revision is not the one the save quoted.
    Stale,
    Db(String),
}

impl From<rusqlite::Error> for SaveError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e.to_string())
    }
}

/// Saves `state` when `expected` is the stored revision; returns the new one.
pub fn state_save(
    tx: &Transaction<'_>,
    key: &str,
    state: &Value,
    expected: i64,
    now: i64,
) -> Result<i64, SaveError> {
    let text = serde_json::to_string(state).map_err(|e| SaveError::Db(e.to_string()))?;
    if text.len() > MAX_STATE_BYTES {
        return Err(SaveError::TooLarge);
    }
    let (_, current) = state_get(tx, key)?;
    if current != expected {
        return Err(SaveError::Stale);
    }
    let next = current + 1;
    tx.execute(
        "INSERT INTO page_state (path_key, state, revision, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(path_key) DO UPDATE SET state = ?2, revision = ?3, updated_at = ?4",
        params![key, text, next, now],
    )?;
    Ok(next)
}

/// Forgets a page's saved state. The revision restarts at 0, so a frame still holding the old
/// one is refused until it reloads.
pub fn state_reset(tx: &Transaction<'_>, key: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM page_state WHERE path_key = ?1", [key])?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pin_json(
    id: String,
    profile_id: String,
    path: String,
    title: String,
    chat_id: Option<String>,
    pinned_at: i64,
    position: Option<i64>,
) -> Value {
    json!({
        "id": id,
        "profileId": profile_id,
        "path": path,
        "title": title,
        "chatId": chat_id,
        "pinnedAt": pinned_at,
        "position": position,
    })
}

fn pin_by_key(tx: &Transaction<'_>, profile: &str, key: &str) -> rusqlite::Result<Option<Value>> {
    tx.query_row(
        "SELECT id, profile_id, path, title, chat_id, pinned_at, position FROM pinned_pages
         WHERE profile_id = ?1 AND path_key = ?2",
        [profile, key],
        |r| {
            Ok(pin_json(
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        },
    )
    .optional()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinError {
    TooMany,
    Db(String),
}

impl From<rusqlite::Error> for PinError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e.to_string())
    }
}

/// Pins a page for a profile, or updates the pin it already has (a new title or linked chat when
/// given; the old ones stay otherwise). Returns the row and whether it is new.
#[allow(clippy::too_many_arguments)]
pub fn pin(
    tx: &Transaction<'_>,
    id: &str,
    profile: &str,
    key: &str,
    path: &str,
    title: Option<&str>,
    chat_id: Option<&str>,
    now: i64,
) -> Result<(Value, bool), PinError> {
    let title = title
        .map(|t| t.trim().chars().take(MAX_TITLE_CHARS).collect::<String>())
        .filter(|t| !t.is_empty());
    if pin_by_key(tx, profile, key)?.is_some() {
        tx.execute(
            "UPDATE pinned_pages SET path = ?3, title = COALESCE(?4, title), chat_id = COALESCE(?5, chat_id)
             WHERE profile_id = ?1 AND path_key = ?2",
            params![profile, key, path, title, chat_id],
        )?;
        return Ok((pin_by_key(tx, profile, key)?.unwrap_or(Value::Null), false));
    }
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pinned_pages WHERE profile_id = ?1",
        [profile],
        |r| r.get(0),
    )?;
    if count as usize >= MAX_PINS {
        return Err(PinError::TooMany);
    }
    let title = title.unwrap_or_else(|| default_title(path));
    tx.execute(
        "INSERT INTO pinned_pages (id, profile_id, path_key, path, title, chat_id, pinned_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, profile, key, path, title, chat_id, now],
    )?;
    Ok((pin_by_key(tx, profile, key)?.unwrap_or(Value::Null), true))
}

fn default_title(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    if stem.is_empty() {
        "Page".to_owned()
    } else {
        stem.chars().take(MAX_TITLE_CHARS).collect()
    }
}

/// Removes a pin. Its saved state stays: pinning the page again finds it as it was.
pub fn unpin(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<bool> {
    Ok(tx.execute("DELETE FROM pinned_pages WHERE id = ?1", [id])? > 0)
}

/// Links a pinned page to a chat (or, with `None`, to none). False when there is no such pin.
pub fn link_chat(tx: &Transaction<'_>, id: &str, chat_id: Option<&str>) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "UPDATE pinned_pages SET chat_id = ?2 WHERE id = ?1",
        params![id, chat_id],
    )? > 0)
}

/// One profile's view of the list: placed pins first by `position`, the rest newest first.
const PIN_ORDER: &str = "ORDER BY position IS NULL, position, pinned_at DESC, rowid DESC";

/// The pinned pages: once reordered by hand that order, otherwise newest first. With a profile:
/// that profile's pages and the ones pinned before any profile existed.
pub fn list(tx: &Transaction<'_>, profile: Option<&str>) -> rusqlite::Result<Vec<Value>> {
    let mut st = tx.prepare(&format!(
        "SELECT id, profile_id, path, title, chat_id, pinned_at, position FROM pinned_pages
         WHERE (?1 IS NULL OR profile_id = ?1 OR profile_id = '')
         {PIN_ORDER}"
    ))?;
    let rows = st.query_map([profile], |r| {
        Ok(pin_json(
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })?;
    rows.collect()
}

/// Move a pin to stand just before another in its profile's list (or at the end when `before`
/// is `None`), renumbering the whole list it sits in so the places are dense from 0. The list
/// renumbered is the one that profile sees: its own pins and the pins left over from before
/// profiles existed; a pin left over from then is moved within the list its anchor stands in.
/// A move that changes nothing (same place, an unknown id, or an anchor in another profile's
/// list) writes nothing and answers false.
pub fn move_pin(tx: &Transaction<'_>, id: &str, before: Option<&str>) -> rusqlite::Result<bool> {
    let owner = |pin: &str| -> rusqlite::Result<Option<String>> {
        tx.query_row(
            "SELECT profile_id FROM pinned_pages WHERE id = ?1",
            [pin],
            |r| r.get(0),
        )
        .optional()
    };
    let Some(profile) = owner(id)? else {
        return Ok(false);
    };
    // A pin from before profiles existed has no list of its own: it moves within the list
    // its anchor stands in, or the leftover pins alone when dropped at the end.
    let scope = if profile.is_empty() {
        match before.map(owner).transpose()? {
            Some(Some(anchor)) if !anchor.is_empty() => anchor,
            _ => String::new(),
        }
    } else {
        profile
    };
    let mut order: Vec<String> = {
        let mut st = tx.prepare(&format!(
            "SELECT id FROM pinned_pages WHERE profile_id = ?1 OR profile_id = '' {PIN_ORDER}"
        ))?;
        let rows = st.query_map([scope.as_str()], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()?
    };
    let Some(at) = order.iter().position(|x| x == id) else {
        return Ok(false);
    };
    order.remove(at);
    let to = match before {
        Some(b) if b == id => return Ok(false),
        Some(b) => match order.iter().position(|x| x == b) {
            Some(p) => p,
            None => return Ok(false),
        },
        None => order.len(),
    };
    if to == at {
        return Ok(false);
    }
    order.insert(to, id.to_owned());
    for (i, pin_id) in order.iter().enumerate() {
        tx.execute(
            "UPDATE pinned_pages SET position = ?2 WHERE id = ?1",
            params![pin_id, i as i64],
        )?;
    }
    Ok(true)
}

/// Whether the page is pinned for `profile`: its own pin, or one pinned before profiles existed
/// (which every profile sees, as in [`list`]). An empty profile — an open beside no chat, where no
/// profile is known — matches any pin, so opening a pinned page from the Pages list runs it
/// online. The open's network rule reads this.
pub fn is_pinned_for(tx: &Transaction<'_>, profile: &str, key: &str) -> rusqlite::Result<bool> {
    let found: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM pinned_pages
             WHERE path_key = ?2 AND (profile_id = ?1 OR profile_id = '' OR ?1 = '')",
            [profile, key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// The pin for a page, for any profile that has pinned it.
pub fn pins_for_key(tx: &Transaction<'_>, key: &str) -> rusqlite::Result<Vec<Value>> {
    let mut st = tx.prepare(
        "SELECT id, profile_id, path, title, chat_id, pinned_at, position FROM pinned_pages
         WHERE path_key = ?1 ORDER BY pinned_at DESC",
    )?;
    let rows = st.query_map([key], |r| {
        Ok(pin_json(
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    fn db() -> (tempfile::TempDir, Database) {
        let mut reg = MigrationRegistry::new();
        reg.initializer("pages", init);
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        (dir, db)
    }

    /// Saved state: first save, the revision rule, the size limit, reset, and that rows are
    /// separate per page key.
    #[test]
    fn saved_state_is_optimistic_bounded_and_per_page() {
        let (_d, db) = db();
        let get = |k: &str| db.snapshot(|tx| state_get(tx, k)).unwrap();
        let save = |k: &str, v: Value, rev: i64| db.write(|tx| state_save(tx, k, &v, rev, 1));
        assert_eq!(get("a"), (Value::Null, 0));
        assert_eq!(save("a", json!({"open": [1]}), 0), Ok(1));
        assert_eq!(get("a"), (json!({"open": [1]}), 1));
        assert_eq!(save("a", json!("x"), 0), Err(SaveError::Stale));
        assert_eq!(save("a", json!("x"), 5), Err(SaveError::Stale));
        assert_eq!(save("a", json!("x"), 1), Ok(2));
        // Another page has its own row and revision.
        assert_eq!(get("b"), (Value::Null, 0));
        assert_eq!(save("b", json!(true), 0), Ok(1));
        assert_eq!(get("a").0, json!("x"));
        // The limit counts the serialised text.
        let big = Value::String("x".repeat(MAX_STATE_BYTES));
        assert_eq!(save("a", big, 2), Err(SaveError::TooLarge));
        assert_eq!(get("a").1, 2);
        // Reset forgets one page only.
        db.write(|tx| state_reset(tx, "a")).unwrap();
        assert_eq!(get("a"), (Value::Null, 0));
        assert_eq!(get("b").1, 1);
    }

    /// Pinning: new and repeat pins, profiles, listing, linking a chat, a deleted chat and unpin.
    #[test]
    fn pins_are_per_profile_listed_linked_and_survive_their_chat() {
        let (_d, db) = db();
        db.write(|tx| {
            tx.execute("INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'p', 'C:/p', 0)", [])?;
            tx.execute("INSERT INTO chats (id, project_id, created_at, updated_at) VALUES ('c1', 'p', 0, 0)", [])
        })
        .unwrap();
        let pin_it = |id: &str,
                      profile: &str,
                      key: &str,
                      title: Option<&str>,
                      chat: Option<&str>,
                      now: i64| {
            db.write(|tx| {
                pin(
                    tx,
                    id,
                    profile,
                    key,
                    &format!("C:\\w\\{key}.html"),
                    title,
                    chat,
                    now,
                )
            })
        };
        let (a, fresh) = pin_it("i1", "", "k1", None, Some("c1"), 10).unwrap();
        assert!(fresh);
        assert_eq!(
            (a["title"].as_str(), a["chatId"].as_str()),
            (Some("k1"), Some("c1"))
        );
        // Pinning again keeps the id and the chat, and takes a new title.
        let (b, fresh) = pin_it("zz", "", "k1", Some("  Progress  "), None, 20).unwrap();
        assert!(!fresh);
        assert_eq!(
            (b["id"].as_str(), b["title"].as_str(), b["chatId"].as_str()),
            (Some("i1"), Some("Progress"), Some("c1"))
        );
        pin_it("i2", "profile-a", "k2", Some("Mine"), None, 30).unwrap();
        pin_it("i3", "profile-b", "k3", Some("Other"), None, 40).unwrap();
        let titles = |profile: Option<&str>| -> Vec<String> {
            db.snapshot(|tx| list(tx, profile))
                .unwrap()
                .iter()
                .map(|p| p["title"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(titles(None), ["Other", "Mine", "Progress"]);
        // A profile sees its own pages and the unassigned ones.
        assert_eq!(titles(Some("profile-a")), ["Mine", "Progress"]);
        assert_eq!(db.snapshot(|tx| pins_for_key(tx, "k1")).unwrap().len(), 1);
        // The network rule: a profile's own pin and the legacy unassigned pins count for it,
        // another profile's pin does not; an unknown (empty) profile counts any pin.
        let pinned_for =
            |profile: &str, key: &str| db.snapshot(|tx| is_pinned_for(tx, profile, key)).unwrap();
        assert!(pinned_for("", "k1"));
        assert!(
            pinned_for("profile-a", "k1"),
            "a pin from before profiles applies to all"
        );
        assert!(pinned_for("profile-a", "k2"));
        assert!(
            !pinned_for("profile-b", "k2"),
            "another profile's pin does not carry over"
        );
        assert!(pinned_for("", "k2"));
        assert!(!pinned_for("profile-a", "k3"));
        assert!(!pinned_for("profile-a", "never"));
        // The same page in two profiles is two pins.
        pin_it("i4", "profile-a", "k1", None, None, 50).unwrap();
        assert_eq!(db.snapshot(|tx| pins_for_key(tx, "k1")).unwrap().len(), 2);

        // Deleting the linked chat keeps the pin and clears the link.
        db.write(|tx| tx.execute("DELETE FROM chats WHERE id = 'c1'", []))
            .unwrap();
        let after = db.snapshot(|tx| list(tx, Some(""))).unwrap();
        assert!(after.iter().all(|p| p["chatId"].is_null()));
        assert!(
            db.write(|tx| link_chat(tx, "i1", Some("c1"))).is_err(),
            "no such chat"
        );
        assert!(db.write(|tx| link_chat(tx, "i1", None)).unwrap());
        assert!(!db.write(|tx| link_chat(tx, "nope", None)).unwrap());

        // Unpinning removes the pin only.
        db.write(|tx| state_save(tx, "k2", &json!(1), 0, 1))
            .unwrap();
        assert!(db.write(|tx| unpin(tx, "i2")).unwrap());
        assert!(!db.write(|tx| unpin(tx, "i2")).unwrap());
        assert_eq!(db.snapshot(|tx| state_get(tx, "k2")).unwrap().1, 1);
        assert!(
            !pinned_for("profile-a", "k2"),
            "unpinning takes the network rule away too"
        );

        // The per-profile cap.
        db.write(|tx| {
            for n in 0..MAX_PINS {
                pin(
                    tx,
                    &format!("m{n}"),
                    "full",
                    &format!("f{n}"),
                    r"C:\f.html",
                    None,
                    None,
                    1,
                )?;
            }
            Ok::<_, PinError>(())
        })
        .unwrap();
        assert_eq!(
            pin_it("over", "full", "one-more", None, None, 1),
            Err(PinError::TooMany)
        );
    }

    /// Hand reorder: a move places a pin where it is dropped and gives the list dense
    /// positions; a pin pinned later joins the newest-first tail; a move that changes
    /// nothing writes nothing; each profile moves its own list (the pins left over from
    /// before profiles existed stand in every list that sees them).
    #[test]
    fn pins_can_be_reordered_by_hand() {
        let (_d, db) = db();
        let pin_it = |id: &str, profile: &str, now: i64| {
            db.write(|tx| {
                pin(
                    tx,
                    id,
                    profile,
                    &format!("k{id}"),
                    &format!("C:\\w\\{id}.html"),
                    Some(id),
                    None,
                    now,
                )
            })
        };
        pin_it("l", "", 5).unwrap(); // pinned before profiles existed: stands in every list
        pin_it("a", "p", 10).unwrap();
        pin_it("b", "p", 20).unwrap();
        pin_it("c", "p", 30).unwrap();
        let ids = |profile: Option<&str>| -> Vec<String> {
            db.snapshot(|tx| list(tx, profile))
                .unwrap()
                .iter()
                .map(|p| p["id"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(
            ids(Some("p")),
            ["c", "b", "a", "l"],
            "newest first until someone reorders"
        );

        // Drag a above c: it takes the front place and the places become dense from 0.
        assert!(db.write(|tx| move_pin(tx, "a", Some("c"))).unwrap());
        assert_eq!(ids(Some("p")), ["a", "c", "b", "l"]);
        // Drag l in front of b: it lands between them.
        assert!(db.write(|tx| move_pin(tx, "l", Some("b"))).unwrap());
        assert_eq!(ids(Some("p")), ["a", "c", "l", "b"]);
        // A pin made after the reorder joins the tail, not the placed run.
        pin_it("d", "p", 40).unwrap();
        assert_eq!(ids(Some("p")), ["a", "c", "l", "b", "d"]);
        // Drop b at the end, before nothing.
        assert!(db.write(|tx| move_pin(tx, "b", None)).unwrap());
        assert_eq!(ids(Some("p")), ["a", "c", "l", "d", "b"]);

        // The same place, an unknown pin and an unknown anchor change nothing.
        assert!(!db.write(|tx| move_pin(tx, "a", Some("c"))).unwrap());
        assert!(!db.write(|tx| move_pin(tx, "gone", Some("c"))).unwrap());
        assert!(!db.write(|tx| move_pin(tx, "a", Some("gone"))).unwrap());
        assert_eq!(ids(Some("p")), ["a", "c", "l", "d", "b"]);

        // The places are dense and readable on the rows themselves.
        let rows = db.snapshot(|tx| list(tx, Some("p"))).unwrap();
        assert_eq!(rows[0]["position"], json!(0));
        assert_eq!(rows[1]["position"], json!(1));
        assert_eq!(rows[4]["position"], json!(4));

        // Another profile keeps its own tail; the shared leftover pin carries the place the
        // first profile's reorder wrote, so it leads there too.
        pin_it("x", "q", 50).unwrap();
        assert_eq!(ids(Some("q")), ["l", "x"]);
        assert_eq!(ids(None).len(), 6);
    }
}
