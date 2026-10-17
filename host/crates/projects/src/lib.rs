//! Helm projects and sidebar.
//!
//! One grouped sidebar: folder projects (made in Helm) and board projects (mirrored read-only from
//! Agent Standup) live in the one core `projects` table, and nest under each other locally.
//! A chat runs in its folder project (`chats.project_id`, untouched here) and is *shown* under its
//! `chat_groups` row when it has one.
//!
//! | Module | Holds |
//! |---|---|
//! | [`types`] | the record shapes and op arguments the UI sees |
//! | [`schema`] | side tables and the archived-project triggers |
//! | [`store`] | project and sidebar reads and writes: names, nesting, order, archive, paging |
//! | [`ops`] | the `api` ops |
//! | [`board`] | the Agent Standup connection and sync ops |
//! | [`bindings`] | `ui/src/api/generated/projects/` |

pub mod bindings;
pub mod board;
pub mod ops;
pub mod schema;
pub mod store;
pub mod types;

#[cfg(test)]
mod tests;

pub use ops::register_ops;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    schema::register(reg);
}
