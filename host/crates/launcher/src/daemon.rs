//! The launcher process itself: `helm-launcher.exe --profile <dir>`.
//!
//! Two tasks on one current-thread runtime:
//!
//! - **The watch.** While Helm runs it waits on the process's handle (no polling); when that process
//!   ends it looks again after [`watch::RESTART_DELAY`] and restarts Helm if [`watch::decide`] says
//!   it crashed. While Helm is not running it reads `run.json` every [`watch::LOOK_EVERY`].
//! - **The hub line.** One outbound WebSocket to the hub's `/launcher` (the saved relay
//!   addresses, LAN first, with the relay token), with backoff from 1 s up to 60 s, and 10 minutes
//!   for an older hub that has no such line. It says whether Helm runs, and starts Helm when the
//!   hub asks (`launch`), hidden in the tray.
//!
//! A third, plain thread waits on the profile's stop event, which Helm sets before it replaces
//! the program or when the switch goes off. The log is `diagnostics/launcher.log` in the profile;
//! it never holds the token.

use crate::frames::{self, FromHub, Wake};
use crate::install::{self, Status as FileStatus};
use crate::{hubline, sys, watch};
use futures_util::{SinkExt, StreamExt};
use helm_core::credentials::CredentialStore;
use helm_core::profile::Profile;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{watch as tokio_watch, Notify};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// An older hub without the launcher line answers 404: ask again this much later.
const OLD_HUB_BACKOFF: Duration = Duration::from_secs(10 * 60);
/// With no hub saved, how often the store is read again.
const UNPAIRED_LOOK: Duration = Duration::from_secs(60);
/// A Helm the launcher started this recently counts as starting: a second `launch` does not start
/// another.
const STARTING_FOR: Duration = Duration::from_secs(60);
/// The largest frame the hub sends here is a short JSON object.
const MAX_FRAME: usize = 64 * 1024;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// What the hub hears about Helm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HelmState {
    pub helm_running: bool,
    pub crash_loop: bool,
}

pub struct Daemon {
    profile_dir: PathBuf,
    is_default_profile: bool,
    creds: CredentialStore,
    state: tokio_watch::Sender<HelmState>,
    /// Woken after a launch, so the watch picks the new Helm up at once.
    started: Notify,
    /// The Helm this launcher started last, and when.
    last_start: Mutex<Option<(Instant, u32)>>,
    /// The hub has taken this PC's wake details (kept across lines: the hub keeps them too).
    wake_told: Mutex<bool>,
}

enum ConnectError {
    /// An older hub with no launcher line.
    Missing,
    Other(String),
}

impl Daemon {
    pub fn new(profile: &Profile, keychain: Arc<dyn helm_platform::Keychain>) -> Arc<Self> {
        Arc::new(Daemon {
            profile_dir: profile.dir.clone(),
            is_default_profile: crate::ops::is_default_profile(profile),
            creds: CredentialStore::new(&profile.path("credentials"), &profile.hash, keychain),
            state: tokio_watch::channel(HelmState::default()).0,
            started: Notify::new(),
            last_start: Mutex::new(None),
            wake_told: Mutex::new(false),
        })
    }

    /// The handoff, or the end of this launcher: missing or switched off means it should not run.
    fn handoff_or_exit(&self) -> install::Handoff {
        match install::read_handoff(&self.profile_dir) {
            Some(h) if h.enabled => h,
            _ => {
                log::info!("launcher: switched off in Helm; exiting");
                install::remove_pid(&self.profile_dir);
                std::process::exit(0);
            }
        }
    }

