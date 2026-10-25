//! The orchestrator workflow: the per-chat Orchestrator state, tasks and changes, stage gates,
//! commit-bound approval and the land step.
//!
//! ## Where the gates run
//!
//! The task and change engine runs **on the host** and is reached through the reserved host tools
//! (`app_workflow`, `app_workflow_report`, `app_orchestrator_on`). The host re-derives everything
//! that decides an irreversible step, so keeping the engine beside it loses no safety. The per-tool
//! hooks for the spawn and edit gates are not wired yet. The landing guard is: the worker asks
//! [`hook`] before each Bash, PowerShell or MCP call in a chat with a task.
//!
//! A mid-chat change of a chat's Orchestrator state cannot reach a persistent worker's launch, so
//! it is delivered as an **app note** (`helm_chats::submit::host_send`, origin App, Exempt, one
//! fixed submission id per change) carrying the Workflow rules; the next launch carries the state
//! through the prompt addendum.
//!
//! ## Contract
//!
//! | Surface | What |
//! |---|---|
//! | op `workflow_settings_get` / `_save` | Settings → Orchestrator ([`settings`]): strict validation, unknown keys refused, invalid stored value → defaults, no revision, master toggle refused mid-turn |
//! | `orchestrator` key of `app_get_settings` / `app_update_settings` | a chat reads all of Settings → Orchestrator and may change only `newChats` and each stage's agent ([`agent_settings`]) |
//! | op `workflow_chat_mode_get` / `_set` | the composer switch ([`mode`]): on / off / auto, set by user / agent / default |
//! | table `workflow_settings` | one row, the validated JSON |
//! | table `chat_workflow_modes` | per chat: mode, set_by, reason ≤ 500, updated_at |
//! | host tool `app_orchestrator_on {reason}` | an Auto chat's root agent turns it on |
//! | prompt section `workflow` | the generated Workflow rules, plus the Move fast block when on |
//! | host tools `app_workflow`, `app_workflow_report` | tasks, changes, plan of record, rounds and caps, commit-bound approval, hand-off |
//! | tables `workflow_runs`, `workflow_changes`, `workflow_events` | per task / per change / per event |
//! | allow kinds | `land`, `ready`, `skip`, `allowMore`, `drop`, `reopen` on `helm_consent` |
//! | worker line `guard_check` → `guard_verdict` | the landing guard on Bash, PowerShell and MCP calls ([`hook`]): only in a chat with Orchestrator on and an active task, while Settings → Orchestrator → Landing guard is on (default on); overrides recorded as `guard_override`, refusals as `gate_denied`, both listed under Needs you |
//! | table `workflow_standup_outbox` | stage records written to Agent Standup, with an "outcome unknown" read-back before any resend |
//! | land step | `gh` merge pinned to the approved head, readback must say MERGED; draft land |
//! | op `workflow_run_get`, event `workflow_run` | the workflow panel's live read |
//! | ops `workflow_change_action`, `workflow_change_ready`, `workflow_task_action` | the user's buttons and task menu ([`user`]); Ready for review is its own non-maintenance op because it asks GitHub |
//!
//! Events: `workflow_settings {}`, `workflow_chat_mode {chatId}` and `workflow_run {chatId}` tell the
//! UI to re-read. `workflow_run` follows every write of a task, change or event row, whoever made it
//! (an op, a tool or the land step): the store marks the chat and the caller emits after its
//! transaction ([`store::emit_touched`]). The UI reads the run with `workflow_run_get {chatId}`.

pub mod actions;
pub mod agent_settings;
pub mod bindings;
pub mod engine;
#[cfg(test)]
mod flow_tests;
pub mod gh;
pub mod git;
pub mod guard;
pub mod handoff;
pub mod hook;
pub mod kinds;
pub mod land;
#[cfg(test)]
mod land_tests;
pub mod mode;
pub mod notes;
mod ops;
pub mod outage_rules;
pub mod overrides;
pub mod reports;
pub mod rules;
pub mod sa_outbox;
#[cfg(test)]
mod sa_tests;
pub mod sa_writer;
pub mod settings;
pub mod shell;
pub mod store;
pub mod tool;
pub mod types;
pub mod user;
#[cfg(test)]
mod user_tests;
pub mod wftools;

pub use agent_settings::extension as settings_extension;
pub use hook::register_worker_lines;
pub use kinds::register_allow_kinds;
pub use ops::register_ops;
pub use tool::{register_host_tools, WorkflowSection};
pub use types::*;
pub use user::{afk_sign_off_waiting, AfkPlanSignOff};

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
