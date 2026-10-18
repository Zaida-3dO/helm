//! The database.
//!
//! One mutex-guarded writer connection, plus read-only snapshot connections that see a consistent
//! WAL snapshot without waiting for the writer. WAL journal, `synchronous=FULL`, foreign keys on,
//! 3 s busy timeout on the writer and 100 ms on snapshot readers: a reader
//! still locked out after that falls back to reading on the writer, which waits its 3 s and then
//! fails with an error. Nothing retries in a loop. Opening runs the core migrations, refuses a newer schema, then runs every
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
/// A snapshot reader's busy timeout, before it falls back to waiting on the writer.
pub const READER_BUSY_TIMEOUT: Duration = Duration::from_millis(100);

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
    /// Tests: make the next snapshot reader find the database busy.
    #[cfg(test)]
    reader_busy: std::sync::atomic::AtomicBool,
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
            #[cfg(test)]
            reader_busy: Default::default(),
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
        let mut conn = self.open_reader()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        // The first read pins the snapshot for the rest of the transaction.
        match self.pin(&tx) {
            Ok(()) => f(&tx),
            Err(e) if is_busy(&e) => {
                drop(tx);
                self.read_on_writer(f)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn open_reader(&self) -> rusqlite::Result<Connection> {
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(READER_BUSY_TIMEOUT)?;
        conn.pragma_update(None, "query_only", true)?;
        Ok(conn)
    }

    fn pin(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
        #[cfg(test)]
        if self
            .reader_busy
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(busy_error());
        }
        tx.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
    }

    /// A snapshot that could not start in time: read inside a read-only transaction on the writer,
    /// waiting on its lock and its busy timeout.
    fn read_on_writer<T, E>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<rusqlite::Error>,
    {
        // Bounded, so a snapshot taken inside a write on this thread fails instead of deadlocking.
        let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
        let mut conn = loop {
            match self.writer.try_lock() {
                Ok(c) => break c,
                Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(E::from(busy_error()));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        conn.pragma_update(None, "query_only", true)?;
        let out = (|| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
            tx.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
            f(&tx)
        })();
        conn.pragma_update(None, "query_only", false)?;
        out
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

fn busy_error() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None)
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
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

    #[test]
    fn a_busy_writer_fails_with_an_error_after_its_timeout_and_does_not_loop() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let other = Connection::open(db.path()).unwrap();
        other.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let started = std::time::Instant::now();
        let r = db.write(|tx| tx.execute("INSERT INTO counters(key, value) VALUES ('a', 1)", []));
        let took = started.elapsed();
        assert!(
            matches!(&r, Err(e) if is_busy(e)),
            "a busy error comes back: {r:?}"
        );
        assert!(
            took >= BUSY_TIMEOUT - Duration::from_millis(200) && took < BUSY_TIMEOUT * 2,
            "waited its busy timeout once: {took:?}"
        );
        other.execute_batch("COMMIT;").unwrap();
        db.write(|tx| tx.execute("INSERT INTO counters(key, value) VALUES ('a', 1)", []))
            .unwrap();
    }

    #[test]
    fn snapshot_readers_wait_100_ms_the_writer_3_s() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let reader: i64 = db
            .open_reader()
            .unwrap()
            .pragma_query_value(None, "busy_timeout", |r| r.get(0))
            .unwrap();
        assert_eq!(reader, 100);
        let writer: i64 = db
            .writer
            .lock()
            .unwrap()
            .pragma_query_value(None, "busy_timeout", |r| r.get(0))
            .unwrap();
        assert_eq!(writer, 3000);
    }

    #[test]
    fn a_busy_reader_falls_back_to_reading_on_the_writer() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        db.write(|tx| tx.execute("INSERT INTO counters(key, value) VALUES ('a', 1)", []))
            .unwrap();
        db.reader_busy
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let n: i64 = db
            .snapshot(|tx| tx.query_row("SELECT count(*) FROM counters", [], |r| r.get(0)))
            .expect("read on the writer instead of failing");
        assert_eq!(n, 1);
        assert!(
            !db.reader_busy.load(std::sync::atomic::Ordering::SeqCst),
            "the reader was tried first"
        );
        // The fallback is read-only, and the writer writes again afterwards.
        db.reader_busy
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let r: Result<usize, rusqlite::Error> =
            db.snapshot(|tx| tx.execute("INSERT INTO counters(key, value) VALUES ('b', 1)", []));
        assert!(r.is_err(), "no write through a snapshot");
        db.write(|tx| tx.execute("INSERT INTO counters(key, value) VALUES ('c', 1)", []))
            .unwrap();
        let keys: i64 = db
            .snapshot(|tx| tx.query_row("SELECT count(*) FROM counters", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(keys, 2);
    }

    #[test]
    fn a_busy_reader_inside_a_write_fails_instead_of_deadlocking() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let started = std::time::Instant::now();
        let r: Result<i64, rusqlite::Error> = db.write(|_| {
            db.reader_busy
                .store(true, std::sync::atomic::Ordering::SeqCst);
            db.snapshot(|tx| tx.query_row("SELECT count(*) FROM counters", [], |r| r.get(0)))
        });
        assert!(matches!(&r, Err(e) if is_busy(e)), "{r:?}");
        assert!(started.elapsed() < BUSY_TIMEOUT * 2);
    }
}
