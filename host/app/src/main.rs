//! The Helm desktop app.
//!
//! Startup: resolve the profile, take `app.lock` (or signal the running instance and exit), open the
//! database and run recovery, then build the Tauri app with one command (`api`), one event channel
//! and one main window built in code. Closing the main window hides it; quitting goes through the
//! confirmation and the quit sequence in `helm_core::lifecycle`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pet;
mod petworld;
mod pref_rules;
mod registrations;
mod shell;
mod system;
mod tray;

use helm_core::db::{Database, DbError, MigrationRegistry};
use helm_core::lifecycle::{PlatformDialogs, QuitCoordinator, Shell, WorkerDrain, Workers};
use helm_core::profile::{self, LockOutcome, Profile};
use helm_core::{recovery, revisions::KeyedRevisions};
use helm_ipc::{ApiError, Caller, Dispatcher, OpCtx, OpRegistry, Services};
use helm_platform::Platform;
use serde_json::Value;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tauri::Manager;

/// The one IPC command. Identity comes from the transport, never from `args`.
#[tauri::command]
async fn api(
    webview: tauri::Webview,
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
    op: String,
    args: Option<Value>,
) -> Result<Value, ApiError> {
    let caller = Caller {
        window_label: webview.label().to_owned(),
        url: webview.url().map(|u| u.to_string()).unwrap_or_default(),
    };
    dispatcher
        .call(&caller, &op, args.unwrap_or(Value::Null))
        .await
}

/// Debug builds log to the console. Release builds have no console under
/// `windows_subsystem = "windows"`; their log is written once the profile is known (see
/// [`init_host_log`]), and a failure before that shows as a dialog anyway.
fn init_logging() {
    #[cfg(debug_assertions)]
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
}

/// Release builds: the host's own log to a rolling file in the profile's `diagnostics`
/// folder, the same size-capped one-older-file shape as the other diagnostics logs.
/// Lines never carry tokens or codes.
fn init_host_log(profile: &Profile) {
    #[cfg(not(debug_assertions))]
    {
        let path = profile.path("diagnostics").join("host.log");
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .target(env_logger::Target::Pipe(Box::new(
                helm_core::hostlog::RollingFile::new(&path),
            )))
            .init();
        log::info!("host log: {}", path.display());
    }
    #[cfg(debug_assertions)]
    let _ = profile;
}

/// Any thread's panic goes to the log with its backtrace before the thread (or the app) dies. The
/// hook that was there before still runs after it, so a panic still reaches stderr as ever.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let back = std::backtrace::Backtrace::force_capture();
        log::error!("panic: {info}\n{back}");
        previous(info);
    }));
}

/// One line about the run that ended without closing its record.
fn describe_unclean(u: &helm_core::trail::Unclean) -> String {
    let run = u
        .previous
        .as_ref()
        .map(|p| {
            format!(
                "started {}, version {}",
                p["startedAt"].as_i64().unwrap_or(0),
                p["version"].as_str().unwrap_or("?"),
            )
        })
        .unwrap_or_else(|| "record unreadable".to_owned());
    let beat = match u.last_heartbeat.as_ref().and_then(|h| h["at"].as_i64()) {
        Some(at) => format!("last heartbeat at {}", helm_core::trail::clock_text(at)),
        None => "no heartbeat".to_owned(),
    };
    format!("{run}, {beat}")
}

