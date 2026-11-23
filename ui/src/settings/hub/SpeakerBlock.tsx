// Settings → Hub's Speakers block: the chats the house's room speakers talk to, the model and
// effort a new one starts with, whether this PC takes them, and how long a talk that ended may be
// named to the next one. It reads and saves through the host's speaker ops; the hub line is set
// above it in the same pane.

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { LocalModelInfo } from '../../api/types.ts';
import { hubOps, type SpeakerSettings } from '../../api/hub.ts';
import { localModelChoices } from '../../composer/modelEffort.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Picker, SettingRow } from '../../ui/index.ts';
import { SETTINGS_DIALOG } from '../SettingsDialog.tsx';
import { EFFORT_OPTIONS } from '../runtime/model.ts';

type Choice = { value: string; title: string; subtitle?: string };

const PREVIOUS_MIN = 0;
const PREVIOUS_MAX = 72;

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

/** The app default first, then the model list; a saved model the list lacks still shows. */
function modelChoices(models: { id: string; name: string; description?: string }[], saved: string): Choice[] {
  const out: Choice[] = [
    { value: '', title: 'App default', subtitle: 'The same model any other new chat starts with' },
  ];
  for (const m of models) {
    if (!m.id || out.some((o) => o.value === m.id)) continue;
    out.push({ value: m.id, title: m.name || m.id, subtitle: m.description || undefined });
  }
  if (saved && !out.some((o) => o.value === saved)) out.push({ value: saved, title: saved });
  return out;
}

function effortChoices(saved: string): Choice[] {
  const out: Choice[] = [
    { value: '', title: 'App default', subtitle: 'The same effort any other new chat starts with' },
    ...EFFORT_OPTIONS,
  ];
  if (saved && !out.some((o) => o.value === saved)) out.push({ value: saved, title: saved });
  return out;
}

export function SpeakerBlock({ profileId }: { profileId: string | null }) {
  const shell = useShell();
  const ops = useMemo(() => hubOps(shell.api), [shell.api]);
  const [speaker, setSpeaker] = useState<SpeakerSettings | null>(null);
  const [previous, setPrevious] = useState('');
  const [models, setModels] = useState<{ id: string; name: string; description?: string }[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const show = useCallback((v: SpeakerSettings) => {
    setSpeaker(v);
    setPrevious(String(v.previousHours));
  }, []);

  useEffect(() => {
    if (!profileId) return;
    let live = true;
    const read = () =>
      void ops
        .speaker({ profileId })
        .then((v) => live && show(v))
        .catch((e: unknown) => live && setError(errorText(e)));
    read();
    void shell
      .api<{ models: { id: string; name: string; description?: string }[]; local?: LocalModelInfo[] }>('model_catalogue', { harness: 'claude' })
      .then((c) => live && setModels([...c.models, ...localModelChoices(c.local)]))
      .catch(() => undefined);
    // A chat that changes the policy through its settings tools: re-read so the pickers follow.
    const off = shell.bus.on('relay_speaker', read);
    return () => {
      live = false;
      off();
    };
  }, [ops, shell, profileId, show]);

  const run = async (p: Promise<SpeakerSettings>) => {
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

  const save = (patch: Partial<SpeakerSettings>) => {
    if (speaker && profileId) void run(ops.saveSpeaker({ ...speaker, ...patch, profileId }));
  };

  const hours = Number(previous);
  const previousValid = Number.isInteger(hours) && hours >= PREVIOUS_MIN && hours <= PREVIOUS_MAX;
  const previousChanged = speaker !== null && hours !== speaker.previousHours;
  const modelOptions = useMemo(() => modelChoices(models, speaker?.model ?? ''), [models, speaker?.model]);

  return (
    <div data-testid="hub-speaker">
      <h3 className="hub-heading">Speakers</h3>
      <SettingRow
        testId="hub-speaker-talks"
        title="Speaker talks"
        description={
          speaker?.talks.length
            ? `Each talk on a room speaker is its own chat. It is put away when the speaker has gone quiet for a while, and the next word starts a new one.`
            : 'No speaker talk is open on this PC: the next word at a speaker starts one.'
        }
      >
        {speaker?.talks.map((t) => (
          <Button
            key={t.session}
            variant="secondary"
            disabled={busy}
            onClick={() => {
              shell.select(t.chatId);
              shell.closeDialog(SETTINGS_DIALOG);
            }}
          >
            Open {t.title}
          </Button>
        ))}
        {speaker?.talks.length ? (
          <Button
            variant="text"
            data-testid="hub-speaker-reset"
            disabled={busy}
            onClick={() => profileId && void run(ops.resetSpeaker({ profileId }))}
          >
            Start over
          </Button>
        ) : null}
      </SettingRow>
      {speaker && (
        <SettingRow
          title="Model for new speaker chats"
          description="The model a new room-speaker chat opens with. A small local one answers quickest, which is what a spoken answer needs. App default opens it on the same model as any other new chat."
        >
          <Picker
            settings
            label="Model for new speaker chats"
            testId="hub-speaker-model"
            value={speaker.model}
            options={modelOptions}
            searchable
            disabled={busy}
            onChange={(v) => save({ model: v })}
          />
        </SettingRow>
      )}
      {speaker && (
        <SettingRow
          title="Effort for new speaker chats"
          description="How hard the agent thinks before answering in a new room-speaker chat. Low keeps replies quick."
        >
          <Picker
            settings
            label="Effort for new speaker chats"
            testId="hub-speaker-effort"
            value={speaker.effort}
            options={effortChoices(speaker.effort)}
            disabled={busy}
            onChange={(v) => save({ effort: v })}
          />
        </SettingRow>
      )}
      {speaker && (
        <SettingRow
          testId="hub-speaker-take"
          title="Take the house speaker on this PC"
          description="Off, this PC never receives this person's room-speaker turns, even while it is the only one up. On, it joins the PCs that may carry them. The PC of the home profile takes it unless you switch it off."
        >
          <Checkbox
            checked={speaker.takeSpeaker}
            disabled={busy}
            aria-label="Take the house speaker on this PC"
            data-testid="hub-speaker-take-box"
            onChange={(on) => save({ takeSpeaker: on })}
          />
        </SettingRow>
      )}
      {speaker && (
        <SettingRow
          title="Remember a finished talk for"
          description="A new talk can be told what the last one was about, for this long. 0 never tells it."
        >
          <label className="hub-limit">
            <input
              type="number"
              min={PREVIOUS_MIN}
              max={PREVIOUS_MAX}
              aria-label="Hours a finished talk is remembered"
              data-testid="hub-speaker-previous"
              value={previous}
              onChange={(e) => setPrevious(e.target.value)}
            />
            hours
          </label>
          <Button
            variant="secondary"
            disabled={busy || !previousValid || !previousChanged}
            onClick={() => save({ previousHours: hours })}
          >
            Save
          </Button>
        </SettingRow>
      )}
      {speaker && !previousValid && (
        <div className="ui-field-error">
          That must be {PREVIOUS_MIN} to {PREVIOUS_MAX} hours.
        </div>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="hub-speaker-error">
          {error}
        </div>
      )}
    </div>
  );
}
