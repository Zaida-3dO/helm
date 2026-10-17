//! The Helm desktop app.
//!
//! Startup: resolve the profile, take `app.lock` (or signal the running instance and exit), open the
//! database and run recovery, then build the Tauri app with one command (`api`), one event channel
//! and one main window built in code. Closing the main window hides it; quitting goes through the
//! confirmation and the quit sequence in `helm_core::lifecycle`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod registrations;
mod shell;

use helm_core::db::{Database, DbError, MigrationRegistry};
use helm_core::lifecycle::{PlatformDialogs, QuitCoordinator, Shell, Workers};
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

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let platform = Platform::native();
    let profile = match profile::resolve_profile_dir().and_then(|d| Profile::open(&d)) {
        Ok(p) => p,
        Err(e) => return fatal(&platform, &format!("Helm could not open its profile: {e}")),
    };
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
    run_app(profile, db, platform, ops, clock);
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

fn test_confirm(debug: bool, value: Option<&str>) -> bool {
    debug && value == Some("ok")
}

fn fatal(platform: &Platform, message: &str) -> ExitCode {
    log::error!("{message}");
    platform.dialogs.confirm("Helm", message);
    ExitCode::FAILURE
}

fn run_app(
    profile: Profile,
    db: Arc<Database>,
    platform: Platform,
    ops: OpRegistry,
    clock: Arc<helm_chats::clock::Clock>,
) {
    let debug = cfg!(debug_assertions);
    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![api])
        .setup(move |app| {
            let handle = app.handle().clone();
            let shell = Arc::new(shell::TauriShell::new(handle.clone()));
            let events: Arc<dyn helm_ipc::EventSink> =
                Arc::new(shell::TauriEvents::new(handle.clone()));
            let worker_source = helm_workers::stage::default_source(
                debug,
                app.path().resource_dir().ok().as_deref(),
            );
            let spawner = Arc::new(helm_chats::ProcessSpawner::new(
                profile.path(""),
                profile.display_dir(),
                debug,
                worker_source,
            ));
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
            let mut services = Services::new();
            services.insert(Arc::new(profile.clone()));
            services.insert(db.clone());
            services.insert(clock.clone());
            services.insert(Arc::new(QuitCoordinator::new()));
            services.insert(Arc::new(KeyedRevisions::new()));
            services.insert(Arc::new(PlatformDialogs(dialogs)));
            services.insert(Arc::new(Workers(Arc::new(helm_chats::RuntimeDrain(
                runtime.clone(),
            )))));
            services.insert(Arc::new(runtime.clone()));
            services.insert(Arc::new(Shell(shell.clone())));
            services.insert(Arc::new(platform.clone()));
            registrations::services(&mut services, &events);
            let ctx = OpCtx {
                services: Arc::new(services),
                events,
            };
            let dispatcher = Arc::new(Dispatcher::new(ops.clone(), ctx, debug));
            app.manage(dispatcher);

            shell::build_main_window(app, &profile, debug)?;
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

    app.run(|_app, event| {
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
        let mut m = MigrationRegistry::new();
        registrations::migrations(&mut m);
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
