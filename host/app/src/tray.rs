//! The Windows shell's Tauri side: the one tray icon and its menu,
//! the global hotkey, the taskbar overlay badge and flash, native toasts, and the event tap that
//! feeds `helm_shell::ShellService`.

use helm_chats::types::Completion;
use helm_ipc::{AppEvent, EventSink, MAIN_WINDOW};
use helm_shell::tray::TrayMenu;
use helm_shell::{NotificationMode, ShellHost, ShellService};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub const TRAY_ID: &str = "helm";

/// Every event on its way to the renderer also goes past the shell service. The service is
/// set once it exists; events before that are only forwarded.
pub struct TappedEvents {
    inner: Arc<dyn EventSink>,
    shell: OnceLock<Arc<ShellService>>,
}

impl TappedEvents {
    pub fn new(inner: Arc<dyn EventSink>) -> Self {
        Self {
            inner,
            shell: OnceLock::new(),
        }
    }

    pub fn attach(&self, shell: Arc<ShellService>) {
        let _ = self.shell.set(shell);
    }
}

impl EventSink for TappedEvents {
    fn emit(&self, event: AppEvent) {
        if let Some(s) = self.shell.get() {
            if matches!(event.kind.as_str(), "attention" | "completion") {
                s.observe(&event);
            }
        }
        self.inner.emit(event);
    }
}

/// The tray, hotkey, badge and toasts, drawn with Tauri.
pub struct TauriShellHost {
    app: AppHandle,
    tray: Mutex<Option<TrayIcon>>,
    hotkey: Mutex<Option<Shortcut>>,
    service: OnceLock<Arc<ShellService>>,
}

