//! Native Windows toasts.
//!
//! Helm registers an app id under the current user's classes (no administrator rights, no
//! installer step), shows toasts under it, and hears clicks through the toast's own in-process
//! `Activated` event. That works while Helm runs, which it does from the tray; a click on a toast
//! that outlived Helm only opens the Start menu entry, because Helm registers no COM activator.
//!
//! Everything WinRT happens on one thread of its own that joins the multithreaded apartment for
//! the life of the process, so the click handlers have somewhere to run. Everything that can be
//! decided without Windows (the tag, the markup, what a button sends back) is a plain function
//! here and is tested everywhere. On other systems nothing is shown and [`Toasts::show`] says so,
//! so the caller can fall back.

use std::path::Path;
use std::sync::{Arc, Mutex};

/// The notification group every chat toast is filed in.
pub const GROUP: &str = "chats";
/// The id of the reply box in the markup; the click hands back whatever was typed there.
pub const REPLY_INPUT_ID: &str = "reply";
/// Toasts remembered so their click handlers stay alive.
pub const MAX_LIVE: usize = 64;

/// One button under the toast. `arguments` come back untouched in [`Activation`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastAction {
    pub label: String,
    pub arguments: String,
}

/// A text box on the toast with its own Send button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyBox {
    pub placeholder: String,
    pub send_label: String,
    /// What a press of Send hands back; the typed text arrives as [`Activation::reply_text`].
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastSpec {
    /// Replaces a toast with the same tag; see [`tag_for`].
    pub tag: String,
    pub group: String,
    pub title: String,
    pub body: String,
    pub actions: Vec<ToastAction>,
    pub reply_box: Option<ReplyBox>,
    /// What a click on the toast itself (not a button) hands back.
    pub launch: String,
}

/// A click on a toast or one of its buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    pub arguments: String,
    pub reply_text: Option<String>,
}

pub type ActivationHandler = Arc<dyn Fn(Activation) + Send + Sync>;

pub trait Toasts: Send + Sync {
    /// Show the toast, replacing one with the same tag. An error says why it could not be shown.
    fn show(&self, spec: &ToastSpec) -> Result<(), String>;
    /// Take the toast with this tag back, from the screen and from the Action Center.
    fn withdraw(&self, tag: &str);
    /// Where clicks go. One handler; setting another replaces it.
    fn on_activated(&self, handler: ActivationHandler);
}

