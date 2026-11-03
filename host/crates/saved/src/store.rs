//! The two tables and everything that reads or writes them: the bounds, the duplicate rule,
//! listing and search, updates with an optional revision check, the note trail, and the sweep
//! that turns a reminder due into exactly one due item.

use rusqlite::{params, params_from_iter, OptionalExtension, Transaction};
use serde::Serialize;

pub const MAX_ITEMS: i64 = 10_000;
pub const MAX_NOTES: i64 = 200;
pub const MAX_NOTE_BYTES: usize = 8_000;
pub const MAX_BODY_BYTES: usize = 16_384;
pub const MAX_TITLE_CHARS: usize = 200;
pub const PAGE_SIZE: i64 = 50;

pub const STATES: [&str; 4] = ["saved", "filed", "done", "dismissed"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedItem {
    pub id: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub chat_id: Option<String>,
    pub message_id: Option<i64>,
    pub capture_key: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub revision: i64,
    pub remind_at: Option<i64>,
    pub reminded_at: Option<i64>,
    pub note_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedNote {
    pub id: i64,
    pub item_id: String,
    pub author_type: String,
    pub author_id: Option<String>,
    pub text: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedPage {
    pub items: Vec<SavedItem>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

/// Why a write was refused; the message is safe to show as-is.
#[derive(Debug)]
pub enum StoreError {
    /// The caller's text is out of bounds.
    Bad(&'static str),
    NotFound,
    /// Someone else changed the item since the caller last read it.
    Conflict,
    Full,
}

// Database::write and snapshot need E: From<rusqlite::Error>, so the store's error can drive them
// directly; a host failure is rare and shows as a plain refusal.
impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Bad(Box::leak(
            format!("Helm could not read the saved items: {e}").into_boxed_str(),
        ))
    }
}

impl StoreError {
    pub fn message(&self) -> String {
        match self {
            StoreError::Bad(m) => (*m).to_owned(),
            StoreError::NotFound => "No saved item has that id.".to_owned(),
            StoreError::Conflict => "The item changed since you read it.".to_owned(),
            StoreError::Full => {
                format!("Helm holds at most {MAX_ITEMS} saved items; remove some first.")
            }
        }
    }
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS saved_items (
             id TEXT PRIMARY KEY,
             title TEXT NOT NULL,
             body TEXT NOT NULL,
             state TEXT NOT NULL DEFAULT 'saved' CHECK (state IN ('saved','filed','done','dismissed')),
             chat_id TEXT,
             message_id INTEGER,
             capture_key TEXT,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             revision INTEGER NOT NULL DEFAULT 1,
             remind_at INTEGER,
             reminded_at INTEGER
         );
         CREATE UNIQUE INDEX IF NOT EXISTS saved_origin ON saved_items(chat_id, message_id)
             WHERE chat_id IS NOT NULL AND message_id IS NOT NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS saved_key ON saved_items(capture_key) WHERE capture_key IS NOT NULL;
         CREATE TABLE IF NOT EXISTS saved_item_notes (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             item_id TEXT NOT NULL,
             author_type TEXT NOT NULL CHECK (author_type IN ('user','agent')),
             author_id TEXT,
             text TEXT NOT NULL,
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS saved_notes_item ON saved_item_notes(item_id);",
    )
}

fn check_text(title: &str, body: &str) -> Result<(), StoreError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(StoreError::Bad("A saved item needs a title."));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(StoreError::Bad(
            "Saved item titles are at most 200 characters.",
        ));
    }
    if body.trim().is_empty() {
        return Err(StoreError::Bad(
            "A saved item needs something written down.",
        ));
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(StoreError::Bad("Saved item text is at most 16 KiB."));
    }
    Ok(())
}

fn row_to_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<SavedItem> {
    Ok(SavedItem {
        id: r.get(0)?,
        title: r.get(1)?,
        body: r.get(2)?,
        state: r.get(3)?,
        chat_id: r.get(4)?,
        message_id: r.get(5)?,
        capture_key: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        revision: r.get(9)?,
        remind_at: r.get(10)?,
        reminded_at: r.get(11)?,
        note_count: r.get(12)?,
    })
}

const COLS: &str = "s.id, s.title, s.body, s.state, s.chat_id, s.message_id, s.capture_key, \
                    s.created_at, s.updated_at, s.revision, s.remind_at, s.reminded_at, \
                    (SELECT COUNT(*) FROM saved_item_notes n WHERE n.item_id = s.id)";

/// What to save: the text, where it came from (at most one rule applies), and an optional
/// reminder. An `origin` pair is a transcript row to dedupe on; `capture_key` is the caller's own
/// stable key for the same job.
#[derive(Default)]
pub struct NewItem<'a> {
    pub chat_id: Option<&'a str>,
    pub message_id: Option<i64>,
    pub capture_key: Option<&'a str>,
    pub title: &'a str,
    pub body: &'a str,
    pub remind_at: Option<i64>,
}

