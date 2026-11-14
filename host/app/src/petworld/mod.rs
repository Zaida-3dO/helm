//! A pet that roams the whole screen: a prototype that runs only in debug builds on Windows, and
//! only when `HELM_PET_PHYSICS=1` is set. It lives beside the ordinary desktop pet and shares
//! nothing with it but the creature drawings.
//!
//! The window is the sprite (96 logical px square) and the host moves it. A loop thread runs the
//! physics in `physics`, reads the cursor and the monitors itself and moves the window straight
//! through Win32, so a frame never waits on the main thread. The window can never be activated,
//! so pressing or dragging the pet leaves the keyboard with whatever app had it. The page draws
//! the creature in its mood's pose, tells the host the mood and when the sprite is grabbed or let
//! go, and hears the body's state when it changes. The loop ticks at 60 Hz while anything moves,
//! 4 Hz at rest and once a second while the pet is hidden for a fullscreen app.

pub mod physics;

use helm_ipc::{
    ApiError, AppEvent, Dispatcher, EventSink, OpClass, OpCtx, OpRegistry, EVENT_CHANNEL,
};
use physics::{Mood, SPRITE};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use tauri::{AppHandle, Emitter, Manager};

pub const LABEL: &str = "petworld";

/// The events the page draws from, as for the ordinary pet.
const FORWARDED: &[&str] = &["attention", "completion", "chat", "preferences"];

/// Whether the switch asks for the roaming pet. Release builds never open it.
fn wanted(debug: bool, value: Option<&str>) -> bool {
    debug && value == Some("1")
}

/// Whether the roaming pet runs in this process.
pub fn enabled() -> bool {
    cfg!(windows)
        && wanted(
            cfg!(debug_assertions),
            std::env::var("HELM_PET_PHYSICS").ok().as_deref(),
        )
}

/// What the page has said since the loop last looked.
struct Control {
    ready: bool,
    /// Grabbed this many logical px from the sprite's corner.
    grab: Option<(f64, f64)>,
    release: bool,
    mood: Option<(Mood, bool)>,
}

static CONTROL: Mutex<Control> = Mutex::new(Control {
    ready: false,
    grab: None,
    release: false,
    mood: None,
});
/// Wakes the loop when the page says something, so a grab at rest is not left waiting a tick.
static WAKE: Condvar = Condvar::new();

fn control(change: impl FnOnce(&mut Control)) {
    change(&mut CONTROL.lock().unwrap_or_else(|p| p.into_inner()));
    WAKE.notify_all();
}

/// Make the window and start its loop, when the switch is set. Called from setup.
pub fn start(app: &AppHandle, data_dir: PathBuf, debug: bool) {
    if !enabled() {
        return;
    }
    #[cfg(windows)]
    {
        let app = app.clone();
        // Making a window waits on the event loop, which setup is still inside.
        std::thread::spawn(move || win::run(app, data_dir, debug));
    }
    #[cfg(not(windows))]
    let _ = (app, data_dir, debug);
}

/// Every event on its way out also reaches the roaming pet's page, when the switch is set.
pub fn wrap(inner: Arc<dyn EventSink>, app: AppHandle) -> Arc<dyn EventSink> {
    if !enabled() {
        return inner;
    }
    Arc::new(Forward { inner, app })
}

struct Forward {
    inner: Arc<dyn EventSink>,
    app: AppHandle,
}

impl EventSink for Forward {
    fn emit(&self, event: AppEvent) {
        if FORWARDED.contains(&event.kind.as_str()) && self.app.get_webview_window(LABEL).is_some()
        {
            let _ = self.app.emit_to(LABEL, EVENT_CHANNEL, event.to_value());
        }
        self.inner.emit(event);
    }
}

// --- ops the page may call ------------------------------------------------------------------------

/// The reads the page draws from, the ordinary pet's open (a click opens the chat that needs
/// the user), and its own four.
pub fn allow_ops(dispatcher: &mut Dispatcher) {
    dispatcher.allow_window_ops(
        LABEL,
        &[
            "attention_get",
            "preference_get",
            "pet_open",
            "petworld_ready",
            "petworld_grab",
            "petworld_release",
            "petworld_mood",
        ],
    );
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("petworld_ready", OpClass::Blocking, ready)
        .blocking("petworld_grab", OpClass::Blocking, grab)
        .blocking("petworld_release", OpClass::Blocking, release)
        .blocking("petworld_mood", OpClass::Blocking, mood);
}