/// A toast tag for a chat: a 64-bit hash of its id in 16 hex digits (tags are limited to 16
/// characters on older Windows 10).
pub fn tag_for(chat_id: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in chat_id.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // The markup is XML 1.0: these cannot appear in it at all.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// The toast's markup.
pub fn toast_xml(spec: &ToastSpec) -> String {
    let mut x = format!(
        "<toast launch=\"{}\" activationType=\"foreground\"><visual><binding template=\"ToastGeneric\"><text>{}</text>",
        esc(&spec.launch),
        esc(&spec.title)
    );
    if !spec.body.is_empty() {
        x.push_str(&format!("<text>{}</text>", esc(&spec.body)));
    }
    x.push_str("</binding></visual>");
    if spec.reply_box.is_some() || !spec.actions.is_empty() {
        x.push_str("<actions>");
        if let Some(r) = &spec.reply_box {
            x.push_str(&format!(
                "<input id=\"{REPLY_INPUT_ID}\" type=\"text\" placeHolderContent=\"{}\"/>\
                 <action content=\"{}\" arguments=\"{}\" activationType=\"foreground\" hint-inputId=\"{REPLY_INPUT_ID}\"/>",
                esc(&r.placeholder),
                esc(&r.send_label),
                esc(&r.arguments)
            ));
        }
        for a in &spec.actions {
            x.push_str(&format!(
                "<action content=\"{}\" arguments=\"{}\" activationType=\"foreground\"/>",
                esc(&a.label),
                esc(&a.arguments)
            ));
        }
        x.push_str("</actions>");
    }
    x.push_str("</toast>");
    x
}

/// Register the app id Helm's toasts are shown under, and make it this process's own. Writes only
/// to the current user's classes. `icon` is a PNG or icon file the toast header shows.
pub fn register_app_id(aumid: &str, display_name: &str, icon: Option<&Path>) -> Result<(), String> {
    if aumid.is_empty() || aumid.len() > 128 || aumid.contains(['\\', '/', '\0']) {
        return Err("the app id is not usable".into());
    }
    register_impl(aumid, display_name, icon)
}

/// Where toasts go when there is no Windows to show them: they are refused, and the caller falls
/// back.
pub struct NoToasts;

impl Toasts for NoToasts {
    fn show(&self, _spec: &ToastSpec) -> Result<(), String> {
        Err("native toasts are not available on this system".into())
    }
    fn withdraw(&self, _tag: &str) {}
    fn on_activated(&self, _handler: ActivationHandler) {}
}

/// The real toasts for the running OS.
pub struct NativeToasts {
    #[cfg_attr(not(windows), allow(dead_code))]
    handler: Arc<Mutex<Option<ActivationHandler>>>,
    #[cfg(windows)]
    worker: win::Worker,
}

impl NativeToasts {
    /// Toasts shown under `aumid` (register it first with [`register_app_id`]).
    pub fn new(aumid: &str) -> Self {
        let handler: Arc<Mutex<Option<ActivationHandler>>> = Arc::new(Mutex::new(None));
        #[cfg(windows)]
        {
            Self {
                worker: win::Worker::start(aumid.to_owned(), handler.clone()),
                handler,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = aumid;
            Self { handler }
        }
    }
}

impl Toasts for NativeToasts {
    fn show(&self, spec: &ToastSpec) -> Result<(), String> {
        #[cfg(windows)]
        {
            self.worker.show(spec)
        }
        #[cfg(not(windows))]
        {
            let _ = spec;
            Err("native toasts are not available on this system".into())
        }
    }

    fn withdraw(&self, tag: &str) {
        #[cfg(windows)]
        self.worker.withdraw(tag);
        #[cfg(not(windows))]
        let _ = tag;
    }

    fn on_activated(&self, handler: ActivationHandler) {
        *self.handler.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
    }
}

#[cfg(not(windows))]
fn register_impl(_aumid: &str, _name: &str, _icon: Option<&Path>) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn register_impl(aumid: &str, display_name: &str, icon: Option<&Path>) -> Result<(), String> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    fn set_text(key: HKEY, name: &str, value: &str) -> Result<(), String> {
        let name_w = crate::fs::wide(name);
        let value_w = crate::fs::wide(value);
        // SAFETY: both buffers are NUL-terminated and outlive the call; the size counts bytes
        // including the terminator, as REG_SZ wants.
        let r = unsafe {
            RegSetValueExW(
                key,
                name_w.as_ptr(),
                0,
                REG_SZ,
                value_w.as_ptr().cast::<u8>(),
                (value_w.len() * 2) as u32,
            )
        };
        if r == 0 {
            Ok(())
        } else {
            Err(format!("could not write {name} (code {r})"))
        }
    }

    let subkey = crate::fs::wide(format!("Software\\Classes\\AppUserModelId\\{aumid}"));
    let mut key: HKEY = null_mut();
    // SAFETY: `subkey` is NUL-terminated and outlives the call; `key` is written only on success
    // and closed below.
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        )
    };
    if r != 0 {
        return Err(format!("could not register the app id (code {r})"));
    }
    let wrote = set_text(key, "DisplayName", display_name).and_then(|()| match icon {
        Some(p) => set_text(key, "IconUri", &p.to_string_lossy()),
        None => Ok(()),
    });
    // SAFETY: `key` was opened above and is closed once.
    unsafe { RegCloseKey(key) };
    wrote?;
    let id = crate::fs::wide(aumid);
    // SAFETY: NUL-terminated and outlives the call.
    let hr = unsafe { SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
    if hr < 0 {
        return Err(format!("could not set the process app id (code {hr})"));
    }
    Ok(())
}

#[cfg(windows)]
mod win {
    use super::{toast_xml, Activation, ActivationHandler, ToastSpec, MAX_LIVE, REPLY_INPUT_ID};
    use std::collections::HashMap;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use windows::core::{IInspectable, Interface, HSTRING};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::{IPropertyValue, TypedEventHandler};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::UI::Notifications::{
        ToastActivatedEventArgs, ToastNotification, ToastNotificationManager,
    };

    /// How long a caller waits for the toast thread to show one.
    const SHOW_WAIT: Duration = Duration::from_secs(5);

    enum Cmd {
        Show(Box<ToastSpec>, Sender<Result<(), String>>),
        Withdraw(String),
    }

    pub(super) struct Worker {
        tx: Mutex<Sender<Cmd>>,
    }

    impl Worker {
        pub(super) fn start(aumid: String, handler: Arc<Mutex<Option<ActivationHandler>>>) -> Self {
            let (tx, rx) = channel::<Cmd>();
            let spawned = std::thread::Builder::new()
                .name("toasts".into())
                .spawn(move || {
                    // SAFETY: plain COM initialisation for this thread; it is never undone, so the
                    // apartment lives as long as the process and click handlers can run in it.
                    unsafe {
                        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    }
                    let mut live: HashMap<String, ToastNotification> = HashMap::new();
                    while let Ok(cmd) = rx.recv() {
                        match cmd {
                            Cmd::Show(spec, reply) => {
                                let r = show_one(&aumid, &spec, &handler, &mut live)
                                    .map_err(|e| e.to_string());
                                let _ = reply.send(r);
                            }
                            Cmd::Withdraw(tag) => {
                                live.remove(&tag);
                                if let Err(e) = remove_from_history(&aumid, &tag) {
                                    log::debug!("toast withdraw: {e}");
                                }
                            }
                        }
                    }
                });
            if let Err(e) = spawned {
                log::warn!("toast thread: {e}");
            }
            Self { tx: Mutex::new(tx) }
        }

