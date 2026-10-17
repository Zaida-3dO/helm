// Settings → Shell: the global hotkey, keep-awake, where notifications go, the microphone and
// the terminal's shell. Every control saves at once (user-only keys, through shell_settings_save).

import { useEffect, useState } from 'react';
import { GLOBAL_HOTKEY_BINDING, shellOps, type AudioInput, type ShellSettings, type ShellSettingsView } from '../../api/shell.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, keys, normaliseChord, Picker, Segmented, SettingRow, TextInput } from '../../ui/index.ts';
import { registerSettingsPane } from '../registry.ts';
import { hotkeyProblem } from './hotkey.ts';

const AUTO = '__automatic__';

function ShellPane() {
  const shell = useShell();
  const ops = shellOps(shell.api);
  const [view, setView] = useState<ShellSettingsView | null>(null);
  const [mics, setMics] = useState<AudioInput[]>([]);
  const [chord, setChord] = useState('');
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    ops
      .get()
      .then((v) => {
        setView(v);
        setChord(v.settings.globalHotkey ?? '');
      })
      .catch((e: unknown) => setError(String((e as Error).message ?? e)));
    ops
      .audioInputs()
      .then(setMics)
      .catch(() => undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [shell]);
  if (!view) return <p className="secondary">{error ?? 'Loading…'}</p>;
  const s = view.settings;
  const save = (next: ShellSettings) => {
    setError(null);
    ops
      .save(next)
      .then((v) => {
        setView(v);
        setChord(v.settings.globalHotkey ?? '');
        keys.register({ id: GLOBAL_HOTKEY_BINDING, title: 'Show or hide Helm (anywhere in Windows)', defaultKeys: v.settings.globalHotkey, scope: 'global' });
      })
      .catch((e: unknown) => setError(String((e as Error).message ?? e)));
  };
  const saveHotkey = (text: string) => {
    const t = text.trim();
    if (!t) return save({ ...s, globalHotkey: null });
    const problem = hotkeyProblem(t, keys.list());
    if (problem) return setError(problem);
    save({ ...s, globalHotkey: normaliseChord(t) });
  };
  const micOptions = [
    { value: AUTO, title: 'Automatic', subtitle: 'The Windows default microphone' },
    ...mics.map((m) => ({
      value: m.id,
      title: m.name,
      text: m.name,
      subtitle: m.available ? (m.isDefault ? 'Windows default' : undefined) : 'Not connected',
    })),
  ];
  return (
    <div data-testid="shell-pane">
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <SettingRow
        title="Global hotkey"
        description={
          view.hotkeyError ? (
            <span className="error">{view.hotkeyError}</span>
          ) : (
            'Shows or hides Helm from any app. Leave empty to turn it off.'
          )
        }
        testId="setting-hotkey"
      >
        <TextInput
          value={chord}
          placeholder="Ctrl+Alt+H"
          aria-label="Global hotkey"
          style={{ width: 140 }}
          onChange={(e) => setChord(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') saveHotkey(chord);
          }}
        />
        <Button onClick={() => saveHotkey(chord)}>Save</Button>
      </SettingRow>
      <SettingRow title="Keep the PC awake while chats run" description="Stops idle sleep only; the screen can still turn off and lock.">
        <Checkbox checked={s.keepAwake} onChange={(on) => save({ ...s, keepAwake: on })} />
      </SettingRow>
      {view.keepAwakeError && <p className="error">{view.keepAwakeError}</p>}
      <SettingRow title="Notifications" description="Where a finished chat tells you about it when you are not looking at it.">
        <Segmented
          label="Notifications"
          value={s.notifications}
          options={[
            { value: 'in_app', label: 'In Helm' },
            { value: 'native', label: 'Windows' },
          ]}
          onChange={(v) => save({ ...s, notifications: v })}
        />
      </SettingRow>
      <SettingRow title="Microphone" description="Used for dictation when it arrives. Also in the tray menu.">
        <Picker
          label="Microphone"
          settings
          value={s.dictationMic?.uid ?? AUTO}
          options={micOptions}
          onChange={(v) => {
            if (v === AUTO) return save({ ...s, dictationMic: null });
            const m = mics.find((x) => x.id === v);
            if (m) save({ ...s, dictationMic: { uid: m.id, name: m.name } });
          }}
        />
      </SettingRow>
      <SettingRow title="Terminal shell" description="The shell new terminal tabs start.">
        <Segmented
          label="Terminal shell"
          value={s.terminalShell}
          options={[
            { value: 'pwsh', label: 'PowerShell 7' },
            { value: 'powershell', label: 'Windows PowerShell' },
            { value: 'gitbash', label: 'Git Bash' },
          ]}
          onChange={(v) => save({ ...s, terminalShell: v })}
        />
      </SettingRow>
    </div>
  );
}

registerSettingsPane({ id: 'shell', title: 'Shell', order: 5, render: () => <ShellPane /> });
