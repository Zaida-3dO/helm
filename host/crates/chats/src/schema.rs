//! The chats crate's side tables and startup steps (host/README.md "Adding SQLite tables").
//!
//! | Table | Holds |
//! |---|---|
//! | `submission_meta` | per submission: origin (`user`/`harness`), send priority (`now` or null), the run it was dispatched in |
//! | `restart_continuations` | chats owed one restart-continuation prompt (at most 100 rows) |
//! | `chat_titles` | per chat: where the title came from (`fallback`/`generated`/`manual`) |
//! | `unread_marks` | manual unread marks with their token |
//! | `chat_completions` | the current completion per chat |
//! | `chat_mutes` | muted chats (user-only) |
//! | `chat_activity` | per chat: the last worker output time, the stalled flag, and how many local chats stand ahead in the model server's queue while this chat's turn waits on it |
//! | `chat_attention` | per chat: when its attention was marked addressed, and a snooze |
//! | `chat_stop_holds` | per chat: when its last turn end was handed to the stop judge and is still held; read by the waits watcher so a held chat reads as busy, and cleared at start-up |
//! | `chat_triage` | per chat: the idle check's last verdict, and the activity it was made for |
//! | `chat_shares` | the profiles besides its own that a chat is shown to; the chat row itself carries its `profile_id` and `host_device_id` |
//! | `chat_flags` | per chat: the question its agent flagged for the user ([`crate::flags`]) |
//! | trigger `restart_cancel_changed_chat` | drops a pending continuation on archive or a project, harness, model or effort change |
//! | `chat_forks` | fork ids and what they were asked for, for idempotent forks |
//! | `pending_edits` | an edit staged with its submission, applied once the worker closes ([`crate::edit`]) |
//! | `chat_attachments` | images attached to a chat, then to the submission that sent them ([`crate::attachments`]) |
//! | `shown_galleries`, `shown_images`, `option_boards` | pictures an agent showed in the chat and the option boards it offered, with the user's answers ([`crate::shown_images`]) |
//! | `fork_carry` | a message fork's carried history, sent once with its first prompt ([`crate::fork`]) |
//! | `chat_file_cleanup` | files a hard delete left behind, removed after commit and retried at startup ([`crate::delete`]) |
//! | `submission_context` | the panel-context block a composer send carried |
//! | `chat_succession` | a roll-over successor's predecessor, read when the successor's first message is sent ([`crate::rollover`]) |
//! | chats columns `stack_id`, `prev_session_id`, `next_session_id` | the roll-over stack: every session of a rolled-over chat shares a stack id (the oldest session's) and links to the sessions before and after it ([`crate::rollover`]) |
//! | `chat_turn_usage` | per turn: the tokens and cost it used, projected from the session log's `usage` entries ([`crate::turn_usage`]) |
//! | `chat_dispatch_holds` | the turn starts waiting for capacity, with their class, line and reason ([`crate::queue`]) |
//! | `build_queue` | the features to run one after another: their order, state, and the chat that runs each one ([`crate::buildqueue`]) |
//! | view `chat_search_source` | the searchable message text the search crate indexes ([`crate::search_source`]) |
//!
//! Recovery, inside the one startup transaction:
//! - **capture** (before core recovery rewrites statuses): every chat still `starting` or
//!   `running` gets a continuation row. `stopping` chats are excluded: the user asked them to stop.
//! - **after**: retry
//!   states left `retrying` or `waiting` on a message become `cancelled`; every
//!   persisted stalled flag and stored queue position is cleared, since no turn survives a
//!   restart.

use helm_core::db::MigrationRegistry;
use rusqlite::Transaction;
use std::collections::HashMap;

pub const MAX_CONTINUATIONS: i64 = 100;

