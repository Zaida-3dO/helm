//! The registration file: one line per crate that serves `api` ops or owns SQLite tables.
//!
//! To add a crate: depend on it in `host/app/Cargo.toml`, then add one line below in each function
//! it takes part in. Nothing else in the app changes (`host/README.md`).

use helm_core::db::MigrationRegistry;
use helm_ipc::OpRegistry;

pub fn ops(reg: &mut OpRegistry) {
    helm_core::ops::register_ops(reg);
}

pub fn migrations(reg: &mut MigrationRegistry) {
    helm_core::ops::register_migrations(reg);
}
