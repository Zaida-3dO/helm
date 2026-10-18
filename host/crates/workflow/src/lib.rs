//! The orchestrator workflow: the per-chat Orchestrator state, tasks and changes, stage gates,
//! commit-bound approval and the land step.
//!
//! ## Where the gates run
//!
//! The task and change engine runs **on the host** and is reached through the reserved host tools
//! (`app_workflow`, `app_workflow_report`, `app_orchestrator_on`). The host re-derives everything
//! that decides an irreversible step, so keeping the engine beside it loses no safety. The per-tool
//! hooks (spawn gate, edit gate, the Bash landing guard) are not wired yet; their evaluators are
//! built here as pure functions ready to wire.
//!
//! A mid-chat change of a chat's Orchestrator state cannot reach a persistent worker's launch, so
//! it is delivered as an **app note** (`helm_chats::submit::host_send`, origin App, Exempt, one
//! fixed submission id per change) carrying the Workflow rules; the next launch carries the state
//! through the prompt addendum.
//!
//! ## Contract (built top down; the rest follow in order)
//!
//! | Surface | What |
//! |---|---|
//! | op `workflow_settings_get` / `_save` | Settings → Orchestrator ([`settings`]): strict validation, unknown keys refused, invalid stored value → defaults, no revision, master toggle refused mid-turn |
//! | op `workflow_chat_mode_get` / `_set` | the composer switch ([`mode`]): on / off / auto, set by user / agent / default |
//! | table `workflow_settings` | one row, the validated JSON |
//! | table `chat_workflow_modes` | per chat: mode, set_by, reason ≤ 500, updated_at |
//! | host tool `app_orchestrator_on {reason}` | an Auto chat's root agent turns it on |
//! | prompt section `workflow` | the generated Workflow rules, plus the Move fast block when on |
//! | host tools `app_workflow`, `app_workflow_report` | tasks, changes, plan of record, rounds and caps, commit-bound approval, hand-off |
//! | tables `workflow_runs`, `workflow_changes`, `workflow_events` | per task / per change / per event |
//! | allow kinds | `land`, `ready`, `skip`, `allowMore`, `drop`, `reopen` on `helm_consent` |
//! | guard evaluator | landing-guard classifier and the override marker, pure, not wired yet |
//! | table `workflow_standup_outbox` | stage records written to Agent Standup, with an "outcome unknown" read-back before any resend |
//! | land step | `gh` merge pinned to the approved head, readback must say MERGED; draft land |
//!
//! Events: `workflow_settings {}`, `workflow_chat_mode {chatId}` and `workflow {chatId}` tell the UI
//! to re-read.

pub mod actions;
pub mod bindings;
pub mod engine;
#[cfg(test)]
mod flow_tests;
pub mod gh;
pub mod git;
pub mod handoff;
pub mod kinds;
pub mod land;
#[cfg(test)]
mod land_tests;
pub mod mode;
pub mod notes;
mod ops;
pub mod reports;
pub mod rules;
pub mod sa_outbox;
#[cfg(test)]
mod sa_tests;
pub mod sa_writer;
pub mod settings;
pub mod store;
pub mod tool;
pub mod types;
pub mod wftools;

pub use kinds::register_allow_kinds;
pub use ops::register_ops;
pub use tool::{register_host_tools, WorkflowSection};
pub use types::*;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("workflow-settings", settings::init)
        .initializer("workflow-chat-modes", mode::init)
        .initializer("workflow-tasks", store::init)
        .initializer("workflow-standup-outbox", sa_outbox::init)
        .initializer("workflow-land-waits", land::init);
}

/// Every minute, send the Agent Standup records that are due. Started once with the host
/// tools; does nothing while no board is connected.
pub fn start_standup_upkeep(handle: &tokio::runtime::Handle, ctx: &helm_ipc::OpCtx) {
    let services = ctx.services.clone();
    handle.spawn(async move {
        loop {
            let s = services.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let (Some(db), Some(target)) =
                    (s.get::<helm_core::db::Database>(), sa_writer::target(&s))
                else {
                    return;
                };
                let t = sa_writer::HttpWrites::default();
                if let Err(e) = sa_outbox::flush(&db, &target, &t, helm_core::now_ms(), 50) {
                    log::warn!("Agent Standup outbox: {e}");
                }
            })
            .await;
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });
}
