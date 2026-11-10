//! The desktop pet's window: a small transparent always-on-top window that shows the same page as
//! the main window with `?window=pet`, so the renderer draws a creature instead of the app.
//!
//! The `desktopPet` preference decides everything: `sync_pet` makes the window when it is enabled
//! and destroys it when it is not, on every change and once at start-up. The window may call only
//! the few ops listed in `allow_ops`, and it gets only the events its page draws from (the
//! attention summary, finished replies, chat records and preferences). Its position is the user's:
//! the page reports where the window was dragged and the host stores it, clamped to a visible
//! monitor whenever the window is made again.

use crate::shell::{
    guard_permissions, show_without_activating, watch_process_failures, BROWSER_ARGS,
};
use helm_core::db::Database;
use helm_core::lifecycle::Shell;
use helm_core::prefs;
use helm_ipc::authority::main_window_may_navigate;
use helm_ipc::{
    ApiError, AppEvent, Dispatcher, EventSink, OpClass, OpCtx, OpRegistry, EVENT_CHANNEL,
};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

pub const PET_WINDOW: &str = "pet";

/// The window's size in logical pixels: room for the largest creature and its speech bubble.
const WIDTH: f64 = 168.0;
const HEIGHT: f64 = 196.0;
/// The gap to the screen edges when the pet has no saved place.
const MARGIN: f64 = 24.0;

/// The events the pet's page draws from. Chat streaming and everything else stays in the main
/// window.
const FORWARDED: &[&str] = &["attention", "completion", "chat", "preferences"];

/// What making the window needs, set once at start-up.
struct Config {
    data_dir: PathBuf,
    debug: bool,
}

static CONFIG: OnceLock<Config> = OnceLock::new();
static APP: OnceLock<AppHandle> = OnceLock::new();
/// One window change at a time: two quick preference changes must not both make the window.
static SYNC: Mutex<()> = Mutex::new(());

// --- pure helpers -------------------------------------------------------------------------------

/// A rectangle on the desktop in physical pixels: x, y, width, height.
type Area = (i32, i32, i32, i32);

/// Whether the preference asks for the pet.
pub fn wants_pet(pet: &Value) -> bool {
    pet.get("enabled").and_then(Value::as_bool) == Some(true)
}

/// The window's saved top-left corner.
pub fn saved_position(pet: &Value) -> Option<(i32, i32)> {
    let p = pet.get("position")?;
    let coord = |name: &str| i32::try_from(p.get(name)?.as_i64()?).ok();
    Some((coord("x")?, coord("y")?))
}

/// Where the window goes: the saved corner kept fully inside the screen it was on, or, when that
/// screen is gone (or nothing is saved), the bottom-right of the primary work area with `margin`
/// to the edges. `areas` are the monitors' work areas.
fn place(
    saved: Option<(i32, i32)>,
    size: (i32, i32),
    margin: i32,
    areas: &[Area],
    primary: usize,
) -> (i32, i32) {
    let Some(&home) = areas.get(primary).or(areas.first()) else {
        return saved.unwrap_or((0, 0));
    };
    let keep = |(ax, ay, aw, ah): Area, (x, y): (i32, i32)| {
        (
            x.clamp(ax, (ax + aw - size.0).max(ax)),
            y.clamp(ay, (ay + ah - size.1).max(ay)),
        )
    };
    if let Some((x, y)) = saved {
        let (cx, cy) = (x + size.0 / 2, y + size.1 / 2);
        let on = areas
            .iter()
            .find(|&&(ax, ay, aw, ah)| cx >= ax && cx < ax + aw && cy >= ay && cy < ay + ah);
        if let Some(&area) = on {
            return keep(area, (x, y));
        }
    }
    keep(
        home,
        (
            home.0 + home.2 - size.0 - margin,
            home.1 + home.3 - size.1 - margin,
        ),
    )
}

/// The stored preference with `change` applied to its fields (a missing one starts empty).
fn changed_pref(stored: Option<Value>, change: impl FnOnce(&mut Map<String, Value>)) -> Value {
    let mut fields = match stored {
        Some(Value::Object(o)) => o,
        _ => Map::new(),
    };
    change(&mut fields);
    Value::Object(fields)
}

// --- the window ---------------------------------------------------------------------------------

fn rect(r: &tauri::PhysicalRect<i32, u32>) -> Area {
    (
        r.position.x,
        r.position.y,
        r.size.width as i32,
        r.size.height as i32,
    )
}