fn main() -> ExitCode {
    init_logging();
    let platform = Platform::native();
    let profile = match profile::resolve_profile_dir().and_then(|d| Profile::open(&d)) {
        Ok(p) => p,
        Err(e) => return fatal(&platform, &format!("Helm could not open its profile: {e}")),
    };
    init_host_log(&profile);
    install_panic_hook();
    helm_platform::crashdumps::install(&profile.path("diagnostics"));
    let lock = match profile.lock() {
        Ok(LockOutcome::Acquired(lock)) => lock,
        Ok(LockOutcome::AlreadyRunning) => {
            // A second launch only focuses the first instance and exits.
            let endpoint = profile.endpoint();
            return match helm_platform::instance::signal_focus(&endpoint, Duration::from_secs(5)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    log::error!("another Helm holds this profile but did not answer: {e}");
                    ExitCode::from(2)
                }
            };
        }
        Err(e) => return fatal(&platform, &format!("Helm could not lock its profile: {e}")),
    };

    // The trail of this run: its record, and what the run before it ended like.
    let started = std::time::Instant::now();
    if let Some(u) = helm_core::trail::start(
        &profile.path("diagnostics"),
        helm_core::now_ms(),
        env!("CARGO_PKG_VERSION"),
    ) {
        log::warn!(
            "previous run ended without a clean exit: {}",
            describe_unclean(&u)
        );
    }

    // A restore the user staged last session applies now: the profile is ours (locked) and nothing
    // has opened its database yet.
    match helm_backup::apply_pending(&profile.dir) {
        Ok(Some(applied)) => log::info!(
            "restored the profile from {} ({} entries); the previous database is kept in {}",
            applied.archive,
            applied.entries,
            applied.safety_dir.display()
        ),
        Ok(None) => {}
        Err(e) => {
            return fatal(
                &platform,
                &format!("Helm could not finish restoring its backup: {e}"),
            )
        }
    }

    let mut migrations = MigrationRegistry::new();
    registrations::migrations(&mut migrations);
    let db = match Database::open(&profile.db_path(), &migrations) {
        Ok(db) => Arc::new(db),
        Err(e @ DbError::NewerSchema { .. }) => return fatal(&platform, &e.to_string()),
        Err(e) => return fatal(&platform, &format!("Helm could not open its database: {e}")),
    };
    match recovery::run(&db, &migrations) {
        Ok(report) => log::info!("startup recovery: {report:?}"),
        Err(e) => return fatal(&platform, &format!("Startup recovery failed: {e}")),
    }
    let seed = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT COALESCE(MAX(value), 0) FROM counters WHERE key = 'revision'",
                [],
                |r| r.get::<_, i64>(0),
            )
        })
        .unwrap_or(0);
    let clock = Arc::new(helm_chats::clock::Clock::new(seed));
    helm_chats::reconcile_all(&db, &profile, &clock);

    let mut ops = OpRegistry::new();
    registrations::ops(&mut ops);
    run_app(profile, db, platform, ops, clock, started);
    drop(lock);
    ExitCode::SUCCESS
}

/// The quit confirmation. Debug builds honour `HELM_TEST_CONFIRM=ok`, which answers OK without
/// showing the dialog. Release builds ignore it.
fn quit_dialogs(platform: &Platform, debug: bool) -> Arc<dyn helm_platform::Dialogs> {
    if test_confirm(debug, std::env::var("HELM_TEST_CONFIRM").ok().as_deref()) {
        Arc::new(helm_platform::dialogs::FixedDialogs(true))
    } else {
        platform.dialogs.clone()
    }
}

/// A custom-scheme answer from a served reply.
fn scheme_response(
    reply: helm_panel::serve::Reply,
) -> tauri::http::Response<std::borrow::Cow<'static, [u8]>> {
    let mut b = tauri::http::Response::builder().status(reply.status);
    for (k, v) in &reply.headers {
        b = b.header(*k, v.as_str());
    }
    b.body(std::borrow::Cow::Owned(reply.body))
        .unwrap_or_else(|_| {
            tauri::http::Response::builder()
                .status(500)
                .body(std::borrow::Cow::Owned(Vec::new()))
                .expect("an empty response builds")
        })
}

fn test_confirm(debug: bool, value: Option<&str>) -> bool {
    debug && value == Some("ok")
}

fn fatal(platform: &Platform, message: &str) -> ExitCode {
    log::error!("{message}");
    platform.dialogs.confirm("Helm", message);
    ExitCode::FAILURE
}

