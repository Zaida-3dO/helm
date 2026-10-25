//! The shell service: it watches `attention` and `completion` events on their way
//! to the renderer (the app's event tap calls [`ShellService::observe`]) and drives the taskbar
//! badge, the window flash, keep-awake, the tray menu and native toasts through a [`ShellHost`]
//! the app implements with Tauri. It works with the window hidden.

use crate::notify::{badge_label, want_keep_awake, Announcer, FlashTracker};
use crate::probe::Probe;
use crate::settings;
use crate::tray::{menu_model, TrayMenu};
use crate::types::{AudioInput, MicChoice, NotificationMode, ShellSettings, ShellSettingsView};
use helm_chats::types::{AttentionSummary, Completion};
use helm_core::db::Database;
use helm_ipc::AppEvent;
use helm_platform::{AudioInputs, CaptureDevice, PowerAssertions};
use serde_json::json;
use std::sync::{Arc, Mutex, OnceLock};

/// What the app does for the shell. Every call is quick and never blocks on the UI.
pub trait ShellHost: Send + Sync {
    /// The taskbar overlay: None clears it.
    fn set_badge(&self, label: Option<&str>);
    /// One taskbar flash (informational).
    fn flash(&self);
    /// Whether the main window is visible and focused.
    fn is_focused(&self) -> bool;
    /// Replace the tray's menu.
    fn set_tray(&self, menu: &TrayMenu);
    /// Post (or replace) the chat's Windows toast.
    fn post_toast(&self, c: &Completion);
    /// Withdraw the chat's Windows toast, if any.
    fn withdraw_toast(&self, chat_id: &str);
    /// Register the global hotkey (None unregisters). Err names why it failed.
    fn register_hotkey(&self, chord: Option<&str>) -> Result<(), String>;
    /// Zoom the main web view itself (1.0 is 100%), not the page's CSS.
    fn set_zoom(&self, factor: f64);
}

#[derive(Default)]
struct State {
    settings: ShellSettings,
    summary: Option<AttentionSummary>,
    announcer: Announcer,
    flash: FlashTracker,
    keep_awake_on: bool,
    keep_awake_error: Option<String>,
    hotkey_registered: bool,
    hotkey_error: Option<String>,
    shutting_down: bool,
    devices: Vec<CaptureDevice>,
    badge: Option<Option<String>>,
}

pub struct ShellService {
    db: Arc<Database>,
    power: Arc<dyn PowerAssertions>,
    audio: Arc<dyn AudioInputs>,
    probe: Probe,
    host: OnceLock<Arc<dyn ShellHost>>,
    state: Mutex<State>,
    clock: fn() -> i64,
}

