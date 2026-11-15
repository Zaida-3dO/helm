//! Feeding the hub's search index from this machine.
//!
//! [`sources`] is the reading end: one reader per kind of local source, each a pure function
//! that turns what is on disk into a [`sources::DocOut`] — a doc made of [`sources::ChunkOut`]
//! chunks, hashed so an unchanged source costs nothing to re-check. [`feeds`] finds what each
//! source holds and who may see it; [`engine`] pushes it to the hub and gives new chunks their
//! vectors.

pub mod answer;
pub mod bindings;
pub mod engine;
pub mod feeds;
pub mod hub;
pub mod model;
mod ops;
pub mod service;
pub mod sources;
pub mod store;
mod tool;

pub use service::{register_services, start, Service};
pub use tool::{register_allow_kinds, register_host_tools};

pub fn register_ops(reg: &mut helm_ipc::OpRegistry) {
    ops::register_ops(reg);
}

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("memindex", store::init);
}

#[cfg(test)]
mod tests;
