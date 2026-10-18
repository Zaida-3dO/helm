//! Windows auto-AFK signals.
//!
//! - **Session lock / unlock**: WTS session-change notifications. They need a window and a message
//!   loop, so this module owns one hidden top-level window on its own thread; nothing in the tray
//!   or the platform crate is involved.
//! - **Sleep / resume**: `WM_POWERBROADCAST` on the same window (registered for suspend and resume
//!   notifications too, so Modern Standby machines report them). The sleep signal is applied
//!   synchronously, before the handler returns to the OS.
//! - **Idle**: seconds since the last input in this session (`GetLastInputInfo`), read on ticks.
//!
//! Off Windows nothing starts and idle reads as unknown.

use crate::monitor::Signal;
use crate::service::AfkService;
use std::sync::Arc;

/// Seconds since the last keyboard or pointer input in this session, if readable.
pub fn idle_secs() -> Option<u64> {
    imp::idle_secs()
}

/// Start the signal thread. Safe to call once; later calls do nothing.
pub fn start(svc: Arc<AfkService>) {
    imp::start(svc)
}

/// After a lock, post the grace signal once the grace has passed.
fn after_lock(svc: &Arc<AfkService>, at: i64) {
    let grace = svc.settings().auto.lock_grace_seconds;
    if grace == 0 {
        return;
    }
    let svc = svc.clone();
    let _ = std::thread::Builder::new()
        .name("helm-afk-grace".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(grace as u64));
            let now = helm_core::now_ms();
            svc.signal(Signal::LockGrace { at: now.max(at) }, now);
        });
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Power::RegisterSuspendResumeNotification;
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
        TranslateMessage, DEVICE_NOTIFY_WINDOW_HANDLE, MSG, PBT_APMRESUMEAUTOMATIC,
        PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDCLASSW,
        WS_OVERLAPPED, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    static SVC: OnceLock<Arc<AfkService>> = OnceLock::new();

    pub fn idle_secs() -> Option<u64> {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a valid, correctly sized LASTINPUTINFO.
        if unsafe { GetLastInputInfo(&mut info) } == 0 {
            return None;
        }
        // Both are 32-bit millisecond tick counts; wrapping subtraction survives the 49-day wrap.
        let now = unsafe { GetTickCount() };
        Some(now.wrapping_sub(info.dwTime) as u64 / 1000)
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if let Some(svc) = SVC.get() {
            let now = helm_core::now_ms();
            match msg {
                WM_WTSSESSION_CHANGE => match wp as u32 {
                    WTS_SESSION_LOCK => {
                        svc.signal(Signal::Locked { at: now }, now);
                        after_lock(svc, now);
                    }
                    WTS_SESSION_UNLOCK => svc.signal(Signal::Unlocked, now),
                    _ => {}
                },
                WM_POWERBROADCAST => {
                    match wp as u32 {
                        PBT_APMSUSPEND => svc.signal(Signal::WillSleep { sent_at: now }, now),
                        PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                            svc.signal(Signal::DidWake { at: now }, now)
                        }
                        _ => {}
                    }
                    return 1;
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    pub fn start(svc: Arc<AfkService>) {
        if SVC.set(svc).is_err() {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("helm-afk-signals".into())
            .spawn(|| unsafe { run() });
        if let Err(e) = spawned {
            log::warn!("auto-AFK signals not started: {e}");
        }
    }

    unsafe fn run() {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("HelmAfkSignals");
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
            return log::warn!("auto-AFK signals: window class not registered");
        }
        // A top-level window that is never shown: power broadcasts reach top-level windows only.
        let title = wide("Helm AFK signals");
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
            return log::warn!("auto-AFK signals: hidden window not created");
        }
        if WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) == 0 {
            log::warn!("auto-AFK signals: session-lock notifications unavailable");
        }
        if RegisterSuspendResumeNotification(hwnd, DEVICE_NOTIFY_WINDOW_HANDLE) == 0 {
            log::info!("auto-AFK signals: suspend/resume registration unavailable");
        }
        log::info!("auto-AFK signals listening (lock, sleep, resume)");
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

    pub fn idle_secs() -> Option<u64> {
        None
    }

    pub fn start(_svc: Arc<AfkService>) {
        let _ = after_lock;
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn idle_seconds_are_readable_on_windows() {
        // A day of idleness would mean the clock maths is wrong, not that the machine is idle.
        let s = super::idle_secs().expect("GetLastInputInfo answers in a desktop session");
        assert!(s < 24 * 3600, "{s}");
    }
}
