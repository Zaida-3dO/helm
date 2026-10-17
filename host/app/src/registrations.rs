//! The registration file: one line per crate that serves `api` ops, owns SQLite tables, adds a
//! shared service or contributes to worker launches.
//!
//! To add a crate: depend on it in `host/app/Cargo.toml`, then add one line below in each function
//! it takes part in. Nothing else in the app changes (`host/README.md`).

use helm_core::db::MigrationRegistry;
use helm_ipc::{EventSink, OpRegistry, Services};
use std::sync::Arc;

pub fn ops(reg: &mut OpRegistry) {
    helm_core::ops::register_ops(reg);
    helm_chats::register_ops(reg);
    helm_actions::register_ops(reg);
    helm_projects::register_ops(reg);
    helm_panel::register_ops(reg);
    helm_search::register_ops(reg);
}

pub fn migrations(reg: &mut MigrationRegistry) {
    helm_core::ops::register_migrations(reg);
    helm_chats::register_migrations(reg);
    helm_actions::register_migrations(reg);
    helm_projects::register_migrations(reg);
    helm_panel::register_migrations(reg);
    helm_search::register_migrations(reg);
}

/// Shared services a crate adds for op handlers (`ctx.services.need::<T>()`), called once at
/// startup after the app's own services (database, profile, clock, platform, chat runtime, shell)
/// are in place, so a crate can read those here. Signature for a crate's function:
/// `pub fn register_services(services: &mut helm_ipc::Services, events: &Arc<dyn EventSink>)`.
pub fn services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let _ = (services, events);
}

// Launch contributors: the chats crate adds `pub fn launch_contributors(...)` here together
// with the `helm_workers::LaunchContributor` trait; each crate then adds one line to it.