impl TauriShellHost {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            tray: Mutex::new(None),
            hotkey: Mutex::new(None),
            service: OnceLock::new(),
        })
    }

    /// Build the tray icon (once) and wire its events to the service.
    pub fn install(self: &Arc<Self>, service: Arc<ShellService>) -> tauri::Result<()> {
        let _ = self.service.set(service.clone());
        let me = self.clone();
        let mut builder = TrayIconBuilder::with_id(TRAY_ID)
            .tooltip("Helm")
            .show_menu_on_left_click(false)
            .on_menu_event(move |app, ev| me.on_menu(app, ev.id().as_ref()));
        if let Some(icon) = self.app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }
        let me = self.clone();
        builder = builder.on_tray_icon_event(move |_tray, ev| match ev {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => me.toggle_window(),
            // The menu opens on release; refresh the microphone list on press.
            TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Down,
                ..
            } => {
                if let Some(s) = me.service.get() {
                    let menu = s.tray_menu(true);
                    me.set_tray(&menu);
                }
            }
            _ => {}
        });
        let tray = builder.build(&self.app)?;
        *self.tray.lock().unwrap() = Some(tray);
        Ok(())
    }

    fn window(&self) -> Option<tauri::WebviewWindow> {
        self.app.get_webview_window(MAIN_WINDOW)
    }

    fn show(&self) {
        if let Some(w) = self.window() {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }

    /// Show and focus Helm, or hide it when it is already in front (tray left-click, hotkey).
    fn toggle_window(&self) {
        let Some(w) = self.window() else { return };
        let front = w.is_visible().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false);
        if front {
            let _ = w.hide();
        } else {
            self.show();
        }
    }

    fn on_menu(&self, app: &AppHandle, id: &str) {
        let svc = self.service.get();
        let report = |r: Result<(), String>| {
            if let Err(e) = r {
                log::warn!("tray: {e}");
            }
        };
        match id {
            "show" => self.show(),
            "quit" => {
                // The normal quit path, with its confirmation (helm_core::ops quit_request).
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(d) = app.try_state::<Arc<helm_ipc::Dispatcher>>() {
                        let caller = helm_ipc::Caller {
                            window_label: MAIN_WINDOW.to_owned(),
                            url: "http://tauri.localhost/".to_owned(),
                        };
                        let _ = d
                            .call(&caller, "quit_request", serde_json::Value::Null)
                            .await;
                    }
                });
            }
            "keep_awake" => {
                if let Some(s) = svc {
                    report(s.set_keep_awake_setting(!s.settings().keep_awake));
                }
            }
            "notif:in_app" => {
                if let Some(s) = svc {
                    report(s.set_notification_mode(NotificationMode::InApp));
                }
            }
            "notif:native" => {
                if let Some(s) = svc {
                    report(s.set_notification_mode(NotificationMode::Native));
                }
            }
            "mic:auto" => {
                if let Some(s) = svc {
                    report(s.pick_mic(None));
                }
            }
            other => {
                if let Some(uid) = other.strip_prefix("mic:") {
                    if let Some(s) = svc {
                        report(s.pick_mic(Some(uid)));
                    }
                } else if let Some(chat) = other.strip_prefix("chat:") {
                    self.open_chat(chat, None);
                }
            }
        }
    }

    /// The shared "open chat" path: show the window and ask the UI to open the chat.
    pub fn open_chat(&self, chat_id: &str, completion_id: Option<&str>) {
        self.show();
        let mut ev = AppEvent::new("shell_open_chat").with("chatId", chat_id);
        if let Some(c) = completion_id {
            ev = ev.with("completionId", c);
        }
        crate::shell::emit(&self.app, ev);
    }

    fn build_menu(&self, m: &TrayMenu) -> tauri::Result<Menu<tauri::Wry>> {
        let app = &self.app;
        let menu = Menu::new(app)?;
        menu.append(&MenuItem::with_id(
            app,
            "show",
            "Show Helm",
            true,
            None::<&str>,
        )?)?;
        for g in &m.groups {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
            menu.append(&MenuItem::with_id(
                app,
                format!("head:{}", g.label),
                &g.label,
                false,
                None::<&str>,
            )?)?;
            for it in &g.items {
                menu.append(&MenuItem::with_id(
                    app,
                    format!("chat:{}", it.chat_id),
                    format!("    {}", it.label),
                    true,
                    None::<&str>,
                )?)?;
            }
        }
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        let mics = Submenu::with_id(app, "mics", "Microphone", true)?;
        for e in &m.mics {
            let id = match &e.id {
                None => "mic:auto".to_owned(),
                Some(uid) => format!("mic:{uid}"),
            };
            mics.append(&CheckMenuItem::with_id(
                app,
                id,
                &e.label,
                true,
                e.checked,
                None::<&str>,
            )?)?;
        }
        menu.append(&mics)?;
        menu.append(&CheckMenuItem::with_id(
            app,
            "keep_awake",
            "Keep awake while chats run",
            true,
            m.keep_awake,
            None::<&str>,
        )?)?;
        let notif = Submenu::with_id(app, "notif", "Notifications", true)?;
        notif.append(&CheckMenuItem::with_id(
            app,
            "notif:in_app",
            "In Helm",
            true,
            m.notifications == NotificationMode::InApp,
            None::<&str>,
        )?)?;
        notif.append(&CheckMenuItem::with_id(
            app,
            "notif:native",
            "Windows notifications",
            true,
            m.notifications == NotificationMode::Native,
            None::<&str>,
        )?)?;
        menu.append(&notif)?;
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        menu.append(&MenuItem::with_id(
            app,
            "quit",
            "Quit Helm",
            true,
            None::<&str>,
        )?)?;
        Ok(menu)
    }
}

impl ShellHost for TauriShellHost {
    fn set_badge(&self, label: Option<&str>) {
        let Some(w) = self.window() else { return };
        #[cfg(windows)]
        {
            let icon = label.map(overlay_icon);
            if let Err(e) = w.set_overlay_icon(icon) {
                log::warn!("taskbar badge: {e}");
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (w, label);
        }
    }

    fn flash(&self) {
        if let Some(w) = self.window() {
            let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
        }
    }

    fn is_focused(&self) -> bool {
        self.window()
            .map(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
            .unwrap_or(false)
    }

    fn set_tray(&self, menu: &TrayMenu) {
        let built = match self.build_menu(menu) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("tray menu: {e}");
                return;
            }
        };
        if let Some(t) = self.tray.lock().unwrap().as_ref() {
            let _ = t.set_menu(Some(built));
        }
    }

