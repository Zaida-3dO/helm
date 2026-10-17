//! Helm chats: one Claude Code chat end to end.
//!
//! | Module | What |
//! |---|---|
//! | [`types`] | the records, op arguments and events the UI sees |
//! | [`ops`] | the chat `api` ops |
//! | [`send_behavior`] | the send-behaviour setting and its defaults |
//! | [`bindings`] | the generated TypeScript in `ui/src/api/generated/` |
//! | [`schema`] | side tables and recovery steps |
//! | [`store`] | chat rows, drafts, submissions and transcript pages |
//! | [`session_log`] | the session log, its projection and repair (`SESSION-FORMAT.md`) |
//! | [`catalogue`] | the Claude model catalogue |
//! | [`clock`] | the revision clock for UI events |
//! | [`runtime`] | the runtime service, the worker spawner seam, quit drain |
//! | `actor` | one task per chat: worker, streaming, tools, interactions, Stop, timers |
//! | [`submit`] | sends, queue actions, model changes, restart continuation |

mod actor;
pub mod bindings;
pub mod catalogue;
pub mod clock;
pub mod ops;
pub mod runtime;
#[cfg(test)]
mod runtime_tests;
pub mod schema;
pub mod send_behavior;
pub mod session_log;
pub mod store;
pub mod submit;
pub mod types;

pub use ops::register_ops;
pub use runtime::{ChatRuntime, ProcessSpawner, RuntimeConfig, RuntimeDrain};

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    schema::register(reg);
}

/// Startup, after recovery: project any session-log entries the database is missing (a crash
/// between an append and its projection), chat by chat. A chat whose log cannot be read is logged
/// and skipped; its rows stay as they were.
pub fn reconcile_all(
    db: &helm_core::db::Database,
    profile: &helm_core::profile::Profile,
    clock: &clock::Clock,
) {
    let sessions = profile.path("sessions");
    let artifacts = profile.path("artifacts");
    let ids: Vec<String> = db
        .snapshot(|tx| {
            let mut st = tx.prepare("SELECT id FROM chats")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap_or_default();
    for id in ids {
        if !session_log::log_path(&sessions, &id).exists() {
            continue;
        }
        match session_log::reconcile(db, &sessions, &artifacts, &id, clock.next()) {
            Ok(0) => {}
            Ok(n) => log::info!("chat {id}: projected {n} entries missing from the database"),
            Err(e) => log::error!("chat {id}: session log not readable: {e}"),
        }
    }
}