/// Save an item. The same transcript row (chat and message) or the same capture key answers
/// with the item that is already there and `already` — never a second copy.
pub fn save(
    tx: &Transaction<'_>,
    n: &NewItem<'_>,
    now: i64,
) -> Result<(SavedItem, bool), StoreError> {
    let NewItem {
        chat_id,
        message_id,
        capture_key,
        title,
        body,
        remind_at,
    } = *n;
    check_text(title, body)?;
    if let (Some(c), Some(m)) = (chat_id, message_id) {
        let found = tx
            .query_row(
                &format!(
                    "SELECT {COLS} FROM saved_items s WHERE s.chat_id = ?1 AND s.message_id = ?2"
                ),
                params![c, m],
                row_to_item,
            )
            .optional()?;
        if let Some(item) = found {
            return Ok((item, true));
        }
    }
    if let Some(k) = capture_key {
        let found = tx
            .query_row(
                &format!("SELECT {COLS} FROM saved_items s WHERE s.capture_key = ?1"),
                params![k],
                row_to_item,
            )
            .optional()?;
        if let Some(item) = found {
            return Ok((item, true));
        }
    }
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM saved_items", params![], |r| r.get(0))?;
    if count >= MAX_ITEMS {
        return Err(StoreError::Full);
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    tx.execute(
        "INSERT INTO saved_items (id, title, body, chat_id, message_id, capture_key, created_at, updated_at, remind_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
        params![id, title.trim(), body, chat_id, message_id, capture_key, now, remind_at],
    )?;
    Ok((get(tx, &id)?, false))
}

pub fn get(tx: &Transaction<'_>, id: &str) -> Result<SavedItem, StoreError> {
    tx.query_row(
        &format!("SELECT {COLS} FROM saved_items s WHERE s.id = ?1"),
        params![id],
        row_to_item,
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

/// One search over every page. `query` matches a case-insensitive substring of the title, the
/// body or any note; newest change first, 50 per page.
pub fn list(
    tx: &Transaction<'_>,
    state: Option<&str>,
    query: Option<&str>,
    page: i64,
) -> Result<SavedPage, StoreError> {
    let state = match state {
        None => None,
        Some(s) if STATES.contains(&s) => Some(s),
        Some(_) => {
            return Err(StoreError::Bad(
                "state must be saved, filed, done or dismissed.",
            ))
        }
    };
    let page = page.max(1);
    // Case-insensitive the way SQLite is: ASCII folding, on both sides.
    let q = query
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let mut where_sql = String::from("WHERE 1");
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = state {
        where_sql.push_str(" AND s.state = ?");
        args.push(s.to_owned());
    }
    if let Some(q) = q {
        where_sql.push_str(
            " AND (instr(lower(s.title), ?) > 0 OR instr(lower(s.body), ?) > 0 \
             OR EXISTS (SELECT 1 FROM saved_item_notes n WHERE n.item_id = s.id AND instr(lower(n.text), ?) > 0))",
        );
        args.extend([q.to_owned(), q.to_owned(), q.to_owned()]);
    }
    let total: i64 = tx.query_row(
        &format!("SELECT COUNT(*) FROM saved_items s {where_sql}"),
        params_from_iter(args.iter()),
        |r| r.get(0),
    )?;
    args.push(PAGE_SIZE.to_string());
    args.push(((page - 1) * PAGE_SIZE).to_string());
    let items = tx
        .prepare(&format!(
            "SELECT {COLS} FROM saved_items s {where_sql} ORDER BY s.updated_at DESC, s.id LIMIT ? OFFSET ?"
        ))?
        .query_map(params_from_iter(args.iter()), row_to_item)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SavedPage {
        items,
        total,
        page,
        page_size: PAGE_SIZE,
    })
}

/// The fields to change; absent fields stay as they are.
#[derive(Default)]
pub struct Update {
    pub title: Option<String>,
    pub body: Option<String>,
    pub state: Option<String>,
    /// `Some(None)` clears the reminder; setting one arms it again even if it already fired.
    pub remind_at: Option<Option<i64>>,
    pub expect_revision: Option<i64>,
}

pub fn update(
    tx: &Transaction<'_>,
    id: &str,
    u: &Update,
    now: i64,
) -> Result<SavedItem, StoreError> {
    if u.title.is_some() || u.body.is_some() {
        let current = get(tx, id)?;
        let title = u.title.as_deref().unwrap_or(&current.title);
        let body = u.body.as_deref().unwrap_or(&current.body);
        check_text(title, body)?;
    }
    if let Some(s) = &u.state {
        if !STATES.contains(&s.as_str()) {
            return Err(StoreError::Bad(
                "state must be saved, filed, done or dismissed.",
            ));
        }
    }
    let mut sets: Vec<String> = Vec::new();
    let mut bind: Vec<String> = Vec::new();
    if let Some(t) = &u.title {
        sets.push("title = ?".to_owned());
        bind.push(t.trim().to_owned());
    }
    if let Some(b) = &u.body {
        sets.push("body = ?".to_owned());
        bind.push(b.clone());
    }
    if let Some(s) = &u.state {
        sets.push("state = ?".to_owned());
        bind.push(s.clone());
    }
    if let Some(r) = u.remind_at {
        match r {
            Some(v) => {
                sets.push("remind_at = ?".to_owned());
                bind.push(v.to_string());
            }
            None => sets.push("remind_at = NULL".to_owned()),
        }
        // A new reminder time re-arms the cue, even one that already sounded.
        sets.push("reminded_at = NULL".to_owned());
    }
    if sets.is_empty() {
        return get(tx, id);
    }
    sets.push("revision = revision + 1".to_owned());
    sets.push("updated_at = ?".to_owned());
    bind.push(now.to_string());
    let mut sql = format!("UPDATE saved_items SET {} WHERE id = ?", sets.join(", "));
    bind.push(id.to_owned());
    if let Some(r) = u.expect_revision {
        sql.push_str(" AND revision = ?");
        bind.push(r.to_string());
    }
    let found = tx.execute(&sql, params_from_iter(bind.iter()))?;
    if found == 0 {
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM saved_items WHERE id = ?",
            params![id],
            |r| r.get(0),
        )?;
        return Err(if exists == 0 {
            StoreError::NotFound
        } else {
            StoreError::Conflict
        });
    }
    get(tx, id)
}

pub fn remove(tx: &Transaction<'_>, id: &str) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM saved_item_notes WHERE item_id = ?1",
        params![id],
    )?;
    let gone = tx.execute("DELETE FROM saved_items WHERE id = ?1", params![id])?;
    if gone == 0 {
        return Err(StoreError::NotFound);
    }
    Ok(())
}