impl ShellService {
    pub fn new(
        db: Arc<Database>,
        power: Arc<dyn PowerAssertions>,
        audio: Arc<dyn AudioInputs>,
        probe: Probe,
    ) -> Self {
        let settings = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(settings::load(tx)))
            .unwrap_or_default();
        Self {
            db,
            power,
            audio,
            probe,
            host: OnceLock::new(),
            state: Mutex::new(State {
                settings,
                ..Default::default()
            }),
            clock: helm_core::now_ms,
        }
    }

    #[cfg(test)]
    fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.clock = clock;
        self
    }

    /// Attach the app side once the Tauri app exists, then apply everything (hotkey, tray,
    /// badge, keep-awake) from the current state.
    pub fn attach(&self, host: Arc<dyn ShellHost>) {
        if self.host.set(host).is_err() {
            return;
        }
        self.refresh_devices();
        let chord = self.state.lock().unwrap().settings.global_hotkey.clone();
        self.apply_hotkey(chord.as_deref());
        let summary = self.state.lock().unwrap().summary.clone();
        if let Some(s) = summary {
            self.on_summary(s, true);
        } else {
            self.publish_tray();
        }
    }

    fn host(&self) -> Option<&Arc<dyn ShellHost>> {
        self.host.get()
    }

    /// The event tap: called for every event on its way to the renderer.
    pub fn observe(&self, event: &AppEvent) {
        match event.kind.as_str() {
            "attention" => {
                if let Some(s) = event
                    .fields
                    .get("summary")
                    .and_then(|v| serde_json::from_value::<AttentionSummary>(v.clone()).ok())
                {
                    self.on_summary(s, false);
                }
            }
            "completion" => {
                if let Some(c) = event
                    .fields
                    .get("completion")
                    .and_then(|v| serde_json::from_value::<Completion>(v.clone()).ok())
                {
                    self.on_completion(&c);
                }
            }
            _ => {}
        }
    }

    /// A new attention summary (also the launch summary, with `initial`).
    pub fn on_summary(&self, s: AttentionSummary, initial: bool) {
        let mut st = self.state.lock().unwrap();
        // Publishers emit outside their lock, so an older summary can arrive after a newer one.
        if st
            .summary
            .as_ref()
            .is_some_and(|old| s.revision < old.revision)
        {
            return;
        }
        let flash = st.flash.observe(&s) && !initial;
        let withdrawn = st.announcer.withdrawals(&s);
        let label = badge_label(s.badge);
        let badge_changed = st.badge.as_ref() != Some(&label);
        st.badge = Some(label.clone());
        st.summary = Some(s.clone());
        let mode = st.settings.notifications;
        drop(st);
        if badge_changed {
            if let Some(h) = self.host() {
                h.set_badge(label.as_deref());
            }
            self.probe.record("badge", json!({"count": s.badge}));
        }
        if flash {
            if let Some(h) = self.host() {
                if !h.is_focused() {
                    h.flash();
                    self.probe.record("flash", json!({}));
                }
            }
        }
        for chat in withdrawn {
            if mode == NotificationMode::Native {
                if let Some(h) = self.host() {
                    h.withdraw_toast(&chat);
                }
            }
            self.probe
                .record("toast_withdrawn", json!({"chatId": chat, "mode": mode}));
        }
        self.apply_keep_awake();
        self.publish_tray();
    }

    /// A newly announced completion: decide whether it gets a native toast. In-app toasts are the
    /// renderer's (it sees the same event); the decision here only covers native mode and the probe.
    pub fn on_completion(&self, c: &Completion) {
        let now = (self.clock)();
        let mut st = self.state.lock().unwrap();
        if !st.announcer.on_completion(c, now) {
            return;
        }
        let mode = st.settings.notifications;
        drop(st);
        if mode == NotificationMode::Native {
            if let Some(h) = self.host() {
                h.post_toast(c);
            }
        }
        self.probe
            .record("toast", json!({"chatId": c.chat_id, "mode": mode}));
    }

    fn apply_keep_awake(&self) {
        let mut st = self.state.lock().unwrap();
        let busy = st.summary.as_ref().map(|s| s.busy).unwrap_or(0);
        let want = want_keep_awake(busy, st.settings.keep_awake, st.shutting_down);
        // A failed request is retried on the next change: only a success moves keep_awake_on.
        if want == st.keep_awake_on && st.keep_awake_error.is_none() {
            return;
        }
        match self.power.set_keep_awake(want) {
            Ok(()) => {
                st.keep_awake_on = want;
                st.keep_awake_error = None;
                drop(st);
                self.probe.record("keep_awake", json!({"on": want}));
            }
            Err(e) => {
                if st.keep_awake_error.is_none() {
                    log::error!("keep-awake: {e}");
                }
                st.keep_awake_error = Some(e);
            }
        }
    }

    fn apply_hotkey(&self, chord: Option<&str>) {
        let Some(h) = self.host() else { return };
        let r = h.register_hotkey(chord);
        let mut st = self.state.lock().unwrap();
        match r {
            Ok(()) => {
                st.hotkey_registered = chord.is_some();
                st.hotkey_error = None;
            }
            Err(e) => {
                if st.hotkey_error.as_deref() != Some(e.as_str()) {
                    log::warn!("global hotkey: {e}");
                }
                st.hotkey_registered = false;
                st.hotkey_error = Some(e);
            }
        }
        let registered = st.hotkey_registered;
        drop(st);
        self.probe
            .record("hotkey", json!({"chord": chord, "registered": registered}));
    }

    fn refresh_devices(&self) {
        match self.audio.capture_devices() {
            Ok(d) => self.state.lock().unwrap().devices = d,
            Err(e) => log::warn!("{e}"),
        }
    }

    /// The tray menu now, re-reading the capture devices (the menu refreshes on every open).
    pub fn tray_menu(&self, refresh_devices: bool) -> TrayMenu {
        if refresh_devices {
            self.refresh_devices();
        }
        let st = self.state.lock().unwrap();
        menu_model(st.summary.as_ref(), &st.devices, &st.settings)
    }

    /// Rebuild the tray menu and record it.
    pub fn publish_tray(&self) {
        let menu = self.tray_menu(false);
        if let Some(h) = self.host() {
            h.set_tray(&menu);
        }
        self.probe.record(
            "tray_menu",
            json!({"groups": menu.groups, "mics": menu.mics}),
        );
    }

    /// The settings plus live status, for Settings → Shell.
    pub fn view(&self) -> ShellSettingsView {
        let st = self.state.lock().unwrap();
        ShellSettingsView {
            settings: st.settings.clone(),
            hotkey_registered: st.hotkey_registered,
            hotkey_error: st.hotkey_error.clone(),
            keep_awake_error: st.keep_awake_error.clone(),
        }
    }

    /// Validate, store and apply new settings.
    pub fn save(&self, next: ShellSettings) -> Result<ShellSettingsView, String> {
        // A new microphone choice must name a connected device; keeping a stored choice that has
        // since been unplugged is fine.
        if let Some(m) = &next.dictation_mic {
            let st = self.state.lock().unwrap();
            let unchanged = st
                .settings
                .dictation_mic
                .as_ref()
                .is_some_and(|p| p.uid == m.uid);
            if !unchanged && !st.devices.iter().any(|d| d.id == m.uid) {
                return Err("that microphone is not connected".into());
            }
        }
        let stored = self
            .db
            .write(|tx| Ok::<_, rusqlite::Error>(settings::save(tx, &next)))
            .map_err(|e| e.to_string())??;
        let mut st = self.state.lock().unwrap();
        let prev = std::mem::replace(&mut st.settings, stored.clone());
        let withdraw = if prev.notifications == NotificationMode::Native
            && stored.notifications != NotificationMode::Native
        {
            st.announcer.withdraw_all()
        } else {
            Vec::new()
        };
        drop(st);
        for chat in withdraw {
            if let Some(h) = self.host() {
                h.withdraw_toast(&chat);
            }
        }
        if prev.global_hotkey != stored.global_hotkey {
            self.apply_hotkey(stored.global_hotkey.as_deref());
        }
        self.apply_keep_awake();
        self.publish_tray();
        Ok(self.view())
    }

    /// The tray's Microphone pick: None is Automatic. The name comes from the device list, so a
    /// uid that is not connected is refused (and the OS default is never written as a choice).
    pub fn pick_mic(&self, id: Option<&str>) -> Result<(), String> {
        let r = self.pick_mic_inner(id);
        self.retick_on_error(r)
    }

    /// The tray's check items tick themselves when clicked; after a failed change, redraw the
    /// menu so the ticks show what is really applied.
    fn retick_on_error(&self, r: Result<(), String>) -> Result<(), String> {
        if r.is_err() {
            self.publish_tray();
        }
        r
    }

    fn pick_mic_inner(&self, id: Option<&str>) -> Result<(), String> {
        let mut next = self.state.lock().unwrap().settings.clone();
        next.dictation_mic = match id {
            None => None,
            Some(id) => {
                let devices = self.state.lock().unwrap().devices.clone();
                let name = devices
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.name.clone())
                    .or_else(|| {
                        next.dictation_mic
                            .as_ref()
                            .filter(|m| m.uid == id)
                            .map(|m| m.name.clone())
                    })
                    .ok_or_else(|| "that microphone is no longer connected".to_owned())?;
                Some(MicChoice {
                    uid: id.to_owned(),
                    name,
                })
            }
        };
        self.save(next).map(|_| ())
    }

    pub fn set_keep_awake_setting(&self, on: bool) -> Result<(), String> {
        let mut next = self.state.lock().unwrap().settings.clone();
        next.keep_awake = on;
        let r = self.save(next).map(|_| ());
        self.retick_on_error(r)
    }

    pub fn set_notification_mode(&self, mode: NotificationMode) -> Result<(), String> {
        let mut next = self.state.lock().unwrap().settings.clone();
        next.notifications = mode;
        let r = self.save(next).map(|_| ());
        self.retick_on_error(r)
    }

    /// The capture devices for the picker, plus the stored choice when its device is gone.
    pub fn audio_inputs(&self) -> Vec<AudioInput> {
        self.refresh_devices();
        let st = self.state.lock().unwrap();
        let mut out: Vec<AudioInput> = st
            .devices
            .iter()
            .map(|d| AudioInput {
                id: d.id.clone(),
                name: d.name.clone(),
                is_default: d.is_default,
                available: true,
            })
            .collect();
        if let Some(m) = &st.settings.dictation_mic {
            if !out.iter().any(|a| a.id == m.uid) {
                out.push(AudioInput {
                    id: m.uid.clone(),
                    name: m.name.clone(),
                    is_default: false,
                    available: false,
                });
            }
        }
        out
    }

    /// Quit: release keep-awake and never take it again.
    pub fn shutdown(&self) {
        self.state.lock().unwrap().shutting_down = true;
        self.apply_keep_awake();
    }

    /// Settings → General → Zoom: a level of 50-200 in steps of 10, applied to the web view.
    pub fn set_zoom(&self, level: i64) -> Result<(), String> {
        if !(50..=200).contains(&level) || level % 10 != 0 {
            return Err("Zoom is 50 to 200 percent in steps of 10.".into());
        }
        if let Some(h) = self.host() {
            h.set_zoom(level as f64 / 100.0);
        }
        Ok(())
    }

    pub fn settings(&self) -> ShellSettings {
        self.state.lock().unwrap().settings.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_chats::types::{AttentionKind, AttentionRow};
    use helm_core::db::MigrationRegistry;
    use helm_platform::audio::FixedAudio;
    use helm_platform::power::NativePower;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct FakeHost {
        log: Mutex<Vec<String>>,
        focused: AtomicBool,
        refuse_hotkey: AtomicBool,
    }
    impl ShellHost for FakeHost {
        fn set_badge(&self, label: Option<&str>) {
            self.log
                .lock()
                .unwrap()
                .push(format!("badge {}", label.unwrap_or("-")));
        }
        fn flash(&self) {
            self.log.lock().unwrap().push("flash".into());
        }
        fn is_focused(&self) -> bool {
            self.focused.load(Ordering::SeqCst)
        }
        fn set_tray(&self, m: &TrayMenu) {
            self.log
                .lock()
                .unwrap()
                .push(format!("tray {} {}", m.groups.len(), m.mics.len()));
        }
        fn post_toast(&self, c: &Completion) {
            self.log
                .lock()
                .unwrap()
                .push(format!("toast {}", c.chat_id));
        }
        fn withdraw_toast(&self, chat: &str) {
            self.log.lock().unwrap().push(format!("withdraw {chat}"));
        }
        fn set_zoom(&self, factor: f64) {
            self.log.lock().unwrap().push(format!("zoom {factor}"));
        }
        fn register_hotkey(&self, chord: Option<&str>) -> Result<(), String> {
            if self.refuse_hotkey.load(Ordering::SeqCst) {
                return Err("taken".into());
            }
            self.log
                .lock()
                .unwrap()
                .push(format!("hotkey {}", chord.unwrap_or("-")));
            Ok(())
        }
    }
    impl FakeHost {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut self.log.lock().unwrap())
        }
    }

    struct Rig {
        _dir: tempfile::TempDir,
        svc: ShellService,
        host: Arc<FakeHost>,
        power: Arc<NativePower>,
        probe: std::path::PathBuf,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            Database::open(&dir.path().join("t.sqlite3"), &MigrationRegistry::new()).unwrap(),
        );
        let power = Arc::new(NativePower::new());
        let audio = Arc::new(FixedAudio(vec![CaptureDevice {
            id: "m1".into(),
            name: "USB mic".into(),
            is_default: true,
        }]));
        let probe = dir.path().join("probe.jsonl");
        let svc = ShellService::new(db, power.clone(), audio, Probe::at(Some(probe.clone())))
            .with_clock(|| 100_000);
        let host = Arc::new(FakeHost::default());
        svc.attach(host.clone());
        Rig {
            _dir: dir,
            svc,
            host,
            power,
            probe,
        }
    }

    fn summary(badge: u32, busy: u32, unread: &[&str], needs: &[&str]) -> AttentionSummary {
        let row = |c: &str, k| AttentionRow {
            chat_id: c.into(),
            title: "T".into(),
            project: "P".into(),
            kind: k,
            reason: String::new(),
        };
        AttentionSummary {
            badge,
            busy,
            needs_you: needs
                .iter()
                .map(|c| row(c, AttentionKind::Approval))
                .collect(),
            running: vec![],
            unread: unread
                .iter()
                .map(|c| row(c, AttentionKind::Unread))
                .collect(),
            stalled: vec![],
            flagged: vec![],
            revision: 1,
        }
    }

    fn completion(chat: &str) -> Completion {
        Completion {
            id: format!("k-{chat}"),
            chat_id: chat.into(),
            title: "T".into(),
            project: "P".into(),
            preview: "done".into(),
            completed_at: 99_000,
            read: false,
            dismissed: false,
        }
    }

    fn probes(r: &Rig, kind: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&r.probe)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["probe"] == kind)
            .collect()
    }

    fn event(kind: &str, key: &str, v: impl serde::Serialize) -> AppEvent {
        AppEvent::new(kind).with(key, serde_json::to_value(v).unwrap())
    }

    #[test]
    fn attach_registers_the_default_hotkey_and_builds_the_tray() {
        let r = rig();
        let log = r.host.take();
        assert!(log.contains(&"hotkey Ctrl+Alt+H".to_owned()), "{log:?}");
        assert!(
            log.iter().any(|l| l == "tray 0 2"),
            "Automatic + one mic: {log:?}"
        );
        assert!(r.svc.view().hotkey_registered);
        let h = probes(&r, "hotkey");
        assert_eq!(h[0]["registered"], true);
        assert_eq!(probes(&r, "tray_menu")[0]["mics"][0]["label"], "Automatic");
    }

    #[test]
    fn a_taken_hotkey_is_reported_not_fatal() {
        let r = rig();
        r.host.refuse_hotkey.store(true, Ordering::SeqCst);
        let mut s = r.svc.settings();
        s.global_hotkey = Some("Ctrl+Alt+J".into());
        let v = r.svc.save(s).unwrap();
        assert!(!v.hotkey_registered);
        assert_eq!(v.hotkey_error.as_deref(), Some("taken"));
    }

    #[test]
    fn an_older_summary_arriving_late_is_dropped() {
        let r = rig();
        r.host.take();
        let mut newer = summary(3, 0, &["a", "b", "c"], &[]);
        newer.revision = 5;
        let mut older = summary(0, 0, &[], &[]);
        older.revision = 4;
        r.svc.observe(&event("attention", "summary", newer));
        r.svc.observe(&event("attention", "summary", older));
        let badges: Vec<_> = r
            .host
            .take()
            .into_iter()
            .filter(|l| l.starts_with("badge"))
            .collect();
        assert_eq!(badges, ["badge 3"], "the stale summary changed nothing");
    }

    #[test]
    fn a_failed_tray_change_redraws_the_menu() {
        let r = rig();
        r.host.take();
        assert!(r.svc.pick_mic(Some("gone")).is_err());
        assert!(
            r.host.take().iter().any(|l| l.starts_with("tray ")),
            "the menu is republished so its ticks show the applied state"
        );
    }

    #[test]
    fn badge_follows_the_summary_and_is_only_set_on_change() {
        let r = rig();
        r.host.take();
        r.svc
            .observe(&event("attention", "summary", summary(0, 0, &[], &[])));
        r.svc
            .observe(&event("attention", "summary", summary(1, 0, &["c1"], &[])));
        r.svc
            .observe(&event("attention", "summary", summary(1, 0, &["c1"], &[])));
        r.svc
            .observe(&event("attention", "summary", summary(12, 0, &["c1"], &[])));
        r.svc
            .observe(&event("attention", "summary", summary(0, 0, &[], &[])));
        let badges: Vec<_> = r
            .host
            .take()
            .into_iter()
            .filter(|l| l.starts_with("badge"))
            .collect();
        assert_eq!(badges, ["badge -", "badge 1", "badge 9+", "badge -"]);
        let counts: Vec<_> = probes(&r, "badge")
            .iter()
            .map(|v| v["count"].as_u64().unwrap())
            .collect();
        assert_eq!(counts, [0, 1, 12, 0]);
    }

    #[test]
    fn keep_awake_follows_busy_and_the_setting() {
        let r = rig();
        r.svc
            .observe(&event("attention", "summary", summary(0, 2, &[], &[])));
        assert!(r.power.is_on());
        r.svc.set_keep_awake_setting(false).unwrap();
        assert!(!r.power.is_on(), "toggling applies at once");
        r.svc.set_keep_awake_setting(true).unwrap();
        assert!(r.power.is_on());
        r.svc
            .observe(&event("attention", "summary", summary(0, 0, &[], &[])));
        assert!(!r.power.is_on());
        r.svc
            .observe(&event("attention", "summary", summary(0, 1, &[], &[])));
        r.svc.shutdown();
        assert!(!r.power.is_on(), "released at shutdown");
        r.svc
            .observe(&event("attention", "summary", summary(0, 1, &[], &[])));
        assert!(!r.power.is_on(), "never re-taken after shutdown");
        let ons: Vec<_> = probes(&r, "keep_awake")
            .iter()
            .map(|v| v["on"].as_bool().unwrap())
            .collect();
        assert_eq!(ons, [true, false, true, false, true, false]);
    }

    #[test]
    fn one_flash_per_new_blocking_request_only_in_the_background() {
        let r = rig();
        r.svc
            .observe(&event("attention", "summary", summary(0, 0, &[], &[])));
        r.host.take();
        r.svc
            .observe(&event("attention", "summary", summary(1, 0, &[], &["c1"])));
        r.svc
            .observe(&event("attention", "summary", summary(1, 0, &[], &["c1"])));
        r.host.focused.store(true, Ordering::SeqCst);
        r.svc.observe(&event(
            "attention",
            "summary",
            summary(2, 0, &[], &["c1", "c2"]),
        ));
        let flashes = r.host.take().into_iter().filter(|l| l == "flash").count();
        assert_eq!(flashes, 1);
    }

    #[test]
    fn in_app_mode_posts_no_native_toast_but_records_the_decision() {
        let r = rig();
        r.host.take();
        r.svc
            .observe(&event("completion", "completion", completion("c1")));
        assert!(!r.host.take().iter().any(|l| l.starts_with("toast")));
        let t = probes(&r, "toast");
        assert_eq!(t[0]["chatId"], "c1");
        assert_eq!(t[0]["mode"], "in_app");
    }

    #[test]
    fn native_mode_posts_once_and_withdraws_on_read() {
        let r = rig();
        r.svc
            .set_notification_mode(NotificationMode::Native)
            .unwrap();
        r.svc
            .observe(&event("attention", "summary", summary(1, 0, &["c1"], &[])));
        r.host.take();
        r.svc
            .observe(&event("completion", "completion", completion("c1")));
        r.svc
            .observe(&event("completion", "completion", completion("c1")));
        r.svc
            .observe(&event("attention", "summary", summary(0, 0, &[], &[])));
        assert_eq!(
            r.host
                .take()
                .into_iter()
                .filter(|l| l.starts_with("toast") || l.starts_with("withdraw"))
                .collect::<Vec<_>>(),
            ["toast c1", "withdraw c1"]
        );
    }

    #[test]
    fn switching_away_from_native_withdraws_what_was_posted() {
        let r = rig();
        r.svc
            .set_notification_mode(NotificationMode::Native)
            .unwrap();
        r.svc
            .observe(&event("completion", "completion", completion("c1")));
        r.host.take();
        r.svc
            .set_notification_mode(NotificationMode::InApp)
            .unwrap();
        assert!(r.host.take().contains(&"withdraw c1".to_owned()));
    }

    #[test]
    fn picking_a_mic_persists_it_and_unknown_ids_are_refused() {
        let r = rig();
        r.svc.pick_mic(Some("m1")).unwrap();
        assert_eq!(r.svc.settings().dictation_mic.unwrap().name, "USB mic");
        let ticked: Vec<_> = r
            .svc
            .tray_menu(false)
            .mics
            .into_iter()
            .filter(|m| m.checked)
            .collect();
        assert_eq!(ticked[0].id.as_deref(), Some("m1"));
        assert!(r.svc.pick_mic(Some("nope")).is_err());
        r.svc.pick_mic(None).unwrap();
        assert!(r.svc.settings().dictation_mic.is_none());
        let stored = r
            .svc
            .db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(settings::load(tx)))
            .unwrap();
        assert!(stored.dictation_mic.is_none());
    }

    #[test]
    fn audio_inputs_keep_a_missing_choice_as_unavailable() {
        let r = rig();
        let gone = ShellSettings {
            dictation_mic: Some(MicChoice {
                uid: "gone".into(),
                name: "Headset".into(),
            }),
            ..Default::default()
        };
        // Stored while it was connected; the device has since gone.
        r.svc
            .db
            .write(|tx| Ok::<_, rusqlite::Error>(settings::save(tx, &gone)))
            .unwrap()
            .unwrap();
        let svc = ShellService::new(
            r.svc.db.clone(),
            r.power.clone(),
            Arc::new(FixedAudio(vec![CaptureDevice {
                id: "m1".into(),
                name: "USB mic".into(),
                is_default: true,
            }])),
            Probe::disabled(),
        );
        let list = svc.audio_inputs();
        assert_eq!(list.len(), 2);
        assert!(list[0].available && list[0].is_default);
        assert!(!list[1].available);
        assert_eq!(list[1].name, "Headset");
        // Re-saving with the unplugged choice unchanged is fine; a new unknown uid is refused.
        assert!(svc.save(svc.settings()).is_ok());
        let mut other = svc.settings();
        other.dictation_mic = Some(MicChoice {
            uid: "other".into(),
            name: "X".into(),
        });
        assert!(svc.save(other).is_err());
    }

    #[test]
    fn zoom_reaches_the_web_view_and_only_in_range() {
        let r = rig();
        r.host.take();
        r.svc.set_zoom(120).unwrap();
        r.svc.set_zoom(50).unwrap();
        assert_eq!(r.host.take(), ["zoom 1.2", "zoom 0.5"]);
        for bad in [40, 210, 125, 0] {
            assert!(r.svc.set_zoom(bad).is_err(), "{bad}");
        }
        assert!(r.host.take().is_empty(), "a refused level changes nothing");
    }
}