/// The UI content process can die: probe the main window's page and reload it, with a
/// diagnostic line, once it stops answering while shown (`helm_shell::uiwatch`).
fn start_ui_watchdog(app: tauri::AppHandle, diagnostics: std::path::PathBuf) {
    use helm_shell::uiwatch::{self, Action, UiWatchdog};
    use std::sync::Mutex;
    let dog = Arc::new(Mutex::new(UiWatchdog::new()));
    std::thread::spawn(move || loop {
        std::thread::sleep(uiwatch::PROBE_EVERY);
        let Some(w) = app.get_webview_window(helm_ipc::MAIN_WINDOW) else {
            continue;
        };
        let shown = w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false);
        let action = dog
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .tick(std::time::Instant::now(), shown);
        match action {
            Action::Nothing => {}
            Action::Probe => {
                let dog = dog.clone();
                let _ = w.eval_with_callback(uiwatch::PROBE_SCRIPT, move |answer| {
                    dog.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .answered(&answer);
                });
            }
            Action::Reload { silent_for } => {
                log::error!(
                    "the main window's page did not answer for {} s; reloading it",
                    silent_for.as_secs()
                );
                if let Err(e) =
                    uiwatch::write_diagnostic(&diagnostics, helm_core::now_ms(), silent_for)
                {
                    log::warn!("ui diagnostic not written: {e}");
                }
                if let Err(e) = w.reload() {
                    log::error!("reloading the main window failed: {e}");
                }
            }
        }
    });
}

/// Item titles from the task board, for the links nobody named.
struct BoardTitles(Arc<helm_standup::BoardLink>);

impl helm_itemlinks::titles::TitleSource for BoardTitles {
    fn title(&self, item_id: &str) -> Result<Option<String>, helm_itemlinks::titles::Unavailable> {
        use helm_itemlinks::titles::Unavailable;
        use helm_standup::StandupError;
        let client = self.0.attach_only().map_err(|_| Unavailable)?;
        // Links keep ids without hyphens; the board writes a full id with them.
        let id = if item_id.len() == 32 {
            format!(
                "{}-{}-{}-{}-{}",
                &item_id[..8],
                &item_id[8..12],
                &item_id[12..16],
                &item_id[16..20],
                &item_id[20..]
            )
        } else {
            item_id.to_owned()
        };
        match client.item(&id) {
            Ok(v) => Ok(v
                .get("item")
                .unwrap_or(&v)
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)),
            Err(
                StandupError::NotConfigured
                | StandupError::NotStarted
                | StandupError::Unreachable(_)
                | StandupError::Unauthorized,
            ) => Err(Unavailable),
            Err(StandupError::Status(s)) if s == 429 || s >= 500 => Err(Unavailable),
            Err(_) => Ok(None),
        }
    }
}

