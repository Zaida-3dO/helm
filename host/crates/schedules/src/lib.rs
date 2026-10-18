//! Schedules: saved prompts sent into an existing chat on a timer.
//!
//! - They run only while the app is open and awake.
//! - Runs missed meanwhile coalesce into one.
//! - A busy chat makes the run wait; it is claimed once the chat is idle.
//! - They are sent through `helm_chats::submit::host_send` with origin `schedule`, gated, under a
//!   fixed submission id per (schedule, slot). Never through `chat_send`: that records the send as
//!   the user's own.
//!
//! | Module | What |
//! |---|---|
//! | [`types`] | the shapes shared with the UI |
//! | [`timing`] | once / every N seconds / weekly next-run maths over an injectable timezone |
//! | [`store`] | the tables, create / update / cancel, the claim, recovery |
//! | [`scheduler`] | the 5 s tick, the prompt framing and the send |
//! | [`templates`] | the 30-minute check-in and the noon digest, off until enabled |
//! | `ops` | `schedule_list`, `schedule_runs`, `schedule_create`, `schedule_update`, `schedule_cancel` |
//! | [`bindings`] | generated TypeScript in `ui/src/api/generated/schedules/` |
//!
//! Not built here (follow-ups): reminder mode and the reminders inbox, `newChat` destinations, the
//! agent host tools (`app_*_schedule`), run-slot waiting.

pub mod bindings;
mod ops;
pub mod scheduler;
pub mod store;
pub mod templates;
pub mod timing;
pub mod types;

pub use ops::register_ops;
pub use scheduler::{SchedulesService, Sender};
pub use types::*;

use helm_core::db::Database;
use helm_ipc::{EventSink, Services};
use std::sync::Arc;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("schedules", store::init);
}

/// The shared [`SchedulesService`], and the tick when the chat runtime is there to send through.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let Some(db) = services.get::<Database>() else {
        return;
    };
    let svc = Arc::new(SchedulesService {
        db: db.clone(),
        zone: Arc::new(timing::LocalZone),
        events: events.clone(),
    });
    services.insert(svc.clone());
    match services.get::<helm_chats::ChatRuntime>() {
        Some(rt) => {
            scheduler::recover(&db);
            scheduler::start(svc, (*rt).clone());
        }
        None => log::warn!("schedules: no chat runtime, so nothing will fire"),
    }
}

#[cfg(test)]
mod tests;