pub fn register(reg: &mut MigrationRegistry) {
    reg.initializer("chats-side-tables", init)
        .initializer("chats-attachment-refs", crate::attachments::init)
        .initializer("chats-flags", crate::flags::init)
        .initializer("chats-shown-images", crate::shown_images::init)
        .initializer("chats-activity-queue", activity_queue)
        .initializer("chats-triage-tries", triage_tries)
        .initializer("chats-identity", identity)
        .initializer("chats-triage-priority", crate::triage::init)
        .initializer("chats-created-by", crate::mute::init)
        .initializer("chats-late-columns", late_columns)
        .initializer("chats-activity-state", activity_state)
        .initializer("chats-dispatch-holds", dispatch_holds)
        .initializer("chats-rollover-stack", rollover_stack)
        .initializer("chats-build-queue", build_queue)
        .one_time("chats-origin-user-is-typed", origin_user_is_typed)
        .recovery_capture("chats-restart-capture", capture_continuations)
        .recovery_step("chats-retry-leftovers", cancel_retry_leftovers)
        .recovery_step("chats-stall-reset", reset_stalls)
        .recovery_step("chats-stop-holds-clear", clear_stop_holds)
        .recovery_step("chats-auto-holds-clear", clear_auto_holds);
}

fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS submission_meta (
             submission_id  TEXT PRIMARY KEY REFERENCES submissions(id) ON DELETE CASCADE,
             origin         TEXT NOT NULL DEFAULT 'user',
             priority       TEXT,
             run_id         TEXT,
             afk_note       INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS restart_continuations (
             chat_id     TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             created_at  INTEGER NOT NULL
         );
         DROP TABLE IF EXISTS chat_queue_holds;
         -- Messages no longer wait behind other chats: the wait queue and its settings table go,
         -- there is nothing worth keeping in them.
         DROP TABLE IF EXISTS slot_waits;
         DROP TABLE IF EXISTS run_slot_settings;
         -- A message never waits for a turn to end: nothing uses the table that held messages
         -- waiting for their turn, or the one that remembered where in that hold each one sat; the
         -- rows that mattered stay in `submissions` and are handed to Claude at once. Both tables
         -- stay, empty, so an older version that still reads them can open this profile again.
         CREATE TABLE IF NOT EXISTS queued_submissions (
             submission_id  TEXT PRIMARY KEY REFERENCES submissions(id) ON DELETE CASCADE,
             chat_id        TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             position       INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS draft_queue_slots (
             chat_id   TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             position  INTEGER NOT NULL,
             revision  INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS messages_chat_order ON messages(chat_id, active, timestamp, id);
         CREATE TABLE IF NOT EXISTS chat_titles (
             chat_id  TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             state    TEXT NOT NULL CHECK (state IN ('fallback', 'generated', 'manual')),
             job      TEXT,
             updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS unread_marks (
             chat_id    TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             token      TEXT NOT NULL,
             marked_at  INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_completions (
             chat_id       TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             id            TEXT NOT NULL UNIQUE,
             preview       TEXT NOT NULL DEFAULT '',
             completed_at  INTEGER NOT NULL,
             read          INTEGER NOT NULL DEFAULT 0,
             dismissed     INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS chat_mutes (
             chat_id   TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             muted_at  INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_activity (
             chat_id           TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             last_activity_at  INTEGER NOT NULL,
             stalled           INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS chat_attention (
             chat_id        TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             addressed_at   INTEGER,
             snoozed_until  INTEGER,
             snoozed_at     INTEGER
         );
         CREATE TABLE IF NOT EXISTS chat_stop_holds (
             chat_id  TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             since    INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_triage (
             chat_id     TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             stamp       INTEGER NOT NULL,
             checked_at  INTEGER NOT NULL,
             needs       INTEGER NOT NULL DEFAULT 0,
             reason      TEXT NOT NULL DEFAULT '',
             failed      INTEGER NOT NULL DEFAULT 0,
             tries       INTEGER NOT NULL DEFAULT 0
         );
         -- A pending restart continuation goes when its chat is archived or its project,
         -- harness, provider, model or effort changes; deletion cascades.
         CREATE TRIGGER IF NOT EXISTS restart_cancel_changed_chat
         AFTER UPDATE OF archived, project_id, harness, provider, model, thinking ON chats
         WHEN new.archived != old.archived OR new.project_id IS NOT old.project_id
           OR new.harness IS NOT old.harness OR new.provider IS NOT old.provider
           OR new.model IS NOT old.model OR new.thinking IS NOT old.thinking
         BEGIN
             DELETE FROM restart_continuations WHERE chat_id = new.id;
         END;
         CREATE TABLE IF NOT EXISTS chat_forks (
             fork_id         TEXT PRIMARY KEY,
             source_chat_id  TEXT NOT NULL,
             message_id      INTEGER,
             side            INTEGER NOT NULL DEFAULT 0,
             chat_id         TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             created_at      INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS pending_edits (
             chat_id        TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             submission_id  TEXT NOT NULL,
             entry_id       TEXT NOT NULL,
             log_len        INTEGER NOT NULL,
             marker_id      TEXT NOT NULL,
             created_at     INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_attachments (
             id             TEXT PRIMARY KEY,
             chat_id        TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             name           TEXT NOT NULL,
             mime           TEXT NOT NULL,
             bytes          INTEGER NOT NULL,
             width          INTEGER NOT NULL,
             height         INTEGER NOT NULL,
             submission_id  TEXT,
             created_at     INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS chat_attachments_submission ON chat_attachments(submission_id);
         CREATE TABLE IF NOT EXISTS fork_carry (
             chat_id  TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             text     TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_file_cleanup (
             rel        TEXT PRIMARY KEY,
             queued_at  INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS submission_context (
             submission_id  TEXT PRIMARY KEY REFERENCES submissions(id) ON DELETE CASCADE,
             text           TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_succession (
             chat_id         TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             predecessor_id  TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_turn_usage (
             chat_id             TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             entry_id            TEXT NOT NULL,
             timestamp           INTEGER NOT NULL,
             native_session      TEXT,
             input_tokens        INTEGER NOT NULL,
             output_tokens       INTEGER NOT NULL,
             cache_read_tokens   INTEGER NOT NULL,
             cache_write_tokens  INTEGER NOT NULL,
             cost_usd            REAL NOT NULL,
             running             TEXT NOT NULL,
             provider            TEXT NOT NULL DEFAULT 'claude',
             PRIMARY KEY (chat_id, entry_id)
         );
         CREATE INDEX IF NOT EXISTS chat_turn_usage_session ON chat_turn_usage(native_session, timestamp);
         CREATE VIEW IF NOT EXISTS chat_search_source AS
             SELECT m.id AS message_id, m.chat_id AS chat_id, m.role AS role, m.text AS text,
                    m.timestamp AS timestamp, c.archived AS archived
             FROM messages m JOIN chats c ON c.id = m.chat_id
             WHERE m.active = 1 AND m.role IN ('user', 'assistant') AND m.text != ''
               AND c.side_source_id IS NULL;",
    )
}

/// Early rows spelled the user's own messages `user`; they are `typed` now. Rewritten once, so the
/// column's `user` default (a row written without an origin) can never pass for typed words in
/// the consent verifier.
fn origin_user_is_typed(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE submission_meta SET origin = 'typed' WHERE origin = 'user'",
        [],
    )?;
    Ok(())
}

/// Record a continuation for `chat_id`, keeping at most [`MAX_CONTINUATIONS`] (newest kept).
pub fn record_continuation(tx: &Transaction<'_>, chat_id: &str, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO restart_continuations(chat_id, created_at) VALUES (?1, ?2)
         ON CONFLICT(chat_id) DO UPDATE SET created_at = excluded.created_at",
        (chat_id, now),
    )?;
    tx.execute(
        "DELETE FROM restart_continuations WHERE chat_id NOT IN (
             SELECT chat_id FROM restart_continuations ORDER BY created_at DESC LIMIT ?1)",
        [MAX_CONTINUATIONS],
    )?;
    Ok(())
}

fn capture_continuations(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let now = helm_core::now_ms();
    let ids: Vec<String> = {
        let mut st = tx.prepare(
            "SELECT id FROM chats WHERE status IN ('starting', 'running') AND archived = 0",
        )?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for id in ids {
        record_continuation(tx, &id, now)?;
    }
    Ok(())
}

/// A stalled flag or a stored queue position belongs to the turn that went quiet; after a restart
/// no turn is running, and the actor's in-memory copy starts clear, so a stored one would never
/// be cleared.
fn reset_stalls(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE chat_activity SET stalled = 0, local_ahead = NULL, queue_reason = NULL, state = NULL
         WHERE stalled <> 0 OR local_ahead IS NOT NULL OR queue_reason IS NOT NULL OR state IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// A turn start held by capacity belongs to a running app: after a restart each actor re-runs its
/// own gate over its first waiting send, which records the hold afresh if it still stands. A
/// hold made by hand is the user's, and waits for them across restarts.
fn clear_auto_holds(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM chat_dispatch_holds WHERE reason <> 'manual'",
        [],
    )?;
    Ok(())
}

/// Where a turn start waits while capacity settles: one row per held chat, written when the
/// start is held and removed when it opens, goes back or is released. `reason` says why
/// (`local_cap`, `claude_capacity`, `at_limit` or `manual`), `class` is the queue class the
/// start had, `server_id` the local server it waits on (nothing else has one), `seq` its place
/// in line and `top` whether the queue pushed it to the front.
fn dispatch_holds(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_dispatch_holds (
             chat_id     TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             reason      TEXT NOT NULL,
             class       TEXT NOT NULL,
             server_id   TEXT,
             seq         INTEGER NOT NULL,
             since       INTEGER NOT NULL,
             top         INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS dispatch_holds_line ON chat_dispatch_holds(top DESC, seq);",
    )
}

/// The roll-over stack: every session of a rolled-over chat shares a `stack_id` — the oldest
/// session's id — and each session names the session it was rolled over from and the one that
/// rolled over from it. The roll-over flow writes these on both rows as it archives the old
/// chat; roll overs recorded in `chat_succession` before the columns existed are linked here, so
/// one window can span them too. The walk is cheap (chains are short; at most 50 hops) and a
/// session that already carries a `stack_id` keeps it, so a re-run changes nothing and nothing
/// is ever un-archived or re-parented.
fn rollover_stack(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for column in ["stack_id", "prev_session_id", "next_session_id"] {
        let has: bool = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('chats') WHERE name = ?1",
            [column],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )?;
        if !has {
            tx.execute(&format!("ALTER TABLE chats ADD COLUMN {column} TEXT"), [])?;
        }
    }
    tx.execute(
        "CREATE INDEX IF NOT EXISTS chats_stack ON chats(stack_id)",
        [],
    )?;
    link_old_rolls(tx)
}

/// Walk every `chat_succession` chain and give its sessions one stack id and their links,
/// leaving any session already linked alone. `pub(crate)` so the roll-over tests can link a
/// chain the way a start-up would.
pub(crate) fn link_old_rolls(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    // The chains as recorded: each successor with its predecessor, and each predecessor with
    // its successor.
    let mut pred: HashMap<String, String> = HashMap::new();
    let mut succ: HashMap<String, String> = HashMap::new();
    {
        let mut st = tx.prepare("SELECT chat_id, predecessor_id FROM chat_succession")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (c, p) = row?;
            succ.insert(p.clone(), c.clone());
            pred.insert(c, p);
        }
    }
    // A session whose stack is already known ends a walk and lends its id; this also makes a
    // second pass do nothing.
    let mut stack_of: HashMap<String, String> = HashMap::new();
    {
        let mut st = tx.prepare("SELECT id, stack_id FROM chats WHERE stack_id IS NOT NULL")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, s) = row?;
            stack_of.insert(id, s);
        }
    }
    let mut links: Vec<(String, String, Option<String>, Option<String>)> = Vec::new();
    for c in pred.keys() {
        if stack_of.contains_key(c) {
            continue;
        }
        // Walk from this successor back to the oldest session: one that no successor names as
        // its predecessor, one whose stack is already known, or the fiftieth hop.
        let mut chain = vec![c.clone()];
        let mut cur = pred[c].clone();
        let stack = loop {
            chain.push(cur.clone());
            if let Some(s) = stack_of.get(&cur) {
                break s.clone();
            }
            match pred.get(&cur) {
                Some(n) if chain.len() < 50 => cur = n.clone(),
                _ => break cur.clone(),
            }
        };
        for id in &chain {
            if stack_of.contains_key(id) {
                continue;
            }
            links.push((
                id.clone(),
                stack.clone(),
                pred.get(id).cloned(),
                succ.get(id).cloned(),
            ));
            stack_of.insert(id.clone(), stack.clone());
        }
    }
    for (id, stack, prev, next) in links {
        tx.execute(
            "UPDATE chats SET stack_id = ?1, prev_session_id = ?2, next_session_id = ?3
             WHERE id = ?4 AND stack_id IS NULL",
            rusqlite::params![stack, prev, next, id],
        )?;
    }
    Ok(())
}

/// The build queue: the features to run one after another, with their hand-set order (`position`),
/// their state (`waiting`, `running`, `paused`, `done` or `removed`), and the chat each running
/// entry is being carried out in ([`crate::buildqueue`]). `started_at` and `finished_at` mark the
/// entry's run; `profile_id` says who added it.
fn build_queue(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS build_queue (
             id           TEXT PRIMARY KEY,
             profile_id   TEXT NOT NULL DEFAULT '',
             title        TEXT NOT NULL,
             brief        TEXT,
             item_id      TEXT,
             project_id   TEXT,
             model        TEXT,
             position     INTEGER NOT NULL,
             state        TEXT NOT NULL
                 CHECK (state IN ('waiting', 'running', 'paused', 'done', 'removed')),
             chat_id      TEXT REFERENCES chats(id) ON DELETE SET NULL,
             started_at   INTEGER,
             finished_at  INTEGER
         );
         CREATE INDEX IF NOT EXISTS build_queue_order ON build_queue(state, position);",
    )
}

/// `chat_activity` gained the live-state column (what a busy chat is doing right now, as the
/// maintenance tick last saw it); older databases get it here.
fn activity_state(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let has: bool = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('chat_activity') WHERE name = 'state'",
        [],
        |r| r.get::<_, i64>(0).map(|n| n > 0),
    )?;
    if !has {
        tx.execute("ALTER TABLE chat_activity ADD COLUMN state TEXT", [])?;
    }
    Ok(())
}

/// A held turn end belongs to a running app; after a restart no hold is open.
fn clear_stop_holds(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM chat_stop_holds", [])?;
    Ok(())
}

/// `chat_activity` gained the queue-position column after the first tables were written; older
/// databases get it here.
fn activity_queue(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let has: bool = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('chat_activity') WHERE name = 'local_ahead'",
        [],
        |r| r.get::<_, i64>(0).map(|n| n > 0),
    )?;
    if !has {
        tx.execute(
            "ALTER TABLE chat_activity ADD COLUMN local_ahead INTEGER",
            [],
        )?;
    }
    Ok(())
}

/// Columns added after their tables were first written; older databases get them here.
/// `submission_meta.afk_note` marks an app note that already says AFK is on, so it does not carry
/// the turn text as well. `chat_turn_usage.provider` says which provider ran the turn (`claude` or
/// `local`), so a chat that switched provider reports each turn under the right one.
fn late_columns(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for (table, column, definition) in [
        ("submission_meta", "afk_note", "INTEGER NOT NULL DEFAULT 0"),
        (
            "chat_turn_usage",
            "provider",
            "TEXT NOT NULL DEFAULT 'claude'",
        ),
        // Why this chat's turn start waits, while it does (a queue reason code; see
        // [`crate::queue`]). `chat_activity.local_ahead` already says how many stand ahead.
        ("chat_activity", "queue_reason", "TEXT"),
        // A message written by a page (origin `page`) carries the page's name and its short
        // summary beside its origin, so the transcript's card can name the page again after a
        // reload. Both stay null for every other send.
        ("submission_meta", "page_title", "TEXT"),
        ("submission_meta", "page_overview", "TEXT"),
        // What kind of message a chat-to-chat send or a Helm-written note is (a MessageKind in
        // kebab-case: `steer`, `wait-finished`, ...). Null for the user's own sends and for
        // notes that name no kind. `wait_chat` rides with `wait-finished` notes about a chat:
        // the watched chat as JSON {chatId,title}, so the compact line can name it.
        ("submission_meta", "kind", "TEXT"),
        ("submission_meta", "wait_chat", "TEXT"),
        // The chat's own fast-mode pick: 1 on, 0 off, null follows the new-chat default.
        ("chats", "fast_mode", "INTEGER"),
    ] {
        let has: bool = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
            [table, column],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )?;
        if !has {
            tx.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )?;
            if column == "provider" {
                // Turns recorded before the column existed ran on whatever their chat runs on now:
                // the best that can be told.
                tx.execute(
                    "UPDATE chat_turn_usage SET provider = 'local'
                     WHERE chat_id IN (SELECT id FROM chats WHERE provider IS NOT NULL)",
                    [],
                )?;
            }
        }
    }
    Ok(())
}

/// Every chat carries the profile it belongs to and the device it runs on, as columns on the row.
/// Both are filled when the chat is made (by the profiles crate's insert trigger, or by the code
/// that makes the chat) and again at start-up for any row still empty. `chat_shares` lists the
/// profiles besides its own that a chat is shown to; it lives here so every chat read can name
/// them. Older databases get the columns here.
fn identity(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for column in ["profile_id", "host_device_id"] {
        let has: bool = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('chats') WHERE name = ?1",
            [column],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )?;
        if !has {
            tx.execute(
                &format!("ALTER TABLE chats ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"),
                [],
            )?;
        }
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS chats_profile ON chats(profile_id);
         CREATE TABLE IF NOT EXISTS chat_shares (
             chat_id     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             profile_id  TEXT NOT NULL,
             PRIMARY KEY (chat_id, profile_id)
         );
         CREATE INDEX IF NOT EXISTS chat_shares_profile ON chat_shares(profile_id);",
    )
}

/// `chat_triage` gained the tries column after the first table was written; older databases get
/// it here.
fn triage_tries(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let has: bool = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('chat_triage') WHERE name = 'tries'",
        [],
        |r| r.get::<_, i64>(0).map(|n| n > 0),
    )?;
    if !has {
        tx.execute(
            "ALTER TABLE chat_triage ADD COLUMN tries INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    Ok(())
}

fn cancel_retry_leftovers(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE messages SET details = json_set(details, '$.retry.state', 'cancelled')
         WHERE details IS NOT NULL AND json_valid(details)
           AND json_extract(details, '$.retry.state') IN ('retrying', 'waiting')",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use helm_core::db::Database;

    pub fn temp_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        register(&mut reg);
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        (dir, db)
    }

    pub fn registry() -> MigrationRegistry {
        let mut reg = MigrationRegistry::new();
        register(&mut reg);
        reg
    }

    fn col(db: &Database, sql: &str) -> Vec<String> {
        db.snapshot(|tx| {
            let mut st = tx.prepare(sql)?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
    }

    #[test]
    fn recovery_captures_running_chats_keeps_unsent_messages_and_cancels_retries() {
        let (_d, db) = temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats(id, project_id, status, created_at, updated_at) VALUES
                   ('run', 'p', 'running', 0, 0), ('stop', 'p', 'stopping', 0, 0),
                   ('idle', 'p', 'idle', 0, 0);
                 INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES
                   ('q-run', 'run', 't', 'queued', 0), ('q-stop', 'stop', 't', 'queued', 0);
                 INSERT INTO messages(chat_id, source_key, role, status, timestamp, details) VALUES
                   ('run', 'm1', 'assistant', 'error', 0, '{\"retry\":{\"state\":\"retrying\",\"attempt\":1}}'),
                   ('run', 'm2', 'assistant', 'error', 0, '{\"retry\":{\"state\":\"failed\",\"attempt\":2}}');",
            )
        })
        .unwrap();
        helm_core::recovery::run(&db, &registry()).unwrap();
        // The retired queue tables are still there (empty) for an older version to open.
        assert_eq!(
            col(
                &db,
                "SELECT name FROM sqlite_master WHERE name IN ('queued_submissions', 'draft_queue_slots') ORDER BY name"
            ),
            ["draft_queue_slots", "queued_submissions"]
        );
        assert_eq!(
            col(&db, "SELECT chat_id FROM restart_continuations"),
            ["run"],
            "only the running chat; a user-stopping chat is excluded"
        );
        // The `run` chat stands for a killed process (a crash, a power cut, the installer ending
        // Helm): no shutdown code ran, so its row still says `running`, and it is captured exactly
        // like a chat found busy at a quit. Messages waiting from an older version are left for
        // the startup send to deliver; nothing marks them held.
        assert_eq!(
            col(
                &db,
                "SELECT id FROM submissions WHERE state = 'queued' ORDER BY id"
            ),
            ["q-run", "q-stop"]
        );
        assert_eq!(
            col(
                &db,
                "SELECT source_key || '=' || json_extract(details, '$.retry.state') FROM messages ORDER BY source_key"
            ),
            ["m1=cancelled", "m2=failed"]
        );
    }

    #[test]
    fn recovery_keeps_parked_chats_and_forgets_auto_holds() {
        let (_d, db) = temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('a', 'p', 0, 0), ('b', 'p', 0, 0);
                 INSERT INTO chat_dispatch_holds(chat_id, reason, class, seq, since) VALUES
                   ('a', 'claude_capacity', 'normal', 1, 0), ('b', 'manual', 'normal', 2, 0);
                 INSERT INTO chat_activity(chat_id, last_activity_at, local_ahead, queue_reason, state)
                   VALUES ('a', 0, 2, 'claude_capacity', '{\"kind\":\"queued\"}');",
            )
        })
        .unwrap();
        helm_core::recovery::run(&db, &registry()).unwrap();
        assert_eq!(
            col(
                &db,
                "SELECT chat_id || '=' || reason FROM chat_dispatch_holds"
            ),
            ["b=manual"]
        );
        assert_eq!(
            col(
                &db,
                "SELECT COALESCE(local_ahead, 'x') || '/' || COALESCE(queue_reason, 'x') || '/' || COALESCE(state, 'x') FROM chat_activity"
            ),
            ["x/x/x"],
            "the stored position and reason belong to the run that ended"
        );
    }

    #[test]
    fn the_wave_2_user_origin_is_rewritten_to_typed() {
        let (_d, db) = temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('c', 'p', 0, 0);
                 INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES
                   ('old', 'c', 't', 'complete', 0), ('app', 'c', 't', 'complete', 0);
                 INSERT INTO submission_meta(submission_id, origin) VALUES ('old', 'user'), ('app', 'harness');",
            )?;
            origin_user_is_typed(tx)
        })
        .unwrap();
        assert_eq!(
            col(
                &db,
                "SELECT submission_id || '=' || origin FROM submission_meta ORDER BY submission_id"
            ),
            ["app=harness", "old=typed"]
        );
    }

    #[test]
    fn continuations_keep_the_newest_hundred() {
        let (_d, db) = temp_db();
        db.write(|tx| {
            tx.execute(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0)",
                [],
            )?;
            for i in 0..105 {
                tx.execute(
                    "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 0, 0)",
                    [format!("c{i:03}")],
                )?;
                record_continuation(tx, &format!("c{i:03}"), i)?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let ids = col(
            &db,
            "SELECT chat_id FROM restart_continuations ORDER BY chat_id",
        );
        assert_eq!(ids.len(), 100);
        assert_eq!(ids[0], "c005");
    }

    /// Two roll overs recorded before the stack columns existed become one stack, oldest-named;
    /// a second pass changes nothing, and no chat is un-archived by the walk.
    #[test]
    fn old_roll_overs_link_into_one_stack_and_a_re_run_changes_nothing() {
        let (_d, db) = temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats(id, project_id, created_at, updated_at, archived) VALUES
                   ('a', 'p', 1, 1, 1), ('b', 'p', 2, 2, 1), ('c', 'p', 3, 3, 0),
                   ('lonely', 'p', 4, 4, 0);
                 INSERT INTO chat_succession(chat_id, predecessor_id) VALUES ('b', 'a'), ('c', 'b');",
            )?;
            rollover_stack(tx)
        })
        .unwrap();
        let linked = || {
            col(
                &db,
                "SELECT id || '/' || COALESCE(stack_id, 'x') || '/' || COALESCE(prev_session_id, 'x') \
                     || '/' || COALESCE(next_session_id, 'x') FROM chats ORDER BY id",
            )
        };
        assert_eq!(
            linked(),
            ["a/a/x/b", "b/a/a/c", "c/a/b/x", "lonely/x/x/x"],
            "the stack is named for its oldest session; an unlinked chat gets nothing"
        );
        assert_eq!(
            col(
                &db,
                "SELECT CAST(archived AS TEXT) FROM chats WHERE id = 'a'"
            ),
            ["1"],
            "the walk never un-archives or archives"
        );
        let before = linked();
        db.write(rollover_stack).unwrap();
        assert_eq!(linked(), before, "a re-run is a no-op");
    }
}