fn run_app(
    profile: Profile,
    db: Arc<Database>,
    platform: Platform,
    ops: OpRegistry,
    clock: Arc<helm_chats::clock::Clock>,
    started: std::time::Instant,
) {
    let debug = cfg!(debug_assertions);
    let diag = profile.path("diagnostics");
    let diag_for_setup = diag.clone();
    // Previewed PDFs, audio, video and fonts are served in ranges through one custom scheme; the
    // same server is the `file_read` op's way of issuing the short-lived addresses.
    let private_roots = helm_panel::private::PrivateRoots::new(&{
        let mut roots = vec![profile.dir.clone()];
        if let Ok(d) = helm_core::profile::default_profile_dir() {
            roots.push(d);
        }
        roots
    });
    let media = Arc::new(helm_panel::serve::MediaServer::new(private_roots.clone()));
    let media_for_scheme = media.clone();
    // Local HTML pages run sealed in the side panel; their own scheme serves each open session.
    let pages = Arc::new(helm_pages::PageServer::new(private_roots));
    // An offline page's scripts run only where the web view enforces the header that keeps them
    // off WebRTC; anywhere else such a page is shown without scripts.
    pages.set_offline_scripts(helm_pages::server::engine_seals_offline_scripts(
        cfg!(windows),
        &tauri::webview_version().unwrap_or_default(),
    ));
    let pages_for_scheme = pages.clone();
    let app = tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol(
            helm_panel::serve::SCHEME,
            move |_ctx, request, responder| {
                let media = media_for_scheme.clone();
                // Reads of up to one chunk happen off the main thread.
                std::thread::spawn(move || {
                    let token = request.uri().path().trim_matches('/').to_owned();
                    let range = request
                        .headers()
                        .get("range")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    let reply = media.respond(request.method().as_str(), &token, range.as_deref());
                    responder.respond(scheme_response(reply));
                });
            },
        )
        .register_asynchronous_uri_scheme_protocol(
            helm_pages::server::SCHEME,
            move |_ctx, request, responder| {
                let pages = pages_for_scheme.clone();
                // A page is served from memory, but its assets are read from disk.
                std::thread::spawn(move || {
                    let reply = pages.respond(request.method().as_str(), request.uri().path());
                    responder.respond(scheme_response(reply));
                });
            },
        )
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| tray::on_shortcut(app, event.state()))
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![api])
        .setup(move |app| {
            let handle = app.handle().clone();
            let shell = Arc::new(shell::TauriShell::new(handle.clone()));
            // Events pass the shell service on their way out.
            let tapped = Arc::new(tray::TappedEvents::new(Arc::new(shell::TauriEvents::new(
                handle.clone(),
            ))));
            // The desktop pet's window hears the few events its page draws from.
            let events = pet::PetEvents::wrap(tapped.clone(), handle.clone());
            let events = petworld::wrap(events, handle.clone());
            let worker_source = helm_workers::stage::default_source(
                debug,
                app.path().resource_dir().ok().as_deref(),
            );
            let spawner = Arc::new(
                helm_chats::ProcessSpawner::new(
                    profile.path(""),
                    profile.display_dir(),
                    debug,
                    worker_source,
                )
                .with_contributors(registrations::launch_contributors(&db)),
            );
            let tauri::async_runtime::RuntimeHandle::Tokio(tokio_handle) =
                tauri::async_runtime::handle();
            let runtime = helm_chats::ChatRuntime::new(
                db.clone(),
                clock.clone(),
                events.clone(),
                spawner,
                helm_chats::RuntimeConfig {
                    profile_dir: profile.path(""),
                    limits: Default::default(),
                },
                tokio_handle,
            );
            let dialogs = quit_dialogs(&platform, debug);
            let crashes = shell::WindowCrashes::new(&profile, debug, dialogs.clone());
            let quit = Arc::new(QuitCoordinator::new());
            let drain: Arc<dyn WorkerDrain> = Arc::new(helm_chats::RuntimeDrain(runtime.clone()));
            // The heartbeat: a cheap line every so often, so a silent death has a time and a
            // pile-up has a shape.
            {
                let workers = drain.clone();
                let diag = diag_for_setup.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(helm_core::trail::HEARTBEAT_EVERY);
                    let working = workers.running_work().len();
                    helm_core::trail::heartbeat(
                        &diag,
                        helm_core::now_ms(),
                        started.elapsed().as_secs(),
                        working,
                    );
                });
            }
            // Chats that lose the connection or the computer's attention get a line saying so, and
            // are asked to carry on once things are back.
            let outages = helm_chats::outage::OutageWatch::new(runtime.clone());
            outages.start();
            let mut services = Services::new();
            services.insert(Arc::new(profile.clone()));
            services.insert(media.clone());
            services.insert(pages.clone());
            services.insert(db.clone());
            services.insert(clock.clone());
            services.insert(quit.clone());
            services.insert(Arc::new(KeyedRevisions::new()));
            services.insert(Arc::new(PlatformDialogs(dialogs)));
            services.insert(Arc::new(Workers(drain.clone())));
            services.insert(Arc::new(runtime.clone()));
            services.insert(Arc::new(Shell(shell.clone())));
            services.insert(Arc::new(platform.clone()));
            registrations::services(&mut services, &events);
            // Sleep, wake, sign-out and shutdown.
            helm_platform::session::watch(Arc::new(system::AppSystem {
                outages,
                quit,
                events: events.clone(),
                workers: drain,
                db: db.clone(),
                shell: shell.clone(),
            }));
            let shell_svc = services.get::<helm_shell::ShellService>();
            if let Some(t) = services.get::<helm_terminal::TerminalManager>() {
                app.manage(t);
            }
            if let Some(svc) = &shell_svc {
                tapped.attach(svc.clone());
            }
            if let Some(watch) = services.get::<helm_relay::voice_watch::VoiceWatch>() {
                tapped.attach_voice(watch);
            }
            if let Some(tap) = services.get::<helm_devices::EventTap>() {
                tapped.attach_devices(tap);
            }
            if let Some(trigger) = services.get::<helm_skills::sync::Trigger>() {
                tapped.attach_knowledge(trigger);
            }
            if let Some(cell) = services.get::<helm_mcp::lines::ServicesCell>() {
                tapped.attach_mcp(cell);
            }
            let ctx = OpCtx {
                services: Arc::new(services),
                events,
            };
            let mut dispatcher = Dispatcher::new(ops.clone(), ctx, debug);
            pet::allow_ops(&mut dispatcher);
            petworld::allow_ops(&mut dispatcher);
            let dispatcher = Arc::new(dispatcher);
            registrations::attach_host_tools(&runtime, &dispatcher);
            // Items linked to chats that nobody named are looked up on the board, while one is
            // connected, through the same board connection the board tools use.
            if let Some(board) = dispatcher.ctx().services.get::<helm_standup::BoardLink>() {
                helm_itemlinks::titles::start(
                    db.clone(),
                    Arc::new(BoardTitles(board)),
                    dispatcher.ctx().events.clone(),
                );
            }
            app.manage(dispatcher);
            pet::start(&handle, &db, profile.path("webview"), debug);
            petworld::start(&handle, profile.path("webview"), debug);

            shell::build_main_window(&*app, &crashes)?;
            // If the UI never reports its first frame (a broken bundle), show the window anyway.
            let late = shell.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(4));
                late.show_if_hidden();
            });
            start_ui_watchdog(handle.clone(), profile.path("diagnostics"));
            // The tray, hotkey, badge and keep-awake start from the launch attention summary.
            if let Some(svc) = shell_svc {
                let host = tray::TauriShellHost::new(handle.clone());
                app.manage(host.clone());
                app.manage(svc.clone());
                if let Err(e) = host.install(svc.clone()) {
                    log::error!("tray icon: {e}");
                }
                match runtime.attention() {
                    Ok(s) => svc.on_summary(s, true),
                    Err(e) => log::warn!("launch attention summary: {e}"),
                }
                svc.attach(host);
            }
            // Chats that were working when Helm last closed get their one continuation.
            runtime.start_continuations();

            // The first instance listens for "focus" from later launches.
            let endpoint = profile.endpoint();
            let focus = shell.clone();
            tauri::async_runtime::spawn(async move {
                use helm_core::lifecycle::AppControl;
                if let Err(e) =
                    helm_platform::instance::serve(endpoint, move || focus.show_main()).await
                {
                    log::error!("single-instance listener stopped: {e}");
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == helm_ipc::MAIN_WINDOW {
                    // Closing only hides; the UI flushes drafts on `closing`.
                    api.prevent_close();
                    shell::emit(window.app_handle(), helm_ipc::AppEvent::new("closing"));
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Tauri app");

    app.run(move |app, event| {
        if let tauri::RunEvent::Exit = event {
            // Close this run's record before anything else goes.
            helm_core::trail::record_exit(&diag, helm_core::now_ms());
            // Release keep-awake and never take it again.
            if let Some(svc) = app.try_state::<Arc<helm_shell::ShellService>>() {
                svc.shutdown();
            }
            // Every terminal's tree ends with the app.
            if let Some(t) = app.try_state::<Arc<helm_terminal::TerminalManager>>() {
                t.shutdown();
            }
        }
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            // No code means "the last window went away", which never quits Helm: only an explicit
            // quit (which passes a code) does.
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registration_line_is_distinct() {
        // Building the registries panics on a duplicate op name or migration step.
        let mut ops = OpRegistry::new();
        registrations::ops(&mut ops);
        assert!(ops.contains("app_info"));
        assert!(ops.contains("quit_request"));
        assert!(ops.contains("chat_send"), "the chats crate is registered");
        assert!(
            ops.contains("shell_settings_get"),
            "the shell crate is registered"
        );
        let mut m = MigrationRegistry::new();
        registrations::migrations(&mut m);
    }

    /// Every table any registered crate creates has a class.
    #[test]
    fn every_table_is_classified() {
        let dir = std::env::temp_dir().join(format!("helm-tables-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut m = MigrationRegistry::new();
        registrations::migrations(&mut m);
        let db = Database::open(&dir.join("t.sqlite3"), &m).unwrap();
        let names: Vec<String> = db
            .snapshot(|tx| {
                let mut st = tx.prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                )?;
                let rows = st.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>()
            })
            .unwrap();
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
        let missing: Vec<&String> = names
            .iter()
            .filter(|n| helm_core::db::tables::table_class(n).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "unclassified tables (add a line to helm_core::db::tables::CLASSIFIED_TABLES): {missing:?}"
        );
        assert!(names.len() >= 6, "the migrations ran: {names:?}");
    }

    /// Every setting the tools list (a preference key or a key an extension owns) has exactly one
    /// risk class, and every High one can be staged: a preference with options, or an extension key.
    #[test]
    fn every_setting_key_has_one_risk_class_and_every_high_one_can_be_staged() {
        use helm_hosttools::settings_allow::{high_keys, risk, Risk, NAMED, READ_ONLY};
        let dir = std::env::temp_dir().join(format!("helm-risk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ext_keys: Vec<&str> = [
            helm_workflow::settings_extension(),
            helm_localmodels::settings_extension(),
            helm_relay::settings_extension(&dir),
            helm_chats::runtime_ext::settings_extension(dir.clone()),
            helm_dictation::ext::settings_extension(),
            helm_projects::repo_undo::settings_extension(&dir),
            helm_skills::undo::settings_extension(&dir),
        ]
        .iter()
        .flat_map(|x| x.keys())
        .collect();
        // The extensions that need a live service to build: their keys by name.
        ext_keys.extend([
            helm_mcp::unencrypted::KEY,
            helm_governor::ext::KEY,
            helm_relay::hub_view::KEY,
            "webSearchInClaudeChats",
        ]);
        let _ = std::fs::remove_dir_all(&dir);
        for k in &ext_keys {
            assert!(
                risk(k).is_some() != READ_ONLY.contains(k),
                "{k} is an extension key: give it a class in settings_allow::NAMED"
            );
            assert!(
                helm_core::prefs::classify(k).is_none(),
                "{k} clashes with a preference key"
            );
        }
        for (k, r, _) in NAMED {
            if *r == Risk::Low {
                assert!(
                    ext_keys.contains(k),
                    "{k} is named Low but no extension owns it"
                );
            }
        }
        // A High key is either a preference with options (so it has a validator) or an extension
        // key (whose own apply is the validator).
        for k in high_keys() {
            let pref = helm_core::prefs::classify(k).is_some();
            assert!(
                (pref && helm_hosttools::settings_options::describe(k).is_some())
                    || (!pref && ext_keys.contains(&k)),
                "{k} is High but cannot be staged"
            );
        }
    }

    #[test]
    fn the_confirm_probe_is_debug_only() {
        assert!(test_confirm(true, Some("ok")));
        assert!(!test_confirm(false, Some("ok")), "release builds ignore it");
        assert!(!test_confirm(true, Some("yes")));
        assert!(!test_confirm(true, None));
    }

    /// The CSP relaxes styles only: inline styles are allowed and
    /// Tauri leaves style-src alone so its nonce cannot cancel 'unsafe-inline'; scripts keep
    /// 'self' plus Tauri's nonces.
    #[test]
    fn csp_relaxes_styles_only() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let sec = &conf["app"]["security"];
        assert_eq!(sec["csp"]["style-src"], "'self' 'unsafe-inline'");
        assert_eq!(sec["csp"]["script-src"], "'self'");
        assert_eq!(sec["csp"]["default-src"], "'none'");
        assert_eq!(
            sec["dangerousDisableAssetCspModification"],
            serde_json::json!(["style-src"])
        );
    }
}
