//! Declared waits: "wake me when this changes".
//!
//! An agent that would otherwise sleep and poll declares what it is waiting on with
//! `app_wait_for` and ends its turn. A fixed watcher in the host (no model) looks at the target
//! and, when it finishes, changes, dies or times out, sends the waiting chat exactly one message
//! that starts a turn if the chat is idle and queues behind the running turn if it is busy.
//!
//! | Module | What |
//! |---|---|
//! | [`model`] | the wait kinds, what the watcher sees, and the pure rule that fires a wait |
//! | [`store`] | the `agent_waits` table: waits survive a restart |
//! | [`probes`] | looking at chats, processes, files and GitHub pull requests (read-only) |
//! | [`service`] | the two sweeps, firing once, and delivery |
//! | [`tools`] | `app_wait_for` and `app_background_result` |
//!
//! Kinds: another chat finishing its turn, a process exiting, a file appearing or changing, and
//! a GitHub pull request's checks or review (through the `gh` tool). Waiting on a board item is
//! not offered.

pub mod model;
pub mod probes;
pub mod service;
pub mod store;
pub mod tools;

#[cfg(test)]
mod testkit;

pub use service::{Deliver, Delivery, WaitsService};
pub use tools::register_host_tools;

use helm_core::db::Database;
use helm_ipc::{EventSink, Services};
use std::sync::Arc;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("agent-waits", store::init);
}

/// The shared [`WaitsService`], reading through the real probes.
pub fn register_services(services: &mut Services, _events: &Arc<dyn EventSink>) {
    let Some(db) = services.get::<Database>() else {
        return;
    };
    let probes = Arc::new(probes::RealProbes { db: db.clone() });
    services.insert(Arc::new(WaitsService::new(db, probes)));
}

/// Start the watcher loops and point delivery at the chat runtime.
pub fn start(runtime: &helm_chats::ChatRuntime, services: &Services) {
    let Some(svc) = services.get::<WaitsService>() else {
        return;
    };
    svc.set_sink(Arc::new(service::RuntimeSink(runtime.clone())));
    let local = svc.clone();
    runtime.handle().spawn(async move {
        let mut every =
            tokio::time::interval(std::time::Duration::from_secs(service::LOCAL_SWEEP_SECS));
        loop {
            every.tick().await;
            let svc = local.clone();
            let _ = tokio::task::spawn_blocking(move || svc.sweep_local(helm_core::now_ms())).await;
        }
    });
    runtime.handle().spawn(async move {
        let mut every =
            tokio::time::interval(std::time::Duration::from_secs(service::NETWORK_SWEEP_SECS));
        loop {
            every.tick().await;
            let svc = svc.clone();
            let _ =
                tokio::task::spawn_blocking(move || svc.sweep_network(helm_core::now_ms())).await;
        }
    });
}