/// The monitors' work areas and which one is the primary.
fn desktop(app: &AppHandle) -> (Vec<Area>, usize) {
    let areas: Vec<Area> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| rect(m.work_area()))
        .collect();
    let primary = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| rect(m.work_area()))
        .and_then(|p| areas.iter().position(|a| *a == p))
        .unwrap_or(0);
    (areas, primary)
}

/// The pet's page failed (WebView2 reported a failed process). The same loop guard as the main
/// window's decides; the pet never asks the user anything, so where the main window would offer a
/// reload, the pet is closed until the next start or the next change to its preference.
fn on_failed(app: &AppHandle, kind: i32) {
    use helm_shell::uiwatch::{CrashGuard, Failure, Recovery};
    static GUARD: Mutex<Option<CrashGuard>> = Mutex::new(None);
    let failure = Failure::from_kind(kind);
    let recovery = GUARD
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(CrashGuard::new)
        .on_failure(std::time::Instant::now(), failure);
    log::warn!("the desktop pet's page reported {failure:?}; {recovery:?}");
    let app = app.clone();
    // Off the web view's own event: closing and making a window both wait on its thread.
    std::thread::spawn(move || {
        let Some(window) = app.get_webview_window(PET_WINDOW) else {
            return;
        };
        match recovery {
            Recovery::Ignore => {}
            Recovery::Reload => {
                let _ = window.reload();
            }
            Recovery::Rebuild | Recovery::Offer => {
                let at = window.outer_position().ok().map(|p| (p.x, p.y));
                let _one_at_a_time = SYNC.lock().unwrap_or_else(|p| p.into_inner());
                let _ = window.destroy();
                if recovery == Recovery::Rebuild {
                    // The old window's label is free a moment after it is destroyed.
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    if let Err(e) = build(&app, at) {
                        log::warn!("the desktop pet window could not be made again: {e}");
                    }
                }
            }
        }
    });
}

fn build(app: &AppHandle, saved: Option<(i32, i32)>) -> tauri::Result<()> {
    let Some(config) = CONFIG.get() else {
        return Ok(());
    };
    let debug = config.debug;
    // Hidden until the page has drawn its first frame and says `pet_ready`.
    let window = WebviewWindowBuilder::new(
        app,
        PET_WINDOW,
        WebviewUrl::App("index.html?window=pet".into()),
    )
    .title("Helm pet")
    .inner_size(WIDTH, HEIGHT)
    .transparent(true)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focused(false)
    .resizable(false)
    .shadow(false)
    .visible(false)
    // The same data folder and browser switches as the main window, so both share one web view
    // environment.
    .data_directory(config.data_dir.clone())
    .additional_browser_args(BROWSER_ARGS)
    .on_navigation(move |url| main_window_may_navigate(url.as_str(), debug))
    .on_new_window(|_url, _features| tauri::webview::NewWindowResponse::Deny)
    .build()?;
    // The pet never dictates, so it gets no microphone.
    guard_permissions(&window, debug, false);
    watch_process_failures(&window, on_failed);
    let scale = window.scale_factor().unwrap_or(1.0);
    let (areas, primary) = desktop(app);
    let size = (
        (WIDTH * scale).round() as i32,
        (HEIGHT * scale).round() as i32,
    );
    let (x, y) = place(
        saved,
        size,
        (MARGIN * scale).round() as i32,
        &areas,
        primary,
    );
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    Ok(())
}

/// Make the window match the preference: made when enabled, destroyed when not, left alone when it
/// already matches (a position-only change never moves it).
pub fn sync_pet(app: &AppHandle, pet: &Value) {
    let Some(config) = CONFIG.get() else { return };
    // A test run in the background window never opens a second, visible window.
    if crate::shell::test_background(config.debug) {
        return;
    }
    let _one_at_a_time = SYNC.lock().unwrap_or_else(|p| p.into_inner());
    let window = app.get_webview_window(PET_WINDOW);
    match (wants_pet(pet), window) {
        (false, Some(w)) => {
            let _ = w.destroy();
        }
        (true, None) => {
            if let Err(e) = build(app, saved_position(pet)) {
                log::warn!("the desktop pet window could not be made: {e}");
            }
        }
        _ => {}
    }
}

