//! The consent primitive.
//!
//! The one place Helm decides that an agent's step counts as the user's own consent. Two proofs:
//!
//! - **An Allow grant**: a request the user's click on the card in the main window moved to
//!   `allowed` ([`store::decide`], reached through the main window's `consent_decide` op, or for a chat served to another
//!   device through that PC's own decide path, which checks the chat). Bound to one
//!   chat, one allow kind and the SHA-256 of a canonical target the host builds itself;
//!   single-use; 30 minutes by default (the agent may ask 5–120, the user may pick 5–240).
//! - **The user's words**: a message the user typed in this chat that is exactly the step's fixed
//!   form, within 2 h of a refusal the app recorded itself, among the first two typed messages
//!   after it ([`store::verify_words`], [`words`]).
//!
//! Spending ([`store::spend_grant`], [`store::spend_words`]) runs on the action's own transaction.
//! Requests are capped (4 pending per chat, 20 per hour, 50 app-wide) and live 24 h.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `consent_list` | blocking | [`ConsentListArgs`] to [`ConsentList`] |
//! | `consent_decide` | blocking | [`ConsentDecideArgs`] to [`ConsentRequest`] (`conflict` when no longer pending); emits `consent {chatId}` |
//!
//! Kinds register through `registrations::allow_kinds` (one [`AllowKinds::register`] line per
//! kind). The MCP crate has `mcp_repo_server` and, in tests and debug builds only,
//! [`kinds::test_kind`].
//!
//! **Applying kinds** ([`AllowKinds::register_applying`], [`apply`]): a change Helm makes itself
//! the moment the user clicks Approve, so nothing can drift between the card and the change. The
//! kind's own tool stages the request (host-built target and card lines; `app_request_allow`
//! refuses these kinds), the card says Approve and lists one line per change, and
//! [`apply::decide_and_apply`], the one decide path every surface calls, spends the grant and
//! runs the kind's [`apply::Applier`] under the maintenance lock. Two columns serve them:
//! `outcome` (what applying did, sent to the agent as the decision note) and `decided_via`
//! (where the user answered; `app` for this PC's main window, `device:<name>` for another device). Upkeep holds an applying
//! request's note until its outcome is written, or reports after ten minutes that Helm stopped
//! while applying.

pub mod apply;
pub mod bindings;
pub mod kinds;
mod ops;
pub mod store;
pub mod tool;
pub mod types;
pub mod words;

pub use apply::{decide_and_apply, Applier, ApplyJob, MaintenanceLock};
pub use kinds::{AllowKind, AllowKinds, Canonical};
pub use ops::register_ops;
pub use store::{ConsentRecord, Delivery, NewRequest, Refusal, Upkeep};
pub use tool::{register_host_tools, ConsentWake};
pub use types::{
    ConsentDecideArgs, ConsentList, ConsentListArgs, ConsentRequest, Decision, RequestState,
};

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("consent-tables", store::init);
}

/// This crate's own kinds: the test kind, in debug builds only (the e2e Allow-card flow).
pub fn register_allow_kinds(k: &mut AllowKinds) {
    if cfg!(debug_assertions) {
        k.register(kinds::test_kind());
    }
}

#[cfg(test)]
mod tests;
