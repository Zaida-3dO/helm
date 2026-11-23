//! The build queue: features to run one after another, started by the host rather than by hand.
//!
//! An entry stands for one feature: a title, a brief or a board item to work on, and an optional
//! project and model. One entry runs at a time. Each look at the queue (every minute, and at
//! once when any chat's status changes) asks one question about the running entry: has its chat
//! and every chat under it — children, grandchildren, and chats any of those started — gone quiet,
//! and stayed quiet for two whole minutes? Quiet means no turn open, no waiting turn start held
//! in the capacity queue, and no declared wait still open; an archived chat counts as quiet, and
//! so does one that stopped. A chat that was interrupted a short while ago does not: Helm may
//! carry it on by itself, so the entry waits for that rather than moving on. Then the entry is `done`, and the next `waiting` entry starts as a new chat:
//! its title, the brief (or "Work on board item <id>") as the first message, the entry's model
//! and project. Nothing is started ahead of the order; a `paused` entry is skipped and starts
//! again at `resume`; `remove` stops tracking an entry but never stops a chat it already started.
//! After a restart the running entry is picked up where it was, and its two quiet minutes must
//! start over — the chats it was tracking may all read idle the moment the app is back.

use crate::runtime::ChatRuntime;
use crate::types::{BuildQueueEntry, ChatSendArgs, SendAction, SendOrigin};
use helm_core::db::Database;
use rusqlite::{params, OptionalExtension};
use serde_json::json;

/// How long the running entry's whole chat tree must stand quiet before the entry is done.
pub const DONE_QUIET_MS: i64 = 120_000;

/// How long an entry whose start failed waits before the first retry; it doubles after each
/// further failure, up to [`RETRY_MAX_MS`].
const RETRY_BASE_MS: i64 = 30_000;
const RETRY_MAX_MS: i64 = 10 * 60_000;

pub const WAITING: &str = "waiting";
pub const RUNNING: &str = "running";
pub const PAUSED: &str = "paused";
pub const DONE: &str = "done";
pub const REMOVED: &str = "removed";

const COLS: &str = "id, profile_id, title, brief, item_id, project_id, model, position, state, \
                    chat_id, started_at, finished_at";

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BuildQueueEntry> {
    Ok(BuildQueueEntry {
        id: r.get(0)?,
        profile_id: r.get(1)?,
        title: r.get(2)?,
        brief: r.get(3)?,
        item_id: r.get(4)?,
        project_id: r.get(5)?,
        model: r.get(6)?,
        position: r.get(7)?,
        state: r.get(8)?,
        chat_id: r.get(9)?,
        started_at: r.get(10)?,
        finished_at: r.get(11)?,
    })
}

