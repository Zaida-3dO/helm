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
    helm_hosttools::register_ops(reg);
    helm_usage::register_ops(reg);
    helm_itemlinks::register_ops(reg);
    helm_schedules::register_ops(reg);
    helm_workflow::register_ops(reg);
    helm_afk::register_ops(reg);
    helm_governor::register_ops(reg);
    helm_relay::register_ops(reg);
    helm_updates::register_ops(reg);
}

pub fn migrations(reg: &mut MigrationRegistry) {
    helm_core::ops::register_migrations(reg);
    helm_chats::register_migrations(reg);
    helm_actions::register_migrations(reg);
    helm_projects::register_migrations(reg);
    helm_panel::register_migrations(reg);
    helm_search::register_migrations(reg);
    helm_consent::register_migrations(reg);
    helm_hosttools::register_migrations(reg);
    helm_usage::register_migrations(reg);
    helm_itemlinks::register_migrations(reg);
    helm_schedules::register_migrations(reg);
    helm_workflow::register_migrations(reg);
    helm_afk::register_migrations(reg);
    helm_governor::register_migrations(reg);
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
    helm_usage::register_services(services, events);
    helm_schedules::register_services(services, events);

    // After usage, which the governor reads.
    helm_afk::register_services(services, events);
    helm_governor::register_services(services, events);
    helm_relay::register_services(services, events);
    helm_updates::register_services(services, events);
    services.insert(Arc::new(host_tools()));
    // Settings that live outside the preference table but that chats read and change.
    let mut extensions = vec![helm_workflow::settings_extension()];
    if let Some(rt) = services.get::<helm_chats::ChatRuntime>() {
        extensions.push(helm_chats::slot_settings::extension((*rt).clone()));
    }
    services.insert(Arc::new(helm_hosttools::SettingsExtensions(extensions)));
    services.insert(Arc::new(worker_lines()));
    // Other crates reach running workers through the chat runtime (helm_hosttools::lines).
    if let Some(rt) = services.get::<helm_chats::ChatRuntime>() {
        services.insert(Arc::new(helm_hosttools::Broadcaster(rt)));
    }
    services.insert(Arc::new(allow_kinds()));
    services.insert(Arc::new(helm_consent::ConsentWake::default()));
    // The MCP crate decides each launch's native-server flags.
    if let (Some(gate), Some(db)) = (
        services.get::<helm_mcp::RepoGate>(),
        services.get::<helm_core::db::Database>(),
    ) {
        let store = services.get::<helm_mcp::McpStore>();
        services.insert(Arc::new(helm_hosttools::McpLaunchGate(Arc::new(
            McpLaunchFlags { gate, db, store },
        ))));
    } else {
        log::warn!("MCP launch gate unavailable: launches keep native MCP on and disable nothing");
    }
}

/// The launch's MCP flags from the MCP crate's repository gate and native switch: native off
/// adds `--strict-mcp-config`; held repository servers go to the CLI's disabled list.
struct McpLaunchFlags {
    gate: Arc<helm_mcp::RepoGate>,
    db: Arc<helm_core::db::Database>,
    store: Option<Arc<helm_mcp::McpStore>>,
}

impl helm_hosttools::McpLaunchSource for McpLaunchFlags {
    fn for_launch(&self, _chat_id: &str, cwd: &std::path::Path) -> helm_hosttools::McpLaunch {
        let g = self
            .gate
            .for_launch(cwd, helm_mcp::repo_gate::native_on(&self.db));
        let helm_servers = self
            .store
            .as_ref()
            .and_then(|s| {
                s.load()
                    .map_err(|e| log::warn!("MCP config unreadable at launch: {e}"))
                    .ok()
            })
            .is_some_and(|l| l.iter().any(|s| s.enabled));
        helm_hosttools::McpLaunch {
            native: g.native,
            disabled: g.disabled,
            helm_servers,
        }
    }
}

/// Every crate's host tools. Each crate adds one
/// `register_host_tools` line; the built registry is a shared service the request channel reads.
pub fn host_tools() -> helm_hosttools::HostToolRegistry {
    let mut reg = helm_hosttools::HostToolRegistry::new();
    helm_chats::register_host_tools(&mut reg);
    helm_hosttools::register_host_tools(&mut reg);
    helm_consent::register_host_tools(&mut reg);
    helm_mcp::register_host_tools(&mut reg);
    helm_itemlinks::register_host_tools(&mut reg);
    helm_workflow::register_host_tools(&mut reg);
    helm_relay::register_host_tools(&mut reg);
    reg
}

/// The worker-message registry (plan-review change 2): handlers for worker lines the chats actor
/// does not own, and lines each worker gets at start. Each crate adds one `register_worker_lines`
/// line.
pub fn worker_lines() -> helm_hosttools::WorkerLines {
    let mut l = helm_hosttools::WorkerLines::new();
    helm_mcp::register_worker_lines(&mut l);
    helm_usage::register_worker_lines(&mut l);
    helm_chats::submit::register_worker_lines(&mut l);
    l
}