/// Read the preference once and make the window if it is on. Called from setup.
pub fn start(app: &AppHandle, db: &Database, data_dir: PathBuf, debug: bool) {
    let _ = CONFIG.set(Config { data_dir, debug });
    let _ = APP.set(app.clone());
    let pet = db
        .snapshot(|tx| Ok::<_, prefs::PrefError>(prefs::read(tx, "desktopPet")))
        .ok()
        .flatten();
    if let Some(pet) = pet.filter(wants_pet) {
        let app = app.clone();
        // Making a window waits on the event loop, which setup is still inside.
        std::thread::spawn(move || sync_pet(&app, &pet));
    }
}

/// Every event on its way out also reaches the pet's page when it has one, and a change to the
/// `desktopPet` preference makes or closes the window.
pub struct PetEvents {
    inner: Arc<dyn EventSink>,
    app: AppHandle,
}

impl PetEvents {
    pub fn wrap(inner: Arc<dyn EventSink>, app: AppHandle) -> Arc<dyn EventSink> {
        Arc::new(Self { inner, app })
    }
}

impl EventSink for PetEvents {
    fn emit(&self, event: AppEvent) {
        if FORWARDED.contains(&event.kind.as_str())
            && self.app.get_webview_window(PET_WINDOW).is_some()
        {
            let _ = self
                .app
                .emit_to(PET_WINDOW, EVENT_CHANNEL, event.to_value());
        }
        if event.kind == "preferences" {
            let pet = event
                .fields
                .get("changed")
                .and_then(|c| c.get("desktopPet"));
            if let Some(pet) = pet {
                let (app, pet) = (self.app.clone(), pet.clone());
                std::thread::spawn(move || sync_pet(&app, &pet));
            }
        }
        self.inner.emit(event);
    }
}

// --- ops the pet's page may call ------------------------------------------------------------------

/// The only ops the pet's window may call: the attention summary and preference reads its page
/// draws from, and its own four.
pub fn allow_ops(dispatcher: &mut Dispatcher) {
    dispatcher.allow_window_ops(
        PET_WINDOW,
        &[
            "attention_get",
            "preference_get",
            "pet_ready",
            "pet_moved",
            "pet_open",
            "pet_hide",
        ],
    );
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("pet_ready", OpClass::Blocking, pet_ready)
        .blocking("pet_moved", OpClass::Blocking, pet_moved)
        .blocking("pet_open", OpClass::Blocking, pet_open)
        .blocking("pet_hide", OpClass::Blocking, pet_hide)
        .blocking("intro_allowed", OpClass::Blocking, intro_allowed);
}

/// Store a change to the pet's preference and tell the windows.
fn update(ctx: &OpCtx, change: impl FnOnce(&mut Map<String, Value>)) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let pet = db
        .write(|tx| {
            let pet = changed_pref(prefs::read(tx, "desktopPet"), change);
            prefs::set(tx, prefs::DESKTOP_PET, &pet)?;
            Ok::<_, prefs::PrefError>(pet)
        })
        .map_err(|e| ApiError::internal(e.to_string()))?;
    ctx.events
        .emit(AppEvent::new("preferences").with("changed", json!({ "desktopPet": pet })));
    Ok(Value::Null)
}

/// The page drew its first frame: show the window, without taking focus.
fn pet_ready(_ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    if let Some(app) = APP.get() {
        if let Some(window) = app.get_webview_window(PET_WINDOW) {
            let shown = window.clone();
            let _ = app.run_on_main_thread(move || show_without_activating(&shown));
        }
    }
    Ok(Value::Null)
}

/// `{x, y}`: where the user dragged the window (the page throttles these).
fn pet_moved(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let coord = |name: &str| {
        args.get(name)
            .and_then(Value::as_i64)
            .filter(|n| n.abs() <= 100_000)
            .ok_or_else(|| ApiError::bad_args(format!("{name} must be a whole number")))
    };
    let (x, y) = (coord("x")?, coord("y")?);
    update(ctx, |pet| {
        pet.insert("position".into(), json!({ "x": x, "y": y }));
    })
}