/// Every entry the queue still shows, front of the line first; removed entries are gone.
pub fn list(db: &Database) -> Vec<BuildQueueEntry> {
    db.snapshot(|tx| {
        let mut st = tx.prepare(&format!(
            "SELECT {COLS} FROM build_queue WHERE state <> 'removed' ORDER BY position, id"
        ))?;
        let rows = st.query_map([], map_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .unwrap_or_default()
}

pub fn entry_by_id(db: &Database, id: &str) -> Option<BuildQueueEntry> {
    db.snapshot(|tx| {
        tx.query_row(
            &format!("SELECT {COLS} FROM build_queue WHERE id = ?1"),
            [id],
            map_row,
        )
        .optional()
    })
    .ok()
    .flatten()
}

/// Add an entry at the back of the line. `profile_id` says who added it.
pub fn add(
    db: &Database,
    profile_id: &str,
    title: &str,
    brief: Option<&str>,
    item_id: Option<&str>,
    project_id: Option<&str>,
    model: Option<&str>,
) -> rusqlite::Result<BuildQueueEntry> {
    let id = uuid::Uuid::new_v4().to_string();
    db.write(|tx| {
        let pos: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM build_queue",
            [],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO build_queue(id, profile_id, title, brief, item_id, project_id, model, position, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'waiting')",
            params![id, profile_id, title, brief, item_id, project_id, model, pos],
        )?;
        Ok::<_, rusqlite::Error>(())
    })?;
    entry_by_id(db, &id).ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)
}

/// Drag one entry to stand just before another (or at the back with `before_id` `None`), then
/// number the line densely so the order shown is the order that stands. False when the entry or
/// the anchor is no longer in the queue.
pub fn move_entry(db: &Database, id: &str, before_id: Option<&str>) -> rusqlite::Result<bool> {
    let mut order = list(db);
    let at = match order.iter().position(|e| e.id == id) {
        Some(i) => i,
        None => return Ok(false),
    };
    let mine = order.remove(at);
    if before_id == Some(id) {
        return Ok(true);
    }
    let to = match before_id {
        None => order.len(),
        Some(b) => match order.iter().position(|e| e.id == b) {
            Some(i) => i,
            None => return Ok(false),
        },
    };
    order.insert(to, mine);
    db.write(|tx| {
        for (i, e) in order.iter().enumerate() {
            let pos = i64::try_from(i).unwrap_or(i64::MAX);
            if e.position != pos {
                tx.execute(
                    "UPDATE build_queue SET position = ?2 WHERE id = ?1",
                    params![e.id, pos],
                )?;
            }
        }
        Ok::<_, rusqlite::Error>(())
    })?;
    Ok(true)
}

/// Stop tracking an entry while it waits or runs: it keeps its place and its chat, but a paused
/// entry is never settled or started on the queue's own. Any quiet spell counted for it is
/// dropped, so it starts over when the entry comes back.
pub fn pause(rt: &ChatRuntime, id: &str) -> rusqlite::Result<Option<String>> {
    let Some(e) = entry_by_id(rt.db(), id) else {
        return Ok(None);
    };
    match e.state.as_str() {
        PAUSED => Ok(Some(PAUSED.to_owned())),
        WAITING | RUNNING => {
            let wrote = rt.db().write(|tx| {
                Ok::<_, rusqlite::Error>(tx.execute(
                    "UPDATE build_queue SET state = 'paused' WHERE id = ?1 AND state IN ('waiting', 'running')",
                    [id],
                )? > 0)
            })?;
            rt.sh.build_queue_quiet.lock().unwrap().remove(id);
            Ok(wrote.then(|| PAUSED.to_owned()))
        }
        other => Ok(Some(other.to_owned())),
    }
}

/// The entry waits again (from `paused`): back in line (`waiting`) when its chat never started,
/// tracked again (`running`) when it did — the two quiet minutes then start over, so a resume
/// never finishes an entry on a quiet spell from before the pause.
pub fn resume(rt: &ChatRuntime, id: &str) -> rusqlite::Result<Option<String>> {
    let Some(e) = entry_by_id(rt.db(), id) else {
        return Ok(None);
    };
    if e.state != PAUSED {
        return Ok(Some(e.state));
    }
    let to = if e.chat_id.is_some() {
        RUNNING
    } else {
        WAITING
    };
    let wrote = rt.db().write(|tx| {
        Ok::<_, rusqlite::Error>(
            tx.execute(
                "UPDATE build_queue SET state = ?2 WHERE id = ?1 AND state = 'paused'",
                params![id, to],
            )? > 0,
        )
    })?;
    rt.sh.build_queue_quiet.lock().unwrap().remove(id);
    Ok(wrote.then(|| to.to_owned()))
}

/// Stop tracking an entry for good. A chat it already started keeps running.
pub fn remove(rt: &ChatRuntime, id: &str, now: i64) -> rusqlite::Result<bool> {
    let wrote = rt.db().write(|tx| {
        Ok::<_, rusqlite::Error>(tx.execute(
            "UPDATE build_queue SET state = 'removed', finished_at = ?2 WHERE id = ?1 AND state <> 'removed'",
            params![id, now],
        )? > 0)
    })?;
    rt.sh.build_queue_quiet.lock().unwrap().remove(id);
    rt.sh.build_queue_failed.lock().unwrap().remove(id);
    Ok(wrote)
}

fn mark_done(db: &Database, id: &str, now: i64) -> rusqlite::Result<bool> {
    db.write(|tx| {
        Ok::<_, rusqlite::Error>(tx.execute(
            "UPDATE build_queue SET state = 'done', finished_at = ?2 WHERE id = ?1 AND state = 'running'",
            params![id, now],
        )? > 0)
    })
}

fn mark_running(db: &Database, id: &str, chat_id: &str, now: i64) -> rusqlite::Result<bool> {
    db.write(|tx| {
        Ok::<_, rusqlite::Error>(tx.execute(
            "UPDATE build_queue SET state = 'running', chat_id = ?2, started_at = ?3 WHERE id = ?1 AND state = 'waiting'",
            params![id, chat_id, now],
        )? > 0)
    })
}

/// Whether this chat or any chat under it (by parenthood, by having been started by it, or by
/// having taken over from it in a roll-over) is still working: a turn open, a chat interrupted
/// within the window Helm still carries one on by itself, a waiting turn start held, or a
/// declared wait still open. The walk is a recursive set; `UNION` deduplicates it, so a parent
/// chain cannot spin forever. The waits table belongs to another crate: where it has not been
/// made there are no waits.
fn tree_working(db: &Database, chat_id: &str, now: i64) -> bool {
    db.snapshot(|tx| {
        let waits: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_waits')",
            [],
            |r| r.get(0),
        )?;
        let sql = format!(
            "WITH RECURSIVE tree(id) AS (
                 SELECT ?1
                 UNION SELECT c.id FROM chats c JOIN tree t ON c.parent_chat_id = t.id
                 UNION SELECT c.id FROM chats c JOIN tree t ON c.created_by_chat = t.id
                 UNION SELECT s.chat_id FROM chat_succession s JOIN tree t ON s.predecessor_id = t.id
             )
             SELECT EXISTS(
                 SELECT 1 FROM tree t JOIN chats c ON c.id = t.id
                  WHERE c.archived = 0
                    AND (c.status IN ('starting', 'running', 'stopping')
                         OR (c.status = 'interrupted' AND c.updated_at >= ?2))
                 UNION
                 SELECT 1 FROM tree t JOIN chat_dispatch_holds h ON h.chat_id = t.id{}
             )",
            if waits {
                " UNION SELECT 1 FROM tree t JOIN agent_waits w ON w.chat_id = t.id AND w.state = 'open'"
            } else {
                ""
            }
        );
        let since = now - crate::outage::RECENT_MS;
        tx.query_row(&sql, params![chat_id, since], |r| r.get::<_, bool>(0))
    })
    // A read that cannot be answered keeps the entry running: settling it on a database hiccup
    // would start the next feature over live work.
    .unwrap_or(true)
}

