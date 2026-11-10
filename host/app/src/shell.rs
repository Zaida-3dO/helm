//! Tauri glue: the main window, the event sink and the app-control seam.

use helm_core::lifecycle::AppControl;
use helm_core::profile::Profile;
use helm_ipc::{
    authority::main_window_may_navigate, AppEvent, EventSink, EVENT_CHANNEL, MAIN_WINDOW,
};
use helm_shell::uiwatch::{self, CrashGuard, Failure, Recovery};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// The web view's command line. Setting it replaces the ones the web view adds by default, so they
/// are listed in full: the defaults (no mini menu, no SmartScreen) plus audio that may start
/// without a click, so the first chime after launch is heard. The last switch keeps WebRTC from
/// using the machine's own network addresses: with it, a page cannot open a direct UDP path to
/// anywhere (Helm uses WebRTC nowhere), which closes the one network route a page's content
/// security policy does not cover.
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required --force-webrtc-ip-handling-policy=disable_non_proxied_udp";

/// Emit one event on the tagged channel, to the main window only.
pub fn emit(app: &AppHandle, event: AppEvent) {
    if let Err(e) = app.emit_to(MAIN_WINDOW, EVENT_CHANNEL, event.to_value()) {
        log::warn!("event {} not delivered: {e}", event.kind);
    }
}

pub struct TauriEvents {
    app: AppHandle,
}

impl TauriEvents {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriEvents {
    fn emit(&self, event: AppEvent) {
        emit(&self.app, event);
    }
}

pub struct TauriShell {
    app: AppHandle,
    shown: std::sync::atomic::AtomicBool,
}

impl TauriShell {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            shown: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl TauriShell {
    /// Show the main window only if it has never been shown (the start-up fallback), so a
    /// window the user already closed to the tray stays hidden.
    pub fn show_if_hidden(&self) {
        if self.shown.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.show_main();
    }
}

impl AppControl for TauriShell {
    fn show_main(&self) {
        self.shown.store(true, std::sync::atomic::Ordering::SeqCst);
        // A background test window is already on screen (off-screen): showing it again would
        // activate it, take focus and un-minimise it, which is what that mode exists to avoid.
        if test_background(cfg!(debug_assertions)) {
            return;
        }
        if let Some(w) = self.app.get_webview_window(MAIN_WINDOW) {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }

    fn hide_main(&self) {
        if let Some(w) = self.app.get_webview_window(MAIN_WINDOW) {
            let _ = w.hide();
        }
    }

    fn exit(&self, code: i32) {
        self.app.exit(code);
    }
}

/// Debug builds honour `HELM_TEST_WINDOW=background`: the main window opens off-screen, at a fixed
/// size, already shown but never focused, and later "show" requests leave it alone. Automated
/// checks use it so a run never takes over the screen or steals focus. Release builds ignore it.
pub fn test_background(debug: bool) -> bool {
    background_window(debug, std::env::var("HELM_TEST_WINDOW").ok().as_deref())
}

fn background_window(debug: bool, value: Option<&str>) -> bool {
    debug && value == Some("background")
}

/// Show a window without activating it (Windows' `SW_SHOWNOACTIVATE`), so it never takes focus.
#[cfg(windows)]
fn show_without_activating(window: &tauri::WebviewWindow) {
    #[link(name = "user32")]
    extern "system" {
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    }
    const SW_SHOWNOACTIVATE: i32 = 4;
    if let Ok(hwnd) = window.hwnd() {
        // SAFETY: a plain Win32 call on the window handle Tauri just gave us.
        unsafe {
            ShowWindow(hwnd.0 as isize, SW_SHOWNOACTIVATE);
        }
    }
}

#[cfg(not(windows))]
fn show_without_activating(window: &tauri::WebviewWindow) {
    let _ = window.show();
}

/// What it takes to repair the main window when WebView2 says one of its processes failed: the
/// loop guard, where to write the diagnostic line, a way to ask the user, and what is needed to
/// build the window again.
pub struct WindowCrashes {
    guard: Mutex<CrashGuard>,
    diagnostics: std::path::PathBuf,
    dialogs: Arc<dyn helm_platform::Dialogs>,
    profile: Profile,
    debug: bool,
}

impl WindowCrashes {
    pub fn new(
        profile: &Profile,
        debug: bool,
        dialogs: Arc<dyn helm_platform::Dialogs>,
    ) -> Arc<Self> {
        Arc::new(Self {
            guard: Mutex::new(CrashGuard::new()),
            diagnostics: profile.path("diagnostics"),
            dialogs,
            profile: profile.clone(),
            debug,
        })
    }

