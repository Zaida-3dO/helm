// Settings → Shell → Dictation: whether dictation goes live or records for the server, where the
// speech-to-text server is, whether an address on the home network is allowed, the language, a
// Test button, and how much graphics memory a local model leaves free for speech. Each control saves at once; the host refuses a bad address with a
// sentence that is shown under the rows.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Segmented, SettingRow, TextInput } from '../../ui/index.ts';
import { dictationOps, type DictationMode, type DictationSettingsChange, type DictationView } from '../../composer/dictation.ts';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export function DictationRows() {
  const shell = useShell();
  const ops = useMemo(() => dictationOps(shell.api), [shell.api]);
  const [view, setView] = useState<DictationView | null>(null);
  /** What is typed in a field, or null while the field follows the saved value. */
  const [url, setUrl] = useState<string | null>(null);
  const [language, setLanguage] = useState<string | null>(null);
  const [reserve, setReserve] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tested, setTested] = useState<{ ok: boolean; message: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    ops
      .get()
      .then(setView)
      .catch((e: unknown) => setError(errorText(e)));
  }, [ops]);
  useEffect(() => {
    load();
    return shell.bus.on('dictation', load);
  }, [shell, load]);

  if (!view) return error ? <p className="error">{error}</p> : null;

  const save = (change: DictationSettingsChange) => {
    setError(null);
    setTested(null);
    ops
      .set(change)
      .then((v) => {
        setView(v);
        setUrl(null);
        setLanguage(null);
        setReserve(null);
      })
      .catch((e: unknown) => setError(errorText(e)));
  };
  const test = () => {
    setBusy(true);
    setTested(null);
    ops
      .test()
      .then(setTested)
      .catch((e: unknown) => setTested({ ok: false, message: errorText(e) }))
      .finally(() => setBusy(false));
  };
  const commitReserve = () => {
    if (reserve === null) return;
    const n = Number(reserve);
    setReserve(null);
    if (reserve.trim() === '' || !Number.isInteger(n) || n < 0 || n > view.speechReserveMaxGb) {
      setError(`Memory kept for speech is a whole number of GB from 0 to ${view.speechReserveMaxGb}.`);
      return;
    }
    if (n !== view.speechReserveGb) save({ speechReserveGb: n });
  };

  const liveNote = view.liveAvailable
    ? 'Windows speech recognition is ready on this PC.'
    : (view.liveUnavailable ?? "Windows speech recognition isn't available on this PC.");
  const shownUrl = url ?? view.sttUrl ?? '';
  const shownLanguage = language ?? view.language;
  const shownReserve = reserve ?? String(view.speechReserveGb);
  return (
    <div data-testid="dictation-settings">
      <SettingRow
        title="Dictation: how it listens"
        description={`Live: words appear as you speak, using Windows speech recognition, and the speech-to-text server below takes over if live cannot start. Record: the whole clip goes to the server when you finish, which is slower but usually more accurate. With no server set, dictation is always live. ${liveNote}`}
        testId="setting-dictation-mode"
      >
        <Segmented<DictationMode>
          label="Dictation mode"
          testId="dictation-mode-choice"
          value={view.mode}
          options={[
            { value: 'live', label: 'Live' },
            { value: 'record', label: 'Record, then transcribe' },
          ]}
          onChange={(mode) => save({ mode })}
        />
      </SettingRow>
      <SettingRow
        title="Dictation: speech-to-text address"
        description="The Whisper server dictation sends a short recording to (an OpenAI-style one, on this computer unless you allow your network below). The words go into your draft; Helm never sends the message for you. Leave empty to use live dictation only."
        testId="setting-dictation-url"
      >
        <TextInput
          value={shownUrl}
          placeholder="http://127.0.0.1:8000"
          aria-label="Speech-to-text address"
          data-testid="dictation-url"
          style={{ width: 230 }}
          onChange={(e) => setUrl(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') save({ sttUrl: shownUrl });
          }}
        />
        <Button disabled={url === null || shownUrl === (view.sttUrl ?? '')} data-testid="dictation-save" onClick={() => save({ sttUrl: shownUrl })}>
          Save
        </Button>
        <Button busy={busy} disabled={!view.configured} data-testid="dictation-test" onClick={test}>
          Test
        </Button>
      </SettingRow>
      {tested && (
        <p className={tested.ok ? 'secondary' : 'error'} role="status" data-testid="dictation-test-result">
          {tested.message}
        </p>
      )}
      <SettingRow
        title="Allow an address on my network"
        description="Off: only this computer. On: a numeric address on your home network or Tailscale, like 192.168.1.20. A public address is never allowed."
        testId="setting-dictation-private"
      >
        <Checkbox checked={view.allowPrivateNetwork} aria-label="Allow an address on my network" data-testid="dictation-private" onChange={(on) => save({ allowPrivateNetwork: on })} />
      </SettingRow>
      <SettingRow title="Dictation language" description="A language code like en, es or fr, or auto to let the server decide." testId="setting-dictation-language">
        <TextInput
          value={shownLanguage}
          aria-label="Dictation language"
          data-testid="dictation-language"
          style={{ width: 80 }}
          onChange={(e) => setLanguage(e.target.value)}
          onBlur={() => language !== null && language !== view.language && save({ language: language.trim() })}
          onKeyDown={(e) => {
            if (e.key === 'Enter') save({ language: shownLanguage.trim() });
          }}
        />
      </SettingRow>
      <SettingRow
        title="Keep GPU memory for speech"
        description={`When Helm has to unload other local models to load one, it frees this much more so speech-to-text still fits (0 to ${view.speechReserveMaxGb} GB). Speech-to-text never unloads a model.`}
        testId="setting-dictation-reserve"
      >
        <TextInput
          inputMode="numeric"
          value={shownReserve}
          aria-label="Memory kept for speech (GB)"
          data-testid="dictation-reserve"
          style={{ width: 64 }}
          onChange={(e) => setReserve(e.target.value)}
          onBlur={commitReserve}
          onKeyDown={(e) => {
            if (e.key === 'Enter') commitReserve();
            if (e.key === 'Escape') setReserve(null);
          }}
        />
        <span className="secondary">GB</span>
      </SettingRow>
      {error && (
        <p className="error" role="alert" data-testid="dictation-error-text">
          {error}
        </p>
      )}
    </div>
  );
}