/// Attach host tools to the chat runtime once the op dispatcher exists.
pub fn attach_host_tools(
    runtime: &helm_chats::ChatRuntime,
    dispatcher: &Arc<helm_ipc::Dispatcher>,
) {
    runtime.attach_host(host_seam(dispatcher));
    start_consent_upkeep(runtime, dispatcher.ctx());
    // Send the workflow's Agent Standup stage records every minute.
    helm_workflow::start_standup_upkeep(runtime.handle(), dispatcher.ctx());
    // The land step (merges and draft lands of handed-off changes).
    helm_workflow::land::start_driver(runtime.handle(), dispatcher.ctx());
    // Auto-AFK signals and the 30 s AFK tick.
    helm_afk::start(runtime, &dispatcher.ctx().services);
    helm_governor::start(runtime, dispatcher);
    // The outbound line to the hub (off until a connection is saved).
    helm_relay::start(
        runtime.handle(),
        dispatcher.ctx(),
        host_seam(dispatcher).tools,
    );
    helm_relay::attach_ops(dispatcher); // Telegram into chats
}

/// The consent upkeep: every minute, and at once after a decision,
/// expire overdue requests and grants and deliver each decision to its agent once, as an app note
/// under a fixed submission id (so a retry can never deliver it twice).
pub fn start_consent_upkeep(runtime: &helm_chats::ChatRuntime, ctx: &helm_ipc::OpCtx) {
    let (Some(db), Some(kinds)) = (
        ctx.services.get::<helm_core::db::Database>(),
        ctx.services.get::<helm_consent::AllowKinds>(),
    ) else {
        return;
    };
    let wake = ctx.services.get::<helm_consent::ConsentWake>();
    let events = ctx.events.clone();
    let rt = runtime.clone();
    runtime.handle().spawn(async move {
        loop {
            consent_tick(&rt, &db, &kinds, &events);
            let wait = tokio::time::sleep(std::time::Duration::from_secs(60));
            match &wake {
                Some(w) => {
                    tokio::select! { _ = wait => {}, _ = w.0.notified() => {} }
                }
                None => wait.await,
            }
        }
    });
}

fn consent_tick(
    rt: &helm_chats::ChatRuntime,
    db: &helm_core::db::Database,
    kinds: &helm_consent::AllowKinds,
    events: &Arc<dyn EventSink>,
) {
    let now = helm_core::now_ms();
    let up = match helm_consent::store::upkeep(db, kinds, now) {
        Ok(u) => u,
        Err(e) => return log::warn!("consent upkeep: {}", e.message),
    };
    for chat in up.changed_chats {
        events.emit(helm_ipc::AppEvent::new("consent").with("chatId", chat));
    }
    for d in up.deliveries {
        let sent = helm_chats::submit::host_send(
            rt,
            helm_chats::submit::HostSend {
                chat_id: &d.chat_id,
                submission_id: &d.submission_id,
                text: &d.text,
                mode: helm_chats::submit::HostSendMode::Queue,
                origin: helm_chats::types::SubmissionOrigin::App,
                gated: false,
            },
        );
        match sent {
            Ok(_) => {}
            // Gone or archived: the agent cannot be told; stop retrying. A stopping chat
            // ("busy") or a full house ("capacity") is retried at the next tick.
            Err(e) if e.code == "not_found" || e.code == "refused" => {
                log::info!(
                    "consent decision for {} not deliverable: {}",
                    d.chat_id,
                    e.message
                )
            }
            Err(e) => {
                log::warn!(
                    "consent decision for {}: {}; retrying",
                    d.chat_id,
                    e.message
                );
                continue;
            }
        }
        let _ = helm_consent::store::mark_delivered(db, &d.request_id, now);
    }
}

/// The host-tool seam: the registry, the worker lines, the op dispatcher's maintenance lock (so
/// mutating tools never overlap maintenance ops) and the dispatcher itself, which tools that act
/// through `api` ops call.
pub fn host_seam(dispatcher: &Arc<helm_ipc::Dispatcher>) -> helm_chats::HostSeam {
    let ctx = dispatcher.ctx();
    let registry = ctx
        .services
        .get::<helm_hosttools::HostToolRegistry>()
        .unwrap_or_else(|| Arc::new(host_tools()));
    let lines = ctx
        .services
        .get::<helm_hosttools::WorkerLines>()
        .unwrap_or_else(|| Arc::new(worker_lines()));
    let ops: Arc<dyn helm_hosttools::OpInvoker> = dispatcher.clone();
    helm_chats::HostSeam {
        ctx: ctx.clone(),
        tools: Arc::new(
            helm_hosttools::HostTools::new(registry)
                .with_maintenance(dispatcher.maintenance())
                .with_ops(ops),
        ),
        lines,
    }
}

/// Every allow kind (consent). Each crate adds one `register_allow_kinds` line.
pub fn allow_kinds() -> helm_consent::AllowKinds {
    let mut k = helm_consent::AllowKinds::new();
    helm_consent::register_allow_kinds(&mut k);
    helm_workflow::register_allow_kinds(&mut k);
    k
}