/// The page drew its first frame: the loop shows the window and lets the pet drop.
fn ready(_ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    control(|c| c.ready = true);
    Ok(Value::Null)
}

/// `{x, y}`: where on the sprite the press began (CSS px from its corner).
fn grab(_ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let coord = |name: &str| {
        args.get(name)
            .and_then(Value::as_f64)
            .filter(|n| (0.0..=SPRITE * 2.0).contains(n))
            .ok_or_else(|| ApiError::bad_args(format!("{name} must be a point on the pet")))
    };
    let at = (coord("x")?, coord("y")?);
    control(|c| {
        c.grab = Some(at);
        c.release = false;
    });
    Ok(Value::Null)
}

fn release(_ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    control(|c| c.release = true);
    Ok(Value::Null)
}

/// `{pose, still}`: the mood pose, and whether motion is off (Quiet or reduced motion).
fn mood(_ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let pose = args
        .get("pose")
        .and_then(Value::as_str)
        .and_then(Mood::parse)
        .ok_or_else(|| ApiError::bad_args("pose must be one of the pet's poses"))?;
    let still = args.get("still").and_then(Value::as_bool).unwrap_or(false);
    control(|c| c.mood = Some((pose, still)));
    Ok(Value::Null)
}

// --- the window and its loop (Windows) ------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::physics::{Body, Desktop, Monitor, Pet, Rect, State, SPRITE};
    use super::{Control, CONTROL, EVENT_CHANNEL, LABEL, WAKE};
    use crate::shell::{
        guard_permissions, show_without_activating, watch_process_failures, BROWSER_ARGS,
    };
    use helm_ipc::authority::main_window_may_navigate;
    use helm_ipc::AppEvent;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

    #[repr(C)]
    #[derive(Default)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Default, PartialEq, Eq)]
    struct WinRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct MonitorInfo {
        size: u32,
        monitor: WinRect,
        work: WinRect,
        flags: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn SetWindowPos(
            hwnd: isize,
            after: isize,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            flags: u32,
        ) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
        fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
        fn GetCursorPos(point: *mut Point) -> i32;
        fn GetAsyncKeyState(key: i32) -> i16;
        fn GetSystemMetrics(index: i32) -> i32;
        fn GetForegroundWindow() -> isize;
        fn GetWindowRect(hwnd: isize, rect: *mut WinRect) -> i32;
        fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
        fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
        fn GetClassNameW(hwnd: isize, name: *mut u16, max: i32) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHQueryUserNotificationState(state: *mut i32) -> i32;
    }

    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_ASYNCWINDOWPOS: u32 = 0x4000;
    const HWND_TOPMOST: isize = -1;
    const SW_HIDE: i32 = 0;
    const SW_SHOWNOACTIVATE: i32 = 4;
    const GWL_EXSTYLE: i32 = -20;
    const WS_EX_TOOLWINDOW: isize = 0x0080;
    const WS_EX_NOACTIVATE: isize = 0x0800_0000;
    const SM_SWAPBUTTON: i32 = 23;
    const VK_LBUTTON: i32 = 0x01;
    const VK_RBUTTON: i32 = 0x02;
    const MONITOR_DEFAULTTONULL: u32 = 0;
    /// `SHQueryUserNotificationState`: busy (a fullscreen app), Direct3D fullscreen, presenting.
    const QUIET_STATES: [i32; 3] = [2, 3, 4];

    const FAST: Duration = Duration::from_micros(16_667);
    const REST: Duration = Duration::from_millis(250);
    const HIDDEN: Duration = Duration::from_secs(1);
    const RESCAN: Duration = Duration::from_secs(1);
    const TOPMOST_EVERY: Duration = Duration::from_secs(5);

    fn build(app: &AppHandle, data_dir: PathBuf, debug: bool) -> tauri::Result<WebviewWindow> {
        // The window's capability is granted only here, so a build without the switch never has
        // it.
        app.add_capability(include_str!("../../capabilities/petworld.json"))?;
        // Hidden until the page has drawn its first frame. Not focusable: Windows never activates
        // it, so a press on the pet leaves the keyboard where it was.
        let window = WebviewWindowBuilder::new(
            app,
            LABEL,
            WebviewUrl::App("index.html?window=petworld".into()),
        )
        .title("Helm pet")
        .inner_size(SPRITE, SPRITE)
        // Windows holds a window to a minimum width wider than the sprite unless told otherwise.
        .min_inner_size(SPRITE, SPRITE)
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .resizable(false)
        .shadow(false)
        .visible(false)
        // The same data folder and browser switches as the other windows: one web view
        // environment.
        .data_directory(data_dir)
        .additional_browser_args(BROWSER_ARGS)
        .on_navigation(move |url| main_window_may_navigate(url.as_str(), debug))
        .on_new_window(|_url, _features| tauri::webview::NewWindowResponse::Deny)
        .build()?;
        guard_permissions(&window, debug, false);
        watch_process_failures(&window, |app, kind| {
            log::warn!("the roaming pet's page failed ({kind}); it stays closed until restart");
            let app = app.clone();
            // Off the web view's own event: closing waits on its thread.
            std::thread::spawn(move || {
                if let Some(w) = app.get_webview_window(LABEL) {
                    let _ = w.destroy();
                }
            });
        });
        Ok(window)
    }

    /// Keep the window a no-activate tool window (out of Alt+Tab and Task View) and on top.
    fn assert_style(hwnd: isize) {
        // SAFETY: plain Win32 calls on the window handle Tauri gave us.
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let want = ex | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            if want != ex {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
            }
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
            );
        }
    }

    /// Move without waiting on the window's thread and without activating it.
    fn move_to(hwnd: isize, (x, y): (i32, i32)) {
        // SAFETY: as above.
        unsafe {
            SetWindowPos(
                hwnd,
                0,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
            );
        }
    }

    fn cursor() -> (f64, f64) {
        let mut p = Point::default();
        // SAFETY: as above; `p` outlives the call.
        unsafe { GetCursorPos(&mut p) };
        (f64::from(p.x), f64::from(p.y))
    }

    /// Whether the primary button is down (the right one when the user swapped them): the net for
    /// a release the page never reported.
    fn button_down() -> bool {
        // SAFETY: plain Win32 reads.
        unsafe {
            let key = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
                VK_RBUTTON
            } else {
                VK_LBUTTON
            };
            GetAsyncKeyState(key) as u16 & 0x8000 != 0
        }
    }

    /// A fullscreen app is in front (a game, a video, a presentation): the pet keeps out of it.
    fn fullscreen_in_front() -> bool {
        let mut state = 0i32;
        // SAFETY: plain Win32 reads into locals that outlive the calls.
        unsafe {
            if SHQueryUserNotificationState(&mut state) == 0 && QUIET_STATES.contains(&state) {
                return true;
            }
            let front = GetForegroundWindow();
            if front == 0 {
                return false;
            }
            let mut class = [0u16; 32];
            let n = GetClassNameW(front, class.as_mut_ptr(), class.len() as i32);
            let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
            // The desktop itself covers the screen.
            if class == "Progman" || class == "WorkerW" {
                return false;
            }
            let mut rect = WinRect::default();
            let monitor = MonitorFromWindow(front, MONITOR_DEFAULTTONULL);
            let mut info = MonitorInfo {
                size: std::mem::size_of::<MonitorInfo>() as u32,
                ..Default::default()
            };
            // A maximised window overhangs its screen by its invisible border, so only an exact
            // match is fullscreen.
            monitor != 0
                && GetWindowRect(front, &mut rect) != 0
                && GetMonitorInfoW(monitor, &mut info) != 0
                && rect == info.monitor
        }
    }

    /// The monitors, the primary first.
    fn desktop(app: &AppHandle) -> Option<Desktop> {
        let monitors = app.available_monitors().ok()?;
        if monitors.is_empty() {
            return None;
        }
        let primary = app.primary_monitor().ok().flatten().map(|m| *m.position());
        let mut list: Vec<(bool, Monitor)> = monitors
            .iter()
            .map(|m| {
                let (p, s, w) = (m.position(), m.size(), m.work_area());
                let monitor = Monitor {
                    bounds: Rect::new(
                        f64::from(p.x),
                        f64::from(p.y),
                        f64::from(s.width),
                        f64::from(s.height),
                    ),
                    work: Rect::new(
                        f64::from(w.position.x),
                        f64::from(w.position.y),
                        f64::from(w.size.width),
                        f64::from(w.size.height),
                    ),
                    scale: m.scale_factor(),
                };
                (Some(*p) != primary, monitor)
            })
            .collect();
        list.sort_by_key(|(other, _)| *other);
        Some(Desktop::new(list.into_iter().map(|(_, m)| m).collect()))
    }

    /// Wait up to `period`, or until the page says something; then take what it said.
    fn wait(period: Duration) -> Control {
        let mut c = CONTROL.lock().unwrap_or_else(|p| p.into_inner());
        if c.grab.is_none() && !c.release && c.mood.is_none() {
            c = WAKE
                .wait_timeout(c, period)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        Control {
            ready: c.ready,
            grab: c.grab.take(),
            release: std::mem::take(&mut c.release),
            mood: c.mood.take(),
        }
    }

    pub fn run(app: AppHandle, data_dir: PathBuf, debug: bool) {
        let window = match build(&app, data_dir, debug) {
            Ok(w) => w,
            Err(e) => return log::warn!("the roaming pet's window could not be made: {e}"),
        };
        let Ok(hwnd) = window.hwnd() else { return };
        let hwnd = hwnd.0 as isize;
        assert_style(hwnd);
        // Made at Windows' minimum width; with the minimum lowered, it can now be the sprite.
        let _ = window.set_size(tauri::LogicalSize::new(SPRITE, SPRITE));
        // Shown once the page has drawn.
        {
            let mut c = CONTROL.lock().unwrap_or_else(|p| p.into_inner());
            while !c.ready {
                c = WAKE
                    .wait_timeout(c, HIDDEN)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
                if app.get_webview_window(LABEL).is_none() {
                    return;
                }
            }
        }
        let Some(mut desk) = desktop(&app) else {
            return log::warn!("the roaming pet found no monitors");
        };
        let Some(home) = desk.home() else { return };
        let scale = desk.scale_at(home.x + home.w / 2.0, home.y);
        let size = SPRITE * scale;
        // It drops in from the top of the primary screen.
        let body = Body::new(home.x + (home.w - size) / 2.0, home.y, size);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        let mut pet = Pet::new(seed, body);
        let mut corner = pet.body.corner();
        move_to(hwnd, corner);
        let shown = window.clone();
        let _ = app.run_on_main_thread(move || show_without_activating(&shown));
        log::info!("the roaming pet is out");

        let mut sent: Option<(State, super::physics::Facing)> = None;
        let mut hidden = false;
        let mut last = Instant::now();
        let (mut scanned, mut topped) = (last, last);
        loop {
            let period = if hidden {
                HIDDEN
            } else if pet.moving() {
                FAST
            } else {
                REST
            };
            let input = wait(period);
            if app.get_webview_window(LABEL).is_none() {
                return log::info!("the roaming pet's window closed");
            }
            let now = Instant::now();
            let elapsed = now.duration_since(last).as_secs_f64();
            last = now;
            if now.duration_since(scanned) >= RESCAN {
                scanned = now;
                if let Some(d) = desktop(&app) {
                    desk = d;
                }
                let hide = fullscreen_in_front();
                if hide != hidden {
                    hidden = hide;
                    // SAFETY: as above.
                    unsafe { ShowWindow(hwnd, if hide { SW_HIDE } else { SW_SHOWNOACTIVATE }) };
                    if !hide {
                        assert_style(hwnd);
                    }
                }
            }
            if now.duration_since(topped) >= TOPMOST_EVERY {
                topped = now;
                assert_style(hwnd);
            }
            if let Some((mood, still)) = input.mood {
                pet.set_mood(mood, still);
            }
            if hidden {
                if pet.held() {
                    pet.release(&desk);
                }
                continue;
            }
            if let Some(grip) = input.grab {
                pet.grab(grip);
            }
            if pet.held() && (input.release || !button_down()) {
                pet.release(&desk);
            }
            if let Some(speed) = pet.tick(&desk, elapsed, cursor()) {
                log::info!(
                    "the roaming pet landed at {speed:.0} px/s, at {:?}",
                    pet.body.corner()
                );
            }
            let at = pet.body.corner();
            if at != corner {
                corner = at;
                move_to(hwnd, at);
            }
            let body = (pet.body.state, pet.body.facing);
            if sent != Some(body) {
                sent = Some(body);
                let event = AppEvent::new("petworld_body")
                    .with("state", body.0.as_str())
                    .with("facing", body.1.as_str());
                let _ = app.emit_to(LABEL, EVENT_CHANNEL, event.to_value());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_debug_build_with_the_switch_opens_it() {
        for (debug, value, want) in [
            (true, Some("1"), true),
            (true, Some("0"), false),
            (true, Some("yes"), false),
            (true, None, false),
            (false, Some("1"), false),
        ] {
            assert_eq!(wanted(debug, value), want, "{debug} {value:?}");
        }
    }
}
