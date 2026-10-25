//! Helm's MCP client, host side.
//!
//! | Module | What |
//! |---|---|
//! | [`types`] | the server model and value references |
//! | [`validate`] | server rules and the plain-http address rule |
//! | [`config`] | the stored file, revisions, credential-backed literals, resolution |
//! | [`admin`] | the `app_` MCP admin tool bodies |
//! | `ops` | the Settings → MCP `api` ops |
//! | [`lines`] | the `mcp_config` start line and re-send, and `mcp_status` from workers |
//! | [`probe`] | Settings → MCP's Test connection: one handshake and a tool count |
//! | [`presets`] | Agent Standup, Home Assistant and hub presets (MC7) |
//! | [`repo_gate`] | the repository-server gate and its seeding from Claude Code |
//! | [`status`] | the newest per-server status across workers (MC3) |
//! | [`tools`] | their registration with the host-tool registry |
//! | [`bindings`] | generated TypeScript in `ui/src/api/generated/mcp/` |
//!
//! The app adds one `helm_mcp::register_worker_lines` line once the chats actor routes worker
//! lines through the host-tools registry; the launch flags are the host-tools crate's.

pub mod admin;
pub mod bindings;
pub mod config;
pub mod lines;
mod ops;
pub mod presets;
pub mod probe;
pub mod repo_gate;
pub mod status;
pub mod tools;
pub mod types;
pub mod validate;

pub use config::{McpStore, StoreError};
pub use lines::register_worker_lines;
pub use ops::register_ops;
pub use repo_gate::{LaunchGate, RepoGate};
pub use status::McpStatusStore;
pub use tools::register_host_tools;
pub use types::*;

use helm_core::credentials::CredentialStore;
use helm_core::profile::Profile;
use helm_ipc::{AppEvent, EventSink, Services};
use helm_platform::Platform;
use std::sync::Arc;

/// The shared [`McpStore`], with its change signal wired to the `mcp_config` event.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let (Some(profile), Some(platform)) = (services.get::<Profile>(), services.get::<Platform>())
    else {
        return;
    };
    let creds = CredentialStore::new(
        &profile.path("credentials"),
        &profile.hash,
        platform.keychain.clone(),
    );
    let store = Arc::new(McpStore::new(&profile.dir, creds));
    let status = Arc::new(McpStatusStore::default());
    let ev = events.clone();
    let st = status.clone();
    let weak = Arc::downgrade(&store);
    let cell = Arc::new(lines::ServicesCell::default());
    let cell2 = cell.clone();
    store.on_change(move |rev| {
        cell2.broadcast();
        if let Some(s) = weak.upgrade() {
            if let Ok(list) = s.load() {
                st.retain(&list.into_iter().map(|x| x.id).collect::<Vec<_>>());
            }
        }
        ev.emit(AppEvent::new("mcp_config").with("revision", rev));
    });
    services.insert(store);
    services.insert(status);
    services.insert(cell);
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from);
    services.insert(Arc::new(RepoGate::new(&profile.dir, home)));
}
