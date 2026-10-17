//! The consent primitive.
//!
//! The one place Helm decides that an agent's step counts as the user's own consent. Two proofs:
//!
//! - **An Allow grant**: a request the user's click on the card in the main window moved to
//!   `allowed` ([`store::decide`], reached only through the `consent_decide` op). Bound to one
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

pub mod bindings;
pub mod kinds;
mod ops;
pub mod store;
pub mod types;
pub mod words;

pub use kinds::{AllowKind, AllowKinds, Canonical};
pub use ops::register_ops;
pub use store::{ConsentRecord, Delivery, NewRequest, Refusal, Upkeep};
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