        pub(super) fn show(&self, spec: &ToastSpec) -> Result<(), String> {
            let (reply, wait) = channel();
            self.tx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .send(Cmd::Show(Box::new(spec.clone()), reply))
                .map_err(|_| "the toast thread is not running".to_owned())?;
            wait.recv_timeout(SHOW_WAIT)
                .map_err(|_| "the toast was not shown in time".to_owned())?
        }

        pub(super) fn withdraw(&self, tag: &str) {
            let _ = self
                .tx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .send(Cmd::Withdraw(tag.to_owned()));
        }
    }

    fn show_one(
        aumid: &str,
        spec: &ToastSpec,
        handler: &Arc<Mutex<Option<ActivationHandler>>>,
        live: &mut HashMap<String, ToastNotification>,
    ) -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(toast_xml(spec)))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        toast.SetTag(&HSTRING::from(spec.tag.as_str()))?;
        toast.SetGroup(&HSTRING::from(spec.group.as_str()))?;
        let handler = handler.clone();
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
            move |_sender, args| {
                let Some(args) = args.as_ref() else {
                    return Ok(());
                };
                let Ok(a) = args.cast::<ToastActivatedEventArgs>() else {
                    return Ok(());
                };
                let arguments = a.Arguments()?.to_string();
                let reply_text = typed_reply(&a);
                let h = handler.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(h) = h {
                    h(Activation {
                        arguments,
                        reply_text,
                    });
                }
                Ok(())
            },
        ))?;
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))?;
        notifier.Show(&toast)?;
        if live.len() >= MAX_LIVE && !live.contains_key(&spec.tag) {
            if let Some(old) = live.keys().next().cloned() {
                live.remove(&old);
            }
        }
        live.insert(spec.tag.clone(), toast);
        Ok(())
    }

    /// What was typed in the reply box, if the click carried any.
    fn typed_reply(a: &ToastActivatedEventArgs) -> Option<String> {
        let input = a.UserInput().ok()?;
        let value = input.Lookup(&HSTRING::from(REPLY_INPUT_ID)).ok()?;
        let text = value.cast::<IPropertyValue>().ok()?.GetString().ok()?;
        Some(text.to_string())
    }

    fn remove_from_history(aumid: &str, tag: &str) -> windows::core::Result<()> {
        ToastNotificationManager::History()?.RemoveGroupedTagWithId(
            &HSTRING::from(tag),
            &HSTRING::from(super::GROUP),
            &HSTRING::from(aumid),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ToastSpec {
        ToastSpec {
            tag: tag_for("c1"),
            group: GROUP.into(),
            title: "Fix <it> & \"go\"".into(),
            body: "line one\u{7}".into(),
            actions: vec![
                ToastAction {
                    label: "Continue".into(),
                    arguments: "continue:c1".into(),
                },
                ToastAction {
                    label: "Open".into(),
                    arguments: "open:c1".into(),
                },
            ],
            reply_box: Some(ReplyBox {
                placeholder: "Reply…".into(),
                send_label: "Send".into(),
                arguments: "reply:c1".into(),
            }),
            launch: "open:c1".into(),
        }
    }

    #[test]
    fn tags_are_short_stable_and_differ_per_chat() {
        assert_eq!(tag_for("c1"), tag_for("c1"));
        assert_ne!(tag_for("c1"), tag_for("c2"));
        for id in ["c1", "", "a-long-chat-id-with-many-characters-0123456789"] {
            let t = tag_for(id);
            assert_eq!(t.len(), 16, "{id}");
            assert!(t.chars().all(|c| c.is_ascii_hexdigit()), "{t}");
        }
    }

    #[test]
    fn the_markup_escapes_text_and_carries_the_buttons_in_order() {
        let x = toast_xml(&spec());
        assert!(x.starts_with("<toast launch=\"open:c1\" activationType=\"foreground\">"));
        assert!(
            x.contains("<text>Fix &lt;it&gt; &amp; &quot;go&quot;</text>"),
            "{x}"
        );
        assert!(
            x.contains("<text>line one</text>"),
            "control characters are dropped: {x}"
        );
        let at = |needle: &str| x.find(needle).unwrap_or_else(|| panic!("{needle} in {x}"));
        assert!(at("<input id=\"reply\"") < at("arguments=\"reply:c1\""));
        assert!(at("arguments=\"reply:c1\"") < at("arguments=\"continue:c1\""));
        assert!(at("arguments=\"continue:c1\"") < at("arguments=\"open:c1\" activationType"));
        assert!(x.contains("hint-inputId=\"reply\""));
        assert!(x.ends_with("</actions></toast>"));
        // No buttons and no reply box: no actions element; an empty body: one text line.
        let plain = toast_xml(&ToastSpec {
            actions: vec![],
            reply_box: None,
            body: String::new(),
            ..spec()
        });
        assert!(
            !plain.contains("<actions>") && plain.matches("<text>").count() == 1,
            "{plain}"
        );
    }

    #[test]
    fn other_systems_show_nothing_and_say_so() {
        assert!(NoToasts.show(&spec()).is_err());
        assert!(register_app_id("", "Helm", None).is_err());
        assert!(register_app_id("a\\b", "Helm", None).is_err());
    }
}
