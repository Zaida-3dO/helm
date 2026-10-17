//! The migration registry: the seam through which a feature crate adds SQLite tables.
//!
//! Core tables evolve only through the versioned [`CORE_MIGRATIONS`](super::schema). Features never
//! bump the version. Instead a crate registers:
//!
//! - an **initializer**: idempotent DDL (`CREATE TABLE IF NOT EXISTS`, indexes, triggers), run
//!   at every open;
//! - a **one-time migration**: run once, then marked done by a row in `counters` (set only after
//!   it succeeds);
//! - a **recovery step**: run inside the one startup recovery transaction (finishing or rolling
//!   back that feature's half-done intents).
//!
//! A crate exposes `pub fn register_migrations(reg: &mut MigrationRegistry)` and the app calls it
//! from one line in `host/app/src/registrations.rs`.

use rusqlite::Transaction;

pub type StepFn = fn(&Transaction<'_>) -> rusqlite::Result<()>;

#[derive(Clone, Copy)]
pub struct Step {
    pub name: &'static str,
    pub run: StepFn,
}

#[derive(Default, Clone)]
pub struct MigrationRegistry {
    pub(crate) initializers: Vec<Step>,
    pub(crate) one_time: Vec<Step>,
    pub(crate) recovery: Vec<Step>,
}

impl MigrationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn initializer(&mut self, name: &'static str, run: StepFn) -> &mut Self {
        Self::push(&mut self.initializers, name, run);
        self
    }

    /// `marker` must be unique and must never be reused for a different migration.
    pub fn one_time(&mut self, marker: &'static str, run: StepFn) -> &mut Self {
        Self::push(&mut self.one_time, marker, run);
        self
    }

    pub fn recovery_step(&mut self, name: &'static str, run: StepFn) -> &mut Self {
        Self::push(&mut self.recovery, name, run);
        self
    }

    fn push(list: &mut Vec<Step>, name: &'static str, run: StepFn) {
        assert!(
            list.iter().all(|s| s.name != name),
            "migration step registered twice: {name}"
        );
        list.push(Step { name, run });
    }
}

/// The `counters` key that marks a one-time migration as done.
pub fn marker_key(name: &str) -> String {
    format!("migration:{name}")
}