/// Launch contributors: what each crate adds to every worker launch (`PATH`
/// prefixes, variables, system-prompt fragments; `helm_workers::contrib`). Each crate adds one
/// `add` line. Nothing contributed means a launch identical to one without this seam.
pub fn launch_contributors(
    db: &Arc<helm_core::db::Database>,
) -> helm_workers::contrib::LaunchContributors {
    // The prompt addendum layer owns the system-prompt text: one contributor
    // composing every section. Each crate adds one `register_section` line.
    let mut a = helm_hosttools::Addendum::new();
    a.register_section(Arc::new(helm_hosttools::sections::VerbositySection {
        db: db.clone(),
    }))
    .register_section(Arc::new(helm_hosttools::sections::AdoptedSection {
        id: "links",
        order: 20,
        inner: Arc::new(helm_panel::vault::VaultInstructions { db: db.clone() }),
    }))
    .register_section(Arc::new(helm_hosttools::sections::GuidelinesSection {
        registry: Arc::new(host_tools()),
    }))
    .register_section(Arc::new(helm_itemlinks::ItemLinksSection))
    .register_section(Arc::new(helm_workflow::WorkflowSection { db: db.clone() }));
    let mut c = helm_workers::contrib::LaunchContributors::new();
    c.add(Arc::new(a));
    c
}

#[cfg(test)]
mod host_tool_tests {
    use std::sync::Arc;

    /// Building the registry panics on a bad, unreserved, dropped or duplicate tool.
    #[test]
    fn every_host_tool_registers() {
        let reg = super::host_tools();
        for name in reg.names() {
            assert!(name.starts_with("app_"));
        }
        let names = |k: helm_hosttools::LaunchKeys| -> Vec<String> {
            reg.for_launch(&k).into_iter().map(|e| e.name).collect()
        };
        // Tool-surface baseline: every optional feature off, then the exact delta each adds.
        // A new tool, or one leaking past its condition, fails here.
        let base = [
            "app_list_chats",
            "app_read_chat",
            "app_open_chat",
            "app_send_message",
            "app_create_chat",
            "app_fork_chat",
            "app_update_chat",
            "app_set_chat_archived",
            "app_stop_chat",
            "app_list_projects",
            "app_create_project",
            "app_update_project",
            "app_get_settings",
            "app_update_settings",
            "app_list_attention",
            "app_update_attention",
            "app_list_saved_items",
            "app_save_item",
            "app_update_saved_item",
            "app_list_schedules",
            "app_create_schedule",
            "app_update_schedule",
            "app_cancel_schedule",
            "app_feedback",
            "app_feedback_summary",
            "app_create_feedback",
            "app_append_feedback",
            "app_update_feedback",
            "app_list_feedback",
            "app_get_feedback",
            "app_request_allow",
            "app_list_mcp_servers",
            "app_add_mcp_server",
            "app_edit_mcp_server",
            "app_update_mcp_server",
            "app_remove_mcp_server",
            "app_link_item",
            "app_orchestrator_on",
            "app_workflow",
            "app_workflow_report",
            "app_telegram_ask",
            "app_telegram_send",
        ];
        assert_eq!(names(Default::default()), base);
        let delta = |k: helm_hosttools::LaunchKeys| -> Vec<String> {
            names(k)
                .into_iter()
                .filter(|n| !base.contains(&n.as_str()))
                .collect()
        };
        let pm = helm_hosttools::LaunchKeys {
            projects_mode: true,
            ..Default::default()
        };
        assert_eq!(delta(pm), ["app_repo_locations"]);
        let all = helm_hosttools::LaunchKeys {
            all_settings: true,
            ..Default::default()
        };
        assert!(
            delta(all).is_empty(),
            "the all-settings switch alone adds no tool"
        );
        let both = helm_hosttools::LaunchKeys {
            projects_mode: true,
            all_settings: true,
            ..Default::default()
        };
        assert_eq!(delta(both), ["app_repo_locations", "app_update_repo"]);
        let mcp = helm_hosttools::LaunchKeys {
            mcp_enabled: true,
            ..Default::default()
        };
        assert!(
            delta(mcp).is_empty(),
            "app_mcp_tools is the worker's, not the host's"
        );
        let restricted = helm_hosttools::LaunchKeys {
            restricted: true,
            ..both
        };
        assert!(names(restricted).is_empty());
        // The channel's cap covers the largest request any tool accepts.
        assert!(reg.largest_request() <= 32 * 1024);
        // The seam main.rs attaches can run ops (tools that act through them would refuse
        // everything otherwise).
        let d = Arc::new(helm_ipc::Dispatcher::new(
            helm_ipc::OpRegistry::new(),
            helm_ipc::OpCtx {
                services: Arc::new(helm_ipc::Services::new()),
                events: Arc::new(helm_ipc::NullSink),
            },
            false,
        ));
        assert!(super::host_seam(&d).tools.has_ops());
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
