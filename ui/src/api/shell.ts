// The shell's ops (helm-shell).

import type { Api } from './client.ts';
import type { AudioInput, ShellSettings, ShellSettingsView } from './generated/shell/index.ts';

export type { AudioInput, ShellSettings, ShellSettingsView };

export function shellOps(api: Api) {
  return {
    get: () => api<ShellSettingsView>('shell_settings_get'),
    save: (s: ShellSettings) => api<ShellSettingsView>('shell_settings_save', s),
    audioInputs: () => api<AudioInput[]>('audio_inputs'),
    openExternal: (url: string) => api<null>('open_external', { url }),
  };
}

export type ShellOps = ReturnType<typeof shellOps>;

/** The global hotkey's id in the key registry (scope "global": the host registers it). */
export const GLOBAL_HOTKEY_BINDING = 'app.globalHotkey';