    fn post_toast(&self, c: &Completion) {
        use tauri_plugin_notification::NotificationExt;
        let title = if c.title.trim().is_empty() {
            "Task finished".to_owned()
        } else {
            c.title.clone()
        };
        let body = if c.project.is_empty() {
            c.preview.clone()
        } else {
            format!("{}\n{}", c.project, c.preview)
        };
        if let Err(e) = self
            .app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
        {
            log::warn!("native toast: {e}");
        }
    }

    fn withdraw_toast(&self, _chat_id: &str) {
        // The notification plugin cannot retract a posted Windows toast; it ages out of the
        // Action Center on its own (follow-up: a WinRT ToastNotifier with per-chat tags).
    }

    fn register_hotkey(&self, chord: Option<&str>) -> Result<(), String> {
        let gs = self.app.global_shortcut();
        if let Some(old) = self.hotkey.lock().unwrap().take() {
            let _ = gs.unregister(old);
        }
        let Some(chord) = chord else { return Ok(()) };
        let sc: Shortcut = chord
            .parse()
            .map_err(|e| format!("{chord} is not a shortcut Windows accepts: {e}"))?;
        gs.register(sc).map_err(|e| {
            format!("{chord} could not be registered (another app may own it): {e}")
        })?;
        *self.hotkey.lock().unwrap() = Some(sc);
        Ok(())
    }
}

/// The plugin's handler: a press of the registered chord toggles the window.
pub fn on_shortcut(app: &AppHandle, state: ShortcutState) {
    if state != ShortcutState::Pressed {
        return;
    }
    if let Some(host) = app.try_state::<Arc<TauriShellHost>>() {
        host.toggle_window();
    }
}

/// A 16x16 overlay: a dark disc with the count in white, drawn from a 3x5 pixel font so no image
/// files are needed for "1"-"9" and "9+".
#[cfg(windows)]
fn overlay_icon(label: &str) -> tauri::image::Image<'static> {
    const W: usize = 16;
    let mut px = vec![0u8; W * W * 4];
    let put = |px: &mut Vec<u8>, x: usize, y: usize, c: [u8; 4]| {
        if x < W && y < W {
            px[(y * W + x) * 4..(y * W + x) * 4 + 4].copy_from_slice(&c);
        }
    };
    for y in 0..W {
        for x in 0..W {
            let (dx, dy) = (x as f32 - 7.5, y as f32 - 7.5);
            if dx * dx + dy * dy <= 7.6 * 7.6 {
                put(&mut px, x, y, [0xb5, 0x4a, 0x44, 0xff]);
            }
        }
    }
    let glyph = |ch: char| -> [u8; 5] {
        match ch {
            '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
            '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
            '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
            '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
            '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
            '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
            '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
            '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
            '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
            '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
            _ => [0; 5],
        }
    };
    let chars: Vec<char> = label.chars().take(2).collect();
    let scale = if chars.len() == 1 { 2 } else { 1 };
    let total_w = chars.len() * 3 * scale + (chars.len().saturating_sub(1)) * scale;
    let x0 = (W - total_w) / 2;
    let y0 = (W - 5 * scale) / 2;
    for (i, ch) in chars.iter().enumerate() {
        let g = glyph(*ch);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            put(
                                &mut px,
                                x0 + i * 4 * scale + col * scale + sx,
                                y0 + row * scale + sy,
                                [0xff, 0xff, 0xff, 0xff],
                            );
                        }
                    }
                }
            }
        }
    }
    tauri::image::Image::new_owned(px, W as u32, W as u32)
}
