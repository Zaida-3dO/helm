//! The read-only plan-usage meter.
//!
//! Plan usage comes **only** from the unmodified claude binary: the `get_usage` control request
//! (no model call, read by `helpers/usage`) and the `rate_limit_event` lines chats already see.
//! Helm never reads a credential store and never calls an OAuth endpoint (CLAUDE.md hard rule 2).
//!
//! | Module | What |
//! |---|---|
//! | [`types`] | the reading, readiness and snapshot shapes shared with the UI |
//! | [`store`] | newer-wins-per-window merging, staleness and the persisted last reading |
//! | [`service`] | the cadence (launch, 10 min while visible, Refresh; 60 s floor) and readiness |
//! | [`helper`] | starting `helpers/usage` |
//! | [`modes`] | Request modes: Fast / Standard / unknown counts from Helm's native transcripts |
//! | [`summary`] | the `app_usage_summary` host tool: tokens and cost by model, day and chat |
//! | [`prices`] | what local tokens would have cost on Claude, in one small price table |
//! | [`lines`] | the `plan_usage` worker line, through the host-tools registry |
//! | `ops` | `usage_get`, `usage_refresh` and `request_modes` |
//! | [`bindings`] | generated TypeScript in `ui/src/api/generated/usage/` |
//!
//! What the wire does and does not say is in `NOTES.md` (US6).

pub mod bindings;
pub mod helper;
pub mod lines;
pub mod modes;
mod ops;
pub mod prices;
pub mod service;
pub mod store;
pub mod summary;
pub mod types;

pub use lines::register_worker_lines;
pub use modes::RequestModes;
pub use ops::register_ops;
pub use prices::CompareChoice;
pub use service::{ControlAnswer, PlanReader, UsageService};
pub use summary::register_host_tools;
pub use types::*;

use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{EventSink, Services};
use std::sync::Arc;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("plan-usage", store::init);
}

/// The shared [`UsageService`], reading through the app's helper.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let (Some(db), Some(profile)) = (services.get::<Database>(), services.get::<Profile>()) else {
        return;
    };
    let reader = Arc::new(helper::ProcessReader::new(
        profile.path(""),
        cfg!(debug_assertions),
    ));
    services.insert(Arc::new(modes::ModesService::new(
        reader.clone(),
        profile.path("sessions"),
        modes::default_projects_dir(),
    )));
    services.insert(Arc::new(UsageService::new(db, events.clone(), reader)));
}
