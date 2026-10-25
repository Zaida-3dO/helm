//! Updating Helm from a folder on this computer.
//!
//! There is no update feed and nothing is downloaded. The user points Helm at the folder where the
//! installer build writes `Helm_<version>_x64-setup.exe` (Settings, General). After launch and then
//! every 10 minutes Helm looks there for a setup file with a higher version than its own. When one
//! is found the top bar offers "Restart to update"; confirming quits Helm the way a quit does
//! (drafts saved, workers drained) and starts that setup file, hands-free and detached.
//!
//! | op | class | does |
//! |---|---|---|
//! | `update_status` | blocking | the folder, the newer setup found (if any), the last check |
//! | `update_check` | blocking | look in the folder now, then the same as `update_status` |
//! | `update_source_set` | maintenance | `{path}`: choose the folder (`null` clears it), then check |
//! | `update_prepare` | blocking | re-check the setup file; the running chats the restart would stop |
//! | `update_install` | async | quit with drafts saved, then start the setup file and exit |
//!
//! The renderer never names the file to run: the host re-checks its own folder setting and picks
//! the file itself, so a compromised page cannot make Helm run an arbitrary program.

pub mod scan;

use helm_core::db::Database;
use helm_core::lifecycle::{QuitCoordinator, QuitOutcome, Shell, Workers};
use helm_core::prefs::{self, PrefKey};
use helm_ipc::{ApiError, AppEvent, EventSink, OpClass, OpCtx, OpRegistry, Services};
use scan::{Candidate, Version};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The folder to look in. Only the user chooses it, because it decides what program Helm runs.
pub const UPDATE_SOURCE: PrefKey = PrefKey::classified("updateSource");

const FIRST_CHECK_AFTER: Duration = Duration::from_secs(15);
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
/// The restart dialog lists this many running chats and counts the rest.
const BUSY_SHOWN: usize = 4;
/// The flags that make the per-user setup run without questions, replace the running app and
/// open it again. Never the silent flag: the user watches it happen.
#[cfg_attr(not(windows), allow(dead_code))]
const SETUP_ARGS: [&str; 3] = ["/P", "/UPDATE", "/R"];

/// Starts a setup file. The app uses the real launcher; tests swap it so no program ever runs.
pub type LaunchFn = dyn Fn(&Path) -> std::io::Result<()> + Send + Sync;

#[derive(Clone)]
pub struct SetupLauncher(pub Arc<LaunchFn>);

impl SetupLauncher {
    pub fn real() -> Self {
        SetupLauncher(Arc::new(launch_detached))
    }
}

#[cfg(windows)]
fn launch_detached(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    std::process::Command::new(path)
        .args(SETUP_ARGS)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
fn launch_detached(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "updating from a setup file works on Windows only",
    ))
}

#[derive(Default)]
struct State {
    found: Option<Candidate>,
    checked_at: Option<u64>,
    error: Option<String>,
}

pub struct UpdateService {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    current: Version,
    state: Mutex<State>,
}

impl UpdateService {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>, current: Version) -> Self {
        Self {
            db,
            events,
            current,
            state: Mutex::new(State::default()),
        }
    }

    fn source(&self) -> Option<PathBuf> {
        self.db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, UPDATE_SOURCE.name())))
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(PathBuf::from))
            .filter(|p| !p.as_os_str().is_empty())
    }

    /// Look in the folder now. Emits `update` when what the user sees changed.
    pub fn check(&self) -> Value {
        let (found, error) = match self.source() {
            None => (None, None),
            Some(dir) => match scan::scan(&dir, &self.current) {
                Ok(found) => (found, None),
                Err(e) => (None, Some(e)),
            },
        };
        let changed = {
            let mut st = self.state.lock().unwrap();
            let changed = st.found != found || st.error != error;
            st.found = found;
            st.error = error;
            st.checked_at = Some(now_ms());
            changed
        };
        if changed {
            self.events.emit(AppEvent::new("update"));
        }
        self.status()
    }

    pub fn status(&self) -> Value {
        let st = self.state.lock().unwrap();
        json!({
            "current": env!("CARGO_PKG_VERSION"),
            "source": self.source().map(|p| p.display().to_string()),
            "available": st.found.as_ref().map(|c| json!({"version": c.version, "fileName": c.file_name})),
            "checkedAt": st.checked_at,
            "error": st.error,
        })
    }

    /// The newer setup file, checked again against the disk. The path is built here, from the
    /// saved folder, never taken from the caller.
    fn candidate(&self) -> Result<(Candidate, PathBuf), ApiError> {
        let dir = self
            .source()
            .ok_or_else(|| ApiError::conflict("No update folder is chosen."))?;
        let found = scan::scan(&dir, &self.current)
            .map_err(ApiError::conflict)?
            .ok_or_else(|| ApiError::conflict("There is no newer setup file in the folder."))?;
        let path =
            scan::validate(&dir, &found.file_name, &self.current).map_err(ApiError::conflict)?;
        Ok((found, path))
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("update_status", OpClass::Blocking, update_status)
        .blocking("update_check", OpClass::Blocking, update_check)
        .blocking("update_source_set", OpClass::Maintenance, update_source_set)
        .blocking("update_prepare", OpClass::Blocking, update_prepare)
        .async_op("update_install", update_install);
}

