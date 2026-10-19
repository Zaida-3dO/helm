//! Tauri glue: the main window, the event sink and the app-control seam.

use helm_core::lifecycle::AppControl;
use helm_core::profile::Profile;
use helm_ipc::{
    authority::main_window_may_navigate, AppEvent, EventSink, EVENT_CHANNEL, MAIN_WINDOW,
};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

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

/// The main window is built in code, not from static config, so its navigation policy can be
/// attached: only the app origin, `about:srcdoc` and (debug) the dev server; no new
/// windows. Its WebView2 data lives inside the profile, so profiles never share web storage.
pub fn build_main_window(app: &tauri::App, profile: &Profile, debug: bool) -> tauri::Result<()> {
    let data_dir = profile.path("webview");
    helm_core::privfs::create_private_dir_all(&data_dir)?;
    let window = WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::App("index.html".into()))
        .title("Helm")
        // Restored size 1220 x 820, minimum 760 x 540, opens maximised, created
        // hidden and shown by the UI's `window_ready` once the first frame is drawn. Helm draws
        // its own caption, so the native frame is off.
        .inner_size(1220.0, 820.0)
        .min_inner_size(760.0, 540.0)
        .maximized(true)
        .visible(false)
        .decorations(false)
        .data_directory(data_dir)
        .on_navigation(move |url| {
            let ok = main_window_may_navigate(url.as_str(), debug);
            if !ok {
                log::warn!("main window navigation refused");
            }
            ok
        })
        .on_new_window(|_url, _features| tauri::webview::NewWindowResponse::Deny)
        .build()?;
    // Files dragged in from Explorer reach the composer as paths.
    let handle = app.handle().clone();
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
