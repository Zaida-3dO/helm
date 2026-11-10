//! Sealed pages.
//!
//! A local `.html` file can run its own scripts inside the side panel without being able to reach
//! anything of Helm's. This crate is the host side of that:
//!
//! | Module | What |
//! |---|---|
//! | [`server`] | page sessions and the `helmpage` scheme: reads the file once per open, serves the page, the runtime script and the page's own assets, nothing else |
//! | [`store`] | the pinned pages list and each page's saved state |
//! | [`ops`] | the `api` ops the renderer's page frame uses |
//!
//! How a page is kept apart from the app:
//!
//! - It loads from its own scheme and a random per-open address, inside an `<iframe>` whose sandbox
//!   leaves out `allow-same-origin`, so it has an opaque origin: no cookies, no web storage, no
//!   reading the app's page or any other page.
//! - Its policy (sent with the page, see [`server::csp`]) allows no connection except `blob:` and
//!   `data:`, which also shuts the app's IPC transport. Reaching the network is a per-open switch.
//! - It cannot call an op: ops answer only the main window, and the page is neither the main window
//!   nor on the app's origin. Everything it may ask for goes through the renderer's bridge, which
//!   knows the page only by the frame it created.

pub mod ops;
pub mod server;
pub mod store;

pub use ops::register_ops;
pub use server::PageServer;

/// Tells the pages crate which profile a chat belongs to, so a pinned page lands in that profile's
/// list. The profiles crate registers it as a service; with none registered every page is pinned
/// without a profile and shows in every list.
pub trait ChatProfiles: Send + Sync {
    fn profile_of(&self, chat_id: &str) -> Option<String>;
}

/// The service wrapper for [`ChatProfiles`].
#[derive(Clone)]
pub struct ChatProfileLookup(pub std::sync::Arc<dyn ChatProfiles>);

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("pages", store::init);
}