/// Who starts a waiting entry's chat and sends its first message; answers the new chat's id.
pub trait Starter: Send + Sync {
    fn start(&self, entry: &BuildQueueEntry) -> Result<String, String>;

    /// Take back a chat that `start` made but that no entry took up (the entry was paused or
    /// removed while the chat was being made), so it is not left running unowned.
    fn discard(&self, _chat_id: &str) {}
}

/// The real starter: `chat_create` through the app's op context (attached with the host tools),
/// a rename to the entry's title, and the first message. A send that fails takes the fresh chat
/// back (archive, then delete) rather than leaving it titleless and silent.
pub struct ChatStarter {
    pub rt: ChatRuntime,
}

impl Starter for ChatStarter {
    fn start(&self, entry: &BuildQueueEntry) -> Result<String, String> {
        let rt = &self.rt;
        let seam = rt
            .sh
            .host
            .get()
            .cloned()
            .ok_or("Helm's op context is not ready yet")?;
        let cwd = folder_for(rt.db(), &seam.ctx, entry.project_id.as_deref())?;
        let mut args = json!({"cwd": cwd, "projectId": entry.project_id});
        if let Some(m) = entry.model.as_deref() {
            args["model"] = json!(m);
        }
        let made = crate::ops::chat_create(&seam.ctx, args).map_err(|e| e.message)?;
        let id = made["id"]
            .as_str()
            .ok_or("the new chat came back without an id")?
            .to_owned();
        let _ = crate::titles::rename(rt, &id, &entry.title);
        let text = entry
            .brief
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "Work on board item {}",
                    entry.item_id.clone().unwrap_or_default()
                )
            });
        let sent = crate::submit::chat_send(
            rt,
            ChatSendArgs {
                chat_id: id.clone(),
                submission_id: uuid::Uuid::new_v4().to_string(),
                text,
                action: SendAction::Default,
                draft_revision: 0,
                origin: Some(SendOrigin::Action),
                attachments: None,
                context: None,
                page_title: None,
                page_overview: None,
            },
            None,
        );
        if let Err(e) = sent {
            let ctx2 = seam.ctx.clone();
            let args = json!({"chatId": id});
            let _ = crate::manage::chat_archive(&ctx2, args.clone())
                .and_then(|_| crate::manage::chat_delete(&ctx2, args));
            return Err(format!(
                "the chat was made but its first message could not be sent: {}",
                e.message
            ));
        }
        Ok(id)
    }

    fn discard(&self, chat_id: &str) {
        let Some(seam) = self.rt.sh.host.get().cloned() else {
            return;
        };
        let args = json!({"chatId": chat_id});
        let _ = crate::manage::chat_archive(&seam.ctx, args);
    }
}

