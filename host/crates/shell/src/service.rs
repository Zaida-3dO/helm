//! The shell service: it watches `attention`, `completion`, `schedules-wake` (a saved schedule
//! coming due) and `preferences` events on their way
//! to the renderer (the app's event tap calls [`ShellService::observe`]) and drives the taskbar
//! badge, the window flash, keep-awake, the tray menu and its state dot, and native toasts through
//! a [`ShellHost`] the app implements with Tauri. It works with the window hidden.
//!
//! Native toasts are opt-in (Settings, Shell). With them on, a finished turn and a chat that
//! starts needing the user each raise one, unless the chat is muted or Helm is the window in
//! front, where the flags already say so.

use crate::notify::{badge_label, want_keep_awake, Announcer, FlashTracker};
use crate::probe::Probe;
use crate::settings;
use crate::toasts::{finished_toast, needs_you_toast, tray_dot, NeedsTracker};
use crate::tray::{menu_model, TrayMenu};
use crate::types::{
    AudioInput, KeepAwake, MicChoice, NotificationMode, ShellSettings, ShellSettingsView,
};
use helm_chats::types::{AttentionSummary, Completion};
use helm_core::db::Database;
use helm_ipc::AppEvent;
use helm_platform::toast::ToastSpec;
use helm_platform::trayicon::TrayDot;
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
    /// Post (or replace) a Windows toast. The spec's tag is the chat's, so a chat has one.
    fn post_toast(&self, spec: &ToastSpec);
    /// Withdraw the chat's Windows toast, if any.
    fn withdraw_toast(&self, chat_id: &str);
    /// Draw the tray icon with this state dot.
    fn set_tray_dot(&self, dot: TrayDot);
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
    needs: NeedsTracker,
    dot: TrayDot,
    flash: FlashTracker,
    keep_awake_on: bool,
    keep_awake_error: Option<String>,
    /// A saved schedule is close to running (from the scheduler's wake transitions).
    due_soon: bool,
    /// The last mode that was not off, so the tray toggle can restore it.
    last_keep_awake: KeepAwake,
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
        // The launch summary was taken before the app side existed: show its dot now.
        let dot = self.state.lock().unwrap().dot;
        if dot != TrayDot::None {
            if let Some(h) = self.host() {
                h.set_tray_dot(dot);
            }
        }
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
            "schedules-wake" => {
                let Some(due) = event.fields.get("dueSoon").and_then(|v| v.as_bool()) else {
                    return;
                };
                if std::mem::replace(&mut self.state.lock().unwrap().due_soon, due) != due {
                    self.apply_keep_awake();
                }
            }
            "preferences" => {
                // A changed preference row for the mode (the agent-facing settings tool writes
                // it directly): adopt it and apply, rather than waiting for a restart.
                if !event
                    .fields
                    .get("changed")
                    .is_some_and(|c| c.get(settings::KEEP_AWAKE.name()).is_some())
                {
                    return;
                }
                let Ok(loaded) = self
                    .db
                    .snapshot(|tx| Ok::<_, rusqlite::Error>(settings::load(tx)))
                else {
                    return;
                };
                let mode = loaded.keep_awake;
                {
                    let mut st = self.state.lock().unwrap();
                    st.settings.keep_awake = mode;
                    if mode != KeepAwake::Off {
                        st.last_keep_awake = mode;
                    }
                }
                self.apply_keep_awake();
                self.publish_tray();
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
        let asking = st.needs.fresh(&s);
        let answered = st.needs.resolved(&s);
        let dot = tray_dot(&s);
        let dot_changed = std::mem::replace(&mut st.dot, dot) != dot;
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
        if dot_changed {
            if let Some(h) = self.host() {
                h.set_tray_dot(dot);
            }
            self.probe.record("tray_dot", json!({"dot": dot.as_str()}));
        }
        if flash {
            if let Some(h) = self.host() {
                if !h.is_focused() {
                    h.flash();
                    self.probe.record("flash", json!({}));
                }
            }
        }
        for chat in withdrawn.into_iter().chain(answered) {
            if mode == NotificationMode::Native {
                if let Some(h) = self.host() {
                    h.withdraw_toast(&chat);
                }
            }
            self.probe
                .record("toast_withdrawn", json!({"chatId": chat, "mode": mode}));
        }
        // A chat that has just started needing the user. Never answered from the toast: it only
        // opens the chat.
        if !initial {
            for row in asking {
                let shown = self.toast(mode, &row.chat_id, || needs_you_toast(&row));
                if shown {
                    self.state.lock().unwrap().needs.posted(&row.chat_id);
                }
                self.probe.record(
                    "toast",
                    json!({"chatId": row.chat_id, "mode": mode, "kind": "needs_you", "shown": shown}),
                );
            }
        }
        self.apply_keep_awake();
        self.publish_tray();
    }

    /// A newly announced completion: decide whether it gets a native toast. The decision here only
    /// covers native mode and the probe; a finished chat is also in the attention views.
    pub fn on_completion(&self, c: &Completion) {
        let now = (self.clock)();
        let mut st = self.state.lock().unwrap();
        if !st.announcer.on_completion(c, now) {
            return;
        }
        let mode = st.settings.notifications;
        drop(st);
        let shown = self.toast(mode, &c.chat_id, || finished_toast(c));
        self.probe.record(
            "toast",
            json!({"chatId": c.chat_id, "mode": mode, "kind": "finished", "shown": shown}),
        );
    }

    /// Post the toast `spec` builds when toasts are on, the chat is not muted and Helm is not the
    /// window in front. True when it was posted.
    fn toast(
        &self,
        mode: NotificationMode,
        chat_id: &str,
        spec: impl FnOnce() -> ToastSpec,
    ) -> bool {
        if mode != NotificationMode::Native || self.is_muted(chat_id) {
            return false;
        }
        let Some(h) = self.host() else { return false };
        if h.is_focused() {
            return false;
        }
        h.post_toast(&spec());
        true
    }

    /// Whether the chat is muted. A muted chat never raises a toast, whatever path asks.
    fn is_muted(&self, chat_id: &str) -> bool {
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM chat_mutes WHERE chat_id = ?1)",
                    [chat_id],
                    |r| r.get::<_, bool>(0),
                )
            })
            .unwrap_or(false)
    }

    fn apply_keep_awake(&self) {
        let mut st = self.state.lock().unwrap();
        let busy = st.summary.as_ref().map(|s| s.busy).unwrap_or(0);
        let want = want_keep_awake(busy, st.settings.keep_awake, st.due_soon, st.shutting_down);
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
        if stored.keep_awake != KeepAwake::Off {
            st.last_keep_awake = stored.keep_awake;
        }
        let prev = std::mem::replace(&mut st.settings, stored.clone());
        let withdraw = if prev.notifications == NotificationMode::Native
            && stored.notifications != NotificationMode::Native
        {
            let mut all = st.announcer.withdraw_all();
            all.extend(st.needs.withdraw_all());
            all.sort();
            all.dedup();
            all
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

    /// The tray's keep-awake check item: off, or back to the mode that was on before it was off.
    pub fn toggle_keep_awake(&self) -> Result<(), String> {
        let (current, remembered) = {
            let st = self.state.lock().unwrap();
            (st.settings.keep_awake, st.last_keep_awake)
        };
        let mode = if current == KeepAwake::Off {
            remembered
        } else {
            KeepAwake::Off
        };
        let mut next = self.state.lock().unwrap().settings.clone();
        next.keep_awake = mode;
        let r = self.save(next).map(|_| ());
        self.retick_on_error(r)
    }

    /// The tray's Windows notifications check item: on, or back off.
    pub fn toggle_notifications(&self) -> Result<(), String> {
        let now = self.state.lock().unwrap().settings.notifications;
        self.set_notification_mode(if now == NotificationMode::Native {
            NotificationMode::Off
        } else {
            NotificationMode::Native
        })
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
        specs: Mutex<Vec<ToastSpec>>,
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
        fn post_toast(&self, spec: &ToastSpec) {
            let chat = spec.launch.strip_prefix("open:").unwrap_or(&spec.launch);
            self.log.lock().unwrap().push(format!("toast {chat}"));
            self.specs.lock().unwrap().push(spec.clone());
        }
        fn set_tray_dot(&self, dot: TrayDot) {
            self.log
                .lock()
                .unwrap()
                .push(format!("dot {}", dot.as_str()));
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
            muted: vec![],
            triage: vec![],
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
        r.svc.toggle_keep_awake().unwrap();
        assert!(!r.power.is_on(), "toggling applies at once");
        r.svc.toggle_keep_awake().unwrap();
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
    fn the_mode_a_due_run_and_an_agent_setting_all_keep_the_pc_awake() {
        let r = rig();
        let mut s = r.svc.settings();
        // Always: idle chats keep it on; off releases again; the tray restores the remembered mode.
        s.keep_awake = KeepAwake::Always;
        r.svc.save(s.clone()).unwrap();
        assert!(r.power.is_on(), "always, while open");
        r.svc.toggle_keep_awake().unwrap();
        assert!(!r.power.is_on());
        r.svc.toggle_keep_awake().unwrap();
        assert_eq!(r.svc.settings().keep_awake, KeepAwake::Always);
        assert!(r.power.is_on(), "the tray restores the mode it turned off");
        // Back to busy: idle again.
        let mut busy_mode = r.svc.settings();
        busy_mode.keep_awake = KeepAwake::Busy;
        r.svc.save(busy_mode).unwrap();
        assert!(
            !r.power.is_on(),
            "idle and busy mode: nothing to keep awake for"
        );
        // A saved schedule coming due, reported by the scheduler's tick.
        r.svc
            .observe(&event("schedules-wake", "dueSoon", json!(true)));
        assert!(r.power.is_on(), "awake ahead of the run");
        r.svc
            .observe(&event("schedules-wake", "dueSoon", json!(false)));
        assert!(!r.power.is_on());
        // The tray menu names the mode that is on.
        let menu = r.svc.tray_menu(false);
        assert_eq!(menu.keep_awake_label, "Keep awake while chats run");
        assert!(menu.keep_awake);
        // An agent-facing settings write lands as a preference event and is applied at once.
        set_keep_awake_pref(&r, "off");
        assert!(!r.power.is_on(), "the adopted mode took effect");
        assert_eq!(r.svc.settings().keep_awake, KeepAwake::Off);
        assert!(!r.svc.tray_menu(false).keep_awake, "unticked");
    }

    /// Stand-in for the preference op: write the row, then emit the event the op sends.
    fn set_keep_awake_pref(r: &Rig, value: &str) {
        r.svc
            .db
            .write(|tx| {
                helm_core::prefs::set(tx, settings::KEEP_AWAKE, &json!(value))
                    .map_err(|_| rusqlite::Error::InvalidQuery)
            })
            .unwrap();
        r.svc.observe(&event(
            "preferences",
            "changed",
            json!({ settings::KEEP_AWAKE.name(): value }),
        ));
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
    fn with_toasts_off_nothing_is_posted_but_the_decision_is_recorded() {
        let r = rig();
        assert_eq!(
            r.svc.settings().notifications,
            NotificationMode::Off,
            "opt-in"
        );
        r.host.take();
        r.svc
            .observe(&event("completion", "completion", completion("c1")));
        assert!(!r.host.take().iter().any(|l| l.starts_with("toast")));
        let t = probes(&r, "toast");
        assert_eq!(t[0]["chatId"], "c1");
        assert_eq!(t[0]["mode"], "off");
        assert_eq!(t[0]["shown"], false);
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
        r.svc.set_notification_mode(NotificationMode::Off).unwrap();
        assert!(r.host.take().contains(&"withdraw c1".to_owned()));
    }

    /// Mark chats muted the way the chats crate does (its table; the shell test database has none).
    fn mute(r: &Rig, chats: &[&str]) {
        r.svc
            .db
            .write(|tx| {
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS chat_mutes (chat_id TEXT PRIMARY KEY, muted_at INTEGER NOT NULL)",
                )?;
                for c in chats {
                    tx.execute(
                        "INSERT OR IGNORE INTO chat_mutes(chat_id, muted_at) VALUES (?1, 1)",
                        [c],
                    )?;
                }
                Ok::<_, rusqlite::Error>(())
            })
            .unwrap();
    }

    /// Which things toast: a finished turn (reply, continue, open) and a chat that starts needing
    /// the user (open only), in Windows mode only, never for a muted chat, never while Helm is the
    /// window in front, and never for a request that was already waiting at launch.
    #[test]
    fn native_toasts_cover_finished_and_needs_you_and_never_muted_or_focused_chats() {
        let r = rig();
        mute(&r, &["m1"]);
        // Off: nothing, whatever happens.
        r.svc
            .observe(&event("attention", "summary", summary(1, 0, &[], &["w0"])));
        r.svc.observe(&event(
            "attention",
            "summary",
            summary(2, 0, &[], &["w0", "off1"]),
        ));
        r.svc
            .observe(&event("completion", "completion", completion("off2")));
        assert!(!r.host.take().iter().any(|l| l.starts_with("toast")));

        r.svc
            .set_notification_mode(NotificationMode::Native)
            .unwrap();
        r.host.take();
        // A finished turn: title, reply box and two buttons.
        r.svc
            .observe(&event("completion", "completion", completion("c1")));
        // A new request: Open only. w0 was already waiting, so it raises nothing.
        r.svc.observe(&event(
            "attention",
            "summary",
            summary(3, 0, &["c1"], &["w0", "off1", "c2"]),
        ));
        // A muted chat: neither kind.
        r.svc
            .observe(&event("completion", "completion", completion("m1")));
        r.svc.observe(&event(
            "attention",
            "summary",
            summary(4, 0, &["c1"], &["w0", "off1", "c2", "m1"]),
        ));
        // Helm in front: the flags already say so.
        r.host.focused.store(true, Ordering::SeqCst);
        r.svc
            .observe(&event("completion", "completion", completion("f1")));
        r.svc.observe(&event(
            "attention",
            "summary",
            summary(5, 0, &["c1"], &["w0", "off1", "c2", "m1", "f2"]),
        ));
        let log = r.host.take();
        let toasts: Vec<&String> = log.iter().filter(|l| l.starts_with("toast")).collect();
        assert_eq!(toasts, ["toast c1", "toast c2"], "{log:?}");
        let specs = r.host.specs.lock().unwrap().clone();
        assert_eq!(specs[0].title, "T");
        assert_eq!(
            specs[0].reply_box.as_ref().map(|b| b.arguments.as_str()),
            Some("reply:c1")
        );
        assert_eq!(
            specs[0]
                .actions
                .iter()
                .map(|a| a.label.as_str())
                .collect::<Vec<_>>(),
            ["Continue", "Open"]
        );
        assert_eq!(specs[1].title, "T needs you");
        assert_eq!(
            specs[1]
                .actions
                .iter()
                .map(|a| a.label.as_str())
                .collect::<Vec<_>>(),
            ["Open"]
        );
        assert!(
            specs[1].reply_box.is_none(),
            "nothing is answered from a toast"
        );
        let shown: Vec<(String, bool)> = probes(&r, "toast")
            .iter()
            .filter(|p| p["mode"] == "native")
            .map(|p| {
                (
                    p["chatId"].as_str().unwrap().to_owned(),
                    p["shown"].as_bool().unwrap(),
                )
            })
            .collect();
        assert!(shown.contains(&("m1".to_owned(), false)));
        assert!(shown.contains(&("f1".to_owned(), false)));
        // Answering the request takes its toast back.
        r.host.focused.store(false, Ordering::SeqCst);
        r.svc.observe(&event(
            "attention",
            "summary",
            summary(4, 0, &["c1"], &["w0", "off1", "m1", "f2"]),
        ));
        assert!(r.host.take().contains(&"withdraw c2".to_owned()));
    }

    #[test]
    fn the_tray_dot_follows_what_needs_the_user_and_what_runs() {
        let r = rig();
        r.host.take();
        let running = |n: usize| {
            let mut s = summary(0, n as u32, &[], &[]);
            s.running = (0..n)
                .map(|i| AttentionRow {
                    chat_id: format!("r{i}"),
                    title: "T".into(),
                    project: "P".into(),
                    kind: AttentionKind::Running,
                    reason: String::new(),
                })
                .collect();
            s
        };
        for s in [
            running(0),
            running(2),
            running(1),
            summary(1, 0, &[], &["c1"]),
            summary(0, 0, &[], &[]),
        ] {
            r.svc.observe(&event("attention", "summary", s));
        }
        let dots: Vec<String> = r
            .host
            .take()
            .into_iter()
            .filter(|l| l.starts_with("dot"))
            .collect();
        assert_eq!(
            dots,
            ["dot amber", "dot red", "dot none"],
            "only changes are drawn"
        );
        let probed: Vec<String> = probes(&r, "tray_dot")
            .iter()
            .map(|p| p["dot"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(probed, ["amber", "red", "none"]);
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
