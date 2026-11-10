//! The shell's record shapes, exported to `ui/src/api/generated/shell/`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Whether Windows raises a toast when a chat finishes or needs the user. Off unless the user
/// turns it on in Settings, Shell. The in-window notice stack this setting once chose between is
/// gone, so the old stored text `in_app` reads as off: only a stored `native` ever turns toasts on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum NotificationMode {
    #[default]
    #[serde(alias = "in_app")]
    Off,
    Native,
}

/// The terminal dock's shell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum TerminalShell {
    #[default]
    Pwsh,
    Powershell,
    Gitbash,
}

/// How long the machine is kept from sleeping: never, while chats run (or a saved schedule is
/// close to due), or the whole time Helm is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum KeepAwake {
    Off,
    #[default]
    Busy,
    Always,
}

/// The chosen microphone, kept even while the device is unplugged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MicChoice {
    /// The capture endpoint's id.
    pub uid: String,
    pub name: String,
}

/// Settings → Shell (all user-only keys, plus `keepAwake`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ShellSettings {
    /// A chord such as "Ctrl+Alt+H"; null when cleared.
    pub global_hotkey: Option<String>,
    pub keep_awake: KeepAwake,
    pub notifications: NotificationMode,
    /// Null (no row) means Automatic: the system's default capture device.
    pub dictation_mic: Option<MicChoice>,
    pub terminal_shell: TerminalShell,
}

pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+H";

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            global_hotkey: Some(DEFAULT_HOTKEY.to_owned()),
            keep_awake: KeepAwake::Busy,
            notifications: NotificationMode::Off,
            dictation_mic: None,
            terminal_shell: TerminalShell::Pwsh,
        }
    }
}

/// What `shell_settings_get` / `shell_settings_save` answer: the settings plus live status the
/// Shell pane shows (a hotkey that could not be registered, a keep-awake failure).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ShellSettingsView {
    pub settings: ShellSettings,
    pub hotkey_registered: bool,
    pub hotkey_error: Option<String>,
    pub keep_awake_error: Option<String>,
}

/// One capture device for the microphone picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AudioInput {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    /// False only for the stored choice when its device is gone.
    pub available: bool,
}

/// `open_external {url}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct OpenExternalArgs {
    pub url: String,
}
