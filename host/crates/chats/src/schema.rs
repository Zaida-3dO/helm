//! The chats crate's side tables and startup steps (host/README.md "Adding SQLite tables").
//!
//! | Table | Holds |
//! |---|---|
//! | `submission_meta` | per submission: origin (`user`/`harness`), send priority (`now` or null), the run it was dispatched in |
//! | `restart_continuations` | chats owed one restart-continuation prompt (at most 100 rows) |
//! | `chat_queue_holds` | chats whose queue waits for Send or `queue_resume` after a restart |
//! | `chat_titles` | per chat: where the title came from (`fallback`/`generated`/`manual`) |
//! | `unread_marks` | manual unread marks with their token |
//! | `chat_completions` | the current completion per chat |
//! | `chat_mutes` | muted chats (user-only) |
//! | `chat_activity` | per chat: the last worker output time and the stalled flag |
//! | `chat_attention` | per chat: when its attention was marked addressed, and a snooze |
//! | trigger `restart_cancel_changed_chat` | drops a pending continuation on archive or a project, harness, model or effort change |
//! | `chat_forks` | fork ids and what they were asked for, for idempotent forks |
//! | `pending_edits` | an edit staged with its submission, applied once the worker closes ([`crate::edit`]) |
//! | `chat_attachments` | images attached to a chat, then to the submission that sent them ([`crate::attachments`]) |
//! | `fork_carry` | a message fork's carried history, sent once with its first prompt ([`crate::fork`]) |
//! | `chat_file_cleanup` | files a hard delete left behind, removed after commit and retried at startup ([`crate::delete`]) |
//! | `submission_context` | the panel-context block a composer send carried |
//! | view `chat_search_source` | the searchable message text the search crate indexes ([`crate::search_source`]) |
//!
//! Recovery, inside the one startup transaction:
//! - **capture** (before core recovery rewrites statuses): every chat still `starting` or
//!   `running` gets a continuation row. `stopping` chats are excluded: the user asked them to stop.
//! - **after**: a chat with queued follow-ups but no continuation gets its queue held; retry
//!   states left `retrying` or `waiting` on a message become `cancelled`.

use helm_core::db::MigrationRegistry;
use rusqlite::Transaction;

pub const MAX_CONTINUATIONS: i64 = 100;

pub fn register(reg: &mut MigrationRegistry) {
    reg.initializer("chats-side-tables", init)
        .recovery_capture("chats-restart-capture", capture_continuations)
        .recovery_step("chats-queue-holds", hold_queues)
        .recovery_step("chats-retry-leftovers", cancel_retry_leftovers);
}

fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS submission_meta (
             submission_id  TEXT PRIMARY KEY REFERENCES submissions(id) ON DELETE CASCADE,
             origin         TEXT NOT NULL DEFAULT 'user',
             priority       TEXT,
             run_id         TEXT
         );
         CREATE TABLE IF NOT EXISTS restart_continuations (
             chat_id     TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             created_at  INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chat_queue_holds (
             chat_id  TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             held_at  INTEGER NOT NULL
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
         CREATE VIEW IF NOT EXISTS chat_search_source AS
             SELECT m.id AS message_id, m.chat_id AS chat_id, m.role AS role, m.text AS text,
                    m.timestamp AS timestamp, c.archived AS archived
             FROM messages m JOIN chats c ON c.id = m.chat_id
             WHERE m.active = 1 AND m.role IN ('user', 'assistant') AND m.text != ''
               AND c.side_source_id IS NULL;",
    )
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

fn hold_queues(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO chat_queue_holds(chat_id, held_at)
         SELECT DISTINCT q.chat_id, ?1 FROM queued_submissions q
         JOIN submissions s ON s.id = q.submission_id
         WHERE s.state = 'queued'
           AND q.chat_id NOT IN (SELECT chat_id FROM restart_continuations)",
        [helm_core::now_ms()],
    )?;
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
    fn recovery_captures_running_chats_holds_queues_and_cancels_retries() {
        let (_d, db) = temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats(id, project_id, status, created_at, updated_at) VALUES
                   ('run', 'p', 'running', 0, 0), ('stop', 'p', 'stopping', 0, 0),
                   ('idle', 'p', 'idle', 0, 0);
                 INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES
                   ('q-run', 'run', 't', 'queued', 0), ('q-stop', 'stop', 't', 'queued', 0);
                 INSERT INTO queued_submissions(submission_id, chat_id, position) VALUES
                   ('q-run', 'run', 0), ('q-stop', 'stop', 0);
                 INSERT INTO messages(chat_id, source_key, role, status, timestamp, details) VALUES
                   ('run', 'm1', 'assistant', 'error', 0, '{\"retry\":{\"state\":\"retrying\",\"attempt\":1}}'),
                   ('run', 'm2', 'assistant', 'error', 0, '{\"retry\":{\"state\":\"failed\",\"attempt\":2}}');",
            )
        })
        .unwrap();
        helm_core::recovery::run(&db, &registry()).unwrap();
        assert_eq!(
            col(&db, "SELECT chat_id FROM restart_continuations"),
            ["run"],
            "only the running chat; a user-stopping chat is excluded"
        );
        assert_eq!(col(&db, "SELECT chat_id FROM chat_queue_holds"), ["stop"]);
        assert_eq!(
            col(
                &db,
                "SELECT source_key || '=' || json_extract(details, '$.retry.state') FROM messages ORDER BY source_key"
            ),
            ["m1=cancelled", "m2=failed"]
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
}
