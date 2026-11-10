// Settings → Hub's Voice block: the chat the phone's voice screen talks to, the model and effort a
// new voice chat starts with, how long it may sit idle before the next message starts a new one,
// and what the attention watch wakes it for. It reads and saves through the host's voice ops; the
// connection to the hub is set above it in the same pane.

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { LocalModelInfo } from '../../api/types.ts';
import { hubOps, type VoiceSettings, type VoiceWatch } from '../../api/hub.ts';
import { localModelChoices } from '../../composer/modelEffort.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Picker, SettingRow } from '../../ui/index.ts';
import { SETTINGS_DIALOG } from '../SettingsDialog.tsx';
import { EFFORT_OPTIONS } from '../runtime/model.ts';
import { compactCount } from '../../transcript/rowMeta.ts';

type Choice = { value: string; title: string; subtitle?: string };

const WATCH_OPTIONS: { value: VoiceWatch; title: string; subtitle: string }[] = [
  { value: 'off', title: 'Off', subtitle: 'It only speaks up when you talk to it' },
  { value: 'flags', title: 'Chats that need you', subtitle: 'It hears when a chat flags itself, asks something or stalls' },
  { value: 'flags+done', title: 'Those, and finished replies', subtitle: 'It also hears when a chat finishes a reply' },
];

const IDLE_MIN = 15;
const IDLE_MAX = 1440;

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

/** The app default first, then the model list; a saved model the list lacks still shows. */
function modelChoices(models: { id: string; name: string; description?: string }[], saved: string): Choice[] {
  const out: Choice[] = [{ value: '', title: 'App default', subtitle: 'The same model any other new chat starts with' }];
  for (const m of models) {
    if (!m.id || out.some((o) => o.value === m.id)) continue;
    out.push({ value: m.id, title: m.name || m.id, subtitle: m.description || undefined });
  }
  if (saved && !out.some((o) => o.value === saved)) out.push({ value: saved, title: saved });
  return out;
}

function effortChoices(saved: string): Choice[] {
  const out: Choice[] = [{ value: '', title: 'App default', subtitle: 'The same effort any other new chat starts with' }, ...EFFORT_OPTIONS];
  if (saved && !out.some((o) => o.value === saved)) out.push({ value: saved, title: saved });
  return out;
}

export function VoiceBlock() {
  const shell = useShell();
  const ops = useMemo(() => hubOps(shell.api), [shell.api]);
  const [voice, setVoice] = useState<VoiceSettings | null>(null);
  const [idle, setIdle] = useState('');
  const [models, setModels] = useState<{ id: string; name: string; description?: string }[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const show = useCallback((v: VoiceSettings) => {
    setVoice(v);
    setIdle(String(v.idleMinutes));
  }, []);

  useEffect(() => {
    let live = true;
    const read = () =>
      void ops
        .voice()
        .then((v) => live && show(v))
        .catch((e: unknown) => live && setError(errorText(e)));
    read();
    void shell
      .api<{ models: { id: string; name: string; description?: string }[]; local?: LocalModelInfo[] }>('model_catalogue', { harness: 'claude' })
      .then((c) => live && setModels([...c.models, ...localModelChoices(c.local)]))
      .catch(() => undefined);
    // A chat that changes the policy through its settings tools: re-read so the pickers follow.
    const off = shell.bus.on('relay_voice', read);
    return () => {
      live = false;
      off();
    };
  }, [ops, shell, show]);

  const run = async (p: Promise<VoiceSettings>) => {
    setBusy(true);
    try {
      show(await p);
      setError(null);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const save = (patch: Partial<Omit<VoiceSettings, 'chat'>>) => {
    if (voice) void run(ops.saveVoice({ ...voice, ...patch }));
  };

  const minutes = Number(idle);
  const idleValid = Number.isInteger(minutes) && minutes >= IDLE_MIN && minutes <= IDLE_MAX;
  const idleChanged = voice !== null && minutes !== voice.idleMinutes;
  const modelOptions = useMemo(() => modelChoices(models, voice?.model ?? ''), [models, voice?.model]);

  return (
    <div data-testid="hub-voice">
      <h3 className="hub-heading">Voice</h3>
      <SettingRow
        testId="hub-voice-chat"
        title="Voice chat"
        description={
          voice?.chat
            ? `Messages from your phone's voice screen go to this chat. It started ${new Date(voice.chat.startedAt).toLocaleString()} and has used ${compactCount(voice.chat.tokensUsed)} tokens since.`
            : 'No voice chat yet: the next message from your phone starts one.'
        }
      >
        {voice?.chat && (
          <>
            <Button
              variant="secondary"
              onClick={() => {
                if (!voice.chat) return;
                shell.select(voice.chat.id);
                shell.closeDialog(SETTINGS_DIALOG);
              }}
            >
              Open {voice.chat.title}
            </Button>
            <Button variant="text" data-testid="hub-voice-reset" disabled={busy} onClick={() => void run(ops.resetVoice())}>
              Start a new chat
            </Button>
          </>
        )}
      </SettingRow>
      {voice && (
        <SettingRow title="Model for new voice chats" description="The model a new voice chat opens with. A quick one keeps answers fast. App default opens it on the same model as any other new chat.">
          <Picker
            settings
            label="Model for new voice chats"
            testId="hub-voice-model"
            value={voice.model}
            options={modelOptions}
            searchable
            disabled={busy}
            onChange={(v) => save({ model: v })}
          />
        </SettingRow>
      )}
      {voice && (
        <SettingRow title="Effort for new voice chats" description="How hard the agent thinks before answering in a new voice chat. Low keeps replies quick.">
          <Picker
            settings
            label="Effort for new voice chats"
            testId="hub-voice-effort"
            value={voice.effort}
            options={effortChoices(voice.effort)}
            disabled={busy}
            onChange={(v) => save({ effort: v })}
          />
        </SettingRow>
      )}
      {voice && (
        <SettingRow
          title="Start a new voice chat after"
          description="A message that comes this long after the last one starts a fresh chat, so yesterday's conversation is not dragged along."
        >
          <label className="hub-limit">
            <input
              type="number"
              min={IDLE_MIN}
              max={IDLE_MAX}
              aria-label="Idle minutes"
              data-testid="hub-voice-idle"
              value={idle}
              onChange={(e) => setIdle(e.target.value)}
            />
            minutes idle
          </label>
          <Button variant="secondary" disabled={busy || !idleValid || !idleChanged} onClick={() => save({ idleMinutes: minutes })}>
            Save
          </Button>
        </SettingRow>
      )}
      {voice && !idleValid && <div className="ui-field-error">Idle time must be {IDLE_MIN} to {IDLE_MAX} minutes.</div>}
      {voice && (
        <SettingRow title="Watching other chats" description="What the voice chat is woken for while you are away from the PC. It decides for itself whether anything is worth telling you.">
          <Picker
            settings
            label="Watching other chats"
            testId="hub-voice-watch"
            value={voice.watch}
            options={WATCH_OPTIONS}
            disabled={busy}
            onChange={(v) => save({ watch: v as VoiceWatch })}
          />
        </SettingRow>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="hub-voice-error">
          {error}
        </div>
      )}
    </div>
  );
}
