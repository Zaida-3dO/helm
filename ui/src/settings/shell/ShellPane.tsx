// Settings → Shell: the global hotkey, keep-awake, Windows notifications, the microphone and
// the terminal's shell. Every control saves at once (user-only keys, through shell_settings_save).

import { useEffect, useState } from 'react';
import { GLOBAL_HOTKEY_BINDING, shellOps, type AudioInput, type ShellSettings, type ShellSettingsView } from '../../api/shell.ts';
import { useShell } from '../../shell/context.ts';
import { Button, keys, normaliseChord, Picker, Segmented, SettingRow, TextInput } from '../../ui/index.ts';
import { registerSettingsPane } from '../registry.ts';
import { DictationRows } from './DictationRows.tsx';
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
      <SettingRow
        title="Keep the PC awake"
        description="Stops idle sleep only; the screen can still turn off and lock. “While chats run” also covers the couple of minutes before a saved schedule is due."
        testId="setting-keep-awake"
      >
        <Segmented
          label="Keep the PC awake"
          value={s.keepAwake}
          options={[
            { value: 'off', label: 'Off' },
            { value: 'busy', label: 'While chats run' },
            { value: 'always', label: 'Always' },
          ]}
          onChange={(v) => save({ ...s, keepAwake: v })}
        />
      </SettingRow>
      {view.keepAwakeError && <p className="error">{view.keepAwakeError}</p>}
      <SettingRow
        title="Windows notifications"
        description="Off by default. When on, Windows shows a toast when a chat finishes (reply, Continue or Open from it) and when a chat needs you (Open only: answers and approvals stay in Helm). Muted chats never notify, and nothing shows while Helm is in front."
        testId="setting-notifications"
      >
        <Segmented
          label="Windows notifications"
          value={s.notifications}
          options={[
            { value: 'off', label: 'Off' },
            { value: 'native', label: 'On' },
          ]}
          onChange={(v) => save({ ...s, notifications: v })}
        />
      </SettingRow>
      <SettingRow title="Microphone" description="Used for dictation. Also in the tray menu.">
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
      <DictationRows />
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