    /// WebView2 reported a failed process (`kind` is its event's number). Chats live in the host,
    /// not the page, so repairing the page loses none of them; drafts are saved as they are typed.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn on_failed(self: &Arc<Self>, app: &AppHandle, kind: i32) {
        let failure = Failure::from_kind(kind);
        let recovery = self
            .guard
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .on_failure(Instant::now(), failure);
        log::error!("the web view reported {failure:?}; {recovery:?}");
        if let Err(e) =
            uiwatch::write_crash(&self.diagnostics, helm_core::now_ms(), failure, recovery)
        {
            log::warn!("ui diagnostic not written: {e}");
        }
        if recovery == Recovery::Ignore {
            return;
        }
        // Off the web view's own event: asking and rebuilding both wait on its thread.
        let me = self.clone();
        let app = app.clone();
        std::thread::spawn(move || {
            let mut recovery = recovery;
            if recovery == Recovery::Offer {
                let yes = me.dialogs.confirm(
                    "Helm",
                    "Helm's window has stopped working several times in a row. Your chats are safe. Reload the window?",
                );
                if !yes {
                    return;
                }
                me.guard
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .offered_and_accepted(Instant::now());
                recovery = if failure == Failure::BrowserExited {
                    Recovery::Rebuild
                } else {
                    Recovery::Reload
                };
            }
            me.repair(&app, recovery);
        });
    }

    fn repair(self: &Arc<Self>, app: &AppHandle, recovery: Recovery) {
        match recovery {
            Recovery::Reload => {
                if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
                    if let Err(e) = w.reload() {
                        log::error!("reloading the main window failed: {e}");
                    }
                }
            }
            Recovery::Rebuild => {
                if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
                    let _ = w.destroy();
                }
                // The old window's label is free a moment after it is destroyed.
                for _ in 0..5 {
                    match build_main_window(app, self) {
                        Ok(()) => return,
                        Err(e) => {
                            log::warn!("building the main window again failed: {e}");
                            std::thread::sleep(Duration::from_millis(300));
                        }
                    }
                }
                log::error!("the main window could not be built again");
            }
            Recovery::Ignore | Recovery::Offer => {}
        }
    }
}

/// Listen for the web view's process-failed event on the main window.
#[cfg(windows)]
fn watch_process_failures(window: &tauri::WebviewWindow, crashes: Arc<WindowCrashes>) {
    use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PROCESS_FAILED_KIND;
    use webview2_com::ProcessFailedEventHandler;
    let app = window.app_handle().clone();
    let attached = window.with_webview(move |platform| {
        // SAFETY: COM calls on the controller Tauri created, made on the thread that owns it.
        unsafe {
            let core = match platform.controller().CoreWebView2() {
                Ok(core) => core,
                Err(e) => return log::warn!("web view crash handler not attached: {e}"),
            };
            let handler = ProcessFailedEventHandler::create(Box::new(move |_sender, args| {
                if let Some(args) = args {
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND(0);
                    if args.ProcessFailedKind(&mut kind).is_ok() {
                        crashes.on_failed(&app, kind.0);
                    }
                }
                Ok(())
            }));
            let mut token = 0i64;
            if let Err(e) = core.add_ProcessFailed(&handler, &mut token) {
                log::warn!("web view crash handler not attached: {e}");
            }
        }
    });
    if let Err(e) = attached {
        log::warn!("web view crash handler not attached: {e}");
    }
}