pub fn add_note(
    tx: &Transaction<'_>,
    item_id: &str,
    author_type: &str,
    author_id: Option<&str>,
    text: &str,
    now: i64,
) -> Result<SavedNote, StoreError> {
    if !matches!(author_type, "user" | "agent") {
        return Err(StoreError::Bad("authorType must be user or agent."));
    }
    let text = text.trim();
    if text.is_empty() {
        return Err(StoreError::Bad("A note needs some text."));
    }
    if text.len() > MAX_NOTE_BYTES {
        return Err(StoreError::Bad("Notes are at most 8 KiB each."));
    }
    let held: i64 = tx.query_row(
        "SELECT COUNT(*) FROM saved_item_notes WHERE item_id = ?1",
        params![item_id],
        |r| r.get(0),
    )?;
    if held == 0 && get(tx, item_id).is_err() {
        return Err(StoreError::NotFound);
    }
    if held >= MAX_NOTES {
        return Err(StoreError::Bad("Each saved item holds at most 200 notes."));
    }
    tx.execute(
        "INSERT INTO saved_item_notes (item_id, author_type, author_id, text, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![item_id, author_type, author_id, text, now],
    )?;
    Ok(SavedNote {
        id: tx.last_insert_rowid(),
        item_id: item_id.to_owned(),
        author_type: author_type.to_owned(),
        author_id: author_id.map(str::to_owned),
        text: text.to_owned(),
        created_at: now,
    })
}

