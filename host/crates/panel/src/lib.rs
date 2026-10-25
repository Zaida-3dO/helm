//! Helm side panel, host side.
//!
//! | Module | What |
//! |---|---|
//! | [`pathgate`] | the lexical path check every op runs before touching the disk |
//! | [`private`] | private roots, by path and by file identity |
//! | [`viewer`] | `file_read` / `dir_list` within the viewer's limits |
//! | [`handler`] | the checked default-app handler and reveal in Explorer |
//! | [`resolve`] | clickable paths written in replies (`~`, drives, chat-relative) |
//! | [`quick`] | quick-open by path and over the project's files |
//! | [`git`] | the hardened git runner (reads killed on a deadline, the one write never) |
//! | [`changes`] | the Changes view, diffs and staging |
//! | [`context`] | the side panel context block (the eye toggle) |
//! | [`comments`] | review comments on working-tree diffs |
//! | [`netchange`] | the net change of a file over a turn |
//! | [`turns`] | turn change sets for the card under a completed turn |
//! | [`tabs`] | saved panel tabs in a per-chat side table |
//! | [`vault`] | Obsidian vaults, note resolution, confined reads, Open in Obsidian, the agent link instructions |
//! | [`ops`] | the `api` ops |

pub mod changes;
pub mod comments;
pub mod context;
pub mod git;
pub mod handler;
pub mod netchange;
pub mod ops;
pub use helm_platform::pathgate;
pub use helm_platform::private;
pub mod quick;
pub mod resolve;
pub mod tabs;
pub mod turns;
pub mod vault;
pub mod viewer;

pub use ops::register_ops;

/// Registers the panel-context builder behind chat_send's `SendContext` seam.
pub fn register_services(
    services: &mut helm_ipc::Services,
    _events: &std::sync::Arc<dyn helm_ipc::EventSink>,
) {
    let mut roots = Vec::new();
    if let Some(p) = services.get::<helm_core::profile::Profile>() {
        roots.push(p.dir.clone());
    }
    if let Ok(d) = helm_core::profile::default_profile_dir() {
        roots.push(d);
    }
    let builder = context::PanelContext {
        roots: private::PrivateRoots::new(&roots),
    };
    services.insert(std::sync::Arc::new(helm_chats::seams::SendContext(
        std::sync::Arc::new(builder),
    )));
}

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("panel_tab_sets", tabs::init)
        .initializer("panel_vaults", vault::init)
        .initializer("panel_context", context::init)
        .initializer("review_comments", comments::init);
}