#[cfg(not(windows))]
fn watch_process_failures(_window: &tauri::WebviewWindow, _crashes: Arc<WindowCrashes>) {}

/// Answer the web view's permission requests itself. The microphone (dictation) is allowed for
/// Helm's own page only, never for a page in a frame. The clipboard and autoplay requests are left
/// to the web view's other handler. Every other kind (camera, location, sensors, notifications,
/// fonts, files and the rest) is denied: Helm uses none of them.
#[cfg(windows)]
fn guard_permissions(window: &tauri::WebviewWindow, debug: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_AUTOPLAY,
        COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW, COREWEBVIEW2_PERMISSION_STATE_DENY,
    };
    use webview2_com::{take_pwstr, PermissionRequestedEventHandler};
    let attached = window.with_webview(move |platform| {
        // SAFETY: COM calls on the controller Tauri created, made on the thread that owns it.
        unsafe {
            let core = match platform.controller().CoreWebView2() {
                Ok(core) => core,
                Err(e) => return log::warn!("permission handler not attached: {e}"),
            };
            let handler =
                PermissionRequestedEventHandler::create(Box::new(move |_sender, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    args.PermissionKind(&mut kind)?;
                    let state = match kind {
                        COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ
                        | COREWEBVIEW2_PERMISSION_KIND_AUTOPLAY => return Ok(()),
                        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE => {
                            let mut uri = Default::default();
                            args.Uri(&mut uri)?;
                            if helm_ipc::authority::is_trusted_origin(&take_pwstr(uri), debug) {
                                COREWEBVIEW2_PERMISSION_STATE_ALLOW
                            } else {
                                COREWEBVIEW2_PERMISSION_STATE_DENY
                            }
                        }
                        _ => COREWEBVIEW2_PERMISSION_STATE_DENY,
                    };
                    args.SetState(state)?;
                    Ok(())
                }));
            let mut token = 0i64;
            if let Err(e) = core.add_PermissionRequested(&handler, &mut token) {
                log::warn!("permission handler not attached: {e}");
            }
        }
    });
    if let Err(e) = attached {
        log::warn!("permission handler not attached: {e}");
    }
}

#[cfg(not(windows))]
fn guard_permissions(_window: &tauri::WebviewWindow, _debug: bool) {}

/// The main window is built in code, not from static config, so its navigation policy can be
/// attached: only the app origin, `about:srcdoc` and (debug) the dev server; no new
/// windows. Its WebView2 data lives inside the profile, so profiles never share web storage.
/// Also called to build it again after the web view's browser process has died.
pub fn build_main_window<M: Manager<tauri::Wry>>(
    app: &M,
    crashes: &Arc<WindowCrashes>,
) -> tauri::Result<()> {
    let (profile, debug) = (&crashes.profile, crashes.debug);
    let data_dir = profile.path("webview");
    helm_core::privfs::create_private_dir_all(&data_dir)?;
    let builder = WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::App("index.html".into()))
        .title("Helm");
    let builder = if test_background(debug) {
        // Off-screen and never focused, at the size the checks expect. It is shown right after
        // creation (a hidden window may stop painting, and the checks take screenshots).
        builder
            .inner_size(1440.0, 900.0)
            .maximized(false)
            .visible(false)
            .focused(false)
    } else {
        // Restored size 1220 x 820, opens maximised, created hidden and shown by the UI's
        // `window_ready` once the first frame is drawn.
        builder
            .inner_size(1220.0, 820.0)
            .maximized(true)
            .visible(false)
    };
    let window = builder
        // Minimum 760 x 540. Helm draws its own caption, so the native frame is off.
        .min_inner_size(760.0, 540.0)
        .decorations(false)
        .data_directory(data_dir)
        .additional_browser_args(BROWSER_ARGS)
        .on_navigation(move |url| {
            let ok = main_window_may_navigate(url.as_str(), debug);
            if !ok {
                log::warn!("main window navigation refused");
            }
            ok
        })
        .on_new_window(|_url, _features| tauri::webview::NewWindowResponse::Deny)
        .build()?;
    if test_background(debug) {
        // Moved off-screen first, then shown without activating it (the builder's own position is
        // not always honoured, and a plain show would take focus).
        let _ = window.set_position(tauri::PhysicalPosition::new(-32000, -32000));
        show_without_activating(&window);
    }
    watch_process_failures(&window, crashes.clone());
    guard_permissions(&window, debug);
    // Files dragged in from Explorer reach the composer as paths.
    let handle = app.app_handle().clone();
    window.on_window_event(move |e| {
        if let tauri::WindowEvent::DragDrop(d) = e {
            emit(&handle, file_drop_event(d));
        }
    });
    Ok(())
}