pub fn notes(tx: &Transaction<'_>, item_id: &str) -> Result<Vec<SavedNote>, StoreError> {
    get(tx, item_id)?;
    tx.prepare(
        "SELECT id, item_id, author_type, author_id, text, created_at \
         FROM saved_item_notes WHERE item_id = ?1 ORDER BY id",
    )?
    .query_map(params![item_id], |r| {
        Ok(SavedNote {
            id: r.get(0)?,
            item_id: r.get(1)?,
            author_type: r.get(2)?,
            author_id: r.get(3)?,
            text: r.get(4)?,
            created_at: r.get(5)?,
        })
    })?
    .collect::<Result<Vec<_>, _>>()
    .map_err(Into::into)
}

/// Every reminder due by `now`, exactly once: the rows are marked before they are returned, so
/// the next sweep is silent until a reminder is set again.
pub fn take_due(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<Vec<SavedItem>> {
    let ids: Vec<String> = tx
        .prepare("SELECT id FROM saved_items WHERE remind_at IS NOT NULL AND remind_at <= ?1 AND reminded_at IS NULL")?
        .query_map(params![now], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    tx.execute(
        "UPDATE saved_items SET reminded_at = ?1 WHERE remind_at IS NOT NULL AND remind_at <= ?1 AND reminded_at IS NULL",
        params![now],
    )?;
    let marks = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    tx.prepare(&format!(
        "SELECT {COLS} FROM saved_items s WHERE s.id IN ({marks})"
    ))?
    .query_map(params_from_iter(ids.iter()), row_to_item)?
    .collect::<Result<Vec<_>, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    fn temp_db() -> (tempfile::TempDir, Database) {
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        (dir, db)
    }

    // StoreError carries rusqlite failures, so a store call drives write/snapshot directly.
    fn item<'a>(
        chat: Option<&'a str>,
        msg: Option<i64>,
        key: Option<&'a str>,
        title: &'a str,
        body: &'a str,
    ) -> NewItem<'a> {
        NewItem {
            chat_id: chat,
            message_id: msg,
            capture_key: key,
            title,
            body,
            ..Default::default()
        }
    }
    fn seed(db: &Database, now: i64, title: &str, body: &str) -> SavedItem {
        let n = NewItem {
            title,
            body,
            ..Default::default()
        };
        db.write(|tx| save(tx, &n, now)).unwrap().0
    }

    #[test]
    fn the_shape_holds_bounded_broad_reads() {
        let (_d, db) = temp_db();
        let a = seed(
            &db,
            1000,
            "Fix the sign-in copy",
            "the wording on the sign-in screen is stiff",
        );
        let b = seed(
            &db,
            2000,
            "Ship release notes",
            "write the v0.7 notes before the cut",
        );
        let c = seed(
            &db,
            3000,
            "Book the dentist",
            "call the clinic in the morning",
        );
        db.write(|tx| {
            add_note(
                tx,
                &a.id,
                "user",
                None,
                "they changed the button label since",
                3100,
            )?;
            Ok::<(), StoreError>(())
        })
        .unwrap();

        let page = db.snapshot(|tx| list(tx, None, None, 1)).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(
            page.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec![c.id.as_str(), b.id.as_str(), a.id.as_str()],
            "newest change first"
        );
        assert_eq!(page.items[2].note_count, 1);
        assert_eq!(a.revision, 1);

        // One search reaches titles, bodies and notes; state narrows; paging walks.
        let hit = db.snapshot(|tx| list(tx, None, Some("clinic"), 1)).unwrap();
        assert_eq!(
            hit.items
                .iter()
                .map(|i| i.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Book the dentist"]
        );
        let hit = db
            .snapshot(|tx| list(tx, None, Some("button label"), 1))
            .unwrap();
        assert_eq!(hit.items.len(), 1, "notes are searched too");
        let hit = db
            .snapshot(|tx| list(tx, None, Some("SIGN-IN"), 1))
            .unwrap();
        assert_eq!(hit.items.len(), 1, "the match is case-insensitive");
        db.write(|tx| {
            update(
                tx,
                &b.id,
                &Update {
                    state: Some("done".to_owned()),
                    ..Default::default()
                },
                4000,
            )
        })
        .unwrap();
        let done = db.snapshot(|tx| list(tx, Some("done"), None, 1)).unwrap();
        assert_eq!(
            done.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec![b.id.as_str()]
        );
        let second = db.snapshot(|tx| list(tx, None, None, 2)).unwrap();
        assert_eq!(second.total, 3);
        assert_eq!(
            second.items.len(),
            0,
            "three items all fit on page 1 of fifty"
        );
        let bad = db.snapshot(|tx| list(tx, Some("shipped"), None, 1));
        assert!(matches!(bad.unwrap_err(), StoreError::Bad(_)));

        // Updates bump the revision; a stale expectRevision conflicts instead of overwriting.
        let bumped = db
            .write(|tx| {
                update(
                    tx,
                    &c.id,
                    &Update {
                        title: Some("Book the dentist (call back)".to_owned()),
                        expect_revision: Some(1),
                        ..Default::default()
                    },
                    5000,
                )
            })
            .unwrap();
        assert_eq!(bumped.revision, 2);
        assert_eq!(bumped.title, "Book the dentist (call back)");
        assert_eq!(bumped.updated_at, 5000);
        let stale = db.write(|tx| {
            update(
                tx,
                &c.id,
                &Update {
                    title: Some("gone".to_owned()),
                    expect_revision: Some(1),
                    ..Default::default()
                },
                6000,
            )
        });
        assert!(matches!(stale.unwrap_err(), StoreError::Conflict));
        let gone = db.write(|tx| {
            update(
                tx,
                "no-such-id",
                &Update {
                    state: Some("done".to_owned()),
                    ..Default::default()
                },
                6000,
            )
        });
        assert!(matches!(gone.unwrap_err(), StoreError::NotFound));
        let e = db.write(|tx| {
            update(
                tx,
                &a.id,
                &Update {
                    state: Some("shipped".to_owned()),
                    ..Default::default()
                },
                1,
            )
        });
        assert!(matches!(e.unwrap_err(), StoreError::Bad(_)));

        // Out-of-bounds text is refused with a plain reason, on save and on update.
        let long_title = "t".repeat(201);
        let long_body = "x".repeat(16_385);
        for (title, body) in [
            ("", "x"),
            ("   ", "x"),
            (long_title.as_str(), "x"),
            ("ok", ""),
            ("ok", "   "),
            ("ok", long_body.as_str()),
        ] {
            let e = db.write(|tx| save(tx, &item(None, None, None, title, body), 1));
            assert!(
                matches!(e.unwrap_err(), StoreError::Bad(_)),
                "title {title:?} body {} bytes",
                body.len()
            );
        }
        let e = db.write(|tx| {
            update(
                tx,
                &a.id,
                &Update {
                    body: Some(long_body.clone()),
                    ..Default::default()
                },
                1,
            )
        });
        assert!(matches!(e.unwrap_err(), StoreError::Bad(_)));

        // Removal takes the note trail with it.
        db.write(|tx| remove(tx, &a.id)).unwrap();
        assert_eq!(
            db.snapshot(|tx| list(tx, None, Some("button label"), 1))
                .unwrap()
                .items
                .len(),
            0
        );
        let e = db.write(|tx| remove(tx, &a.id));
        assert!(matches!(e.unwrap_err(), StoreError::NotFound));
    }

    #[test]
    fn the_same_origin_or_key_answers_with_what_is_already_saved() {
        let (_d, db) = temp_db();
        let first = db
            .write(|tx| {
                save(
                    tx,
                    &item(Some("chat-1"), Some(7), None, "Turn A", "what turn A said"),
                    1000,
                )
            })
            .unwrap();
        assert!(!first.1);
        let again = db
            .write(|tx| {
                save(
                    tx,
                    &item(
                        Some("chat-1"),
                        Some(7),
                        None,
                        "a different title",
                        "different text",
                    ),
                    2000,
                )
            })
            .unwrap();
        assert!(
            again.1,
            "the second save of the same row answers with the first"
        );
        assert_eq!(again.0.id, first.0.id);
        assert_eq!(again.0.title, "Turn A", "the original text is kept");
        let keyed = db
            .write(|tx| {
                save(
                    tx,
                    &item(
                        None,
                        None,
                        Some("nightly-sync"),
                        "Sync",
                        "run the nightly sync",
                    ),
                    1000,
                )
            })
            .unwrap();
        assert!(!keyed.1);
        let keyed2 = db
            .write(|tx| {
                save(
                    tx,
                    &item(None, None, Some("nightly-sync"), "Sync", "again"),
                    2000,
                )
            })
            .unwrap();
        assert!(keyed2.1);
        assert_eq!(keyed2.0.id, keyed.0.id);
        // Different messages and chats are different items.
        for (chat, msg) in [("chat-1", 8i64), ("chat-2", 7i64)] {
            let other = db
                .write(|tx| save(tx, &item(Some(chat), Some(msg), None, "Turn", "text"), 1000))
                .unwrap();
            assert!(!other.1);
        }
        assert_eq!(db.snapshot(|tx| list(tx, None, None, 1)).unwrap().total, 4);
    }

    #[test]
    fn reminders_fire_once_and_notes_are_bounded() {
        let (_d, db) = temp_db();
        let item = seed(&db, 1000, "Pay the invoice", "due on the first");
        db.write(|tx| {
            update(
                tx,
                &item.id,
                &Update {
                    remind_at: Some(Some(5000)),
                    ..Default::default()
                },
                1100,
            )
        })
        .unwrap();
        let quiet = db.write(|tx| take_due(tx, 4000)).unwrap();
        assert!(quiet.is_empty(), "a future reminder is not due");
        let due = db.write(|tx| take_due(tx, 6000)).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, item.id);
        assert_eq!(due[0].reminded_at, Some(6000));
        assert!(
            db.write(|tx| take_due(tx, 7000)).unwrap().is_empty(),
            "the same reminder never fires twice"
        );
        db.write(|tx| {
            update(
                tx,
                &item.id,
                &Update {
                    remind_at: Some(Some(8000)),
                    ..Default::default()
                },
                7100,
            )
        })
        .unwrap();
        assert_eq!(
            db.write(|tx| take_due(tx, 9000)).unwrap().len(),
            1,
            "setting a reminder again re-arms it"
        );
        db.write(|tx| {
            update(
                tx,
                &item.id,
                &Update {
                    remind_at: Some(None),
                    ..Default::default()
                },
                9100,
            )
        })
        .unwrap();
        assert!(
            db.write(|tx| take_due(tx, 9900)).unwrap().is_empty(),
            "clearing it stops it firing"
        );

        // Notes: bounded per note and per item, on real items only.
        for n in 0..MAX_NOTES {
            db.write(|tx| {
                add_note(
                    tx,
                    &item.id,
                    "user",
                    Some("me"),
                    &format!("note {n}"),
                    10000,
                )
            })
            .unwrap();
        }
        let e = db.write(|tx| add_note(tx, &item.id, "user", None, "one too many", 1));
        assert!(
            matches!(e.unwrap_err(), StoreError::Bad(_)),
            "200 notes per item"
        );
        let long = "x".repeat(8001);
        for (text, author) in [
            ("", "user"),
            ("   ", "user"),
            (long.as_str(), "user"),
            ("fine", "ghost"),
        ] {
            let e = db.write(|tx| add_note(tx, &item.id, author, None, text, 1));
            assert!(
                matches!(e.unwrap_err(), StoreError::Bad(_)),
                "{text:?} {author}"
            );
        }
        let e = db.write(|tx| add_note(tx, "no-such-id", "user", None, "hello", 1));
        assert!(matches!(e.unwrap_err(), StoreError::NotFound));
        let listed = db.snapshot(|tx| notes(tx, &item.id)).unwrap();
        assert_eq!(listed.len(), MAX_NOTES as usize);
        assert_eq!(listed[0].text, "note 0");
        assert_eq!(
            db.snapshot(|tx| list(tx, None, Some("note 199"), 1))
                .unwrap()
                .items
                .len(),
            1
        );
    }
}
