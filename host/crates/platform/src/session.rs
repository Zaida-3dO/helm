//! The machine around Helm: sign-out, shutdown, sleep and wake.
//!
//! Windows tells a program about these through messages to a top-level window, so this module owns
//! one hidden window on a thread of its own (nothing else in the process is involved).
//!
//! - **Sleep**: the suspend message gives a program about two seconds before the machine goes
//!   down, and the system does not wait longer. [`SystemWatcher::suspending`] therefore runs
//!   synchronously on the window's thread and must only do quick work (record state, one small
//!   write). Wake arrives as one or two messages; callers must tolerate the repeat.
//! - **Sign-out and shutdown**: the first message asks whether the session may end; Helm always
//!   says yes, and asks the system to wait while it saves. The second message says the session is
//!   ending for certain; [`SystemWatcher::session_ending`] runs the normal quit flow there and
//!   returns when it has finished or the budget has passed. If another program vetoes the
//!   shutdown, the second message says so and nothing was started.
//!
//! Off Windows nothing starts.

use std::sync::Arc;
use std::time::Duration;

/// How long [`SystemWatcher::session_ending`] may take before the system stops waiting. A
/// sign-out gives a program a few seconds, so this stays under five.
pub const SESSION_END_BUDGET: Duration = Duration::from_millis(4500);

/// What the app wants to know about the machine. Every call comes from the watcher's own thread.
pub trait SystemWatcher: Send + Sync + 'static {
    /// The machine is about to sleep. Return quickly: the system waits about two seconds.
    fn suspending(&self);
    /// The machine woke. May be called twice for one wake.
    fn resumed(&self);
    /// The user is signing out or the computer is shutting down, and it will happen. Run the quit
    /// flow without asking, and return once it has finished or `budget` has passed.
    fn session_ending(&self, budget: Duration);
}

/// Start watching. Safe to call once; later calls do nothing.
pub fn watch(listener: Arc<dyn SystemWatcher>) {
    imp::start(listener)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Power::RegisterSuspendResumeNotification;
    use windows_sys::Win32::System::Shutdown::{
        ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
        TranslateMessage, DEVICE_NOTIFY_WINDOW_HANDLE, MSG, PBT_APMRESUMEAUTOMATIC,
        PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION,
        WNDCLASSW, WS_OVERLAPPED,
    };

    static WATCHER: OnceLock<Arc<dyn SystemWatcher>> = OnceLock::new();

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if let Some(w) = WATCHER.get() {
            match msg {
                WM_POWERBROADCAST => {
                    match wp as u32 {
                        PBT_APMSUSPEND => w.suspending(),
                        PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => w.resumed(),
                        _ => {}
                    }
                    // TRUE: the request is granted.
                    return 1;
                }
                WM_QUERYENDSESSION => {
                    // Yes, the session may end; and please wait while Helm saves.
                    let reason = wide("Helm is saving your chats");
                    ShutdownBlockReasonCreate(hwnd, reason.as_ptr());
                    return 1;
                }
                WM_ENDSESSION => {
                    if wp != 0 {
                        w.session_ending(SESSION_END_BUDGET);
                    }
                    ShutdownBlockReasonDestroy(hwnd);
                    return 0;
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    pub fn start(listener: Arc<dyn SystemWatcher>) {
        if WATCHER.set(listener).is_err() {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("helm-system-events".into())
            .spawn(|| unsafe { run() });
        if let Err(e) = spawned {
            log::warn!("system events not watched: {e}");
        }
    }

    unsafe fn run() {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("HelmSystemEvents");
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        if RegisterClassW(&wc) == 0 {
            return log::warn!("system events: window class not registered");
        }
        // A top-level window that is never shown: sign-out and power messages reach top-level
        // windows only.
        let title = wide("Helm system events");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return log::warn!("system events: hidden window not created");
        }
        // Machines on Modern Standby report suspend and resume only to a registered window.
        if RegisterSuspendResumeNotification(hwnd, DEVICE_NOTIFY_WINDOW_HANDLE) == 0 {
            log::info!("system events: suspend/resume registration unavailable");
        }
        log::info!("system events listening (sleep, wake, sign-out, shutdown)");
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn start(_listener: Arc<dyn SystemWatcher>) {}
}
