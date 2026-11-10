//! The Windows shell's Tauri side: the one tray icon, its menu and its state dot,
//! the global hotkey, the taskbar overlay badge and flash, native toasts, and the event tap that
//! feeds `helm_shell::ShellService`.
//!
//! Toasts are WinRT toasts (`helm_platform::toast`) shown under an app id registered for the
//! current user. A click comes back in process while Helm runs: Open shows the chat; Continue and
//! Reply are the user's own sends from a notification. If Windows will not show a toast, the
//! notification plugin's plain one is used instead.

use helm_ipc::{AppEvent, EventSink, MAIN_WINDOW};
use helm_platform::toast::{
    register_app_id, tag_for, Activation, NativeToasts, NoToasts, ToastSpec, Toasts,
};
use helm_platform::trayicon::{self, TrayDot};
use helm_shell::toasts::{parse_click, submission_id, Click};
use helm_shell::tray::TrayMenu;
use helm_shell::{NotificationMode, ShellHost, ShellService};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub const TRAY_ID: &str = "helm";

/// The icon the toast header shows, written beside the temporary files on first use.
const TOAST_ICON: &[u8] = include_bytes!("../icons/128x128.png");

/// True in a debug build driven by a check (it asks for the probe file or the background window).
/// Such a run never shows a real toast and never falls back to one.
fn under_test() -> bool {
    cfg!(debug_assertions)
        && [helm_shell::probe::PROBE_ENV, "HELM_TEST_WINDOW"]
            .iter()
            .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

fn toast_icon_path() -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join("helm-toast-icon.png");
    let same = std::fs::metadata(&path).is_ok_and(|m| m.len() == TOAST_ICON.len() as u64);
    if !same {
        std::fs::write(&path, TOAST_ICON).ok()?;
    }
    Some(path)
}

/// Every event on its way to the renderer also goes past the shell service. The service is
/// set once it exists; events before that are only forwarded.
pub struct TappedEvents {
    inner: Arc<dyn EventSink>,
    shell: OnceLock<Arc<ShellService>>,
    voice: OnceLock<Arc<helm_relay::voice_watch::VoiceWatch>>,
    devices: OnceLock<Arc<helm_devices::EventTap>>,
    knowledge: OnceLock<Arc<helm_skills::sync::Trigger>>,
}

impl TappedEvents {
    pub fn new(inner: Arc<dyn EventSink>) -> Self {
        Self {
            inner,
            shell: OnceLock::new(),
            voice: OnceLock::new(),
            devices: OnceLock::new(),
            knowledge: OnceLock::new(),
        }
    }

    pub fn attach(&self, shell: Arc<ShellService>) {
        let _ = self.shell.set(shell);
    }

    /// The voice chat's watch also sees attention, finished replies and busy chats.
    pub fn attach_voice(&self, watch: Arc<helm_relay::voice_watch::VoiceWatch>) {
        let _ = self.voice.set(watch);
    }

    /// Other devices that watch this PC's chats hear chat events too.
    pub fn attach_devices(&self, tap: Arc<helm_devices::EventTap>) {
        let _ = self.devices.set(tap);
    }

    /// A change to a knowledge layer wakes the knowledge sync.
    pub fn attach_knowledge(&self, trigger: Arc<helm_skills::sync::Trigger>) {
        let _ = self.knowledge.set(trigger);
    }
}

