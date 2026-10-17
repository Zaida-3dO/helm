//! Startup recovery.
//!
//! One transaction, so a crash part-way leaves the database as it was:
//!
//! 1. chats that were `running`, `starting`, `stopping` or `importing` become `interrupted`, with
//!    an explanatory error;
//! 2. open runs (no `ended_at`) are interrupted;
//! 3. in-flight submissions (`dispatching`) become `uncertain`, never to be resent automatically;
//! 4. streaming messages become `interrupted`;
//! 5. retry markers are cleared;
//! 6. every registered feature recovery step runs (finishing or rolling back its intents).
//!
//! Submission states: only `dispatching` counts as in flight. `queued` submissions stay queued,
//! because the follow-up queue is durable by design.
//! Restart-continuation capture lands with the worker integration.

use crate::db::{Database, MigrationRegistry};

pub const INTERRUPTED_ERROR: &str = "Helm closed while this chat was working.";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub chats: usize,
    pub runs: usize,
    pub submissions: usize,
    pub messages: usize,
    pub retry_markers: usize,
}

pub fn run(db: &Database, registry: &MigrationRegistry) -> rusqlite::Result<RecoveryReport> {
    let now = crate::now_ms();
    db.write(|tx| {
        let chats = tx.execute(
            "UPDATE chats SET status = 'interrupted', error = ?1, updated_at = ?2 \
             WHERE status IN ('running', 'starting', 'stopping', 'importing')",
            (INTERRUPTED_ERROR, now),
        )?;
        let runs = tx.execute(
            "UPDATE runs SET status = 'interrupted', ended_at = ?1 WHERE ended_at IS NULL",
            [now],
        )?;
        let submissions = tx.execute(
            "UPDATE submissions SET state = 'uncertain' WHERE state = 'dispatching'",
            [],
        )?;
        let messages = tx.execute(
            "UPDATE messages SET status = 'interrupted' WHERE status = 'streaming'",
            [],
        )?;
        let retry_markers = tx.execute("DELETE FROM chat_retry_markers", [])?;
        for step in &registry.recovery {
            (step.run)(tx)?;
        }
        if chats + runs + submissions + messages > 0 {
            crate::revisions::bump_global(tx)?;
        }
        Ok(RecoveryReport {
            chats,
            runs,
            submissions,
            messages,
            retry_markers,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::temp_db;
    use rusqlite::Transaction;

    fn seed(db: &Database) {
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats(id, project_id, status, created_at, updated_at) VALUES
                   ('run', 'p', 'running', 0, 0), ('sta', 'p', 'starting', 0, 0),
                   ('sto', 'p', 'stopping', 0, 0), ('imp', 'p', 'importing', 0, 0),
                   ('idle', 'p', 'idle', 0, 0);
                 INSERT INTO runs(id, chat_id, status, started_at, ended_at) VALUES
                   ('r-open', 'run', 'running', 0, NULL), ('r-done', 'idle', 'completed', 0, 5);
                 INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES
                   ('s-fly', 'run', 't', 'dispatching', 0), ('s-q', 'idle', 't', 'queued', 0),
                   ('s-ok', 'idle', 't', 'accepted', 0);
                 INSERT INTO messages(chat_id, source_key, role, status, timestamp) VALUES
                   ('run', 'm1', 'assistant', 'streaming', 0), ('idle', 'm2', 'assistant', 'complete', 0);
                 INSERT INTO chat_retry_markers(chat_id, attempt) VALUES ('run', 2);",
            )
        })
        .unwrap();
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
    fn marks_interrupted_work_in_one_pass() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        seed(&db);
        let report = run(&db, &MigrationRegistry::new()).unwrap();
        assert_eq!(
            report,
            RecoveryReport {
                chats: 4,
                runs: 1,
                submissions: 1,
                messages: 1,
                retry_markers: 1
            }
        );
        assert_eq!(
            col(&db, "SELECT id || '=' || status FROM chats ORDER BY id"),
            [
                "idle=idle",
                "imp=interrupted",
                "run=interrupted",
                "sta=interrupted",
                "sto=interrupted"
            ]
        );
        assert_eq!(
            col(
                &db,
                "SELECT id || '=' || state FROM submissions ORDER BY id"
            ),
            ["s-fly=uncertain", "s-ok=accepted", "s-q=queued"]
        );
        assert_eq!(
            col(&db, "SELECT id || '=' || status FROM runs ORDER BY id"),
            ["r-done=completed", "r-open=interrupted"]
        );
        assert_eq!(
            col(
                &db,
                "SELECT source_key || '=' || status FROM messages ORDER BY source_key"
            ),
            ["m1=interrupted", "m2=complete"]
        );
        assert!(col(&db, "SELECT chat_id FROM chat_retry_markers").is_empty());
        // Idempotent: a second pass finds nothing to do.
        assert_eq!(
            run(&db, &MigrationRegistry::new()).unwrap(),
            RecoveryReport::default()
        );
    }

    fn failing_step(_tx: &Transaction<'_>) -> rusqlite::Result<()> {
        Err(rusqlite::Error::InvalidQuery)
    }

    #[test]
    fn a_failure_in_any_step_rolls_everything_back() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        seed(&db);
        let mut reg = MigrationRegistry::new();
        reg.recovery_step("boom", failing_step);
        assert!(run(&db, &reg).is_err());
        assert_eq!(
            col(&db, "SELECT status FROM chats WHERE id = 'run'"),
            ["running"]
        );
        assert_eq!(
            col(&db, "SELECT state FROM submissions WHERE id = 's-fly'"),
            ["dispatching"]
        );
        assert_eq!(col(&db, "SELECT chat_id FROM chat_retry_markers"), ["run"]);
    }
}
