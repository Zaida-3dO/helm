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
    helm_localmodels::register_ops(reg);
    helm_relay::register_ops(reg);
    helm_updates::register_ops(reg);
    helm_github::register_ops(reg);
    helm_webtools::register_ops(reg);
    helm_saved::register_ops(reg);
    helm_skills::register_ops(reg);
    helm_claudechats::register_ops(reg);
    helm_backup::register_ops(reg);
    helm_profiles::register_ops(reg);
    helm_pages::register_ops(reg);
    helm_devices::register_ops(reg);
    crate::pet::register_ops(reg);
    helm_dictation::register_ops(reg);
}

pub fn migrations(reg: &mut MigrationRegistry) {
    helm_core::ops::register_migrations(reg);
    helm_chats::register_migrations(reg);
    helm_actions::register_migrations(reg);
    helm_projects::register_migrations(reg);
    helm_panel::register_migrations(reg);
    helm_search::register_migrations(reg);
    helm_claudechats::register_migrations(reg);
    helm_consent::register_migrations(reg);
    helm_hosttools::register_migrations(reg);
    helm_usage::register_migrations(reg);
    helm_itemlinks::register_migrations(reg);
    helm_schedules::register_migrations(reg);
    helm_workflow::register_migrations(reg);
    helm_afk::register_migrations(reg);
    helm_governor::register_migrations(reg);
    helm_localmodels::register_migrations(reg);
    helm_waits::register_migrations(reg);
    helm_webtools::register_migrations(reg);
    helm_saved::register_migrations(reg);
    helm_profiles::register_migrations(reg);
    helm_pages::register_migrations(reg);
    helm_devices::register_migrations(reg);
    helm_dictation::register_migrations(reg);
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
    // Knowledge layers changing on this PC wake the knowledge sync (the relay hangs on it).
    services.insert(Arc::new(helm_skills::sync::Trigger::default()));
    helm_usage::register_services(services, events);
    helm_schedules::register_services(services, events);
    helm_waits::register_services(services, events);
    // Saved for later: the reminder sweep's service.
    helm_saved::register_services(services, events);

    // After usage, which the governor reads.
    helm_afk::register_services(services, events);
    helm_governor::register_services(services, events);
    helm_localmodels::register_services(services, events);
    helm_relay::register_services(services, events);
    // Graphics memory is kept free for speech-to-text only once it is set up: a dictation server
    // saved, or the phone's voice screen used with this PC.
    if let (Some(local), Some(db), Some(profile)) = (
        services.get::<helm_localmodels::LocalService>(),
        services.get::<helm_core::db::Database>(),
        services.get::<helm_core::profile::Profile>(),
    ) {
        let voice_tag = helm_relay::voice::tag_path(&profile.dir);
        local.set_speech_probe(Arc::new(move || {
            let dictation = db
                .snapshot(|tx| {
                    Ok::<_, helm_core::db::DbError>(
                        helm_dictation::settings::load(tx).stt_url.is_some(),
                    )
                })
                .unwrap_or(false);
            dictation || voice_tag.exists()
        }));
    }
    // Where chat events reach the service that answers other devices.
    helm_devices::register_services(services, events);
    // The people the hub names become profiles here.
    services.insert(Arc::new(helm_profiles::people_sync()));
    helm_updates::register_services(services, events);
    helm_github::register_services(services, events);
    // The Brave key and the gate that tells launches whether to list web search.
    helm_webtools::register_services(services, events);
    // A page pinned from a chat is filed under that chat's profile.
    if let Some(db) = services.get::<helm_core::db::Database>() {
        services.insert(Arc::new(helm_pages::ChatProfileLookup(Arc::new(
            PageProfiles { db },
        ))));
    }
    services.insert(Arc::new(host_tools()));
    // Settings that live outside the preference table but that chats read and change.
    let mut extensions = vec![
        helm_workflow::settings_extension(),
        helm_localmodels::settings_extension(),
    ];
    if let Some(web) = services.get::<helm_webtools::WebSearch>() {
        extensions.push(helm_webtools::settings_extension(web));
    }
    // Servers a chat has opted in to plain http on the private network, with Undo.
    if let Some(store) = services.get::<helm_mcp::McpStore>() {
        extensions.push(helm_mcp::unencrypted::settings_extension(store));
    }
    // The Telegram new-chat defaults: chats read the whole policy, change only model and effort.
    if let Some(profile) = services.get::<helm_core::profile::Profile>() {
        extensions.push(helm_relay::settings_extension(&profile.dir));
        // A chat's edits to the repository list, which Undo puts back.
        extensions.push(helm_projects::repo_undo::settings_extension(&profile.dir));
        // A chat's edits to the rules and skills, which Undo puts back.
        extensions.push(helm_skills::undo::settings_extension(&profile.dir));
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
        if let Some(store) = &store {
            // The launch contributors are built before the services; the prompt's memory line
            // reads the store through this cell once it is set.
            let _ = MCP_STORE.set(store.clone());
        }
        services.insert(Arc::new(helm_hosttools::McpLaunchGate(Arc::new(
            McpLaunchFlags { gate, db, store },
        ))));
    } else {
        log::warn!("MCP launch gate unavailable: launches keep native MCP on and disable nothing");
    }
}