    /// The Helm process now running in this profile, if any.
    fn helm_pid(&self, helm_exe: &Path) -> Option<u32> {
        let name = helm_exe
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "helm.exe".to_owned());
        let rec = watch::read_run(&self.profile_dir);
        watch::running_pid(rec.as_ref(), |p| sys::process_is(p, &name))
    }

    fn set_state(&self, f: impl FnOnce(&mut HelmState)) {
        self.state.send_if_modified(|s| {
            let before = *s;
            f(s);
            *s != before
        });
    }

    /// Start Helm hidden in the tray, in this launcher's profile.
    fn start_helm(&self, helm_exe: &Path) -> Result<u32, &'static str> {
        if !helm_exe.is_file() {
            return Err("Helm's program file is missing on this PC; it may have been uninstalled.");
        }
        let dir = crate::runkey::plain_path(&self.profile_dir);
        // The default profile is Helm's own default; any other is named the way Helm reads it.
        let profile_env = if self.is_default_profile {
            None
        } else {
            Some(OsStr::new(&dir))
        };
        match sys::spawn_detached(
            helm_exe,
            &[OsStr::new("--background")],
            &[(helm_core::profile::PROFILE_ENV, profile_env)],
        ) {
            Ok(pid) => {
                *self.last_start.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some((Instant::now(), pid));
                self.started.notify_one();
                Ok(pid)
            }
            Err(e) => {
                log::warn!("launcher: Helm could not be started: {e}");
                Err("Windows refused to start Helm on this PC.")
            }
        }
    }

    /// A Helm this launcher started a moment ago that is still coming up.
    fn starting(&self) -> bool {
        self.last_start
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some_and(|(at, pid)| at.elapsed() < STARTING_FOR && sys::process_alive(pid))
    }

    /// Answer the hub's `launch`.
    fn on_launch(&self, id: &str) -> serde_json::Value {
        let handoff = self.handoff_or_exit();
        let exe = PathBuf::from(&handoff.helm_exe);
        if self.helm_pid(&exe).is_some() || self.starting() {
            return frames::launched(id, true, true, None);
        }
        match self.start_helm(&exe) {
            Ok(pid) => {
                log::info!("launcher: started Helm for the hub (pid {pid})");
                frames::launched(id, true, false, None)
            }
            Err(why) => frames::launched(id, false, false, Some(why)),
        }
    }

    /// Watch Helm: wait on it while it runs, and restart it after a crash.
    pub async fn watch_helm(self: Arc<Self>) {
        let mut restarts = watch::Restarts::default();
        loop {
            let handoff = self.handoff_or_exit();
            let exe = PathBuf::from(&handoff.helm_exe);
            let Some(pid) = self.helm_pid(&exe) else {
                self.set_state(|s| s.helm_running = false);
                let next = if self.starting() {
                    Duration::from_secs(1)
                } else {
                    watch::LOOK_EVERY
                };
                tokio::select! {
                    _ = tokio::time::sleep(next) => {}
                    _ = self.started.notified() => {}
                }
                continue;
            };
            let Some(handle) = sys::process_handle(pid) else {
                // It ended between the two looks: look again in a moment.
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            };
            // Helm is up. After a crash loop, a Helm that runs again was started some other way:
            // restarting may begin afresh.
            if self.state.borrow().crash_loop {
                restarts.clear();
            }
            self.set_state(|s| {
                s.helm_running = true;
                s.crash_loop = false;
            });
            let _ = tokio::task::spawn_blocking(move || handle.wait()).await;
            self.set_state(|s| s.helm_running = false);
            tokio::time::sleep(watch::RESTART_DELAY).await;
            if sys::shutting_down() {
                continue;
            }
            let handoff = self.handoff_or_exit();
            let exe = PathBuf::from(&handoff.helm_exe);
            let rec = watch::read_run(&self.profile_dir);
            let alive = rec
                .as_ref()
                .and_then(|r| r.pid)
                .is_some_and(sys::process_alive);
            let now = Instant::now();
            match watch::decide(
                rec.as_ref(),
                alive,
                restarts.in_window(now),
                watch::age_of(&exe),
            ) {
                watch::Decision::Nothing => {}
                watch::Decision::Relaunch => {
                    log::warn!("launcher: Helm (pid {pid}) ended without closing; restarting it");
                    restarts.record(now);
                    let _ = self.start_helm(&exe);
                }
                watch::Decision::GiveUp => {
                    log::error!(
                        "launcher: Helm keeps ending without closing ({} restarts in {} minutes); not restarting it again",
                        watch::MAX_RESTARTS,
                        watch::RESTART_WINDOW.as_secs() / 60
                    );
                    self.set_state(|s| s.crash_loop = true);
                }
            }
        }
    }

    fn write_status(&self, hub: &str) {
        let wake = *self.wake_told.lock().unwrap_or_else(|p| p.into_inner());
        let s = FileStatus {
            hub: hub.to_owned(),
            wake,
            at: helm_core::now_ms(),
        };
        if let Err(e) = install::write_status(&self.profile_dir, &s) {
            log::warn!("launcher: status not written: {e}");
        }
    }

    /// Keep the hub line up.
    pub async fn hub_line(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        let mut told_old_hub = false;
        loop {
            let line = hubline::effective(&self.creds);
            if !line.usable() {
                self.write_status("off");
                tokio::time::sleep(UNPAIRED_LOOK).await;
                continue;
            }
            let mut served = false;
            let mut old_hub = false;
            for url in line.launcher_urls() {
                match connect(&url, &line.token).await {
                    Ok(ws) => {
                        backoff = Duration::from_secs(1);
                        told_old_hub = false;
                        log::info!("launcher: hub line up");
                        self.serve(ws).await;
                        log::info!("launcher: hub line down");
                        self.write_status("offline");
                        served = true;
                        break;
                    }
                    Err(ConnectError::Missing) => old_hub = true,
                    Err(ConnectError::Other(why)) => log::info!("launcher: {why}"),
                }
            }
            let wait = if served {
                Duration::from_secs(1)
            } else if old_hub {
                if !told_old_hub {
                    log::info!(
                        "launcher: the hub has no launcher line (an older hub); asking again every 10 minutes"
                    );
                    told_old_hub = true;
                }
                self.write_status("unsupported");
                OLD_HUB_BACKOFF
            } else {
                self.write_status("offline");
                let b = backoff;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                b
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// One line, until it ends.
    async fn serve(&self, ws: Ws) {
        let wake = match ws.get_ref() {
            tokio_tungstenite::MaybeTlsStream::Plain(s) => s.local_addr().ok(),
            _ => None,
        }
        .and_then(|a| match a.ip() {
            std::net::IpAddr::V4(v4) => crate::wake::for_local_address(v4),
            std::net::IpAddr::V6(_) => None,
        });
        let (mut tx, mut rx) = ws.split();
        let mut state = self.state.subscribe();
        let now = *state.borrow_and_update();
        if send(&mut tx, frames::hello(now.helm_running, wake.as_ref()))
            .await
            .is_err()
        {
            return;
        }
        let mut ping = tokio::time::interval(frames::PING_EVERY);
        ping.tick().await; // the immediate first tick
        let silence = tokio::time::sleep(frames::SILENCE);
        tokio::pin!(silence);
        loop {
            tokio::select! {
                m = rx.next() => {
                    silence.as_mut().reset(tokio::time::Instant::now() + frames::SILENCE);
                    match m {
                        Some(Ok(Message::Text(t))) => match frames::parse(t.as_str()) {
                            FromHub::Welcome { .. } => self.welcomed(wake.as_ref()),
                            FromHub::Launch { id, reason } => {
                                log::info!("launcher: the hub asked to start Helm ({})", reason_text(&reason));
                                let reply = self.on_launch(&id);
                                if send(&mut tx, reply).await.is_err() { break; }
                            }
                            FromHub::Other => {}
                        },
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => {} // pings are answered by the library
                    }
                }
                changed = state.changed() => {
                    if changed.is_err() { break; }
                    let s = *state.borrow_and_update();
                    if send(&mut tx, frames::state(s.helm_running, s.crash_loop)).await.is_err() { break; }
                }
                _ = ping.tick() => {
                    if tx.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
                }
                _ = &mut silence => {
                    log::info!("launcher: the hub went quiet; reconnecting");
                    break;
                }
            }
        }
        let _ = tx.close().await;
    }

    fn welcomed(&self, wake: Option<&Wake>) {
        if wake.is_some() {
            *self.wake_told.lock().unwrap_or_else(|p| p.into_inner()) = true;
        }
        log::info!(
            "launcher: the hub welcomed this PC (wake details {})",
            if wake.is_some() { "sent" } else { "not known" }
        );
        self.write_status("connected");
    }
}

/// The hub's reason, for the log: one of the known words, never the hub's raw text.
fn reason_text(reason: &str) -> &'static str {
    match reason {
        "telegram" => "a Telegram message",
        "voice" => "a voice turn",
        "button" => "a button",
        _ => "another reason",
    }
}

async fn send(
    tx: &mut futures_util::stream::SplitSink<Ws, Message>,
    v: serde_json::Value,
) -> Result<(), ()> {
    tx.send(Message::Text(v.to_string().into()))
        .await
        .map_err(|_| ())
}

async fn connect(url: &str, token: &str) -> Result<Ws, ConnectError> {
    let mut req = url
        .into_client_request()
        .map_err(|_| ConnectError::Other("the hub address is not valid".into()))?;
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| ConnectError::Other("the relay token is not valid".into()))?,
    );
    let mut cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    cfg.max_message_size = Some(MAX_FRAME);
    cfg.max_frame_size = Some(MAX_FRAME);
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(req, Some(cfg), false),
    )
    .await
    {
        Ok(Ok((ws, _))) => Ok(ws),
        // Never the error's text: it can carry the request.
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(r))) if r.status().as_u16() == 404 => {
            Err(ConnectError::Missing)
        }
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(r))) => Err(ConnectError::Other(
            format!("the hub refused the line (HTTP {})", r.status().as_u16()),
        )),
        Ok(Err(_)) => Err(ConnectError::Other("the hub could not be reached".into())),
        Err(_) => Err(ConnectError::Other("the hub did not answer in time".into())),
    }
}

