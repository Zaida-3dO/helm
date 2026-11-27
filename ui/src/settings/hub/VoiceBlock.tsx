// Settings → Hub's Voice block: the chat the phone's voice screen talks to, the model and effort a
// new voice chat starts with, how long it may sit idle before the next message starts a new one,
// and what the attention watch wakes it for. It reads and saves through the host's voice ops; the
// connection to the hub is set above it in the same pane.

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { LocalModelInfo } from '../../api/types.ts';
import { hubOps, type VoiceSettings, type VoiceWatch } from '../../api/hub.ts';
import { localModelChoices } from '../../composer/modelEffort.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Picker, SettingRow } from '../../ui/index.ts';
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
// A lane's roll-over size: tokens, or 0 to leave it off (the host checks the same range).
const ROLL_MIN = 4_000;
const ROLL_MAX = 200_000;

function rollValid(raw: string): boolean {
  const n = Number(raw);
  return Number.isInteger(n) && (n === 0 || (n >= ROLL_MIN && n <= ROLL_MAX));
}

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

export function VoiceBlock({ profileId }: { profileId: string | null }) {
  const shell = useShell();
  const ops = useMemo(() => hubOps(shell.api), [shell.api]);
  const [voice, setVoice] = useState<VoiceSettings | null>(null);
  const [idle, setIdle] = useState('');
  const [quickRoll, setQuickRoll] = useState('');
  const [deepRoll, setDeepRoll] = useState('');
  const [models, setModels] = useState<{ id: string; name: string; description?: string }[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [pinNote, setPinNote] = useState<string | null>(null);

  const show = useCallback((v: VoiceSettings) => {
    setVoice(v);
    setIdle(String(v.idleMinutes));
    setQuickRoll(String(v.quickRollAt));
    setDeepRoll(String(v.deepRollAt));
  }, []);

  useEffect(() => {
    if (!profileId) return;
    setPinNote(null);
    let live = true;
    const read = () =>
      void ops
        .voice({ profileId })
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
  }, [ops, shell, profileId, show]);

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
    if (voice && profileId) void run(ops.saveVoice({ ...voice, ...patch, profileId }));
  };

  /** Ask the hub to start this person's voice sessions on this PC (the bot can change it back). */
  const pin = async () => {
    if (!profileId) return;
    setBusy(true);
    try {
      await ops.routePin({ profileId, channel: 'voice' });
      setPinNote('Asked the hub to start your voice turns on this PC.');
      setError(null);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
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
            <Button variant="text" data-testid="hub-voice-reset" disabled={busy} onClick={() => profileId && void run(ops.resetVoice({ profileId }))}>
              Start a new chat
            </Button>
          </>
        )}
      </SettingRow>
      {voice && (
        <SettingRow title="Model for new voice chats" description="The model a plain (deep) message opens its chat with. App default opens it on the same model as any other new chat.">
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
          title="Model for quick answers"
          description="A message marked quick on your phone opens its own chat on this model — a small local one answers fastest, and it stays separate from the deep one. Leave it unset and quick messages open like the others."
        >
          <Picker
            settings
            label="Model for quick answers"
            testId="hub-voice-quick-model"
            value={voice.quickModel}
            options={modelChoices(models, voice.quickModel)}
            searchable
            disabled={busy}
            onChange={(v) => save({ quickModel: v })}
          />
        </SettingRow>
      )}
      {voice && (
        <SettingRow
          title="Effort for quick answers"
          description="How hard the quick model thinks. Medium keeps thinking on while staying fast."
        >
          <Picker
            settings
            label="Effort for quick answers"
            testId="hub-voice-quick-effort"
            value={voice.quickEffort}
            options={effortChoices(voice.quickEffort)}
            disabled={busy}
            onChange={(v) => save({ quickEffort: v })}
          />
        </SettingRow>
      )}
      {voice && (
        <SettingRow
          title="Roll a voice chat over at"
          description="When the next message arrives to a chat this grown, it continues in a fresh one with a short carry-over note — no model turn, nothing lost. One size for each lane; 0 leaves it off."
        >
          <label className="hub-limit">
            Quick
            <input
              type="number"
              min={0}
              max={ROLL_MAX}
              step={1000}
              aria-label="Quick lane roll-over size"
              data-testid="hub-voice-quick-roll"
              value={quickRoll}
              onChange={(e) => setQuickRoll(e.target.value)}
            />
          </label>
          <label className="hub-limit">
            Deep
            <input
              type="number"
              min={0}
              max={ROLL_MAX}
              step={1000}
              aria-label="Deep lane roll-over size"
              data-testid="hub-voice-deep-roll"
              value={deepRoll}
              onChange={(e) => setDeepRoll(e.target.value)}
            />
          </label>
          <Button
            variant="secondary"
            disabled={busy || !rollValid(quickRoll) || !rollValid(deepRoll) || (Number(quickRoll) === voice.quickRollAt && Number(deepRoll) === voice.deepRollAt)}
            onClick={() => save({ quickRollAt: Number(quickRoll), deepRollAt: Number(deepRoll) })}
          >
            Save
          </Button>
        </SettingRow>
      )}
      {voice && (!rollValid(quickRoll) || !rollValid(deepRoll)) && (
        <div className="ui-field-error">A roll-over size must be {ROLL_MIN} to {ROLL_MAX.toLocaleString()} tokens, or 0 to leave it off.</div>
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
      {voice && (
        <SettingRow
          testId="hub-voice-take"
          title="Take my voice on this PC"
          description="Off, this PC never receives this person's voice turns, even while it is the only one up. On, it joins the PCs that may carry them. The PC of the home profile takes it unless you switch it off."
        >
          <Checkbox
            checked={voice.takeVoice}
            disabled={busy}
            aria-label="Take my voice on this PC"
            data-testid="hub-voice-take-box"
            onChange={(on) => save({ takeVoice: on })}
          />
        </SettingRow>
      )}
      {voice && (
        <SettingRow
          testId="hub-voice-route"
          title="Where my voice goes"
          description="A voice session stays on the PC it started on. Pinning asks the hub to send the next one here instead (this PC must take it, above)."
        >
          <Button variant="secondary" disabled={busy || !profileId || !voice.takeVoice} data-testid="hub-voice-route-pin" onClick={() => void pin()}>
            Use this PC
          </Button>
          {pinNote && (
            <span data-testid="hub-voice-pinned" className="hub-state">
              {pinNote}
            </span>
          )}
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