/// `{chatId}` opens that chat in Helm, `{chatId: null}` opens the Needs attention list,
/// `{settings: true}` opens Settings on the Appearance pane, and no arguments just bring Helm to
/// the front (every form does that).
fn pet_open(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let event = if args.get("settings").and_then(Value::as_bool) == Some(true) {
        Some(AppEvent::new("shell_open_settings").with("pane", "appearance"))
    } else {
        match args.get("chatId") {
            None => None,
            Some(Value::Null) => Some(AppEvent::new("shell_open_attention")),
            Some(Value::String(id)) if !id.is_empty() && id.len() <= 200 => {
                Some(AppEvent::new("shell_open_chat").with("chatId", id.as_str()))
            }
            _ => return Err(ApiError::bad_args("chatId must be a chat id or null")),
        }
    };
    ctx.services.need::<Shell>()?.0.show_main();
    if let Some(event) = event {
        ctx.events.emit(event);
    }
    Ok(Value::Null)
}

/// Turn the pet off from its own menu; the window closes through the preference change.
fn pet_hide(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    update(ctx, |pet| {
        pet.insert("enabled".into(), Value::Bool(false));
    })
}

/// Whether the first-run intro may play. Not in a test run in the background window (debug builds
/// only), where it would sit over the screen the checks click on.
fn intro_allowed(_ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    Ok(json!({"allowed": !crate::shell::test_background(cfg!(debug_assertions))}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preference_decides_whether_there_is_a_pet() {
        for (pet, want) in [
            (json!(null), false),
            (json!({}), false),
            (json!({"enabled": false, "kind": "crab"}), false),
            (json!({"enabled": "yes"}), false),
            (json!({"enabled": true}), true),
        ] {
            assert_eq!(wants_pet(&pet), want, "{pet}");
        }
        assert_eq!(
            saved_position(&json!({"position": {"x": -40, "y": 90}})),
            Some((-40, 90))
        );
        for pet in [
            json!({}),
            json!({"position": null}),
            json!({"position": {"x": 1}}),
            json!({"position": {"x": "1", "y": 2}}),
            json!({"position": {"x": 1e12, "y": 2}}),
        ] {
            assert_eq!(saved_position(&pet), None, "{pet}");
        }
    }

    #[test]
    fn the_window_stays_on_a_visible_screen() {
        // Two screens side by side; the second is the primary.
        let areas: [Area; 2] = [(0, 0, 1920, 1040), (1920, 0, 1280, 720)];
        let size = (168, 196);
        // No saved place: the primary's bottom-right corner with the margin.
        assert_eq!(
            place(None, size, 24, &areas, 1),
            (1920 + 1280 - 168 - 24, 720 - 196 - 24)
        );
        // A saved place that is on a screen is kept; one hanging off an edge is pulled back in.
        assert_eq!(place(Some((300, 200)), size, 24, &areas, 1), (300, 200));
        assert_eq!(place(Some((1700, 900)), size, 24, &areas, 1), (1700, 844));
        assert_eq!(place(Some((-5, -5)), size, 24, &areas, 1), (0, 0));
        // A screen that is gone: back to the default on the primary.
        assert_eq!(
            place(Some((5000, 100)), size, 24, &areas, 1),
            (1920 + 1280 - 168 - 24, 720 - 196 - 24)
        );
        // No monitors known: the saved place as it is, or the origin.
        assert_eq!(place(Some((7, 8)), size, 24, &[], 0), (7, 8));
        assert_eq!(place(None, size, 24, &[], 0), (0, 0));
        // A screen smaller than the window never makes the clamp panic.
        assert_eq!(place(None, size, 24, &[(0, 0, 100, 100)], 0), (0, 0));
    }

    #[test]
    fn a_change_keeps_the_fields_it_leaves_alone() {
        let stored = Some(json!({"enabled": true, "kind": "crab", "position": {"x": 1, "y": 2}}));
        let moved = changed_pref(stored.clone(), |p| {
            p.insert("position".into(), json!({"x": 9, "y": 8}));
        });
        assert_eq!(
            moved,
            json!({"enabled": true, "kind": "crab", "position": {"x": 9, "y": 8}})
        );
        let hidden = changed_pref(stored, |p| {
            p.insert("enabled".into(), Value::Bool(false));
        });
        assert_eq!(hidden["kind"], "crab");
        assert_eq!(hidden["enabled"], false);
        // Nothing stored yet, or something that is not an object, starts from empty.
        assert_eq!(
            changed_pref(None, |p| {
                p.insert("enabled".into(), Value::Bool(false));
            }),
            json!({"enabled": false})
        );
        assert_eq!(changed_pref(Some(json!(3)), |_| {}), json!({}));
    }
}