/// A one-line-per-entry logger into the profile's rolling `launcher.log`.
struct FileLog {
    file: Mutex<helm_core::hostlog::RollingFile>,
}

impl log::Log for FileLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        use std::io::Write;
        if !self.enabled(r.metadata()) {
            return;
        }
        let line = format!(
            "{} {} {}\n",
            helm_core::trail::clock_text(helm_core::now_ms()),
            r.level(),
            r.args()
        );
        let _ = self
            .file
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .write_all(line.as_bytes());
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
    }
    fn flush(&self) {}
}

fn init_log(profile_dir: &Path) {
    let path = profile_dir.join("diagnostics").join("launcher.log");
    let logger = FileLog {
        file: Mutex::new(helm_core::hostlog::RollingFile::new(path)),
    };
    if log::set_logger(Box::leak(Box::new(logger))).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// The profile named by `--profile <dir>`, else the one Helm itself would use.
fn profile_arg(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .position(|a| a == "--profile")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
}

/// The launcher's `main`.
pub fn main() -> ExitCode {
    if !cfg!(windows) {
        eprintln!("helm-launcher runs on Windows only");
        return ExitCode::SUCCESS;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = match profile_arg(&args)
        .map(Ok)
        .unwrap_or_else(helm_core::profile::resolve_profile_dir)
    {
        Ok(d) => d,
        Err(_) => return ExitCode::FAILURE,
    };
    // A profile that is gone is not recreated here: there is nothing to launch.
    if !dir.is_dir() {
        return ExitCode::SUCCESS;
    }
    let Ok(profile) = Profile::open(&dir) else {
        return ExitCode::FAILURE;
    };
    init_log(&profile.dir);
    // One launcher per profile; a second start leaves quietly.
    let Some(_instance) = sys::take_instance(&sys::instance_name(&profile.hash)) else {
        return ExitCode::SUCCESS;
    };
    match install::read_handoff(&profile.dir) {
        Some(h) if h.enabled => {}
        _ => return ExitCode::SUCCESS,
    }
    let Some(stop) = sys::stop_event(&sys::stop_name(&profile.hash)) else {
        log::error!("launcher: its stop signal could not be created; exiting");
        return ExitCode::FAILURE;
    };
    if let Err(e) = install::write_pid(&profile.dir, std::process::id()) {
        log::warn!("launcher: its process id was not noted: {e}");
    }
    let pid_dir = profile.dir.clone();
    std::thread::spawn(move || {
        stop.wait();
        log::info!("launcher: asked to stop by Helm");
        install::remove_pid(&pid_dir);
        std::process::exit(0);
    });
    log::info!("launcher: started, version {}", env!("CARGO_PKG_VERSION"));
    let daemon = Daemon::new(
        &profile,
        Arc::new(helm_platform::keychain::NativeKeychain::new()),
    );
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log::error!("launcher: no runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(async {
        tokio::join!(daemon.clone().watch_helm(), daemon.hub_line());
    });
    install::remove_pid(&profile.dir);
    ExitCode::SUCCESS
}
