//! The registration file: one line per crate that serves `api` ops, owns SQLite tables, adds a
//! shared service or contributes to worker launches.
//!
//! To add a crate: depend on it in `host/app/Cargo.toml`, then add one line below in each function
//! it takes part in. Nothing else in the app changes (`host/README.md`).

use helm_core::db::MigrationRegistry;
use helm_ipc::{EventSink, OpRegistry, Services};
use std::sync::Arc;

pub fn ops(reg: &mut OpRegistry) {
    helm_core::ops::register_ops(reg);
    helm_chats::register_ops(reg);
    helm_actions::register_ops(reg);
    helm_projects::register_ops(reg);
    helm_panel::register_ops(reg);
    helm_search::register_ops(reg);
    helm_shell::register_ops(reg);
    helm_terminal::register_ops(reg);
    helm_mcp::register_ops(reg);
    helm_consent::register_ops(reg);
}

pub fn migrations(reg: &mut MigrationRegistry) {
    helm_core::ops::register_migrations(reg);
    helm_chats::register_migrations(reg);
    helm_actions::register_migrations(reg);
    helm_projects::register_migrations(reg);
    helm_panel::register_migrations(reg);
    helm_search::register_migrations(reg);
    helm_consent::register_migrations(reg);
}

/// Shared services a crate adds for op handlers (`ctx.services.need::<T>()`), called once at
/// startup after the app's own services (database, profile, clock, platform, chat runtime, shell)
/// are in place, so a crate can read those here. Signature for a crate's function:
/// `pub fn register_services(services: &mut helm_ipc::Services, events: &Arc<dyn EventSink>)`.
pub fn services(services: &mut Services, events: &Arc<dyn EventSink>) {
    helm_shell::register_services(services, events);
    helm_projects::register_services(services, events);
    helm_terminal::register_services(services, events);
    helm_panel::register_services(services, events);
    helm_mcp::register_services(services, events);
    services.insert(Arc::new(host_tools()));
    services.insert(Arc::new(worker_lines()));
    // Other crates reach running workers through the chat runtime (helm_hosttools::lines).
    if let Some(rt) = services.get::<helm_chats::ChatRuntime>() {
        services.insert(Arc::new(helm_hosttools::Broadcaster(rt)));
    }
    services.insert(Arc::new(allow_kinds()));
}

/// Every crate's host tools. Each crate adds one
/// `register_host_tools` line; the built registry is a shared service the request channel reads.
pub fn host_tools() -> helm_hosttools::HostToolRegistry {
    let mut reg = helm_hosttools::HostToolRegistry::new();
    helm_hosttools::register_host_tools(&mut reg);
    helm_mcp::register_host_tools(&mut reg);
    reg
}

/// The worker-message registry (plan-review change 2): handlers for worker lines the chats actor
/// does not own, and lines each worker gets at start. Each crate adds one `register_worker_lines`
/// line.
pub fn worker_lines() -> helm_hosttools::WorkerLines {
    #[allow(unused_mut)]
    let mut l = helm_hosttools::WorkerLines::new();
    l
}

/// Attach host tools to the chat runtime once the op dispatcher exists, sharing its maintenance
/// lock so mutating tools never overlap maintenance ops.
pub fn attach_host_tools(
    runtime: &helm_chats::ChatRuntime,
    ctx: &helm_ipc::OpCtx,
    maintenance: Arc<tokio::sync::Mutex<()>>,
) {
    let registry = ctx
        .services
        .get::<helm_hosttools::HostToolRegistry>()
        .unwrap_or_else(|| Arc::new(host_tools()));
    let lines = ctx
        .services
        .get::<helm_hosttools::WorkerLines>()
        .unwrap_or_else(|| Arc::new(worker_lines()));
    runtime.attach_host(helm_chats::HostSeam {
        ctx: ctx.clone(),
        tools: Arc::new(helm_hosttools::HostTools::new(registry).with_maintenance(maintenance)),
        lines,
    });
}

/// Every allow kind (consent). Each crate adds one `register_allow_kinds` line.
pub fn allow_kinds() -> helm_consent::AllowKinds {
    let mut k = helm_consent::AllowKinds::new();
    helm_consent::register_allow_kinds(&mut k);
    k
}

/// Launch contributors: what each crate adds to every worker launch (`PATH`
/// prefixes, variables, system-prompt fragments; `helm_workers::contrib`). Each crate adds one
/// `add` line. Nothing contributed means a launch identical to one without this seam.
pub fn launch_contributors(
    db: &Arc<helm_core::db::Database>,
) -> helm_workers::contrib::LaunchContributors {
    let mut c = helm_workers::contrib::LaunchContributors::new();
    c.add(Arc::new(helm_panel::vault::VaultInstructions {
        db: db.clone(),
    }));
    c
}

#[cfg(test)]
mod host_tool_tests {
    /// Building the registry panics on a bad, unreserved, dropped or duplicate tool.
    #[test]
    fn every_host_tool_registers() {
        let reg = super::host_tools();
        for name in reg.names() {
            assert!(name.starts_with("app_"));
        }
        // Building the kinds panics on a bad or duplicate kind; debug builds carry the test kind.
        let kinds = super::allow_kinds();
        assert_eq!(kinds.get("test_action").is_some(), cfg!(debug_assertions));
    }
}

#[cfg(test)]
mod launch_tests {
    /// The link-hint fragment reaches every launch: the list is not empty and adds a
    /// system-prompt fragment teaching the clickable link forms.
    #[test]
    fn launch_contributors_add_the_vault_instructions() {
        let dir = std::env::temp_dir().join(format!(
            "helm-launch-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = helm_core::db::MigrationRegistry::new();
        super::migrations(&mut reg);
        let db = std::sync::Arc::new(
            helm_core::db::Database::open(&dir.join("t.sqlite3"), &reg).unwrap(),
        );
        let list = super::launch_contributors(&db);
        assert!(!list.is_empty());
        let extras = list.collect(&helm_workers::contrib::LaunchCtx {
            chat_id: "c",
            cwd: "C:/w",
        });
        let prompt = helm_workers::contrib::system_prompt(&extras).unwrap_or_default();
        // The file-link part is always there; the vault part needs a known vault on the machine.
        assert!(
            prompt.contains("Helm shows your links in a side panel"),
            "the fragment teaches the clickable link forms"
        );
    }
}
