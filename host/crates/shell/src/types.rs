//! The shell's record shapes, exported to `ui/src/api/generated/shell/`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Where finished-turn notices go: in-app toasts by default, Windows
/// toasts behind the setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum NotificationMode {
    #[default]
    InApp,
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
    pub keep_awake: bool,
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
            keep_awake: true,
            notifications: NotificationMode::InApp,
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
