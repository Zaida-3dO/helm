//! The `api` ops served by this crate, and its migration registrations.
//!
//! | op | class | does |
//! |---|---|---|
//! | `app_info` | blocking | product name, version, schema version, profile folder |
//! | `preference_get` | blocking | `{key}` → stored JSON or `null` |
//! | `preference` | maintenance | `{key, value}` through the generic writer; emits `preferences` |
//! | `quit_request` | async | the quit sequence (confirmation, `quitting`, drain, checkpoint, exit) |
//! | `quit_ready` | blocking | the UI reports drafts and settings saved |
//! | `window_hide` | blocking | hide the main window (closing only hides it) |
//! | `window_ready` | blocking | the UI drew its first frame: show the window created hidden |

use crate::db::{Database, MigrationRegistry};
use crate::lifecycle::{PlatformDialogs, QuitCoordinator, QuitOutcome, Shell, Workers};
use crate::prefs;
use crate::profile::Profile;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde_json::{json, Value};

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("app_info", OpClass::Blocking, app_info)
        .blocking("preference_get", OpClass::Blocking, preference_get)
        .blocking("preference", OpClass::Maintenance, preference_set)
        .blocking("quit_ready", OpClass::Blocking, quit_ready)
        .blocking("window_hide", OpClass::Blocking, window_hide)
        .blocking("window_ready", OpClass::Blocking, window_ready)
        .async_op("quit_request", quit_request);
}

/// Core tables come from the versioned migrations; core registers no feature steps yet.
pub fn register_migrations(_reg: &mut MigrationRegistry) {}

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, ApiError> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_args(format!("{name} must be a string")))
}

fn app_info(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let db = ctx.services.need::<Database>()?;
    let schema = db
        .user_version()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(json!({
        "name": "Helm",
        "edition": "desktop",
        "version": env!("CARGO_PKG_VERSION"),
        "schemaVersion": schema,
        "profileDir": profile.display_dir(),
    }))
}

fn preference_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let key = str_arg(&args, "key")?;
    if prefs::classify(key).is_none() {
        return Err(ApiError::bad_args("unknown preference"));
    }
    let db = ctx.services.need::<Database>()?;
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, key).unwrap_or(Value::Null)))
        .map_err(|e| ApiError::internal(e.to_string()))
}

fn preference_set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let key = str_arg(&args, "key")?.to_owned();
    let value = args.get("value").cloned().unwrap_or(Value::Null);
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| prefs::set_generic(tx, &key, &value))
        .map_err(|e| match e {
            prefs::PrefError::Sql(e) => ApiError::internal(e.to_string()),
            other => ApiError::bad_args(other.to_string()),
        })?;
    ctx.events
        .emit(AppEvent::new("preferences").with("changed", json!({ key: value })));
    Ok(Value::Null)
}

fn quit_ready(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    ctx.services.need::<QuitCoordinator>()?.ui_saved();
    Ok(Value::Null)
}

fn window_hide(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    ctx.services.need::<Shell>()?.0.hide_main();
    Ok(Value::Null)
}

fn window_ready(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    ctx.services.need::<Shell>()?.0.show_main();
    Ok(Value::Null)
}

async fn quit_request(ctx: OpCtx, _args: Value) -> Result<Value, ApiError> {
    let quit = ctx.services.need::<QuitCoordinator>()?;
    let dialogs = ctx.services.need::<PlatformDialogs>()?.0.clone();
    let workers = ctx.services.need::<Workers>()?.0.clone();
    let shell = ctx.services.need::<Shell>()?.0.clone();
    let db = ctx.services.get::<Database>();
    let outcome = quit.run(dialogs, ctx.events.clone(), workers, db).await;
    match outcome {
        QuitOutcome::Quit { ui_saved, drained } => {
            log::info!("quitting (ui saved: {ui_saved}, workers drained: {drained})");
            shell.exit(0);
            Ok(json!({"outcome": "quit"}))
        }
        QuitOutcome::Cancelled => Ok(json!({"outcome": "cancelled"})),
        QuitOutcome::Busy => Ok(json!({"outcome": "busy"})),
        QuitOutcome::SaveFailed => Ok(json!({"outcome": "save_failed"})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{AppControl, NoWorkers};
    use helm_ipc::{Caller, Dispatcher, RecordingSink, Services};
    use helm_platform::dialogs::FixedDialogs;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;

    struct FakeShell(AtomicI32);
    impl AppControl for FakeShell {
        fn show_main(&self) {}
        fn hide_main(&self) {}
        fn exit(&self, code: i32) {
            self.0.store(code + 100, Ordering::SeqCst);
        }
    }

    fn setup(
        confirm: bool,
    ) -> (
        tempfile::TempDir,
        Dispatcher,
        Arc<RecordingSink>,
        Arc<FakeShell>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::open(&dir.path().join("prof")).unwrap();
        let db = Database::open(&profile.db_path(), &MigrationRegistry::new()).unwrap();
        let shell = Arc::new(FakeShell(AtomicI32::new(0)));
        let mut s = Services::new();
        s.insert(Arc::new(profile));
        s.insert(Arc::new(db));
        s.insert(Arc::new(QuitCoordinator::new()));
        s.insert(Arc::new(PlatformDialogs(Arc::new(FixedDialogs(confirm)))));
        s.insert(Arc::new(Workers(Arc::new(NoWorkers))));
        s.insert(Arc::new(Shell(shell.clone())));
        let sink = Arc::new(RecordingSink::default());
        let mut reg = OpRegistry::new();
        register_ops(&mut reg);
        let ctx = OpCtx {
            services: Arc::new(s),
            events: sink.clone(),
        };
        (dir, Dispatcher::new(reg, ctx, false), sink, shell)
    }

    fn main() -> Caller {
        Caller {
            window_label: "main".into(),
            url: "http://tauri.localhost/".into(),
        }
    }

    #[tokio::test]
    async fn app_info_reports_the_profile() {
        let (_d, api, _, _) = setup(true);
        let info = api.call(&main(), "app_info", Value::Null).await.unwrap();
        assert_eq!(info["name"], "Helm");
        assert_eq!(info["schemaVersion"], crate::db::schema::CURRENT_VERSION);
    }

    #[tokio::test]
    async fn preference_round_trip_emits_an_event() {
        let (_d, api, sink, _) = setup(true);
        api.call(
            &main(),
            "preference",
            json!({"key": "sidebarWidth", "value": 300}),
        )
        .await
        .unwrap();
        let v = api
            .call(&main(), "preference_get", json!({"key": "sidebarWidth"}))
            .await;
        assert_eq!(v.unwrap(), json!(300));
        assert_eq!(sink.kinds(), ["preferences"]);
        let bad = api
            .call(
                &main(),
                "preference",
                json!({"key": "sidebarWidth", "value": 1}),
            )
            .await;
        assert_eq!(bad.unwrap_err().code, "bad_args");
    }

    #[tokio::test]
    async fn confirmed_quit_exits_and_cancelled_does_not() {
        let (_d, api, _, shell) = setup(false);
        let out = api
            .call(&main(), "quit_request", Value::Null)
            .await
            .unwrap();
        assert_eq!(out["outcome"], "cancelled");
        assert_eq!(shell.0.load(Ordering::SeqCst), 0);

        let (_d2, api, sink, shell) = setup(true);
        let out = api
            .call(&main(), "quit_request", Value::Null)
            .await
            .unwrap();
        assert_eq!(out["outcome"], "quit");
        assert_eq!(shell.0.load(Ordering::SeqCst), 100, "exit(0) was called");
        assert_eq!(sink.kinds(), ["quitting"]);
    }
}