/// Tells the pages crate which profile a chat belongs to.
struct PageProfiles {
    db: Arc<helm_core::db::Database>,
}

impl helm_pages::ChatProfiles for PageProfiles {
    fn profile_of(&self, chat_id: &str) -> Option<String> {
        self.db
            .snapshot(|tx| helm_profiles::store::profile_of_chat(tx, chat_id))
            .ok()
            .flatten()
            .map(|p| p.id)
    }
}

/// The MCP store, for the prompt section that mentions the shared memories (set once the
/// services exist; a launch before that, or in a test, simply has no such line).
static MCP_STORE: std::sync::OnceLock<Arc<helm_mcp::McpStore>> = std::sync::OnceLock::new();

/// The `knowledge` prompt section: the folder's project note and the shared-memories line.
fn knowledge_section(db: &Arc<helm_core::db::Database>) -> helm_hosttools::KnowledgeSection {
    let note_db = db.clone();
    helm_hosttools::KnowledgeSection {
        note: Arc::new(move |chat: &str| helm_projects::note::note_text_for_chat(&note_db, chat)),
        hub: Arc::new(|| {
            let servers = MCP_STORE.get()?.load().ok()?;
            helm_mcp::presets::connected_memory_server(&servers)
        }),
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
        let all_on = helm_mcp::repo_gate::all_on(&self.db);
        let g = self
            .gate
            .for_launch(cwd, all_on && helm_mcp::repo_gate::native_on(&self.db));
        let any_enabled = self
            .store
            .as_ref()
            .and_then(|s| {
                s.load()
                    .map_err(|e| log::warn!("MCP config unreadable at launch: {e}"))
                    .ok()
            })
            .is_some_and(|l| l.iter().any(|s| s.enabled));
        let helm_servers = all_on && any_enabled;
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
    helm_waits::register_host_tools(&mut reg);
    helm_usage::register_host_tools(&mut reg);
    helm_webtools::register_host_tools(&mut reg);
    helm_skills::register_host_tools(&mut reg);
    helm_dictation::register_host_tools(&mut reg);
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
    helm_workflow::register_worker_lines(&mut l);
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
    helm_localmodels::start(runtime, dispatcher);
    // The outbound line to the hub (off until a connection is saved).
    helm_relay::start(
        runtime.handle(),
        dispatcher.ctx(),
        host_seam(dispatcher).tools,
    );
    helm_relay::attach_ops(dispatcher); // Telegram into chats
                                        // Declared waits: the watcher loops and the delivery of their messages.
    helm_waits::start(runtime, &dispatcher.ctx().services);
    // Saved items: the reminder sweep, five seconds at worst.
    helm_saved::start(runtime, &dispatcher.ctx().services);
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
    // Pending Allow requests count in the attention summary; it emits only when it changed.
    rt.publish_attention();
    for d in up.deliveries {
        let sent = helm_chats::submit::host_send(
            rt,
            helm_chats::submit::HostSend {
                chat_id: &d.chat_id,
                submission_id: &d.submission_id,
                text: &d.text,
                origin: helm_chats::types::SubmissionOrigin::App,
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

/// Tells the launch whether Orchestrator is on for the chat, so settings that apply only to such
/// chats (the sub-agent limits) leave every other chat exactly as it was.
struct OrchestratorScope {
    db: Arc<helm_core::db::Database>,
}

impl helm_workers::contrib::LaunchContributor for OrchestratorScope {
    fn contribute(
        &self,
        ctx: &helm_workers::contrib::LaunchCtx<'_>,
    ) -> helm_workers::contrib::LaunchExtras {
        let on = self
            .db
            .snapshot(|tx| {
                let s = helm_workflow::settings::load(tx)?;
                helm_workflow::mode::in_effect(tx, ctx.chat_id, &s)
            })
            .unwrap_or(false);
        helm_workers::contrib::LaunchExtras {
            orchestrator_on: on,
            ..Default::default()
        }
    }
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
    .register_section(Arc::new(helm_hosttools::StaticSection {
        id: "cards",
        order: 35,
        text: helm_hosttools::show_images::CARDS_ADDENDUM,
    }))
    .register_section(Arc::new(knowledge_section(db)))
    .register_section(Arc::new(helm_itemlinks::ItemLinksSection))
    .register_section(Arc::new(helm_workflow::WorkflowSection { db: db.clone() }));
    // The knowledge layers: who a chat is for (its profile and project), and the rules of everyone
    // and of that person.
    if let Some(profile_dir) = db.path().parent() {
        let lookup = Arc::new(helm_skills::launch::DbLookup { db: db.clone() });
        helm_skills::launch::set_lookup(lookup.clone());
        a.register_section(Arc::new(helm_skills::launch::RulesSection::new(
            profile_dir.to_path_buf(),
            lookup,
        )));
    }
    let mut c = helm_workers::contrib::LaunchContributors::new();
    // Whose chat this is opens the system prompt, ahead of the addendum.
    c.add(Arc::new(helm_profiles::launch::WhoLaunch {
        db: db.clone(),
    }));
    c.add(Arc::new(a));
    // The workflow's dispatch and edit gates: a launch variable, only for chats they may act in.
    c.add(Arc::new(helm_workflow::GateLaunch { db: db.clone() }));
    // The git stand-in goes on a chat's PATH only while some repository has worktrees off.
    if let Some(profile_dir) = db.path().parent() {
        c.add(Arc::new(helm_projects::guard::GitGuard::new(
            profile_dir.to_path_buf(),
        )));
    }
    c.add(Arc::new(OrchestratorScope { db: db.clone() }));
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
            "app_this_chat",
            "app_list_chats",
            "app_read_chat",
            "app_open_chat",
            "app_send_message",
            "app_create_chat",
            "app_fork_chat",
            "app_update_chat",
            "app_set_chat_archived",
            "app_stop_chat",
            "app_flag_attention",
            "app_list_projects",
            "app_create_project",
            "app_update_project",
            "app_read_project_context",
            "app_update_project_context",
            "app_worktree_status",
            "app_register_checkout",
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
            "app_ide_context",
            "app_show_images",
            "app_offer_options",
            "app_pin_page",
            "app_list_pages",
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
            "app_voice_show",
            "app_wait_for",
            "app_background_result",
            "app_usage_summary",
            "app_list_knowledge",
            "app_save_rule",
            "app_remove_rule",
            "app_add_skill",
            "app_remove_skill",
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
        let web = helm_hosttools::LaunchKeys {
            web_tools: true,
            ..Default::default()
        };
        assert_eq!(delta(web), ["app_web_search"]);
        let local_without_key = helm_hosttools::LaunchKeys {
            local_chat: true,
            ..Default::default()
        };
        assert!(
            delta(local_without_key).is_empty(),
            "a local chat is offered web search only when the launch says a key is set"
        );
        let restricted = helm_hosttools::LaunchKeys {
            restricted: true,
            web_tools: true,
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
        assert!(
            !extras.orchestrator_on,
            "a chat with no Orchestrator is not marked, so its launch is unchanged"
        );
        let prompt = helm_workers::contrib::system_prompt(&extras).unwrap_or_default();
        // The file-link part is always there; the vault part needs a known vault on the machine.
        assert!(
            prompt.contains("Helm shows your links in a side panel"),
            "the fragment teaches the clickable link forms"
        );
    }
}
