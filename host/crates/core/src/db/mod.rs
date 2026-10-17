//! The database.
//!
//! One mutex-guarded writer connection, plus read-only snapshot connections that see a consistent
//! WAL snapshot without waiting for the writer. WAL journal, `synchronous=FULL`, foreign keys on,
//! 3 s busy timeout. Opening runs the core migrations, refuses a newer schema, then runs every
//! registered feature initializer and pending one-time migration.

pub mod registry;
pub mod schema;
pub mod tables;

pub use registry::{MigrationRegistry, Step};

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub const BUSY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(
        "This profile's database was written by a newer version of Helm (schema {found}; this \
         build understands up to {supported}). Update Helm to open it."
    )]
    NewerSchema { found: u32, supported: u32 },
    #[error("foreign-key check failed after migration {0}")]
    ForeignKeys(u32),
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("database file: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Database {
    path: PathBuf,
    writer: Mutex<Connection>,
}

impl Database {
    pub fn open(path: &Path, registry: &MigrationRegistry) -> Result<Self, DbError> {
        let existed = path.exists();
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        if !existed {
            helm_platform::fs::restrict_to_owner(path)?;
        }
        configure_writer(&conn)?;
        migrate(&mut conn)?;
        run_feature_steps(&mut conn, registry)?;
        for suffix in ["-wal", "-shm"] {
            let side = PathBuf::from(format!("{}{suffix}", path.display()));
            if side.exists() {
                helm_platform::fs::restrict_to_owner(&side)?;
            }
        }
        Ok(Self {
            path: path.to_owned(),
            writer: Mutex::new(conn),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run `f` in an immediate transaction on the writer and commit it. An error rolls back.
    pub fn write<T, E>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<rusqlite::Error>,
    {
        let mut conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Run `f` against one consistent read-only snapshot, on its own connection.
    pub fn snapshot<T, E>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<rusqlite::Error>,
    {
        let mut conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "query_only", true)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        // The first read pins the snapshot for the rest of the transaction.
        tx.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
        f(&tx)
    }

    /// Fold the WAL back into the main file (the last quit step).
    pub fn checkpoint(&self) -> Result<(), DbError> {
        let conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn user_version(&self) -> Result<u32, DbError> {
        let conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
    }
}

fn configure_writer(conn: &Connection) -> Result<(), DbError> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(DbError::Sql(rusqlite::Error::InvalidQuery));
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    let found: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if found > schema::CURRENT_VERSION {
        return Err(DbError::NewerSchema {
            found,
            supported: schema::CURRENT_VERSION,
        });
    }
    for &(version, sql) in schema::CORE_MIGRATIONS {
        if version <= found {
            continue;
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(sql)?;
        let violations: i64 =
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })?;
        if violations > 0 {
            return Err(DbError::ForeignKeys(version)); // dropping tx rolls back
        }
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

fn run_feature_steps(conn: &mut Connection, reg: &MigrationRegistry) -> Result<(), DbError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for step in &reg.initializers {
        (step.run)(&tx).map_err(|e| {
            log::error!("initializer {} failed: {e}", step.name);
            e
        })?;
    }
    tx.commit()?;
    for step in &reg.one_time {
        let key = registry::marker_key(step.name);
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let done: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM counters WHERE key = ?1)",
            [&key],
            |r| r.get(0),
        )?;
        if !done {
            (step.run)(&tx)?;
            tx.execute(
                "INSERT INTO counters(key, value) VALUES (?1, ?2)",
                (&key, crate::now_ms()),
            )?;
        }
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn temp_db(reg: &MigrationRegistry) -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), reg).unwrap();
        (dir, db)
    }

    #[test]
    fn pragmas_are_durable_settings() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let conn = db.writer.lock().unwrap();
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        let sync: i64 = conn
            .pragma_query_value(None, "synchronous", |r| r.get(0))
            .unwrap();
        let fks: i64 = conn
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        let busy: i64 = conn
            .pragma_query_value(None, "busy_timeout", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(sync, 2, "FULL");
        assert_eq!(fks, 1);
        assert_eq!(busy, 3000);
    }

    #[test]
    fn database_file_is_owner_only() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        assert!(helm_platform::fs::is_owner_only(db.path()).unwrap());
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sqlite3");
        {
            let c = Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", schema::CURRENT_VERSION + 1)
                .unwrap();
        }
        match Database::open(&path, &MigrationRegistry::new()) {
            Err(DbError::NewerSchema { found, supported }) => {
                assert_eq!(found, schema::CURRENT_VERSION + 1);
                assert_eq!(supported, schema::CURRENT_VERSION);
            }
            Err(e) => panic!("wrong error: {e}"),
            Ok(_) => panic!("a newer schema must be refused"),
        }
    }

    #[test]
    fn migrations_set_the_current_version() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        assert_eq!(db.user_version().unwrap(), schema::CURRENT_VERSION);
    }

    fn init_widgets(tx: &Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS widgets (id INTEGER PRIMARY KEY)")
    }

    fn seed_widget(tx: &Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute("INSERT INTO widgets DEFAULT VALUES", [])
            .map(|_| ())
    }

    #[test]
    fn reopening_is_idempotent_and_one_time_steps_run_once() {
        let mut reg = MigrationRegistry::new();
        reg.initializer("widgets", init_widgets)
            .one_time("seed-widget", seed_widget);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sqlite3");
        for _ in 0..3 {
            let db = Database::open(&path, &reg).unwrap();
            drop(db);
        }
        let db = Database::open(&path, &reg).unwrap();
        let n: i64 = db
            .snapshot(|tx| tx.query_row("SELECT count(*) FROM widgets", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_failed_write_rolls_back() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let r: Result<(), rusqlite::Error> = db.write(|tx| {
            tx.execute("INSERT INTO counters(key, value) VALUES ('a', 1)", [])?;
            tx.execute("INSERT INTO counters(key, value) VALUES ('a', 2)", [])?; // PK clash
            Ok(())
        });
        assert!(r.is_err());
        let n: i64 = db
            .snapshot(|tx| tx.query_row("SELECT count(*) FROM counters", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_snapshot_does_not_see_later_commits() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let n: rusqlite::Result<(i64, i64)> = db.snapshot(|tx| {
            let before: i64 = tx.query_row("SELECT count(*) FROM counters", [], |r| r.get(0))?;
            db.write(|w| w.execute("INSERT INTO counters(key, value) VALUES ('x', 1)", []))?;
            let after: i64 = tx.query_row("SELECT count(*) FROM counters", [], |r| r.get(0))?;
            Ok((before, after))
        });
        assert_eq!(n.unwrap(), (0, 0));
        let now: i64 = db
            .snapshot(|tx| tx.query_row("SELECT count(*) FROM counters", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(now, 1);
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let r: rusqlite::Result<usize> = db.write(|tx| {
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('c', 'nope', 0, 0)",
                [],
            )
        });
        assert!(r.is_err());
    }

    #[test]
    fn harness_check_refuses_pi() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let r: rusqlite::Result<usize> = db.write(|tx| {
            tx.execute(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO chats(id, project_id, harness, created_at, updated_at) \
                 VALUES ('c', 'p', 'pi', 0, 0)",
                [],
            )
        });
        assert!(r.is_err());
    }

    #[test]
    fn checkpoint_empties_the_wal() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        db.write(|tx| tx.execute("INSERT INTO counters(key, value) VALUES ('x', 1)", []))
            .unwrap();
        let wal = PathBuf::from(format!("{}-wal", db.path().display()));
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        db.checkpoint().unwrap();
        assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    }
}