impl EventSink for TappedEvents {
    fn emit(&self, event: AppEvent) {
        if let Some(s) = self.shell.get() {
            if matches!(event.kind.as_str(), "attention" | "completion") {
                s.observe(&event);
            }
        }
        if let Some(v) = self.voice.get() {
            v.observe(&event);
        }
        if let Some(d) = self.devices.get() {
            d.observe(&event);
        }
        if let Some(k) = self.knowledge.get() {
            k.observe(&event);
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
    toasts: Arc<dyn Toasts>,
    /// A check is driving this run: no toast is shown, not even the plain fallback.
    quiet: bool,
    /// The tray icon's own pixels (RGBA, width, height), which the state dot is drawn on.
    base_icon: Mutex<Option<(Vec<u8>, u32, u32)>>,
}

impl TauriShellHost {
    pub fn new(app: AppHandle) -> Arc<Self> {
        let quiet = under_test();
        let toasts: Arc<dyn Toasts> = if quiet {
            Arc::new(NoToasts)
        } else {
            let aumid = app.config().identifier.clone();
            match register_app_id(&aumid, "Helm", toast_icon_path().as_deref()) {
                Ok(()) => Arc::new(NativeToasts::new(&aumid)),
                Err(e) => {
                    log::warn!("toast app id: {e}");
                    Arc::new(NoToasts)
                }
            }
        };
        Arc::new(Self {
            app,
            tray: Mutex::new(None),
            hotkey: Mutex::new(None),
            service: OnceLock::new(),
            toasts,
            quiet,
            base_icon: Mutex::new(None),
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
        if let Some(icon) = self.app.default_window_icon() {
            *self.base_icon.lock().unwrap() =
                Some((icon.rgba().to_vec(), icon.width(), icon.height()));
        }
        // Clicks on a toast come back here while Helm runs.
        let me = self.clone();
        self.toasts
            .on_activated(Arc::new(move |a: Activation| me.on_toast_click(&a)));
        Ok(())
    }

    /// A click on a toast or one of its buttons.
    fn on_toast_click(&self, a: &Activation) {
        match parse_click(a) {
            Click::Open(chat) => self.open_chat(&chat, None),
            Click::Send { chat_id, text } => self.send_from_toast(&chat_id, &text),
            Click::Ignore => {}
        }
    }

    /// Reply and Continue: the user's own send, from a notification. It follows send behaviour
    /// like any send, so it queues behind a running turn.
    fn send_from_toast(&self, chat_id: &str, text: &str) {
        let Some(d) = self.app.try_state::<Arc<helm_ipc::Dispatcher>>() else {
            return;
        };
        let Some(rt) = d.ctx().services.get::<helm_chats::runtime::ChatRuntime>() else {
            return;
        };
        let id = submission_id();
        let sent = helm_chats::submit::host_send(
            &rt,
            helm_chats::submit::HostSend {
                chat_id,
                submission_id: &id,
                text,
                origin: helm_chats::types::SubmissionOrigin::Notification,
            },
        );
        if let Err(e) = sent {
            log::warn!("toast reply not sent: {}", e.message);
        }
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
                    report(s.toggle_keep_awake());
                }
            }
            "notif:native" => {
                if let Some(s) = svc {
                    report(s.toggle_notifications());
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
            &m.keep_awake_label,
            true,
            m.keep_awake,
            None::<&str>,
        )?)?;
        menu.append(&CheckMenuItem::with_id(
            app,
            "notif:native",
            "Windows notifications",
            true,
            m.notifications == NotificationMode::Native,
            None::<&str>,
        )?)?;
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

    fn set_zoom(&self, factor: f64) {
        if let Some(w) = self.window() {
            if let Err(e) = w.set_zoom(factor) {
                log::warn!("web view zoom: {e}");
            }
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

    fn post_toast(&self, spec: &ToastSpec) {
        if self.quiet {
            return;
        }
        let Err(why) = self.toasts.show(spec) else {
            return;
        };
        log::warn!("native toast: {why}; using the plain one");
        use tauri_plugin_notification::NotificationExt;
        if let Err(e) = self
            .app
            .notification()
            .builder()
            .title(spec.title.clone())
            .body(spec.body.clone())
            .show()
        {
            log::warn!("plain toast: {e}");
        }
    }

    fn withdraw_toast(&self, chat_id: &str) {
        if !self.quiet {
            self.toasts.withdraw(&tag_for(chat_id));
        }
    }

    fn set_tray_dot(&self, dot: TrayDot) {
        let Some((base, w, h)) = self.base_icon.lock().unwrap().clone() else {
            return;
        };
        let px = trayicon::compose(&base, w, h, dot);
        if let Some(t) = self.tray.lock().unwrap().as_ref() {
            if let Err(e) = t.set_icon(Some(tauri::image::Image::new_owned(px, w, h))) {
                log::warn!("tray icon: {e}");
            }
        }
    }

    fn register_hotkey(&self, chord: Option<&str>) -> Result<(), String> {
        let gs = self.app.global_shortcut();
        let mut held = self.hotkey.lock().unwrap();
        let Some(chord) = chord else {
            if let Some(old) = held.take() {
                let _ = gs.unregister(old);
            }
            return Ok(());
        };
        let sc: Shortcut = chord
            .parse()
            .map_err(|e| format!("{chord} is not a shortcut Windows accepts: {e}"))?;
        if *held == Some(sc) {
            return Ok(());
        }
        let old = held.take();
        if let Some(o) = old {
            let _ = gs.unregister(o);
        }
        if let Err(e) = gs.register(sc) {
            // Keep the previous chord working rather than leaving no hotkey at all.
            if let Some(o) = old {
                if gs.register(o).is_ok() {
                    *held = Some(o);
                }
            }
            return Err(format!(
                "{chord} could not be registered (another app may own it): {e}"
            ));
        }
        *held = Some(sc);
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