/// Adds the service and starts the periodic look: once shortly after launch, then every 10 minutes.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let Some(db) = services.get::<Database>() else {
        log::error!("helm-updates: database missing; update checks are off");
        return;
    };
    let Some(current) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        log::error!("helm-updates: the app version is not a plain version; update checks are off");
        return;
    };
    let svc = Arc::new(UpdateService::new(db, events.clone(), current));
    services.insert(Arc::new(SetupLauncher::real()));
    services.insert(svc.clone());
    std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            std::thread::sleep(FIRST_CHECK_AFTER);
            loop {
                svc.check();
                std::thread::sleep(CHECK_EVERY);
            }
        })
        .map_or_else(|e| log::warn!("update check thread: {e}"), |_| ());
}

fn update_status(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    Ok(ctx.services.need::<UpdateService>()?.status())
}

fn update_check(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    Ok(ctx.services.need::<UpdateService>()?.check())
}

fn update_source_set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let svc = ctx.services.need::<UpdateService>()?;
    let value = match args.get("path") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) if s.trim().is_empty() => Value::Null,
        Some(Value::String(s)) => {
            let dir = Path::new(s.trim());
            std::fs::symlink_metadata(dir)
                .ok()
                .filter(|m| dir.is_absolute() && m.is_dir() && !helm_platform::fs::is_link_like(m))
                .ok_or_else(|| ApiError::bad_args("Choose an existing folder."))?;
            Value::String(s.trim().to_owned())
        }
        Some(_) => return Err(ApiError::bad_args("path must be a string or null")),
    };
    svc.db
        .write(|tx| prefs::set(tx, UPDATE_SOURCE, &value))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(svc.check())
}

fn update_prepare(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let svc = ctx.services.need::<UpdateService>()?;
    let (found, _) = svc.candidate()?;
    let running = ctx
        .services
        .get::<Workers>()
        .map(|w| w.0.running_work())
        .unwrap_or_default();
    Ok(json!({
        "version": found.version,
        "busy": running.iter().take(BUSY_SHOWN).collect::<Vec<_>>(),
        "busyTotal": running.len(),
    }))
}