/// A native file drag as a `file_drop` event: `phase` (enter, over, drop, leave), the paths (at most
/// 20, on enter and drop) and the pointer in physical pixels (`x`, `y`); the UI divides by its own
/// device pixel ratio, which covers zoom too.
pub fn file_drop_event(d: &tauri::DragDropEvent) -> AppEvent {
    let paths = |p: &[std::path::PathBuf]| -> Vec<String> {
        p.iter()
            .take(20)
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    };
    let at = |e: AppEvent, pos: &tauri::PhysicalPosition<f64>| e.with("x", pos.x).with("y", pos.y);
    match d {
        tauri::DragDropEvent::Enter { paths: p, position } => at(
            AppEvent::new("file_drop")
                .with("phase", "enter")
                .with("paths", paths(p)),
            position,
        ),
        tauri::DragDropEvent::Over { position } => {
            at(AppEvent::new("file_drop").with("phase", "over"), position)
        }
        tauri::DragDropEvent::Drop { paths: p, position } => at(
            AppEvent::new("file_drop")
                .with("phase", "drop")
                .with("paths", paths(p)),
            position,
        ),
        _ => AppEvent::new("file_drop").with("phase", "leave"),
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;

    #[test]
    fn the_background_test_window_is_a_debug_build_switch() {
        for (debug, value, want) in [
            (true, Some("background"), true),
            (true, Some("shown"), false),
            (true, None, false),
            (false, Some("background"), false),
        ] {
            assert_eq!(background_window(debug, value), want, "{debug} {value:?}");
        }
    }

    #[test]
    fn the_web_view_keeps_its_defaults_and_closes_webrtc() {
        for part in [
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection",
            "--autoplay-policy=no-user-gesture-required",
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        ] {
            assert!(BROWSER_ARGS.contains(part), "{part}");
        }
    }

    #[test]
    fn a_drop_carries_its_paths_and_position() {
        let e = file_drop_event(&tauri::DragDropEvent::Drop {
            paths: vec![std::path::PathBuf::from(r"C:\work\notes.md")],
            position: tauri::PhysicalPosition::new(120.0, 640.5),
        });
        let v = e.to_value();
        assert_eq!(v["kind"], "file_drop");
        assert_eq!(v["phase"], "drop");
        assert_eq!(v["paths"], serde_json::json!([r"C:\work\notes.md"]));
        assert_eq!(
            (v["x"].as_f64(), v["y"].as_f64()),
            (Some(120.0), Some(640.5))
        );
        let many: Vec<_> = (0..30)
            .map(|i| std::path::PathBuf::from(format!("C:/f{i}")))
            .collect();
        let v = file_drop_event(&tauri::DragDropEvent::Enter {
            paths: many,
            position: tauri::PhysicalPosition::new(0.0, 0.0),
        })
        .to_value();
        assert_eq!(v["paths"].as_array().unwrap().len(), 20);
        assert_eq!(
            file_drop_event(&tauri::DragDropEvent::Leave).to_value()["phase"],
            "leave"
        );
    }
}
