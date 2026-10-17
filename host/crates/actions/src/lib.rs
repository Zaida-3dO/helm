//! One-click action buttons.
//!
//! A button is a label and the exact text it sends into a chat (or into a brand-new chat, for the
//! `new_chat` kind). Helm never runs anything itself: a "script" button is a prompt asking the
//! agent to run it. The set lives in one row, revisioned for compare-and-set saves, and is
//! seeded on first read with a handful of common requests (section 7). Seeds are versioned, so a
//! later release can add a seed without overwriting edits or bringing back one the user deleted.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `actions_get` | blocking | `{}` to [`ActionSet`] |
//! | `actions_save` | maintenance | [`ActionsSaveArgs`] to [`ActionSet`] (`conflict` on a stale revision) |

pub mod bindings;
mod ops;
mod seeds;
mod store;
pub mod types;

pub use ops::register_ops;
pub use types::{ActionButton, ActionKind, ActionSet, ActionsSaveArgs, Placement};

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("actions-tables", store::init);
}
