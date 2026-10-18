//! Host tools.
//!
//! Agents act on Helm through `app_` host tools. This crate is the contract every crate codes
//! against:
//!
//! - [`ToolSpec`] and [`HostToolRegistry`]: a crate declares each tool next to its Rust handler,
//!   through `pub fn register_host_tools(r: &mut HostToolRegistry)` and one line in
//!   `host/app/src/registrations.rs`.
//! - [`HostToolRegistry::for_launch`]: the per-launch catalogue written into `launch.json` as
//!   `hostTools[]`; the worker's in-process MCP server serves exactly that list, plus its own
//!   compatibility tools (`app_list_tools`, `app_mcp_call`, and `app_mcp_tools` with an MCP server
//!   enabled).
//! - [`reserved`]: the one reserved-name file both host and worker read.
//! - [`addendum`]: the prompt addendum layer.
//!
//! - [`dispatch`]: the host side of the request channel: gates, limits and execution, and
//!   the `response.json` mailbox.
//! - [`lines`]: the worker-message registry: handlers for new worker lines, lines sent at worker
//!   start, and the broadcast service.

pub mod addendum;
pub mod dispatch;
pub mod feedback;
pub mod lines;
pub mod registry;
pub mod reserved;
pub mod sections;
pub mod settings;
pub mod settings_allow;
pub mod shells;
pub mod spec;
pub mod tools;

pub use addendum::{Addendum, AddendumSection, StaticSection};
pub use dispatch::{Caller, CallerBinding, ChatRequest, HostTools};
pub use lines::{Broadcaster, WorkerBroadcast, WorkerLineCtx, WorkerLines};
pub use registry::{
    CallOrigin, Changed, HostToolRegistry, OpInvoker, RegisteredTool, ToolCtx, ToolError,
    ToolFuture, ToolHandler, ToolResult,
};
pub use spec::{
    Availability, CatalogueEntry, Condition, LaunchKeys, McpLaunch, McpLaunchGate, McpLaunchSource,
    RequestLimit, ToolSpec, DEFAULT_TIMEOUT_MS, MAX_DESCRIPTION,
};

/// This crate's own tools: projects and repositories ([`tools`]). The chat tools register from
/// the chats crate.
pub fn register_host_tools(r: &mut HostToolRegistry) {
    tools::register_host_tools(r);
    settings::register_host_tools(r);
    shells::register_host_tools(r);
    // Registered last among the built-ins.
    feedback::register_host_tools(r);
}

/// The settings change ring (`settings_changes`).
pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("hosttools-settings-changes", settings::init);
    reg.initializer("hosttools-agent-feedback", feedback::init);
}

/// `settings_changes_list` and `settings_undo`.
pub fn register_ops(reg: &mut helm_ipc::OpRegistry) {
    settings::register_ops(reg);
    feedback::register_ops(reg);
}
