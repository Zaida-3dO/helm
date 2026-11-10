//! Saved for later: things the user parks in Helm to come back to.
//!
//! A saved item is a title, some text, and optionally the chat message it came from. It can carry
//! short notes, a reminder time, and four states (`saved`, `filed`, `done`, `dismissed`). It lives
//! **only in Helm** (tables `saved_items` and `saved_item_notes`); nothing is written to the board.
//!
//! | Module | What |
//! |---|---|
//! | [`store`] | the two tables: saving, the bounded note trail, listing and searching, updates, soft removal with an undo (a removed item is kept a week, then purged), and the due sweep |
//! | `ops` | `saved_save`, `saved_list`, `saved_update`, `saved_note`, `saved_comments` |
//! | [`service`] | the reminder sweep: a reminder that comes due is announced once with `saved_due {items}`, and stays due until the user deals with it |
//!
//! Event `saved {ids}` tells the UI to re-read after any change. Saving the same transcript row
//! twice (same chat and message), or the same `captureKey` twice, answers with the item that is
//! already there rather than making a second one. `helm_hosttools` exposes the three
//! `app_*saved*` tools over these ops.

pub mod ops;
pub mod service;
pub mod store;

pub use ops::register_ops;
pub use service::SavedService;

use helm_core::db::Database;
use helm_ipc::{EventSink, Services};
use std::sync::Arc;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("saved-items", store::init);
}

/// The shared [`SavedService`]; the reminder sweep emits through `events`.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let Some(db) = services.get::<Database>() else {
        return;
    };
    services.insert(Arc::new(SavedService::new(db, events.clone())));
}

/// Start the reminder sweep. A reminder is at worst five seconds late, and only ever fires once
/// until the item's reminder is set again.
pub fn start(runtime: &helm_chats::ChatRuntime, services: &Services) {
    let Some(svc) = services.get::<SavedService>() else {
        return;
    };
    let local = svc.clone();
    runtime.handle().spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(5));
        let mut ticks: u32 = 0;
        loop {
            every.tick().await;
            let svc = local.clone();
            // Items removed more than a week ago go for good: at start, then about hourly.
            let purge = ticks.is_multiple_of(720);
            ticks = ticks.wrapping_add(1);
            let _ = tokio::task::spawn_blocking(move || {
                let now = helm_core::now_ms();
                if purge {
                    svc.purge(now);
                }
                svc.sweep(now);
            })
            .await;
        }
    });
}
