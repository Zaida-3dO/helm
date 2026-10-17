//! Helm side panel, host side.
//!
//! | Module | What |
//! |---|---|
//! | [`pathgate`] | the lexical path check every op runs before touching the disk |
//! | [`private`] | private roots, by path and by file identity |
//! | [`viewer`] | `file_read` / `dir_list` within the viewer's limits |
//! | [`handler`] | the checked default-app handler and reveal in Explorer |
//! | [`tabs`] | saved panel tabs in a per-chat side table |
//! | [`ops`] | the `api` ops |

pub mod handler;
pub mod ops;
pub mod pathgate;
pub mod private;
pub mod tabs;
pub mod viewer;

pub use ops::register_ops;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("panel_tab_sets", tabs::init);
}