async fn update_install(ctx: OpCtx, _args: Value) -> Result<Value, ApiError> {
    let svc = ctx.services.need::<UpdateService>()?;
    let launcher = ctx.services.need::<SetupLauncher>()?;
    let quit = ctx.services.need::<QuitCoordinator>()?;
    let workers = ctx.services.need::<Workers>()?.0.clone();
    let shell = ctx.services.need::<Shell>()?.0.clone();
    // Refuse before stopping anything when the file is not good.
    svc.candidate()?;
    match quit
        .run_for_update(ctx.events.clone(), workers, ctx.services.get::<Database>())
        .await
    {
        QuitOutcome::Quit { ui_saved, drained } => {
            log::info!("restarting to update (ui saved: {ui_saved}, workers drained: {drained})");
            // Check once more as late as possible, then start the setup file and leave.
            let started = svc.candidate().and_then(|(_, path)| {
                (launcher.0)(&path).map_err(|e| ApiError::internal(e.to_string()))
            });
            match started {
                Ok(()) => {
                    shell.exit(0);
                    Ok(json!({"outcome": "restarting"}))
                }
                Err(e) => {
                    quit.abort();
                    Err(e)
                }
            }
        }
        QuitOutcome::SaveFailed => Ok(json!({"outcome": "save_failed"})),
        QuitOutcome::Busy => Ok(json!({"outcome": "busy"})),
        QuitOutcome::Cancelled => Ok(json!({"outcome": "cancelled"})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;
    use helm_core::lifecycle::{AppControl, WorkerDrain};
    use helm_core::profile::Profile;
    use helm_ipc::RecordingSink;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicI32, Ordering};

    struct FakeShell(AtomicI32);
    impl AppControl for FakeShell {
        fn show_main(&self) {}
        fn hide_main(&self) {}
        fn exit(&self, code: i32) {
            self.0.store(code + 100, Ordering::SeqCst);
        }
    }

    struct Busy(usize);
    impl WorkerDrain for Busy {
        fn running_work(&self) -> Vec<String> {
            (0..self.0).map(|i| format!("chat {i} (C:/p)")).collect()
        }
        fn drain(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async {})
        }
    }

    #[tokio::test]
    async fn choose_a_folder_see_the_pill_data_and_restart_into_the_setup() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::open(&dir.path().join("prof")).unwrap();
        let db = Arc::new(Database::open(&profile.db_path(), &MigrationRegistry::new()).unwrap());
        let sink = Arc::new(RecordingSink::default());
        let shell = Arc::new(FakeShell(AtomicI32::new(0)));
        let launched: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let l2 = launched.clone();
        let quit = Arc::new(QuitCoordinator::new());
        let svc = Arc::new(UpdateService::new(
            db.clone(),
            sink.clone(),
            Version::parse("0.3.0").unwrap(),
        ));
        let mut s = Services::new();
        s.insert(db);
        s.insert(svc.clone());
        s.insert(quit.clone());
        s.insert(Arc::new(Workers(Arc::new(Busy(6)))));
        s.insert(Arc::new(Shell(shell.clone())));
        s.insert(Arc::new(SetupLauncher(Arc::new(move |p| {
            l2.lock().unwrap().push(p.to_path_buf());
            Ok(())
        }))));
        let ctx = OpCtx {
            services: Arc::new(s),
            events: sink.clone(),
        };

        // Nothing chosen: no pill, and the restart is refused.
        assert_eq!(
            update_status(&ctx, Value::Null).unwrap()["available"],
            Value::Null
        );
        assert_eq!(
            update_prepare(&ctx, Value::Null).unwrap_err().code,
            "conflict"
        );
        assert_eq!(
            update_install(ctx.clone(), Value::Null)
                .await
                .unwrap_err()
                .code,
            "conflict"
        );
        // A file is not a folder, a folder is.
        let setups = dir.path().join("setups");
        std::fs::create_dir(&setups).unwrap();
        std::fs::write(dir.path().join("a-file"), b"x").unwrap();
        let set = |p: &Path| update_source_set(&ctx, json!({"path": p.display().to_string()}));
        assert_eq!(
            set(&dir.path().join("a-file")).unwrap_err().code,
            "bad_args"
        );
        let st = set(&setups).unwrap();
        assert_eq!(st["available"], Value::Null);
        assert_eq!(st["error"], Value::Null);

        // A newer setup appears: the next check reports it once, and the dialog data lists at
        // most four of the six running chats.
        std::fs::write(setups.join("Helm_0.3.1_x64-setup.exe"), b"MZ").unwrap();
        let kinds_before = sink.kinds().len();
        let st = update_check(&ctx, Value::Null).unwrap();
        assert_eq!(st["available"]["version"], "0.3.1");
        update_check(&ctx, Value::Null).unwrap();
        assert_eq!(
            sink.kinds().len(),
            kinds_before + 1,
            "one event for one change"
        );
        let prep = update_prepare(&ctx, Value::Null).unwrap();
        assert_eq!(prep["busy"].as_array().unwrap().len(), 4);
        assert_eq!(prep["busyTotal"], 6);

        // Confirm: drafts are saved (the acknowledgement), the setup starts, Helm exits.
        let q2 = quit.clone();
        let acker = std::thread::spawn(move || {
            while !q2.is_quitting() {
                std::thread::sleep(Duration::from_millis(2));
            }
            q2.ui_saved();
        });
        let out = update_install(ctx.clone(), Value::Null).await.unwrap();
        acker.join().unwrap();
        assert_eq!(out["outcome"], "restarting");
        assert_eq!(
            *launched.lock().unwrap(),
            vec![setups.join("Helm_0.3.1_x64-setup.exe")]
        );
        assert_eq!(shell.0.load(Ordering::SeqCst), 100);
    }
}