/// The folder a start runs in: the entry's project, which must be a folder project (a board
/// project works too, when the board names a folder — `chat_create` shows the chat on the board
/// and runs it in the board's folder). An entry with no usable project cannot start; the caller
/// leaves it waiting and says so.
fn folder_for(
    db: &Database,
    ctx: &helm_ipc::OpCtx,
    project_id: Option<&str>,
) -> Result<String, String> {
    let p = project_id.ok_or("the entry names no project to run in")?;
    let boards = crate::store::Boards::of(&ctx.services);
    db.snapshot(|tx| {
        let row: Option<(String, Option<String>)> = tx
            .query_row("SELECT kind, path FROM projects WHERE id = ?1", [p], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        Ok::<_, rusqlite::Error>(match row {
            Some((kind, Some(path))) if kind == "folder" && !path.trim().is_empty() => {
                Ok(path.trim().to_owned())
            }
            // A board: its folder, if it has one and still takes chats.
            _ => match boards.board(tx, p)? {
                Some(b)
                    if !b.archived
                        && !b.closed
                        && !b.stale
                        && b.folder_path
                            .as_deref()
                            .is_some_and(|f| !f.trim().is_empty()) =>
                {
                    Ok(b.folder_path.unwrap().trim().to_owned())
                }
                _ => Err("the entry's project has no folder to run in".to_owned()),
            },
        })
    })
    .map_err(|e| e.to_string())?
}

/// One look at the queue, at time `now`: settle any running entry whose chat tree has stayed
/// quiet long enough, then — only when nothing runs — start exactly one next waiting entry.
/// `quiet` remembers, per entry, since when its tree has been quiet; it starts empty after a
/// restart, so a running entry's two minutes must be lived again.
pub fn tick(rt: &ChatRuntime, now: i64, starter: &dyn Starter) {
    let db = rt.db();
    let mut rows = list(db);
    if !rows
        .iter()
        .any(|e| e.state == RUNNING || e.state == WAITING)
    {
        return;
    }
    let mut changed = false;
    {
        let mut quiet = rt.sh.build_queue_quiet.lock().unwrap();
        for e in rows.iter_mut().filter(|e| e.state == RUNNING) {
            let Some(chat) = e.chat_id.clone() else {
                // The chat it was running is gone: nothing left to track, and starting a
                // replacement behind the person's back is worse than settling it.
                quiet.remove(&e.id);
                if mark_done(db, &e.id, now).unwrap_or(false) {
                    e.state = DONE.to_owned();
                    changed = true;
                    log::warn!("build queue: {} lost its chat; settled as done", e.title);
                }
                continue;
            };
            if tree_working(db, &chat, now) {
                quiet.remove(&e.id);
                continue;
            }
            let first = *quiet.entry(e.id.clone()).or_insert(now);
            if now - first >= DONE_QUIET_MS {
                quiet.remove(&e.id);
                if mark_done(db, &e.id, now).unwrap_or(false) {
                    e.state = DONE.to_owned();
                    changed = true;
                    log::info!("build queue: {} is done", e.title);
                }
            }
        }
    }
    if changed {
        publish(rt);
    }
    // One at a time: only a free slot starts the next entry, and only its front entry. A paused
    // entry holds its slot while its chat still works, so pausing never lets the next entry
    // start over a live chat; once that chat goes quiet the slot is free, and a resumed entry
    // simply rejoins the tracking (the queue never starts a chat twice for one entry).
    let slot_held = rows.iter().any(|e| match e.state.as_str() {
        RUNNING => true,
        PAUSED => e
            .chat_id
            .as_deref()
            .is_some_and(|c| tree_working(db, c, now)),
        _ => false,
    });
    if slot_held {
        return;
    }
    let Some(next) = rows.iter().find(|e| e.state == WAITING).cloned() else {
        return;
    };
    // An entry whose start keeps failing is tried less often, not on every look.
    let backing_off = rt
        .sh
        .build_queue_failed
        .lock()
        .unwrap()
        .get(&next.id)
        .is_some_and(|&(_, retry_at)| now < retry_at);
    if backing_off {
        return;
    }
    match starter.start(&next) {
        Ok(chat) => {
            rt.sh.build_queue_failed.lock().unwrap().remove(&next.id);
            if mark_running(db, &next.id, &chat, now).unwrap_or(false) {
                rt.sh.build_queue_quiet.lock().unwrap().remove(&next.id);
                publish(rt);
                log::info!("build queue: {} started as chat {chat}", next.title);
            } else {
                // The entry stopped waiting while its chat was being made: do not leave the
                // chat running with no entry to track it.
                starter.discard(&chat);
                log::warn!(
                    "build queue: {} changed while its chat was starting; the chat was set aside",
                    next.title
                );
            }
        }
        Err(msg) => {
            // Not skipped: nothing is started ahead of an entry that cannot start yet.
            let mut failed = rt.sh.build_queue_failed.lock().unwrap();
            let tries = failed.get(&next.id).map_or(0, |&(n, _)| n) + 1;
            let wait = (RETRY_BASE_MS << (tries - 1).min(10)).min(RETRY_MAX_MS);
            failed.insert(next.id.clone(), (tries, now + wait));
            log::warn!(
                "build queue: could not start {} (try {tries}, next in {} s): {msg}",
                next.title,
                wait / 1000
            );
        }
    }
}

/// Tell the UI the queue changed.
pub fn publish(rt: &ChatRuntime) {
    rt.sh
        .events
        .emit(helm_ipc::AppEvent::new("build_queue_changed"));
}

/// The profile new entries belong to: the one the app treats as active, or no one. This is the
/// same reading the chat identity uses, so an entry and the chat it starts carry the same name.
pub fn active_profile(db: &Database) -> String {
    db.snapshot(|tx| {
        Ok::<_, rusqlite::Error>(
            helm_core::prefs::read(tx, "activeProfile")
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
        )
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// The fake start: remembers what it was asked for, and writes a real idle chat row so the
    /// tree checks see it. `fail` makes every start answer a plain reason.
    struct FakeStarter {
        db: Arc<Database>,
        asked: Mutex<Vec<String>>,
        fail: Option<String>,
    }

    impl FakeStarter {
        fn new(db: &Arc<Database>) -> Self {
            Self {
                db: db.clone(),
                asked: Mutex::new(Vec::new()),
                fail: None,
            }
        }
        fn failing(db: &Arc<Database>, why: &str) -> Self {
            Self {
                db: db.clone(),
                asked: Mutex::new(Vec::new()),
                fail: Some(why.to_owned()),
            }
        }
    }

    impl Starter for FakeStarter {
        fn start(&self, e: &BuildQueueEntry) -> Result<String, String> {
            self.asked.lock().unwrap().push(e.title.clone());
            if let Some(f) = &self.fail {
                return Err(f.clone());
            }
            let id = format!("chat-{}", e.title);
            self.db
                .write(|tx| {
                    tx.execute(
                        "INSERT INTO chats(id, project_id, created_at, updated_at, status) VALUES (?1, 'p', 0, 0, 'idle')",
                        [&id],
                    )
                })
                .map_err(|e| e.to_string())?;
            Ok(id)
        }
    }

    /// A harness with the rows the queue tests share: one folder project and one idle chat.
    fn harness_seeded() -> crate::runtime_tests::H {
        let h = crate::runtime_tests::harness();
        h.db
            .write(|tx| {
                tx.execute_batch(
                    "INSERT INTO projects(id, kind, name, path, created_at) VALUES ('p', 'folder', 'P', 'C:/w', 0);
                     INSERT INTO chats(id, project_id, created_at, updated_at, status)
                       VALUES ('run1', 'p', 0, 0, 'idle');",
                )
            })
            .unwrap();
        h
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_entry_joins_the_back_and_a_drag_reorders_the_line() {
        let h = harness_seeded();
        let db = h.db.clone();
        let a = add(&db, "me", "First", Some("do it"), None, Some("p"), None).unwrap();
        let b = add(
            &db,
            "",
            "Second",
            None,
            Some("item-9"),
            Some("p"),
            Some("opus"),
        )
        .unwrap();
        assert_eq!((a.position, b.position), (0, 1));
        assert_eq!(
            (a.profile_id.as_str(), b.item_id.as_deref()),
            ("me", Some("item-9"))
        );
        assert!(move_entry(&db, &b.id, Some(&a.id)).unwrap());
        let order: Vec<String> = list(&db).into_iter().map(|e| e.title).collect();
        assert_eq!(order, ["Second", "First"], "the drag puts Second in front");
        assert!(
            !move_entry(&db, "gone", Some(&a.id)).unwrap(),
            "an unknown entry cannot move"
        );
        assert!(
            !move_entry(&db, &a.id, Some("gone")).unwrap(),
            "an unknown anchor refuses"
        );
        assert!(
            move_entry(&db, &a.id, Some(&a.id)).unwrap(),
            "dropping a row on itself changes nothing"
        );
        remove(&h.rt, &a.id, 5).unwrap();
        assert_eq!(list(&db).len(), 1, "a removed entry leaves the list");
        let c = add(&db, "", "Third", Some("x"), None, Some("p"), None).unwrap();
        assert!(c.position > b.position, "a new entry still joins the back");
    }

    #[test]
    fn a_tree_with_a_busy_grandchild_is_not_done_and_a_quiet_one_is() {
        let (_d, db) = crate::schema::tests::temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, kind, name, path, created_at) VALUES ('p', 'folder', 'P', 'C:/w', 0);
                 INSERT INTO chats(id, project_id, created_at, updated_at, status) VALUES
                   ('c1', 'p', 0, 0, 'idle'), ('c2', 'p', 0, 0, 'idle'), ('c3', 'p', 0, 0, 'idle'),
                   ('g', 'p', 0, 0, 'running');
                 UPDATE chats SET parent_chat_id = 'c1' WHERE id = 'c2';
                 UPDATE chats SET parent_chat_id = 'c2' WHERE id = 'g';",
            )
        })
        .unwrap();
        assert!(
            tree_working(&db, "c1", 0),
            "a busy grandchild keeps the whole tree working"
        );
        assert!(!tree_working(&db, "c3", 0), "a bare idle chat stands alone");
        // A started-by chat is in the tree too, even without a parent link.
        db.write(|tx| {
            tx.execute(
                "INSERT INTO chats(id, project_id, created_by_chat, created_at, updated_at, status)
                 VALUES ('kid', 'p', 'c3', 0, 0, 'running')",
                [],
            )
        })
        .unwrap();
        assert!(tree_working(&db, "c3", 0), "a chat it started works");
        db.write(|tx| tx.execute("UPDATE chats SET status = 'idle' WHERE id = 'kid'", []))
            .unwrap();
        // A dispatch hold is a working start, and so is an open declared wait.
        crate::queue::write_hold(
            &db,
            "c3",
            crate::queue::AT_LIMIT,
            crate::queue::QueueClass::Normal,
            None,
            1,
            0,
        );
        assert!(tree_working(&db, "c3", 0), "a held start counts as working");
        crate::queue::clear_hold(&db, "c3");
        assert!(!tree_working(&db, "c3", 0), "released again");
        db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE agent_waits (id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, state TEXT NOT NULL);
                 INSERT INTO agent_waits(id, chat_id, state) VALUES ('w', 'c3', 'open');",
            )
        })
        .unwrap();
        assert!(tree_working(&db, "c3", 0), "an open wait keeps it running");
        db.write(|tx| tx.execute("UPDATE agent_waits SET state = 'settled'", []))
            .unwrap();
        assert!(
            !tree_working(&db, "c3", 0),
            "a settled wait says nothing about the chat"
        );
        // An archived working chat is out of the tree: the queue asks whether work is moving,
        // not why it stopped.
        db.write(|tx| tx.execute("UPDATE chats SET archived = 1 WHERE id = 'g'", []))
            .unwrap();
        assert!(
            !tree_working(&db, "c1", 0),
            "an archived working chat is quiet"
        );
        // An unknown chat works nowhere: an entry naming a deleted chat is not kept alive by it.
        assert!(!tree_working(&db, "gone", 0), "no tree, no work");
        // A roll-over successor is in the tree through its link, with no parent or starter link.
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO chats(id, project_id, created_at, updated_at, status)
                   VALUES ('next', 'p', 0, 0, 'running');
                 INSERT INTO chat_succession(chat_id, predecessor_id) VALUES ('next', 'c3');",
            )
        })
        .unwrap();
        assert!(
            tree_working(&db, "c3", 0),
            "work that moved to the successor is still the entry's work"
        );
        db.write(|tx| {
            tx.execute(
                "UPDATE chats SET status = 'interrupted' WHERE id = 'next'",
                [],
            )
        })
        .unwrap();
        assert!(
            tree_working(&db, "c3", 1_000),
            "a chat interrupted a moment ago may be carried on, so the entry waits"
        );
        assert!(
            !tree_working(&db, "c3", crate::outage::RECENT_MS + 1_000),
            "one interrupted long ago will not be, so it counts as quiet"
        );
    }

    /// The whole tick on hand-written rows: the two-minute window from a restart, and one start
    /// per look, front of the line only.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_tick_settles_and_starts_exactly_one_at_a_time() {
        let h = harness_seeded();
        let rt = &h.rt;
        let db = h.db.clone();
        let e1 = add(&db, "", "One", Some("first brief"), None, Some("p"), None).unwrap();
        let e2 = add(&db, "", "Two", Some("second brief"), None, Some("p"), None).unwrap();
        let e3 = add(&db, "", "Three", None, Some("item-3"), Some("p"), None).unwrap();
        db.write(|tx| {
            tx.execute(
                "UPDATE build_queue SET state = 'running', chat_id = 'run1', started_at = 1000 WHERE id = ?1",
                [&e1.id],
            )
        })
        .unwrap();
        let starter = FakeStarter::new(&db);
        // A restart: the entry runs on the books and every chat reads idle, so the two-minute
        // window only starts counting from this first look — a restart never finishes an entry
        // at once.
        tick(rt, 2_000, &starter);
        assert_eq!(starter.asked.lock().unwrap().len(), 0, "the slot is taken");
        assert_eq!(entry_by_id(&db, &e1.id).unwrap().state, RUNNING);
        assert!(
            rt.sh.build_queue_quiet.lock().unwrap().contains_key(&e1.id),
            "quiet is being counted from now"
        );
        // One minute of quiet: not done yet.
        tick(rt, 62_000, &starter);
        assert_eq!(entry_by_id(&db, &e1.id).unwrap().state, RUNNING);
        // Two minutes from the window's start: done, and exactly one next entry starts.
        tick(rt, 122_000, &starter);
        let done = entry_by_id(&db, &e1.id).unwrap();
        assert_eq!(
            (done.state.as_str(), done.finished_at),
            (DONE, Some(122_000))
        );
        let started = entry_by_id(&db, &e2.id).unwrap();
        assert_eq!(started.state, RUNNING, "the front of the line starts");
        assert_eq!(started.chat_id.as_deref(), Some("chat-Two"));
        assert_eq!(
            entry_by_id(&db, &e3.id).unwrap().state,
            WAITING,
            "nothing starts ahead of it"
        );
        // The new entry's chat working again ends the quiet spell.
        db.write(|tx| {
            tx.execute(
                "UPDATE chats SET status = 'running' WHERE id = 'chat-Two'",
                [],
            )
        })
        .unwrap();
        tick(rt, 123_000, &starter);
        assert!(
            !rt.sh.build_queue_quiet.lock().unwrap().contains_key(&e2.id),
            "a working tree clears the quiet mark"
        );
        assert_eq!(entry_by_id(&db, &e2.id).unwrap().state, RUNNING);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_paused_entry_holds_its_slot_while_its_chat_works() {
        let h = harness_seeded();
        let rt = &h.rt;
        let db = h.db.clone();
        let e1 = add(&db, "", "One", Some("b"), None, Some("p"), None).unwrap();
        let e2 = add(&db, "", "Two", Some("b"), None, Some("p"), None).unwrap();
        db.write(|tx| {
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at, status) VALUES ('live', 'p', 0, 0, 'running')",
                [],
            )?;
            tx.execute(
                "UPDATE build_queue SET chat_id = 'live', started_at = 10 WHERE id = ?1",
                [&e1.id],
            )
        })
        .unwrap();
        // Start it, then pause it by hand: a paused entry keeps its place and its chat.
        db.write(|tx| {
            tx.execute(
                "UPDATE build_queue SET state = 'running' WHERE id = ?1",
                [&e1.id],
            )
        })
        .unwrap();
        assert_eq!(pause(rt, &e1.id).unwrap(), Some(PAUSED.to_owned()));
        // Paused, but its chat is still working: the queue starts nothing over it.
        tick(rt, 500_000, &FakeStarter::new(&db));
        assert_eq!(
            entry_by_id(&db, &e2.id).unwrap().state,
            WAITING,
            "the paused entry still holds the slot while its chat works"
        );
        // Its chat goes quiet: the slot frees, and the waiting entry starts. The paused entry
        // never settles by itself — it is the person's to end or resume.
        db.write(|tx| tx.execute("UPDATE chats SET status = 'idle' WHERE id = 'live'", []))
            .unwrap();
        tick(rt, 501_000, &FakeStarter::new(&db));
        assert_eq!(
            entry_by_id(&db, &e1.id).unwrap().state,
            PAUSED,
            "a paused entry is left alone however long its chat stays quiet"
        );
        assert_eq!(
            entry_by_id(&db, &e2.id).unwrap().state,
            RUNNING,
            "the line moves on once the paused chat is quiet"
        );
        // Resume takes a paused entry back to being tracked, not to the back of the line; one
        // that never started a chat waits again instead. Any quiet spell counted before the
        // pause is dropped, so the two-minute window starts over.
        rt.sh
            .build_queue_quiet
            .lock()
            .unwrap()
            .insert(e1.id.clone(), 10);
        assert_eq!(resume(rt, &e1.id).unwrap(), Some(RUNNING.to_owned()));
        assert!(
            !rt.sh.build_queue_quiet.lock().unwrap().contains_key(&e1.id),
            "resume drops the old quiet mark"
        );
        tick(rt, 999_000, &FakeStarter::new(&db));
        assert_eq!(
            entry_by_id(&db, &e1.id).unwrap().state,
            RUNNING,
            "the window starts over from this look, not on the old quiet"
        );
        assert_eq!(
            rt.sh.build_queue_quiet.lock().unwrap().get(&e1.id).copied(),
            Some(999_000)
        );
        let e3 = add(&db, "", "Three", Some("b"), None, Some("p"), None).unwrap();
        db.write(|tx| {
            tx.execute(
                "UPDATE build_queue SET state = 'paused' WHERE id = ?1",
                [&e3.id],
            )
        })
        .unwrap();
        assert_eq!(resume(rt, &e3.id).unwrap(), Some(WAITING.to_owned()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_entry_that_cannot_start_holds_the_line_and_a_lost_chat_settles() {
        let h = harness_seeded();
        let rt = &h.rt;
        let db = h.db.clone();
        let e1 = add(&db, "", "One", Some("b"), None, Some("p"), None).unwrap();
        let e2 = add(&db, "", "Two", Some("b"), None, Some("p"), None).unwrap();
        let starter = FakeStarter::failing(&db, "the entry's project has no folder to run in");
        tick(rt, 1_000, &starter);
        assert_eq!(
            entry_by_id(&db, &e1.id).unwrap().state,
            WAITING,
            "a failed start leaves the entry waiting"
        );
        assert_eq!(
            entry_by_id(&db, &e2.id).unwrap().state,
            WAITING,
            "nothing starts ahead of an entry that cannot start"
        );
        assert_eq!(starter.asked.lock().unwrap().len(), 1);
        tick(rt, 2_000, &starter);
        assert_eq!(
            starter.asked.lock().unwrap().len(),
            1,
            "a start that just failed is not tried again at the next look"
        );
        // A running entry whose chat was deleted (the link goes with it) settles at once, and
        // the next entry starts in its place.
        db.write(|tx| {
            tx.execute(
                "UPDATE build_queue SET state = 'running', started_at = 1500 WHERE id = ?1",
                [&e1.id],
            )
        })
        .unwrap();
        tick(rt, 3_000, &FakeStarter::new(&db));
        assert_eq!(
            entry_by_id(&db, &e1.id).unwrap().state,
            DONE,
            "a running entry with no chat left settles as done"
        );
        assert_eq!(
            entry_by_id(&db, &e2.id).unwrap().state,
            RUNNING,
            "the front waiting entry starts"
        );
    }
}
